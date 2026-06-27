// LOCK HIERARCHY (always acquire in this order):
// 1. state.db
// 2. state.pipeline
// 3. state.normalizer
// 4. state.history
// 5. state.session
// 6. state.cache
// 7. state.settings
// 8. state.data_dir
// 9. state.media_registry

use crate::aligner;
use crate::audio;
use crate::db::SpeechSegment;
use crate::diff::TextDiff;
use crate::health;
use crate::history::Command;
use crate::history::HistoryManager;
use crate::models;
use crate::pipeline::PipelineEvent;
use crate::quality;
use crate::settings::AppSettings;
use crate::stats;
use crate::throttle::{RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::validation::input as validate;
use crate::AppState;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgenticReadinessCheck {
    pub id: String,
    pub label: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgenticReadiness {
    pub status: String,
    pub ready: bool,
    pub source_reference_models: Vec<String>,
    pub available_hypothesis_models: Vec<String>,
    pub required_hypothesis_models: usize,
    pub checks: Vec<AgenticReadinessCheck>,
}

fn readiness_check(id: &str, label: &str, status: &str, detail: impl Into<String>) -> AgenticReadinessCheck {
    AgenticReadinessCheck {
        id: id.to_string(),
        label: label.to_string(),
        status: status.to_string(),
        detail: detail.into(),
    }
}

fn model_downloaded(model_status: &[serde_json::Value], filename: &str) -> bool {
    model_status.iter().any(|model| {
        model.get("filename").and_then(serde_json::Value::as_str) == Some(filename)
            && model.get("downloaded").and_then(serde_json::Value::as_bool) == Some(true)
    })
}

fn kill_and_reap_child(child: &mut std::process::Child, context: &str) {
    if let Err(error) = child.kill() {
        tracing::warn!("Failed to kill {context}: {error}");
    }
    if let Err(error) = child.wait() {
        tracing::warn!("Failed to reap {context}: {error}");
    }
}

/// Probe `wsl --status` with a bounded timeout. `wsl --status` is known to hang indefinitely when
/// the WSL/LxssManager subsystem is wedged; a bare `.output()` would then block the import/jury path
/// (and the check_* command handlers) forever. This mirrors the kill+reap hardening already on the
/// real WSL ASR subprocess: on timeout, kill+reap the child and degrade to "not available".
fn wsl_status_available() -> bool {
    let mut cmd = std::process::Command::new("wsl");
    cmd.arg("--status").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    kill_and_reap_child(&mut child, "timed-out WSL status probe");
                    return false; // wedged WSL -> degrade, never hang
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                kill_and_reap_child(&mut child, "failed WSL status probe");
                return false;
            }
        }
    }
}

pub(crate) fn external_provider_status(settings: &AppSettings) -> serde_json::Value {
    let Some(script) = settings.external_asr_script_path() else {
        return serde_json::json!({
            "available": false,
            "message": "No external ASR provider script configured"
        });
    };

    let wsl_available = wsl_status_available();
    serde_json::json!({
        "available": wsl_available,
        "script": script,
        "message": if wsl_available {
            "WSL is available; provider script will be used for external ASR"
        } else {
            "WSL is not available or not healthy on this machine"
        }
    })
}

fn build_agentic_readiness(
    settings: &AppSettings,
    model_status: &[serde_json::Value],
    external_provider: &serde_json::Value,
) -> AgenticReadiness {
    let mut checks = Vec::new();
    let source_reference_models = settings.source_reference_models();
    if !settings.jury_cloud_opt_in {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "blocked",
            "Enable jury cloud opt-in to create Gemini whole-file reference transcripts before chunking.",
        ));
    } else if settings.llm_api_key.trim().is_empty() {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "blocked",
            "Gemini API key is not loaded in this session, so source-reference transcription would fail before chunking.",
        ));
    } else if source_reference_models.len() < 2 {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "degraded",
            format!(
                "Only {} source-reference model is configured; use at least two models for multi-reference agreement.",
                source_reference_models.len()
            ),
        ));
    } else {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "ready",
            format!("Configured source-reference models: {}", source_reference_models.join(", ")),
        ));
    }

    let ctc_300m_ready = model_downloaded(model_status, models::OMNIASR_CTC_300M_MODEL)
        && model_downloaded(model_status, models::OMNIASR_CTC_300M_TOKENS);
    let ctc_1b_ready = model_downloaded(model_status, models::OMNIASR_CTC_1B_MODEL)
        && model_downloaded(model_status, models::OMNIASR_CTC_1B_TOKENS);
    let wsl_ready = external_provider.get("available").and_then(serde_json::Value::as_bool).unwrap_or(false)
        && settings.external_asr_script_path().is_some();

    let mut available_hypothesis_models = Vec::new();
    if wsl_ready {
        available_hypothesis_models.push("omniasr-wsl-7b".to_string());
    }
    if ctc_1b_ready {
        available_hypothesis_models.push("omniasr-ctc-1b".to_string());
    }
    if ctc_300m_ready {
        available_hypothesis_models.push("omniasr-ctc-300m".to_string());
    }

    if wsl_ready {
        checks.push(readiness_check(
            "primary_asr",
            "Primary OmniASR 7B",
            "ready",
            "WSL external ASR provider is configured and WSL reports healthy.",
        ));
    } else {
        let message = external_provider
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("WSL external ASR provider is not ready.");
        checks.push(readiness_check(
            "primary_asr",
            "Primary OmniASR 7B",
            "degraded",
            format!(
                "{message} The app can still use local CTC hypotheses, but the requested 7B primary path is not ready."
            ),
        ));
    }

    let required_hypothesis_models = quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE;
    if available_hypothesis_models.len() >= required_hypothesis_models {
        checks.push(readiness_check(
            "hypothesis_coverage",
            "Multi-model hypothesis coverage",
            "ready",
            format!("Available hypothesis models: {}", available_hypothesis_models.join(", ")),
        ));
    } else {
        checks.push(readiness_check(
            "hypothesis_coverage",
            "Multi-model hypothesis coverage",
            "blocked",
            format!(
                "Only {} usable hypothesis model(s) are ready; at least {required_hypothesis_models} are required for automatic training-grade promotion.",
                available_hypothesis_models.len()
            ),
        ));
    }

    let status = if checks.iter().any(|check| check.status == "blocked") {
        "blocked"
    } else if checks.iter().any(|check| check.status == "degraded") {
        "degraded"
    } else {
        "ready"
    }
    .to_string();
    AgenticReadiness {
        ready: status == "ready",
        status,
        source_reference_models,
        available_hypothesis_models,
        required_hypothesis_models,
        checks,
    }
}

pub(crate) fn build_agentic_readiness_snapshot(
    settings: &AppSettings,
    model_status: &[serde_json::Value],
    external_provider: &serde_json::Value,
) -> serde_json::Value {
    match serde_json::to_value(build_agentic_readiness(settings, model_status, external_provider)) {
        Ok(value) => value,
        Err(error) => serde_json::json!({
            "status": "blocked",
            "ready": false,
            "sourceReferenceModels": settings.source_reference_models(),
            "availableHypothesisModels": [],
            "requiredHypothesisModels": quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE,
            "checks": [{
                "id": "readiness_snapshot",
                "label": "Agentic readiness snapshot",
                "status": "blocked",
                "detail": format!("Could not serialize agentic readiness snapshot: {error}")
            }]
        }),
    }
}

fn agentic_readiness_snapshot_for_state(state: &AppState, settings: &AppSettings) -> serde_json::Value {
    let model_status = {
        let model_manager = state.lock_model_manager();
        model_manager.status()
    };
    let external_provider = external_provider_status(settings);
    build_agentic_readiness_snapshot(settings, &model_status, &external_provider)
}

#[tauri::command]
pub fn open_audio_file(app: tauri::AppHandle) -> Result<Option<String>, String> {
    RATE_LIMITER.check("open_audio_file")?;
    use tauri_plugin_dialog::DialogExt;
    let path = app
        .dialog()
        .file()
        .add_filter("Audio", &["wav", "mp3", "flac", "m4a", "ogg", "aac", "opus", "mp4", "webm", "wma", "mov"])
        .blocking_pick_file();
    Ok(path.and_then(|p| p.as_path().map(|p| p.to_string_lossy().to_string())))
}

fn emit_or_log<T>(app: &tauri::AppHandle, event: &str, payload: T)
where
    T: serde::Serialize + Clone,
{
    if let Err(error) = app.emit(event, payload) {
        tracing::warn!("Failed to emit {event}: {error}");
    }
}

fn send_audio_duration_probe_result(
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
pub fn import_directory(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("import_directory")?;
    use tauri_plugin_dialog::DialogExt;
    let dir = app.dialog().file().blocking_pick_folder();
    let dir_path = match dir.and_then(|p| p.as_path().map(|p| p.to_path_buf())) {
        Some(p) => p,
        None => return Err("No directory selected".into()),
    };
    validate::validate_file_path(&dir_path.to_string_lossy())?;

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
            pipeline.import_directory_with_agent_run_id(&dir_path, cancel, Some(&agent_run_id), |event| {
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

#[tauri::command]
pub fn import_audio_file(
    path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("import_audio_file")?;
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
                // done_payload after the background jury finishes. Drop the pipeline's own Completed so
                // it can't fire a PREMATURE terminal event that tears down the pipeline UI (clears the
                // agent stages, flips to idle) before adjudication even starts. The directory import
                // path uses a different code path and keeps its Completed.
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
                            let agentic_readiness = agentic_readiness_snapshot_for_state(&app_state, &settings);
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
                            let mut report_options = crate::runs::AgentImportReportOptions::from_settings(&settings);
                            report_options.agent_run_id = Some(agent_run_id.clone());
                            report_options.agentic_readiness = Some(agentic_readiness);
                            match run_jury_pipeline_core(&db, &settings, segment_ids.clone()) {
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

#[tauri::command]
pub fn app_health(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("app_health")?;
    let db = state.lock_db();
    let mm = state.lock_model_manager();
    health::health_check(&db, &mm).map_err(|e| e.to_string())
}

/// Opt-in: transcribe a segment with the CONSTRAINED Kurdish-token CTC decode (guarantees
/// Kurdish-script output) via the `ort` raw-logits path, instead of the default sherpa-onnx
/// decode. Additive — it does NOT touch the default `transcribe_segment` path. Loads a fresh ort
/// session per call (fine for a user-initiated action; session caching is a perf follow-up).
#[tauri::command]
pub fn transcribe_segment_constrained(
    audio_path: String,
    alignment_json: Option<String>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("transcribe_segment_constrained")?;
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    let models = crate::models::active_models_dir();
    let model = models.join(crate::models::OMNIASR_CTC_300M_MODEL);
    let tokens = models.join(crate::models::OMNIASR_CTC_300M_TOKENS);
    if !model.exists() || !tokens.exists() {
        return Err("OmniASR model/tokens not found for constrained decode".to_string());
    }
    // decode_to_pcm returns 16 kHz mono PCM (the model's expected input rate).
    let (rate, pcm) = crate::audio::decode_to_pcm(&audio_path).map_err(|e| e.to_string())?;
    // Slice only THIS segment's clip — every VAD chunk shares the whole-source audio_path (the range is
    // in alignment_json), so decoding `pcm` directly would re-transcribe the ENTIRE recording into one
    // segment. None alignment (single-segment file) = whole file. Mirrors the finetuned/Scribe paths.
    let (clip, _suffix) =
        crate::chunking::slice_pcm_by_alignment(&pcm, rate, alignment_json.as_deref()).map_err(|e| e.to_string())?;
    let audio: Vec<f32> = clip.iter().map(|&s| s as f32 / 32768.0).collect();
    let text = crate::constrained_decode::run_constrained(&model, &tokens, &audio, true)?;
    Ok(serde_json::json!({ "text": text, "rawTranscript": text }))
}

/// Opt-in: transcribe a segment with the fine-tuned Kurdish Wav2Vec2-CTC model (ONNX via `ort`),
/// which roughly halves CER vs the stock OmniASR path (see docs/EVAL.md). Additive — the default
/// `transcribe_segment` path is unchanged. Resolves the model from `CORTEX_FINETUNED_ONNX` +
/// `CORTEX_FINETUNED_VOCAB` (dev/testing) or `<models>/finetuned-mms-ckb/{model.onnx,vocab.json}`.
#[tauri::command]
pub fn transcribe_segment_finetuned(
    audio_path: String,
    alignment_json: Option<String>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("transcribe_segment_finetuned")?;
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    let (onnx, vocab) = if let (Ok(o), Ok(v)) =
        (std::env::var("CORTEX_FINETUNED_ONNX"), std::env::var("CORTEX_FINETUNED_VOCAB"))
    {
        (std::path::PathBuf::from(o), std::path::PathBuf::from(v))
    } else {
        // Resolve finetuned-mms-ckb/{model.onnx,vocab.json} from the active (user/APPDATA) models
        // dir, then the bundled models dir (installer resources / dev tree).
        let mut found = None;
        for base in [crate::models::active_models_dir(), crate::models::bundled_models_dir()] {
            let dir = base.join("finetuned-mms-ckb");
            let onnx = dir.join("model.onnx");
            let vocab = dir.join("vocab.json");
            if onnx.exists() && vocab.exists() {
                found = Some((onnx, vocab));
                break;
            }
        }
        match found {
            Some(p) => p,
            None => {
                return Err("fine-tuned model not found (models/finetuned-mms-ckb/{model.onnx,vocab.json})".to_string())
            }
        }
    };
    let (rate, pcm) = crate::audio::decode_to_pcm(&audio_path).map_err(|e| e.to_string())?;
    // Slice out only THIS segment's clip — every VAD chunk shares the whole-source audio_path (the range
    // lives in alignment_json), so transcribing `pcm` directly would re-transcribe the ENTIRE recording
    // and write the whole-file text into one segment. None alignment (single-segment file) = whole file.
    let (clip, _suffix) =
        crate::chunking::slice_pcm_by_alignment(&pcm, rate, alignment_json.as_deref()).map_err(|e| e.to_string())?;
    let audio: Vec<f32> = clip.iter().map(|&s| s as f32 / 32768.0).collect();
    let text = crate::wav2vec2_asr::run_wav2vec2(&onnx, &vocab, "ckb", &audio)?;
    Ok(serde_json::json!({ "text": text, "rawTranscript": text }))
}

#[tauri::command]
pub fn transcribe_segment(
    segment_id: Option<String>,
    audio_path: String,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("transcribe_segment")?;
    if let Some(ref id) = segment_id {
        validate::validate_identifier(id)?;
    }
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    // Clone the pipeline (Arc-wrapped internals) so the global pipeline mutex is released before the
    // possibly-long WSL/ONNX transcription — holding it would serialize every other pipeline command.
    let pipeline = state.lock_pipeline().clone();
    let (raw_text, corrected_text, confidence) = pipeline
        .transcribe(segment_id.as_deref(), &audio_path, alignment_json.as_deref())
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "text": corrected_text, "rawTranscript": raw_text, "confidence": confidence }))
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

        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut previous_segments: Vec<crate::db::SpeechSegment> = Vec::new();
        let mut transcribed_ids: Vec<String> = Vec::new();
        let mut cancelled = false;

        // Pre-fetch all target segments in a SINGLE DB lock (one WHERE IN query)
        // instead of re-locking on every loop iteration. For a 500-segment batch
        // this drops mutex acquisitions from 500 → 1 for the read phase.
        let mut seg_map: std::collections::HashMap<String, crate::db::SpeechSegment> = {
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

        for (i, id) in ids.iter().enumerate() {
            if i % 10 == 0 {
                health::check_memory_pressure();
            }

            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }

            let Some(app_state) = app_clone.try_state::<AppState>() else {
                break;
            };
            // Use the pre-fetched normalizer (avoids re-cloning Arc on every iteration).
            let normalizer = normalizer_arc.as_ref().unwrap_or_else(|| &app_state.normalizer);

            let seg = seg_map.remove(id.as_str());

            if let Some(mut seg) = seg {
                // Capture full snapshot BEFORE transcription for complete undo.
                let pre_transcription_snapshot = seg.clone();
                match pipeline.transcribe(Some(id), &seg.audio_path, seg.alignment_json.as_deref()) {
                    Ok((raw_text, corrected_text, confidence)) => {
                        seg.raw_transcript = raw_text;
                        // CRITICAL: Do not overwrite a human-corrected annotation.
                        // Only set annotated_transcript if none exists yet.
                        if seg.annotated_transcript.is_none() {
                            seg.annotated_transcript = Some(corrected_text.clone());
                        }
                        seg.normalized_transcript = Some(normalizer.normalize(&corrected_text));
                        seg.confidence = confidence;
                        match app_state.lock_db().insert_segment(&seg) {
                            Ok(()) => {
                                previous_segments.push(pre_transcription_snapshot);
                                transcribed_ids.push(id.clone());
                                succeeded += 1;
                            }
                            Err(error) => {
                                tracing::error!("Batch transcribe DB insert failed for {id}: {error}");
                                failed += 1;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Batch transcribe failed for {id}: {e}");
                        failed += 1;
                    }
                }
            } else {
                failed += 1;
            }

            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": i + 1, "total": total,
                    "file": id, "status": "transcribing", "operation": "transcribe"
                }),
            );
        }

        if !previous_segments.is_empty() {
            if let Some(app_state) = app_clone.try_state::<AppState>() {
                app_state.lock_history().push(Command::BatchTranscribe { previous_segments });
            }
        }

        if !transcribed_ids.is_empty() {
            if let Some(app_state) = app_clone.try_state::<AppState>() {
                let settings = app_state.lock_settings().clone();
                // Separate WAL connection (not the shared lock_db guard) so the post-batch jury's
                // possible T2 cloud calls don't starve the UI's get_segments while it runs.
                match open_jury_db_connection(&app_state) {
                    Some(db) => {
                        if let Err(error) = run_jury_pipeline_core(&db, &settings, transcribed_ids) {
                            log_jury_pipeline_failure("batch transcription", &error);
                        }
                    }
                    None => log_jury_pipeline_failure(
                        "batch transcription",
                        "app data directory unavailable; could not open jury DB connection",
                    ),
                }
            }
        }

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "completed", "total": total,
                "succeeded": succeeded, "failed": failed,
                "cancelled": cancelled, "operation": "transcribe"
            }),
        );
    });

    Ok(serde_json::json!({ "status": "started" }))
}

