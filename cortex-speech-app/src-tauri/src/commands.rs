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
use crate::health;
use crate::history::Command;
use crate::models;
use crate::pipeline::PipelineEvent;
use crate::quality;
use crate::settings::{AppSettings, AsrModelSize};
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

// Week-4 decomposition: the export commands live in their own slice. Re-exported so the
// command names (and lib.rs's invoke_handler) are completely unchanged.
mod export;
pub use export::*;
mod model_download;
pub use model_download::*;
mod batch;
pub use batch::*;
mod dataset_analytics;
pub use dataset_analytics::*;
mod gold_eval;
pub use gold_eval::*;
mod transcribe;
pub use transcribe::*;
mod jury;
pub use jury::*;
mod segments_read;
pub use segments_read::*;
mod segments_write;
pub use segments_write::*;
mod agentic;
pub use agentic::*;
mod infra;
pub use infra::*;
mod settings;
pub use settings::*;

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

/// Run blocking work (DB scans, serialization, hashing, file I/O) OFF the main/UI thread via a
/// `spawn_blocking` pool, so a slow `#[tauri::command]` can't freeze the window (Tauri runs sync
/// commands on the main thread — the same class that caused the Open/Import freeze). A panic in the
/// task becomes a clean error instead of aborting the process. The closure must own everything it
/// needs (clone `state.db_arc()` etc. BEFORE calling — never borrow `State` across the await).
async fn run_blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("background task failed: {e}"))?
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
        // (offline-first). The selected primary ASR remains fully functional without them, so this is
        // the chosen configuration — NOT a degradation that should nag on every import.
        //
        // But it is not "ready" either, and saying so was a green tick the feature had not earned: a
        // reviewer scanning the panel could not tell "source references are covering your data" from
        // "source references are off", because both printed `ready` in the same emerald. Deep-audit
        // 2026-08-05 named this exactly — "not required in this mode is not the same state as ready".
        //
        // `not_required` keeps the non-nagging intent (the aggregate below treats anything that is
        // neither blocked nor degraded as ready, so the overall verdict is unchanged) while refusing to
        // claim coverage that does not exist. Pinned by
        // `disabled_cloud_reports_not_required_not_ready_but_keeps_the_overall_verdict_ready`.
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "not_required",
            "Offline mode: cloud whole-file references are off by choice (an optional cross-check). Primary ASR readiness is reported separately. Enable jury cloud opt-in to add Gemini whole-file references.",
        ));
    } else if settings.llm_api_key.trim().is_empty() {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "blocked",
            "Gemini API key is not loaded in this session, so source-reference transcription would fail before chunking.",
        ));
    } else if source_reference_models.is_empty() {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "blocked",
            "No owner-approved source-reference model is configured.",
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

    let auxiliary_mode = settings.multi_engine_hypotheses && settings.asr_model_size != AsrModelSize::WSL7B;
    let (primary_model_id, primary_label, primary_ready, primary_detail) = match &settings.asr_model_size {
        AsrModelSize::WSL7B => {
            let detail = if wsl_ready {
                "WSL external ASR provider is configured and WSL reports healthy.".to_string()
            } else {
                external_provider
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("WSL external ASR provider is not ready.")
                    .to_string()
            };
            ("omniasr-wsl-7b", "Primary OmniASR 7B", wsl_ready, detail)
        }
        AsrModelSize::CTC1B => (
            "omniasr-ctc-1b",
            "Selected OmniASR CTC 1B",
            ctc_1b_ready,
            if ctc_1b_ready {
                "The explicitly selected CTC 1B model and tokens are available.".to_string()
            } else {
                "The explicitly selected CTC 1B model or tokens are missing.".to_string()
            },
        ),
        AsrModelSize::CTC300M => (
            "omniasr-ctc-300m",
            "Selected OmniASR CTC 300M",
            ctc_300m_ready,
            if ctc_300m_ready {
                "The explicitly selected CTC 300M model and tokens are available.".to_string()
            } else {
                "The explicitly selected CTC 300M model or tokens are missing.".to_string()
            },
        ),
    };

    // Report engines that this configuration can actually invoke, not every optional model merely
    // installed on disk. In champion-only mode an installed CTC model is not an active hypothesis
    // source and must not make the UI imply that it will run.
    let mut available_hypothesis_models = Vec::new();
    if primary_ready {
        available_hypothesis_models.push(primary_model_id.to_string());
    }
    if auxiliary_mode {
        for (ready, model_id) in
            [(wsl_ready, "omniasr-wsl-7b"), (ctc_1b_ready, "omniasr-ctc-1b"), (ctc_300m_ready, "omniasr-ctc-300m")]
        {
            if ready && !available_hypothesis_models.iter().any(|existing| existing == model_id) {
                available_hypothesis_models.push(model_id.to_string());
            }
        }
    }

    if primary_ready {
        checks.push(readiness_check("primary_asr", primary_label, "ready", primary_detail));
    } else {
        checks.push(readiness_check(
            "primary_asr",
            primary_label,
            "blocked",
            format!(
                "{primary_detail} The selected primary engine is unavailable; refusing to substitute another engine."
            ),
        ));
    }

    let required_hypothesis_models = quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE;
    if !auxiliary_mode {
        checks.push(readiness_check(
            "hypothesis_coverage",
            "Multi-model hypothesis coverage",
            "not_required",
            format!(
                "Single-engine mode: {primary_model_id} is the only automatic ASR source. Optional engines will not run. Training/export promotion still validates its required stored corroboration separately (minimum {required_hypothesis_models})."
            ),
        ));
    } else if available_hypothesis_models.len() >= required_hypothesis_models {
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

/// Decode MANY clips out of ONE source recording in a SINGLE streaming pass.
///
/// A per-clip decoder walks the file from byte zero to the clip's end. That is right for one clip and
/// QUADRATIC for a pack: this library keeps 416 verified clips inside a single podcast FLAC,
/// so exporting it re-decoded that FLAC 416 times, each walk longer than the last. Measured
/// 2026-08-18 on the live library: **87 rows in 56 minutes (~36 s/row)**, which puts a full pack in
/// the hours and makes the challenger loop's "zero manual steps" unreachable in practice.
///
/// This walks the source once and hands each clip over the moment its window has passed, so peak
/// memory is the clips overlapping the current 30 s window rather than the whole file — the same
/// bound the streaming import already holds itself to. The PCM handed to `on_clip` is
/// byte-for-byte what the per-clip decoder produced: same decoder, same window size, same slice
/// arithmetic, so a pack's manifest hash (its snapshot id) does not move because of this change.
pub(crate) fn decode_finetuned_clips_16k<F>(audio_path: &str, spans: &[ClipSpan], mut on_clip: F) -> Result<(), String>
where
    F: FnMut(&str, Vec<i16>) -> Result<(), String>,
{
    if spans.is_empty() {
        return Ok(());
    }
    // Opened in start order so a clip can be closed and its buffer freed as soon as the walk passes it.
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| (spans[i].start_ms, i));
    let last_end = spans.iter().map(|s| s.end_ms).max().unwrap_or(0);

    let mut next = 0_usize;
    let mut open: Vec<(usize, Vec<i16>)> = Vec::new();
    let mut rate = crate::audio::TARGET_SAMPLE_RATE;
    const CLIPS_DONE: &str = "__clip_windows_done__";

    let finish = |idx: usize, acc: Vec<i16>, rate: u32, on_clip: &mut F| -> Result<(), String> {
        let (_r, pcm16) = crate::audio::ensure_pcm_16khz(rate, acc).map_err(|e| e.to_string())?;
        on_clip(&spans[idx].segment_id, pcm16)
    };

    let res = crate::audio::decode_pcm_windows(audio_path, 30_000, |win| {
        rate = win.sample_rate.max(1);
        let win_start = win.offset_ms;
        let dur_ms = (win.pcm.len() as i64 * 1000) / rate as i64;
        let win_end = win_start + dur_ms;

        while next < order.len() && spans[order[next]].start_ms < win_end {
            open.push((order[next], Vec::new()));
            next += 1;
        }
        for (idx, acc) in open.iter_mut() {
            let span = &spans[*idx];
            if win_end > span.start_ms && win_start < span.end_ms {
                let a_ms = (span.start_ms.max(win_start) - win_start).max(0);
                let b_ms = (span.end_ms.min(win_end) - win_start).max(0);
                let a = ((a_ms * rate as i64) / 1000) as usize;
                let b = (((b_ms * rate as i64) / 1000) as usize).min(win.pcm.len());
                if b > a {
                    acc.extend_from_slice(&win.pcm[a..b]);
                }
            }
        }
        // Hand over every clip the walk has now passed, so its buffer does not outlive its window.
        let mut still_open = Vec::with_capacity(open.len());
        for (idx, acc) in open.drain(..) {
            if spans[idx].end_ms <= win_end {
                finish(idx, acc, rate, &mut on_clip).map_err(crate::error::AppError::Other)?;
            } else {
                still_open.push((idx, acc));
            }
        }
        open = still_open;

        // Everything asked for has been delivered — stop rather than decode the rest of a long file.
        if next >= order.len() && open.is_empty() && win_start >= last_end {
            return Err(crate::error::AppError::Other(CLIPS_DONE.to_string()));
        }
        Ok(())
    });
    match res {
        Ok(()) => {}
        Err(crate::error::AppError::Other(m)) if m == CLIPS_DONE => return Ok(()),
        // An `on_clip` failure travels back out as itself, not as a decode error.
        Err(crate::error::AppError::Other(m)) => return Err(m),
        Err(e) => return Err(e.to_string()),
    }
    // EOF with clips still open (a span running past the end of the file) or never opened at all:
    // hand over what was actually decoded. An empty buffer reaches the caller as an empty clip, which
    // is the same "undecodable, skip it" signal the per-clip decoder gives.
    for (idx, acc) in std::mem::take(&mut open) {
        finish(idx, acc, rate, &mut on_clip)?;
    }
    for &idx in &order[next..] {
        finish(idx, Vec::new(), rate, &mut on_clip)?;
    }
    Ok(())
}

/// Record the FIRST failure only, so the reported cause is the one that actually stopped the run.
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
fn parse_batch_concurrency(raw: Option<&str>) -> usize {
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
                let pre_transcription_snapshot = seg.clone();
                // The batch's cancel token rides into the 7B call (2026-08-20 external review: it
                // passed None, so Cancel could not reach an in-flight or gate-queued champion call).
                match pipeline.transcribe(
                    Some(id),
                    &seg.audio_path,
                    seg.alignment_json.as_deref(),
                    Some(cancel.as_atomic()),
                ) {
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
                }
            }
        }

        // A run that stopped on a failure is reported as HALTED, with the first cause named. It must
        // never arrive at the UI as an ordinary "completed" — that is precisely how a half-drafted
        // dataset gets mistaken for a finished one.
        let halted_by = first_failure.into_inner().ok().flatten();
        if let Some(reason) = &halted_by {
            tracing::error!(
                "Batch transcribe HARD-STOPPED after {succeeded} succeeded, {skipped} skipped: {reason}. \
                 Remaining clips were NOT transcribed; the dataset is incomplete, not finished."
            );
        }
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

/// Restrict review evidence to the ASR mode the owner selected. In champion mode, hypotheses left by
/// older builds (300M/1B/MMS/Scribe) are historical artifacts, not voters. If a pre-provenance 7B row
/// has no hypothesis record, synthesize the one honest vote from the segment's persisted champion
/// transcript so review still works without allowing stale engines back into the decision.
/// The provenance id written by the PRE-REGISTRY champion.
///
/// Production no longer names the champion by string — identity is content-addressed, and
/// `pipeline::CHAMPION_MODEL_ID` is `#[cfg(test)]` for exactly that reason. But rows drafted before
/// the registry existed still carry this id on disk, and champion review must keep recognising them
/// as champion-produced or it would hide the evidence for most of the existing corpus. This is
/// historical RECOGNITION only; nothing selects or serves a model by this string.
const LEGACY_CHAMPION_MODEL_ID: &str = "omniasr-wsl-7b";

/// Was this segment drafted by a champion-family model? Answers the DB question
/// [`hypotheses_for_selected_asr`] deliberately does not ask itself, so that filter stays pure.
/// The legacy constant covers rows written before the registry existed; the registry covers the rest.
/// Unknown provenance is NOT champion — fail closed.
fn segment_recorded_model_is_champion(db: &crate::db::Database, segment: &crate::db::SpeechSegment) -> bool {
    let Some(recorded) = segment.model_version_id.as_deref().map(str::trim).filter(|id| !id.is_empty()) else {
        return false;
    };
    if recorded == LEGACY_CHAMPION_MODEL_ID {
        return true;
    }
    match crate::registry::is_family_model(db, recorded, crate::deployment::OMNIASR_7B_FAMILY) {
        Ok(is_champion) => is_champion,
        Err(error) => {
            // Fail CLOSED (hide the votes) but never silently: a registry read that fails here would
            // otherwise look identical to "this row was drafted by a weaker engine".
            tracing::error!("champion-family lookup failed for model {recorded}: {error}");
            false
        }
    }
}

fn hypotheses_for_selected_asr(
    selected: &crate::settings::AsrModelSize,
    segment: &crate::db::SpeechSegment,
    mut hypotheses: Vec<crate::db::SegmentHypothesis>,
    recorded_model_is_champion: bool,
) -> Vec<crate::db::SegmentHypothesis> {
    if *selected != crate::settings::AsrModelSize::WSL7B {
        return hypotheses;
    }

    let Some(recorded_model_id) = segment.model_version_id.as_deref().filter(|id| !id.trim().is_empty()) else {
        // Without a persisted producing-version id, choosing the *current* champion would rewrite
        // history after promotion. Return no attributable vote instead of inventing provenance.
        return Vec::new();
    };
    // CHAMPION SUPREMACY (canon). Matching the row's own producing model is the right unit of
    // provenance, but on its own it re-admits the very thing the fixed-string filter excluded: a clip
    // drafted by a weaker engine BEFORE WSL7B was selected carries that engine's id, so a per-row
    // match would surface its hypotheses during champion review. This library contains exactly such
    // rows — 494/494 clips were once silently drafted by `finetuned-mms-ckb` while WSL7B was selected.
    // A non-champion producer contributes NO auxiliary vote, as before.
    if !recorded_model_is_champion {
        return Vec::new();
    }
    hypotheses.retain(|hypothesis| {
        hypothesis.model_id == recorded_model_id
            && !hypothesis.transcript.trim().is_empty()
            && !crate::quality::is_placeholder_transcript(&hypothesis.transcript)
    });
    if hypotheses.is_empty()
        && !segment.raw_transcript.trim().is_empty()
        && !crate::quality::is_placeholder_transcript(&segment.raw_transcript)
    {
        hypotheses.push(crate::db::SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: recorded_model_id.to_string(),
            transcript: segment.raw_transcript.clone(),
            confidence: segment.confidence,
        });
    }
    hypotheses
}

