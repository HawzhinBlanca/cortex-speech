//! Desktop ingest selection, resumable import orchestration and batch transcription commands.

use super::*;

#[tauri::command]
pub async fn open_audio_file(app: tauri::AppHandle) -> Result<Option<String>, String> {
    RATE_LIMITER.check("open_audio_file")?;
    use tauri_plugin_dialog::DialogExt;
    // MUST be async + non-blocking: this command runs on the main thread, and blocking_pick_file
    // there freezes the ENTIRE app UI for as long as the picker is open (confirmed: a second
    // command hangs while the dialog is up). pick_file schedules the native dialog on the event
    // loop and delivers the result via a oneshot without ever blocking the main thread.
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Audio", &["wav", "mp3", "flac", "m4a", "ogg", "aac", "opus", "mp4", "webm", "wma", "mov"])
        .pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    let picked = rx.await.map_err(|_| "file dialog closed unexpectedly".to_string())?;
    Ok(picked.and_then(|p| p.as_path().map(|p| p.to_string_lossy().to_string())))
}

pub(super) fn emit_or_log<T>(app: &tauri::AppHandle, event: &str, payload: T)
where
    T: serde::Serialize + Clone,
{
    if let Err(error) = app.emit(event, payload) {
        tracing::warn!("Failed to emit {event}: {error}");
    }
}

pub(super) fn send_audio_duration_probe_result(
    tx: std::sync::mpsc::Sender<crate::error::AppResult<i64>>,
    result: crate::error::AppResult<i64>,
) {
    if tx.send(result).is_err() {
        tracing::warn!("Audio duration probe worker could not send result; receiver was dropped or timed out");
    }
}

struct AgentStageEmission<'a> {
    stage: &'a str,
    status: &'a str,
    file: &'a str,
    detail: &'a str,
    current: usize,
    total: usize,
}

fn emit_agent_stage_event(app: &tauri::AppHandle, run_id: Option<&str>, source: &str, event: AgentStageEmission<'_>) {
    if let Some(run_id) = run_id {
        if let Some(app_state) = app.try_state::<AppState>() {
            let db = app_state.lock_db();
            if let Err(error) = crate::runs::record_agent_stage_event(
                &db,
                run_id,
                source,
                event.stage,
                event.status,
                event.file,
                event.detail,
                event.current,
                event.total,
            ) {
                tracing::warn!("Failed to persist agent stage event {run_id}/{}: {error}", event.stage);
            }
        }
    }

    emit_or_log(
        app,
        "pipeline-agent-stage",
        serde_json::json!({
            "stage": event.stage,
            "status": event.status,
            "file": event.file,
            "detail": event.detail,
            "current": event.current,
            "total": event.total,
        }),
    );
}

fn emit_pipeline_event(app: &tauri::AppHandle, event: &PipelineEvent, run_id: Option<&str>, source: &str) {
    match event {
        PipelineEvent::Started { total } => {
            emit_or_log(app, "pipeline-started", serde_json::json!({ "total": total }));
        }
        PipelineEvent::Phase { phase } => {
            emit_or_log(app, "pipeline-phase", serde_json::json!({ "phase": phase }));
        }
        PipelineEvent::AgentStage { stage, status, file, detail, current, total } => {
            emit_agent_stage_event(
                app,
                run_id,
                source,
                AgentStageEmission { stage, status, file, detail, current: *current, total: *total },
            );
        }
        PipelineEvent::Progress { current, total, file, status } => {
            emit_or_log(
                app,
                "pipeline-progress",
                serde_json::json!({
                    "current": current, "total": total, "file": file, "status": status
                }),
            );
        }
        PipelineEvent::Completed { total, succeeded, failed } => {
            // Use the caller's source label, not a hardcoded "directory" — this same mapper handles
            // single-file imports (source "file"), where a stray source:"directory" completion would
            // mislabel the event the UI routes on.
            let payload = serde_json::json!({
                "total": total, "succeeded": succeeded, "failed": failed,
                "source": source,
            });
            emit_or_log(app, "pipeline-complete", payload.clone());
            emit_or_log(app, "import-complete", payload);
        }
        PipelineEvent::Error { file, error } => {
            tracing::warn!("Import error for {file}: {error}");
            emit_or_log(
                app,
                "pipeline-error",
                serde_json::json!({
                    "file": file, "error": error
                }),
            );
        }
    }
}

fn log_jury_pipeline_failure(context: &str, error: &str) {
    tracing::error!("Jury pipeline failed after {context}: {error}");
}