#[tauri::command]
pub fn normalize_text(text: String, state: State<'_, AppState>) -> Result<String, String> {
    RATE_LIMITER.check("normalize_text")?;
    validate::validate_text(&text, 100000, "Normalization text")?;
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

#[tauri::command]
pub fn align_segment(
    audio_path: String,
    text: String,
    alignment_json: Option<String>,
    segment_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<aligner::WordTimestamp>, String> {
    RATE_LIMITER.check("align_segment")?;
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    if let Some(ref id) = segment_id {
        validate::validate_identifier(id)?;
    }
    if text.trim().is_empty() {
        return Err("Alignment text cannot be empty".to_string());
    }
    validate::validate_text(&text, 100000, "Alignment text")?;
    let (timestamps, quality) = {
        // Clone the pipeline OUT of the lock so the global mutex is released before the slow decode +
        // ONNX forced alignment runs — holding it serializes every other pipeline command (the UI's
        // get_import_status polling, transcribe, get_waveform) for the whole alignment. Matches the
        // get_waveform / rediarize_segments pattern; ProcessingPipeline is Clone and align takes &self.
        let pipeline = state.lock_pipeline().clone();
        pipeline.align(&audio_path, &text, alignment_json.as_deref()).map_err(|e| e.to_string())?
    };
    // Write the HONEST alignment_quality back to the segment so validation/provenance can
    // distinguish real CTC forced alignment from the linear/energy heuristic fallback —
    // never label heuristic output as 'ctc_forced'.
    if let Some(ref id) = segment_id {
        if !timestamps.is_empty() {
            let db = state.lock_db();
            db.update_alignment_quality(id, quality.as_db_str())
                .map_err(|error| format!("Failed to stamp alignment quality for {id}: {error}"))?;
        }
    }
    Ok(timestamps)
}

#[tauri::command]
pub fn get_segments(verified: Option<bool>, state: State<'_, AppState>) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_segments")?;
    let db = state.lock_db();
    db.get_segments(verified).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_segments(query: String, state: State<'_, AppState>) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("search_segments")?;
    let db = state.lock_db();
    db.search_segments(&query).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_segment(segment: SpeechSegment, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("update_segment")?;
    validate::validate_identifier(&segment.id)?;
    if let Some(ref aj) = segment.alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    let db = state.lock_db();
    let path_changed = match db.get_segment_by_id(&segment.id) {
        Ok(Some(existing)) => existing.audio_path != segment.audio_path,
        Ok(None) => true,
        Err(e) => return Err(e.to_string()),
    };
    drop(db);
    if path_changed {
        validate::validate_file_path(&segment.audio_path)?;
    }
    validate::validate_text(&segment.raw_transcript, 100000, "Raw transcript")?;
    if let Some(ref t) = segment.normalized_transcript {
        validate::validate_text(t, 100000, "Normalized transcript")?;
    }
    if let Some(ref t) = segment.annotated_transcript {
        validate::validate_text(t, 100000, "Annotated transcript")?;
    }
    if let Some(ref s) = segment.speaker_id {
        if !s.is_empty() {
            validate::validate_text(s, 256, "Speaker ID")?;
        }
    }
    let db = state.lock_db();
    let history = state.lock_history();
    HistoryManager::persist_segment_update(&db, &history, &segment).map_err(|e| e.to_string())?;
    drop(history);
    drop(db);

    state.session_auto_save();
    Ok(())
}

#[tauri::command]
pub fn delete_segment(id: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("delete_segment")?;
    validate::validate_identifier(&id)?;
    let db = state.lock_db();
    let previous = db.get_segment_by_id(&id).map_err(|e| e.to_string())?;
    db.delete_segment(&id).map_err(|e| e.to_string())?;
    drop(db);

    if let Some(seg) = previous {
        let history = state.lock_history();
        history.push(Command::DeleteSegments { segments: vec![seg] });
    }

    state.session_auto_save();
    Ok(())
}

#[tauri::command]
pub fn delete_segments_batch(ids: Vec<String>, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("delete_segments_batch")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    let db = state.lock_db();
    // Single batch-SELECT instead of N individual get_segment_by_id calls (O(1) SQL round trip).
    let segments = db.get_segments_by_ids(&ids).map_err(|e| e.to_string())?;
    db.delete_segments_batch(&ids).map_err(|e| e.to_string())?;
    drop(db);

    if !segments.is_empty() {
        let history = state.lock_history();
        history.push(Command::DeleteSegments { segments });
    }

    state.session_auto_save();
    Ok(())
}

#[tauri::command]
pub fn merge_dataset_json(json_content: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("merge_dataset_json")?;
    // Sanity-bound the pasted payload (generous enough for a real multi-segment dataset) so a
    // pathological blob can't drive an unbounded parse — matching the size guard every other
    // JSON-accepting command applies.
    validate::validate_text(&json_content, 50_000_000, "Dataset JSON")?;
    let db = state.lock_db();
    let (created, updated) = db.merge_dataset_json(&json_content).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "created": created,
        "updated": updated
    }))
}

#[tauri::command]
pub fn export_dataset(path: String, format: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("export_dataset")?;
    let validated_path = validate::validate_output_path(&path)?;
    let fmt = match format.to_lowercase().as_str() {
        "csv" => crate::settings::ExportFormat::Csv,
        "jsonl" => crate::settings::ExportFormat::Jsonl,
        "parquet" => crate::settings::ExportFormat::Parquet,
        _ => crate::settings::ExportFormat::Json,
    };
    let db = state.lock_db();
    crate::export::export_dataset(&db, Path::new(&validated_path), &fmt).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn export_dataset_bundle(
    path: String,
    production: bool,
    warning_threshold: Option<usize>,
    state: State<'_, AppState>,
) -> Result<crate::export_bundle::BundleExportResult, String> {
    STRICT_RATE_LIMITER.check("export_dataset_bundle")?;
    let validated_path = validate::validate_output_path(&path)?;
    let warning_threshold = warning_threshold.unwrap_or(0);
    let db = state.lock_db();
    let settings = state.lock_settings().clone();
    let model_manager = state.lock_model_manager();
    crate::export_bundle::export_dataset_bundle(
        &db,
        &model_manager,
        Path::new(&validated_path),
        &settings,
        production,
        warning_threshold,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn export_huggingface_dataset(path: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("export_huggingface_dataset")?;
    let validated_path = validate::validate_output_path(&path)?;
    let db = state.lock_db();
    let settings = state.lock_settings().clone();
    crate::export::export_huggingface_dataset(&db, Path::new(&validated_path), &settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_dataset_run(name: Option<String>, state: State<'_, AppState>) -> Result<crate::runs::DatasetRun, String> {
    RATE_LIMITER.check("create_dataset_run")?;
    let db = state.lock_db();
    let settings = state.lock_settings().clone();
    crate::runs::create_dataset_run(&db, name, crate::runs::config_from_settings(&settings)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_dataset_runs(state: State<'_, AppState>) -> Result<Vec<crate::runs::DatasetRun>, String> {
    RATE_LIMITER.check("list_dataset_runs")?;
    let db = state.lock_db();
    crate::runs::list_dataset_runs(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_agent_import_reports(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::runs::AgentImportReport>, String> {
    RATE_LIMITER.check("list_agent_import_reports")?;
    let db = state.lock_db();
    crate::runs::list_agent_import_reports(&db, limit).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_agent_stage_events(
    run_id: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::runs::AgentStageEvent>, String> {
    RATE_LIMITER.check("list_agent_stage_events")?;
    let db = state.lock_db();
    crate::runs::list_agent_stage_events(&db, run_id.as_deref(), limit).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn start_job(
    kind: String,
    summary: Option<String>,
    cancellable: Option<bool>,
    state: State<'_, AppState>,
) -> Result<crate::runs::JobStatus, String> {
    RATE_LIMITER.check("start_job")?;
    validate::validate_identifier(&kind)?;
    let db = state.lock_db();
    crate::runs::create_job(&db, kind, summary, cancellable.unwrap_or(false)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_job_status(id: String, state: State<'_, AppState>) -> Result<crate::runs::JobStatus, String> {
    RATE_LIMITER.check("get_job_status")?;
    validate::validate_identifier(&id)?;
    let db = state.lock_db();
    crate::runs::get_job(&db, &id).map_err(|e| e.to_string())?.ok_or_else(|| format!("Job not found: {id}"))
}

#[tauri::command]
pub fn cancel_job(id: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("cancel_job")?;
    validate::validate_identifier(&id)?;
    state.cancel_current_operation();
    let db = state.lock_db();
    crate::runs::cancel_job(&db, &id).map_err(|e| e.to_string())
}

/// The model registry, newest-first within each family — what a registry panel lists.
#[tauri::command]
pub fn list_model_versions(state: State<'_, AppState>) -> Result<Vec<crate::registry::ModelVersion>, String> {
    RATE_LIMITER.check("list_model_versions")?;
    let db = state.lock_db();
    crate::registry::list_model_versions(&db).map_err(|e| e.to_string())
}

/// The current champion for a family, if one is crowned. Reserved programmatic accessor: the
/// model-registry UI surfaces the champion via each row's `status` field, so this is intentionally
/// not invoked from the frontend — it stays for CLI/scripted callers. (IPC-surface audit 2026-06-25.)
#[tauri::command]
pub fn get_champion_model(
    family: String,
    state: State<'_, AppState>,
) -> Result<Option<crate::registry::ModelVersion>, String> {
    RATE_LIMITER.check("get_champion_model")?;
    validate::validate_identifier(&family)?;
    let db = state.lock_db();
    crate::registry::get_champion(&db, &family).map_err(|e| e.to_string())
}

/// Import an externally fine-tuned checkpoint into the registry as a gated candidate. The SHA is
/// computed server-side from the file; the caller never supplies it. Promotion is a separate,
/// gated step (not exposed yet — it must run through the eval gate), so this can only ever add a
/// candidate, never crown a champion.
#[tauri::command]
pub fn import_model_checkpoint(
    id: String,
    family: String,
    checkpoint_path: String,
    source: String,
    license: String,
    model_card_name: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    STRICT_RATE_LIMITER.check("import_model_checkpoint")?;
    validate::validate_identifier(&id)?;
    validate::validate_identifier(&family)?;
    validate::validate_identifier(&source)?;
    validate::validate_identifier(&license)?;
    if let Some(ref card) = model_card_name {
        validate::validate_text(card, 256, "model_card_name")?;
    }
    let checkpoint_path = validate::validate_file_path(&checkpoint_path)?;
    let db = state.lock_db();
    crate::registry::import_checkpoint(&db, &id, &family, &checkpoint_path, &source, &license, model_card_name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_blocking_validation_issues(
    warning_threshold: Option<usize>,
    state: State<'_, AppState>,
) -> Result<crate::export_bundle::BlockingValidationIssues, String> {
    RATE_LIMITER.check("get_blocking_validation_issues")?;
    let db = state.lock_db();
    let settings = state.lock_settings();
    crate::export_bundle::blocking_issues(&db, &settings, warning_threshold.unwrap_or(0)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn register_media_asset(
    audio_path: String,
    state: State<'_, AppState>,
) -> Result<crate::media::MediaGrant, String> {
    RATE_LIMITER.check("register_media_asset")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    // Membership check under a SHORT-LIVED db lock, then release it before the (potentially
    // gigabyte) cache copy — holding the global db mutex across std::fs::copy stalled get_segments
    // and every other DB-touching IPC for the copy duration.
    let canonical = {
        let db = state.lock_db();
        crate::media::MediaRegistry::ensure_imported(&db, &audio_path)?
    };
    let mut registry = state.lock_media_registry();
    registry.register_cached(&data_dir, &canonical)
}

#[tauri::command]
pub fn get_media_asset_url(id: String, state: State<'_, AppState>) -> Result<String, String> {
    RATE_LIMITER.check("get_media_asset_url")?;
    validate::validate_identifier(&id)?;
    let mut registry = state.lock_media_registry();
    registry.resolve(&id)
}

#[tauri::command]
pub fn check_external_provider(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("check_external_provider")?;
    let settings = state.lock_settings().clone();
    Ok(external_provider_status(&settings))
}

#[tauri::command]
pub fn check_agentic_readiness(state: State<'_, AppState>) -> Result<AgenticReadiness, String> {
    RATE_LIMITER.check("check_agentic_readiness")?;
    let settings = state.lock_settings().clone();
    let model_status = {
        let model_manager = state.lock_model_manager();
        model_manager.status()
    };
    let external_provider = external_provider_status(&settings);
    Ok(build_agentic_readiness(&settings, &model_status, &external_provider))
}

#[tauri::command]
pub fn rediarize_segments(ids: Vec<String>, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("rediarize_segments")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    // Clone the pipeline and let it open its own DB connection, so neither the global pipeline nor
    // db mutex is held across the per-file decode + diarization-inference loop (which would freeze
    // every other db-touching command for the decode duration).
    let pipeline = state.lock_pipeline().clone();
    pipeline.rediarize_segments(&ids).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn rename_speaker(old_id: String, new_id: String, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("rename_speaker")?;
    validate::validate_identifier(&new_id)?;
    let db = state.lock_db();
    db.rename_speaker(&old_id, &new_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_audio_duration(path: String) -> Result<i64, String> {
    RATE_LIMITER.check("get_audio_duration")?;
    let validated = validate::validate_file_path(&path)?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = audio::get_duration_ms(&validated);
        send_audio_duration_probe_result(tx, result);
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(dur)) => Ok(dur),
        Ok(Err(e)) => Err(e.to_string()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err("Audio duration probe timed out after 30s".to_string()),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("Audio duration probe thread disconnected".to_string())
        }
    }
}

#[tauri::command]
pub fn get_waveform(
    path: String,
    num_points: usize,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<f32>, String> {
    RATE_LIMITER.check("get_waveform")?;
    let validated = validate::validate_file_path(&path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    // Clone the pipeline out of the global lock before the (up to 30 s) decode so a waveform
    // render never starves other pipeline-lock users (matches import_audio_file / rediarize /
    // run_gold_eval_asr, which all clone for the same reason).
    let pipeline = state.lock_pipeline().clone();
    pipeline.get_waveform(&validated, num_points, alignment_json.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_import_status(state: State<'_, AppState>) -> Result<crate::pipeline::ImportStatus, String> {
    RATE_LIMITER.check("get_import_status")?;
    let pipeline = state.lock_pipeline();
    Ok(pipeline.import_status())
}

#[tauri::command]
pub fn get_dataset_stats(state: State<'_, AppState>) -> Result<stats::DatasetStats, String> {
    RATE_LIMITER.check("get_dataset_stats")?;
    let db = state.lock_db();
    stats::compute_stats(&db).map_err(|e| e.to_string())
}

/// The complete speaker list (not the truncated top-10 dashboard summary) so the speaker-management
/// panel can rename every speaker, including low-frequency ones.
#[tauri::command]
pub fn get_speakers(state: State<'_, AppState>) -> Result<Vec<stats::SpeakerStat>, String> {
    RATE_LIMITER.check("get_speakers")?;
    let db = state.lock_db();
    stats::list_speakers(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_dataset_quality(state: State<'_, AppState>) -> Result<quality::DatasetQuality, String> {
    RATE_LIMITER.check("get_dataset_quality")?;
    let db = state.lock_db();
    let settings = state.lock_settings();
    quality::compute_quality_with_settings(&db, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    RATE_LIMITER.check("get_settings")?;
    let settings = state.lock_settings();
    Ok(settings.for_client_response())
}

#[tauri::command]
pub fn update_settings(mut settings: AppSettings, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("update_settings")?;
    // Server-side trust boundary: reject a malicious endpoint/oversized payload before it
    // can take effect and redirect LLM requests (+ the API key) to an attacker's server.
    settings.validate().map_err(|e| e.to_string())?;
    let settings_path = {
        let mut current = state.lock_settings();
        settings.merge_session_secret_from(&current);
        *current = settings.clone();
        state.lock_data_dir().clone().map(|d| d.join("settings.json"))
    };
    // Apply to the running pipeline immediately so the session reflects the change.
    state.update_pipeline_settings(settings.clone());
    // Persist. A save failure (e.g. a consent toggle that never reached disk) must be SURFACED, not
    // swallowed — otherwise the user believes the change stuck while it silently reverts on the next
    // launch (a privacy hazard for the cloud opt-in toggles).
    if let Some(path) = settings_path {
        settings.save(&path).map_err(|e| {
            tracing::error!("Failed to save settings to {path:?}: {e}");
            format!(
                "Settings applied for this session but could not be saved to disk (they will revert on restart): {e}"
            )
        })?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_cache_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("get_cache_info")?;
    Ok(serde_json::json!({ "entries": state.cache.size(), "maxEntries": 1000 }))
}

#[tauri::command]
pub fn clear_cache(state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("clear_cache")?;
    state.cache.clear();
    Ok(())
}

#[tauri::command]
pub fn get_fingerprint_count(state: State<'_, AppState>) -> Result<usize, String> {
    RATE_LIMITER.check("get_fingerprint_count")?;
    Ok(state.fingerprint.count())
}

#[tauri::command]
pub fn undo(state: State<'_, AppState>) -> Result<Option<String>, String> {
    RATE_LIMITER.check("undo")?;
    let db = state.lock_db();
    let history = state.lock_history();
    history.undo(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn redo(state: State<'_, AppState>) -> Result<Option<String>, String> {
    RATE_LIMITER.check("redo")?;
    let db = state.lock_db();
    let history = state.lock_history();
    history.redo(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn can_undo(state: State<'_, AppState>) -> bool {
    state.lock_history().can_undo()
}

#[tauri::command]
pub fn can_redo(state: State<'_, AppState>) -> bool {
    state.lock_history().can_redo()
}

#[tauri::command]
pub fn compute_diff(raw: String, annotated: String) -> Result<TextDiff, String> {
    RATE_LIMITER.check("compute_diff")?;
    validate::validate_text(&raw, 100000, "Raw text")?;
    validate::validate_text(&annotated, 100000, "Annotated text")?;
    let meta = crate::telemetry::Tracer::metadata(vec![
        ("raw_len", raw.len().to_string()),
        ("ann_len", annotated.len().to_string()),
    ]);
    Ok(crate::telemetry::TRACER.record("diff.compute", meta, || crate::diff::compute_diff(&raw, &annotated)))
}

#[tauri::command]
pub fn get_tracing_stats(_state: State<'_, AppState>) -> Result<crate::telemetry::TracingStats, String> {
    RATE_LIMITER.check("get_tracing_stats")?;
    Ok(crate::telemetry::TRACER.stats())
}

#[tauri::command]
pub fn get_recent_spans(count: Option<usize>) -> Result<Vec<crate::telemetry::Span>, String> {
    RATE_LIMITER.check("get_recent_spans")?;
    let spans = crate::telemetry::TRACER.get_recent();
    let count = count.unwrap_or(50).min(spans.len());
    Ok(spans.into_iter().rev().take(count).collect())
}

#[tauri::command]
pub fn clear_tracing_spans() -> Result<(), String> {
    STRICT_RATE_LIMITER.check("clear_tracing_spans")?;
    crate::telemetry::TRACER.clear();
    Ok(())
}

/// Persist the user's view-state (search query + sort order) so it survives a restart. The values
/// are held in the session manager so the periodic counts-only auto_save preserves them too.
#[tauri::command]
pub fn save_session(search_query: String, sort_order: String, state: State<'_, AppState>) -> Result<(), String> {
    validate::validate_text(&search_query, 1000, "search_query")?;
    validate::validate_text(&sort_order, 64, "sort_order")?;
    let db = state.lock_db();
    let mut session = state.lock_session();
    session.set_view_state(search_query, sort_order);
    session.save(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn restore_session(state: State<'_, AppState>) -> Result<Option<crate::session::SessionState>, String> {
    let mut session = state.lock_session();
    session.restore().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn validate_dataset_cmd(state: State<'_, AppState>) -> Result<crate::validation::ValidationReport, String> {
    let db = state.lock_db();
    let settings = state.lock_settings();
    crate::validation::validate_dataset_with_settings(&db, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn export_audio(
    segment_ids: Vec<String>,
    options: crate::export_audio::AudioExportOptions,
    state: State<'_, AppState>,
) -> Result<crate::export_audio::AudioExportResult, String> {
    for id in &segment_ids {
        validate::validate_identifier(id)?;
    }
    let db = state.lock_db();
    crate::export_audio::export_audio_segments(&db, &segment_ids, &options).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn batch_verify(
    ids: Vec<String>,
    verified: bool,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("batch_verify")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }

    let total = ids.len();
    state.try_start_batch()?;

    let cancel = state.ensure_cancel_token()?;
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
            serde_json::json!({ "type": "started", "total": total, "operation": "verify" }),
        );

        // One targeted UPDATE per segment — no read-modify-write cycle.
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut cancelled = false;

        for (i, id) in ids.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let update_ok = if let Some(app_state) = app_clone.try_state::<AppState>() {
                match app_state.lock_db().update_verified(id, verified) {
                    Ok(updated) => updated,
                    Err(error) => {
                        tracing::error!("Batch verify DB update failed for {id}: {error}");
                        false
                    }
                }
            } else {
                false
            };

            if update_ok {
                succeeded += 1;
            } else {
                failed += 1;
            }

            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": i + 1, "total": total,
                    "file": id,
                    "status": if verified { "verifying" } else { "unverifying" },
                    "operation": "verify"
                }),
            );
        }

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "completed", "total": total,
                "succeeded": succeeded, "failed": failed,
                "cancelled": cancelled, "operation": "verify"
            }),
        );
    });

    Ok(serde_json::json!({ "status": "started" }))
}

#[tauri::command]
pub fn batch_assign_speaker(
    ids: Vec<String>,
    speaker_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("batch_assign_speaker")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    if !speaker_id.is_empty() {
        validate::validate_text(&speaker_id, 256, "Speaker ID")?;
    }

    let total = ids.len();
    state.try_start_batch()?;

    let cancel = state.ensure_cancel_token()?;
    let app_clone = app.clone();
    let speaker_id_clone = speaker_id.clone();

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
            serde_json::json!({ "type": "started", "total": total, "operation": "assign_speaker" }),
        );

        // One targeted UPDATE per segment — avoids full read-modify-write cycle.
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut cancelled = false;
        let spk: Option<&str> = if speaker_id_clone.is_empty() { None } else { Some(&speaker_id_clone) };

        for (i, id) in ids.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let update_ok = if let Some(app_state) = app_clone.try_state::<AppState>() {
                match app_state.lock_db().update_speaker_id(id, spk) {
                    Ok(updated) => updated,
                    Err(error) => {
                        tracing::error!("Batch speaker assignment DB update failed for {id}: {error}");
                        false
                    }
                }
            } else {
                false
            };

            if update_ok {
                succeeded += 1;
            } else {
                failed += 1;
            }

            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": i + 1, "total": total,
                    "file": id, "status": "assigning speaker",
                    "operation": "assign_speaker"
                }),
            );
        }

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "completed", "total": total,
                "succeeded": succeeded, "failed": failed,
                "cancelled": cancelled, "operation": "assign_speaker"
            }),
        );
    });

    Ok(serde_json::json!({ "status": "started" }))
}

#[tauri::command]
pub fn batch_normalize(
    ids: Vec<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("batch_normalize")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }

    let total = ids.len();
    state.try_start_batch()?;

    let cancel = state.ensure_cancel_token()?;
    let settings = state.lock_settings().clone();
    let config = crate::normalizer::NormalizationConfig {
        normalize_numbers: settings.auto_normalize,
        verbalize_numbers: settings.verbalize_numbers,
        normalize_hamza: true,
        remove_diacritics: false,
    };
    let normalizer = Arc::new(crate::normalizer::SoraniNormalizer::with_config(config));
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
                "type": "started", "total": total, "operation": "normalize"
            }),
        );

        let mut prefetch_failed_ids: Vec<String> = Vec::new();
        let segments: Vec<SpeechSegment> = if let Some(app_state) = app_clone.try_state::<AppState>() {
            let db = app_state.lock_db();
            let mut found = Vec::new();
            for id in &ids {
                match db.get_segment_by_id(id) {
                    Ok(Some(seg)) => found.push(seg),
                    Ok(None) => {
                        tracing::warn!("Batch normalize segment not found during prefetch: {id}");
                        prefetch_failed_ids.push(id.clone());
                    }
                    Err(error) => {
                        tracing::error!("Batch normalize DB prefetch failed for {id}: {error}");
                        prefetch_failed_ids.push(id.clone());
                    }
                }
            }
            found
        } else {
            tracing::error!("Batch normalize app state unavailable during prefetch");
            prefetch_failed_ids.extend(ids.iter().cloned());
            Vec::new()
        };

        // Fold the result-affecting config flags into the cache key. NORMALIZER_CACHE is a
        // never-cleared process-global static, so keying on raw text alone replayed the FIRST
        // config's normalization for the same text after the user toggled auto_normalize /
        // verbalize_numbers (digit handling differs), persisting the wrong normalized_transcript.
        let (auto_norm, verbalize) = (settings.auto_normalize, settings.verbalize_numbers);
        let results = crate::perf::parallel_batch(&segments, |seg| {
            let cache_key = format!("{}|{}|{}", auto_norm as u8, verbalize as u8, seg.raw_transcript);
            let normalized =
                crate::perf::NORMALIZER_CACHE.memoize(&cache_key, |_| normalizer.normalize(&seg.raw_transcript));
            (seg.id.clone(), normalized)
        });

        let mut succeeded = 0u32;
        let mut failed = prefetch_failed_ids.len() as u32;
        let mut cancelled = false;

        for (i, id) in prefetch_failed_ids.iter().enumerate() {
            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": i + 1, "total": total,
                    "file": id, "status": "failed", "operation": "normalize"
                }),
            );
        }

        for (i, (id, normalized)) in results.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }

            let update_ok = if let Some(app_state) = app_clone.try_state::<AppState>() {
                let db = app_state.lock_db();
                match db.get_segment_by_id(id) {
                    Ok(Some(mut seg)) => {
                        seg.normalized_transcript = Some(normalized.clone());
                        // CRITICAL: Do NOT overwrite annotated_transcript here.
                        // annotated_transcript is the human-corrected or LLM-refined
                        // ground truth. Normalization only affects the normalized field.
                        match db.insert_segment(&seg) {
                            Ok(()) => true,
                            Err(error) => {
                                tracing::error!("Batch normalize DB update failed for {id}: {error}");
                                false
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::warn!("Batch normalize segment disappeared before update: {id}");
                        false
                    }
                    Err(error) => {
                        tracing::error!("Batch normalize DB lookup failed before update for {id}: {error}");
                        false
                    }
                }
            } else {
                tracing::error!("Batch normalize app state unavailable before update for {id}");
                false
            };

            if update_ok {
                succeeded += 1;
            } else {
                failed += 1;
            }

            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": prefetch_failed_ids.len() + i + 1, "total": total,
                    "file": id, "status": "normalizing", "operation": "normalize"
                }),
            );
        }

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "completed", "total": total,
                "succeeded": succeeded, "failed": failed,
                "cancelled": cancelled, "operation": "normalize"
            }),
        );
    });

    Ok(serde_json::json!({ "status": "started" }))
}

#[tauri::command]
pub fn check_audio(path: String) -> Result<serde_json::Value, String> {
    let validated = validate::validate_file_path(&path)?;
    let info = audio::check_audio_file(&validated).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "duration_ms": info.duration_ms,
        "sample_rate": info.sample_rate,
        "channels": info.channels,
        "format": info.format,
    }))
}