/// Offline best-of-N consensus DRAFT for a segment: an ability-weighted vote over its ASR hypotheses
/// (no cloud) so review can start from a transcript better than any single model and highlight exactly
/// where the models disagreed. Empty when the segment has no hypotheses to vote over.
#[tauri::command]
pub fn get_segment_consensus(state: State<'_, AppState>, segment_id: String) -> Result<SegmentConsensus, String> {
    RATE_LIMITER.check("get_segment_consensus")?;
    validate::validate_identifier(&segment_id)?;
    let selected = state.lock_settings().asr_model_size.clone();
    let (segment, hyps, recorded_is_champion) = {
        let db = state.lock_db();
        let segment = db
            .get_segment_by_id(&segment_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Segment '{segment_id}' no longer exists"))?;
        let hypotheses = db.get_hypotheses_for_segment(&segment_id).map_err(|e| e.to_string())?;
        // Answered while the lock is held; the filter itself stays pure and DB-free.
        let is_champion = segment_recorded_model_is_champion(&db, &segment);
        (segment, hypotheses, is_champion)
    };
    let hyps = hypotheses_for_selected_asr(&selected, &segment, hyps, recorded_is_champion);
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

/// Hydrate one selected list row with its full alignment/evidence payload.
#[tauri::command]
pub fn get_segment(segment_id: String, state: State<'_, AppState>) -> Result<SpeechSegment, String> {
    RATE_LIMITER.check("get_segment")?;
    validate::validate_identifier(&segment_id)?;
    let db = state.lock_db();
    db.get_segment_by_id(&segment_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Segment '{segment_id}' no longer exists"))
}

#[tauri::command]
pub fn get_segments_page(
    verified: Option<bool>,
    query: Option<String>,
    sort: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
    focused: Option<bool>,
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
        validate::validate_text(cursor, 2048, "Segment page cursor")?;
        if !cursor.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            return Err("Invalid segment page cursor".to_string());
        }
    }
    let limit = limit.unwrap_or(200).clamp(1, 500);
    // Voice focus for the DESKTOP review queue (owner report 2026-08-20: guests still played on
    // desktop while the phones were narrowed — the focus lived only on the couch path). Same
    // semantics as couch.rs: a MISSING file is no restriction, a file that EXISTS but cannot be
    // honoured serves NOTHING (present-but-broken fails CLOSED), and it is re-read per fetch so an
    // edit takes effect on the next refill. Only the review queue asks (`focused: true`); the
    // curate/library views stay unfocused — the queue narrows, the library does not.
    let focus = if focused.unwrap_or(false) {
        let dir = state.lock_data_dir().clone();
        crate::voice_focus::resolve(dir.as_deref())?
    } else {
        None
    };
    let db = state.lock_db();
    db.get_segments_page_focused(verified, query.as_deref(), &sort, limit, cursor.as_deref(), focus.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_segment_ids_for_view(
    verified: Option<bool>,
    query: Option<String>,
    transcript_state: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    RATE_LIMITER.check("get_segment_ids_for_view")?;
    if let Some(ref query) = query {
        validate::validate_text(query, 1000, "Search query")?;
    }
    let transcript_state = transcript_state.unwrap_or_else(|| "any".into());
    match transcript_state.as_str() {
        "any" | "real" | "missing" => {}
        _ => return Err("Invalid transcript state".into()),
    }
    let db = state.lock_db();
    db.get_segment_ids_for_view(verified, query.as_deref(), &transcript_state).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_signal_anomaly_segments(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_signal_anomaly_segments")?;
    let db = state.lock_db();
    db.get_signal_anomaly_segments(limit.unwrap_or(100)).map_err(|e| e.to_string())
}

/// Apply the whitelisted curation fields from an autosave `fields` object onto a segment row. Pure and
/// unit-tested. Only the three fields the debounced curation autosave edits are accepted; an unknown
/// key is a LOUD error (never silently dropped — a typo'd field must not look saved). Each value may
/// be a string or `null` (all three columns are nullable).
pub(crate) fn apply_curation_fields(
    segment: &mut SpeechSegment,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    fn opt_string(key: &str, v: &serde_json::Value) -> Result<Option<String>, String> {
        if v.is_null() {
            Ok(None)
        } else {
            v.as_str().map(str::to_string).map(Some).ok_or_else(|| format!("{key} must be a string or null"))
        }
    }
    for (key, value) in fields {
        match key.as_str() {
            "annotatedTranscript" => {
                let v = opt_string(key, value)?;
                if let Some(ref t) = v {
                    validate::validate_text(t, 100000, "Annotated transcript")?;
                }
                segment.annotated_transcript = v;
            }
            "speakerId" => {
                let v = opt_string(key, value)?;
                if let Some(ref s) = v {
                    if !s.is_empty() {
                        validate::validate_text(s, 256, "Speaker ID")?;
                    }
                }
                segment.speaker_id = v;
            }
            "alignmentJson" => {
                let v = opt_string(key, value)?;
                if let Some(ref aj) = v {
                    validate::validate_alignment_json(aj)?;
                }
                segment.alignment_json = v;
            }
            "verified" => {
                // Verifying is a single-field curation action. Routing it through this field-level path
                // (update_segment_fields reads the FRESH row by id, applies only this field, persists) —
                // instead of the whole-row api.updateSegment upsert handleToggleVerify used to send — means a
                // concurrent writer that holds NO $isProcessing lock, notably the WSL-7B refinement loop
                // (it emits wsl-log events, not batch-progress, so the Verify button stays live), cannot have
                // its raw_transcript write reverted by a stale whole-row spread. Matches the sibling
                // handleSaveAnnotation / handleSaveSpeaker conversions to field-level updates.
                segment.verified = value.as_bool().ok_or_else(|| format!("{key} must be a boolean"))?;
            }
            other => {
                return Err(format!(
                    "update_segment_fields: unsupported field '{other}' — only curation fields \
                     (annotatedTranscript, speakerId, alignmentJson, verified) may be partially updated"
                ));
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn merge_dataset_json(json_content: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("merge_dataset_json")?;
    // Sanity-bound the pasted payload (generous enough for a real multi-segment dataset) so a
    // pathological blob can't drive an unbounded parse — matching the size guard every other
    // JSON-accepting command applies.
    validate::validate_text(&json_content, 50_000_000, "Dataset JSON")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        let (created, updated) = db.merge_dataset_json(&json_content).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "created": created,
            "updated": updated
        }))
    })
    .await
}

/// Recent durable jobs (newest first) for a UI activity surface — a long op bracketed via
/// `Database::run_tracked` shows here as running/succeeded/failed, and a crash residue reaped at
/// startup shows as failed/INTERRUPTED. Cheap read; safe to poll.
#[tauri::command]
pub async fn get_jobs(state: State<'_, AppState>) -> Result<Vec<crate::jobs::Job>, String> {
    RATE_LIMITER.check("get_jobs")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        db.list_recent_jobs(50).map_err(|e| e.to_string())
    })
    .await
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    /// True only when the warm server reports the exact id + deployment SHA selected by registry.
    pub ready: bool,
    pub port: u16,
    pub identity_matches: bool,
    pub expected_model_version_id: Option<String>,
    pub expected_deployment_sha256: Option<String>,
    pub loaded_model_version_id: Option<String>,
    pub loaded_deployment_sha256: Option<String>,
    pub reason: Option<String>,
}

/// Renderer-safe registry row. The durable checkpoint path remains backend-only; the UI needs the
/// content identity and provenance, never a local filesystem location.
#[derive(serde::Serialize)]
pub struct ModelVersionSummary {
    pub id: String,
    pub family: String,
    pub model_card_name: Option<String>,
    pub checkpoint_sha256: String,
    pub source: String,
    pub license: String,
    pub status: String,
}

impl From<crate::registry::ModelVersion> for ModelVersionSummary {
    fn from(version: crate::registry::ModelVersion) -> Self {
        Self {
            id: version.id,
            family: version.family,
            model_card_name: version.model_card_name,
            checkpoint_sha256: version.checkpoint_sha256,
            source: version.source,
            license: version.license,
            status: version.status,
        }
    }
}

/// The model registry, newest-first within each family — what a registry panel lists.
#[tauri::command]
pub fn list_model_versions(state: State<'_, AppState>) -> Result<Vec<ModelVersionSummary>, String> {
    RATE_LIMITER.check("list_model_versions")?;
    let db = state.lock_db();
    crate::registry::list_model_versions(&db)
        .map(|versions| {
            versions
                .into_iter()
                .filter(|version| version.family == crate::deployment::OMNIASR_7B_FAMILY)
                .map(ModelVersionSummary::from)
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// Import an externally fine-tuned checkpoint into the registry as a gated candidate. The SHA is
/// computed server-side from the file; the caller never supplies it. Promotion is a separate,
/// gated step (not exposed yet — it must run through the eval gate), so this can only ever add a
/// candidate, never crown a champion.
#[tauri::command]
pub async fn import_model_checkpoint(
    id: String,
    checkpoint_path: String,
    source: String,
    license: String,
    model_card_name: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    STRICT_RATE_LIMITER.check("import_model_checkpoint")?;
    validate::validate_identifier(&id)?;
    validate::validate_identifier(&source)?;
    validate::validate_identifier(&license)?;
    if let Some(ref card) = model_card_name {
        validate::validate_text(card, 256, "model_card_name")?;
    }
    let checkpoint_path = validate::validate_file_path(&checkpoint_path)?;
    let db = state.db_arc();
    run_blocking(move || {
        // Own the restore fence in the worker itself. Cancelling the async IPC must not detach the
        // multi-GB hash from the generation it will eventually mutate.
        let _mutation = begin_mutation()?;
        // Hash the (potentially multi-GB) checkpoint off the main thread AND before taking the DB lock
        // — holding the global db mutex across the full-file SHA-256 would starve every UI DB poll.
        let sha = crate::registry::hash_checkpoint(&checkpoint_path).map_err(|e| e.to_string())?;
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::registry::register_checkpoint(
            &db,
            &id,
            crate::deployment::OMNIASR_7B_FAMILY,
            &checkpoint_path,
            &source,
            &license,
            model_card_name,
            sha,
        )
        .map_err(|e| e.to_string())
    })
    .await
}

/// Import a content-addressed OmniASR-7B deployment. Identity comes from the verified manifest,
/// never from renderer-supplied model/card fields, and all four behavior-determining components are
/// hashed before the DB lock is acquired.
#[tauri::command]
pub async fn import_model_deployment(
    manifest_path: String,
    expected_deployment_sha256: String,
    expected_model_id: String,
    source: String,
    license: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ModelVersionSummary, String> {
    STRICT_RATE_LIMITER.check("import_model_deployment")?;
    validate::validate_identifier(&source)?;
    validate::validate_identifier(&license)?;
    validate::validate_identifier(&expected_model_id)?;
    if expected_deployment_sha256.len() != 64
        || !expected_deployment_sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("expectedDeploymentSha256 must be a canonical lowercase SHA-256".into());
    }
    if manifest_path.trim().is_empty() || manifest_path.len() > 4096 || manifest_path.chars().any(char::is_control) {
        return Err("manifestPath is empty, too long, or contains control characters".into());
    }
    let db = state.db_arc();
    run_blocking(move || {
        // Manifest verification can take ten minutes. It and the final registry write are one
        // generation-bound mutation, and the guard must outlive cancellation of the async caller.
        let _mutation = begin_mutation()?;
        let verified = if manifest_path.starts_with('/') {
            let server = crate::engine_runtime::server_script_path(&app)
                .ok_or_else(|| "bundled cortex_7b_server.py verifier could not be resolved".to_string())?;
            crate::deployment::verify_deployment_manifest_wsl(
                &server,
                &manifest_path,
                &expected_deployment_sha256,
                &expected_model_id,
                std::time::Duration::from_secs(10 * 60),
            )
            .map_err(|error| error.to_string())?
        } else {
            let local = validate::validate_file_path(&manifest_path)?;
            let local = crate::deployment::verify_deployment_manifest(
                std::path::Path::new(&local),
                Some(&expected_deployment_sha256),
            )
            .map_err(|error| error.to_string())?;
            if local.manifest.model_id != expected_model_id {
                return Err(format!(
                    "deployment manifest model id '{}' does not match expectedModelId '{}'",
                    local.manifest.model_id, expected_model_id
                ));
            }
            local.record()
        };
        let db = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::registry::register_verified_deployment_record(&db, &verified, &source, &license)
            .map(ModelVersionSummary::from)
            .map_err(|error| error.to_string())
    })
    .await
}

/// One-time admission of the historically measured incumbent. This is deliberately a different
/// command from challenger import: the registry family must be completely empty and the verified
/// composite must match every owner-measured legacy pin. It cannot be reused for a future model.
#[tauri::command]
pub async fn bootstrap_legacy_champion(
    manifest_path: String,
    expected_deployment_sha256: String,
    expected_model_id: String,
    license: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ModelVersionSummary, String> {
    STRICT_RATE_LIMITER.check("bootstrap_legacy_champion")?;
    validate::validate_identifier(&expected_model_id)?;
    validate::validate_identifier(&license)?;
    if expected_deployment_sha256.len() != 64
        || !expected_deployment_sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("expectedDeploymentSha256 must be a canonical lowercase SHA-256".into());
    }
    if manifest_path.trim().is_empty() || manifest_path.len() > 4096 || manifest_path.chars().any(char::is_control) {
        return Err("manifestPath is empty, too long, or contains control characters".into());
    }
    let db = state.db_arc();
    let data_dir =
        state.lock_data_dir().clone().ok_or_else(|| "application data directory is unavailable".to_string())?;
    run_blocking(move || {
        // Champion publication spans external verification, a registry transaction, and an atomic
        // pointer update. Never allow a restore to split those across database generations.
        let _mutation = begin_mutation()?;
        let verified = if manifest_path.starts_with('/') {
            let server = crate::engine_runtime::server_script_path(&app)
                .ok_or_else(|| "bundled cortex_7b_server.py verifier could not be resolved".to_string())?;
            crate::deployment::verify_deployment_manifest_wsl(
                &server,
                &manifest_path,
                &expected_deployment_sha256,
                &expected_model_id,
                std::time::Duration::from_secs(10 * 60),
            )
            .map_err(|error| error.to_string())?
        } else {
            let local = validate::validate_file_path(&manifest_path)?;
            let local = crate::deployment::verify_deployment_manifest(
                std::path::Path::new(&local),
                Some(&expected_deployment_sha256),
            )
            .map_err(|error| error.to_string())?;
            if local.manifest.model_id != expected_model_id {
                return Err("legacy deployment model id does not match expectedModelId".into());
            }
            local.record()
        };
        let db = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let model = crate::registry::bootstrap_verified_legacy_deployment(&db, &verified, &license)
            .map_err(|error| error.to_string())?;
        crate::registry::sync_champion_pointer(&db, &data_dir).map_err(|error| error.to_string())?;
        Ok(ModelVersionSummary::from(model))
    })
    .await
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
pub fn db_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let db = state.lock_db();
    db.info().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn db_backup(dest: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let validated = validate::validate_output_path(&dest)?;
    // Dedicated connection (true-10 audit 2026-07-09): holding the global DB mutex for the whole
    // online backup froze every DB-touching command for the full copy duration on a slow external
    // drive. Grab the path under a brief lock, then do the whole copy + verify OFF the main thread.
    let db_path = {
        let db = state.lock_db();
        db.path().to_string()
    };
    run_blocking(move || {
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
    })
    .await
}

/// One admission boundary for every AppState DB lock and every restore reservation. A normal caller
/// takes `admission` through the point where it owns the DB mutex. A restore takes `admission`, wins
/// the pending flag with compare-exchange, then keeps the reservation until its DB, history, settings,
/// and pipeline work is complete. Therefore a caller is ordered either wholly before the mandatory
/// pre-restore snapshot or after the complete restore; it cannot be stranded in between.
struct RestoreAdmission {
    pending: std::sync::atomic::AtomicBool,
    admission: std::sync::Mutex<RestoreAdmissionState>,
    complete: std::sync::Condvar,
}

#[derive(Debug, Default)]
struct RestoreAdmissionState {
    captures_active: usize,
    mutations_active: usize,
    generation: u64,
    phase: RestorePhase,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum RestorePhase {
    #[default]
    Idle,
    ActiveNew,
    ActiveArmed,
    Parked,
}

impl RestoreAdmission {
    const fn new() -> Self {
        Self {
            pending: std::sync::atomic::AtomicBool::new(false),
            admission: std::sync::Mutex::new(RestoreAdmissionState {
                captures_active: 0,
                mutations_active: 0,
                generation: 0,
                phase: RestorePhase::Idle,
            }),
            complete: std::sync::Condvar::new(),
        }
    }

    fn is_pending(&self) -> bool {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn try_reserve(&self) -> Result<RestoreReservation<'_>, String> {
        self.reserve(false)
    }

    fn claim_recovery(&self) -> Result<RestoreReservation<'_>, String> {
        self.reserve(true)
    }

    fn reserve(&self, recovery_required: bool) -> Result<RestoreReservation<'_>, String> {
        let mut admission = self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match admission.phase {
            RestorePhase::Idle => {
                self.pending
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .map_err(|_| "Database restore admission is inconsistent; restart before retrying.".to_string())?;
                admission.phase = if recovery_required { RestorePhase::ActiveArmed } else { RestorePhase::ActiveNew };
            }
            RestorePhase::Parked if recovery_required => {
                // Exact retry reclaims the parked state; `pending` deliberately stayed true while no
                // command-local token existed.
                self.pending.store(true, std::sync::atomic::Ordering::SeqCst);
                admission.phase = RestorePhase::ActiveArmed;
            }
            RestorePhase::Parked => {
                return Err("An interrupted database restore is recovery-required; retry its exact recorded snapshot."
                    .to_string());
            }
            RestorePhase::ActiveNew | RestorePhase::ActiveArmed => {
                return Err("A database restore is already in progress — wait for it to finish.".to_string());
            }
        }
        admission.generation = admission.generation.wrapping_add(1);
        let generation = admission.generation;
        // Setting pending first prevents a new capture from entering while an already-active capture
        // drains. The returned token contains only a shared reference, so it remains Send across await.
        while admission.captures_active > 0 {
            admission = self.complete.wait(admission).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if admission.mutations_active > 0 {
            if admission.phase == RestorePhase::ActiveArmed {
                admission.phase = RestorePhase::Parked;
                self.pending.store(true, std::sync::atomic::Ordering::SeqCst);
            } else {
                admission.phase = RestorePhase::Idle;
                self.pending.store(false, std::sync::atomic::Ordering::SeqCst);
                self.complete.notify_all();
            }
            return Err(
                "A database or configuration mutation is already in progress — let it finish before restoring."
                    .to_string(),
            );
        }
        Ok(RestoreReservation { admission: self, generation })
    }

    fn begin_capture(&self) -> Result<SnapshotCaptureReservation<'_>, String> {
        let mut admission = self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_pending() {
            return Err("snapshot refused because a database restore is reserved/in progress".to_string());
        }
        admission.captures_active = admission.captures_active.saturating_add(1);
        Ok(SnapshotCaptureReservation { admission: self })
    }

    fn begin_mutation(&self) -> Result<MutationGuard<'_>, String> {
        let mut admission = self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if admission.phase != RestorePhase::Idle || self.is_pending() {
            return Err(RESTORE_IN_PROGRESS_MSG.to_string());
        }
        admission.mutations_active = admission.mutations_active.saturating_add(1);
        Ok(MutationGuard { admission: self })
    }

    fn lock<'a, T>(&self, mutex: &'a std::sync::Mutex<T>) -> std::sync::LockResult<std::sync::MutexGuard<'a, T>> {
        let mut admission = self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while self.is_pending() {
            admission = self.complete.wait(admission).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        // Keep admission until the caller OWNS the DB mutex. Otherwise a caller already queued here
        // could lose the race to a new reservation, run after its snapshot, and be overwritten.
        let database = mutex.lock();
        drop(admission);
        database
    }
}

static RESTORE_ADMISSION: RestoreAdmission = RestoreAdmission::new();

/// Fence a full mutation that can release AppState's DB mutex, write through a dedicated connection,
/// or change dataset-coupled configuration. The guard must live from before the first read/inference
/// until after the final DB/config publication.
pub(crate) fn begin_mutation() -> Result<MutationGuard<'static>, String> {
    RESTORE_ADMISSION.begin_mutation()
}

/// Admit one complete rotating/offsite snapshot capture. The returned guard must span DB backup,
/// config/policy capture, manifest verification, and promotion. A durable marker from a crashed
/// named restore is a hard stop: capturing that mixed generation would create a plausible-looking
/// artifact that can never be trusted.
pub(crate) fn begin_snapshot_capture(primary_data_dir: &Path) -> Result<SnapshotCaptureReservation<'static>, String> {
    let capture = RESTORE_ADMISSION.begin_capture()?;
    let pending = primary_data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
    match std::fs::symlink_metadata(&pending) {
        Ok(_) => Err(format!(
            "snapshot refused while interrupted restore barrier {} exists; complete or repair that restore first",
            pending.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(capture),
        Err(error) => Err(format!("snapshot could not inspect restore barrier {}: {error}", pending.display())),
    }
}

/// Send-safe RAII token for a complete snapshot capture. It owns no mutex guard across long I/O.
pub(crate) struct SnapshotCaptureReservation<'a> {
    admission: &'a RestoreAdmission,
}

/// Send-safe full-operation mutation token. Restore admission checks this count after publishing its
/// pending state, so an existing long operation makes restore fail and a new one cannot start after
/// restore publication.
pub(crate) struct MutationGuard<'a> {
    admission: &'a RestoreAdmission,
}

impl Drop for MutationGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.mutations_active = state.mutations_active.saturating_sub(1);
        self.admission.complete.notify_all();
    }
}

impl Drop for SnapshotCaptureReservation<'_> {
    fn drop(&mut self) {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.captures_active = state.captures_active.saturating_sub(1);
        self.admission.complete.notify_all();
    }
}

/// True while a DB restore is reserved/in progress — consulted by every writer-START path.
pub(crate) fn restore_pending() -> bool {
    RESTORE_ADMISSION.is_pending()
}