#[tauri::command]
pub async fn import_directory(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("import_directory")?;
    {
        // Scope State before the dialog await. An unavailable dedup index must return its stable code
        // immediately rather than opening a picker for work the backend is forbidden to admit.
        let state = app.state::<AppState>();
        state.require_audio_import_ready().map_err(|error| error.to_string())?;
    }
    use tauri_plugin_dialog::DialogExt;
    // async + non-blocking folder picker — blocking_pick_folder on this main-thread command froze
    // the whole UI while the picker was open (same footgun as open_audio_file). State is fetched
    // AFTER the await so no State borrow is held across it.
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    let dir = rx.await.map_err(|_| "folder dialog closed unexpectedly".to_string())?;
    let dir_path = match dir.and_then(|p| p.as_path().map(|p| p.to_path_buf())) {
        Some(p) => p,
        None => return Err("No directory selected".into()),
    };
    validate::validate_file_path(&dir_path.to_string_lossy())?;

    let state = app.state::<AppState>();
    state.try_start_import()?;

    let cancel = Some(state.start_cancel_token());

    let pipeline = state.lock_pipeline().clone();

    let agent_run_id = uuid::Uuid::new_v4().to_string();
    let app_clone = app.clone();
    std::thread::spawn(move || {
        struct ImportGuard {
            app: tauri::AppHandle,
        }
        impl Drop for ImportGuard {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_import();
                }
            }
        }
        let _guard = ImportGuard { app: app_clone.clone() };

        // Panic-guard the directory worker (same rationale as the single-file path): an unwound
        // panic must not leave the import UI stuck "processing".
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.import_directory_with_agent_run_id(&dir_path, cancel, Some(&agent_run_id), None, None, |event| {
                emit_pipeline_event(&app_clone, &event, Some(&agent_run_id), "directory");
            })
        }));
        let result = match caught {
            Ok(r) => r,
            Err(_) => {
                Err(crate::error::AppError::Other("Import failed unexpectedly (internal error); see logs.".to_string()))
            }
        };

        if let Err(e) = result {
            let error = e.to_string();
            tracing::warn!("Import directory failed: {error}");
            emit_or_log(
                &app_clone,
                "pipeline-error",
                serde_json::json!({
                    "file": dir_path.to_string_lossy(),
                    "error": error,
                }),
            );
            let payload = serde_json::json!({
                "total": 0, "succeeded": 0, "failed": 1, "cancelled": false, "source": "directory"
            });
            emit_or_log(&app_clone, "import-complete", payload.clone());
            emit_or_log(&app_clone, "pipeline-complete", payload);
        }
    });

    Ok(serde_json::json!({ "status": "started" }))
}

/// P3.2: the crashed directory import to resume, if any. Query at STARTUP — when no import is active,
/// a still-'running' job is a crash.
#[tauri::command]
pub fn get_interrupted_import(state: State<'_, AppState>) -> Result<Option<crate::db::ImportJob>, String> {
    RATE_LIMITER.check("get_interrupted_import")?;
    state.job_store().find_interrupted_import().map_err(|error| error.to_string())
}

/// P3.2: discard an interrupted import job (the user chose not to resume).
#[tauri::command]
pub fn discard_interrupted_import(job_id: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("discard_interrupted_import")?;
    validate::validate_identifier(&job_id)?;
    state.job_store().discard_interrupted_import(&job_id).map_err(|error| error.to_string())
}

/// P3.2: resume the interrupted directory import — re-run its folder, skipping files already imported
/// in the crashed run (their segments persisted per-file). Retires the old crashed job so it is not
/// offered again; the fresh import job now tracks progress.
#[tauri::command]
pub fn resume_interrupted_import(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("resume_interrupted_import")?;
    state.require_audio_import_ready().map_err(|error| error.to_string())?;
    let job = state.job_store().find_interrupted_import().map_err(|error| error.to_string())?;
    let Some(job) = job else { return Err("No interrupted import to resume".into()) };
    let dir_path = std::path::PathBuf::from(&job.dir);
    if !dir_path.is_dir() {
        return Err(format!("The import folder no longer exists: {}", job.dir));
    }
    let completed: std::collections::HashSet<String> = job.completed_paths.iter().cloned().collect();

    state.try_start_import()?;
    // Atomically hand the old journal to a successor BEFORE the worker is spawned. The transaction
    // copies every completed path and retires the old row in one commit, so a kill here leaves exactly
    // one resumable journal. On a handoff error, release the in-process single-flight claim while the
    // original durable journal remains untouched.
    let resume_job_id = match state.job_store().handoff_import_for_resume(&job.id) {
        Ok(job_id) => job_id,
        Err(error) => {
            state.finish_import();
            return Err(format!("Could not claim the interrupted import journal for resume: {error}"));
        }
    };
    let cancel = Some(state.start_cancel_token());
    let pipeline = state.lock_pipeline().clone();
    let agent_run_id = uuid::Uuid::new_v4().to_string();
    let app_clone = app.clone();
    let worker_resume_job_id = resume_job_id.clone();
    let worker = std::thread::Builder::new().name("cortex-import-resume".into()).spawn(move || {
        struct ImportGuard {
            app: tauri::AppHandle,
        }
        impl Drop for ImportGuard {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_import();
                }
            }
        }
        let _guard = ImportGuard { app: app_clone.clone() };
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.import_directory_with_agent_run_id(
                &dir_path,
                cancel,
                Some(&agent_run_id),
                Some(&completed),
                Some(&worker_resume_job_id),
                |event| emit_pipeline_event(&app_clone, &event, Some(&agent_run_id), "directory"),
            )
        }));
        let result = match caught {
            Ok(r) => r,
            Err(_) => {
                Err(crate::error::AppError::Other("Import failed unexpectedly (internal error); see logs.".to_string()))
            }
        };
        if let Err(e) = result {
            let error = e.to_string();
            tracing::warn!("Resume import failed: {error}");
            emit_or_log(
                &app_clone,
                "pipeline-error",
                serde_json::json!({ "file": dir_path.to_string_lossy(), "error": error }),
            );
            let payload = serde_json::json!({ "total": 0, "succeeded": 0, "failed": 1, "cancelled": false, "source": "directory" });
            emit_or_log(&app_clone, "import-complete", payload.clone());
            emit_or_log(&app_clone, "pipeline-complete", payload);
        }
    });
    if let Err(error) = worker {
        state.finish_import();
        return Err(format!(
            "Could not start the resume worker: {error}. Durable import journal {resume_job_id} remains resumable."
        ));
    }
    Ok(serde_json::json!({ "status": "started", "resuming": true, "importJobId": resume_job_id }))
}