#[tauri::command]
pub fn db_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let db = state.lock_db();
    db.info().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn db_backup(dest: String, state: State<'_, AppState>) -> Result<(), String> {
    let validated = validate::validate_output_path(&dest)?;
    let db = state.lock_db();
    db.backup(&validated).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn db_vacuum(state: State<'_, AppState>) -> Result<(), String> {
    let db = state.lock_db();
    db.vacuum().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn db_wal_checkpoint(state: State<'_, AppState>) -> Result<(), String> {
    let db = state.lock_db();
    db.wal_checkpoint().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn models_status(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    RATE_LIMITER.check("models_status")?;
    let mm = state.lock_model_manager();
    Ok(mm.status())
}

#[tauri::command]
pub fn models_download(filename: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("models_download")?;
    let model = models::MODELS
        .iter()
        .find(|m| m.filename == filename)
        .ok_or_else(|| format!("Unknown model filename: {filename}"))?;
    let mm = state.lock_model_manager();
    mm.download_model(model, |progress| {
        tracing::debug!("Download {} progress: {:.0}%", model.name, progress * 100.0);
    })
}

#[tauri::command]
pub fn models_download_all(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("models_download_all")?;
    let mm = state.lock_model_manager();
    let all_missing_count = mm.missing_models().len();
    let missing = mm.downloadable_missing_models();
    let total = missing.len();
    let skipped = all_missing_count.saturating_sub(total);

    if total == 0 {
        return Ok(serde_json::json!({"downloaded": 0, "failed": 0, "total": 0, "skipped": skipped}));
    }

    emit_or_log(
        &app,
        "model-download-progress",
        serde_json::json!({
            "type": "started", "total": total
        }),
    );

    let mut succeeded = 0u32;
    let mut failed = 0u32;

    for (i, model) in missing.iter().enumerate() {
        let name = model.name.to_string();
        let filename = model.filename.to_string();
        match mm.download_model(model, |progress| {
            emit_or_log(
                &app,
                "model-download-progress",
                serde_json::json!({
                    "type": "progress", "current": i + 1, "total": total,
                    "filename": filename, "progress": progress, "status": format!("Downloading {}", name)
                }),
            );
        }) {
            Ok(_) => succeeded += 1,
            Err(e) => {
                tracing::error!("Failed to download {}: {e}", model.name);
                failed += 1;
            }
        }
    }

    emit_or_log(
        &app,
        "model-download-progress",
        serde_json::json!({
            "type": "completed", "total": total, "succeeded": succeeded, "failed": failed
        }),
    );

    Ok(serde_json::json!({
        "downloaded": succeeded, "failed": failed, "total": total, "skipped": skipped
    }))
}

#[tauri::command]
pub fn cancel_operation(state: State<'_, AppState>) -> Result<(), String> {
    state.cancel_current_operation();
    Ok(())
}

#[tauri::command]
pub fn get_inference_stats() -> Result<serde_json::Value, String> {
    Ok(crate::inference::get_inference_stats())
}

static WSL_CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);
const WSL_LOG_LINE_PREVIEW_CHARS: usize = 4096;

fn lock_wsl_child() -> std::sync::MutexGuard<'static, Option<std::process::Child>> {
    WSL_CHILD.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("WSL child process lock was poisoned; recovering inner state");
        poisoned.into_inner()
    })
}