/// The only ordinary AppState DB-lock entry point. Both `AppState::lock_db` and its clonable blocking
/// handle delegate here, so queued foreground writes cannot cross a restore boundary.
pub(crate) fn lock_app_db<'a>(
    db: &'a std::sync::Mutex<crate::db::Database>,
) -> std::sync::LockResult<std::sync::MutexGuard<'a, crate::db::Database>> {
    RESTORE_ADMISSION.lock(db)
}

/// The uniform error a writer-start returns while a restore is pending.
pub(crate) const RESTORE_IN_PROGRESS_MSG: &str =
    "A database restore is in progress — wait for it to finish before starting this operation.";

/// Exclusive RAII reservation. Drop releases an unarmed restore, but parks an armed named restore so
/// a surviving durable marker can never reopen ordinary DB/config writes.
pub(crate) struct RestoreReservation<'a> {
    admission: &'a RestoreAdmission,
    generation: u64,
}

impl RestoreReservation<'_> {
    /// Prove that this exact reservation still owns its admission boundary. Snapshot helpers accept
    /// the reservation explicitly instead of consulting process-global state, so tests can exercise
    /// the production ownership protocol without blocking unrelated parallel tests.
    pub(crate) fn is_active(&self) -> bool {
        let state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation == self.generation
            && matches!(state.phase, RestorePhase::ActiveNew | RestorePhase::ActiveArmed)
    }

    /// Arm fail-closed parking BEFORE attempting the durable marker write. If the write fails cleanly
    /// the caller may disarm; if it panics or leaves uncertain bytes, Drop parks rather than reopening.
    fn arm_named_restore(&self) -> Result<(), String> {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != self.generation {
            return Err("restore reservation lost ownership before durable transaction arm".to_string());
        }
        match state.phase {
            RestorePhase::ActiveNew => {
                state.phase = RestorePhase::ActiveArmed;
                Ok(())
            }
            RestorePhase::ActiveArmed => Ok(()),
            RestorePhase::Idle | RestorePhase::Parked => {
                Err("restore reservation is not active while arming durable transaction".to_string())
            }
        }
    }

    fn disarm_named_restore_if_safe(&self) {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation == self.generation && state.phase == RestorePhase::ActiveArmed {
            state.phase = RestorePhase::ActiveNew;
        }
    }

    fn commit_named_restore(&self) -> Result<(), String> {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != self.generation || state.phase != RestorePhase::ActiveArmed {
            return Err("restore transaction lost its exclusive admission before commit".to_string());
        }
        state.phase = RestorePhase::Idle;
        self.admission.pending.store(false, std::sync::atomic::Ordering::SeqCst);
        self.admission.complete.notify_all();
        Ok(())
    }
}

impl Drop for RestoreReservation<'_> {
    fn drop(&mut self) {
        let mut state = self.admission.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != self.generation {
            return;
        }
        match state.phase {
            RestorePhase::ActiveNew => {
                state.phase = RestorePhase::Idle;
                self.admission.pending.store(false, std::sync::atomic::Ordering::SeqCst);
                self.admission.complete.notify_all();
            }
            RestorePhase::ActiveArmed => {
                state.phase = RestorePhase::Parked;
                tracing::error!(
                    "named restore remains incomplete; database admission is parked until exact recovery completes"
                );
            }
            RestorePhase::Idle | RestorePhase::Parked => {}
        }
    }
}

fn take_mandatory_pre_restore_snapshot(
    reservation: &RestoreReservation<'_>,
    db: &crate::db::Database,
    data_dir: &Path,
) -> Result<std::path::PathBuf, String> {
    crate::snapshot::take_pinned_snapshot_during_restore(reservation, db, data_dir, "prerestore", 3).map_err(
        |e| {
            format!(
                "Database restore refused because the mandatory pre-restore safety snapshot failed: {e}. \
                 The current library has not been overwritten. Free disk space or fix the destination permissions, then retry."
            )
        },
    )
}

/// With ONE caller-owned DB mutex guard, pin the current live database and then replace it. Keeping
/// both operations in this helper prevents a queued write from landing after the safety snapshot and
/// being silently discarded by the restore.
fn restore_with_mandatory_snapshot(
    reservation: &RestoreReservation<'_>,
    db: &mut crate::db::Database,
    data_dir: &Path,
    source: &Path,
) -> Result<std::path::PathBuf, String> {
    // Prove and fully migrate the source in isolation first. A bad source creates neither a safety
    // pin nor a durable review barrier and cannot touch a live page.
    let staged = crate::db::Database::stage_restore_source(source).map_err(|error| error.to_string())?;
    let pinned = take_mandatory_pre_restore_snapshot(reservation, db, data_dir)?;
    tracing::info!("pre-restore snapshot pinned at {}", pinned.display());
    db.commit_staged_restore(&staged).map_err(|error| error.to_string())?;
    Ok(pinned)
}

fn pin_selector(data_dir: &Path, pin: &Path) -> Result<String, String> {
    let relative = pin
        .strip_prefix(data_dir.join("snapshots"))
        .map_err(|_| format!("pre-restore pin {} is outside the snapshot tree", pin.display()))?;
    let parts = relative
        .components()
        .map(|component| component.as_os_str().to_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "pre-restore pin path is not UTF-8".to_string())?;
    if parts.len() != 2 || parts[0] != "pinned" {
        return Err(format!("pre-restore pin {} has an unexpected path", pin.display()));
    }
    Ok(format!("pinned/{}", parts[1]))
}

/// Reuse the original safety pin for every retry of an interrupted named restore. Creating a new
/// `keep=3` pin per retry could evict the only copy of the true pre-restore generation.
fn begin_named_restore_transaction(
    reservation: &RestoreReservation<'_>,
    db: &crate::db::Database,
    data_dir: &Path,
    source_selector: &str,
) -> Result<std::path::PathBuf, String> {
    if let Some(pending) = load_named_restore_pending(data_dir)? {
        if let Some(completed) = pending.completed_selector.as_deref() {
            return Err(format!(
                "restore generation '{completed}' is already complete; only durable barrier cleanup may run"
            ));
        }
        if pending.source_selector != source_selector {
            return Err(format!(
                "an interrupted restore of '{}' is pending; retry that exact snapshot before selecting '{}'",
                pending.source_selector, source_selector
            ));
        }
        let pin = crate::snapshot::resolve_snapshot_dir(data_dir, &pending.pre_restore_pin_selector)?;
        if !crate::snapshot::verify_snapshot_manifest_for_restore(&pin)? {
            return Err(
                "the pending restore's original safety pin is legacy/unverifiable; refusing to continue".to_string()
            );
        }
        tracing::info!("reusing interrupted restore safety pin at {}", pin.display());
        return Ok(pin);
    }
    let pin = take_mandatory_pre_restore_snapshot(reservation, db, data_dir)?;
    let pending = NamedRestorePending {
        schema: NAMED_RESTORE_PENDING_SCHEMA,
        source_selector: source_selector.to_string(),
        pre_restore_pin_selector: pin_selector(data_dir, &pin)?,
        completed_selector: None,
    };
    // This is the commit boundary: source/config preflight and the safety pin already succeeded;
    // the durable fail-closed marker lands immediately before the live SQLite page transaction.
    reservation.arm_named_restore()?;
    if let Err(error) = write_named_restore_pending(data_dir, &pending) {
        if !named_restore_barrier_may_exist(data_dir) {
            reservation.disarm_named_restore_if_safe();
        }
        return Err(error);
    }
    Ok(pin)
}

fn prepare_named_restore_artifacts<F>(
    snapshot_dir: &Path,
    source: &Path,
    before_final_verify: F,
) -> Result<(SnapshotRestorePlan, crate::db::Database), String>
where
    F: FnOnce(),
{
    let manifest_verified = crate::snapshot::verify_snapshot_manifest_for_restore(snapshot_dir)?;
    let plan = inspect_snapshot_restore_plan(snapshot_dir, source, manifest_verified)?;
    let staged = crate::db::Database::stage_restore_source(source).map_err(|error| error.to_string())?;
    before_final_verify();
    // Re-hash after BOTH config-plan capture and DB staging. From this point onward commit uses only
    // owned plan bytes + the staged in-memory DB, so a promoted-source mutation cannot cross the
    // manifest boundary or create a mixed DB/config generation.
    let reverified = crate::snapshot::verify_snapshot_manifest_for_restore(snapshot_dir)?;
    if reverified != manifest_verified {
        return Err("snapshot manifest presence changed during restore preflight".to_string());
    }
    Ok((plan, staged))
}

fn prepare_and_restore_named_transaction(
    reservation: &RestoreReservation<'_>,
    db: &mut crate::db::Database,
    data_dir: &Path,
    snapshot_dir: &Path,
    source: &Path,
    source_selector: &str,
) -> Result<SnapshotRestorePlan, String> {
    let (plan, staged) = prepare_named_restore_artifacts(snapshot_dir, source, || {})?;
    let _pin = begin_named_restore_transaction(reservation, db, data_dir, source_selector)?;
    db.commit_staged_restore(&staged).map_err(|error| error.to_string())?;
    Ok(plan)
}

/// Shared restore precondition (true-10 audit 2026-07-09): refuse while an import/batch worker may
/// be writing, and pin a rotation-exempt copy of the CURRENT live DB first so a mis-restore of the
/// wrong snapshot is itself recoverable (previously only from a ≤10-min rolling snapshot that
/// rotated out within ~100 minutes). Returns a RestoreReservation the caller MUST hold across the
/// restore so no new writer can start mid-restore (P1.3b).
fn prepare_restore(state: &State<'_, AppState>) -> Result<(RestoreReservation<'static>, std::path::PathBuf), String> {
    // Reserve FIRST: set RESTORE_PENDING before checking writers_active, so a writer racing this check
    // observes the reservation and refuses. THEN verify none is already running (the fence). The
    // reservation is closed by ONE OF TWO airtight mechanisms per writer, NOT a single shared lock:
    //   • import/batch and couch::start check restore_pending() UNDER the same mutex writers_active()
    //     reads (import_state / batch_state / COUCH), so their check+register is totally ordered against
    //     the fence read — a concurrent restore either sees them registered or is seen by them.
    //   • the atomic/counter writers (WSL refine and jury via BG_DB_WRITERS) use publish-then-
    //     recheck: they SET their flag, then RE-READ the reservation and roll back if set. Under SeqCst
    //     the fence's {store RESTORE_PENDING; load flag} and the writer's {store flag; load RESTORE_PENDING}
    //     can't both read stale, so one side always refuses.
    // Resolve only immutable process state before reserving. The race is closed by publishing the
    // reservation BEFORE writers_active(), not by reading data_dir first.
    let data_dir = state
        .lock_data_dir()
        .clone()
        .ok_or_else(|| "Database restore refused: the app data directory is unavailable, so a mandatory pre-restore safety snapshot cannot be created.".to_string())?;
    // An earlier post-swap failure parks admission with the durable marker still present. Exact retry
    // reclaims that state; an ordinary restore can only reserve from Idle.
    let reservation = if named_restore_barrier_may_exist(&data_dir) {
        RESTORE_ADMISSION.claim_recovery()?
    } else {
        RESTORE_ADMISSION.try_reserve()?
    };
    if state.writers_active() {
        return Err("A background write is in progress (import, batch, 7B refinement, jury, or the \
                    Couch Review server) — cancel it, let it finish, or stop Couch \
                    Review before restoring. Restoring mid-write would mix pre-restore rows into the \
                    restored library and re-arm stale undo history."
            .to_string());
    }
    Ok((reservation, data_dir))
}

#[tauri::command]
pub async fn db_restore(src: String, state: State<'_, AppState>) -> Result<(), String> {
    // M0.4: Restore a previously backed-up database snapshot. The file must be a valid SQLite
    // database (PRAGMA integrity_check on open verifies this). This completes the backup/restore
    // pair so the app is never at risk of data loss mid-import or mid-review.
    let validated = validate::validate_file_path(&src)?;
    // Hold the reservation across the whole restore: RESTORE_PENDING stays set until this guard drops
    // (after the page swap + history clear), so no new writer can start mid-restore (P1.3b).
    let (restore_reservation, data_dir) = prepare_restore(&state)?;
    refuse_bare_restore_during_controlled_pilot(&data_dir)?;
    // Heavy snapshot + DB file-copy + reopen run off the main thread under ONE uninterrupted raw DB
    // mutex guard. Ordinary raw access is not exposed; every other AppState handle is restore-gated.
    let db = state.db_arc_for_restore();
    let history = state.history_arc_for_restore();
    let restore_reservation = run_blocking(move || {
        let mut guard = db.lock().unwrap_or_else(|p| p.into_inner());
        restore_with_mandatory_snapshot(&restore_reservation, &mut guard, &data_dir, Path::new(&validated))?;
        // Clear old-row undo commands in the same worker, before its reservation can be dropped. A
        // cancelled Tauri future detaches spawn_blocking; cleanup after `.await` would then be lost
        // even though the page publication completed successfully.
        history.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
        Ok(restore_reservation)
    })
    .await?;
    drop(restore_reservation);
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
pub async fn db_vacuum(state: State<'_, AppState>) -> Result<(), String> {
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        db.vacuum().map_err(|e| e.to_string())
    })
    .await
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

/// B2: list the rotating auto-snapshots (newest first) for the restore picker.
#[tauri::command]
pub fn list_db_snapshots(state: State<'_, AppState>) -> Result<Vec<crate::snapshot::SnapshotInfo>, String> {
    RATE_LIMITER.check("list_db_snapshots")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    Ok(crate::snapshot::list_snapshots(&data_dir))
}

/// A dataset snapshot may restore dataset-coupled thresholds, but it must never change which ASR
/// engine the operator is currently running or re-enable heavyweight background inference. Those
/// are live machine/runtime decisions, not historical dataset state.
fn preserve_live_asr_runtime_controls(restored: &mut AppSettings, live: &AppSettings) {
    restored.asr_model_size = live.asr_model_size.clone();
    restored.use_finetuned_asr = live.use_finetuned_asr;
    restored.multi_engine_hypotheses = live.multi_engine_hypotheses;
    restored.external_asr_script_path = live.external_asr_script_path.clone();
    restored.champion_supervision_enabled = live.champion_supervision_enabled;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotPilotPolicyRestore {
    Install(Vec<u8>),
    ExplicitlyAbsent,
    /// Snapshots made before explicit absence markers preserve the current live policy. This keeps
    /// historical DB recovery possible without ever interpreting missing legacy state as permission
    /// to delete/relax a live paid-review cap.
    PreserveLegacy,
}

fn refuse_bare_restore_during_controlled_pilot(data_dir: &Path) -> Result<(), String> {
    match crate::review_pilot::load(data_dir) {
        Ok(None) => match crate::couch::durable_controlled_pilot_state(data_dir) {
            Ok(false) => Ok(()),
            Ok(true) => Err(
                "Bare database restore is refused because the durable Couch session retains a controlled paid-review baseline. Use a policy-bearing named snapshot restore instead."
                    .to_string(),
            ),
            Err(error) => Err(format!(
                "Bare database restore is refused because durable Couch pilot state is not provably safe: {error}"
            )),
        },
        Ok(Some(_)) => Err(
            "Bare database restore is refused while a controlled paid-review pilot is active: its external baseline/policy would no longer match review_events. Use a policy-bearing named snapshot restore instead."
                .to_string(),
        ),
        Err(error) => Err(format!(
            "Bare database restore is refused because controlled paid-review state is not provably safe: {error}"
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NamedRestorePending {
    schema: u32,
    source_selector: String,
    pre_restore_pin_selector: String,
    /// Written only after DB + every required config/settings file has committed. If marker cleanup
    /// then fails or the process crashes, startup clears the barrier without replaying or rolling
    /// back an already-coherent generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_selector: Option<String>,
}

const NAMED_RESTORE_PENDING_SCHEMA: u32 = 2;

fn atomic_write_restore_state(path: &Path, bytes: &[u8]) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            return Err(format!("restore destination {} must be a regular file or absent", path.display()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not inspect restore destination {}: {error}", path.display())),
    }
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("restore-state");
    let temp = path.with_file_name(format!(".{file_name}.restore-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    if let Err(error) = std::fs::write(&temp, bytes) {
        return Err(format!("could not stage {}: {error}", path.display()));
    }
    if let Err(error) = crate::atomic_file::replace_file(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not atomically install {}: {error}", path.display()));
    }
    Ok(())
}

fn remove_live_restore_state(destination: &Path) -> Result<(), String> {
    crate::atomic_file::recover_interrupted_replace(destination)
        .map_err(|error| format!("could not recover {} before explicit removal: {error}", destination.display()))?;
    // Remove recoverable backups FIRST while the canonical file still exists. If cleanup is blocked
    // by an antivirus/indexer lock, returning here leaves the old committed state intact; deleting
    // canonical first could let a leftover backup resurrect it after we had reported absence.
    crate::atomic_file::remove_replacement_backups(destination)
        .map_err(|error| format!("could not remove stale backups for {}: {error}", destination.display()))?;
    match std::fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not remove {}: {error}", destination.display())),
    }
    Ok(())
}

fn restore_required_snapshot_state_atomic(
    plan: &[(crate::snapshot::OptionalSnapshotState, crate::snapshot::OptionalSnapshotRestore)],
    data_dir: &Path,
) -> Result<(), String> {
    for (state, action) in plan {
        if state.live_file == "settings.json" {
            continue;
        }
        let destination = data_dir.join(state.live_file);
        match action {
            crate::snapshot::OptionalSnapshotRestore::Install(bytes) => {
                atomic_write_restore_state(&destination, bytes).map_err(|error| {
                    format!("required snapshot state {} could not be installed atomically: {error}", state.live_file)
                })?;
            }
            crate::snapshot::OptionalSnapshotRestore::ExplicitlyAbsent => {
                remove_live_restore_state(&destination).map_err(|error| {
                    format!("required snapshot state {} could not be made explicitly absent: {error}", state.live_file)
                })?;
            }
            crate::snapshot::OptionalSnapshotRestore::PreserveLegacy => {}
        }
    }
    Ok(())
}

fn inspect_snapshot_pilot_policy(
    snapshot_dir: &Path,
    snapshot_db: &Path,
    manifest_verified: bool,
) -> Result<SnapshotPilotPolicyRestore, String> {
    let policy_path = snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_FILE);
    let absent_path = snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE);
    let read_optional = |path: &Path| -> Result<Option<Vec<u8>>, String> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("snapshot state {} is unreadable: {error}", path.display())),
        }
    };
    match (read_optional(&policy_path)?, read_optional(&absent_path)?) {
        (Some(_), Some(_)) => Err(format!(
            "snapshot is ambiguous: it contains both {} and {}",
            crate::review_pilot::REVIEW_PILOT_FILE,
            crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
        )),
        (Some(bytes), None) => {
            if !manifest_verified {
                return Err(
                    "policy-bearing named snapshot restore requires a verified manifest that cryptographically binds its database, policy, and exact voice focus"
                        .to_string(),
                );
            }
            let raw = std::str::from_utf8(&bytes).map_err(|error| {
                format!("snapshot {} is not UTF-8: {error}", crate::review_pilot::REVIEW_PILOT_FILE)
            })?;
            let policy = crate::review_pilot::parse(raw)?;
            // A policy-bearing artifact is one indivisible DB + policy + exact-focus generation.
            // This applies equally to manifestless legacy snapshots: preserving or inferring a
            // missing focus would turn a bounded campaign into a different paid workload.
            crate::review_pilot::validate_controlled_focus(snapshot_dir)
                .map_err(|error| format!("snapshot controlled-pilot focus is invalid: {error}"))?;
            let source = crate::db::Database::open_immutable_connection(snapshot_db)
                .map_err(|error| format!("snapshot pilot policy could not bind to its database: {error}"))?;
            let max_event_id: i64 = source
                .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
                .map_err(|error| format!("snapshot pilot baseline could not be verified: {error}"))?;
            if policy.after_review_event_id > max_event_id {
                return Err(format!(
                    "snapshot pilot baseline {} is ahead of its database review-event maximum {max_event_id}",
                    policy.after_review_event_id
                ));
            }
            let mut canonical = serde_json::to_vec_pretty(&policy)
                .map_err(|error| format!("snapshot pilot policy could not be canonicalized: {error}"))?;
            canonical.push(b'\n');
            Ok(SnapshotPilotPolicyRestore::Install(canonical))
        }
        (None, Some(marker)) => {
            if marker != crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES {
                return Err(format!(
                    "snapshot {} has invalid contents",
                    crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
                ));
            }
            Ok(SnapshotPilotPolicyRestore::ExplicitlyAbsent)
        }
        (None, None) => {
            if manifest_verified {
                return Err(format!(
                    "manifest-bearing snapshot is missing both {} and {}",
                    crate::review_pilot::REVIEW_PILOT_FILE,
                    crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
                ));
            }
            tracing::warn!(
                "LEGACY MANIFEST-LESS SNAPSHOT: neither {} nor {} is present; preserving the current live paid-review policy exactly",
                crate::review_pilot::REVIEW_PILOT_FILE,
                crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
            );
            Ok(SnapshotPilotPolicyRestore::PreserveLegacy)
        }
    }
}