#[tauri::command]
pub fn import_audio_file(
    path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("import_audio_file")?;
    state.require_audio_import_ready().map_err(|error| error.to_string())?;
    let validated = validate::validate_file_path(&path)?;
    let file_path = Path::new(&validated).to_path_buf();

    state.try_start_import()?;

    // NOTE: do NOT pre-emit pipeline-started/-phase here. The worker emits them via
    // PipelineEvent::Started/Phase (import_single_file_with_events), exactly like the directory path.
    // Pre-emitting fired pipeline-started twice -> two stacked "Pipeline started" toasts per open.

    let cancel = Some(state.start_cancel_token());

    let pipeline = state.lock_pipeline().clone();

    let agent_run_id = uuid::Uuid::new_v4().to_string();
    let app_clone = app.clone();
    std::thread::spawn(move || {
        struct ImportGuard {
            app: tauri::AppHandle,
        }
        impl Drop for ImportGuard {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_import();
                }
            }
        }
        let _guard = ImportGuard { app: app_clone.clone() };

        // Guard the decode/VAD/ASR worker against panics (e.g. a pathological tensor inside
        // onnxruntime/sherpa-onnx). Without this, an unwound panic skips every terminal event
        // below and leaves the import progress UI stuck "processing" forever.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.import_single_file_with_events(&file_path, cancel, Some(&agent_run_id), |event| {
                // This command OWNS the terminal import-complete/pipeline-complete events: it emits
                // ready_payload right after segments exist (so the list renders immediately) and
                // done_payload after the background jury finishes. Drop the pipeline's own terminal
                // Completed event for the single-file path: emit_pipeline_event's Completed arm
                // hard-codes source:"directory" and emits import-complete + pipeline-complete —
                // forwarding it here would fire a SECOND, wrongly-sourced import-complete BEFORE the
                // jury adjudication block below, producing a spurious "Successfully processed 1 file"
                // toast, a premature idle/refresh, and an idle→adjudicating→complete flicker that tears
                // down the pipeline UI (clears the agent stages, flips to idle) before adjudication even
                // starts. The worker emits its own authoritative source:"file" import-complete +
                // pipeline-complete after adjudication, so the frontend still gets exactly one of each.
                // (The directory import path uses a different code path and keeps its Completed.)
                // Forward every other event unchanged.
                if matches!(event, PipelineEvent::Completed { .. }) {
                    return;
                }
                emit_pipeline_event(&app_clone, &event, Some(&agent_run_id), "file");
            })
        }));
        let result = match caught {
            Ok(r) => r,
            Err(_) => {
                let fname = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                emit_or_log(
                    &app_clone,
                    "pipeline-error",
                    serde_json::json!({
                        "file": fname,
                        "error": "Import failed unexpectedly (internal error); see logs.",
                    }),
                );
                let payload = serde_json::json!({ "total": 1, "succeeded": 0, "failed": 1, "source": "file" });
                emit_or_log(&app_clone, "import-complete", payload.clone());
                emit_or_log(&app_clone, "pipeline-complete", payload);
                return;
            }
        };
        match result {
            Ok(segments) => {
                // This command — NOT the pipeline — runs the single-file post-import jury adjudication,
                // exactly ONCE, on the background thread below. The pipeline's single-file path
                // intentionally skips inline adjudication (see pipeline.rs import_single_file_with_events)
                // so the jury runs here on its OWN WAL connection and never holds the shared DB lock
                // across ASR — running it in both places would emit a duplicate adjudicating phase, a
                // contradictory second jury count, a second agent_import_reports row, and double the jury
                // work (and cloud T2 cost/latency under cloud opt-in). We emit the authoritative
                // source:"file" import-complete here (the pipeline's terminal Completed event is dropped
                // in the callback above).
                let segment_ids: Vec<String> = segments.iter().map(|s| s.id.clone()).collect();
                let source_paths = vec![file_path.to_string_lossy().to_string()];
                let post_import_file =
                    file_path.file_name().and_then(|n| n.to_str()).unwrap_or("post-import jury").to_string();
                let seg_count = segments.len();

                // Import is complete once VAD has produced segments — signal it NOW so the UI
                // renders the segment list immediately, then run the heavy, ASR-bearing jury
                // adjudication on a background thread. (Previously adjudication ran inline holding
                // the global DB lock across ASR, starving the UI's get_segments so the list never
                // rendered during import.) Adjudication enriches the segments and emits a refresh.
                let ready_payload = serde_json::json!({
                    "total": 1, "succeeded": 1, "failed": 0,
                    "segmentCount": seg_count, "segmentIds": segment_ids.clone(), "source": "file",
                });
                emit_or_log(&app_clone, "import-complete", ready_payload.clone());
                emit_or_log(&app_clone, "pipeline-complete", ready_payload);

                let app_clone = app_clone.clone();
                let agent_run_id = agent_run_id.clone();
                let segment_ids = segment_ids.clone();
                std::thread::spawn(move || {
                    // Clones reserved for the panic path; the inner closure consumes the originals.
                    let panic_app = app_clone.clone();
                    let panic_run_id = agent_run_id.clone();
                    let panic_file = post_import_file.clone();
                    let panic_seg_count = seg_count;
                    // The jury can call into ASR / onnxruntime; guard against an unwind so a crash here
                    // leaves the adjudication stage settled (blocked + refresh) rather than stuck
                    // "running". Import already emitted its terminal events before this thread spawned.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        // The import fence is already down and this child uses a dedicated WAL
                        // connection. Register the entire adjudication/report mutation BEFORE its
                        // first stage-event write or DB open. A restore that won the same admission
                        // mutex makes this enrichment stop cleanly; otherwise restore sees the active
                        // mutation and refuses until the child finishes.
                        let _mutation = match begin_mutation() {
                            Ok(guard) => guard,
                            Err(error) => {
                                emit_or_log(
                                    &app_clone,
                                    "pipeline-error",
                                    serde_json::json!({ "file": &post_import_file, "error": error }),
                                );
                                let done_payload = serde_json::json!({
                                    "total": 1,
                                    "succeeded": 1,
                                    "failed": 0,
                                    "segmentCount": seg_count,
                                    "segmentIds": segment_ids.clone(),
                                    "source": "file",
                                });
                                emit_or_log(&app_clone, "import-complete", done_payload.clone());
                                emit_or_log(&app_clone, "pipeline-complete", done_payload);
                                return;
                            }
                        };
                        emit_or_log(&app_clone, "pipeline-phase", serde_json::json!({ "phase": "adjudicating" }));
                        let adjudication_detail = format!("Adjudicating {} imported segment(s)", segment_ids.len());
                        emit_agent_stage_event(
                            &app_clone,
                            Some(&agent_run_id),
                            "file",
                            AgentStageEmission {
                                stage: "jury_adjudication",
                                status: "running",
                                file: &post_import_file,
                                detail: &adjudication_detail,
                                current: 0,
                                total: segment_ids.len(),
                            },
                        );
                        let adjudication_result = if let Some(app_state) = app_clone.try_state::<AppState>() {
                            let settings = app_state.lock_settings().clone();
                            // Snapshot agentic readiness the same way pipeline.rs and check_agentic_readiness do
                            // (model status + external-provider status -> build_agentic_readiness_snapshot), so the
                            // background-thread jury report carries an honest readiness state.
                            let agentic_readiness = {
                                let model_status = app_state.lock_model_manager().status();
                                let external_provider = external_provider_status(&settings);
                                build_agentic_readiness_snapshot(&settings, &model_status, &external_provider)
                            };
                            // Adjudication uses its OWN database connection (WAL mode) rather than the
                            // shared Mutex<Database> guard, so the heavy ASR-bearing jury never starves the
                            // UI's get_segments (which locks the shared connection) while it runs. WAL lets
                            // the UI read concurrently while the jury writes verdicts on its own connection.
                            let jury_conn = app_state
                                .data_dir
                                .lock()
                                .ok()
                                .and_then(|g| (*g).clone())
                                .map(|dir| dir.join("cortex-speech.db"))
                                .and_then(|p| crate::db::Database::open(p.to_string_lossy().as_ref()).ok());
                            let Some(db) = jury_conn else {
                                emit_agent_stage_event(
                                    &app_clone,
                                    Some(&agent_run_id),
                                    "file",
                                    AgentStageEmission {
                                        stage: "jury_adjudication",
                                        status: "blocked",
                                        file: &post_import_file,
                                        detail: "could not open a private DB connection for adjudication",
                                        current: 0,
                                        total: segment_ids.len(),
                                    },
                                );
                                // Still emit the terminal refresh — adjudication is best-effort and the
                                // import itself already succeeded, so the UI must not hang waiting for a
                                // completion event that would otherwise never fire on this early return.
                                let done_payload = serde_json::json!({
                                    "total": 1,
                                    "succeeded": 1,
                                    "failed": 0,
                                    "segmentCount": seg_count,
                                    "segmentIds": segment_ids,
                                    "source": "file",
                                });
                                emit_or_log(&app_clone, "import-complete", done_payload.clone());
                                emit_or_log(&app_clone, "pipeline-complete", done_payload);
                                return;
                            };
                            // R3: this background adjudication thread writes verdicts + the import report on
                            // its OWN connection (opened above) AFTER import-complete fired — so the import
                            // fence is already down. Arm the jury writer fence for its lifetime so a restore
                            // cannot run while it writes into a possibly-just-restored library. (The
                            // run_jury_pipeline COMMAND guards its path; this direct-core caller needs its own.)
                            let _jury_writer = BgDbWriterGuard::new();
                            let mut report_options = crate::runs::AgentImportReportOptions::from_settings(&settings);
                            report_options.agent_run_id = Some(agent_run_id.clone());
                            report_options.agentic_readiness = Some(agentic_readiness);
                            let jury_data_dir = app_state.lock_data_dir().clone();
                            match run_jury_pipeline_core_via(
                                &db,
                                &settings,
                                segment_ids.clone(),
                                jury_data_dir.as_deref(),
                            ) {
                                Ok(jury_report) => {
                                    let completion_detail = format!(
                                        "Reference commits: {}; review queue: {}",
                                        jury_report["referenceCommitted"].as_u64().unwrap_or(0),
                                        jury_report["humanInbox"].as_u64().unwrap_or(0)
                                    );
                                    emit_agent_stage_event(
                                        &app_clone,
                                        Some(&agent_run_id),
                                        "file",
                                        AgentStageEmission {
                                            stage: "jury_adjudication",
                                            status: "completed",
                                            file: &post_import_file,
                                            detail: &completion_detail,
                                            current: segment_ids.len(),
                                            total: segment_ids.len(),
                                        },
                                    );
                                    crate::runs::record_agent_import_report_with_options(
                                        &db,
                                        "file",
                                        &source_paths,
                                        &segment_ids,
                                        Some(&jury_report),
                                        None,
                                        report_options,
                                    )
                                    .map(|_| {
                                        emit_agent_stage_event(
                                            &app_clone,
                                            Some(&agent_run_id),
                                            "file",
                                            AgentStageEmission {
                                                stage: "agent_report",
                                                status: "completed",
                                                file: "agent import report",
                                                detail: "Persisted auditable multi-agent import report",
                                                current: segment_ids.len(),
                                                total: segment_ids.len(),
                                            },
                                        );
                                    })
                                    .map_err(|error| {
                                        format!(
                                            "Agent import report persistence failed after single-file import: {error}"
                                        )
                                    })
                                }
                                Err(error) => {
                                    let mut message = format!(
                                        "Post-import jury adjudication failed after single-file import: {error}"
                                    );
                                    if let Err(report_error) = crate::runs::record_agent_import_report_with_options(
                                        &db,
                                        "file",
                                        &source_paths,
                                        &segment_ids,
                                        None,
                                        Some(&error),
                                        report_options,
                                    ) {
                                        message.push_str(&format!(
                                            "; additionally failed to persist agent import report: {report_error}"
                                        ));
                                    }
                                    emit_agent_stage_event(
                                        &app_clone,
                                        Some(&agent_run_id),
                                        "file",
                                        AgentStageEmission {
                                            stage: "jury_adjudication",
                                            status: "blocked",
                                            file: &post_import_file,
                                            detail: &message,
                                            current: 0,
                                            total: segment_ids.len(),
                                        },
                                    );
                                    Err(message)
                                }
                            }
                        } else {
                            let message = "App state unavailable for post-import jury adjudication".to_string();
                            emit_agent_stage_event(
                                &app_clone,
                                Some(&agent_run_id),
                                "file",
                                AgentStageEmission {
                                    stage: "jury_adjudication",
                                    status: "blocked",
                                    file: &post_import_file,
                                    detail: &message,
                                    current: 0,
                                    total: segment_ids.len(),
                                },
                            );
                            Err(message)
                        };
                        if let Err(error) = adjudication_result {
                            // Adjudication is best-effort enrichment; the import already succeeded and the
                            // UI already rendered the segments, so a failure here is a non-fatal notice
                            // (the jury_adjudication stage event already carries the detail) — NOT an
                            // import failure.
                            log_jury_pipeline_failure("single-file import", &error);
                            emit_or_log(
                                &app_clone,
                                "pipeline-error",
                                serde_json::json!({ "file": &post_import_file, "error": error }),
                            );
                        }
                        // Refresh the UI so any references/verdicts produced by adjudication appear.
                        let done_payload = serde_json::json!({
                            "total": 1,
                            "succeeded": 1,
                            "failed": 0,
                            "segmentCount": seg_count,
                            "segmentIds": segment_ids,
                            "source": "file",
                        });
                        emit_or_log(&app_clone, "import-complete", done_payload.clone());
                        emit_or_log(&app_clone, "pipeline-complete", done_payload);
                    }));
                    if outcome.is_err() {
                        // Adjudication unwound (e.g. a panic deep in ASR/onnxruntime). Settle the stage
                        // and refresh so the UI never hangs on "adjudicating" — the import itself
                        // already succeeded and rendered; this enrichment is best-effort.
                        emit_agent_stage_event(
                            &panic_app,
                            Some(&panic_run_id),
                            "file",
                            AgentStageEmission {
                                stage: "jury_adjudication",
                                status: "blocked",
                                file: &panic_file,
                                detail: "post-import adjudication crashed unexpectedly (internal error); see logs",
                                current: 0,
                                total: panic_seg_count,
                            },
                        );
                        let done_payload = serde_json::json!({
                            "total": 1, "succeeded": 1, "failed": 0,
                            "segmentCount": panic_seg_count, "source": "file",
                        });
                        emit_or_log(&panic_app, "import-complete", done_payload.clone());
                        emit_or_log(&panic_app, "pipeline-complete", done_payload);
                    }
                });
            }
            Err(e) => {
                let fname = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                emit_or_log(
                    &app_clone,
                    "pipeline-error",
                    serde_json::json!({
                        "file": fname,
                        "error": e.to_string(),
                    }),
                );
                let payload = serde_json::json!({
                    "total": 1,
                    "succeeded": 0,
                    "failed": 1,
                    "source": "file",
                });
                emit_or_log(&app_clone, "import-complete", payload.clone());
                emit_or_log(&app_clone, "pipeline-complete", payload);
            }
        }
    });

    Ok(serde_json::json!({ "status": "started", "source": "file" }))
}