fn wsl_log_preview(line: &str) -> String {
    let mut chars = line.chars();
    let mut preview: String = chars.by_ref().take(WSL_LOG_LINE_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        preview.push_str(" [truncated WSL log line]");
    }
    preview
}

fn join_wsl_log_reader(thread: std::thread::JoinHandle<()>, stream: &str) {
    if thread.join().is_err() {
        tracing::warn!("WSL {stream} log reader thread panicked");
    }
}

fn wait_for_wsl_child(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    match child.wait() {
        Ok(status) => Some(status),
        Err(error) => {
            tracing::error!("Failed to wait for WSL refinement process: {error}");
            None
        }
    }
}

#[tauri::command]
pub fn run_wsl_refinement(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    limit_files: Option<u32>,
    limit_segments: Option<u32>,
    dry_run: bool,
    test_one: bool,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("run_wsl_refinement")?;

    // Hold the WSL_CHILD lock across BOTH the "already running" check AND the store below, so a
    // second concurrent invocation cannot pass this check during the spawn window and orphan the
    // first child (Child::drop does NOT kill the OS process, and the exit-monitors would cross-wire).
    // The lock spans only the bounded settings read + spawn + pipe setup; early returns drop it,
    // leaving the slot None.
    let mut wsl_slot = lock_wsl_child();
    if wsl_slot.is_some() {
        return Err("WSL 7B refinement batch transcription is already running.".into());
    }

    let external_script = state
        .settings
        .lock()
        .map_err(|e| e.to_string())?
        .external_asr_script_path()
        .ok_or_else(|| "External ASR provider script is not configured in Settings.".to_string())?;

    // Build the command
    let mut cmd = std::process::Command::new("wsl");
    cmd.arg("/root/cortex_env/bin/python3").arg(external_script);

    if let Some(limit) = limit_files {
        cmd.arg("--limit-files").arg(limit.to_string());
    }
    if let Some(limit) = limit_segments {
        cmd.arg("--limit-segments").arg(limit.to_string());
    }
    if dry_run {
        cmd.arg("--dry-run");
    }
    if test_one {
        cmd.arg("--test-one");
    }

    // Piped stdout and stderr so we can read them
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // Hide console window on Windows (prevent popping up CMD window)
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn WSL process: {}", e))?;

    // If we can't capture the pipes, kill + reap the child before bailing — otherwise the
    // spawned `wsl` process is orphaned, since Child::drop does NOT terminate it.
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            kill_and_reap_child(&mut child, "WSL refinement after stdout capture failure");
            return Err("Failed to capture stdout".to_string());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            kill_and_reap_child(&mut child, "WSL refinement after stderr capture failure");
            return Err("Failed to capture stderr".to_string());
        }
    };

    // Save the child handle under the SAME guard acquired at the check above (closing the TOCTOU),
    // then release it before the monitor thread (which re-locks to take() the child on exit).
    *wsl_slot = Some(child);
    drop(wsl_slot);

    let app_clone = app.clone();

    // Spawn thread to read stdout/stderr and monitor exit
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdout_reader = std::io::BufReader::new(stdout);
        let stderr_reader = std::io::BufReader::new(stderr);

        let app_stdout = app_clone.clone();
        let stdout_thread = std::thread::spawn(move || {
            for l in stdout_reader.lines().map_while(Result::ok) {
                emit_or_log(&app_stdout, "wsl-log", wsl_log_preview(&l));
            }
        });

        let app_stderr = app_clone.clone();
        let stderr_thread = std::thread::spawn(move || {
            for l in stderr_reader.lines().map_while(Result::ok) {
                emit_or_log(&app_stderr, "wsl-log", format!("[ERROR] {}", wsl_log_preview(&l)));
            }
        });

        // Wait for readers to finish
        join_wsl_log_reader(stdout_thread, "stdout");
        join_wsl_log_reader(stderr_thread, "stderr");

        // Wait for child to exit
        let exit_status = {
            let mut guard = lock_wsl_child();
            if let Some(mut child) = guard.take() {
                wait_for_wsl_child(&mut child)
            } else {
                None
            }
        };

        match exit_status {
            Some(status) => {
                let code = status.code().unwrap_or(0);
                emit_or_log(
                    &app_clone,
                    "wsl-status",
                    serde_json::json!({
                        "status": if status.success() { "completed" } else { "failed" },
                        "exit_code": code
                    }),
                );
            }
            None => {
                emit_or_log(
                    &app_clone,
                    "wsl-status",
                    serde_json::json!({
                        "status": "cancelled",
                        "exit_code": -1
                    }),
                );
            }
        }
    });

    Ok(serde_json::json!({ "status": "started" }))
}

#[tauri::command]
pub fn cancel_wsl_refinement() -> Result<(), String> {
    let mut guard = lock_wsl_child();
    if let Some(mut child) = guard.take() {
        child.kill().map_err(|error| format!("Failed to cancel WSL refinement process: {error}"))?;
        // Reap the child to match every other WSL kill site's kill+reap invariant — a killed child
        // must be wait()ed so it does not linger as a defunct process on non-Windows hosts.
        if let Err(error) = child.wait() {
            tracing::warn!("Failed to reap cancelled WSL refinement process: {error}");
        }
    }
    Ok(())
}

/// Insert a single segment hypothesis. Reserved programmatic API: the jury/consensus pipeline
/// produces hypotheses internally (ASR votes, Scribe votes), so there is no manual single-insert UI
/// — this stays for CLI/scripted jury orchestration. (IPC-surface audit 2026-06-25.)
#[tauri::command]
pub fn add_segment_hypothesis(state: State<'_, AppState>, hyp: crate::db::SegmentHypothesis) -> Result<(), String> {
    RATE_LIMITER.check("add_segment_hypothesis")?;
    let db = state.lock_db();
    db.insert_hypothesis(&hyp).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn run_consensus_refinery(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("run_consensus_refinery")?;

    let hypotheses = {
        let db = state.lock_db();
        db.get_all_hypotheses().map_err(|e| e.to_string())?
    };

    if hypotheses.is_empty() {
        return Ok(serde_json::json!({
            "status": "ignored",
            "message": "No segment hypotheses found in database"
        }));
    }

    let results = crate::quality::irt::fit_irt_consensus(&hypotheses);

    let mut updates = Vec::new();
    for (segment_id, consensus_text) in &results.consensus_transcripts {
        let confidence = *results.segment_confidences.get(segment_id).unwrap_or(&1.0);
        let normalized_text = state.normalizer.normalize(consensus_text);
        updates.push((segment_id.clone(), consensus_text.clone(), normalized_text, confidence));
    }

    let segments_updated = {
        let db = state.lock_db();
        db.update_segment_consensus_batch(&updates).map_err(|e| e.to_string())?
    };

    Ok(serde_json::json!({
        "status": "success",
        "segmentsUpdated": segments_updated,
        "modelAbilities": results.model_abilities,
    }))
}

#[tauri::command]
pub fn compute_acoustic_scores(state: State<'_, AppState>) -> Result<usize, String> {
    RATE_LIMITER.check("compute_acoustic_scores")?;

    let segments = {
        let db = state.lock_db();
        db.get_segments(None).map_err(|e| e.to_string())?
    };

    let aligner = {
        let settings_gpu = {
            let s = state.lock_settings();
            s.enable_gpu
        };
        let mm = state.lock_model_manager();
        aligner::ForcedAligner::new(&mm.models_dir, settings_gpu).map_err(|e| e.to_string())?
    };

    if !aligner.is_available() {
        return Err("MMS Forced Aligner model (mms_aligner.onnx) is not available.".to_string());
    }

    let mut count = 0;
    for seg in &segments {
        if seg.ctc_score.is_some() {
            continue;
        }

        let text = seg.raw_transcript.clone();
        if text.trim().is_empty() {
            continue;
        }

        let audio_path = seg.audio_path.clone();
        if !std::path::Path::new(&audio_path).exists() {
            tracing::warn!("Skipping acoustic score for {}: audio path not found: {}", seg.id, audio_path);
            continue;
        }

        let (sample_rate, pcm) = match audio::decode_to_pcm_with_timeout(&audio_path, Duration::from_secs(30)) {
            Ok(decoded) => decoded,
            Err(error) => {
                tracing::warn!("Skipping acoustic score for {}: decode failed: {error}", seg.id);
                continue;
            }
        };
        let (_sr, pcm_16k) = match audio::ensure_pcm_16khz(sample_rate, pcm) {
            Ok(resampled) => resampled,
            Err(error) => {
                tracing::warn!("Skipping acoustic score for {}: 16 kHz conversion failed: {error}", seg.id);
                continue;
            }
        };
        let score = match aligner.score_consistency(&pcm_16k, audio::TARGET_SAMPLE_RATE, &text) {
            Ok(score) => score,
            Err(error) => {
                tracing::warn!("Skipping acoustic score for {}: scoring failed: {error}", seg.id);
                continue;
            }
        };

        let db = state.lock_db();
        db.update_ctc_score(&seg.id, score).map_err(|e| e.to_string())?;
        count += 1;
    }

    Ok(count)
}

#[tauri::command]
pub fn get_dataset_certificate(
    state: State<'_, AppState>,
    target_error: f64,
    confidence_level: f64,
) -> Result<crate::quality::conformal::ConformalCertificate, String> {
    RATE_LIMITER.check("get_dataset_certificate")?;

    let segments = {
        let db = state.lock_db();
        db.get_segments(None).map_err(|e| e.to_string())?
    };

    let cert = crate::quality::conformal::calibrate_and_certify(&segments, target_error, confidence_level);
    Ok(cert)
}

#[tauri::command]
pub fn compute_ood_scores(state: State<'_, AppState>) -> Result<usize, String> {
    RATE_LIMITER.check("compute_ood_scores")?;

    let segments = {
        let db = state.lock_db();
        db.get_segments(None).map_err(|e| e.to_string())?
    };

    let mm = state.lock_model_manager();
    let detector = quality::ood::OodDetector::new(&mm.models_dir, 0.35).map_err(|e| e.to_string())?;
    drop(mm);

    let mut count = 0;
    for seg in &segments {
        if seg.ood_score.is_some() {
            continue;
        }

        let audio_path = seg.audio_path.clone();
        if !std::path::Path::new(&audio_path).exists() {
            continue;
        }

        let (sample_rate, pcm) = match audio::decode_to_pcm_with_timeout(&audio_path, Duration::from_secs(30)) {
            Ok(decoded) => decoded,
            Err(error) => {
                tracing::warn!("Skipping OOD score for {}: decode failed: {error}", seg.id);
                continue;
            }
        };
        let (_sr, pcm_16k) = match audio::ensure_pcm_16khz(sample_rate, pcm) {
            Ok(resampled) => resampled,
            Err(error) => {
                tracing::warn!("Skipping OOD score for {}: 16 kHz conversion failed: {error}", seg.id);
                continue;
            }
        };
        let score = match detector.compute_ood_score(&pcm_16k) {
            Ok(score) => score,
            Err(error) => {
                tracing::warn!("Skipping OOD score for {}: scoring failed: {error}", seg.id);
                continue;
            }
        };

        let db = state.lock_db();
        db.update_ood_score(&seg.id, score).map_err(|e| e.to_string())?;
        count += 1;
    }

    Ok(count)
}

#[tauri::command]
pub fn get_active_learning_queue(
    state: State<'_, AppState>,
    target_error: f64,
    confidence_level: f64,
    limit: usize,
) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_active_learning_queue")?;

    let segments = {
        let db = state.lock_db();
        db.get_segments(None).map_err(|e| e.to_string())?
    };

    let cert = quality::conformal::calibrate_and_certify(&segments, target_error, confidence_level);
    let q_hat = cert.threshold;

    let mut candidates: Vec<(SpeechSegment, f64)> = segments
        .into_iter()
        .filter(|s| !s.verified)
        .map(|s| {
            let score = quality::conformal::compute_nonconformity_score(&s);
            let uncertainty = -(score - q_hat).abs();
            (s, uncertainty)
        })
        .collect();

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let results: Vec<SpeechSegment> = candidates.into_iter().take(limit).map(|(s, _)| s).collect();

    Ok(results)
}