#[derive(Debug, Clone)]
struct SnapshotRestorePlan {
    pilot: SnapshotPilotPolicyRestore,
    optional: Vec<(crate::snapshot::OptionalSnapshotState, crate::snapshot::OptionalSnapshotRestore)>,
}

fn inspect_snapshot_restore_plan(
    snapshot_dir: &Path,
    snapshot_db: &Path,
    manifest_verified: bool,
) -> Result<SnapshotRestorePlan, String> {
    let pilot = inspect_snapshot_pilot_policy(snapshot_dir, snapshot_db, manifest_verified)?;
    let optional = crate::snapshot::OPTIONAL_SNAPSHOT_STATE
        .iter()
        .copied()
        .map(|state| {
            crate::snapshot::inspect_optional_state_for_restore(snapshot_dir, state, manifest_verified)
                .map(|action| (state, action))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SnapshotRestorePlan { pilot, optional })
}

fn load_named_restore_pending(data_dir: &Path) -> Result<Option<NamedRestorePending>, String> {
    let pending = data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
    crate::atomic_file::recover_interrupted_replace(&pending)
        .map_err(|error| format!("could not recover the paid-review restore barrier: {error}"))?;
    let bytes = match std::fs::read(&pending) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read restore transaction {}: {error}", pending.display())),
    };
    let state: NamedRestorePending = serde_json::from_slice(&bytes).map_err(|error| {
        format!("restore transaction {} is invalid and paid review remains blocked: {error}", pending.display())
    })?;
    if !matches!(state.schema, 1 | NAMED_RESTORE_PENDING_SCHEMA) {
        return Err(format!("unsupported restore transaction schema {}", state.schema));
    }
    if state.schema == 1 && state.completed_selector.is_some() {
        return Err("legacy restore transaction cannot claim a completed generation".to_string());
    }
    if let Some(completed) = state.completed_selector.as_deref() {
        if completed != state.source_selector && completed != state.pre_restore_pin_selector {
            return Err("restore transaction completion selector is not its target or original pin".to_string());
        }
    }
    Ok(Some(state))
}

/// Conservatively decide whether dropping a restore command must keep the process-wide admission
/// fence parked. Invalid/unreadable marker state is recovery-required too: uncertainty can never be
/// interpreted as permission to resume writes.
fn named_restore_barrier_may_exist(data_dir: &Path) -> bool {
    match load_named_restore_pending(data_dir) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            tracing::error!("restore barrier is invalid or unreadable; keeping database fenced: {error}");
            true
        }
    }
}

fn write_named_restore_pending(data_dir: &Path, state: &NamedRestorePending) -> Result<(), String> {
    if let Some(existing) = load_named_restore_pending(data_dir)? {
        return (existing == *state).then_some(()).ok_or_else(|| {
            format!(
                "another interrupted restore transaction is pending for '{}'; retry that exact snapshot before selecting '{}'",
                existing.source_selector, state.source_selector
            )
        });
    }
    let mut bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("could not serialize restore transaction: {error}"))?;
    bytes.push(b'\n');
    atomic_write_restore_state(&data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), &bytes)
}

fn mark_named_restore_completed(data_dir: &Path, completed_selector: &str) -> Result<(), String> {
    let mut pending = load_named_restore_pending(data_dir)?
        .ok_or_else(|| "restore completion cannot be recorded because its durable marker is missing".to_string())?;
    if completed_selector != pending.source_selector && completed_selector != pending.pre_restore_pin_selector {
        return Err("restore completion selector is not the recorded target or original pin".to_string());
    }
    if let Some(existing) = pending.completed_selector.as_deref() {
        return (existing == completed_selector)
            .then_some(())
            .ok_or_else(|| format!("restore was already completed with a different generation '{existing}'"));
    }
    pending.schema = NAMED_RESTORE_PENDING_SCHEMA;
    pending.completed_selector = Some(completed_selector.to_string());
    let mut bytes = serde_json::to_vec_pretty(&pending)
        .map_err(|error| format!("could not serialize completed restore transaction: {error}"))?;
    bytes.push(b'\n');
    atomic_write_restore_state(&data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), &bytes)
}

fn apply_snapshot_pilot_policy(plan: &SnapshotPilotPolicyRestore, data_dir: &Path) -> Result<(), String> {
    let live = data_dir.join(crate::review_pilot::REVIEW_PILOT_FILE);
    match plan {
        SnapshotPilotPolicyRestore::Install(bytes) => atomic_write_restore_state(&live, bytes),
        SnapshotPilotPolicyRestore::ExplicitlyAbsent => remove_live_restore_state(&live)
            .map_err(|error| format!("could not apply explicit no-pilot snapshot state: {error}")),
        SnapshotPilotPolicyRestore::PreserveLegacy => Ok(()),
    }
}

fn clear_review_pilot_restore_pending(data_dir: &Path) -> Result<(), String> {
    let pending = data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
    // Canonical marker removal is the FINAL commit point. Backups must disappear first; otherwise a
    // cleanup failure after canonical removal could let load-time atomic recovery resurrect a barrier
    // after the in-process admission guard had already been released.
    crate::atomic_file::remove_replacement_backups(&pending).map_err(|error| {
        format!("restore completed, but a stale paid-review restore-barrier backup could not be removed: {error}")
    })?;
    std::fs::remove_file(&pending).map_err(|error| {
        format!(
            "restore completed, but paid review remains fail-closed because {} could not be removed: {error}",
            pending.display()
        )
    })?;
    Ok(())
}