/// P0.2 — expose the git SHA baked into the running exe at build time so the frontend/e2e harness
/// (and a curious user, via the About panel) can confirm the running binary matches a given commit.
/// Referencing `crate::GIT_SHA` here also guarantees the const is retained in the compiled binary.
#[tauri::command]
pub fn app_git_sha() -> String {
    crate::GIT_SHA.to_string()
}

#[tauri::command]
pub fn app_health(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("app_health")?;
    let data_dir = state.lock_data_dir().clone();
    let settings = state.lock_settings().clone();
    let db = state.lock_db();
    let mm = state.lock_model_manager();
    health::health_check(&db, &mm, &settings, data_dir.as_deref()).map_err(|e| e.to_string())
}

/// One clip to slice out of a source recording during a grouped decode. `end_ms == i64::MAX` means
/// "to the end of the file" — the whole-file case expressed as a span, so one walk covers both kinds.
pub(crate) struct ClipSpan {
    pub segment_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Record the FIRST failure only, so the reported cause is the one that actually stopped the run.
/// The terminal cause of a batch run, or `None` — the ONLY shape that may be reported as `completed`.
///
/// A per-clip failure comes first because it is the harder stop (clips were left undrafted). A
/// post-batch jury failure keeps its own wording: every clip WAS drafted, so borrowing the per-clip
/// "remaining clips were not transcribed" phrasing would be its own small lie.
pub(super) fn batch_terminal_halt_cause(clip_failure: Option<String>, jury_failure: Option<String>) -> Option<String> {
    clip_failure.or_else(|| jury_failure.map(|error| format!("post-batch jury adjudication failed: {error}")))
}

fn record_first_failure(slot: &std::sync::Mutex<Option<String>>, message: String) {
    if let Ok(mut guard) = slot.lock() {
        if guard.is_none() {
            *guard = Some(message);
        }
    }
}

/// Worker count for a batch transcription, from `CORTEX_BATCH_CONCURRENCY`.
///
/// Anything absent, unparseable, zero, negative or absurd falls back to 1 — the strictly serial
/// behaviour this command had before concurrency existed. A bad value must never silently become a
/// 32-way fan-out at an ASR server, so the fallback is the SAFE end, not the fast one.
pub(super) fn parse_batch_concurrency(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok()).filter(|n| (1..=32).contains(n)).unwrap_or(1)
}

