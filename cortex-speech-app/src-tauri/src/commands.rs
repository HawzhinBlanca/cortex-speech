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
use crate::db::{SegmentsPage, SpeechSegment};
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
        // Cloud whole-file references are an OPTIONAL enhancement the user has deliberately left off
        // (offline-first). The jury runs local multi-model consensus fine without them, so this is the
        // chosen, fully-functional configuration — "ready", NOT a degradation that nags on every import.
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "ready",
            "Offline mode: cloud whole-file references are off by choice (an optional cross-check; whether local consensus can run is reported by the hypothesis-coverage check). Enable jury cloud opt-in to add Gemini whole-file references.",
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
pub async fn import_directory(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("import_directory")?;
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
            pipeline.import_directory_with_agent_run_id(&dir_path, cancel, Some(&agent_run_id), None, |event| {
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
    let db = state.lock_db();
    db.find_interrupted_import_job().map_err(|e| e.to_string())
}

/// P3.2: discard an interrupted import job (the user chose not to resume).
#[tauri::command]
pub fn discard_interrupted_import(job_id: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("discard_interrupted_import")?;
    validate::validate_identifier(&job_id)?;
    let db = state.lock_db();
    db.discard_import_job(&job_id).map_err(|e| e.to_string())
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
    let job = {
        let db = state.lock_db();
        db.find_interrupted_import_job().map_err(|e| e.to_string())?
    };
    let Some(job) = job else { return Err("No interrupted import to resume".into()) };
    let dir_path = std::path::PathBuf::from(&job.dir);
    if !dir_path.is_dir() {
        return Err(format!("The import folder no longer exists: {}", job.dir));
    }
    let completed: std::collections::HashSet<String> = job.completed_paths.into_iter().collect();

    state.try_start_import()?;
    {
        // Retire the crashed job now that we are resuming it; the fresh import job supersedes it.
        let db = state.lock_db();
        let _ = db.discard_import_job(&job.id);
    }
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
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.import_directory_with_agent_run_id(
                &dir_path,
                cancel,
                Some(&agent_run_id),
                Some(&completed),
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
    Ok(serde_json::json!({ "status": "started", "resuming": true }))
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
    let db = state.lock_db();
    let mm = state.lock_model_manager();
    health::health_check(&db, &mm, data_dir.as_deref()).map_err(|e| e.to_string())
}

/// Return a one-line summary of the most recent crash report (if the last session panicked), surfaced
/// exactly once. The frontend shows it as a notification on startup so a mid-review crash — after which
/// the app relaunches looking normal — is no longer silent. The full report stays in the rolling log.
#[tauri::command]
pub fn take_last_crash(state: State<'_, AppState>) -> Option<String> {
    let data_dir = state.lock_data_dir().clone()?;
    crate::crash::take_latest_crash_summary(&data_dir)
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

/// Decode ONLY a segment's clip window to 16 kHz mono PCM without loading the whole (possibly
/// multi-hour) source file. Reads just the [source_start_ms, source_end_ms] span via the incremental
/// window decoder, early-stopping once past the window. Falls back to a whole-file decode when there is
/// no window (a single-segment file, which is small). Fixes the per-clip fine-tuned re-transcribe
/// failing with "audio too large to decode in one pass" on long recordings.
pub(crate) fn decode_finetuned_clip_16k(
    audio_path: &str,
    alignment_json: Option<&str>,
    // ALIGNMENT PRESENT BUT OFFSET-LESS is ambiguous: a legitimately-aligned SINGLE-segment file
    // carries `{"words":[...]}` with no source offsets (whole-file decode is CORRECT), while a
    // clobbered CHUNK of a multi-segment source carries the same shape (whole-file decode ships the
    // entire recording as one "clip" — the true-10 audit's training-data-corruption major, the exact
    // shape the HF/audio exporters refuse). Only the CALLER can tell them apart, by whether the
    // source has sibling segments in the DB — so it must say whether whole-file is permitted.
    allow_whole_file_without_offsets: bool,
) -> Result<Vec<i16>, String> {
    let whole_file = || -> Result<Vec<i16>, String> {
        let (rate, pcm) = crate::audio::decode_to_pcm(audio_path).map_err(|e| e.to_string())?;
        let (_r, pcm16) = crate::audio::ensure_pcm_16khz(rate, pcm).map_err(|e| e.to_string())?;
        Ok(pcm16)
    };
    let Some(alignment) = alignment_json.map(str::trim).filter(|a| !a.is_empty()) else {
        // Truly absent alignment: a single-segment import — whole file is the clip.
        return whole_file();
    };
    let Some(meta) = crate::chunking::SegmentSourceMeta::from_alignment_json(alignment) else {
        if allow_whole_file_without_offsets {
            return whole_file();
        }
        return Err("alignment_json is present but carries no source offsets (a legacy/clobbered \
                    word-array blob) on a multi-segment source — refusing the whole-file fallback; \
                    re-import the file through the current build to regenerate its slice offsets"
            .to_string());
    };
    let start_ms = meta.source_start_ms.max(0);
    let end_ms = meta.source_end_ms.max(start_ms);
    let mut native: Vec<i16> = Vec::new();
    let mut rate = crate::audio::TARGET_SAMPLE_RATE;
    const CLIP_DONE: &str = "__clip_window_done__";
    let res = crate::audio::decode_pcm_windows(audio_path, 30_000, |win| {
        rate = win.sample_rate.max(1);
        let win_start = win.offset_ms;
        let dur_ms = (win.pcm.len() as i64 * 1000) / rate as i64;
        let win_end = win_start + dur_ms;
        if win_end > start_ms && win_start < end_ms {
            let a_ms = (start_ms.max(win_start) - win_start).max(0);
            let b_ms = (end_ms.min(win_end) - win_start).max(0);
            let a = ((a_ms * rate as i64) / 1000) as usize;
            let b = (((b_ms * rate as i64) / 1000) as usize).min(win.pcm.len());
            if b > a {
                native.extend_from_slice(&win.pcm[a..b]);
            }
        }
        // Past the clip window — stop early rather than decode the rest of a long file (sentinel Err).
        if win_start >= end_ms {
            return Err(crate::error::AppError::Other(CLIP_DONE.to_string()));
        }
        Ok(())
    });
    match res {
        Ok(()) => {}
        Err(crate::error::AppError::Other(m)) if m == CLIP_DONE => {}
        Err(e) => return Err(e.to_string()),
    }
    let (_r, pcm16) = crate::audio::ensure_pcm_16khz(rate, native).map_err(|e| e.to_string())?;
    Ok(pcm16)
}

/// Resolve the fine-tuned model + vocab paths: `CORTEX_FINETUNED_ONNX`/`CORTEX_FINETUNED_VOCAB`
/// (dev/testing) or `finetuned-mms-ckb/{model.onnx,vocab.json}` in the active then bundled models dir.
fn resolve_finetuned_paths() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    if let (Ok(o), Ok(v)) = (std::env::var("CORTEX_FINETUNED_ONNX"), std::env::var("CORTEX_FINETUNED_VOCAB")) {
        return Ok((std::path::PathBuf::from(o), std::path::PathBuf::from(v)));
    }
    for base in [crate::models::active_models_dir(), crate::models::bundled_models_dir()] {
        let dir = base.join("finetuned-mms-ckb");
        let onnx = dir.join("model.onnx");
        let vocab = dir.join("vocab.json");
        if onnx.exists() && vocab.exists() {
            return Ok((onnx, vocab));
        }
    }
    Err("fine-tuned model not found (models/finetuned-mms-ckb/{model.onnx,vocab.json})".to_string())
}

/// P3.4: verify the bundled fine-tuned model's integrity — the DEFINITIVE full model.onnx + vocab.json
/// SHA-256 (hashes the full ~970 MB, so it is on demand, not per-load). The fast size+vocab guard runs
/// at every model load. Returns a confirmation string or a mismatch error.
#[tauri::command]
pub fn verify_finetuned_model_integrity() -> Result<String, String> {
    STRICT_RATE_LIMITER.check("verify_finetuned_model_integrity")?;
    let (onnx, vocab) = resolve_finetuned_paths()?;
    crate::wav2vec2_asr::verify_finetuned_full(&onnx, &vocab)?;
    Ok(format!("verified: {}", onnx.display()))
}

/// Opt-in: transcribe a segment with the fine-tuned Kurdish Wav2Vec2-CTC model (ONNX via `ort`),
/// which roughly halves CER vs the stock OmniASR path (see docs/EVAL.md). Additive — the default
/// `transcribe_segment` path is unchanged. Resolves the model from `CORTEX_FINETUNED_ONNX` +
/// `CORTEX_FINETUNED_VOCAB` (dev/testing) or `<models>/finetuned-mms-ckb/{model.onnx,vocab.json}`.
#[tauri::command]
pub fn transcribe_segment_finetuned(
    audio_path: String,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("transcribe_segment_finetuned")?;
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    let (onnx, vocab) = resolve_finetuned_paths()?;
    // Offset-less alignment is only whole-file-safe when this source has NO sibling chunks (see
    // decode_finetuned_clip_16k): on a multi-segment source it would silently re-transcribe the
    // entire recording into one segment (true-10 audit 2026-07-09).
    let sibling_count: i64 = {
        let db = state.lock_db();
        db.connection()
            .query_row(
                "SELECT COUNT(*) FROM speech_segments WHERE audio_path = ?1",
                rusqlite::params![audio_path],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?
    };
    // Decode ONLY this segment's clip window (16 kHz). Every VAD chunk shares the whole-source
    // audio_path with its range in alignment_json; decoding the WHOLE source first (the old path) failed
    // on long recordings ("audio too large to decode in one pass") even though the clip is a few
    // seconds. The windowed decode reads just the clip, so re-transcribe works on any source length.
    let clip = decode_finetuned_clip_16k(&audio_path, alignment_json.as_deref(), sibling_count <= 1)?;
    // Route through the pipeline's shared windowed wrapper, NOT run_wav2vec2 directly: the model is
    // trained on short utterances and a single unbounded pass over >15 s (a raised max-segment
    // setting, or a whole-file/no-meta clip) duplicates text — the pipeline path sub-splits into
    // ~15 s windows for exactly that reason (true-10 audit 2026-07-09: one engine, two call paths,
    // two different qualities).
    let text = crate::pipeline::ProcessingPipeline::transcribe_chunk_finetuned(&onnx, &vocab, &clip)?;
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
    let draft = pipeline
        .transcribe(segment_id.as_deref(), &audio_path, alignment_json.as_deref(), None)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "text": draft.final_text,
        "rawTranscript": draft.raw_text,
        "confidence": draft.confidence,
        "confidenceSource": draft.confidence_source,
        "modelVersionId": draft.model_version_id,
        "cloudCall": draft.cloud_call
    }))
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
        let mut skipped = 0u32;
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
                cancelled = true;
                break;
            }

            let Some(app_state) = app_clone.try_state::<AppState>() else {
                break;
            };
            // Use the pre-fetched normalizer (avoids re-cloning Arc on every iteration).
            let normalizer = normalizer_arc.as_ref().unwrap_or_else(|| &app_state.normalizer);

            let seg = seg_map.remove(id.as_str());

            if let Some(seg) = seg {
                // Capture full snapshot BEFORE transcription for complete undo.
                let pre_transcription_snapshot = seg.clone();
                match pipeline.transcribe(Some(id), &seg.audio_path, seg.alignment_json.as_deref(), None) {
                    Ok(draft) => {
                        let normalized = normalizer.normalize(&draft.final_text);
                        // Guarded targeted write (NOT a full insert_segment of the stale snapshot): a
                        // human may have verified/edited this row since the batch prefetched it. This
                        // writes only the ASR fields, seeds the annotation solely when still empty
                        // (against the CURRENT row), never touches `verified`, and skips human-owned
                        // rows — so a concurrent curator decision can never be silently lost.
                        match app_state.lock_db().update_batch_transcription_if_unreviewed(
                            id,
                            &draft.raw_text,
                            Some(normalized.as_str()),
                            draft.confidence,
                            draft.confidence_source.as_deref(),
                            draft.model_version_id.as_deref(),
                            draft.cloud_call,
                            &draft.final_text,
                        ) {
                            Ok(true) => {
                                previous_segments.push(pre_transcription_snapshot);
                                transcribed_ids.push(id.clone());
                                succeeded += 1;
                            }
                            Ok(false) => {
                                // Row became human-verified/reviewed after the batch began — skip
                                // rather than overwrite the curator's confirmed label.
                                tracing::info!("Batch transcribe skipped {id}: human-reviewed since batch start");
                                skipped += 1;
                            }
                            Err(error) => {
                                tracing::error!("Batch transcribe DB update failed for {id}: {error}");
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
                // Dedicated connection (not the shared lock_db guard) so the post-batch jury's
                // possible T2 cloud calls don't hold the global db Mutex and starve the UI's
                // get_segments while it runs. with_jury_db retries the dedicated open and only falls
                // back to the shared handle on a hard failure (so a transient lock doesn't skip the
                // jury entirely).
                if let Err(error) =
                    with_jury_db(&app_state, |db| run_jury_pipeline_core(db, &settings, transcribed_ids))
                {
                    log_jury_pipeline_failure("batch transcription", &error);
                }
            }
        }

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "completed", "total": total,
                "succeeded": succeeded, "failed": failed, "skipped": skipped,
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
    // Persist the word timings INTO alignment_json (merged with the existing chunk metadata) AND
    // stamp the honest alignment_quality, so the per-word review features (spoken-span playback,
    // tap-a-word, karaoke highlight) survive a reload instead of vanishing every time. The quality
    // distinguishes real CTC forced alignment from the linear/energy heuristic fallback — heuristic
    // output is never labelled 'ctc_forced'.
    if let Some(ref id) = segment_id {
        if !timestamps.is_empty() {
            let merged = crate::chunking::merge_word_timestamps(alignment_json.as_deref(), &timestamps);
            let db = state.lock_db();
            db.update_segment_alignment_json(id, &merged)
                .map_err(|error| format!("Failed to persist word timings for {id}: {error}"))?;
            db.update_alignment_quality(id, quality.as_db_str())
                .map_err(|error| format!("Failed to stamp alignment quality for {id}: {error}"))?;
        }
    }
    Ok(timestamps)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentConsensus {
    /// Best-of-N draft transcript (ability-weighted vote across the segment's ASR hypotheses).
    pub draft: String,
    /// Per-word breakdown with the agreement signal + what the other models said.
    pub words: Vec<crate::quality::irt::ConsensusWord>,
    pub model_count: usize,
    /// Lowest per-word agreement (0..1) — a quick "how contested is this clip" signal.
    pub min_agreement: f64,
    pub mean_agreement: f64,
    /// Distinct engine ids that produced this segment's hypotheses (e.g. "omniasr-wsl-7b",
    /// "finetuned-mms-ckb", "omniasr-ctc-300m", "scribe-v1"), in first-seen order. Drawn from the
    /// recorded hypotheses — NEVER inferred — so the review UI can honestly name which model(s)
    /// produced the draft. Empty when the segment has no recorded hypotheses (pre-provenance imports).
    pub models: Vec<String>,
}

/// Offline best-of-N consensus DRAFT for a segment: an ability-weighted vote over its ASR hypotheses
/// (no cloud) so review can start from a transcript better than any single model and highlight exactly
/// where the models disagreed. Empty when the segment has no hypotheses to vote over.
#[tauri::command]
pub fn get_segment_consensus(state: State<'_, AppState>, segment_id: String) -> Result<SegmentConsensus, String> {
    RATE_LIMITER.check("get_segment_consensus")?;
    validate::validate_identifier(&segment_id)?;
    let hyps = {
        let db = state.lock_db();
        db.get_hypotheses_for_segment(&segment_id).map_err(|e| e.to_string())?
    };
    // Distinct producing engines, in first-seen order, straight from the recorded hypotheses (never
    // inferred) so the review badge can honestly say which model(s) made the draft.
    let mut models: Vec<String> = Vec::new();
    for h in &hyps {
        let id = h.model_id.trim();
        if !id.is_empty() && !models.iter().any(|m| m == id) {
            models.push(id.to_string());
        }
    }
    let words = crate::quality::irt::segment_consensus_words(&hyps);
    let draft = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    let model_count = words.first().map(|w| w.total_models).unwrap_or(0);
    let (min_agreement, mean_agreement) = if words.is_empty() {
        (0.0, 0.0)
    } else {
        let min = words.iter().map(|w| w.agreement).fold(f64::INFINITY, f64::min);
        let mean = words.iter().map(|w| w.agreement).sum::<f64>() / words.len() as f64;
        (min, mean)
    };
    Ok(SegmentConsensus { draft, words, model_count, min_agreement, mean_agreement, models })
}

#[tauri::command]
pub fn get_segments(verified: Option<bool>, state: State<'_, AppState>) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_segments")?;
    let db = state.lock_db();
    db.get_segments(verified).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_segments_page(
    verified: Option<bool>,
    query: Option<String>,
    sort: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
    state: State<'_, AppState>,
) -> Result<SegmentsPage, String> {
    RATE_LIMITER.check("get_segments_page")?;
    if let Some(ref query) = query {
        validate::validate_text(query, 1000, "Search query")?;
    }
    let sort = sort.unwrap_or_else(|| "newest".to_string());
    validate::validate_text(&sort, 64, "Segment sort")?;
    match sort.as_str() {
        "newest" | "oldest" | "duration" | "verified" | "confidence" | "activeLearning" | "active_learning"
        | "suspectFirst" | "suspect_first" => {}
        _ => return Err(format!("Invalid segment sort: {sort}")),
    }
    if let Some(ref cursor) = cursor {
        validate::validate_text(cursor, 32, "Segment page cursor")?;
        if !cursor.chars().all(|ch| ch.is_ascii_digit()) {
            return Err("Invalid segment page cursor".to_string());
        }
    }
    let limit = limit.unwrap_or(200).clamp(1, 500);
    let db = state.lock_db();
    db.get_segments_page(verified, query.as_deref(), &sort, limit, cursor.as_deref()).map_err(|e| e.to_string())
}

/// M2.5: Return segments ordered by suspect-first priority: escalated + low confidence first.
/// Priority: 1) Jury escalated, 2) Low agent confidence, 3) Chronological.
#[tauri::command]
pub fn get_segments_suspect_first(
    verified: Option<bool>,
    state: State<'_, AppState>,
) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_segments_suspect_first")?;
    let db = state.lock_db();
    db.get_segments_suspect_first(verified).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_segments(query: String, state: State<'_, AppState>) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("search_segments")?;
    // Bound the free-text query like every other text-accepting command (save_session caps its
    // search_query at 1000): an unbounded multi-MB string otherwise reaches the FTS5 MATCH parser.
    validate::validate_text(&query, 1000, "Search query")?;
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

/// Export a plain, human-facing transcript / subtitle file (txt | srt | vtt) from the library —
/// distinct from the ML dataset export. Path is validated the same way; unknown formats fall to txt.
#[tauri::command]
pub fn export_transcript(path: String, format: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("export_transcript")?;
    let validated_path = validate::validate_output_path(&path)?;
    let fmt = crate::transcript_export::TranscriptFormat::from_str_lossy(&format);
    let db = state.lock_db();
    crate::transcript_export::export_transcript(&db, Path::new(&validated_path), fmt).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    /// The champion (OmniASR-7B) warm server is answering on its WSL loopback port.
    pub ready: bool,
    pub port: u16,
}

/// Bounded (~5s) health check of the champion 7B engine, for the UI status pill. Cheap + side-effect
/// free (a TCP probe), so the frontend can poll it.
#[tauri::command]
pub fn get_champion_engine_status() -> EngineStatus {
    EngineStatus { ready: crate::pipeline::probe_wsl_7b_server(3), port: crate::pipeline::WSL_7B_SERVER_PORT }
}

/// Start the champion 7B server (WSL) FROM THE APP so the owner never hand-runs a terminal. Spawns
/// the committed start script DETACHED and returns immediately; the UI then polls
/// get_champion_engine_status until ready (warm-up loads ~30 GB, 1-5 min). The script path comes from
/// CORTEX_7B_START_SCRIPT (the desktop launcher sets it); without it we return an actionable error
/// rather than guess a path.
#[tauri::command]
pub fn start_champion_engine() -> Result<(), String> {
    STRICT_RATE_LIMITER.check("start_champion_engine")?;
    let script = std::env::var("CORTEX_7B_START_SCRIPT").map_err(|_| {
        "Set CORTEX_7B_START_SCRIPT to the full path of scripts/start_7b_server.ps1 (the desktop \
         launcher sets it), or run that script once from a terminal."
            .to_string()
    })?;
    if !Path::new(&script).is_file() {
        return Err(format!("CORTEX_7B_START_SCRIPT does not point at a file: {script}"));
    }
    let mut cmd = std::process::Command::new("powershell");
    cmd.arg("-NoProfile").arg("-ExecutionPolicy").arg("Bypass").arg("-File").arg(&script);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // Detach: the start script waits up to 8 min for warm-up; do NOT block the command on it.
    cmd.spawn().map(|_child| ()).map_err(|e| format!("could not launch the engine start script: {e}"))
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
    // Hash the (potentially multi-GB) checkpoint BEFORE taking the DB lock — holding the global db
    // mutex across the full-file SHA-256 would starve every UI DB poll (get_segments, search, …).
    let sha = crate::registry::hash_checkpoint(&checkpoint_path).map_err(|e| e.to_string())?;
    let db = state.lock_db();
    crate::registry::register_checkpoint(&db, &id, &family, &checkpoint_path, &source, &license, model_card_name, sha)
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
    let mut registry = state.lock_media_registry();
    // Round-25 #7: validate the source (membership check) under a SHORT-LIVED global db lock (fast),
    // then DROP that lock before the potentially multi-GB cache copy in grant_source — holding the
    // global db mutex across std::fs::copy froze every other DB-touching IPC (notably the UI's
    // get_segments) for the length of the copy the first time a large clip was played. The
    // media-registry lock is held throughout (only media commands take it), so this never deadlocks
    // with the db lock.
    let canonical = {
        let db = state.lock_db();
        registry.validate_source(&db, &audio_path)?
    };
    registry.grant_source(&data_dir, canonical)
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
    // Round-22 #7: carry the in-session secret forward and capture the on-disk path WITHOUT yet
    // overwriting the in-memory copy. Persisting BEFORE committing to memory/pipeline means a save
    // failure (full/read-only/locked disk) leaves the in-memory settings, the running pipeline, AND
    // disk all consistent at the OLD value — never a three-way divergence where get_settings() reports
    // an unsaved change (including cloud-consent toggles) that the pipeline and the next launch ignore.
    let settings_path = {
        let current = state.lock_settings();
        settings.merge_session_secret_from(&current);
        state.lock_data_dir().clone().map(|d| d.join("settings.json"))
    };
    // Persist FIRST, before committing to memory/pipeline. A save failure (e.g. a cloud-consent
    // toggle that never reached disk) must be SURFACED, not swallowed — otherwise the user believes
    // the change stuck while it silently reverts on the next launch (a privacy hazard for the cloud
    // opt-in toggles).
    if let Some(path) = settings_path {
        // Propagate a persist failure to the caller (the frontend surfaces settingsSaveFailed). On Err
        // we return BEFORE the commit below, so nothing observable changed: in-memory settings, the
        // running pipeline, AND disk all stay consistent at the OLD value — never a divergence where
        // get_settings() reports an unsaved change the pipeline and next launch ignore.
        settings.save(&path).map_err(|e| {
            tracing::error!("Failed to save settings to {path:?}: {e}");
            format!("Failed to save settings to disk: {e}")
        })?;
    }
    // Disk now holds the new value (or there is no data dir to persist to): commit it to the in-memory
    // store and the running pipeline together.
    *state.lock_settings() = settings.clone();
    state.update_pipeline_settings(settings);
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
pub fn save_session(
    search_query: String,
    sort_order: String,
    filter_verified: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate::validate_text(&search_query, 1000, "search_query")?;
    validate::validate_text(&sort_order, 64, "sort_order")?;
    let db = state.lock_db();
    let mut session = state.lock_session();
    session.set_view_state(search_query, sort_order, filter_verified);
    session.save(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn restore_session(state: State<'_, AppState>) -> Result<Option<crate::session::SessionState>, String> {
    RATE_LIMITER.check("restore_session")?;
    let mut session = state.lock_session();
    session.restore().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn validate_dataset_cmd(state: State<'_, AppState>) -> Result<crate::validation::ValidationReport, String> {
    // Rate-limited like its read siblings: this runs a full-dataset validation scan under the db lock,
    // so an unthrottled webview loop would starve every other DB command.
    RATE_LIMITER.check("validate_dataset_cmd")?;
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
    // Decodes + re-encodes one clip per segment to disk — throttle it like every sibling export
    // command (round-22 #5: it was the lone export missing a rate-limiter, a local DoS/disk-fill gap).
    STRICT_RATE_LIMITER.check("export_audio")?;
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
pub fn db_backup(dest: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let validated = validate::validate_output_path(&dest)?;
    // Dedicated connection (true-10 audit 2026-07-09): holding the global DB mutex for the whole
    // online backup froze every DB-touching command for the full copy duration on a slow external
    // drive — the periodic snapshot path already uses its own connection; so does this now.
    let db_path = {
        let db = state.lock_db();
        db.path().to_string()
    };
    let backup_db = crate::db::Database::open(&db_path).map_err(|e| e.to_string())?;
    backup_db.backup(&validated).map_err(|e| e.to_string())?;
    // Verify the file we WROTE — an off-disk "disaster copy" that is itself bad (destination volume
    // corruption) must fail the backup NOW, not at the disaster. Read-only open: never mutate it.
    let conn = rusqlite::Connection::open_with_flags(
        &validated,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("backup written but could not be opened for verification: {e}"))?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| format!("backup written but failed verification: {e}"))?;
    if integrity != "ok" {
        return Err(format!("backup written but FAILED integrity check: {integrity}"));
    }
    let segment_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM speech_segments", [], |row| row.get(0))
        .map_err(|e| format!("backup written but could not count segments: {e}"))?;
    Ok(serde_json::json!({ "integrityOk": true, "segmentCount": segment_count }))
}

/// Shared restore precondition (true-10 audit 2026-07-09): refuse while an import/batch worker may
/// be writing, and pin a rotation-exempt copy of the CURRENT live DB first so a mis-restore of the
/// wrong snapshot is itself recoverable (previously only from a ≤10-min rolling snapshot that
/// rotated out within ~100 minutes).
fn prepare_restore(state: &State<'_, AppState>) -> Result<(), String> {
    if state.writers_active() {
        return Err("An import or batch operation is running — cancel it or let it finish before \
                    restoring. Restoring mid-write would mix pre-restore rows into the restored \
                    library and re-arm stale undo history."
            .to_string());
    }
    if let Some(data_dir) = state.lock_data_dir().clone() {
        let db = state.lock_db();
        match crate::snapshot::take_pinned_snapshot(&db, &data_dir, "prerestore", 3) {
            Ok(path) => tracing::info!("pre-restore snapshot pinned at {}", path.display()),
            Err(e) => tracing::warn!("pre-restore snapshot failed (continuing with restore): {e}"),
        }
    }
    Ok(())
}

#[tauri::command]
pub fn db_restore(src: String, state: State<'_, AppState>) -> Result<(), String> {
    // M0.4: Restore a previously backed-up database snapshot. The file must be a valid SQLite
    // database (PRAGMA integrity_check on open verifies this). This completes the backup/restore
    // pair so the app is never at risk of data loss mid-import or mid-review.
    let validated = validate::validate_file_path(&src)?;
    prepare_restore(&state)?;
    {
        let mut db = state.lock_db();
        db.restore(&validated).map_err(|e| e.to_string())?;
    } // release the db lock before taking the history lock (consistent ordering)
      // The in-memory undo/redo stack holds Commands (DeleteSegments, BatchTranscribe, ...) that
      // reference rows from the PRE-restore database. Replaying one via Undo after the restore would
      // apply a stale mutation to a different dataset — resurrecting or corrupting rows. Clear it so
      // undo can never cross the restore boundary.
    state.lock_history().clear();
    Ok(())
}

/// Release the quarantine prune-pin EXPLICITLY: archive every `*.corrupt.*` artifact into
/// `<data_dir>/quarantine/` (bytes stay salvageable via `.recover`) so pruning resumes. Previously
/// the pin had NO in-app release — snapshots accumulated a full DB copy every 10 minutes forever
/// (true-10 audit 2026-07-09). Returns how many files were archived.
#[tauri::command]
pub fn acknowledge_quarantine(state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("acknowledge_quarantine")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    crate::snapshot::acknowledge_quarantine(&data_dir).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn db_vacuum(state: State<'_, AppState>) -> Result<(), String> {
    let db = state.lock_db();
    db.vacuum().map_err(|e| e.to_string())
}

/// B2: report whether a past corruption event quarantined a database (files named
/// `cortex-speech.corrupt.<ts>` in the data dir), plus how many restore snapshots exist — so the
/// frontend can show a loud banner instead of the owner silently working in an empty library.
#[tauri::command]
pub fn get_quarantine_notice(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("get_quarantine_notice")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let mut quarantined: Vec<String> = std::fs::read_dir(&data_dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            // recover_database_at renames to `<stem>.corrupt.<ts>[.n]` (+ sidecars); count main files only.
            (name.contains(".corrupt.") && !name.ends_with("-wal") && !name.ends_with("-shm")).then_some(name)
        })
        .collect();
    quarantined.sort();
    let snapshots = crate::snapshot::list_snapshots(&data_dir);
    Ok(serde_json::json!({
        "quarantinedFiles": quarantined,
        "snapshotCount": snapshots.len(),
        "newestSnapshotSegments": snapshots.first().and_then(|s| s.segment_count),
    }))
}

/// Intelligence read-side: LOOP-0 shadow precision (the C5 go-live evidence) + auto-accept
/// precision (C4) joined against subsequent human decisions.
#[tauri::command]
pub fn get_intelligence_report(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("get_intelligence_report")?;
    let db = state.lock_db();
    db.intelligence_report().map_err(|e| e.to_string())
}

/// B2: list the rotating auto-snapshots (newest first) for the restore picker.
#[tauri::command]
pub fn list_db_snapshots(state: State<'_, AppState>) -> Result<Vec<crate::snapshot::SnapshotInfo>, String> {
    RATE_LIMITER.check("list_db_snapshots")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    Ok(crate::snapshot::list_snapshots(&data_dir))
}

/// B2: restore the live database from a named auto-snapshot. The name must be a bare
/// `snapshot_<digits>` component (no separators — cannot traverse), and the snapshot must contain a
/// DB file. Restore goes through the same SQLite online-backup path as `db_restore`.
#[tauri::command]
pub fn restore_db_from_snapshot(name: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("restore_db_from_snapshot")?;
    let valid =
        name.strip_prefix("snapshot_").is_some_and(|ts| !ts.is_empty() && ts.bytes().all(|b| b.is_ascii_digit()));
    if !valid {
        return Err(format!("invalid snapshot name '{name}'"));
    }
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let src = data_dir.join("snapshots").join(&name).join("cortex-speech.db");
    if !src.is_file() {
        return Err(format!("snapshot '{name}' has no database file"));
    }
    prepare_restore(&state)?;
    {
        let mut db = state.lock_db();
        db.restore(&src).map_err(|e| e.to_string())?;
    } // release the db lock before touching the settings/pipeline locks

    // Restore the snapshot's captured config (settings.json / champion.json) so the app returns to a
    // CONSISTENT known-good state — not a rolled-back library sitting beside post-disaster settings, or
    // silently-defaulted settings if the live settings.json was corrupted alongside the DB. Best-effort
    // per file (the DB is already restored; a config-copy failure only warns).
    // Restore exactly the set the snapshot saved (crate::snapshot::EXTRA_STATE) — single source of truth,
    // so save-side and restore-side can never drift out of sync.
    let snap_dir = data_dir.join("snapshots").join(&name);
    for &extra in crate::snapshot::EXTRA_STATE {
        let from = snap_dir.join(extra);
        if from.is_file() {
            if let Err(e) = std::fs::copy(&from, data_dir.join(extra)) {
                tracing::warn!("snapshot restore: could not restore {extra}: {e}");
            }
        }
    }
    // Apply the restored settings to memory AND the running pipeline (mirrors update_settings) so the
    // engine / thresholds / consent flags take effect immediately, not only on the next launch.
    let restored = crate::settings::AppSettings::load(&data_dir.join("settings.json"));
    *state.lock_settings() = restored.clone();
    state.update_pipeline_settings(restored);
    // Clear the undo/redo stack: it references pre-restore rows and replaying it would corrupt the
    // restored dataset (see db_restore).
    state.lock_history().clear();
    tracing::info!("database and config restored from auto-snapshot {name}");
    Ok(())
}

/// P3.3: report which distinct source audio files are missing on disk (moved/renamed/deleted).
#[tauri::command]
pub fn get_audio_health(state: State<'_, AppState>) -> Result<crate::db::AudioHealth, String> {
    RATE_LIMITER.check("get_audio_health")?;
    let db = state.lock_db();
    db.audio_health().map_err(|e| e.to_string())
}

/// P3.3: relink missing source audio by basename against a folder the owner picks.
#[tauri::command]
pub fn relink_audio(search_dir: String, state: State<'_, AppState>) -> Result<crate::db::RelinkResult, String> {
    STRICT_RATE_LIMITER.check("relink_audio")?;
    if search_dir.contains('\0') {
        return Err("Search directory contains null bytes".to_string());
    }
    let db = state.lock_db();
    db.relink_audio(std::path::Path::new(&search_dir)).map_err(|e| e.to_string())
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
    // Clone the manager (just a models_dir PathBuf) and DROP the AppState lock before the
    // multi-hundred-MB blocking download, so the model panel's status poll and the readiness /
    // acoustic-score checks aren't starved on lock_model_manager() for the whole download.
    let mm = state.lock_model_manager().clone();
    mm.download_model(model, |progress| {
        tracing::debug!("Download {} progress: {:.0}%", model.name, progress * 100.0);
    })
}

#[tauri::command]
pub fn models_download_all(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("models_download_all")?;
    // Clone the manager and DROP the AppState lock before the long download loop — otherwise every
    // missing model is fetched (hundreds of MB each) with lock_model_manager() held the whole time,
    // starving the model panel's own progress poll and the readiness/score checks.
    let mm = state.lock_model_manager().clone();
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

const WSL_LOG_LINE_PREVIEW_CHARS: usize = 4096;

/// True while a batch 7B refinement run is in flight. A plain flag (not a child handle) because the
/// batch drives the per-segment warm client in a loop — there is no single long-lived child to hold.
/// Guards against a second concurrent batch starting on top of the first.
static WSL_REFINE_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Set by `cancel_wsl_refinement`; polled between segments by the batch loop AND in-flight by the
/// per-segment spawn so a cancel stops the run within ~50 ms. Reset to false when a new batch starts.
static WSL_REFINE_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Clears the batch flags on drop so they reset even if the worker thread panics mid-batch.
/// Resetting CANCEL here (at run END) — rather than at run start — means a new run never needs a
/// start-of-run reset that could clobber a cancel racing the claim, and a late cancel can't leak
/// into the next run.
struct WslRefineRunningGuard;
impl Drop for WslRefineRunningGuard {
    fn drop(&mut self) {
        WSL_REFINE_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
        WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

fn wsl_log_preview(line: &str) -> String {
    let mut chars = line.chars();
    let mut preview: String = chars.by_ref().take(WSL_LOG_LINE_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        preview.push_str(" [truncated WSL log line]");
    }
    preview
}

/// A segment needs (re)transcription by the 7B batch when it has no usable transcript yet — empty or
/// any placeholder (`[Pending …]`, `[ASR unavailable …]`, `n/a`, `null`). Uses the same predicate as
/// the rest of the app (`quality::is_placeholder_transcript`) so the batch recovers an import that
/// failed under the local CTC engine too, not just the 7B-primary "[Pending]" case. We never target
/// a segment that already has a real transcript, so the batch can't clobber good CTC output (and
/// `update_asr_transcript_if_unreviewed` additionally refuses to overwrite a human decision).
fn segment_awaits_wsl7b(raw_transcript: &str) -> bool {
    let trimmed = raw_transcript.trim();
    trimmed.is_empty() || crate::quality::is_placeholder_transcript(trimmed)
}

/// Within-file ordering key: the chunk's source start offset (ms) parsed from `alignment_json`, or 0
/// when absent. Segments from one import share a 1-second `created_at` and are tie-broken only by a
/// random UUID, so without this the batch would process an arbitrary chunk first.
fn segment_chunk_offset_ms(segment: &crate::db::SpeechSegment) -> i64 {
    segment
        .alignment_json
        .as_deref()
        .and_then(crate::chunking::SegmentSourceMeta::from_alignment_json)
        .map(|meta| meta.source_start_ms)
        .unwrap_or(0)
}

/// Select which segments the batch 7B refinement should transcribe, honoring the panel's limits.
/// Pure (no I/O) so it is unit-testable. Drains the backlog deterministically oldest-first and, WITHIN
/// one import (segments sharing a `created_at`), in chunk order (source start offset) so `test_one`
/// and capped runs process the FIRST chunk rather than an arbitrary UUID-ordered one. `limit_files`
/// caps distinct source files; `limit_segments` caps total segments; `test_one` overrides to a single
/// segment. Returns `(segment_id, audio_path)` pairs.
fn select_wsl_refinement_targets(
    segments: &[crate::db::SpeechSegment],
    limit_files: Option<u32>,
    limit_segments: Option<u32>,
    test_one: bool,
) -> Vec<(String, String)> {
    // Pair each pending segment with its (parsed-once) chunk offset, then sort: oldest import first,
    // same file grouped, earliest chunk first, UUID only as a final stable tiebreak.
    let mut pending: Vec<(&crate::db::SpeechSegment, i64)> = segments
        .iter()
        .filter(|s| segment_awaits_wsl7b(&s.raw_transcript))
        .map(|s| (s, segment_chunk_offset_ms(s)))
        .collect();
    pending.sort_by(|(a, a_offset), (b, b_offset)| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.audio_path.cmp(&b.audio_path))
            .then_with(|| a_offset.cmp(b_offset))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut targets: Vec<(String, String)> =
        pending.iter().map(|(s, _)| (s.id.clone(), s.audio_path.clone())).collect();

    if let Some(max_files) = limit_files.map(|n| n as usize) {
        let mut kept_files: Vec<String> = Vec::new();
        targets.retain(|(_, path)| {
            if kept_files.iter().any(|p| p == path) {
                true
            } else if kept_files.len() < max_files {
                kept_files.push(path.clone());
                true
            } else {
                false
            }
        });
    }

    if test_one {
        targets.truncate(1);
    } else if let Some(max_segments) = limit_segments.map(|n| n as usize) {
        targets.truncate(max_segments);
    }

    targets
}

/// Drain a subprocess log stream line-by-line, decoding each line LOSSILY. `BufRead::lines()` yields
/// `io::Result<String>` and returns `Err(InvalidData)` for any non-UTF-8 line, so the previous
/// `lines().map_while(Result::ok)` permanently terminated the reader on the first such line —
/// silently freezing the live WSL progress feed for the rest of a (possibly hour-long) run on a
/// distro with a non-UTF-8 locale. Reading raw bytes and decoding with `from_utf8_lossy` survives
/// any input (invalid bytes become U+FFFD) so every subsequent line still reaches the feed. The
/// trailing `\r` of a `\r\n` line is trimmed. Retained (with its regression test) as the canonical
/// subprocess-log drainer; the current per-segment warm-client batch streams progress directly, so
/// it has no caller today — kept (allow(dead_code), paired with `join_wsl_log_reader`) so the
/// subprocess log path can be restored without re-deriving the non-UTF-8 contract.
#[allow(dead_code)]
fn drain_log_lines<R: std::io::BufRead>(reader: R, mut on_line: impl FnMut(&str)) {
    for line in reader.split(b'\n') {
        let Ok(bytes) = line else { break }; // genuine I/O error (not an encoding error): stop
        let text = String::from_utf8_lossy(&bytes);
        on_line(text.trim_end_matches('\r'));
    }
}

// Join a WSL subprocess log-reader thread, warning (never panicking) if it unwound. Paired with
// drain_log_lines for the subprocess-spawning log path; the per-segment warm-client batch supersedes
// the in-commands subprocess driver, so this currently has no caller here — kept (allow(dead_code))
// so the subprocess path can be restored without re-deriving it.
#[allow(dead_code)]
fn join_wsl_log_reader(thread: std::thread::JoinHandle<()>, stream: &str) {
    if thread.join().is_err() {
        tracing::warn!("WSL {stream} log reader thread panicked");
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

    // Single-run guard: claim the running flag atomically. If it was already true, a batch is in
    // flight — refuse rather than starting a second concurrent loop over the same segments.
    if WSL_REFINE_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("WSL 7B refinement batch transcription is already running.".into());
    }
    // The running flag is now OURS; every early return below MUST clear it or the guard would wedge.
    // Reset CANCEL at the START of the run (standard cancellation-token pattern) rather than trusting
    // the previous run's guard to have cleared it. The guard clears CANCEL then RUNNING as two separate
    // atomic stores, so a `cancel` that read RUNNING==true just before the guard could set CANCEL=true
    // AFTER the guard cleared it — leaking a stale cancel that would make THIS fresh batch abort
    // immediately, doing zero work, with no error surfaced. Clearing it here, now that RUNNING is
    // exclusively ours, drops that leaked value. (The only residual is a cancel landing in the tiny
    // window between the claim above and this store; that is user-recoverable by clicking cancel again,
    // whereas the leak was silent and unrecoverable.)
    WSL_REFINE_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);

    // Read everything the worker needs under the locks NOW, then release them so the long per-segment
    // loop holds no AppState lock. A 7B call can take seconds; holding a lock across the loop would
    // freeze the UI's get_segments exactly like the jury-starvation bug we already fixed. The
    // poison-recovering lock_* accessors never panic.
    let setup = {
        let settings = state.lock_settings();
        let external_script = settings.external_asr_script_path();
        let auto_normalize = settings.auto_normalize;
        let verbalize_numbers = settings.verbalize_numbers;
        drop(settings);
        external_script.map(|script| (script, auto_normalize, verbalize_numbers))
    };
    let (external_script, auto_normalize, verbalize_numbers) = match setup {
        Some(values) => values,
        None => {
            WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
            return Err("External ASR provider script is not configured in Settings.".into());
        }
    };
    let db_path = state.lock_pipeline().db_path().to_string();

    // Builder::spawn returns Err on OS thread-creation failure instead of PANICKING like thread::spawn,
    // so a failed spawn can't leave WSL_REFINE_RUNNING wedged true (the RAII guard lives inside the
    // closure and would never run on a spawn panic).
    let spawned = std::thread::Builder::new().name("wsl-7b-batch".into()).spawn(move || {
        // Clears WSL_REFINE_RUNNING + WSL_REFINE_CANCEL on every exit path, including a panic.
        let _running = WslRefineRunningGuard;
        // catch_unwind so a panic in the loop still emits a terminal wsl-status — otherwise the panel
        // would stay wedged at "Processing…" forever (it only clears `running` on a wsl-status event).
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_wsl_refinement_loop(
                &app,
                &db_path,
                &external_script,
                auto_normalize,
                verbalize_numbers,
                limit_files,
                limit_segments,
                dry_run,
                test_one,
            )
        }));
        // Carry transcribed AND failed so the UI can be honest: a run with any failures is reported
        // "completed" with failed>0 (not a clean green success), and an all-failed run is "failed".
        let (status, transcribed, failed, exit_code) = match outcome {
            Ok(Ok(summary)) if summary.cancelled => {
                ("cancelled", summary.transcribed as i64, summary.failed as i64, summary.transcribed as i64)
            }
            Ok(Ok(summary)) if summary.transcribed == 0 && summary.failed > 0 => {
                ("failed", 0, summary.failed as i64, -1)
            }
            Ok(Ok(summary)) => ("completed", summary.transcribed as i64, summary.failed as i64, summary.transcribed as i64),
            Ok(Err(message)) => {
                emit_or_log(&app, "wsl-log", format!("[ERROR] {}", wsl_log_preview(&message)));
                ("failed", 0, 0, -1)
            }
            Err(_panic) => {
                emit_or_log(&app, "wsl-log", "[ERROR] WSL 7B batch worker panicked; the run was aborted.".to_string());
                ("failed", 0, 0, -1)
            }
        };
        emit_or_log(
            &app,
            "wsl-status",
            serde_json::json!({ "status": status, "transcribed": transcribed, "failed": failed, "exit_code": exit_code }),
        );
    });
    if let Err(error) = spawned {
        WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        return Err(format!("Failed to start the WSL 7B batch worker thread: {error}"));
    }

    Ok(serde_json::json!({ "status": "started" }))
}

struct WslRefinementSummary {
    transcribed: usize,
    failed: usize,
    cancelled: bool,
}

/// The detached batch worker: drive the per-segment warm 7B client over every pending segment, write
/// each result through the human-decision-safe update, and stream progress as `wsl-log` events. No
/// AppState lock is held here — it owns its own DB connection opened from `db_path`.
#[allow(clippy::too_many_arguments)]
fn run_wsl_refinement_loop(
    app: &tauri::AppHandle,
    db_path: &str,
    external_script: &str,
    auto_normalize: bool,
    verbalize_numbers: bool,
    limit_files: Option<u32>,
    limit_segments: Option<u32>,
    dry_run: bool,
    test_one: bool,
) -> Result<WslRefinementSummary, String> {
    emit_or_log(
        app,
        "wsl-log",
        ">>> Driving the Meta OmniASR 7B warm client over pending segments (one --segment-id call each)...".to_string(),
    );

    // Worker connection (background thread): plain `open`, NOT open_with_retry — the boot-time-only
    // destructive quarantine must not be reachable from a live worker, and the DB was integrity-checked
    // at boot. `open` sets WAL + busy_timeout for contention.
    let db = crate::db::Database::open(db_path).map_err(|e| e.to_string())?;
    let segments = db.get_segments(None).map_err(|e| e.to_string())?;
    let targets = select_wsl_refinement_targets(&segments, limit_files, limit_segments, test_one);

    if targets.is_empty() {
        emit_or_log(
            app,
            "wsl-log",
            ">>> No segments are awaiting 7B transcription (every segment already has a transcript). Nothing to do."
                .to_string(),
        );
        return Ok(WslRefinementSummary { transcribed: 0, failed: 0, cancelled: false });
    }

    let total = targets.len();
    emit_or_log(app, "wsl-log", format!(">>> {total} segment(s) awaiting 7B transcription."));

    if dry_run {
        for (idx, (id, path)) in targets.iter().enumerate() {
            let file = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path.as_str());
            emit_or_log(
                app,
                "wsl-log",
                format!("[dry-run] [{}/{}] would transcribe {} ({})", idx + 1, total, id, wsl_log_preview(file)),
            );
        }
        emit_or_log(app, "wsl-log", ">>> Dry run complete — no transcripts were written.".to_string());
        return Ok(WslRefinementSummary { transcribed: 0, failed: 0, cancelled: false });
    }

    let normalizer = crate::normalizer::SoraniNormalizer::with_config(crate::normalizer::NormalizationConfig {
        normalize_numbers: auto_normalize,
        verbalize_numbers,
        normalize_hamza: true,
        remove_diacritics: false,
    });

    let mut transcribed = 0usize;
    let mut failed = 0usize;
    for (idx, (id, _path)) in targets.iter().enumerate() {
        if WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::Relaxed) {
            emit_or_log(app, "wsl-log", format!(">>> Cancelled by user after {idx}/{total} segment(s)."));
            return Ok(WslRefinementSummary { transcribed, failed, cancelled: true });
        }
        emit_or_log(app, "wsl-log", format!("[{}/{}] transcribing {}...", idx + 1, total, id));
        match crate::pipeline::run_wsl_segment_transcript_with_script(
            external_script,
            id,
            db_path,
            Some(&WSL_REFINE_CANCEL),
        ) {
            Ok((raw_transcript, confidence)) => {
                let normalized = if auto_normalize && !raw_transcript.is_empty() {
                    Some(normalizer.normalize(&raw_transcript))
                } else {
                    None
                };
                match db.update_asr_transcript_if_unreviewed(
                    id,
                    &raw_transcript,
                    normalized.as_deref(),
                    confidence,
                    Some("external_provider"),
                    Some("omniasr-wsl-7b"),
                    false,
                ) {
                    Ok(true) => {
                        transcribed += 1;
                        // Record the champion's provenance hypothesis like BOTH other 7B paths do
                        // (import pass + single re-transcribe) — without it the review badge cannot
                        // name the producing engine for batch-refined segments and the jury lacks
                        // the champion's highest-weight vote for exactly these clips (true-10 audit
                        // 2026-07-09). Best-effort: a provenance-write failure must not fail the
                        // (successful) transcription.
                        if let Err(e) = db.insert_hypothesis(&crate::db::SegmentHypothesis {
                            segment_id: id.to_string(),
                            model_id: "omniasr-wsl-7b".to_string(),
                            transcript: raw_transcript.clone(),
                            confidence,
                        }) {
                            tracing::warn!("could not record omniasr-wsl-7b provenance for {id}: {e}");
                        }
                        emit_or_log(
                            app,
                            "wsl-log",
                            format!("[{}/{}] {} -> {}", idx + 1, total, id, wsl_log_preview(raw_transcript.trim())),
                        );
                    }
                    Ok(false) => emit_or_log(
                        app,
                        "wsl-log",
                        format!("[{}/{}] {} skipped (human-reviewed; transcript not overwritten)", idx + 1, total, id),
                    ),
                    Err(error) => {
                        failed += 1;
                        emit_or_log(
                            app,
                            "wsl-log",
                            format!(
                                "[ERROR] [{}/{}] {} db write failed: {}",
                                idx + 1,
                                total,
                                id,
                                wsl_log_preview(&error.to_string())
                            ),
                        );
                    }
                }
            }
            Err(error) => {
                // A cancel mid-clip surfaces here as an error from the spawn; attribute it to the
                // cancel, not to a failure, and stop the run.
                if WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::Relaxed) {
                    emit_or_log(app, "wsl-log", format!(">>> Cancelled by user during segment {}/{}.", idx + 1, total));
                    return Ok(WslRefinementSummary { transcribed, failed, cancelled: true });
                }
                failed += 1;
                emit_or_log(
                    app,
                    "wsl-log",
                    format!("[ERROR] [{}/{}] {}: {}", idx + 1, total, id, wsl_log_preview(&error.to_string())),
                );
            }
        }
    }

    // A cancel that arrives during the FINAL segment passes every in-loop check (there is no next
    // iteration); re-check once here so it is honestly reported as cancelled, not completed.
    if WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::Relaxed) {
        emit_or_log(app, "wsl-log", format!(">>> Cancelled by user; {transcribed} transcribed before stopping."));
        return Ok(WslRefinementSummary { transcribed, failed, cancelled: true });
    }

    emit_or_log(
        app,
        "wsl-log",
        format!(">>> Complete! {transcribed} transcribed, {failed} failed of {total} pending."),
    );
    Ok(WslRefinementSummary { transcribed, failed, cancelled: false })
}

#[tauri::command]
pub fn cancel_wsl_refinement() -> Result<(), String> {
    // Only arm the cancel while a batch is actually running, so an idle cancel can't leak into and
    // immediately abort the NEXT run. Signals the batch loop (checked between segments) and the
    // in-flight per-segment spawn (which polls this same flag and kills its child) to stop; there is
    // no single child handle to kill here — each per-segment child is owned and reaped in the helper.
    if WSL_REFINE_RUNNING.load(std::sync::atomic::Ordering::SeqCst) {
        WSL_REFINE_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}

/// Insert a single segment hypothesis. Reserved programmatic API: the jury/consensus pipeline
/// produces hypotheses internally (ASR votes, Scribe votes), so there is no manual single-insert UI
/// — this stays for CLI/scripted jury orchestration. (IPC-surface audit 2026-06-25.)
#[tauri::command]
pub fn add_segment_hypothesis(state: State<'_, AppState>, hyp: crate::db::SegmentHypothesis) -> Result<(), String> {
    RATE_LIMITER.check("add_segment_hypothesis")?;
    // This row feeds the IRT jury consensus, so apply the same validation discipline every other write
    // command uses: shaped identifiers and a bounded transcript (round-22 #3) — never an unbounded blob
    // or a malformed id straight from the renderer.
    validate::validate_identifier(&hyp.segment_id)?;
    validate::validate_identifier(&hyp.model_id)?;
    validate::validate_text(&hyp.transcript, 100_000, "Hypothesis transcript")?;
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
        // Missing IRT confidence ⇒ MINIMUM, not maximum: a no-signal segment must not be recorded as
        // maximally confident (which would suppress its escalation downstream).
        let confidence = *results.segment_confidences.get(segment_id).unwrap_or(&0.0);
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
        // Score only THIS segment's clip, not the whole source file. Segments share the source
        // audio_path (the per-segment range lives in alignment_json), so without slicing the acoustic
        // ctc_score — which feeds the conformal jury gate — would be computed over the ENTIRE recording
        // for every segment, a systematically wrong quality signal on any multi-segment import.
        let pcm_16k = match crate::chunking::slice_pcm_by_alignment(
            &pcm_16k,
            audio::TARGET_SAMPLE_RATE,
            seg.alignment_json.as_deref(),
        ) {
            Ok((clip, _)) => clip,
            Err(error) => {
                tracing::warn!("Skipping acoustic score for {}: clip slice failed: {error}", seg.id);
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
        // Score only THIS segment's clip, not the whole source file (same whole-file-vs-clip hazard as
        // the acoustic-score loop): segments share the source audio_path, with the range in alignment_json.
        let pcm_16k = match crate::chunking::slice_pcm_by_alignment(
            &pcm_16k,
            audio::TARGET_SAMPLE_RATE,
            seg.alignment_json.as_deref(),
        ) {
            Ok((clip, _)) => clip,
            Err(error) => {
                tracing::warn!("Skipping OOD score for {}: clip slice failed: {error}", seg.id);
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
    // Validate every frontend-supplied input BEFORE any file is opened — the same guard every other
    // file-opening command applies (import_audio_file, import_model_checkpoint, get_waveform, ...).
    // Without it, a compromised/XSS'd renderer could pass an arbitrary, UNC, or traversal path that
    // eval::import_gold_segments -> source_audio_identity opens and fully reads (info disclosure /
    // outbound-SMB on Windows), plus persist it; the reference was likewise uncapped.
    let validated: Vec<crate::eval::GoldSegmentInput> = inputs
        .into_iter()
        .map(|inp| {
            let audio_path = validate::validate_file_path(&inp.audio_path)?;
            validate::validate_text(&inp.reference, 100_000, "Gold reference")?;
            Ok::<_, String>(crate::eval::GoldSegmentInput { audio_path, ..inp })
        })
        .collect::<Result<_, _>>()?;
    let db = state.lock_db();
    crate::eval::import_gold_segments(&db, validated).map_err(|e| e.to_string())
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
    let (autonomy, learn) = {
        let s = state.lock_settings();
        (s.jury_autonomy_level.clone(), s.irt_ability_learning_enabled)
    };
    crate::jury::run_t0_gate(&db, &segment_ids, &autonomy, learn).map_err(|e| e.to_string())
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

/// M2.7 / P1.6: bulk-promote every reviewed source file into the gold set. Returns rows created.
#[tauri::command]
pub fn import_verified_segments_as_gold(state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("import_verified_segments_as_gold")?;
    let db = state.lock_db();
    crate::eval::import_verified_segments_as_gold(&db).map_err(|e| e.to_string())
}

/// M2.7 / P1.6: export the gold set as a portable eval set (manifest.jsonl + 16 kHz WAV clips) under
/// `out_dir`. Returns the export summary (counts + manifest path).
#[tauri::command]
pub fn export_gold_eval_set(
    out_dir: String,
    state: State<'_, AppState>,
) -> Result<crate::eval::GoldEvalExport, String> {
    STRICT_RATE_LIMITER.check("export_gold_eval_set")?;
    if out_dir.contains('\0') {
        return Err("Output path contains null bytes".to_string());
    }
    let db = state.lock_db();
    crate::eval::export_gold_eval_set(&db, std::path::Path::new(&out_dir)).map_err(|e| e.to_string())
}

/// M5.1 / P5.1: export a fine-tune training pack (trainer manifest + 16 kHz clips) from human-verified
/// segments under `out_dir`, EXCLUDING holdout gold (the leak guard). Returns the pack summary.
#[tauri::command]
pub fn export_finetune_pack(
    out_dir: String,
    state: State<'_, AppState>,
) -> Result<crate::eval::FinetunePackResult, String> {
    STRICT_RATE_LIMITER.check("export_finetune_pack")?;
    if out_dir.contains('\0') {
        return Err("Output path contains null bytes".to_string());
    }
    // P5.5: every pack export appends its provenance line to the durable corpus ledger.
    let ledger = state.lock_data_dir().clone().map(|d| d.join("corpus_ledger.jsonl"));
    let db = state.lock_db();
    crate::eval::export_finetune_pack(&db, std::path::Path::new(&out_dir), ledger.as_deref()).map_err(|e| e.to_string())
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
    // Enforce the cloud-STT privacy gate at the IPC trust boundary: audio must never leave the device
    // for ElevenLabs unless the user explicitly opted in — require_cloud_stt_consent mirrors
    // pipeline::scribe_api_key_if_enabled so every Scribe egress path honors the same toggle, not just
    // the import path.
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
///
/// Round-23 #9: this label is stored as the hypothesis' provenance AND shown to the T2 judge, so it
/// MUST name the model version ACTUALLY transmitted to ElevenLabs (`scribe_api::DEFAULT_MODEL`,
/// currently `scribe_v1`) — never a version that was never invoked. The
/// `scribe_vote_model_id_matches_the_model_actually_sent` test fails if the two ever drift.
const SCRIBE_VOTE_MODEL_ID: &str = "scribe-v1";

/// Add an independent ElevenLabs Scribe hypothesis for the given segments (typically the escalated,
/// hard ones), so the IRT jury sees an architecturally-INDEPENDENT vote rather than only kin OmniASR
/// models — closing the confidently-wrong-correlated-error hole. Opt-in and cost-bounded: it
/// transcribes only the segments the caller chooses and skips any that already have a Scribe vote
/// (idempotent). Scribe is a second opinion (~32% WER on Sorani), never auto-accepted as gold.
/// Returns the number of votes added. Re-run the jury afterwards to fold the new votes into consensus.
#[tauri::command]
pub fn add_scribe_votes(ids: Vec<String>, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("add_scribe_votes")?;
    // Same cloud-STT privacy gate as transcribe_audio_with_scribe: no segment audio is uploaded to
    // ElevenLabs unless the user explicitly opted in, even if a key is present in secrets.env.
    require_cloud_stt_consent(&state)?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let key = crate::api_keys::ApiKeys::load(&data_dir)
        .elevenlabs
        .ok_or_else(|| "No ElevenLabs API key configured — add ELEVENLABS_API_KEY to secrets.env".to_string())?;

    // Read which segments still need a Scribe vote, then RELEASE the db lock before any network call —
    // never hold the global db mutex across a blocking cloud request (round-7 concurrency lesson). We
    // keep the FULL segment (audio_path + alignment_json) so each vote can be sliced to that segment's
    // own audio window rather than the whole source file.
    let to_vote: Vec<crate::db::SpeechSegment> = {
        let db = state.lock_db();
        let segs = db.get_segments_by_ids(&ids).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for seg in segs {
            let existing = db.get_hypotheses_for_segment(&seg.id).map_err(|e| e.to_string())?;
            if !existing.iter().any(|h| h.model_id == SCRIBE_VOTE_MODEL_ID) {
                // Keep the full segment (audio_path + alignment_json) so the Scribe vote covers the SAME
                // audio span as the local hypotheses. Without that window the vote would be the
                // whole-recording transcript (segments share the source path), which can never align with
                // the short local hyps and poisons consensus.
                out.push(seg);
            }
        }
        out
    };

    let mut added = 0usize;
    for seg in to_vote {
        // Send ONLY this segment's sliced audio window to Scribe — never the whole source file. The
        // whole file would store a whole-recording transcript against one segment's `scribe-v1` vote
        // (corrupting consensus) and cost ~N× more. `segment_audio_as_wav_bytes` decodes (cached) and
        // slices by the segment alignment — the same window the T2 listener already sends.
        let wav = match crate::agentic::segment_audio_as_wav_bytes(&seg) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("Scribe vote skipped: could not slice segment audio: {e}");
                continue;
            }
        };
        match crate::scribe_api::transcribe_wav_bytes(
            &wav,
            "segment.wav",
            &key,
            crate::scribe_api::DEFAULT_MODEL,
            crate::scribe_api::SORANI_LANGUAGE_CODE,
        ) {
            Ok(transcript) => {
                let hyp = crate::db::SegmentHypothesis {
                    segment_id: seg.id.clone(),
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
    timestamp_ms: Option<i64>,
) -> Result<(), String> {
    RATE_LIMITER.check("record_human_decision")?;
    // Round-22 #4: validate the id and bound the free text, matching every other write command.
    validate::validate_identifier(&segment_id)?;
    if let Some(t) = corrected_transcript.as_deref() {
        validate::validate_text(t, 100_000, "Corrected transcript")?;
    }
    let db = state.lock_db();
    db.record_human_decision(&segment_id, &decision, corrected_transcript.as_deref(), timestamp_ms)
        .map_err(|e| e.to_string())?;

    // M2.6: Update session with current review segment for cursor persistence on restart.
    let mut session = state.lock_session();
    session.set_current_segment(&segment_id);
    let _ = session.save(&db);

    Ok(())
}

/// P3-3: Revert a segment back to unreviewed state (NULL human_decision).
/// This is the correct undo operation — avoids incorrectly re-setting to 'accept'.
#[tauri::command]
pub fn clear_human_decision(state: State<'_, AppState>, segment_id: String) -> Result<(), String> {
    RATE_LIMITER.check("clear_human_decision")?;
    validate::validate_identifier(&segment_id)?; // round-22 #4
    let db = state.lock_db();
    db.clear_human_decision(&segment_id).map_err(|e| e.to_string())
}

/// Undo a review-inbox `flag()`: clear the escalation the flag set. Distinct from clear_human_decision
/// (which reopens a decided row by SETTING escalated=1); this is the inverse of flag.
#[tauri::command]
pub fn clear_escalation(state: State<'_, AppState>, segment_id: String) -> Result<(), String> {
    RATE_LIMITER.check("clear_escalation")?;
    validate::validate_identifier(&segment_id)?;
    let db = state.lock_db();
    db.clear_escalation(&segment_id).map_err(|e| e.to_string())
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
    // Round-22 #4: validate the id and bound every free-text field, matching the other write commands.
    validate::validate_identifier(&segment_id)?;
    if let Some(t) = transcript.as_deref() {
        validate::validate_text(t, 100_000, "Verdict transcript")?;
    }
    if let Some(r) = rationale.as_deref() {
        validate::validate_text(r, 100_000, "Verdict rationale")?;
    }
    // evidence_json is always serialized JSON; validate_alignment_json both confirms it parses as JSON
    // and bounds it (max 500KB), which is stricter and more apt than a plain length cap.
    if let Some(ej) = evidence_json.as_deref() {
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

/// Run `f` with a database handle that does NOT keep the global `AppState` db Mutex locked for the
/// duration of the call. `run_jury_pipeline_core` interleaves DB reads/writes with (potentially many)
/// cloud T2 network round-trips in a per-segment loop; passing the shared, locked handle would hold
/// the global Mutex across every `listen_and_judge`, freezing every other DB-touching command
/// app-wide for the whole adjudication.
///
/// To avoid that, this opens a SECOND, dedicated connection to the same database file and runs `f`
/// against it — SQLite WAL + `busy_timeout` let the two connections coexist, and all writes land in
/// the same file, so verdicts persist exactly as before. It falls back to the shared, locked handle
/// for an in-memory database (tests can't share `:memory:` across connections, and that path has cloud
/// off so there is no network call to block on) or in the rare event the dedicated open fails.
fn with_jury_db<R>(app_state: &AppState, f: impl FnOnce(&crate::db::Database) -> R) -> R {
    let db_path = { app_state.lock_db().path().to_string() };
    if db_path != ":memory:" {
        // Dedicated worker connection so we never hold the GLOBAL db Mutex across the jury's cloud T2
        // Gemini round-trips (which would freeze every other DB command for minutes). Use plain `open`,
        // NOT open_with_retry: the latter runs a full PRAGMA integrity_check (a per-review-session tax
        // that grows with the library) AND reaches the boot-time-only DESTRUCTIVE quarantine decision
        // from a live worker thread. The file was already integrity-checked at boot; `open` sets WAL +
        // busy_timeout=10000, which is the actual transient-contention retry we want here.
        match crate::db::Database::open(&db_path) {
            Ok(db) => return f(&db),
            Err(e) => tracing::warn!(
                "Jury dedicated db connection open failed after retries ({e}); using the shared handle \
                 (other DB commands may pause during adjudication)"
            ),
        }
    }
    let db = app_state.lock_db();
    f(&db)
}

pub fn run_jury_pipeline_core(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    segment_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    let t1_threshold = settings.jury_t1_threshold;
    let cloud_opt_in = settings.jury_cloud_opt_in;
    let jury_model = settings.jury_model.clone();
    // Floor at 3: self-consistency is meaningless below 3 samples, and a misconfigured 1 would let a
    // single Gemini sample masquerade as a "majority". majority_vote also requires >= 2 agreeing
    // samples, so this is defense in depth at the config boundary.
    let n_samples = (settings.jury_self_consistency_n as usize).max(3);
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
        crate::jury::run_t0_gate(
            db,
            &t0_candidate_ids,
            &settings.jury_autonomy_level,
            settings.irt_ability_learning_enabled,
        )
        .map_err(|e| e.to_string())?
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
    // Run on a dedicated connection so the global db Mutex is not held across the jury's blocking T2
    // cloud calls — holding it would freeze the UI's get_segments for the whole run. with_jury_db
    // retries the dedicated open and only falls back to the shared handle on a hard failure.
    with_jury_db(&state, |db| run_jury_pipeline_core(db, &settings, segment_ids))
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
    // Floor at 3: self-consistency is meaningless below 3 samples, and a misconfigured 1 would let a
    // single Gemini sample masquerade as a "majority". majority_vote also requires >= 2 agreeing
    // samples, so this is defense in depth at the config boundary.
    let n_samples = (settings.jury_self_consistency_n as usize).max(3);
    let cloud_opt_in = settings.jury_cloud_opt_in;

    if !cloud_opt_in {
        return Err("Cloud opt-in is required for T2. Enable it in Settings → Listening Jury.".into());
    }
    if api_key.trim().is_empty() {
        return Err("Gemini API key is required for T2.".into());
    }

    // Gather every DB input under a BRIEF lock, then drop it before listen_and_judge. Holding the
    // global AppState db Mutex across the cloud T2 round-trip (n_samples Gemini audio calls) would
    // freeze every other DB-touching command app-wide for the whole network call. The lock is
    // re-acquired only for the final verdict write below.
    let (audio_b64, hyps, reference_report, t2_evidence, few_shots) = {
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
        (audio_b64, hyps, reference_report, t2_evidence, few_shots)
    };

    // The gather block above released the global DB lock when it ended (all reads are done, few_shots
    // included), so the blocking T2 cloud call below (Gemini, n_samples retries — multiple seconds)
    // never starves other DB users like the UI's get_segments. The verdict write re-acquires briefly.
    let result = crate::jury::t2_listener::listen_and_judge(
        &audio_b64,
        &hyps,
        &t2_evidence,
        &few_shots,
        &api_key,
        &jury_model,
        n_samples,
    );

    // If T2 produced a verdict, write it to the DB automatically (re-acquire the lock briefly).
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
        // Re-acquire the lock only to persist the verdict.
        state
            .lock_db()
            .write_segment_verdict(
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

    // Upper cap at u32::MAX ms (~49.7 days) matches the export/diarization slicer's offset guard
    // (chunking.rs) — a bound beyond that is garbage the slicer would reject downstream anyway, so
    // reject it at the IPC boundary instead of storing an absurd duration/offset from the webview.
    if start_ms < 0 || end_ms < 0 || start_ms >= end_ms || end_ms > u32::MAX as i64 {
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

    #[test]
    fn scribe_vote_model_id_matches_the_model_actually_sent() {
        // Round-23 #9: the stored provenance label must name the model VERSION actually transmitted to
        // ElevenLabs, never a version that was never invoked. If DEFAULT_MODEL ever changes (e.g. to a
        // real scribe_v2), this fails until SCRIBE_VOTE_MODEL_ID is updated to match.
        let sent = crate::scribe_api::DEFAULT_MODEL; // e.g. "scribe_v1"
        let version = sent.rsplit('_').next().unwrap_or(sent); // "v1"
        assert!(
            SCRIBE_VOTE_MODEL_ID.contains(version),
            "Scribe vote label '{SCRIBE_VOTE_MODEL_ID}' must reflect the sent model version '{version}' (from '{sent}')"
        );
    }

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

    #[test]
    fn segment_awaits_wsl7b_flags_only_empty_or_placeholder_transcripts() {
        assert!(segment_awaits_wsl7b(""));
        assert!(segment_awaits_wsl7b("   "));
        assert!(segment_awaits_wsl7b("[Pending WSL 7B ASR]"));
        // Also the placeholders the LOCAL CTC import path can leave, so the batch can recover them too.
        assert!(segment_awaits_wsl7b("[ASR unavailable: model load failed]"));
        assert!(segment_awaits_wsl7b("n/a"));
        // A real transcript (CTC or human) must NOT be flagged — the batch never clobbers good text.
        assert!(!segment_awaits_wsl7b("سڵاو ئەمە دەقێکی ڕاستەقینەیە"));
        assert!(!segment_awaits_wsl7b("a real ctc transcript"));
    }

    #[test]
    fn select_wsl_refinement_targets_orders_by_chunk_offset_within_a_file_not_by_uuid() {
        // Two chunks of ONE file sharing a created_at. The LATER chunk has the lexically-SMALLER id,
        // so a naive id/UUID order (the old `.rev()` of `id ASC`) would pick it first; the chunk's
        // source offset must win so test_one transcribes chunk 0.
        let meta = |start: i64, idx: u32| {
            crate::chunking::SegmentSourceMeta {
                source_start_ms: start,
                source_end_ms: start + 1000,
                chunk_index: idx,
                chunk_count: 2,
            }
            .to_alignment_json()
        };
        let mut early = test_segment("zzz-chunk0", "file.wav", "[Pending WSL 7B ASR]");
        early.created_at = Some("2026-01-01T00:00:00Z".to_string());
        early.alignment_json = Some(meta(0, 0));
        let mut late = test_segment("aaa-chunk1", "file.wav", "[Pending WSL 7B ASR]");
        late.created_at = Some("2026-01-01T00:00:00Z".to_string());
        late.alignment_json = Some(meta(5000, 1));
        // get_segments returns newest-first; pass the later-position chunk first.
        let segments = vec![late, early];
        let targets = select_wsl_refinement_targets(&segments, None, None, true);
        assert_eq!(
            targets,
            vec![("zzz-chunk0".to_string(), "file.wav".to_string())],
            "test_one must pick chunk 0 (earliest source offset), not the lexically-smaller UUID"
        );
    }

    #[test]
    fn select_wsl_refinement_targets_takes_only_pending_oldest_first() {
        // get_segments returns newest-first; the batch must drain oldest-first and skip non-pending.
        let segments = vec![
            test_segment("s4", "b.wav", "real transcript"), // newest, has text -> excluded
            test_segment("s3", "b.wav", "[Pending WSL 7B ASR]"), // pending
            test_segment("s2", "a.wav", ""),                // pending
            test_segment("s1", "a.wav", "already done"),    // oldest, has text -> excluded
        ];
        let targets = select_wsl_refinement_targets(&segments, None, None, false);
        assert_eq!(targets, vec![("s2".to_string(), "a.wav".to_string()), ("s3".to_string(), "b.wav".to_string())]);
    }

    #[test]
    fn select_wsl_refinement_targets_honors_file_segment_and_test_one_limits() {
        // newest-first input; oldest-first pending order is s1(a) s2(a) s3(b) s4(b) s5(c).
        let segments = vec![
            test_segment("s5", "c.wav", ""),
            test_segment("s4", "b.wav", ""),
            test_segment("s3", "b.wav", ""),
            test_segment("s2", "a.wav", ""),
            test_segment("s1", "a.wav", ""),
        ];
        let ids = |targets: Vec<(String, String)>| targets.into_iter().map(|(id, _)| id).collect::<Vec<_>>();

        // limit_files = 2 keeps only the first two distinct files (a, b), not c.
        assert_eq!(ids(select_wsl_refinement_targets(&segments, Some(2), None, false)), vec!["s1", "s2", "s3", "s4"]);
        // limit_segments = 3 caps to the three oldest pending.
        assert_eq!(ids(select_wsl_refinement_targets(&segments, None, Some(3), false)), vec!["s1", "s2", "s3"]);
        // test_one overrides limit_segments down to a single oldest segment.
        assert_eq!(ids(select_wsl_refinement_targets(&segments, None, Some(3), true)), vec!["s1"]);
    }

    fn downloaded_model_status(filename: &str) -> serde_json::Value {
        serde_json::json!({
            "filename": filename,
            "downloaded": true,
        })
    }

    #[test]
    fn agentic_readiness_offline_source_reference_is_ready_not_blocked() {
        let readiness = build_agentic_readiness(
            &crate::settings::AppSettings::default(), // cloud off, no models
            &[],
            &serde_json::json!({
                "available": false,
                "message": "No external ASR provider script configured"
            }),
        );

        // Cloud whole-file references are OPTIONAL — with cloud off (offline by choice), the source-
        // reference check is "ready" (local consensus is the chosen, functional mode), so it does NOT
        // nag on every import. Overall is still blocked ONLY because hypothesis coverage is missing.
        assert!(readiness.checks.iter().any(|check| check.id == "source_reference" && check.status == "ready"));
        assert_eq!(readiness.status, "blocked");
        assert!(!readiness.ready);
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
    fn drain_log_lines_survives_non_utf8_and_delivers_every_line() {
        // A single non-UTF-8 byte mid-stream must NOT terminate the feed (the old
        // lines().map_while(Result::ok) did, silently freezing the live WSL progress feed). Every
        // later line must still arrive, with the bad byte replaced lossily and a trailing CR trimmed.
        let mut data = Vec::new();
        data.extend_from_slice(b"line1\n");
        data.extend_from_slice(&[b'b', b'a', b'd', 0xFF, b'\n']); // invalid UTF-8 line
        data.extend_from_slice("کوردی\n".as_bytes()); // valid Sorani after the bad line
        data.extend_from_slice(b"line4\r\n"); // CRLF — trailing CR must be trimmed

        let mut got = Vec::new();
        drain_log_lines(std::io::Cursor::new(data), |l| got.push(l.to_string()));

        assert_eq!(got.len(), 4, "all four lines must be delivered despite the bad byte: {got:?}");
        assert_eq!(got[0], "line1");
        assert!(got[1].starts_with("bad"), "bad line still delivered lossily: {:?}", got[1]);
        assert_eq!(got[2], "کوردی", "a valid line AFTER the bad one must still arrive");
        assert_eq!(got[3], "line4", "trailing CR is trimmed");
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