fn strict_live_settings_for_restore(path: &Path) -> Result<AppSettings, String> {
    crate::atomic_file::recover_interrupted_replace(path)
        .map_err(|error| format!("could not recover live settings before restore: {error}"))?;
    let mut settings = match std::fs::read(path) {
        Ok(bytes) => crate::settings::AppSettings::parse_recovery_bytes(&bytes)
            .map_err(|error| format!("live settings are invalid; restore remains blocked: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppSettings::default(),
        Err(error) => return Err(format!("live settings are unreadable; restore remains blocked: {error}")),
    };
    settings.enforce_production_canon();
    Ok(settings)
}

fn install_snapshot_restore_plan(
    restore_plan: &SnapshotRestorePlan,
    data_dir: &Path,
    live_controls: &AppSettings,
) -> Result<AppSettings, String> {
    // Dataset-routing state must agree with the restored DB before paid review can resume. Install
    // every required file atomically; settings is typed and handled separately so historical cloud
    // consent and machine routing never touch live disk.
    restore_required_snapshot_state_atomic(&restore_plan.optional, data_dir)?;
    apply_snapshot_pilot_policy(&restore_plan.pilot, data_dir)?;

    let settings_action = restore_plan
        .optional
        .iter()
        .find(|(state, _)| state.live_file == "settings.json")
        .map(|(_, action)| action)
        .ok_or_else(|| "snapshot restore plan omitted settings state".to_string())?;
    let mut restored = match settings_action {
        crate::snapshot::OptionalSnapshotRestore::Install(bytes) => {
            crate::settings::AppSettings::parse_recovery_bytes(bytes)?
        }
        crate::snapshot::OptionalSnapshotRestore::ExplicitlyAbsent => crate::settings::AppSettings::default(),
        crate::snapshot::OptionalSnapshotRestore::PreserveLegacy => live_controls.clone(),
    };
    // Consent and ASR/GPU controls are live operator decisions, never historical dataset state.
    restored.cloud_llm_opt_in = live_controls.cloud_llm_opt_in;
    restored.jury_cloud_opt_in = live_controls.jury_cloud_opt_in;
    preserve_live_asr_runtime_controls(&mut restored, live_controls);
    restored.enforce_production_canon();
    let live_settings_path = data_dir.join("settings.json");
    restored.save(&live_settings_path).map_err(|error| {
        format!(
            "snapshot database was restored, but live-control-preserving settings could not be installed; paid review remains blocked: {error}"
        )
    })?;
    Ok(restored)
}

fn restore_snapshot_generation_offline(
    data_dir: &Path,
    selector: &str,
    live_controls: &AppSettings,
) -> Result<(), String> {
    let snapshot_dir = crate::snapshot::resolve_snapshot_dir(data_dir, selector)?;
    let source = snapshot_dir.join("cortex-speech.db");
    let source_metadata = std::fs::symlink_metadata(&source)
        .map_err(|error| format!("snapshot '{selector}' has no readable database file: {error}"))?;
    if !source_metadata.file_type().is_file() || source_metadata.file_type().is_symlink() {
        return Err(format!("snapshot '{selector}' has no regular database file"));
    }
    let (restore_plan, staged) = prepare_named_restore_artifacts(&snapshot_dir, &source, || {})?;
    let db_path = data_dir.join("cortex-speech.db");
    let mut live = crate::db::Database::open_with_retry(db_path.to_string_lossy().as_ref())
        .map_err(|error| format!("could not open live database for recovery: {error}"))?;
    live.commit_staged_restore(&staged)
        .map_err(|error| format!("could not publish recovered database generation: {error}"))?;
    let integrity =
        live.integrity_check().map_err(|error| format!("recovered live database could not be verified: {error}"))?;
    if integrity.trim() != "ok" {
        return Err(format!("recovered live database failed integrity_check: {integrity}"));
    }
    drop(live);
    install_snapshot_restore_plan(&restore_plan, data_dir, live_controls)?;
    Ok(())
}

/// Complete an interrupted cross-file restore before normal startup performs ANY DB/config write,
/// snapshot, Couch resume, or background work. The intended target is retried first. If that target
/// cannot be made coherent, the manifest-verified original pre-restore generation is restored in
/// full. Both paths keep the durable marker until DB + all required config have committed.
fn recover_interrupted_named_restore_with_admission(
    data_dir: &Path,
    admission: &RestoreAdmission,
) -> Result<bool, String> {
    let Some(pending) = load_named_restore_pending(data_dir)? else {
        return Ok(false);
    };
    let reservation = admission.claim_recovery()?;
    if let Some(completed_selector) = pending.completed_selector.as_deref() {
        // The completion marker is written only after every DB/config/settings leg is durable. Verify
        // the live DB and typed settings without mutating them, then finish the interrupted marker
        // cleanup. Never roll back a generation already recorded as coherent merely because its
        // original source was later moved or pruned.
        let db_path = data_dir.join("cortex-speech.db");
        // Stage into writable memory: FTS5's integrity path may use temporary writes and therefore
        // reports a false "attempt to write a readonly database" against an otherwise healthy file.
        // The shared staging primitive performs immutable-source copy, full integrity, FK and exact
        // migration-history validation without touching the live artifact.
        crate::db::Database::stage_restore_source(&db_path)
            .map_err(|error| format!("completed restore live database could not be verified: {error}"))?;
        strict_live_settings_for_restore(&data_dir.join("settings.json"))?;
        clear_review_pilot_restore_pending(data_dir)?;
        reservation.commit_named_restore()?;
        tracing::warn!(
            "finished durable marker cleanup for already-completed restore '{completed_selector}' before startup"
        );
        return Ok(true);
    }
    let original_pin = crate::snapshot::resolve_snapshot_dir(data_dir, &pending.pre_restore_pin_selector)
        .map_err(|error| format!("interrupted restore has no usable original safety pin: {error}"))?;
    if !crate::snapshot::verify_snapshot_manifest_for_restore(&original_pin)? {
        return Err(
            "interrupted restore's original safety pin is legacy/unverifiable; refusing normal startup".to_string()
        );
    }
    let original_plan = inspect_snapshot_restore_plan(&original_pin, &original_pin.join("cortex-speech.db"), true)?;
    if matches!(original_plan.pilot, SnapshotPilotPolicyRestore::PreserveLegacy)
        || original_plan
            .optional
            .iter()
            .any(|(_, action)| matches!(action, crate::snapshot::OptionalSnapshotRestore::PreserveLegacy))
    {
        return Err("interrupted restore's original safety pin does not explicitly bind every required config state"
            .to_string());
    }

    let settings_path = data_dir.join("settings.json");
    let live_controls = strict_live_settings_for_restore(&settings_path)?;
    let target_result = restore_snapshot_generation_offline(data_dir, &pending.source_selector, &live_controls);
    let completed_selector = match target_result {
        Ok(()) => pending.source_selector.clone(),
        Err(target_error) => {
            tracing::error!(
                "interrupted target restore '{}' could not complete ({target_error}); rolling back verified original '{}'",
                pending.source_selector,
                pending.pre_restore_pin_selector
            );
            restore_snapshot_generation_offline(data_dir, &pending.pre_restore_pin_selector, &live_controls).map_err(
                |rollback_error| {
                    format!(
                        "interrupted restore could not complete target '{}' ({target_error}) and could not roll back verified original '{}' ({rollback_error}); normal startup is blocked",
                        pending.source_selector, pending.pre_restore_pin_selector
                    )
                },
            )?;
            pending.pre_restore_pin_selector.clone()
        }
    };
    // Marker deletion is outside the fallback branch: failure here means the selected generation is
    // already coherent, so rolling it back would be an unnecessary second data transition. Stay fatal
    // and retry marker cleanup idempotently on the next launch.
    mark_named_restore_completed(data_dir, &completed_selector)?;
    clear_review_pilot_restore_pending(data_dir)?;
    reservation.commit_named_restore()?;
    tracing::warn!("completed interrupted restore recovery using '{completed_selector}' before normal startup");
    Ok(true)
}

pub(crate) fn recover_interrupted_named_restore_at_startup(data_dir: &Path) -> Result<bool, String> {
    recover_interrupted_named_restore_with_admission(data_dir, &RESTORE_ADMISSION)
}

/// Restore a verified rotating or pinned recovery artifact. The selector comes only from
/// `list_db_snapshots`; `resolve_snapshot_dir` rejects arbitrary paths/traversal. Both Rust schema-1
/// and headless schema-2 manifests use this same transaction path.
#[tauri::command]
pub async fn restore_db_from_snapshot(name: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("restore_db_from_snapshot")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let snap_dir = crate::snapshot::resolve_snapshot_dir(&data_dir, &name)?;
    let src = snap_dir.join("cortex-speech.db");
    let source_metadata = std::fs::symlink_metadata(&src)
        .map_err(|error| format!("snapshot '{name}' has no readable database file: {error}"))?;
    if !source_metadata.file_type().is_file() || source_metadata.file_type().is_symlink() {
        return Err(format!("snapshot '{name}' has no database file"));
    }
    // Hold the reservation across the whole restore: RESTORE_PENDING stays set until this guard drops
    // and every snapshot capture already in progress drains before preflight. This prevents rotating
    // prune/promotion from racing the selected source while it is being verified.
    let (restore_reservation, restore_data_dir) = prepare_restore(&state)?;
    if let Some(pending) = load_named_restore_pending(&data_dir)? {
        if let Some(completed_selector) = pending.completed_selector.as_deref() {
            if completed_selector != name {
                return Err(format!(
                    "restore '{}' already completed and only its barrier cleanup remains; refusing selector '{}'",
                    completed_selector, name
                ));
            }
            clear_review_pilot_restore_pending(&data_dir)?;
            restore_reservation.commit_named_restore()?;
            tracing::info!("completed pending restore-barrier cleanup for auto-snapshot {name}");
            return Ok(());
        }
    }
    // Source staging, one reusable safety pin, the durable transaction marker, and the page swap run
    // under ONE uninterrupted DB guard. Bad source staging happens before the pin/marker; retries of a
    // post-swap config failure reuse the original pin instead of evicting it.
    let (restore_plan, restore_reservation) = {
        let db = state.db_arc_for_restore();
        let restore_src = src.clone();
        let restore_snapshot_dir = snap_dir.clone();
        let restore_selector = name.clone();
        run_blocking(move || {
            let mut guard = db.lock().unwrap_or_else(|p| p.into_inner());
            let restore_plan = prepare_and_restore_named_transaction(
                &restore_reservation,
                &mut guard,
                &restore_data_dir,
                &restore_snapshot_dir,
                &restore_src,
                &restore_selector,
            )?;
            Ok((restore_plan, restore_reservation))
        })
        .await
    }?; // release the db lock before touching the settings/pipeline locks
        // The staged source was fully proven before its atomic page publication; only a successful commit
        // crosses dataset identity, so only then discard undo/redo commands tied to the old rows.
    state.lock_history().clear();

    let live_controls = state.lock_settings().clone();
    let restored = install_snapshot_restore_plan(&restore_plan, &data_dir, &live_controls)?;
    *state.lock_settings() = restored.clone();
    state.update_pipeline_settings(restored);
    // Commit marker LAST: only now do DB, policy, history, settings, and the running pipeline agree.
    mark_named_restore_completed(&data_dir, &name)?;
    clear_review_pilot_restore_pending(&data_dir)?;
    restore_reservation.commit_named_restore()?;
    drop(restore_reservation);
    // (undo/redo history was already cleared above, right after the DB swap.)
    tracing::info!("database and config restored from auto-snapshot {name}");
    Ok(())
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
pub(crate) static WSL_REFINE_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
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
    // P1.3b: don't start the 7B refinement loop (a background DB writer) while a restore is reserved.
    if restore_pending() {
        return Err(RESTORE_IN_PROGRESS_MSG.into());
    }

    // Single-run guard: claim the running flag atomically. If it was already true, a batch is in
    // flight — refuse rather than starting a second concurrent loop over the same segments.
    if WSL_REFINE_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("WSL 7B refinement batch transcription is already running.".into());
    }
    // P1.3b (publish-then-recheck): the running flag is now PUBLISHED; re-read the reservation. This
    // closes the atomic check-then-set race with prepare_restore (which sets RESTORE_PENDING then reads
    // this flag via writers_active): either it already observed our flag (the fence refuses the restore),
    // or we observe its reservation here and roll back — the two orderings can no longer both slip.
    if restore_pending() {
        WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        return Err(RESTORE_IN_PROGRESS_MSG.into());
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
    // P1.3: the backlog, not the library. This used to read every segment ever imported and then throw
    // away every one that already had a transcript. The SQL prefilter is a deliberate SUPERSET of
    // `segment_awaits_wsl7b`, which stays the authority below — see PendingWork::Transcript.
    let candidates = db.get_pending_segments(crate::db::PendingWork::Transcript).map_err(|e| e.to_string())?;
    let targets = select_wsl_refinement_targets(&candidates, limit_files, limit_segments, test_one);

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
            Ok(result) if result.raw_transcript.trim().is_empty() => {
                // A blank 7B result (silent/music/noise clip — parse_wsl_segment_result returns Ok(""))
                // must NOT overwrite an existing good transcript: update_asr_transcript_if_unreviewed
                // writes raw_transcript unconditionally (guarding only human-reviewed rows). Skip; keep the
                // current text. Neither transcribed nor failed, like the human-reviewed skip below.
                // (blank-transcript-never-overwrites-good; sibling of transcribe_segment / batch_transcribe.)
                emit_or_log(
                    app,
                    "wsl-log",
                    format!(
                        "[{}/{}] {id} produced an empty transcript (silent clip) — existing transcript kept",
                        idx + 1,
                        total
                    ),
                );
            }
            Ok(result) => {
                let raw_transcript = result.raw_transcript;
                let confidence = result.confidence;
                let normalized = if auto_normalize && !raw_transcript.is_empty() {
                    Some(normalizer.normalize(&raw_transcript))
                } else {
                    None
                };
                let champion = crate::db::SegmentHypothesis {
                    segment_id: id.to_string(),
                    model_id: result.model_version_id,
                    transcript: raw_transcript.clone(),
                    confidence,
                };
                match db.commit_champion_transcript_if_unreviewed(
                    &champion,
                    Some(&result.deployment_sha256),
                    normalized.as_deref(),
                    Some("external_provider"),
                    false,
                ) {
                    Ok(true) => {
                        transcribed += 1;
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

#[tauri::command]
pub async fn compute_acoustic_scores(state: State<'_, AppState>) -> Result<usize, String> {
    RATE_LIMITER.check("compute_acoustic_scores")?;
    let mutation = begin_mutation()?;
    let settings_gpu = {
        let s = state.lock_settings();
        s.enable_gpu
    };
    let models_dir = state.lock_model_manager().models_dir.clone();
    let db = state.db_arc();
    run_blocking(move || {
        let _mutation = mutation;
        // P1.3: `WHERE ctc_score IS NULL` instead of reading the whole library and `continue`-ing past
        // every row that already has one. After the first pass this returns nothing at all.
        let segments = {
            let db = db.lock().unwrap_or_else(|p| p.into_inner());
            db.get_pending_segments(crate::db::PendingWork::CtcScore).map_err(|e| e.to_string())?
        };

        let aligner = aligner::ForcedAligner::new(&models_dir, settings_gpu).map_err(|e| e.to_string())?;

        if !aligner.is_available() {
            return Err("MMS Forced Aligner model (mms_aligner.onnx) is not available.".to_string());
        }

        let mut count = 0;
        for seg in &segments {
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

            let guard = db.lock().unwrap_or_else(|p| p.into_inner());
            guard.update_ctc_score(&seg.id, score).map_err(|e| e.to_string())?;
            count += 1;
        }

        Ok(count)
    })
    .await
}

#[tauri::command]
pub async fn compute_signal_anomaly_scores(state: State<'_, AppState>) -> Result<usize, String> {
    RATE_LIMITER.check("compute_signal_anomaly_scores")?;
    let mutation = begin_mutation()?;
    let models_dir = state.lock_model_manager().models_dir.clone();
    let db = state.db_arc();
    run_blocking(move || {
        let _mutation = mutation;
        // P1.3: `WHERE signal_anomaly_score IS NULL` — see the CTC sibling above.
        let segments = {
            let db = db.lock().unwrap_or_else(|p| p.into_inner());
            db.get_pending_segments(crate::db::PendingWork::SignalAnomaly).map_err(|e| e.to_string())?
        };

        let detector = quality::signal_anomaly::SignalAnomalyDetector::new(&models_dir).map_err(|e| e.to_string())?;

        let mut count = 0;
        for seg in &segments {
            let audio_path = seg.audio_path.clone();
            if !std::path::Path::new(&audio_path).exists() {
                continue;
            }

            let (sample_rate, pcm) = match audio::decode_to_pcm_with_timeout(&audio_path, Duration::from_secs(30)) {
                Ok(decoded) => decoded,
                Err(error) => {
                    tracing::warn!("Skipping signal-anomaly score for {}: decode failed: {error}", seg.id);
                    continue;
                }
            };
            let (_sr, pcm_16k) = match audio::ensure_pcm_16khz(sample_rate, pcm) {
                Ok(resampled) => resampled,
                Err(error) => {
                    tracing::warn!("Skipping signal-anomaly score for {}: 16 kHz conversion failed: {error}", seg.id);
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
                    tracing::warn!("Skipping signal-anomaly score for {}: clip slice failed: {error}", seg.id);
                    continue;
                }
            };
            let score = match detector.compute_signal_anomaly_score(&pcm_16k) {
                Ok(score) => score,
                Err(error) => {
                    tracing::warn!("Skipping signal-anomaly score for {}: scoring failed: {error}", seg.id);
                    continue;
                }
            };

            let guard = db.lock().unwrap_or_else(|p| p.into_inner());
            guard.update_signal_anomaly_score(&seg.id, score).map_err(|e| e.to_string())?;
            count += 1;
        }

        Ok(count)
    })
    .await
}

// ════════════════════════════════════════════════════════════════════════════
// Phase 1 — Gold-Set Eval Harness
// ════════════════════════════════════════════════════════════════════════════

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

#[tauri::command]
pub fn list_eval_runs(state: State<'_, AppState>) -> Result<Vec<crate::eval::EvalRun>, String> {
    RATE_LIMITER.check("list_eval_runs")?;
    let db = state.lock_db();
    crate::eval::list_eval_runs(&db).map_err(|e| e.to_string())
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
pub fn couch_review_status() -> Result<crate::couch::CouchStatus, String> {
    RATE_LIMITER.check("couch_review_status")?;
    Ok(crate::couch::status())
}

/// Per-reviewer spot-check scores — how each remote reviewer did on clips whose answer was already
/// known (Migration v44, docs/REMOTE_REVIEW_PLAN.md §2.1).
///
/// Worst `noticed` rate first, because the finding this exists to surface is a reviewer who is not
/// listening, and burying them under the diligent ones would defeat the point.
#[tauri::command]
pub fn spot_check_report(state: State<'_, AppState>) -> Result<Vec<crate::db::SpotCheckScore>, String> {
    RATE_LIMITER.check("spot_check_report")?;
    state.lock_db().spot_check_report().map_err(|e| e.to_string())
}

/// Per-reviewer throughput from the append-only review trail (Migration v45).
///
/// Distinct from `stats.rs`'s median seconds-per-decision, deliberately: that one orders
/// `decision_log` GLOBALLY, which is correct for a single reviewer and meaningless for a team — with
/// several people working at once it would time the gap between two DIFFERENT humans' decisions. This
/// one partitions per reviewer, so the existing metric keeps its meaning and this one is honest.
#[tauri::command]
pub fn reviewer_throughput(state: State<'_, AppState>) -> Result<Vec<crate::db::ReviewerThroughput>, String> {
    RATE_LIMITER.check("reviewer_throughput")?;
    state.lock_db().reviewer_throughput().map_err(|e| e.to_string())
}

/// Write the two-rater agreement sample beside the library and return where it went
/// (docs/REMOTE_REVIEW_PLAN.md §2.4).
///
/// Deliberately exports the TSV rather than computing kappa here. `scripts/agreement_kappa.py`
/// already implements the statistic and is unit-tested against the textbook kappa=0.40 example; a
/// second implementation in Rust would be an unverified copy of a verified thing, and under the
/// honesty law a metric must come from a real run of the real harness. This command produces the
/// evidence; the harness produces the number.
#[tauri::command]
pub fn export_agreement_sample(state: State<'_, AppState>) -> Result<Option<crate::db::AgreementExport>, String> {
    RATE_LIMITER.check("export_agreement_sample")?;
    let (sample, db_path) = {
        let db = state.lock_db();
        (db.agreement_sample().map_err(|e| e.to_string())?, db.path().to_string())
    };
    let Some(mut sample) = sample else {
        return Ok(None); // nothing double-reviewed yet — not an error, just no evidence
    };
    let out = std::path::Path::new(&db_path)
        .parent()
        .ok_or_else(|| "library path has no parent directory".to_string())?
        .join("agreement_sample.tsv");
    std::fs::write(&out, &sample.tsv).map_err(|e| format!("could not write {}: {e}", out.display()))?;
    sample.path = out.to_string_lossy().to_string();
    Ok(Some(sample))
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

/// Count of in-flight BACKGROUND DB writers that use their OWN dedicated connection (NOT the global db
/// Mutex) and may run OUTSIDE the import/batch guards — so they escape the db-Mutex serialization the
/// restore relies on, and `AppState::writers_active` must consult this to fence a restore while any is
/// mid-write (R3). Registrants: the jury writers (run_jury_pipeline / run_t2_for_segment /
/// run_dpo_update + the post-import adjudication thread) and the detached background-alignment thread.
/// A COUNTER, not a bool: these writers may legitimately
/// overlap, and a bool would clear the fence when the FIRST of two concurrent writers finished. This is
/// the one place a NEW dedicated-connection writer registers — extend by taking a guard, not by growing
/// the writers_active() || chain (the recurring "forgot the new writer" bug this closes twice over).
pub(crate) static BG_DB_WRITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// RAII registration for a background DB writer: increments [`BG_DB_WRITERS`] on construction and
/// decrements on drop, so the restore fence is armed for exactly the writer's lifetime — even if the
/// work panics (Drop still runs on unwind). ZST, so it is `Send` and can be moved into a worker thread.
pub(crate) struct BgDbWriterGuard;
impl BgDbWriterGuard {
    pub(crate) fn new() -> Self {
        BG_DB_WRITERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}
impl Drop for BgDbWriterGuard {
    fn drop(&mut self) {
        BG_DB_WRITERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// True while any background DB writer is in flight — consulted by the restore fence.
pub(crate) fn bg_db_writers_active() -> bool {
    BG_DB_WRITERS.load(std::sync::atomic::Ordering::SeqCst) > 0
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
        positional_window: false,
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
            positional_window: false,
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
        positional_window: false,
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
        // SORTED, because this names a SET of models that agreed and a set has no order. It is
        // persisted on the row and shipped in exports as `referenceModelId`, so an unstable
        // rendering would make two identical decisions store two different strings: a corpus diff
        // shows changes nobody made, and any grouping or count by this value splits one category
        // into two. Caught by the suite failing on `flash+pro` where a previous run gave
        // `pro+flash` — same code, different SQLite tie-break, so it had never been the ordering
        // anyone chose.
        //
        // The ROOT-CAUSE fix is the `model_id ASC` tiebreaker in `get_source_transcripts_for_audio`,
        // and that is measured rather than assumed: with neither change the reversed-order test
        // fails; with the tiebreaker alone both pass; the sort alone was not isolated because the
        // tiebreaker already covers it. This line stays anyway — the string names a set, so its
        // canonical form must not depend on why some query happened to order its rows.
        let mut model_ids =
            reports.iter().filter_map(|report| report.reference_model_id.as_deref()).collect::<Vec<_>>();
        model_ids.sort_unstable();
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
    // Require a positional window even for the agreement boost (round-24 hunt #15): a whole-file
    // fallback overlap makes the 0.55 bar much easier for common-word candidates and would auto-commit
    // on evidence that never positionally located the candidate in the source.
    if agreeing_on_best >= 2 && best.selected_score >= 0.55 && !best.scores.is_empty() && best.positional_window {
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
    settings: &crate::settings::AppSettings,
    seg_id: &str,
    seg: &crate::db::SpeechSegment,
) -> Result<Vec<crate::db::SegmentHypothesis>, String> {
    let persisted = db.get_hypotheses_for_segment(seg_id).map_err(|e| e.to_string())?;
    let recorded_is_champion = segment_recorded_model_is_champion(db, seg);
    let mut hyps = hypotheses_for_selected_asr(&settings.asr_model_size, seg, persisted, recorded_is_champion);
    if hyps.is_empty() && settings.asr_model_size != crate::settings::AsrModelSize::WSL7B {
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
    jury_db_source(app_state).with(f)
}

/// Owned, `Send + 'static` form of the jury-DB access above, so an async command can move it into
/// `run_blocking` (a `&AppState` borrow can't cross the await). Carries the db PATH (the dedicated
/// connection is opened lazily inside `with`, on whichever thread runs it) plus the shared handle for
/// the fallback. Same semantics as `with_jury_db` — it IS `with_jury_db`'s implementation.
struct JuryDbSource {
    db_path: String,
    shared: crate::AppDatabaseHandle,
}

fn jury_db_source(app_state: &AppState) -> JuryDbSource {
    JuryDbSource { db_path: app_state.lock_db().path().to_string(), shared: app_state.db_arc() }
}

impl JuryDbSource {
    fn with<R>(&self, f: impl FnOnce(&crate::db::Database) -> R) -> R {
        if self.db_path != ":memory:" {
            // Dedicated worker connection so we never hold the GLOBAL db Mutex across the jury's cloud T2
            // Gemini round-trips (which would freeze every other DB command for minutes). Use plain `open`,
            // NOT open_with_retry: the latter runs a full PRAGMA integrity_check (a per-review-session tax
            // that grows with the library) AND reaches the boot-time-only DESTRUCTIVE quarantine decision
            // from a live worker thread. The file was already integrity-checked at boot; `open` sets WAL +
            // busy_timeout=10000, which is the actual transient-contention retry we want here.
            match crate::db::Database::open(&self.db_path) {
                Ok(db) => return f(&db),
                // One open attempt (SQLite's busy_timeout=10s inside open() is the only retry) —
                // the write-path audit corrected an older comment that claimed app-level retries.
                Err(e) => tracing::warn!(
                    "Jury dedicated db connection open failed ({e}); using the shared handle \
                     (other DB commands may pause during adjudication)"
                ),
            }
        }
        let db = self.shared.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned database lock");
            poisoned.into_inner()
        });
        f(&db)
    }
}

/// OpenRouter's OpenAI-compatible chat endpoint. POLICY (owner, 2026-07-14): the only approved cloud
/// ASR judge for Central Kurdish is **google/gemini-2.5-pro** — Qwen-family ASR has no Sorani support
/// (measured; PROGRESS_LEDGER 2026 sweep) and must not be configured as the judge.
const OPENROUTER_CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Map a jury model name to an OpenRouter slug. A bare Gemini id (the default `gemini-2.5-pro`) maps to
/// `google/gemini-2.5-pro`; an already-slugged id passes through (mechanism only — the approved ckb
/// judge is strictly Gemini 2.5 Pro; any different model first needs a measured ckb CER on frozen gold).
fn openrouter_jury_model_id(jury_model: &str) -> String {
    let m = jury_model.trim();
    if m.is_empty() {
        "google/gemini-2.5-pro".to_string()
    } else if m.contains('/') || !m.starts_with("gemini") {
        m.to_string()
    } else {
        format!("google/{m}")
    }
}

/// Pure branch logic for the T2 audio-judge transport (no filesystem): OpenRouter when the provider is
/// opted into AND an OpenRouter key is present, otherwise direct Gemini. Never routes to keyless cloud.
fn resolve_t2_endpoint_from_keys(
    settings: &crate::settings::AppSettings,
    gemini_key: &str,
    openrouter_key: Option<&str>,
) -> (crate::jury::t2_listener::T2Endpoint, String, String) {
    use crate::jury::t2_listener::T2Endpoint;
    if settings.jury_provider.eq_ignore_ascii_case("openrouter") {
        if let Some(key) = openrouter_key.map(str::trim).filter(|k| !k.is_empty()) {
            return (
                T2Endpoint::OpenAiCompatible { url: OPENROUTER_CHAT_URL.to_string() },
                key.to_string(),
                openrouter_jury_model_id(&settings.jury_model),
            );
        }
    }
    (T2Endpoint::GeminiDirect, gemini_key.to_string(), settings.jury_model.clone())
}

/// Resolve the T2 transport/key/model, loading the OpenRouter key from `secrets.env` in `data_dir` when
/// the jury provider is "openrouter". Falls back to direct Gemini when no OpenRouter key is present.
fn resolve_t2_endpoint(
    settings: &crate::settings::AppSettings,
    gemini_key: &str,
    data_dir: Option<&std::path::Path>,
) -> (crate::jury::t2_listener::T2Endpoint, String, String) {
    let openrouter_key = if settings.jury_provider.eq_ignore_ascii_case("openrouter") {
        data_dir.and_then(|d| crate::api_keys::ApiKeys::load(d).openrouter)
    } else {
        None
    };
    resolve_t2_endpoint_from_keys(settings, gemini_key, openrouter_key.as_deref())
}

/// Direct-Gemini entry point (the default transport, used by tests and any caller without a data dir).
/// Callers with a data dir use `run_jury_pipeline_core_via` to honor the OpenRouter jury setting.
pub fn run_jury_pipeline_core(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    segment_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    run_jury_pipeline_core_via(db, settings, segment_ids, None)
}

pub fn run_jury_pipeline_core_via(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    segment_ids: Vec<String>,
    data_dir: Option<&std::path::Path>,
) -> Result<serde_json::Value, String> {
    // Champion-only production mode has exactly one ASR by design. Running a multi-ASR jury here
    // either treats stale auxiliary rows as votes or immediately fails its two-model coverage guard.
    // Both outcomes violate the owner's sole-champion contract. Leave every fresh draft for the human
    // review flow and keep the stricter multi-model proof confined to explicit non-champion/export use.
    if settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
        return Ok(serde_json::json!({
            "mode": "not_required",
            "totalInput": segment_ids.len(),
            "t0AutoAccepted": 0,
            "t0Escalated": 0,
            "referenceCommitted": 0,
            "referenceGuarded": 0,
            "hypothesisGuarded": 0,
            "t1Committed": 0,
            "t2Committed": 0,
            "humanInbox": segment_ids.len(),
            "reason": "Champion-only mode sends OmniASR 7B drafts directly to human review; auxiliary-ASR jury is not run"
        }));
    }

    let t1_threshold = settings.jury_t1_threshold;
    let cloud_opt_in = settings.jury_cloud_opt_in;
    // Floor at 3: self-consistency is meaningless below 3 samples, and a misconfigured 1 would let a
    // single Gemini sample masquerade as a "majority". majority_vote also requires >= 2 agreeing
    // samples, so this is defense in depth at the config boundary.
    let n_samples = (settings.jury_self_consistency_n as usize).max(3);
    // T2 transport: direct Gemini by default, or OpenRouter (with the OR key from secrets.env) when the
    // jury provider is set to "openrouter".
    let (t2_endpoint, api_key, jury_model) = resolve_t2_endpoint(settings, &settings.llm_api_key, data_dir);

    // The Autonomy Dial governs EVERY machine-commit stage of this pipeline, not just T0
    // (round-24 hunt #1, HIGH). Observe/Propose previously gated only run_t0_gate; the SAME run then
    // re-consumed the staged ('escalated') segments as T1/T2 input and machine-committed them as
    // 'jury_accept' — silently removing from the human queue the very segments the dial had just
    // promised to stage (and, under Observe, the T2-disabled fallback still REWROTE verdicts the dial
    // said it would never touch). Enforced once here, at the single pipeline chokepoint:
    //   - Observe: the pipeline commits NOTHING beyond T0's own no-write contract — no reference
    //     commits, no T1/T2, no re-staging writes.
    //   - Propose: the machine stages ('escalated') but never commits; an already-staged segment
    //     keeps its T0 verdict + IRT confidence (riskiest-first ordering) untouched.
    let machine_commits_allowed = matches!(
        settings.jury_autonomy_level,
        crate::settings::AutonLevel::ActConfirm | crate::settings::AutonLevel::ActAuto
    );

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

        let hyps = load_hypotheses_for_segment(db, settings, seg_id, seg)?;
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
            Some(report) if report.should_commit && machine_commits_allowed => {
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

        // Observe/Propose: the machine may not commit (or, under Observe, write anything at all) —
        // stage-or-skip instead of running T1/T2. See machine_commits_allowed above.
        if !machine_commits_allowed {
            if matches!(settings.jury_autonomy_level, crate::settings::AutonLevel::Observe) {
                continue; // Observe: pure observation — this run writes nothing.
            }
            // Propose: an already-staged segment keeps its verdict + IRT confidence (rewriting it
            // would NULL the confidence that drives the queue's riskiest-first ordering).
            if seg.escalated {
                still_escalated += 1;
                continue;
            }
            // Carry the guard's rationale into the staged verdict when one exists — under the
            // shipped default (Propose) this is the reviewer's context in the inbox.
            let staged_rationale = match reference_reports.remove(seg_id) {
                Some(report) => format!("{} Staged for human review (autonomy dial: Propose).", report.rationale),
                None => "Staged for human review: autonomy dial 'Propose' disables machine commits".to_string(),
            };
            // P1.2: `policy_hold` — this clip was NOT escalated on its merits. The autonomy dial forbade
            // a machine commit, so a decidable clip was staged for a human. That needs a settings
            // change, not a reviewer listening harder, and prose alone cannot be counted or filtered.
            db.write_segment_verdict(
                seg_id,
                "escalated",
                None,
                Some(&staged_rationale),
                Some(&crate::jury::escalation_evidence(&[crate::jury::reason::POLICY_HOLD])),
                None,
                true,
            )
            .map_err(|e| e.to_string())?;
            still_escalated += 1;
            continue;
        }

        // Load hypotheses from database
        let hyps = load_hypotheses_for_segment(db, settings, seg_id, &seg)?;

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
                            // P1.2: `missing_audio` — no judgement was possible because the audio could
                            // not be read. The prose keeps the specific IO error; the code makes the
                            // class countable, and this class is fixed by finding a file.
                            db.write_segment_verdict(
                                seg_id,
                                "escalated",
                                None,
                                Some(&e.to_string()),
                                Some(&crate::jury::escalation_evidence(&[crate::jury::reason::MISSING_AUDIO])),
                                None,
                                true,
                            )
                            .map_err(|e| e.to_string())?;
                            still_escalated += 1;
                            continue;
                        }
                    };

                    let few_shots = crate::jury::get_few_shot_examples(db, seg_id, 5).map_err(|e| e.to_string())?;
                    let t2 = crate::jury::t2_listener::listen_and_judge_via(
                        &t2_endpoint,
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
                        // P1.2: T2 answered, and its answer was "no consensus" — deliberately a
                        // different code from T1_UNRESOLVED below, where T2 never got to answer at all.
                        db.write_segment_verdict(
                            seg_id,
                            "escalated",
                            None,
                            Some(&reason),
                            Some(&crate::jury::escalation_evidence(&[crate::jury::reason::T2_NO_MAJORITY])),
                            None,
                            true,
                        )
                        .map_err(|e| e.to_string())?;
                        still_escalated += 1;
                    }
                } else {
                    // Cloud disabled — escalate to human inbox
                    let reason = reference_report.as_ref().map_or_else(
                        || "T1 could not resolve; T2 disabled (cloud opt-in off)".to_string(),
                        |report| format!("{} T1 could not resolve; T2 disabled (cloud opt-in off)", report.rationale),
                    );
                    // P1.2: T1 could not resolve AND T2 was never consulted, because cloud opt-in is
                    // off. Recorded as T1_UNRESOLVED rather than T2_NO_MAJORITY: the panel declining to
                    // answer is a different fact from the panel answering "no consensus", and only one
                    // of them is fixed by turning a setting on.
                    db.write_segment_verdict(
                        seg_id,
                        "escalated",
                        None,
                        Some(&reason),
                        Some(&crate::jury::escalation_evidence(&[crate::jury::reason::T1_UNRESOLVED])),
                        None,
                        true,
                    )
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
            signal_anomaly_score: None,
            ..crate::db::SpeechSegment::default()
        }
    }

    #[test]
    fn mandatory_pre_restore_snapshot_failure_aborts_before_live_data_can_change() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().expect("local restore reservation");
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let segment = test_segment("restore-safety", "restore-safety.wav", "still live");
        db.insert_segment(&segment).unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let invalid_data_dir = temp.path().join("not-a-directory");
        std::fs::write(&invalid_data_dir, b"blocks snapshot directory creation").unwrap();

        let err = take_mandatory_pre_restore_snapshot(&reservation, &db, &invalid_data_dir).unwrap_err();
        assert!(err.contains("mandatory pre-restore safety snapshot failed"), "{err}");
        assert!(err.contains("has not been overwritten"), "{err}");
        assert!(
            !err.contains("requires an active exclusive restore reservation"),
            "the capability must let this test reach the injected filesystem failure: {err}"
        );
        assert_eq!(
            db.get_segment_by_id(&segment.id).unwrap().unwrap().raw_transcript,
            "still live",
            "a failed safety pin must leave the live library untouched"
        );
    }

    #[test]
    fn restore_admission_is_exclusive_and_releases_waiters_after_error_and_panic() {
        let admission = RestoreAdmission::new();

        let first = admission.try_reserve().expect("first restore owns the reservation");
        assert!(admission.try_reserve().is_err(), "overlapping restores must fail instead of sharing a flag");
        drop(first);
        assert!(!admission.is_pending());

        let failed: Result<(), &str> = {
            let _reservation = admission.try_reserve().expect("reservation for failing restore");
            Err("injected restore error")
        };
        assert_eq!(failed, Err("injected restore error"));
        assert!(!admission.is_pending(), "an error path must release the restore reservation");

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _reservation = admission.try_reserve().expect("reservation for panicking restore");
            panic!("injected restore panic");
        }));
        assert!(panicked.is_err());
        assert!(!admission.is_pending(), "an unwind must release the restore reservation");

        let capture = admission.begin_capture().expect("snapshot capture enters while no restore is pending");
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let admission_ref = &admission;
            scope.spawn(move || {
                let _ = started_tx.send(());
                let reservation = admission_ref.try_reserve().expect("restore waits for active capture to drain");
                let _ = acquired_tx.send(());
                let _ = release_rx.recv();
                drop(reservation);
            });
            started_rx.recv_timeout(std::time::Duration::from_secs(2)).expect("restore waiter thread started");
            let pending_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !admission.is_pending() && std::time::Instant::now() < pending_deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(admission.is_pending(), "restore must publish pending before waiting for the capture");
            assert!(admission.begin_capture().is_err(), "no new snapshot may cross a pending restore generation");
            assert!(
                acquired_rx.recv_timeout(std::time::Duration::from_millis(50)).is_err(),
                "restore publication must wait until the complete capture token drops"
            );
            drop(capture);
            acquired_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("restore proceeds after the snapshot capture drains");
            release_tx.send(()).unwrap();
        });
        assert!(!admission.is_pending());

        let database = std::sync::Mutex::new(());
        let reservation = admission.try_reserve().expect("reservation that fences a queued writer");
        std::thread::scope(|scope| {
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let admission_ref = &admission;
            let database_ref = &database;
            scope.spawn(move || {
                let _db = admission_ref.lock(database_ref).unwrap_or_else(|poisoned| poisoned.into_inner());
                entered_tx.send(()).unwrap();
            });
            assert!(
                entered_rx.recv_timeout(std::time::Duration::from_millis(50)).is_err(),
                "an ordinary AppState DB caller must remain fenced through the complete restore"
            );
            // Models the point after DB pages are restored but before history/config/pipeline work ends.
            // The waiter must still be blocked until the outer command drops its reservation.
            assert!(admission.is_pending());
            drop(reservation);
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("the queued caller resumes after the complete restore");
        });
    }

    #[test]
    fn armed_restore_parks_on_error_and_exact_recovery_is_the_only_reentry() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().expect("new restore reservation");
        reservation.arm_named_restore().expect("durable transaction arm");
        drop(reservation); // models any error/unwind after the marker commit boundary

        assert!(admission.is_pending(), "an armed error must keep ordinary DB/config work fenced");
        assert!(admission.begin_mutation().is_err(), "no mutation may start in recovery-required state");
        assert!(admission.try_reserve().is_err(), "an unrelated restore cannot claim a parked transaction");

        let recovery = admission.claim_recovery().expect("the exact recovery path reclaims parked admission");
        recovery.commit_named_restore().expect("coherent generation commit releases the fence");
        let next = admission.try_reserve().expect("normal restore admission resumes after recovery commit");
        drop(recovery); // a stale generation token must not clear the newer reservation
        assert!(admission.is_pending(), "stale guard drop must not release a newer restore generation");
        drop(next);
        assert!(!admission.is_pending());
    }

    #[test]
    fn full_operation_mutation_and_restore_admission_are_race_closed() {
        let admission = RestoreAdmission::new();
        let mutation = admission.begin_mutation().expect("mutation starts while idle");
        assert!(admission.try_reserve().is_err(), "restore must refuse an already-running long mutation");
        assert!(!admission.is_pending(), "a refused new restore must not strand the admission flag");
        drop(mutation);

        let restore = admission.try_reserve().expect("restore starts after mutation ends");
        assert!(admission.begin_mutation().is_err(), "new long mutation must refuse a published restore");
        drop(restore);
        assert!(admission.begin_mutation().is_ok(), "mutations resume after an unarmed restore ends");
    }

    #[test]
    fn restore_helper_pins_the_old_live_db_before_swapping_to_the_source() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().expect("local restore reservation");
        let temp = tempfile::TempDir::new().unwrap();
        let live_path = temp.path().join("live.db");
        let source_path = temp.path().join("source.db");
        let data_dir = temp.path().join("app-data");
        std::fs::create_dir_all(&data_dir).unwrap();

        let live = crate::db::Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("old-live", "old.wav", "must be recoverable")).unwrap();

        let source = crate::db::Database::open(source_path.to_string_lossy().as_ref()).unwrap();
        source.initialize().unwrap();
        source.insert_segment(&test_segment("restored", "restored.wav", "from snapshot")).unwrap();
        // A manifest-bound snapshot is a frozen, self-contained DB file. Immutable restore staging
        // intentionally ignores live WAL sidecars, so finish this fixture like snapshot promotion
        // does instead of asking raw-file verification to read an open writer's uncheckpointed WAL.
        source.wal_checkpoint().unwrap();
        drop(source);

        let shared = std::sync::Mutex::new(live);
        let pinned = {
            // This is the exact production ownership shape: one DB mutex guard is held while the
            // helper first snapshots and then restores. Rust cannot release/reacquire it in between.
            let mut guard = shared.lock().unwrap();
            restore_with_mandatory_snapshot(&reservation, &mut guard, &data_dir, &source_path).unwrap()
        };

        let restored = shared.lock().unwrap();
        assert!(restored.get_segment_by_id("restored").unwrap().is_some());
        assert!(restored.get_segment_by_id("old-live").unwrap().is_none());

        assert!(
            crate::snapshot::verify_snapshot_manifest_for_restore(&pinned).unwrap(),
            "the mandatory pre-restore pin must be self-verifying"
        );
        let pinned_db = pinned.join("cortex-speech.db");
        let read_only = rusqlite::Connection::open_with_flags(
            pinned_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        let old_rows: i64 = read_only
            .query_row("SELECT COUNT(*) FROM speech_segments WHERE id = 'old-live'", [], |row| row.get(0))
            .unwrap();
        let new_rows: i64 = read_only
            .query_row("SELECT COUNT(*) FROM speech_segments WHERE id = 'restored'", [], |row| row.get(0))
            .unwrap();
        assert_eq!((old_rows, new_rows), (1, 0), "the mandatory pin must contain the exact pre-restore library");
        // A WAL-mode read-only connection may keep transient -shm/-wal siblings open. Production
        // staging closes its source before the final manifest re-verification; model that boundary.
        drop(read_only);
    }

    #[test]
    fn named_restore_reuses_original_transaction_pin_and_bad_source_creates_no_barrier() {
        let admission = RestoreAdmission::new();
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path();
        let mut live = crate::db::Database::open(":memory:").unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("before-source", "before.wav", "source generation")).unwrap();
        let source_dir = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 1000).unwrap().unwrap();
        let source = source_dir.join("cortex-speech.db");
        live.insert_segment(&test_segment("pre-restore-only", "later.wav", "must remain in original pin")).unwrap();

        let reservation = admission.try_reserve().unwrap();
        let first = prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &source_dir,
            &source,
            "snapshot_0000001000",
        )
        .unwrap();
        assert_eq!(first.optional.len(), crate::snapshot::OPTIONAL_SNAPSHOT_STATE.len());
        let pending = load_named_restore_pending(data_dir).unwrap().unwrap();
        let original_pin = crate::snapshot::resolve_snapshot_dir(data_dir, &pending.pre_restore_pin_selector).unwrap();
        let pin_db = rusqlite::Connection::open_with_flags(
            original_pin.join("cortex-speech.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        let original_only: i64 = pin_db
            .query_row("SELECT COUNT(*) FROM speech_segments WHERE id='pre-restore-only'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(original_only, 1, "the transaction pin is the exact generation before the first page swap");

        prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &source_dir,
            &source,
            "snapshot_0000001000",
        )
        .unwrap();
        let pins = std::fs::read_dir(data_dir.join("snapshots").join("pinned"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("prerestore_"))
            .count();
        assert_eq!(pins, 1, "a retry must reuse, never rotate out, the original pre-restore generation");
        assert_eq!(load_named_restore_pending(data_dir).unwrap().unwrap(), pending);
        clear_review_pilot_restore_pending(data_dir).unwrap();
        reservation.commit_named_restore().unwrap();
        drop(reservation);

        let broken_dir = data_dir.join("snapshots").join("snapshot_0000002000");
        std::fs::create_dir_all(&broken_dir).unwrap();
        std::fs::write(broken_dir.join("cortex-speech.db"), b"not sqlite").unwrap();
        let reservation = admission.try_reserve().unwrap();
        let error = prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &broken_dir,
            &broken_dir.join("cortex-speech.db"),
            "snapshot_0000002000",
        )
        .unwrap_err();
        assert!(!error.is_empty());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let pins_after = std::fs::read_dir(data_dir.join("snapshots").join("pinned"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("prerestore_"))
            .count();
        assert_eq!(pins_after, 1, "invalid source validation happens before any new pin or barrier");
        drop(reservation);
    }

    fn snapshot_selector(path: &Path, pinned: bool) -> String {
        let name = path.file_name().and_then(|name| name.to_str()).expect("snapshot directory name");
        if pinned {
            format!("pinned/{name}")
        } else {
            name.to_string()
        }
    }

    #[test]
    fn startup_recovery_completes_the_recorded_target_before_any_normal_work() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("original", "original.wav", "original generation")).unwrap();
        let original_pin =
            crate::snapshot::take_pinned_snapshot_at(&live, data_dir, "startup-original", 3, 1000).unwrap();
        live.insert_segment(&test_segment("target", "target.wav", "target generation")).unwrap();
        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 2000).unwrap().unwrap();
        live.delete_segment("target").unwrap(); // prove recovery publishes the target snapshot, not current pages
        drop(live);

        let pending = NamedRestorePending {
            schema: NAMED_RESTORE_PENDING_SCHEMA,
            source_selector: snapshot_selector(&target, false),
            pre_restore_pin_selector: snapshot_selector(&original_pin, true),
            completed_selector: None,
        };
        write_named_restore_pending(data_dir, &pending).unwrap();
        let admission = RestoreAdmission::new();

        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());
        assert!(!admission.is_pending());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        assert!(recovered.get_segment_by_id("original").unwrap().is_some());
        assert!(recovered.get_segment_by_id("target").unwrap().is_some());
    }

    #[test]
    fn startup_recovery_rolls_back_the_verified_full_original_when_target_preflight_fails() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("original", "original.wav", "original generation")).unwrap();
        let original_pin =
            crate::snapshot::take_pinned_snapshot_at(&live, data_dir, "startup-rollback", 3, 3000).unwrap();
        live.insert_segment(&test_segment("target", "target.wav", "must be rolled back")).unwrap();
        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 4000).unwrap().unwrap();
        drop(live);
        std::fs::write(target.join("settings.json"), b"tampered after manifest").unwrap();

        let pending = NamedRestorePending {
            schema: NAMED_RESTORE_PENDING_SCHEMA,
            source_selector: snapshot_selector(&target, false),
            pre_restore_pin_selector: snapshot_selector(&original_pin, true),
            completed_selector: None,
        };
        write_named_restore_pending(data_dir, &pending).unwrap();
        let admission = RestoreAdmission::new();

        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());
        assert!(!admission.is_pending());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        assert!(recovered.get_segment_by_id("original").unwrap().is_some());
        assert!(recovered.get_segment_by_id("target").unwrap().is_none());
    }

    #[test]
    fn completed_restore_marker_cleanup_never_replays_or_rolls_back_missing_sources() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("committed", "committed.wav", "already coherent")).unwrap();
        drop(live);
        let pending = NamedRestorePending {
            schema: NAMED_RESTORE_PENDING_SCHEMA,
            source_selector: "snapshot_0000009999".to_string(),
            pre_restore_pin_selector: "pinned/missing_0000009998".to_string(),
            completed_selector: Some("snapshot_0000009999".to_string()),
        };
        write_named_restore_pending(data_dir, &pending).unwrap();
        let admission = RestoreAdmission::new();

        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());
        assert!(!admission.is_pending());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        assert!(recovered.get_segment_by_id("committed").unwrap().is_some());
    }

    #[test]
    fn named_restore_rehashes_after_plan_and_staging_and_never_mutates_invalid_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&test_segment("source", "source.wav", "source")).unwrap();
        let source_dir = crate::snapshot::take_snapshot_at(&db, temp.path(), 5, 1000).unwrap().unwrap();
        let source_db = source_dir.join("cortex-speech.db");
        let settings = source_dir.join("settings.json");
        let original = std::fs::read(&settings).unwrap();
        let error = prepare_named_restore_artifacts(&source_dir, &source_db, || {
            std::fs::write(&settings, b"tampered after plan capture").unwrap();
        })
        .err()
        .expect("post-capture mutation must be rejected");
        assert!(error.contains("mismatch"), "{error}");

        // Restore the exact source, then make malformed settings honestly match the manifest. This
        // reaches semantic preflight and proves it rejects without AppSettings::load renaming/mutating
        // the frozen source or touching a live DB.
        std::fs::write(&settings, &original).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source_dir.join(crate::snapshot::MANIFEST_FILE)).unwrap()).unwrap();
        let invalid = b"{}";
        std::fs::write(&settings, invalid).unwrap();
        let row =
            manifest["files"].as_array_mut().unwrap().iter_mut().find(|row| row["path"] == "settings.json").unwrap();
        row["sizeBytes"] = serde_json::json!(invalid.len());
        row["sha256"] = serde_json::json!(crate::models::compute_file_sha256(&settings).unwrap());
        std::fs::write(source_dir.join(crate::snapshot::MANIFEST_FILE), serde_json::to_vec_pretty(&manifest).unwrap())
            .unwrap();
        let semantic = prepare_named_restore_artifacts(&source_dir, &source_db, || {})
            .err()
            .expect("correctly hashed malformed config must fail semantic preflight");
        assert!(semantic.contains("settings.json is invalid"), "{semantic}");
        assert_eq!(std::fs::read(&settings).unwrap(), invalid);
        assert!(
            std::fs::read_dir(&source_dir)
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains("corrupt")),
            "strict recovery parsing must not rename or repair the frozen source"
        );
    }

    #[test]
    fn required_snapshot_routing_state_is_atomic_and_failure_keeps_review_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let live = temp.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
        let payloads = [
            ("champion.json", b"snapshot champion".as_slice()),
            ("reviewer_dialects.json", b"snapshot dialects".as_slice()),
            ("voice_focus.json", b"snapshot focus".as_slice()),
        ];
        let plan = crate::snapshot::OPTIONAL_SNAPSHOT_STATE
            .iter()
            .copied()
            .filter(|state| state.live_file != "settings.json")
            .map(|state| {
                let bytes = payloads.iter().find(|(name, _)| *name == state.live_file).unwrap().1.to_vec();
                (state, crate::snapshot::OptionalSnapshotRestore::Install(bytes))
            })
            .collect::<Vec<_>>();
        std::fs::write(live.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"pending").unwrap();

        // A non-file destination is an injected install failure. The helper must fail, and the
        // command-level commit marker remains authoritative because only the successful tail clears it.
        std::fs::create_dir(live.join("champion.json")).unwrap();
        let error = restore_required_snapshot_state_atomic(&plan, &live).unwrap_err();
        assert!(error.contains("champion.json") && error.contains("regular file or absent"), "{error}");
        assert!(live.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE).is_file());

        std::fs::remove_dir(live.join("champion.json")).unwrap();
        restore_required_snapshot_state_atomic(&plan, &live).unwrap();
        assert_eq!(std::fs::read(live.join("champion.json")).unwrap(), b"snapshot champion");
        assert_eq!(std::fs::read(live.join("reviewer_dialects.json")).unwrap(), b"snapshot dialects");
        assert_eq!(std::fs::read(live.join("voice_focus.json")).unwrap(), b"snapshot focus");
        assert!(live.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE).is_file());

        let stale = live.join("voice_focus.json.replace-bak-99999");
        std::fs::write(&stale, b"stale focus").unwrap();
        let absent = plan
            .iter()
            .map(|(state, _)| (*state, crate::snapshot::OptionalSnapshotRestore::ExplicitlyAbsent))
            .collect::<Vec<_>>();
        restore_required_snapshot_state_atomic(&absent, &live).unwrap();
        for (state, _) in &absent {
            assert!(!live.join(state.live_file).exists(), "{} must restore as explicit absence", state.live_file);
        }
        assert!(!stale.exists(), "explicit absence must not allow an atomic-recovery backup to resurrect old policy");
        assert!(live.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE).is_file());
    }

    #[test]
    fn snapshot_restore_preserves_live_champion_routing_and_gpu_supervision_choice() {
        let live = AppSettings {
            asr_model_size: AsrModelSize::WSL7B,
            use_finetuned_asr: false,
            multi_engine_hypotheses: false,
            external_asr_script_path: "C:/cortex/scripts/cortex_7b_client.py".to_string(),
            champion_supervision_enabled: false,
            ..AppSettings::default()
        };
        let mut restored = AppSettings {
            asr_model_size: AsrModelSize::CTC300M,
            use_finetuned_asr: true,
            multi_engine_hypotheses: true,
            external_asr_script_path: "old-or-missing-client.py".to_string(),
            champion_supervision_enabled: true,
            vad_threshold: 0.77,
            ..AppSettings::default()
        };

        preserve_live_asr_runtime_controls(&mut restored, &live);

        assert_eq!(restored.asr_model_size, AsrModelSize::WSL7B);
        assert!(!restored.use_finetuned_asr);
        assert!(!restored.multi_engine_hypotheses);
        assert_eq!(restored.external_asr_script_path, live.external_asr_script_path);
        assert!(!restored.champion_supervision_enabled, "a restore must not auto-load the 30 GB server");
        assert_eq!(restored.vad_threshold, 0.77, "ordinary snapshot configuration should still restore");
    }

    #[test]
    fn snapshot_pilot_policy_is_exact_atomic_and_fail_closed_during_restore() {
        let temp = tempfile::tempdir().unwrap();
        let snapshot_dir = temp.path().join("snapshot_1");
        let live_dir = temp.path().join("live");
        std::fs::create_dir_all(&snapshot_dir).unwrap();
        std::fs::create_dir_all(&live_dir).unwrap();
        let snapshot_db = snapshot_dir.join("cortex-speech.db");
        let source = crate::db::Database::open(snapshot_db.to_string_lossy().as_ref()).unwrap();
        source.initialize().unwrap();
        source.wal_checkpoint().unwrap();
        drop(source);

        let policy = br#"{
          "schema_version": 1,
          "after_review_event_id": 0,
          "max_total_corpus_actions": 20,
          "reviewers": [
            {"name": "Pavel", "max_corpus_actions": 10},
            {"name": "Hawzhin", "max_corpus_actions": 10}
          ]
        }"#;
        std::fs::write(snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_FILE), policy).unwrap();
        let legacy_policy = inspect_snapshot_pilot_policy(&snapshot_dir, &snapshot_db, false).unwrap_err();
        assert!(
            legacy_policy.contains("requires a verified manifest"),
            "a manifestless policy-bearing tree can never authorize a named restore: {legacy_policy}"
        );
        crate::review_pilot::install_test_focus(&snapshot_dir, ["snapshot-focus"]);
        std::fs::write(
            snapshot_dir.join(crate::voice_focus::VOICE_FOCUS_FILE),
            br#"{"segment_ids":["snapshot-wrong"]}"#,
        )
        .unwrap();
        let wrong_focus = inspect_snapshot_pilot_policy(&snapshot_dir, &snapshot_db, true).unwrap_err();
        assert!(wrong_focus.contains("digest mismatch"), "{wrong_focus}");
        crate::review_pilot::install_test_focus(&snapshot_dir, ["snapshot-focus"]);
        let install = inspect_snapshot_pilot_policy(&snapshot_dir, &snapshot_db, true).unwrap();
        assert!(matches!(install, SnapshotPilotPolicyRestore::Install(_)));
        crate::review_pilot::install_test_focus(&live_dir, ["snapshot-focus"]);

        // Both representations are ambiguous and must fail before any DB swap.
        std::fs::write(
            snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE),
            crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES,
        )
        .unwrap();
        assert!(inspect_snapshot_pilot_policy(&snapshot_dir, &snapshot_db, false).is_err());
        std::fs::remove_file(snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE)).unwrap();

        std::fs::write(live_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"pending").unwrap();
        assert!(
            crate::review_pilot::load(&live_dir).unwrap_err().contains("restore did not finish"),
            "an interrupted cross-file restore must block paid review"
        );
        apply_snapshot_pilot_policy(&install, &live_dir).unwrap();
        assert!(
            crate::review_pilot::load(&live_dir).is_err(),
            "installing policy is not the commit point; the pending barrier remains authoritative"
        );
        clear_review_pilot_restore_pending(&live_dir).unwrap();
        assert_eq!(crate::review_pilot::load(&live_dir).unwrap().unwrap().reviewer_names(), vec!["Hawzhin", "Pavel"]);

        // Explicit absence removes an existing policy only under the same fail-closed marker.
        std::fs::remove_file(snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_FILE)).unwrap();
        std::fs::write(
            snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE),
            crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES,
        )
        .unwrap();
        let absent = inspect_snapshot_pilot_policy(&snapshot_dir, &snapshot_db, false).unwrap();
        assert_eq!(absent, SnapshotPilotPolicyRestore::ExplicitlyAbsent);
        std::fs::write(live_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"pending").unwrap();
        let stale_policy_backup =
            live_dir.join(format!("{}.replace-bak-99999", crate::review_pilot::REVIEW_PILOT_FILE));
        std::fs::write(&stale_policy_backup, policy).unwrap();
        apply_snapshot_pilot_policy(&absent, &live_dir).unwrap();
        assert!(!stale_policy_backup.exists(), "explicit absence must remove recoverable stale policy bytes");
        assert!(crate::review_pilot::load(&live_dir).is_err());
        let stale_barrier_backup =
            live_dir.join(format!("{}.replace-bak-99999", crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE));
        std::fs::write(&stale_barrier_backup, b"stale pending").unwrap();
        clear_review_pilot_restore_pending(&live_dir).unwrap();
        assert!(!stale_barrier_backup.exists(), "a completed restore must not resurrect its barrier");
        assert_eq!(crate::review_pilot::load(&live_dir), Ok(None));

        // Legacy snapshots remain recoverable but can never delete or replace a current live policy.
        std::fs::remove_file(snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE)).unwrap();
        assert!(
            inspect_snapshot_pilot_policy(&snapshot_dir, &snapshot_db, true).is_err(),
            "manifest-bearing snapshots can never infer unrestricted state from missing files"
        );
        assert_eq!(
            inspect_snapshot_pilot_policy(&snapshot_dir, &snapshot_db, false).unwrap(),
            SnapshotPilotPolicyRestore::PreserveLegacy
        );
    }

    #[test]
    fn bare_database_restore_refuses_active_or_uncertain_controlled_pilot_state() {
        let live = tempfile::tempdir().unwrap();
        assert!(refuse_bare_restore_during_controlled_pilot(live.path()).is_ok());

        let policy = br#"{
          "schema_version": 1,
          "after_review_event_id": 0,
          "max_total_corpus_actions": 20,
          "reviewers": [
            {"name": "Hawzhin", "max_corpus_actions": 10},
            {"name": "Pavel", "max_corpus_actions": 10}
          ]
        }"#;
        crate::review_pilot::install_test_focus(live.path(), ["live-focus"]);
        std::fs::write(live.path().join(crate::review_pilot::REVIEW_PILOT_FILE), policy).unwrap();
        let active = refuse_bare_restore_during_controlled_pilot(live.path()).unwrap_err();
        assert!(active.contains("policy-bearing named snapshot"), "{active}");

        std::fs::remove_file(live.path().join(crate::review_pilot::REVIEW_PILOT_FILE)).unwrap();
        let remembered = serde_json::json!({
            "reviewers": {},
            "db_path": "remembered.db",
            "pilot_policy": serde_json::from_slice::<serde_json::Value>(policy).unwrap(),
        });
        std::fs::write(live.path().join("couch_session.json"), serde_json::to_vec(&remembered).unwrap()).unwrap();
        let durable = refuse_bare_restore_during_controlled_pilot(live.path()).unwrap_err();
        assert!(durable.contains("durable Couch session"), "{durable}");
        std::fs::remove_file(live.path().join("couch_session.json")).unwrap();

        std::fs::write(live.path().join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"pending").unwrap();
        let uncertain = refuse_bare_restore_during_controlled_pilot(live.path()).unwrap_err();
        assert!(uncertain.contains("not provably safe"), "{uncertain}");
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

    /// A real, decodable silence WAV of `duration_ms` so `get_duration_ms` returns a true duration —
    /// required for the source-reference POSITIONAL window (round-24 hunt #15). A source-reference
    /// auto-commit needs the source duration AND the segment's offsets; production always has both
    /// (real audio + chunked segments), so the reference-commit tests must supply both too.
    fn real_source_audio(dir: &tempfile::TempDir, name: &str, duration_ms: u32) -> String {
        let audio = dir.path().join(name);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&audio, spec).expect("create wav");
        for _ in 0..(16_000u32 * duration_ms / 1000) {
            writer.write_sample(0i16).expect("write sample");
        }
        writer.finalize().expect("finalize wav");
        audio.to_string_lossy().to_string()
    }

    /// Whole-file source-offset meta (chunk 0 of 1) so a single-segment source spans the whole
    /// reference — a real positional window covering the entire transcript.
    fn whole_file_alignment(duration_ms: i64) -> String {
        crate::chunking::SegmentSourceMeta {
            source_start_ms: 0,
            source_end_ms: duration_ms,
            chunk_index: 0,
            chunk_count: 1,
        }
        .to_alignment_json()
    }

    /// The jury tests below exercise the reference/guard/commit MACHINERY, which only runs when the
    /// Autonomy Dial permits machine commits. The shipped default (AutonLevel::Propose) stages
    /// instead of committing — that contract is pinned separately by
    /// autonomy_dial_governs_every_machine_commit_stage_not_just_t0 — so these tests opt into
    /// ActConfirm, the level whose semantics they were written against.
    fn settings_act_confirm() -> crate::settings::AppSettings {
        crate::settings::AppSettings {
            // These tests intentionally exercise the legacy multi-model jury machinery. Production
            // defaults to WSL7B and bypasses that machinery entirely.
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            jury_autonomy_level: crate::settings::AutonLevel::ActConfirm,
            ..crate::settings::AppSettings::default()
        }
    }

    fn settings_with_source_reference_models(models: &[&str]) -> crate::settings::AppSettings {
        crate::settings::AppSettings {
            source_reference_models: models.iter().map(|model| (*model).to_string()).collect(),
            ..settings_act_confirm()
        }
    }

    #[test]
    fn t2_endpoint_resolves_gemini_by_default_and_openrouter_only_with_a_key() {
        use crate::jury::t2_listener::T2Endpoint;
        let mut s = crate::settings::AppSettings::default();

        // Default provider ("gemini") -> direct Gemini with the passed key + configured jury_model,
        // even when an OpenRouter key happens to be available.
        let (ep, key, model) = resolve_t2_endpoint_from_keys(&s, "gkey", Some("orkey"));
        assert!(matches!(ep, T2Endpoint::GeminiDirect));
        assert_eq!(key, "gkey");
        assert_eq!(model, s.jury_model);

        // Provider "openrouter" but NO OpenRouter key -> fall back to direct Gemini (never keyless cloud).
        s.jury_provider = "openrouter".into();
        let (ep, key, _) = resolve_t2_endpoint_from_keys(&s, "gkey", None);
        assert!(matches!(ep, T2Endpoint::GeminiDirect), "no OR key must stay on Gemini");
        assert_eq!(key, "gkey");
        let (ep, _, _) = resolve_t2_endpoint_from_keys(&s, "gkey", Some("   "));
        assert!(matches!(ep, T2Endpoint::GeminiDirect), "blank OR key must stay on Gemini");

        // Provider "openrouter" WITH a key -> OpenRouter endpoint, the OR key, mapped model slug.
        s.jury_model = "gemini-2.5-pro".into();
        let (ep, key, model) = resolve_t2_endpoint_from_keys(&s, "gkey", Some("orkey"));
        match ep {
            T2Endpoint::OpenAiCompatible { url } => assert!(url.contains("openrouter.ai"), "{url}"),
            _ => panic!("expected OpenRouter endpoint"),
        }
        assert_eq!(key, "orkey");
        assert_eq!(model, "google/gemini-2.5-pro", "a bare Gemini id maps to the OpenRouter slug");

        // An already-slugged model passes through unchanged (mechanism only — policy is that the ckb
        // judge is strictly google/gemini-2.5-pro until another model has a measured ckb CER).
        s.jury_model = "example/future-approved-judge".into();
        let (_, _, model) = resolve_t2_endpoint_from_keys(&s, "gkey", Some("orkey"));
        assert_eq!(model, "example/future-approved-judge");
    }

    #[test]
    fn apply_curation_fields_touches_only_whitelisted_fields_and_rejects_unknown_keys() {
        // F10 root fix: the partial autosave path must be able to change ONLY the whitelisted curation
        // fields (annotatedTranscript, speakerId, alignmentJson, verified); everything else in the row must
        // be bit-identical after the apply, and an unknown key must be a loud error (a typo'd field must
        // never look saved).
        let mut seg = test_segment("s1", "/audio/a.wav", "raw text");
        seg.verified = true;
        seg.confidence = Some(0.42);
        let before = seg.clone();

        let fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"annotatedTranscript": "دەق", "speakerId": "SPEAKER_01"}"#).unwrap();
        apply_curation_fields(&mut seg, &fields).unwrap();
        assert_eq!(seg.annotated_transcript.as_deref(), Some("دەق"));
        assert_eq!(seg.speaker_id.as_deref(), Some("SPEAKER_01"));
        // Every non-curation column is untouched — the stale-store clobber class is closed by construction.
        assert_eq!(seg.verified, before.verified);
        assert_eq!(seg.confidence, before.confidence);
        assert_eq!(seg.raw_transcript, before.raw_transcript);
        assert_eq!(seg.audio_path, before.audio_path);
        assert_eq!(seg.alignment_json, before.alignment_json, "unprovided field stays untouched");

        // null clears a nullable field.
        let clear: serde_json::Map<String, serde_json::Value> = serde_json::from_str(r#"{"speakerId": null}"#).unwrap();
        apply_curation_fields(&mut seg, &clear).unwrap();
        assert_eq!(seg.speaker_id, None);

        // `verified` IS a whitelisted curation field now — handleToggleVerify routes through this field-level
        // path (not a whole-row upsert) so a concurrent WSL-7B refinement write is never reverted. It applies
        // as a bool, and a non-bool is a loud error.
        let ver: serde_json::Map<String, serde_json::Value> = serde_json::from_str(r#"{"verified": false}"#).unwrap();
        apply_curation_fields(&mut seg, &ver).unwrap();
        assert!(!seg.verified, "verified applies as a bool");
        assert_eq!(seg.raw_transcript, before.raw_transcript, "verifying must not touch raw_transcript");
        let ver_bad: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"verified": "yes"}"#).unwrap();
        assert!(apply_curation_fields(&mut seg, &ver_bad).is_err(), "a non-bool verified must be a loud error");

        // A genuinely non-whitelisted key -> loud error, row unchanged.
        let bad: serde_json::Map<String, serde_json::Value> = serde_json::from_str(r#"{"confidence": 0.9}"#).unwrap();
        let err = apply_curation_fields(&mut seg, &bad).unwrap_err();
        assert!(err.contains("unsupported field 'confidence'"), "{err}");
        assert_eq!(seg.confidence, before.confidence, "non-whitelisted field must not change");

        // Wrong value type -> loud error.
        let wrong: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"annotatedTranscript": 7}"#).unwrap();
        assert!(apply_curation_fields(&mut seg, &wrong).is_err());
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
    fn agentic_readiness_offline_source_reference_is_not_required_not_blocked() {
        let readiness = build_agentic_readiness(
            &crate::settings::AppSettings::default(), // cloud off, no models
            &[],
            &serde_json::json!({
                "available": false,
                "message": "No external ASR provider script configured"
            }),
        );

        // Cloud whole-file references are OPTIONAL — with cloud off (offline by choice) the check must
        // NOT nag, and must NOT claim coverage either. It previously said "ready", which painted a
        // switched-off dependency the same emerald as proven coverage (deep audit 2026-08-05). It is
        // now `not_required`: neither a warning nor a green tick.
        assert!(
            readiness.checks.iter().any(|c| c.id == "source_reference" && c.status == "not_required"),
            "an optional dependency that is OFF must report not_required, never ready"
        );
        assert!(
            !readiness.checks.iter().any(|c| c.id == "source_reference" && c.status == "ready"),
            "a disabled cloud cross-check must not claim readiness"
        );
        assert_eq!(readiness.status, "blocked");
        assert!(!readiness.ready);
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.id == "hypothesis_coverage" && check.status == "not_required"));
        assert!(readiness.available_hypothesis_models.is_empty());
        assert_eq!(readiness.required_hypothesis_models, quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE);
    }

    /// The whole risk of introducing `not_required` is that it silently becomes a nag: if the
    /// aggregate treated an unknown status as not-ready, switching cloud OFF would flip the overall
    /// verdict and pester the owner on every import — the exact outcome the original "ready" was
    /// chosen to avoid. This pins that the new state is visible per-check WITHOUT changing the verdict.
    #[test]
    fn disabled_cloud_reports_not_required_not_ready_but_keeps_the_overall_verdict_ready() {
        let settings = crate::settings::AppSettings {
            jury_cloud_opt_in: false, // the state under test
            external_asr_script_path: "/root/cortex_env/omniasr.py".to_string(),
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

        assert!(
            readiness.checks.iter().any(|c| c.id == "source_reference" && c.status == "not_required"),
            "checks: {:?}",
            readiness.checks.iter().map(|c| (&c.id, &c.status)).collect::<Vec<_>>()
        );
        assert_eq!(
            readiness.status, "ready",
            "not_required must not degrade the overall verdict — that would nag on every import"
        );
        assert!(readiness.ready, "the aggregate `ready` flag must stay true for an off-by-choice option");
    }

    #[test]
    fn champion_readiness_ignores_installed_optional_ctc_models() {
        let settings = crate::settings::AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: "session-key".to_string(),
            external_asr_script_path: "/root/cortex_env/omniasr.py".to_string(),
            source_reference_models: vec!["gemini-2.5-pro".to_string()],
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
        assert_eq!(readiness.available_hypothesis_models, vec!["omniasr-wsl-7b".to_string()]);
        assert!(readiness.checks.iter().any(|check| {
            check.id == "hypothesis_coverage"
                && check.status == "not_required"
                && check.detail.contains("Optional engines will not run")
        }));
        assert_eq!(
            readiness.required_hypothesis_models,
            quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE,
            "operational readiness must not weaken the separate training/export proof threshold"
        );
    }

    #[test]
    fn local_readiness_checks_the_exact_explicitly_selected_engine() {
        let settings = crate::settings::AppSettings {
            asr_model_size: AsrModelSize::CTC1B,
            ..crate::settings::AppSettings::default()
        };
        let model_status = vec![
            downloaded_model_status(models::OMNIASR_CTC_1B_MODEL),
            downloaded_model_status(models::OMNIASR_CTC_1B_TOKENS),
        ];
        let readiness = build_agentic_readiness(
            &settings,
            &model_status,
            &serde_json::json!({ "available": false, "message": "WSL is unavailable" }),
        );

        assert_eq!(readiness.status, "ready");
        assert_eq!(readiness.available_hypothesis_models, vec!["omniasr-ctc-1b".to_string()]);
        assert!(readiness.checks.iter().any(|check| check.id == "primary_asr" && check.status == "ready"));
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
        let audio_path = real_source_audio(&dir, "source.wav", 4000);
        let mut segment = test_segment("seg-reference-first", &audio_path, "wrong local consensus");
        segment.alignment_json = Some(whole_file_alignment(4000)); // real positional window
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
    fn champion_review_filters_every_stale_auxiliary_vote() {
        let mut segment = test_segment("champion-consensus", "/audio/champion.wav", "champion draft");
        segment.model_version_id = Some(crate::pipeline::CHAMPION_MODEL_ID.to_string());
        let hypotheses = vec![
            crate::db::SegmentHypothesis {
                segment_id: segment.id.clone(),
                model_id: "omniasr-ctc-300m".to_string(),
                transcript: "stale 300m draft".to_string(),
                confidence: Some(0.99),
            },
            crate::db::SegmentHypothesis {
                segment_id: segment.id.clone(),
                model_id: "finetuned-mms-ckb".to_string(),
                transcript: "stale mms draft".to_string(),
                confidence: Some(0.99),
            },
            crate::db::SegmentHypothesis {
                segment_id: segment.id.clone(),
                model_id: "scribe-v1".to_string(),
                transcript: "stale cloud draft".to_string(),
                confidence: Some(0.99),
            },
        ];

        let filtered = hypotheses_for_selected_asr(&crate::settings::AsrModelSize::WSL7B, &segment, hypotheses, true);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].model_id, crate::pipeline::CHAMPION_MODEL_ID);
        assert_eq!(filtered[0].transcript, "champion draft");
    }

    #[test]
    fn champion_mode_jury_is_a_no_write_human_review_handoff() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let mut segment = test_segment("champion-no-jury", "/audio/champion.wav", "champion draft");
        segment.model_version_id = Some(crate::pipeline::CHAMPION_MODEL_ID.to_string());
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "stale smaller-model vote", 0.99);

        let settings = crate::settings::AppSettings {
            asr_model_size: crate::settings::AsrModelSize::WSL7B,
            multi_engine_hypotheses: true,
            jury_cloud_opt_in: true,
            llm_api_key: "must-not-be-used".to_string(),
            ..crate::settings::AppSettings::default()
        };
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["mode"], "not_required");
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert!(fresh.verdict.is_none(), "champion handoff must not manufacture a machine verdict");
        assert_eq!(fresh.raw_transcript, "champion draft");
    }

    #[test]
    fn autonomy_dial_governs_every_machine_commit_stage_not_just_t0() {
        // Round-24 hunt #1 (HIGH): under Observe/Propose the dial was enforced only inside
        // run_t0_gate — the SAME pipeline run then machine-committed 'jury_accept' via the
        // reference-selection stage (and T2), silently removing from the human queue the very
        // segments the dial promised to stage.

        // ── Propose: a committable reference selection must STAGE, never commit. ──
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "propose.wav");
        let segment = test_segment("seg-propose", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "wrong local consensus", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "wrong local consensus", 0.95);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "correct reference phrase", 0.90);
        insert_source_reference(&db, &audio_path, "correct reference phrase");

        let mut settings = settings_with_source_reference_models(&["gemini-2.5-pro"]);
        settings.jury_autonomy_level = crate::settings::AutonLevel::Propose;
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0), "Propose must never machine-commit: {report}");
        assert_eq!(
            fresh.verdict.as_deref(),
            Some("escalated"),
            "Propose stages the segment for the human, not 'jury_accept'"
        );
        assert!(fresh.escalated, "the staged segment must sit in the human escalation queue");
        assert_eq!(report["humanInbox"].as_u64(), Some(1), "the staged segment is reported in humanInbox: {report}");

        // ── Observe: the pipeline writes NOTHING — a pre-staged verdict survives untouched. ──
        // (Before the fix, the T2-disabled fallback REWROTE the verdict rationale and NULLed the
        // IRT confidence of every escalated segment fed back through the review loop.)
        let db2 = crate::db::Database::open(":memory:").unwrap();
        db2.initialize().unwrap();
        let seg2 = test_segment("seg-observe", "/audio/observe.wav", "draft");
        db2.insert_segment(&seg2).unwrap();
        insert_hypothesis(&db2, &seg2.id, "omniasr-wsl-7b", "draft", 0.9);
        insert_hypothesis(&db2, &seg2.id, "omniasr-ctc-300m", "other draft", 0.5);
        db2.write_segment_verdict(&seg2.id, "escalated", None, Some("original rationale"), None, Some(0.42), true)
            .unwrap();

        let observe = crate::settings::AppSettings {
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            jury_autonomy_level: crate::settings::AutonLevel::Observe,
            ..crate::settings::AppSettings::default()
        };
        run_jury_pipeline_core(&db2, &observe, vec![seg2.id.clone()]).unwrap();
        let fresh2 = db2.get_segment_by_id(&seg2.id).unwrap().unwrap();
        assert_eq!(fresh2.rationale.as_deref(), Some("original rationale"), "Observe must not rewrite verdicts");
        assert_eq!(fresh2.agreement_score, Some(0.42), "Observe must not NULL the staged IRT confidence");

        // ── Propose: an already-staged segment keeps its confidence (riskiest-first ordering). ──
        let propose = crate::settings::AppSettings {
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            jury_autonomy_level: crate::settings::AutonLevel::Propose,
            ..crate::settings::AppSettings::default()
        };
        run_jury_pipeline_core(&db2, &propose, vec![seg2.id.clone()]).unwrap();
        let fresh3 = db2.get_segment_by_id(&seg2.id).unwrap().unwrap();
        assert_eq!(
            fresh3.agreement_score,
            Some(0.42),
            "Propose must not clobber an already-staged segment's IRT confidence"
        );
        assert!(fresh3.escalated, "the segment stays in the human queue");
    }

    /// The provenance string must not depend on the order the references were written.
    ///
    /// `agreeing_source_references_preserve_per_model_evidence` below caught the defect by LUCK: it
    /// asserts one exact string, and the suite happened to fail on the run where SQLite returned the
    /// two same-second rows the other way round. Passing it five times proves nothing, because
    /// nothing in the test controls that tie. This one does: same two models, INSERTED IN THE
    /// OPPOSITE ORDER, must produce the identical canonical value — which cannot hold unless the
    /// join is sorted, whatever the query hands back.
    #[test]
    fn the_consensus_provenance_string_is_canonical_not_insertion_ordered() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = real_source_audio(&dir, "reference-order.wav", 4000);
        let mut segment = test_segment("seg-reference-order", &audio_path, "wrong local consensus");
        segment.alignment_json = Some(whole_file_alignment(4000));
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "correct reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        // FLASH FIRST — the reverse of the sibling test.
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "correct reference phrase");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "correct reference phrase");

        run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();
        let evidence: serde_json::Value =
            serde_json::from_str(fresh.evidence_json.as_deref().expect("reference evidence json")).unwrap();
        assert_eq!(
            evidence.get("referenceModelId").and_then(serde_json::Value::as_str),
            Some("multi-reference-consensus:gemini-2.5-flash+gemini-2.5-pro"),
            "the same set of agreeing models must render identically however they were written — a \
             string that changes between identical runs is not provenance"
        );
    }

    #[test]
    fn agreeing_source_references_preserve_per_model_evidence() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = real_source_audio(&dir, "agreeing-references.wav", 4000);
        let mut segment = test_segment("seg-reference-agreement", &audio_path, "wrong local consensus");
        segment.alignment_json = Some(whole_file_alignment(4000)); // real positional window
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "correct reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "correct reference phrase");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "correct reference phrase");

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("jury_accept"));
        assert_eq!(fresh.verdict_transcript.as_deref(), Some("correct reference phrase"));
        let evidence: serde_json::Value =
            serde_json::from_str(fresh.evidence_json.as_deref().expect("reference evidence json")).unwrap();
        // CANONICAL (sorted), not insertion order. This assertion is what caught the defect: the
        // same binary produced `pro+flash` on one run and `flash+pro` on another, because the two
        // references are written in the same second and the ORDER BY had no tiebreaker. A provenance
        // string that changes between identical runs is not provenance.
        assert_eq!(
            evidence.get("referenceModelId").and_then(serde_json::Value::as_str),
            Some("multi-reference-consensus:gemini-2.5-flash+gemini-2.5-pro")
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

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
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
        // The sole canonical advisory model exists but has no usable text: coverage must fail closed
        // without inventing a second cloud model merely to exercise the guard.
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "");

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Source-reference coverage guard blocked automatic adjudication"));
        assert!(rationale.contains("gemini-2.5-pro"));
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

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
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

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        assert!(fresh.rationale.as_deref().unwrap_or("").contains("T2 disabled"));
    }

    /// A bad CORTEX_BATCH_CONCURRENCY must fall back to SERIAL, never to a wide fan-out.
    ///
    /// This knob decides how many clips are pushed at the ASR server at once. The failure that
    /// matters is not "too slow" — it is a typo silently turning into 32 concurrent requests at a
    /// two-replica server, or a 0 that spawns no workers and hangs the batch forever. Every
    /// unusable value therefore resolves to 1, which is exactly the behaviour this command had
    /// before concurrency existed.
    #[test]
    fn batch_concurrency_falls_back_to_serial_for_every_unusable_value() {
        assert_eq!(parse_batch_concurrency(None), 1, "absent -> serial");
        assert_eq!(parse_batch_concurrency(Some("")), 1, "empty -> serial");
        assert_eq!(parse_batch_concurrency(Some("0")), 1, "zero would spawn no workers and hang");
        assert_eq!(parse_batch_concurrency(Some("-4")), 1, "negative -> serial");
        assert_eq!(parse_batch_concurrency(Some("eight")), 1, "unparseable -> serial");
        assert_eq!(parse_batch_concurrency(Some("33")), 1, "above the cap -> serial, never uncapped");
        assert_eq!(parse_batch_concurrency(Some("999999")), 1, "absurd -> serial");

        assert_eq!(parse_batch_concurrency(Some("1")), 1);
        assert_eq!(parse_batch_concurrency(Some("8")), 8, "a sane value is honoured");
        assert_eq!(parse_batch_concurrency(Some(" 8 ")), 8, "surrounding whitespace is tolerated");
        assert_eq!(parse_batch_concurrency(Some("32")), 32, "the cap itself is allowed");
    }
}