#[tauri::command]
pub fn batch_transcribe(
    ids: Vec<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("batch_transcribe")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    let total = ids.len();

    // PREFLIGHT before claiming the job (owner rule 2026-08-11). Measured 2026-08-11: a 487-clip run
    // was accepted, then HARD-STOPPED on the very first clip because the champion server was not
    // running — the right outcome, but the caller had already been told "started". Failing here
    // returns the reason immediately and leaves the queue untouched, rather than after a write cycle.
    {
        let pipeline = state.lock_pipeline().clone();
        pipeline.preflight_primary_engine().map_err(|e| e.to_string())?;
    }

    state.try_start_batch()?;

    let cancel = state.ensure_cancel_token()?;

    let pipeline = state.lock_pipeline().clone();

    let app_clone = app.clone();
    std::thread::spawn(move || {
        struct BatchGuard {
            app: tauri::AppHandle,
        }
        impl Drop for BatchGuard {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_batch();
                }
            }
        }
        let _guard = BatchGuard { app: app_clone.clone() };

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "started", "total": total, "operation": "transcribe"
            }),
        );

        // CONCURRENCY (2026-08-11). This loop was strictly serial, which is invisible for a local ONNX
        // batch and disastrous for the 7B+cloud path: measured 22.2 s in the WSL 7B and 52.1 s in
        // Gemini refinement per clip — ~74 s of almost pure WAITING — putting 487 clips at ~8 hours
        // with BOTH GPUs at 10-18%. Neither stage is throughput-bound.
        //
        // A bounded pool is enough; no explicit two-stage pipeline is needed. The 7B server is two
        // pre-forked replicas (one per GPU) each serving ONE request at a time, so its accept queue
        // self-throttles ASR to 2 however many workers ask, while the remaining workers overlap in the
        // network-bound refinement. Throughput then lands at 2 clips per ~22 s instead of 1 per ~74 s.
        //
        // Default 1 — byte-identical behaviour to before for every local batch, opt in via
        // CORTEX_BATCH_CONCURRENCY for the 7B+cloud path. Writes are NOT parallelised: every
        // update_batch_transcription_if_unreviewed still runs under the single app_state.lock_db()
        // mutex, and the pipeline's own connections are WAL with busy_timeout=10s.
        let concurrency = parse_batch_concurrency(std::env::var("CORTEX_BATCH_CONCURRENCY").ok().as_deref());
        if concurrency > 1 {
            tracing::info!("Batch transcribe running {concurrency} clips concurrently");
        }

        let next_index = std::sync::atomic::AtomicUsize::new(0);
        let done_count = std::sync::atomic::AtomicUsize::new(0);
        let succeeded_n = std::sync::atomic::AtomicU32::new(0);
        let failed_n = std::sync::atomic::AtomicU32::new(0);
        let skipped_n = std::sync::atomic::AtomicU32::new(0);
        let cancelled_flag = std::sync::atomic::AtomicBool::new(false);
        let previous_segments_shared: std::sync::Mutex<Vec<crate::db::SpeechSegment>> =
            std::sync::Mutex::new(Vec::new());
        let transcribed_ids_shared: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        // The FIRST failure, kept verbatim: it is the one that explains the stop, and later workers
        // finishing their in-flight clip must not overwrite it with a downstream symptom.
        let first_failure: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        // Segments commonly share one long source recording. Verify/decode each unique source once,
        // keep its immutable handle alive through the whole batch, and single-flight concurrent
        // workers that reach the same recording together.
        let source_lease_cache: crate::pipeline::TranscriptionSourceLeaseCache = Default::default();

        // Pre-fetch all target segments in a SINGLE DB lock (one WHERE IN query)
        // instead of re-locking on every loop iteration. For a 500-segment batch
        // this drops mutex acquisitions from 500 → 1 for the read phase.
        let seg_map: std::collections::HashMap<String, crate::db::SpeechSegment> = {
            if let Some(app_state) = app_clone.try_state::<AppState>() {
                let db = app_state.lock_db();
                match db.get_segments_by_ids(&ids) {
                    Ok(segments) => segments.into_iter().map(|s| (s.id.clone(), s)).collect(),
                    Err(error) => {
                        tracing::error!("Batch transcribe DB prefetch failed: {error}");
                        std::collections::HashMap::new()
                    }
                }
            } else {
                std::collections::HashMap::new()
            }
        };
        // Normalizer Arc cloned once, reused across iterations.
        let normalizer_arc = app_clone.try_state::<AppState>().map(|s| Arc::clone(&s.normalizer));
        // Shared across workers: each claims its own segment, so no two ever transcribe the same id.
        let seg_map = std::sync::Mutex::new(seg_map);

        let run_worker = || loop {
            let i = next_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if i >= ids.len() {
                break;
            }
            let id = &ids[i];
            // Real backpressure (the old call discarded its result): under genuine memory pressure
            // (<1 GiB available) warn loudly and pause briefly so the OS can reclaim, instead of
            // marching a heavy ASR loop into an OOM kill mid-batch.
            if i % 10 == 0 && health::check_memory_pressure() {
                tracing::warn!(
                    "memory pressure during batch transcribe ({} MiB available) — pausing 2s at segment {}/{}",
                    health::available_memory_mb(),
                    i,
                    ids.len()
                );
                std::thread::sleep(std::time::Duration::from_secs(2));
            }

            if cancel.is_cancelled() {
                cancelled_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                break;
            }

            let Some(app_state) = app_clone.try_state::<AppState>() else {
                break;
            };
            // Use the pre-fetched normalizer (avoids re-cloning Arc on every iteration).
            let normalizer = normalizer_arc.as_ref().unwrap_or_else(|| &app_state.normalizer);

            let seg = seg_map.lock().ok().and_then(|mut map| map.remove(id.as_str()));

            if let Some(seg) = seg {
                // Capture full snapshot BEFORE transcription for complete undo.
                let mut pre_transcription_snapshot = seg.clone();
                // The batch's cancel token rides into the 7B call (2026-08-20 external review: it
                // passed None, so Cancel could not reach an in-flight or gate-queued champion call).
                let bound_source = pipeline.bind_existing_transcription_source_cached(
                    id,
                    Some(&seg.audio_path),
                    seg.alignment_json.as_deref(),
                    &source_lease_cache,
                );
                let transcription = match bound_source {
                    Ok(source) => {
                        pre_transcription_snapshot = source.segment().clone();
                        pipeline.transcribe_bound(&source, Some(cancel.as_atomic()))
                    }
                    Err(error) => Err(error),
                };
                match transcription {
                    Ok(draft) if draft.final_text.trim().is_empty() && draft.raw_text.trim().is_empty() => {
                        // A blank draft is NOT a transcript. update_batch_transcription_if_unreviewed would
                        // overwrite an existing good (unreviewed) transcript with "" — e.g. a jury_accept 7B
                        // draft re-batch-transcribed by the weaker offline CTC engine that returns Ok("") on
                        // a quiet clip. Skip; keep the current text. (Recurring
                        // blank-transcript-never-overwrites-good data-loss class; matches transcribe_segment.)
                        tracing::info!(
                            "Batch transcribe skipped {id}: empty transcript (silent clip) — existing transcript kept"
                        );
                        skipped_n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    Ok(draft) if draft.committed_by_pipeline => {
                        // ONE commit owner (2026-08-20 external review): the champion branch of
                        // `transcribe` already committed transcript + sole hypothesis + provenance
                        // atomically (and refused if the row gained a human decision). Writing the
                        // same result again here created a second owner whose failure reported
                        // "failed" for a row the first commit had already changed. Account for the
                        // work; write nothing.
                        if let Ok(mut previous_segments) = previous_segments_shared.lock() {
                            previous_segments.push(pre_transcription_snapshot);
                        }
                        if let Ok(mut transcribed_ids) = transcribed_ids_shared.lock() {
                            transcribed_ids.push(id.clone());
                        }
                        succeeded_n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    Ok(draft) => {
                        let normalized = normalizer.normalize(&draft.final_text);
                        // Guarded targeted write (NOT a full insert_segment of the stale snapshot): a
                        // human may have verified/edited this row since the batch prefetched it. This
                        // writes only the ASR fields — never `annotated_transcript` (human-only, by
                        // law; the old seed-when-empty machine write is the 348-row 2026-08-12
                        // incident) — never touches `verified`, and skips human-owned rows, so a
                        // concurrent curator decision can never be silently lost.
                        match app_state.lock_db().update_batch_transcription_if_unreviewed(
                            id,
                            &draft.raw_text,
                            Some(normalized.as_str()),
                            draft.confidence,
                            draft.confidence_source.as_deref(),
                            draft.model_version_id.as_deref(),
                            draft.cloud_call,
                        ) {
                            Ok(true) => {
                                // Guards named for what they hold, so the undo snapshot and the
                                // jury's id list still read exactly as the runtime-panic policy
                                // pins them — the collections moved behind a mutex, the obligation
                                // to record both did not.
                                if let Ok(mut previous_segments) = previous_segments_shared.lock() {
                                    previous_segments.push(pre_transcription_snapshot);
                                }
                                if let Ok(mut transcribed_ids) = transcribed_ids_shared.lock() {
                                    transcribed_ids.push(id.clone());
                                }
                                succeeded_n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                            Ok(false) => {
                                // Row became human-verified/reviewed after the batch began — skip
                                // rather than overwrite the curator's confirmed label.
                                tracing::info!("Batch transcribe skipped {id}: human-reviewed since batch start");
                                skipped_n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                            Err(error) => {
                                tracing::error!("Batch transcribe DB update failed for {id}: {error}");
                                failed_n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Batch transcribe failed for {id}: {e}");
                        failed_n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        record_first_failure(&first_failure, format!("segment {id}: {e}"));
                    }
                }
            } else {
                failed_n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                record_first_failure(&first_failure, format!("segment {id}: not found in the database"));
            }

            // HARD STOP (owner rule, 2026-08-11 — AGENT_CHARTER "Stop on the first failure").
            //
            // This loop used to count a failure and carry on. Measured 2026-08-10: 25 clips whose
            // source container the champion could not decode failed one by one, the batch ran to
            // "completion", and the review queue ended up 462 clips at champion quality and 25 at a
            // weaker engine — with no error surfaced anywhere. A partly-drafted dataset that LOOKS
            // finished is worse than a run that stopped: the mixed provenance is invisible and
            // silently poisons every measurement taken from it afterwards.
            //
            // So the first failure cancels the batch. Everything already written stays written and is
            // reported; the run is reported as FAILED, never as done.
            if first_failure.lock().map(|f| f.is_some()).unwrap_or(false) {
                cancel.cancel();
                break;
            }

            // Completion COUNT, not the claim index: with workers in flight the highest claimed index
            // runs ahead of what is actually finished, and a progress bar must never report work that
            // has not happened.
            let current = done_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": current, "total": total,
                    "file": id, "status": "transcribing", "operation": "transcribe"
                }),
            );
        };

        if concurrency == 1 {
            run_worker();
        } else {
            std::thread::scope(|scope| {
                for _ in 0..concurrency {
                    scope.spawn(run_worker);
                }
            });
        }

        let succeeded = succeeded_n.load(std::sync::atomic::Ordering::SeqCst);
        let failed = failed_n.load(std::sync::atomic::Ordering::SeqCst);
        let skipped = skipped_n.load(std::sync::atomic::Ordering::SeqCst);
        let cancelled = cancelled_flag.load(std::sync::atomic::Ordering::SeqCst);
        let previous_segments = previous_segments_shared.into_inner().unwrap_or_default();
        let transcribed_ids = transcribed_ids_shared.into_inner().unwrap_or_default();

        if !previous_segments.is_empty() {
            if let Some(app_state) = app_clone.try_state::<AppState>() {
                app_state.lock_history().push(Command::BatchTranscribe { previous_segments });
            }
        }

        // A post-batch jury failure is NOT a clean run. The drafts landed, but the adjudication that
        // decides what the review queue and the escalation path see did not — and this was log-only
        // while the terminal event below still said `completed`. That is the same flattering-finish
        // shape the hard stop above exists to kill: nobody reads the log, everybody reads the event.
        let mut jury_failure: Option<String> = None;
        if !transcribed_ids.is_empty() {
            if let Some(app_state) = app_clone.try_state::<AppState>() {
                let settings = app_state.lock_settings().clone();
                // Dedicated connection (not the shared lock_db guard) so the post-batch jury's
                // possible T2 cloud calls don't hold the global db Mutex and starve the UI's
                // get_segments while it runs. with_jury_db retries the dedicated open and only falls
                // back to the shared handle on a hard failure (so a transient lock doesn't skip the
                // jury entirely).
                let jury_data_dir = app_state.lock_data_dir().clone();
                if let Err(error) = with_jury_db(&app_state, |db| {
                    run_jury_pipeline_core_via(db, &settings, transcribed_ids, jury_data_dir.as_deref())
                }) {
                    log_jury_pipeline_failure("batch transcription", &error);
                    jury_failure = Some(error);
                }
            }
        }

        // A run that stopped on a failure is reported as HALTED, with the first cause named. It must
        // never arrive at the UI as an ordinary "completed" — that is precisely how a half-drafted
        // dataset gets mistaken for a finished one.
        let clip_failure = first_failure.into_inner().ok().flatten();
        if let Some(reason) = &clip_failure {
            tracing::error!(
                "Batch transcribe HARD-STOPPED after {succeeded} succeeded, {skipped} skipped: {reason}. \
                 Remaining clips were NOT transcribed; the dataset is incomplete, not finished."
            );
        }
        let halted_by = batch_terminal_halt_cause(clip_failure, jury_failure);
        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": if halted_by.is_some() { "halted" } else { "completed" },
                "total": total,
                "succeeded": succeeded, "failed": failed, "skipped": skipped,
                "cancelled": cancelled, "operation": "transcribe",
                "haltedBy": halted_by,
            }),
        );
    });

    Ok(serde_json::json!({ "status": "started" }))
}