// ════════════════════════════════════════════════════════════════════════════
// Phase 1 — Gold-Set Eval Harness
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub fn import_gold_segments(
    state: State<'_, AppState>,
    inputs: Vec<crate::eval::GoldSegmentInput>,
) -> Result<usize, String> {
    RATE_LIMITER.check("import_gold_segments")?;
    let db = state.lock_db();
    crate::eval::import_gold_segments(&db, inputs).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn run_gold_eval(
    state: State<'_, AppState>,
    model_id: String,
    hypotheses: Vec<(String, String)>,
) -> Result<crate::eval::EvalRunResult, String> {
    RATE_LIMITER.check("run_gold_eval")?;
    let db = state.lock_db();
    crate::eval::run_gold_eval(&db, &model_id, hypotheses).map_err(|e| e.to_string())
}

/// Closed-loop gold eval: runs the real local ASR over the gold set's audio and scores
/// the produced hypotheses (no caller-supplied text). This is the honest-CER entrypoint.
/// `model_id` defaults to the active local model when omitted.
#[tauri::command]
pub fn run_gold_eval_asr(
    state: State<'_, AppState>,
    model_id: Option<String>,
) -> Result<crate::eval::EvalRunResult, String> {
    RATE_LIMITER.check("run_gold_eval_asr")?;
    // Clone the Arc so the (potentially long) ASR loop does not hold the pipeline lock.
    let pipeline = state.lock_pipeline().clone();
    pipeline.run_gold_eval_asr(model_id.as_deref()).map_err(|e| e.to_string())
}

/// Response for `build_scorecard`: the structured scorecard plus a ready-to-paste
/// Markdown rendering (for a README / HuggingFace model card).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardResponse {
    pub scorecard: crate::scorecard::Scorecard,
    pub markdown: String,
}

/// Build a reproducible accuracy scorecard from already-computed gold-eval results:
/// micro WER/CER with bootstrap confidence intervals, plus an optional MAPSSWE
/// significance comparison against a baseline run. Pure and deterministic.
#[tauri::command]
pub fn build_scorecard(
    system: crate::eval::EvalRunResult,
    baseline: Option<crate::eval::EvalRunResult>,
) -> Result<ScorecardResponse, String> {
    RATE_LIMITER.check("build_scorecard")?;
    let scorecard = crate::scorecard::build_scorecard(&system, baseline.as_ref(), Default::default());
    let markdown = crate::scorecard::render_markdown(&scorecard);
    Ok(ScorecardResponse { scorecard, markdown })
}

/// Compute the annotation-drift scorecard for the current dataset: how much human
/// reviewers had to change the raw ASR output (micro WER/CER with bootstrap CIs). Reads
/// the live segments directly — unlike `build_scorecard` it needs no held-out eval run.
#[tauri::command]
pub fn compute_annotation_drift_scorecard(
    state: State<'_, AppState>,
) -> Result<crate::scorecard::AnnotationDriftScorecard, String> {
    RATE_LIMITER.check("compute_annotation_drift_scorecard")?;
    let db = state.lock_db();
    let segments = db.get_segments(None).map_err(|e| e.to_string())?;
    Ok(crate::scorecard::annotation_drift_scorecard(&segments, Default::default()))
}

#[tauri::command]
pub fn run_gold_eval_local(state: State<'_, AppState>, model_id: String) -> Result<crate::eval::EvalRunResult, String> {
    RATE_LIMITER.check("run_gold_eval_local")?;
    // Clone the pipeline and let it open its own DB connection, so neither global mutex is held
    // across the multi-segment ASR eval loop (which would freeze the entire UI for minutes).
    let pipeline = state.lock_pipeline().clone();
    pipeline.run_gold_eval_local(&model_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_eval_runs(state: State<'_, AppState>) -> Result<Vec<crate::eval::EvalRun>, String> {
    RATE_LIMITER.check("list_eval_runs")?;
    let db = state.lock_db();
    crate::eval::list_eval_runs(&db).map_err(|e| e.to_string())
}

/// Measured raw-ASR vs post-jury label-quality lift (M3.1) over human-verified segments.
#[tauri::command]
pub fn get_label_quality_lift(state: State<'_, AppState>) -> Result<crate::eval::LabelQualityLift, String> {
    RATE_LIMITER.check("get_label_quality_lift")?;
    let db = state.lock_db();
    let triples = crate::eval::load_lift_triples(&db).map_err(|e| e.to_string())?;
    Ok(crate::eval::compute_label_quality_lift(&triples, 2000, 1234))
}

#[tauri::command]
pub fn list_gold_segments(state: State<'_, AppState>) -> Result<Vec<crate::eval::GoldSegment>, String> {
    RATE_LIMITER.check("list_gold_segments")?;
    let db = state.lock_db();
    crate::eval::list_gold_segments(&db).map_err(|e| e.to_string())
}

// ════════════════════════════════════════════════════════════════════════════
// Phase 2 — T0 Disagreement Gate + Jury Infrastructure
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub fn run_t0_gate(state: State<'_, AppState>, segment_ids: Vec<String>) -> Result<crate::jury::T0GateReport, String> {
    RATE_LIMITER.check("run_t0_gate")?;
    let db = state.lock_db();
    let autonomy = state.lock_settings().jury_autonomy_level.clone();
    crate::jury::run_t0_gate(&db, &segment_ids, &autonomy).map_err(|e| e.to_string())
}

/// Turn the human-corrected segments of one source file into a holdout GOLD benchmark entry. Run it
/// after correcting a file in the Review inbox: it concatenates the corrected transcripts into the
/// gold reference (excluded from all training). Returns the number of gold rows created.
#[tauri::command]
pub fn create_gold_from_file(audio_path: String, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("create_gold_from_file")?;
    if audio_path.contains('\0') {
        return Err("Audio path contains null bytes".to_string());
    }
    let db = state.lock_db();
    crate::eval::create_gold_from_verified_file(&db, &audio_path).map_err(|e| e.to_string())
}

/// Report which cloud providers have an API key configured (provider NAMES only — never the key
/// values), so the user can confirm the keys they pasted into secrets.env were detected.
#[tauri::command]
pub fn get_configured_providers(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    RATE_LIMITER.check("get_configured_providers")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let keys = crate::api_keys::ApiKeys::load(&data_dir);
    Ok(keys.configured_providers().into_iter().map(String::from).collect())
}

/// Consent gate for any ElevenLabs Scribe upload. Voice is biometric data (GDPR Art. 9), so audio
/// must NEVER be sent to a provider without the user's explicit cloud-STT opt-in. The pipeline path
/// enforces this; the direct Scribe IPC commands must too, or they silently bypass consent.
pub(crate) fn require_cloud_stt_consent(state: &AppState) -> Result<(), String> {
    if state.lock_settings().cloud_stt_opt_in {
        Ok(())
    } else {
        Err("Cloud STT opt-in is required to use ElevenLabs Scribe. Enable it in Settings.".into())
    }
}

/// Consent gate for outbound cloud-LLM channels that POST private, transcript-derived data (e.g. the
/// DPO preference-pair export). Same explicit opt-in the LLM-refine path requires — never ship the
/// user's data to a cloud endpoint without it, even though the endpoint is also allow-list-validated.
pub(crate) fn require_cloud_llm_consent(state: &AppState) -> Result<(), String> {
    if state.lock_settings().cloud_llm_opt_in {
        Ok(())
    } else {
        Err("Cloud LLM opt-in is required for this cloud upload. Enable it in Settings.".into())
    }
}

/// Scribe-transcribe ONLY this segment's clip. Every VAD chunk shares the WHOLE-source audio_path (the
/// per-segment range lives in alignment_json), so uploading `audio_path` directly would send the entire
/// recording to ElevenLabs — billing for the whole file and returning the whole-recording transcript for
/// one short segment. Decode, slice the clip by alignment, write a temp 16 kHz WAV, transcribe that, and
/// delete the temp. `alignment_json` None (a single-segment file) sends the whole file, which is correct.
fn scribe_transcribe_clip(audio_path: &str, alignment_json: Option<&str>, key: &str) -> Result<String, String> {
    let (sr, pcm) = crate::audio::decode_to_pcm(audio_path).map_err(|e| e.to_string())?;
    let (clip, _suffix) =
        crate::chunking::slice_pcm_by_alignment(&pcm, sr, alignment_json).map_err(|e| e.to_string())?;
    let tmp = std::env::temp_dir().join(format!("cortex-scribe-{}.wav", uuid::Uuid::new_v4()));
    crate::export::write_wav_atomic(&tmp, sr, &clip).map_err(|e| e.to_string())?;
    let result =
        crate::scribe_api::transcribe(tmp.to_string_lossy().as_ref(), key, crate::scribe_api::DEFAULT_MODEL, "kur");
    let _ = std::fs::remove_file(&tmp); // best-effort cleanup
    result.map_err(|e| e.to_string())
}

/// Transcribe ONE imported segment's clip with ElevenLabs Scribe (verified working for Sorani). Uses the
/// locally configured ELEVENLABS_API_KEY; errors clearly if it is absent. Returns the transcription text.
#[tauri::command]
pub fn transcribe_audio_with_scribe(
    audio_path: String,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    STRICT_RATE_LIMITER.check("transcribe_audio_with_scribe")?;
    require_cloud_stt_consent(&state)?;
    // Only audio already imported into THIS dataset may be uploaded to the cloud — never an
    // arbitrary local file path handed in by the (untrusted) webview. ensure_imported both
    // validates the path and confirms DB membership.
    let audio_path = {
        let db = state.lock_db();
        crate::media::MediaRegistry::ensure_imported(&db, &audio_path)?
    };
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let key = crate::api_keys::ApiKeys::load(&data_dir)
        .elevenlabs
        .ok_or_else(|| "No ElevenLabs API key configured — add ELEVENLABS_API_KEY to secrets.env".to_string())?;
    scribe_transcribe_clip(&audio_path, alignment_json.as_deref(), &key)
}

/// Model id for the independent ElevenLabs Scribe vote. Scribe is architecturally INDEPENDENT of the
/// OmniASR-CTC family, so (unlike the kin 300M/1B) its vote genuinely corroborates or contradicts the
/// local consensus — the highest-value escalation signal and training pair per the research.
const SCRIBE_VOTE_MODEL_ID: &str = "scribe-v2";

/// Add an independent ElevenLabs Scribe hypothesis for the given segments (typically the escalated,
/// hard ones), so the IRT jury sees an architecturally-INDEPENDENT vote rather than only kin OmniASR
/// models — closing the confidently-wrong-correlated-error hole. Opt-in and cost-bounded: it
/// transcribes only the segments the caller chooses and skips any that already have a Scribe vote
/// (idempotent). Scribe is a second opinion (~32% WER on Sorani), never auto-accepted as gold.
/// Returns the number of votes added. Re-run the jury afterwards to fold the new votes into consensus.
#[tauri::command]
pub fn add_scribe_votes(ids: Vec<String>, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("add_scribe_votes")?;
    require_cloud_stt_consent(&state)?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let key = crate::api_keys::ApiKeys::load(&data_dir)
        .elevenlabs
        .ok_or_else(|| "No ElevenLabs API key configured — add ELEVENLABS_API_KEY to secrets.env".to_string())?;

    // Read which segments still need a Scribe vote, then RELEASE the db lock before any network call —
    // never hold the global db mutex across a blocking cloud request (round-7 concurrency lesson).
    let to_vote: Vec<(String, String, Option<String>)> = {
        let db = state.lock_db();
        let segs = db.get_segments_by_ids(&ids).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for seg in &segs {
            let existing = db.get_hypotheses_for_segment(&seg.id).map_err(|e| e.to_string())?;
            if !existing.iter().any(|h| h.model_id == SCRIBE_VOTE_MODEL_ID) {
                // Capture alignment_json so the Scribe vote covers the SAME audio span as the local
                // hypotheses. Without it the vote would be the whole-recording transcript (segments share
                // the source path), which can never align with the short local hyps and poisons consensus.
                out.push((seg.id.clone(), seg.audio_path.clone(), seg.alignment_json.clone()));
            }
        }
        out
    };

    let mut added = 0usize;
    for (segment_id, audio_path, alignment_json) in to_vote {
        match scribe_transcribe_clip(&audio_path, alignment_json.as_deref(), &key) {
            Ok(transcript) => {
                let hyp = crate::db::SegmentHypothesis {
                    segment_id,
                    model_id: SCRIBE_VOTE_MODEL_ID.to_string(),
                    transcript,
                    confidence: None,
                };
                let db = state.lock_db(); // brief lock for the local insert only
                if let Err(e) = db.insert_hypothesis(&hyp) {
                    tracing::warn!("Failed to store Scribe vote: {e}");
                } else {
                    added += 1;
                }
            }
            Err(e) => tracing::warn!("Scribe vote failed for a segment: {e}"),
        }
    }
    Ok(added)
}

#[tauri::command]
pub fn get_escalation_queue(state: State<'_, AppState>, limit: usize) -> Result<Vec<crate::db::SpeechSegment>, String> {
    RATE_LIMITER.check("get_escalation_queue")?;
    let db = state.lock_db();
    db.get_escalation_queue(limit).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn record_human_decision(
    state: State<'_, AppState>,
    segment_id: String,
    decision: String,
    corrected_transcript: Option<String>,
) -> Result<(), String> {
    RATE_LIMITER.check("record_human_decision")?;
    validate::validate_identifier(&segment_id)?;
    if let Some(ref t) = corrected_transcript {
        validate::validate_text(t, 100000, "Corrected transcript")?;
    }
    let db = state.lock_db();
    db.record_human_decision(&segment_id, &decision, corrected_transcript.as_deref()).map_err(|e| e.to_string())
}

/// P3-3: Revert a segment back to unreviewed state (NULL human_decision).
/// This is the correct undo operation — avoids incorrectly re-setting to 'accept'.
#[tauri::command]
pub fn clear_human_decision(state: State<'_, AppState>, segment_id: String) -> Result<(), String> {
    RATE_LIMITER.check("clear_human_decision")?;
    validate::validate_identifier(&segment_id)?;
    let db = state.lock_db();
    db.connection()
        .execute(
            "UPDATE speech_segments
             SET human_decision = NULL,
                 corrected_at   = NULL,
                 updated_at     = datetime('now')
             WHERE id = ?1",
            rusqlite::params![segment_id],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn write_segment_verdict(
    state: State<'_, AppState>,
    segment_id: String,
    verdict: String,
    transcript: Option<String>,
    rationale: Option<String>,
    evidence_json: Option<String>,
    agent_confidence: Option<f64>,
    escalated: bool,
) -> Result<(), String> {
    RATE_LIMITER.check("write_segment_verdict")?;
    validate::validate_identifier(&segment_id)?;
    if let Some(ref t) = transcript {
        validate::validate_text(t, 100000, "Verdict transcript")?;
    }
    if let Some(ref r) = rationale {
        validate::validate_text(r, 100000, "Verdict rationale")?;
    }
    if let Some(ref ej) = evidence_json {
        validate::validate_alignment_json(ej)?;
    }
    let db = state.lock_db();
    db.write_segment_verdict(
        &segment_id,
        &verdict,
        transcript.as_deref(),
        rationale.as_deref(),
        evidence_json.as_deref(),
        agent_confidence,
        escalated,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_few_shot_examples(
    state: State<'_, AppState>,
    segment_id: String,
    k: usize,
) -> Result<Vec<crate::jury::FewShotExample>, String> {
    RATE_LIMITER.check("get_few_shot_examples")?;
    validate::validate_identifier(&segment_id)?;
    let db = state.lock_db();
    crate::jury::get_few_shot_examples(&db, &segment_id, k).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_escalation_rate_trend(state: State<'_, AppState>) -> Result<Vec<crate::jury::EscalationTrendPoint>, String> {
    RATE_LIMITER.check("get_escalation_rate_trend")?;
    let db = state.lock_db();
    crate::jury::get_escalation_rate_trend(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn run_dpo_update(state: State<'_, AppState>, endpoint: String) -> Result<String, String> {
    RATE_LIMITER.check("run_dpo_update")?;
    // DPO POSTs private, transcript-derived preference pairs outbound — a parallel cloud-LLM channel,
    // so it requires the same explicit cloud-LLM opt-in (the endpoint allow-list is a separate,
    // non-consent control). Gate before building/serializing any of that private data.
    require_cloud_llm_consent(&state)?;
    // Build + POST on a SEPARATE WAL connection (see open_jury_db_connection), never the global lock —
    // run_dpo_update performs a blocking outbound HTTP POST (up to ~120s on a stalled endpoint), and
    // holding lock_db() across it would freeze every other DB-touching IPC (get_segments, search, ...)
    // and the whole UI. Mirrors run_jury_pipeline / run_t2_for_segment.
    let db = open_jury_db_connection(&state)
        .ok_or_else(|| "App data directory is unavailable for the DPO update.".to_string())?;
    crate::jury::learning::run_dpo_update(&db, &endpoint).map_err(|e| e.to_string())
}

// ── Items 1 & 2: Full jury pipeline + T2 direct command ─────────────────────

/// `run_jury_pipeline` — chains T0 → T1 → T2 in a single call.
///
/// For each segment:
///   1. T0 IRT gate: if consensus is strong, auto-accept and skip.
///   2. T1 text judge (lexicon + perplexity): if score ≥ t1_threshold, commit.
///   3. T2 Gemini audio listener (only if cloud_opt_in=true & api_key set):
///      self-consistency N=3 vote.  No majority → escalate to human inbox.
///   4. Any remaining escalations write verdict="escalated" to DB.
///
/// Build a reference-aware candidate selection report for one segment.
fn reference_selection_text_key(report: &crate::agentic::CandidateSelectionReport) -> String {
    crate::wer::normalize_for_metrics(&report.selected_transcript)
}

fn reference_reports_have_commit_consensus(reports: &[crate::agentic::CandidateSelectionReport]) -> bool {
    if reports.is_empty() || !reports.iter().all(|report| report.should_commit) {
        return false;
    }

    let first_key = reference_selection_text_key(&reports[0]);
    !first_key.is_empty() && reports.iter().all(|report| reference_selection_text_key(report) == first_key)
}

fn best_reference_report(
    reports: &[crate::agentic::CandidateSelectionReport],
) -> Option<crate::agentic::CandidateSelectionReport> {
    reports.iter().cloned().max_by(|a, b| {
        a.selected_score
            .partial_cmp(&b.selected_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.margin.partial_cmp(&b.margin).unwrap_or(std::cmp::Ordering::Equal))
    })
}

fn reference_agreement_reports(
    reports: &[crate::agentic::CandidateSelectionReport],
) -> Vec<crate::agentic::ReferenceAgreementReport> {
    reports
        .iter()
        .map(|report| crate::agentic::ReferenceAgreementReport {
            reference_model_id: report.reference_model_id.clone(),
            selected_model_id: report.selected_model_id.clone(),
            selected_transcript: report.selected_transcript.clone(),
            selected_score: report.selected_score,
            confidence: report.confidence,
            margin: report.margin,
            should_commit: report.should_commit,
            reference_window_overlap: report.scores.first().map(|score| score.reference_window_overlap).unwrap_or(0.0),
            reference_global_overlap: report.scores.first().map(|score| score.reference_global_overlap).unwrap_or(0.0),
        })
        .collect()
}

fn hypothesis_coverage_guard(
    seg: &crate::db::SpeechSegment,
    hypotheses: &[crate::db::SegmentHypothesis],
) -> Option<crate::agentic::CandidateSelectionReport> {
    let coverage = crate::quality::hypothesis_coverage_for_model_outputs(hypotheses);
    if coverage.passes_minimum {
        return None;
    }

    let present_display =
        if coverage.non_empty_models.is_empty() { "none".to_string() } else { coverage.non_empty_models.join(", ") };
    Some(crate::agentic::CandidateSelectionReport {
        reference_model_id: Some("multi-model-hypothesis-coverage-guard".into()),
        selected_model_id: "multi-model-hypothesis-coverage-guard".into(),
        selected_transcript: seg
            .verdict_transcript
            .clone()
            .or_else(|| seg.normalized_transcript.clone())
            .unwrap_or_else(|| seg.raw_transcript.clone()),
        selected_score: 0.0,
        confidence: 0.0,
        margin: 0.0,
        should_commit: false,
        rationale: format!(
            "Multi-model hypothesis coverage guard blocked automatic adjudication because fewer than 2 non-empty model hypotheses were available before jury (required {}). Present non-empty model hypotheses: {present_display}.",
            coverage.minimum_non_empty_model_count
        ),
        reference_window_preview: String::new(),
        scores: Vec::new(),
        reference_agreement: Vec::new(),
    })
}

fn source_reference_has_stored_audio_identity(reference: &crate::db::SourceTranscriptRecord) -> bool {
    reference.audio_content_hash.as_deref().map(|hash| !hash.trim().is_empty()).unwrap_or(false)
        || reference.audio_size_bytes.is_some()
}

fn source_reference_current_audio_identity(
    audio_path: &str,
    cache: &mut std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>>,
) -> Option<crate::pipeline::SourceAudioIdentity> {
    if let Some(identity) = cache.get(audio_path) {
        return identity.clone();
    }

    let identity = match crate::pipeline::source_audio_identity(Path::new(audio_path)) {
        Ok(identity) => Some(identity),
        Err(error) => {
            tracing::warn!(
                "Cannot verify source-reference audio identity for {} before jury adjudication: {}",
                audio_path,
                error
            );
            None
        }
    };
    cache.insert(audio_path.to_string(), identity.clone());
    identity
}

fn source_reference_matches_current_audio(
    reference: &crate::db::SourceTranscriptRecord,
    identity_cache: &mut std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>>,
) -> bool {
    if !source_reference_has_stored_audio_identity(reference) {
        return false;
    }

    let Some(current_identity) = source_reference_current_audio_identity(&reference.audio_path, identity_cache) else {
        return false;
    };

    reference.audio_content_hash.as_deref() == Some(current_identity.content_hash.as_str())
        && reference.audio_size_bytes == Some(current_identity.size_bytes)
}

fn filter_source_references_for_current_audio(
    references: Vec<crate::db::SourceTranscriptRecord>,
    identity_cache: &mut std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>>,
) -> (Vec<crate::db::SourceTranscriptRecord>, Vec<String>) {
    let mut usable = Vec::new();
    let mut stale_models = std::collections::BTreeSet::new();

    for reference in references {
        if source_reference_matches_current_audio(&reference, identity_cache) {
            usable.push(reference);
        } else {
            stale_models.insert(reference.model_id);
        }
    }

    (usable, stale_models.into_iter().collect())
}

fn source_reference_coverage_guard(
    settings: &crate::settings::AppSettings,
    seg: &crate::db::SpeechSegment,
    references: &[crate::db::SourceTranscriptRecord],
    stale_reference_models: &[String],
) -> Option<crate::agentic::CandidateSelectionReport> {
    let should_require_coverage = settings.jury_cloud_opt_in || !references.is_empty();
    if !should_require_coverage && stale_reference_models.is_empty() {
        return None;
    }

    let required_models = settings.source_reference_models();
    let stale_required_models = stale_reference_models
        .iter()
        .filter(|model| required_models.iter().any(|required| required == *model))
        .cloned()
        .collect::<Vec<_>>();
    if !stale_required_models.is_empty() {
        return Some(crate::agentic::CandidateSelectionReport {
            reference_model_id: Some(format!(
                "source-reference-audio-identity-guard:stale:{}",
                stale_required_models.join("+")
            )),
            selected_model_id: "source-reference-audio-identity-guard".into(),
            selected_transcript: seg
                .verdict_transcript
                .clone()
                .or_else(|| seg.normalized_transcript.clone())
                .unwrap_or_else(|| seg.raw_transcript.clone()),
            selected_score: 0.0,
            confidence: 0.0,
            margin: 0.0,
            should_commit: false,
            rationale: format!(
                "Source-reference audio identity guard blocked automatic adjudication because stored whole-file references are missing audio identity or no longer match the current source audio bytes: {}. Recreate source references before jury.",
                stale_required_models.join(", ")
            ),
            reference_window_preview: String::new(),
            scores: Vec::new(),
            reference_agreement: Vec::new(),
        });
    }

    let present_models = references
        .iter()
        .filter(|reference| !reference.transcript_text.trim().is_empty())
        .map(|reference| reference.model_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let missing_models =
        required_models.iter().filter(|model| !present_models.contains(*model)).cloned().collect::<Vec<_>>();
    if missing_models.is_empty() {
        return None;
    }

    let present_display = if present_models.is_empty() {
        "none".to_string()
    } else {
        present_models.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    Some(crate::agentic::CandidateSelectionReport {
        reference_model_id: Some(format!("source-reference-coverage-guard:missing:{}", missing_models.join("+"))),
        selected_model_id: "source-reference-coverage-guard".into(),
        selected_transcript: seg
            .verdict_transcript
            .clone()
            .or_else(|| seg.normalized_transcript.clone())
            .unwrap_or_else(|| seg.raw_transcript.clone()),
        selected_score: 0.0,
        confidence: 0.0,
        margin: 0.0,
        should_commit: false,
        rationale: format!(
            "Source-reference coverage guard blocked automatic adjudication because configured whole-file reference models are missing or empty: {}. Required source reference models: {}. Present non-empty source reference models: {}.",
            missing_models.join(", "),
            required_models.join(", "),
            present_display
        ),
        reference_window_preview: String::new(),
        scores: Vec::new(),
        reference_agreement: Vec::new(),
    })
}

fn reference_selection_for_segment(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    seg: &crate::db::SpeechSegment,
    hypotheses: &[crate::db::SegmentHypothesis],
    duration_cache: &mut std::collections::HashMap<String, Option<i64>>,
    identity_cache: &mut std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>>,
) -> Result<Option<crate::agentic::CandidateSelectionReport>, String> {
    let references = db.get_source_transcripts_for_audio(&seg.audio_path).map_err(|e| e.to_string())?;
    let (references, stale_reference_models) = filter_source_references_for_current_audio(references, identity_cache);
    if let Some(report) = source_reference_coverage_guard(settings, seg, &references, &stale_reference_models) {
        return Ok(Some(report));
    }
    if references.is_empty() {
        return Ok(None);
    }

    let duration = if let Some(cached) = duration_cache.get(&seg.audio_path) {
        *cached
    } else {
        let probed = crate::audio::get_duration_ms(Path::new(&seg.audio_path)).ok();
        duration_cache.insert(seg.audio_path.clone(), probed);
        probed
    };

    let mut reports: Vec<crate::agentic::CandidateSelectionReport> = Vec::new();
    for reference in references {
        if reference.transcript_text.trim().is_empty() {
            continue;
        }
        let Some(mut report) = crate::agentic::select_best_candidate_against_reference(
            seg,
            &reference.transcript_text,
            duration,
            hypotheses,
        ) else {
            continue;
        };
        report.reference_model_id = Some(reference.model_id);
        reports.push(report);
    }

    let Some(mut best) = best_reference_report(&reports) else {
        return Ok(None);
    };

    if reports.len() <= 1 {
        return Ok(Some(best));
    }

    let agreement_reports = reference_agreement_reports(&reports);
    let committing_count = reports.iter().filter(|report| report.should_commit).count();

    // ── Full consensus: all references commit and agree on the same transcript ──
    if reference_reports_have_commit_consensus(&reports) {
        let model_ids = reports.iter().filter_map(|report| report.reference_model_id.as_deref()).collect::<Vec<_>>();
        best.reference_model_id = Some(format!("multi-reference-consensus:{}", model_ids.join("+")));
        best.confidence = reports.iter().map(|report| report.confidence).fold(best.confidence, f64::min);
        best.selected_score = reports.iter().map(|report| report.selected_score).fold(best.selected_score, f64::min);
        best.margin = reports.iter().map(|report| report.margin).fold(best.margin, f64::min);
        best.reference_agreement = agreement_reports;
        best.rationale = format!(
            "Reference-aware agent selected '{}' only after {} whole-file references agreed on the transcript. {}",
            best.selected_model_id,
            reports.len(),
            best.rationale
        );
        return Ok(Some(best));
    }

    // ── Agreement boost: references select the same candidate but not all individually
    //    pass the commit threshold.  Cross-reference agreement is strong evidence. ──
    let best_key = reference_selection_text_key(&best);
    let agreeing_on_best = reports
        .iter()
        .filter(|report| !best_key.is_empty() && reference_selection_text_key(report) == best_key)
        .count();
    if agreeing_on_best >= 2 && best.selected_score >= 0.55 && !best.scores.is_empty() {
        let model_ids = reports.iter().filter_map(|report| report.reference_model_id.as_deref()).collect::<Vec<_>>();
        let confidence_boost = 0.08 * (agreeing_on_best as f64 - 1.0).min(2.0);
        best.reference_model_id = Some(format!("multi-reference-agreement-boost:{}", model_ids.join("+")));
        best.should_commit = true;
        best.confidence = (best.confidence + confidence_boost).clamp(0.0, 1.0);
        best.reference_agreement = agreement_reports;
        best.rationale = format!(
            "Reference-aware agent boosted commit confidence because {agreeing_on_best}/{} whole-file references independently selected the same candidate '{}' (score {:.2}, boost +{:.2}). {}",
            reports.len(),
            best.selected_model_id,
            best.selected_score,
            confidence_boost,
            best.rationale
        );
        return Ok(Some(best));
    }

    best.should_commit = false;
    best.reference_agreement = agreement_reports;
    best.rationale = format!(
        "Reference-aware agent guarded this segment because {} whole-file references produced {} committing reports without unanimous transcript agreement.",
        reports.len(),
        committing_count
    );
    Ok(Some(best))
}

fn reference_selection_evidence(report: &crate::agentic::CandidateSelectionReport) -> crate::jury::t1_judge::Evidence {
    crate::jury::t1_judge::Evidence {
        tool: "source_reference_adjudicator".into(),
        result: format!(
            "reference={} winner={} score={:.2} overlap={:.2} margin={:.2} commit={}",
            report.reference_model_id.as_deref().unwrap_or("unknown"),
            report.selected_model_id,
            report.selected_score,
            report.scores.first().map(|score| score.reference_window_overlap).unwrap_or(0.0),
            report.margin,
            report.should_commit
        ),
        supports_hypothesis: report.should_commit,
    }
}

fn load_hypotheses_for_segment(
    db: &crate::db::Database,
    seg_id: &str,
    seg: &crate::db::SpeechSegment,
) -> Result<Vec<crate::db::SegmentHypothesis>, String> {
    let mut hyps = db.get_hypotheses_for_segment(seg_id).map_err(|e| e.to_string())?;
    if hyps.is_empty() {
        hyps.push(crate::db::SegmentHypothesis {
            segment_id: seg_id.to_string(),
            model_id: "asr".into(),
            transcript: seg.raw_transcript.clone(),
            confidence: seg.confidence,
        });
    }
    Ok(hyps)
}

fn has_human_decision(seg: &crate::db::SpeechSegment) -> bool {
    seg.human_decision.as_deref().map(|decision| !decision.trim().is_empty()).unwrap_or(false)
}

fn has_final_machine_verdict(seg: &crate::db::SpeechSegment) -> bool {
    seg.verdict.as_deref().map(|verdict| !verdict.trim().is_empty()).unwrap_or(false) && !seg.escalated
}

pub fn run_jury_pipeline_core(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    segment_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    let t1_threshold = settings.jury_t1_threshold;
    let cloud_opt_in = settings.jury_cloud_opt_in;
    let jury_model = settings.jury_model.clone();
    let n_samples = settings.jury_self_consistency_n as usize;
    let api_key = settings.llm_api_key.clone();

    let initial_seg_map: std::collections::HashMap<String, crate::db::SpeechSegment> = db
        .get_segments_by_ids(&segment_ids)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();

    let mut t1_committed = 0usize;
    let mut t2_committed = 0usize;
    let mut still_escalated = 0usize;
    let mut reference_committed = 0usize;
    let mut reference_guarded = 0usize;
    let mut hypothesis_guarded = 0usize;
    let mut source_duration_cache: std::collections::HashMap<String, Option<i64>> = std::collections::HashMap::new();
    let mut source_identity_cache: std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>> =
        std::collections::HashMap::new();
    let mut reference_reports: std::collections::HashMap<String, crate::agentic::CandidateSelectionReport> =
        std::collections::HashMap::new();
    let mut t0_candidate_ids = Vec::new();
    let mut review_ids = Vec::new();

    // Source-reference adjudication runs before T0 so whole-file context cannot
    // be bypassed by a local multi-ASR consensus auto-accept.
    for seg_id in &segment_ids {
        let Some(seg) = initial_seg_map.get(seg_id) else {
            continue;
        };
        if has_human_decision(seg) || has_final_machine_verdict(seg) {
            continue;
        }

        let hyps = load_hypotheses_for_segment(db, seg_id, seg)?;
        if let Some(report) = hypothesis_coverage_guard(seg, &hyps) {
            reference_reports.insert(seg_id.clone(), report);
            review_ids.push(seg_id.clone());
            hypothesis_guarded += 1;
            continue;
        }
        match reference_selection_for_segment(
            db,
            settings,
            seg,
            &hyps,
            &mut source_duration_cache,
            &mut source_identity_cache,
        )? {
            Some(report) if report.should_commit => {
                let ev_json = serde_json::to_string(&report)
                    .map_err(|e| format!("Failed to serialize reference selection evidence for {seg_id}: {e}"))?;
                db.write_segment_verdict(
                    seg_id,
                    "jury_accept",
                    Some(&report.selected_transcript),
                    Some(&report.rationale),
                    Some(ev_json.as_str()),
                    Some(report.confidence),
                    false,
                )
                .map_err(|e| e.to_string())?;
                reference_committed += 1;
            }
            Some(report) => {
                reference_reports.insert(seg_id.clone(), report);
                review_ids.push(seg_id.clone());
                reference_guarded += 1;
            }
            None if seg.escalated => review_ids.push(seg_id.clone()),
            None => t0_candidate_ids.push(seg_id.clone()),
        }
    }

    let t0_report = if t0_candidate_ids.is_empty() {
        crate::jury::T0GateReport { total: 0, auto_accepted: 0, escalated: 0, decisions: Vec::new() }
    } else {
        crate::jury::run_t0_gate(db, &t0_candidate_ids, &settings.jury_autonomy_level).map_err(|e| e.to_string())?
    };

    if !t0_candidate_ids.is_empty() {
        let post_t0_map: std::collections::HashMap<String, crate::db::SpeechSegment> = db
            .get_segments_by_ids(&t0_candidate_ids)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        for seg_id in &t0_candidate_ids {
            if let Some(seg) = post_t0_map.get(seg_id) {
                if seg.escalated && !has_human_decision(seg) {
                    review_ids.push(seg_id.clone());
                }
            }
        }
    }

    let review_seg_map: std::collections::HashMap<String, crate::db::SpeechSegment> = db
        .get_segments_by_ids(&review_ids)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();

    for seg_id in &review_ids {
        let seg = match review_seg_map.get(seg_id) {
            Some(s) => s.clone(),
            None => continue,
        };

        // Load hypotheses from database
        let hyps = load_hypotheses_for_segment(db, seg_id, &seg)?;

        // ── Stage 2: T1 text judge ────────────────────────────────────────────
        let reference_report = match reference_reports.remove(seg_id) {
            Some(report) => Some(report),
            None => reference_selection_for_segment(
                db,
                settings,
                &seg,
                &hyps,
                &mut source_duration_cache,
                &mut source_identity_cache,
            )?,
        };
        if let Some(report) = &reference_report {
            if report.should_commit {
                let ev_json = serde_json::to_string(report)
                    .map_err(|e| format!("Failed to serialize reference selection evidence for {seg_id}: {e}"))?;
                db.write_segment_verdict(
                    seg_id,
                    "jury_accept",
                    Some(&report.selected_transcript),
                    Some(&report.rationale),
                    Some(ev_json.as_str()),
                    Some(report.confidence),
                    false,
                )
                .map_err(|e| e.to_string())?;
                reference_committed += 1;
                continue;
            }
        }

        let effective_t1_threshold = if reference_report.is_some() { 1.01 } else { t1_threshold };
        match crate::jury::t1_judge::judge_t1(seg_id, &hyps, effective_t1_threshold) {
            crate::jury::t1_judge::T1Decision::Commit { transcript, reason, evidence, confidence, .. } => {
                let evidence_payload = match &reference_report {
                    Some(report) => serde_json::json!({
                        "t1Evidence": evidence,
                        "referenceSelection": report,
                    }),
                    None => serde_json::json!(evidence),
                };
                let ev_json = serde_json::to_string(&evidence_payload)
                    .map_err(|e| format!("Failed to serialize T1 evidence for {seg_id}: {e}"))?;
                db.write_segment_verdict(
                    seg_id,
                    "jury_accept",
                    Some(&transcript),
                    Some(&reason),
                    Some(ev_json.as_str()),
                    Some(confidence),
                    false,
                )
                .map_err(|e| e.to_string())?;
                t1_committed += 1;
            }

            crate::jury::t1_judge::T1Decision::EscalateToT2 { hypotheses, mut t1_evidence, .. } => {
                if let Some(report) = &reference_report {
                    t1_evidence.push(reference_selection_evidence(report));
                }
                // ── Stage 3: T2 Gemini audio listener ─────────────────────────
                if cloud_opt_in && !api_key.trim().is_empty() {
                    // Encode only this segment's source-time span, not the whole long source file.
                    let audio_b64 = match crate::agentic::segment_audio_as_wav_base64(&seg) {
                        Ok(encoded) => encoded,
                        Err(e) => {
                            tracing::warn!("T2: cannot prepare segment audio for {seg_id}: {e}");
                            db.write_segment_verdict(seg_id, "escalated", None, Some(&e.to_string()), None, None, true)
                                .map_err(|e| e.to_string())?;
                            still_escalated += 1;
                            continue;
                        }
                    };

                    let few_shots = crate::jury::get_few_shot_examples(db, seg_id, 5).map_err(|e| e.to_string())?;
                    let t2 = crate::jury::t2_listener::listen_and_judge(
                        &audio_b64,
                        &hypotheses,
                        &t1_evidence,
                        &few_shots,
                        &api_key,
                        &jury_model,
                        n_samples,
                    );

                    if let Some(verdict) = t2.verdict {
                        let evidence_payload = {
                            let mut payload = serde_json::json!({
                                "t2Transcript": verdict.transcript.clone(),
                                "t2Confidence": verdict.confidence,
                                "t2SelfConsistencyAgreement": verdict.self_consistency_agreement,
                                "t2Votes": verdict.votes,
                                "t2Evidence": verdict.evidence.clone(),
                            });
                            if let Some(report) = &reference_report {
                                payload["referenceSelection"] = serde_json::json!(report);
                            }
                            payload
                        };
                        let ev_json = serde_json::to_string(&evidence_payload)
                            .map_err(|e| format!("Failed to serialize T2 evidence for {seg_id}: {e}"))?;
                        db.write_segment_verdict(
                            seg_id,
                            "jury_accept",
                            Some(&verdict.transcript),
                            Some(&verdict.reason),
                            Some(ev_json.as_str()),
                            Some(verdict.confidence),
                            false,
                        )
                        .map_err(|e| e.to_string())?;
                        t2_committed += 1;
                    } else {
                        // T2 failed or no majority — human inbox
                        let reason = t2.error.unwrap_or_else(|| "T2 no majority".into());
                        db.write_segment_verdict(seg_id, "escalated", None, Some(&reason), None, None, true)
                            .map_err(|e| e.to_string())?;
                        still_escalated += 1;
                    }
                } else {
                    // Cloud disabled — escalate to human inbox
                    let reason = reference_report.as_ref().map_or_else(
                        || "T1 could not resolve; T2 disabled (cloud opt-in off)".to_string(),
                        |report| format!("{} T1 could not resolve; T2 disabled (cloud opt-in off)", report.rationale),
                    );
                    db.write_segment_verdict(seg_id, "escalated", None, Some(&reason), None, None, true)
                        .map_err(|e| e.to_string())?;
                    still_escalated += 1;
                }
            }
        }
    }

    Ok(serde_json::json!({
        "totalInput": segment_ids.len(),
        "t0AutoAccepted": t0_report.auto_accepted,
        "t0Escalated": t0_report.escalated,
        "referenceCommitted": reference_committed,
        "referenceGuarded": reference_guarded,
        "hypothesisGuarded": hypothesis_guarded,
        "t1Committed": t1_committed,
        "t2Committed": t2_committed,
        "humanInbox": still_escalated,
    }))
}

/// Open a SEPARATE connection (WAL mode) to the app's SQLite DB for jury/adjudication batches. The
/// jury may make N blocking T2 cloud calls; running it on its own connection — rather than the shared
/// `lock_db()` guard — keeps the global lock free so the UI's `get_segments` is never starved for the
/// duration of the run. Returns None when the app data dir isn't available yet.
pub(crate) fn open_jury_db_connection(app_state: &AppState) -> Option<crate::db::Database> {
    app_state
        .data_dir
        .lock()
        .ok()
        .and_then(|g| (*g).clone())
        .map(|dir| dir.join("cortex-speech.db"))
        .and_then(|p| crate::db::Database::open(p.to_string_lossy().as_ref()).ok())
}

#[tauri::command]
pub fn run_jury_pipeline(state: State<'_, AppState>, segment_ids: Vec<String>) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("run_jury_pipeline")?;
    let settings = state.lock_settings().clone();
    // Run on a separate WAL connection (see open_jury_db_connection) so the batch jury's blocking T2
    // cloud calls never hold the global lock and freeze the UI's get_segments for the whole run.
    let db = open_jury_db_connection(&state)
        .ok_or_else(|| "App data directory is unavailable for the jury run.".to_string())?;
    run_jury_pipeline_core(&db, &settings, segment_ids)
}

/// `run_t2_for_segment` — run Gemini audio judge on a single segment directly.
///
/// Useful for re-running T2 from the Review Inbox or a manual trigger without
/// going through the full pipeline again.
#[tauri::command]
pub fn run_t2_for_segment(
    state: State<'_, AppState>,
    segment_id: String,
    api_key: String,
) -> Result<crate::jury::t2_listener::T2Result, String> {
    STRICT_RATE_LIMITER.check("run_t2_for_segment")?;
    validate::validate_identifier(&segment_id)?;

    let settings = state.lock_settings().clone();
    let jury_model = settings.jury_model.clone();
    let n_samples = settings.jury_self_consistency_n as usize;
    let cloud_opt_in = settings.jury_cloud_opt_in;

    if !cloud_opt_in {
        return Err("Cloud opt-in is required for T2. Enable it in Settings → Listening Jury.".into());
    }
    if api_key.trim().is_empty() {
        return Err("Gemini API key is required for T2.".into());
    }

    let db = state.lock_db();
    let seg = db
        .get_segment_by_id(&segment_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Segment not found: {segment_id}"))?;

    // Base64-encode only the segment span for chunked long-form sources.
    let audio_b64 = crate::agentic::segment_audio_as_wav_base64(&seg)
        .map_err(|e| format!("Cannot prepare segment audio '{}': {e}", seg.audio_path))?;

    // Build a single hypothesis from raw transcript (T2 will hear the audio and judge)
    let mut hyps = db.get_hypotheses_for_segment(&segment_id).map_err(|e| e.to_string())?;
    if hyps.is_empty() {
        hyps.push(crate::db::SegmentHypothesis {
            segment_id: segment_id.clone(),
            model_id: "asr".into(),
            transcript: seg.raw_transcript.clone(),
            confidence: seg.confidence,
        });
    }

    let mut duration_cache = std::collections::HashMap::new();
    let mut identity_cache = std::collections::HashMap::new();
    let reference_report =
        reference_selection_for_segment(&db, &settings, &seg, &hyps, &mut duration_cache, &mut identity_cache)?;
    let t2_evidence = reference_report.as_ref().map(reference_selection_evidence).into_iter().collect::<Vec<_>>();

    let few_shots = crate::jury::get_few_shot_examples(&db, &segment_id, 5).map_err(|e| e.to_string())?;

    // Release the global DB lock BEFORE the blocking T2 cloud call (Gemini, n_samples retries —
    // multiple seconds): holding it across the network request would starve every other DB user
    // (e.g. the UI's get_segments) for the whole call — the same lock-across-blocking-work class as
    // the jury-adjudication fix. All reads above are done; the verdict write below re-acquires.
    drop(db);

    let result = crate::jury::t2_listener::listen_and_judge(
        &audio_b64,
        &hyps,
        &t2_evidence,
        &few_shots,
        &api_key,
        &jury_model,
        n_samples,
    );

    // If T2 produced a verdict, write it to the DB automatically
    if let Some(ref verdict) = result.verdict {
        let evidence_payload = match &reference_report {
            Some(report) => serde_json::json!({
                "t2Evidence": verdict.evidence.clone(),
                "referenceSelection": report,
            }),
            None => serde_json::json!(verdict.evidence.clone()),
        };
        let ev_json = serde_json::to_string(&evidence_payload)
            .map_err(|e| format!("Failed to serialize T2 evidence for {segment_id}: {e}"))?;
        let db = state.lock_db(); // re-acquire the lock only to persist the verdict
        db.write_segment_verdict(
            &segment_id,
            "jury_accept",
            Some(&verdict.transcript),
            Some(&verdict.reason),
            Some(ev_json.as_str()),
            Some(verdict.confidence),
            false,
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(result)
}

/// `update_segment_bounds` — updates the start and end timestamps (in milliseconds)
/// of a speech segment in the database, adjusting duration and alignment metadata.
#[tauri::command]
pub fn update_segment_bounds(id: String, start_ms: i64, end_ms: i64, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("update_segment_bounds")?;
    validate::validate_identifier(&id)?;

    if start_ms < 0 || end_ms < 0 || start_ms >= end_ms {
        return Err("Invalid segment bounds".to_string());
    }

    let db = state.lock_db();
    let mut segment =
        db.get_segment_by_id(&id).map_err(|e| e.to_string())?.ok_or_else(|| format!("Segment not found: {id}"))?;

    let mut meta = if let Some(ref alignment_str) = segment.alignment_json {
        crate::chunking::SegmentSourceMeta::from_alignment_json(alignment_str).unwrap_or(
            crate::chunking::SegmentSourceMeta {
                source_start_ms: start_ms,
                source_end_ms: end_ms,
                chunk_index: 0,
                chunk_count: 1,
            },
        )
    } else {
        crate::chunking::SegmentSourceMeta {
            source_start_ms: start_ms,
            source_end_ms: end_ms,
            chunk_index: 0,
            chunk_count: 1,
        }
    };

    meta.source_start_ms = start_ms;
    meta.source_end_ms = end_ms;

    segment.alignment_json = Some(meta.to_alignment_json());
    segment.duration_ms = end_ms - start_ms;

    let history = state.lock_history();
    crate::history::HistoryManager::persist_segment_update(&db, &history, &segment).map_err(|e| e.to_string())?;
    drop(history);
    drop(db);

    state.session_auto_save();
    Ok(())
}

/// Minimal base64 encoder — avoids pulling in a full base64 crate.
/// Uses the standard alphabet (RFC 4648).
pub fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 { chunk[1] as usize } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as usize } else { 0 };
        out.push(CHARS[b0 >> 2] as char);
        out.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        out.push(if chunk.len() > 1 { CHARS[((b1 & 0xF) << 2) | (b2 >> 6)] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[b2 & 0x3F] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_segment(id: &str, audio_path: &str, raw_transcript: &str) -> crate::db::SpeechSegment {
        crate::db::SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: audio_path.to_string(),
            raw_transcript: raw_transcript.to_string(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 1000,
            speaker_id: None,
            verified: false,
            confidence: Some(0.99),
            ctc_score: Some(0.0),
            clipping_ratio: Some(0.0),
            rms_db: Some(-20.0),
            snr_db: Some(25.0),
            split: None,
            ood_score: None,
            ..crate::db::SpeechSegment::default()
        }
    }

    fn insert_hypothesis(
        db: &crate::db::Database,
        segment_id: &str,
        model_id: &str,
        transcript: &str,
        confidence: f64,
    ) {
        db.insert_hypothesis(&crate::db::SegmentHypothesis {
            segment_id: segment_id.to_string(),
            model_id: model_id.to_string(),
            transcript: transcript.to_string(),
            confidence: Some(confidence),
        })
        .unwrap();
    }

    fn insert_source_reference(db: &crate::db::Database, audio_path: &str, text: &str) {
        insert_source_reference_with_model(db, audio_path, "gemini-2.5-pro", text);
    }

    fn insert_source_reference_with_model(db: &crate::db::Database, audio_path: &str, model_id: &str, text: &str) {
        let identity = crate::pipeline::source_audio_identity(Path::new(audio_path)).ok();
        db.upsert_source_transcript(&crate::db::SourceTranscriptRecord {
            audio_path: audio_path.to_string(),
            model_id: model_id.to_string(),
            audio_content_hash: identity.as_ref().map(|identity| identity.content_hash.clone()),
            audio_size_bytes: identity.as_ref().map(|identity| identity.size_bytes),
            transcript_path: format!("{model_id}.source_reference.txt"),
            transcript_text: text.to_string(),
            created_at: None,
        })
        .unwrap();
    }

    fn test_source_audio(dir: &tempfile::TempDir, name: &str) -> String {
        let audio = dir.path().join(name);
        std::fs::write(&audio, format!("source-audio-bytes:{name}")).unwrap();
        audio.to_string_lossy().to_string()
    }

    fn settings_with_source_reference_models(models: &[&str]) -> crate::settings::AppSettings {
        crate::settings::AppSettings {
            source_reference_models: models.iter().map(|model| (*model).to_string()).collect(),
            ..crate::settings::AppSettings::default()
        }
    }

    fn downloaded_model_status(filename: &str) -> serde_json::Value {
        serde_json::json!({
            "filename": filename,
            "downloaded": true,
        })
    }

    #[test]
    fn agentic_readiness_blocks_when_source_reference_and_hypothesis_coverage_are_missing() {
        let readiness = build_agentic_readiness(
            &crate::settings::AppSettings::default(),
            &[],
            &serde_json::json!({
                "available": false,
                "message": "No external ASR provider script configured"
            }),
        );

        assert_eq!(readiness.status, "blocked");
        assert!(!readiness.ready);
        assert!(readiness.checks.iter().any(|check| check.id == "source_reference" && check.status == "blocked"));
        assert!(readiness.checks.iter().any(|check| check.id == "hypothesis_coverage" && check.status == "blocked"));
        assert!(readiness.available_hypothesis_models.is_empty());
        assert_eq!(readiness.required_hypothesis_models, quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE);
    }

    #[test]
    fn agentic_readiness_is_ready_with_gemini_wsl_and_local_hypothesis_model() {
        let settings = crate::settings::AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: "session-key".to_string(),
            external_asr_script_path: "/root/cortex_env/omniasr.py".to_string(),
            source_reference_models: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
            ..crate::settings::AppSettings::default()
        };
        let model_status = vec![
            downloaded_model_status(models::OMNIASR_CTC_300M_MODEL),
            downloaded_model_status(models::OMNIASR_CTC_300M_TOKENS),
        ];
        let readiness = build_agentic_readiness(
            &settings,
            &model_status,
            &serde_json::json!({
                "available": true,
                "script": "/root/cortex_env/omniasr.py",
                "message": "WSL is available; provider script will be used for external ASR"
            }),
        );

        assert_eq!(readiness.status, "ready");
        assert!(readiness.ready);
        assert_eq!(
            readiness.available_hypothesis_models,
            vec!["omniasr-wsl-7b".to_string(), "omniasr-ctc-300m".to_string()]
        );
        assert!(readiness.checks.iter().all(|check| check.status == "ready"));
    }

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }

    #[test]
    fn wsl_log_preview_keeps_short_lines_unchanged() {
        assert_eq!(wsl_log_preview("ready"), "ready");
    }

    #[test]
    fn wsl_log_preview_caps_long_lines() {
        let long = format!("{}{}", "x".repeat(WSL_LOG_LINE_PREVIEW_CHARS), "extra");

        let preview = wsl_log_preview(&long);

        assert!(preview.contains("[truncated WSL log line]"));
        assert!(!preview.contains("extra"));
        assert_eq!(preview.split(' ').next().unwrap().len(), WSL_LOG_LINE_PREVIEW_CHARS);
    }

    #[test]
    fn wsl_log_preview_caps_without_splitting_kurdish_chars() {
        let long = format!("{}{}", "ژ".repeat(WSL_LOG_LINE_PREVIEW_CHARS), "extra");

        let preview = wsl_log_preview(&long);

        assert!(preview.contains("[truncated WSL log line]"));
        assert!(!preview.contains("extra"));
        assert_eq!(preview.split(' ').next().unwrap().chars().count(), WSL_LOG_LINE_PREVIEW_CHARS);
    }

    #[test]
    fn source_reference_commit_runs_before_t0_auto_accept() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "source.wav");
        let segment = test_segment("seg-reference-first", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "wrong local consensus", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "wrong local consensus", 0.95);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "correct reference phrase", 0.90);
        insert_source_reference(&db, &audio_path, "correct reference phrase");

        let settings = settings_with_source_reference_models(&["gemini-2.5-pro"]);
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(fresh.verdict.as_deref(), Some("jury_accept"));
        assert_eq!(fresh.verdict_transcript.as_deref(), Some("correct reference phrase"));
        let evidence = fresh.evidence_json.as_deref().unwrap_or("");
        assert!(evidence.contains("referenceModelId"));
        assert!(evidence.contains("gemini-2.5-pro"));
    }

    #[test]
    fn agreeing_source_references_preserve_per_model_evidence() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "agreeing-references.wav");
        let segment = test_segment("seg-reference-agreement", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "correct reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "correct reference phrase");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "correct reference phrase");

        let report =
            run_jury_pipeline_core(&db, &crate::settings::AppSettings::default(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("jury_accept"));
        assert_eq!(fresh.verdict_transcript.as_deref(), Some("correct reference phrase"));
        let evidence: serde_json::Value =
            serde_json::from_str(fresh.evidence_json.as_deref().expect("reference evidence json")).unwrap();
        assert_eq!(
            evidence.get("referenceModelId").and_then(serde_json::Value::as_str),
            Some("multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash")
        );
        let agreement = evidence.get("referenceAgreement").and_then(serde_json::Value::as_array).unwrap();
        assert_eq!(agreement.len(), 2);
        assert!(agreement.iter().any(|item| item["referenceModelId"] == "gemini-2.5-pro"));
        assert!(agreement.iter().any(|item| item["referenceModelId"] == "gemini-2.5-flash"));
    }

    #[test]
    fn source_reference_guard_blocks_t0_and_t1_auto_commit_when_inconclusive() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "guarded.wav");
        let segment = test_segment("seg-reference-guard", &audio_path, "fluent local phrase");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "fluent local phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "fluent local phrase", 0.95);
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "unrelated source context");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "unrelated source context");

        let report =
            run_jury_pipeline_core(&db, &crate::settings::AppSettings::default(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        assert!(fresh.rationale.as_deref().unwrap_or("").contains("T2 disabled"));
    }

    #[test]
    fn incomplete_source_reference_model_coverage_blocks_auto_commit() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "incomplete-reference-coverage.wav");
        let segment = test_segment("seg-incomplete-reference-coverage", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "correct reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "correct reference phrase");

        let report =
            run_jury_pipeline_core(&db, &crate::settings::AppSettings::default(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Source-reference coverage guard blocked automatic adjudication"));
        assert!(rationale.contains("gemini-2.5-flash"));
        assert!(rationale.contains("T2 disabled"));
    }

    #[test]
    fn stale_source_reference_audio_identity_blocks_automatic_jury_commit() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio = dir.path().join("same-path-source.wav");
        std::fs::write(&audio, b"current-audio-bytes").unwrap();
        let audio_path = audio.to_string_lossy().to_string();
        let segment = test_segment("seg-stale-source-reference", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "stale reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        db.upsert_source_transcript(&crate::db::SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: Some("old-audio-content-hash".to_string()),
            audio_size_bytes: Some(1),
            transcript_path: "gemini-2.5-pro.source_reference.txt".to_string(),
            transcript_text: "stale reference phrase".to_string(),
            created_at: None,
        })
        .unwrap();

        let settings = settings_with_source_reference_models(&["gemini-2.5-pro"]);
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Source-reference audio identity guard blocked automatic adjudication"));
        assert!(rationale.contains("gemini-2.5-pro"));
        assert!(rationale.contains("T2 disabled"));
    }

    #[test]
    fn missing_source_reference_audio_identity_blocks_automatic_jury_commit() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "legacy-source-reference.wav");
        let segment = test_segment("seg-legacy-source-reference", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "legacy reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        db.upsert_source_transcript(&crate::db::SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: None,
            audio_size_bytes: None,
            transcript_path: "gemini-2.5-pro.source_reference.txt".to_string(),
            transcript_text: "legacy reference phrase".to_string(),
            created_at: None,
        })
        .unwrap();

        let settings = settings_with_source_reference_models(&["gemini-2.5-pro"]);
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Source-reference audio identity guard blocked automatic adjudication"));
        assert!(rationale.contains("gemini-2.5-pro"));
        assert!(rationale.contains("T2 disabled"));
    }

    #[test]
    fn incomplete_hypothesis_model_coverage_blocks_t0_and_t1_auto_commit() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let audio_path = "/audio/one-hypothesis.wav";
        let segment = test_segment("seg-one-hypothesis", audio_path, "fluent single model phrase");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "fluent single model phrase", 0.99);

        let report =
            run_jury_pipeline_core(&db, &crate::settings::AppSettings::default(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["hypothesisGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Multi-model hypothesis coverage guard blocked automatic adjudication"));
        assert!(rationale.contains("fewer than 2 non-empty model hypotheses"));
        assert!(rationale.contains("omniasr-ctc-300m"));
        assert!(rationale.contains("T2 disabled"));
    }

    #[test]
    fn source_reference_disagreement_blocks_automatic_commit() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "conflicting-references.wav");
        let segment = test_segment("seg-reference-conflict", &audio_path, "fluent local phrase");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "first correct phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "second correct phrase", 0.98);
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "first correct phrase");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "second correct phrase");

        let report =
            run_jury_pipeline_core(&db, &crate::settings::AppSettings::default(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        assert!(fresh.rationale.as_deref().unwrap_or("").contains("T2 disabled"));
    }
}