fn validate_normalization_text(text: &str) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    validate::validate_text(text, 100_000, "Normalization text").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "INVALID_NORMALIZATION_TEXT",
            "The transcript normalization input is invalid.",
            false,
        )
    })
}

#[tauri::command]
#[specta::specta]
pub fn normalize_text(text: String, state: State<'_, AppState>) -> Result<String, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("normalize_text").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "RATE_LIMITED",
            "Too many normalization requests. Retry in a moment.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?;
    validate_normalization_text(&text)?;
    let settings = state.lock_settings();
    let config = crate::normalizer::NormalizationConfig {
        normalize_numbers: settings.auto_normalize,
        verbalize_numbers: settings.verbalize_numbers,
        normalize_hamza: true,
        remove_diacritics: false,
    };
    let normalizer = crate::normalizer::SoraniNormalizer::with_config(config);
    Ok(normalizer.normalize(&text))
}

#[cfg(test)]
mod typed_normalization_ipc_tests {
    use super::*;

    #[test]
    fn normalization_validation_error_is_typed_and_scrubbed() {
        let hostile = format!("token=secret {}", "x".repeat(100_000));
        let error = validate_normalization_text(&hostile).expect_err("oversized normalization input must refuse");
        let wire = serde_json::to_string(&error).expect("serialize normalization error");
        assert!(wire.contains("INVALID_NORMALIZATION_TEXT"));
        assert!(!wire.contains("secret"));
        assert!(!wire.contains("token"));
    }
}
