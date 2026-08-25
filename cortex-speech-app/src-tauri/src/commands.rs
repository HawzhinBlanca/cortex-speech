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
    let job = state.job_store().find_interrupted_import().map_err(|error| error.to_string())?;
    let Some(job) = job else { return Err("No interrupted import to resume".into()) };
    let dir_path = std::path::PathBuf::from(&job.dir);
    if !dir_path.is_dir() {
        return Err(format!("The import folder no longer exists: {}", job.dir));
    }
    let completed: std::collections::HashSet<String> = job.completed_paths.into_iter().collect();

    state.try_start_import()?;
    {
        // Retire the crashed job now that we are resuming it; the fresh import job supersedes it.
        let _ = state.job_store().discard_interrupted_import(&job.id);
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
/// The terminal cause of a batch run, or `None` — the ONLY shape that may be reported as `completed`.
///
/// A per-clip failure comes first because it is the harder stop (clips were left undrafted). A
/// post-batch jury failure keeps its own wording: every clip WAS drafted, so borrowing the per-clip
/// "remaining clips were not transcribed" phrasing would be its own small lie.
fn batch_terminal_halt_cause(clip_failure: Option<String>, jury_failure: Option<String>) -> Option<String> {
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
    state
        .segment_queries()
        .get_segment(&segment_id)
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
    state
        .segment_queries()
        .get_segments_page(verified, query.as_deref(), &sort, limit, cursor.as_deref(), focus.as_deref())
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
    state
        .segment_queries()
        .get_segment_ids_for_view(verified, query.as_deref(), &transcript_state)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_signal_anomaly_segments(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_signal_anomaly_segments")?;
    state.segment_queries().get_signal_anomaly_segments(limit.unwrap_or(100)).map_err(|e| e.to_string())
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
    let store = state.job_store();
    run_blocking(move || store.list_recent(50).map_err(|error| error.to_string())).await
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
    // The runtime grants one bounded, restore-gated query snapshot, so a slow external-drive backup
    // neither monopolizes the serialized writer nor crosses a restore generation.
    let database = state.db_runtime();
    run_blocking(move || {
        let backup_db = database.open_read().map_err(|e| e.to_string())?;
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

// Compatibility re-exports keep established command/subcommand paths stable while process-level
// connection ownership and restore admission live behind DatabaseRuntime.
pub(crate) use crate::database_runtime::{
    begin_mutation, restore_pending, RestoreAdmission, RestoreReservation, RESTORE_ADMISSION, RESTORE_IN_PROGRESS_MSG,
};

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

/// These rows are irreversible review/payment evidence, not ordinary dataset state. A restore may
/// add rows, but it may never make any exact pre-restore row disappear or change one of its values.
/// Keep this list explicit so adding another monetary/audit authority requires a conscious review.
const DURABLE_REVIEW_RESTORE_TABLES: [&str; 33] = [
    "review_pilot_hidden_keys",
    "review_events",
    "spot_checks",
    "review_compensation_ledger",
    "review_compensation_settlements",
    "review_compensation_policies",
    "review_effect_state",
    "human_decision_effect_events",
    "human_decision_effect_reversals",
    "review_flag_effect_events",
    "review_flag_effect_reversals",
    "correction_memory",
    "correction_memory_contributions",
    "corrections",
    "playback_receipts",
    "legacy_agent_examples_v60",
    "legacy_corrections_v60",
    "legacy_reviewed_segments_v60",
    "legacy_machine_verdict_segments_v60",
    "review_campaign_registry",
    "review_campaign_focus",
    "review_campaign_transitions",
    "independent_review_decisions",
    "independent_review_reversals",
    "review_campaign_adjudications",
    "review_pool_registry",
    "review_pool_members",
    "review_pool_decisions",
    "review_pool_reversals",
    "review_pool_owner_adjudications",
    "review_pool_voice_certificates",
    "review_pool_dedup_manifests",
    "review_pool_duplicate_exclusions",
];

const EFFECT_BOUND_AGENT_EXAMPLES_RESTORE_PROJECTION: &str =
    "SELECT * FROM agent_examples WHERE effect_event_id IS NOT NULL";
const LEGACY_CORRECTION_MEMORY_RESTORE_PROJECTION: &str = "SELECT * FROM correction_memory WHERE legacy_seed = 1";
const LEGACY_AGENT_EXAMPLES_RESTORE_PROJECTION: &str = "SELECT * FROM legacy_agent_examples_v60";
const LEGACY_CORRECTIONS_RESTORE_PROJECTION: &str = "SELECT * FROM legacy_corrections_v60";
const LEGACY_REVIEWED_SEGMENTS_RESTORE_PROJECTION: &str = "SELECT * FROM legacy_reviewed_segments_v60";
const LEGACY_MACHINE_VERDICTS_RESTORE_PROJECTION: &str = "SELECT * FROM legacy_machine_verdict_segments_v60";

const REVIEWED_SEGMENT_RESTORE_PROJECTION: &str = "SELECT segment.id,
            segment.audio_content_hash,
            segment.audio_fingerprint,
            segment.alignment_json,
            segment.duration_ms,
            segment.human_decision,
            segment.verdict,
            segment.verdict_transcript,
            segment.annotated_transcript,
            segment.verified,
            segment.reviewed_by,
            segment.corrected_at,
            segment.review_revision,
            segment.escalated,
            segment.is_gold
       FROM speech_segments segment
      WHERE segment.human_decision IS NOT NULL
         OR segment.reviewed_by IS NOT NULL
         OR (
            segment.verified = 1
            AND (segment.annotated_transcript IS NOT NULL OR segment.verdict_transcript IS NOT NULL)
         )
         OR EXISTS (
            SELECT 1 FROM review_events event
             WHERE event.segment_id = segment.id
               AND event.source <> 'couch_spot_check'
               AND event.action IN ('accept', 'edit', 'reject')
      )
         OR EXISTS (
            SELECT 1 FROM review_compensation_ledger ledger
             WHERE ledger.segment_id = segment.id
               AND ledger.compensation_action = 'undo'
      )";

fn encode_durable_sqlite_value(value: rusqlite::types::ValueRef<'_>, encoded: &mut Vec<u8>) {
    use rusqlite::types::ValueRef;

    match value {
        ValueRef::Null => encoded.push(0),
        ValueRef::Integer(value) => {
            encoded.push(1);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        ValueRef::Real(value) => {
            encoded.push(2);
            encoded.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        ValueRef::Text(value) => {
            encoded.push(3);
            encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
            encoded.extend_from_slice(value);
        }
        ValueRef::Blob(value) => {
            encoded.push(4);
            encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
            encoded.extend_from_slice(value);
        }
    }
}

fn exact_query_rows(db: &crate::db::Database, label: &str, sql: &str) -> Result<(Vec<String>, Vec<Vec<u8>>), String> {
    let mut statement = db
        .connection()
        .prepare(sql)
        .map_err(|error| format!("durable restore floor {label} is unreadable: {error}"))?;
    let columns = statement.column_names().iter().map(|name| (*name).to_string()).collect::<Vec<_>>();
    let column_count = statement.column_count();
    let mut query =
        statement.query([]).map_err(|error| format!("durable restore floor {label} cannot be scanned: {error}"))?;
    let mut rows = Vec::new();
    while let Some(row) =
        query.next().map_err(|error| format!("durable restore floor {label} cannot be scanned: {error}"))?
    {
        let mut encoded = Vec::new();
        for column in 0..column_count {
            let value = row
                .get_ref(column)
                .map_err(|error| format!("durable restore floor {label} has an unreadable value: {error}"))?;
            encode_durable_sqlite_value(value, &mut encoded);
        }
        rows.push(encoded);
    }
    Ok((columns, rows))
}

fn exact_table_rows(db: &crate::db::Database, table: &str) -> Result<(Vec<String>, Vec<Vec<u8>>), String> {
    // `table` is selected only from DURABLE_REVIEW_RESTORE_TABLES, never caller input.
    exact_query_rows(db, &format!("table {table}"), &format!("SELECT * FROM \"{table}\""))
}

fn require_encoded_row_superset(
    label: &str,
    floor_columns: Vec<String>,
    floor_rows: Vec<Vec<u8>>,
    target_columns: Vec<String>,
    target_rows: Vec<Vec<u8>>,
) -> Result<(), String> {
    if target_columns != floor_columns {
        return Err(format!(
            "database restore refused: target {label} columns do not match the authoritative review-history floor"
        ));
    }
    let mut target_counts = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    for row in target_rows {
        *target_counts.entry(row).or_default() += 1;
    }
    let mut missing = 0usize;
    for row in floor_rows {
        match target_counts.get_mut(&row) {
            Some(count) if *count > 0 => *count -= 1,
            _ => missing += 1,
        }
    }
    if missing != 0 {
        return Err(format!(
            "database restore refused: target would drop or modify {missing} durable row(s) from {label}"
        ));
    }
    Ok(())
}

fn require_encoded_row_equality(
    label: &str,
    floor_columns: Vec<String>,
    floor_rows: Vec<Vec<u8>>,
    target_columns: Vec<String>,
    target_rows: Vec<Vec<u8>>,
) -> Result<(), String> {
    if target_columns != floor_columns {
        return Err(format!(
            "database restore refused: target {label} columns do not match the authoritative review-history floor"
        ));
    }
    let row_counts = |rows: Vec<Vec<u8>>| {
        let mut counts = std::collections::BTreeMap::<Vec<u8>, usize>::new();
        for row in rows {
            *counts.entry(row).or_default() += 1;
        }
        counts
    };
    if row_counts(floor_rows) != row_counts(target_rows) {
        return Err(format!(
            "database restore refused: target must exactly preserve {label}; pseudo-legacy additions are forbidden"
        ));
    }
    Ok(())
}

/// Require `target` to contain every exact durable row in `floor`. Values as well as identities are
/// compared with SQLite storage-class fidelity; a row with the same primary key but changed text,
/// amount, policy, timestamp, or REAL bits is therefore a regression, not a match.
fn require_durable_review_history_superset(
    floor: &crate::db::Database,
    target: &crate::db::Database,
) -> Result<(), String> {
    for table in DURABLE_REVIEW_RESTORE_TABLES {
        let (floor_columns, floor_rows) = exact_table_rows(floor, table)?;
        let (target_columns, target_rows) = exact_table_rows(target, table)?;
        require_encoded_row_superset(table, floor_columns, floor_rows, target_columns, target_rows)?;
    }
    let (floor_columns, floor_rows) =
        exact_query_rows(floor, "effect-bound agent examples", EFFECT_BOUND_AGENT_EXAMPLES_RESTORE_PROJECTION)?;
    let (target_columns, target_rows) =
        exact_query_rows(target, "effect-bound agent examples", EFFECT_BOUND_AGENT_EXAMPLES_RESTORE_PROJECTION)?;
    require_encoded_row_superset(
        "effect-bound agent examples",
        floor_columns,
        floor_rows,
        target_columns,
        target_rows,
    )?;
    let (floor_columns, floor_rows) =
        exact_query_rows(floor, "legacy correction memories", LEGACY_CORRECTION_MEMORY_RESTORE_PROJECTION)?;
    let (target_columns, target_rows) =
        exact_query_rows(target, "legacy correction memories", LEGACY_CORRECTION_MEMORY_RESTORE_PROJECTION)?;
    require_encoded_row_equality("legacy correction memories", floor_columns, floor_rows, target_columns, target_rows)?;
    for (label, projection) in [
        ("legacy agent-example snapshot", LEGACY_AGENT_EXAMPLES_RESTORE_PROJECTION),
        ("legacy correction snapshot", LEGACY_CORRECTIONS_RESTORE_PROJECTION),
        ("legacy reviewed-segment snapshot", LEGACY_REVIEWED_SEGMENTS_RESTORE_PROJECTION),
        ("legacy machine-verdict snapshot", LEGACY_MACHINE_VERDICTS_RESTORE_PROJECTION),
    ] {
        let (floor_columns, floor_rows) = exact_query_rows(floor, label, projection)?;
        let (target_columns, target_rows) = exact_query_rows(target, label, projection)?;
        require_encoded_row_equality(label, floor_columns, floor_rows, target_columns, target_rows)?;
    }
    let (floor_columns, floor_rows) =
        exact_query_rows(floor, "reviewed speech-segment export projection", REVIEWED_SEGMENT_RESTORE_PROJECTION)?;
    let (target_columns, target_rows) =
        exact_query_rows(target, "reviewed speech-segment export projection", REVIEWED_SEGMENT_RESTORE_PROJECTION)?;
    require_encoded_row_superset(
        "reviewed speech-segment export projection",
        floor_columns,
        floor_rows,
        target_columns,
        target_rows,
    )?;
    Ok(())
}

fn has_durable_review_activity(db: &crate::db::Database) -> Result<bool, String> {
    // The policy table is installed before the first paid action and is protected by exact-row
    // comparison below. Once any actual audit/payment/grant row exists, a bare DB-only swap is no
    // longer an adequate recovery protocol because it cannot bind the companion policy/config files.
    for table in [
        "review_pilot_hidden_keys",
        "review_events",
        "spot_checks",
        "review_compensation_ledger",
        "review_compensation_settlements",
        "human_decision_effect_events",
        "human_decision_effect_reversals",
        "review_flag_effect_events",
        "review_flag_effect_reversals",
        "correction_memory",
        "correction_memory_contributions",
        "corrections",
        "playback_receipts",
        "legacy_agent_examples_v60",
        "legacy_corrections_v60",
        "legacy_reviewed_segments_v60",
        "legacy_machine_verdict_segments_v60",
        "review_campaign_registry",
        "review_campaign_focus",
        "review_campaign_transitions",
        "independent_review_decisions",
        "independent_review_reversals",
        "review_campaign_adjudications",
        "review_pool_registry",
        "review_pool_members",
        "review_pool_decisions",
        "review_pool_reversals",
        "review_pool_owner_adjudications",
        "review_pool_voice_certificates",
        "review_pool_dedup_manifests",
        "review_pool_duplicate_exclusions",
    ] {
        let exists: bool = db
            .connection()
            .query_row(&format!("SELECT EXISTS(SELECT 1 FROM \"{table}\" LIMIT 1)"), [], |row| row.get(0))
            .map_err(|error| format!("bare restore could not verify durable review history in {table}: {error}"))?;
        if exists {
            return Ok(true);
        }
    }
    let effect_bound_example_exists: bool = db
        .connection()
        .query_row("SELECT EXISTS(SELECT 1 FROM agent_examples WHERE effect_event_id IS NOT NULL LIMIT 1)", [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("bare restore could not verify effect-bound human examples: {error}"))?;
    if effect_bound_example_exists {
        return Ok(true);
    }
    // The singleton exists in every pristine schema-v60 database.  It becomes durable activity only
    // when it records a non-empty pre-v60 frontier; presence alone must not disable a first restore.
    let nonempty_effect_frontier: bool = db
        .connection()
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM review_effect_state
                  WHERE singleton_key = 1
                    AND (effective_after_review_event_id > 0 OR effective_after_ledger_id > 0)
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("bare restore could not verify the review-effect frontier: {error}"))?;
    if nonempty_effect_frontier {
        return Ok(true);
    }
    let reviewed_truth_exists: bool = db
        .connection()
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM ({REVIEWED_SEGMENT_RESTORE_PROJECTION}) LIMIT 1)"),
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("bare restore could not verify reviewed segment truth: {error}"))?;
    if reviewed_truth_exists {
        return Ok(true);
    }
    Ok(false)
}

fn exact_review_entitlement(duration_ms: i64, basis_points: i64) -> Result<i64, String> {
    if duration_ms <= 0 || !(0..=10_000).contains(&basis_points) {
        return Err("review compensation has invalid duration or basis points".to_string());
    }
    let numerator = i128::from(duration_ms)
        .checked_mul(i128::from(crate::db::REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR))
        .and_then(|value| value.checked_mul(i128::from(basis_points)))
        .ok_or_else(|| "review compensation arithmetic overflow".to_string())?;
    let denominator = 3_600_000_i128 * 10_000_i128;
    if numerator % denominator != 0 {
        return Err("review compensation duration/rate is not an exact micro-IQD amount".to_string());
    }
    i64::try_from(numerator / denominator)
        .map_err(|_| "review compensation entitlement exceeds the supported integer range".to_string())
}

fn review_action_basis_points(action: &str) -> Option<i64> {
    match action {
        "edit" => Some(crate::db::REVIEW_PAY_EDIT_BPS),
        "accept" => Some(crate::db::REVIEW_PAY_ACCEPT_BPS),
        "reject" => Some(crate::db::REVIEW_PAY_REJECT_BPS),
        "skip" => Some(crate::db::REVIEW_PAY_SKIP_BPS),
        _ => None,
    }
}

fn is_canonical_lowercase_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).map(|parsed| parsed.hyphenated().to_string() == value).unwrap_or(false)
}

fn is_canonical_lowercase_64_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_compensation_reviewer(reviewer: &str) -> bool {
    !reviewer.is_empty()
        && reviewer == reviewer.trim()
        && reviewer.chars().count() <= 40
        && !reviewer.chars().any(char::is_control)
}

fn canonical_work_id_has_writer_shape(work_id: &str, reviewer: &str, duration_ms: i64) -> bool {
    let reviewer_key = reviewer.trim().to_lowercase();
    let prefix = format!("reviewer-work-v1:{}:{reviewer_key}:audio-segment-v1:", reviewer_key.len());
    let Some(audio_identity) = work_id.strip_prefix(&prefix) else {
        return false;
    };
    let mut parts = audio_identity.split(':');
    let (Some(content_hash), Some(start), Some(end), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(start), Ok(end)) = (start.parse::<i64>(), end.parse::<i64>()) else {
        return false;
    };
    is_canonical_lowercase_64_hex(content_hash) && crate::db::source_span_matches_duration(start, end, duration_ms)
}

fn canonical_work_audio_identity<'a>(work_id: &'a str, reviewer: &str) -> Option<(&'a str, i64, i64)> {
    let reviewer_key = reviewer.trim().to_lowercase();
    let prefix = format!("reviewer-work-v1:{}:{reviewer_key}:audio-segment-v1:", reviewer_key.len());
    let audio_identity = work_id.strip_prefix(&prefix)?;
    let mut parts = audio_identity.split(':');
    let content_hash = parts.next()?;
    let start = parts.next()?.parse::<i64>().ok()?;
    let end = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || !is_canonical_lowercase_64_hex(content_hash) || start < 0 || end <= start {
        return None;
    }
    Some((content_hash, start, end))
}

/// Reproduce `Database::compensation_audio_identity_tx` and the reviewer namespace byte for byte.
/// Restore validation cannot trust a ledger's self-declared work id: a forged target could otherwise
/// split one clip into several invented work ids and earn the full rate on every split.
fn canonical_compensation_work(
    db: &crate::db::Database,
    segment_id: &str,
    reviewer: &str,
    decision_revision: i64,
) -> Result<Option<(String, i64)>, String> {
    use rusqlite::OptionalExtension;

    if !valid_compensation_reviewer(reviewer) {
        return Err("database restore refused: compensation row has an invalid reviewer identity".to_string());
    }
    let row: Option<(Option<String>, Option<String>, i64, i64)> = db
        .connection()
        .query_row(
            "SELECT audio_content_hash, alignment_json, duration_ms, COALESCE(review_revision, 0)
               FROM speech_segments WHERE id = ?1",
            [segment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| format!("restore target compensation segment identity is unreadable: {error}"))?;
    let Some((content_hash, alignment_json, duration_ms, current_revision)) = row else {
        return Ok(None);
    };
    if current_revision < decision_revision || current_revision < 0 {
        return Err(format!(
            "database restore refused: compensation segment {segment_id} regresses its decision revision"
        ));
    }
    if current_revision != decision_revision {
        return Ok(None);
    }
    if duration_ms <= 0 {
        return Err(format!(
            "database restore refused: current compensation segment {segment_id} has invalid duration"
        ));
    }
    let content_hash = content_hash.as_deref().map(str::trim).filter(|value| !value.is_empty()).ok_or_else(|| {
        format!("database restore refused: compensation segment {segment_id} has fallback audio identity")
    })?;
    let alignment_json = alignment_json.as_deref().ok_or_else(|| {
        format!("database restore refused: compensation segment {segment_id} has no source-span identity")
    })?;
    let alignment: serde_json::Value = serde_json::from_str(alignment_json).map_err(|_| {
        format!("database restore refused: compensation segment {segment_id} has invalid source-span identity")
    })?;
    let start = alignment.get("source_start_ms").and_then(serde_json::Value::as_i64);
    let end = alignment.get("source_end_ms").and_then(serde_json::Value::as_i64);
    let (Some(start), Some(end)) = (start, end) else {
        return Err(format!(
            "database restore refused: compensation segment {segment_id} has incomplete source-span identity"
        ));
    };
    if !crate::db::source_span_matches_duration(start, end, duration_ms) {
        return Err(format!(
            "database restore refused: compensation segment {segment_id} source span disagrees with decoded duration"
        ));
    }
    let reviewer_key = reviewer.trim().to_lowercase();
    let audio_work_id = format!("audio-segment-v1:{content_hash}:{start}:{end}");
    Ok(Some((format!("reviewer-work-v1:{}:{reviewer_key}:{audio_work_id}", reviewer_key.len()), duration_ms)))
}

/// Re-derive the current compensation ledger and settlements from their immutable inputs. Schema
/// triggers protect future writes, but a restored database may contain pre-existing forged extras;
/// this read-only pass proves their complete arithmetic/identity semantics before page publication.
fn validate_review_compensation_semantics(db: &crate::db::Database) -> Result<(), String> {
    use rusqlite::OptionalExtension;

    #[derive(Clone)]
    struct Event {
        segment_id: String,
        reviewer: String,
        action: String,
        compensation_action: Option<String>,
        source: String,
        duration_ms: Option<i64>,
        operation_id: Option<String>,
        operation_payload_hash: Option<String>,
        requested_action: Option<String>,
        requested_transcript: Option<String>,
        served_transcript: Option<String>,
        served_revision: Option<i64>,
    }

    #[derive(Clone)]
    struct Ledger {
        id: i64,
        entry_id: String,
        entry_key: String,
        review_event_id: Option<i64>,
        canonical_work_id: String,
        canonical_identity_kind: String,
        reviewer: String,
        segment_id: String,
        source: String,
        compensation_action: String,
        effective_decision: String,
        decision_revision: Option<i64>,
        duration_ms: i64,
        rate_basis_points: i64,
        entitlement_micro_iqd: i64,
        delta_micro_iqd: i64,
        corrected_entitlement_ms: i64,
        delta_corrected_ms: i64,
        reverses_entry_id: Option<String>,
    }

    let mut policy_statement = db
        .connection()
        .prepare(
            "SELECT policy_version, effective_after_event_id, base_rate_micro_iqd_per_hour,
                    edit_basis_points, accept_basis_points, reject_basis_points, skip_basis_points
               FROM review_compensation_policies ORDER BY policy_version",
        )
        .map_err(|error| format!("restore target compensation policy is unreadable: {error}"))?;
    let policies = policy_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|error| format!("restore target compensation policy is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target compensation policy is unreadable: {error}"))?;
    drop(policy_statement);
    if policies.len() != 1 || policies[0].0 != crate::db::REVIEW_PAY_POLICY_VERSION {
        return Err(format!(
            "database restore refused: target must contain only the exact {} compensation policy row",
            crate::db::REVIEW_PAY_POLICY_VERSION
        ));
    }
    let policy = &policies[0];
    if (policy.2, policy.3, policy.4, policy.5, policy.6)
        != (
            crate::db::REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR,
            crate::db::REVIEW_PAY_EDIT_BPS,
            crate::db::REVIEW_PAY_ACCEPT_BPS,
            crate::db::REVIEW_PAY_REJECT_BPS,
            crate::db::REVIEW_PAY_SKIP_BPS,
        )
    {
        return Err(
            "database restore refused: target compensation policy constants differ from this binary".to_string()
        );
    }
    let cutoff = policy.1;
    let effect_event_frontier: i64 = db
        .connection()
        .query_row(
            "SELECT effective_after_review_event_id FROM review_effect_state WHERE singleton_key = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("restore target review-effect frontier is unreadable: {error}"))?;
    let maximum_event_id: i64 = db
        .connection()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("restore target compensation cutoff cannot be verified: {error}"))?;
    if cutoff < 0 || cutoff > maximum_event_id {
        return Err(format!(
            "database restore refused: target compensation cutoff {cutoff} is outside review history 0..={maximum_event_id}"
        ));
    }

    let mut event_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, reviewer, action, compensation_action, source, duration_ms,
                    operation_id, operation_payload_hash, requested_action,
                    requested_transcript, served_transcript, served_revision
               FROM review_events WHERE id > ?1 ORDER BY id",
        )
        .map_err(|error| format!("restore target prospective compensation events are unreadable: {error}"))?;
    let event_rows = event_statement
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                Event {
                    segment_id: row.get(1)?,
                    reviewer: row.get(2)?,
                    action: row.get(3)?,
                    compensation_action: row.get(4)?,
                    source: row.get(5)?,
                    duration_ms: row.get(6)?,
                    operation_id: row.get(7)?,
                    operation_payload_hash: row.get(8)?,
                    requested_action: row.get(9)?,
                    requested_transcript: row.get(10)?,
                    served_transcript: row.get(11)?,
                    served_revision: row.get(12)?,
                },
            ))
        })
        .map_err(|error| format!("restore target prospective compensation events are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target prospective compensation events are unreadable: {error}"))?;
    drop(event_statement);
    let events = event_rows.into_iter().collect::<std::collections::HashMap<_, _>>();

    let mut operation_ids = std::collections::HashSet::<String>::new();
    for (event_id, event) in &events {
        if !valid_compensation_reviewer(&event.reviewer)
            || !matches!(event.source.as_str(), "couch" | "couch_spot_check")
            || !matches!(event.action.as_str(), "accept" | "edit" | "reject" | "skip")
        {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} is not a valid production Couch action"
            ));
        }
        let compensation_action = event.compensation_action.as_deref().ok_or_else(|| {
            format!("database restore refused: post-cutoff review event {event_id} has no compensation action")
        })?;
        let requested_action = event.requested_action.as_deref().unwrap_or_default();
        let requested_transcript = event.requested_transcript.as_deref().unwrap_or_default();
        let served_transcript = event.served_transcript.as_deref().unwrap_or_default();
        let served_revision = event.served_revision.unwrap_or(-1);
        let expected_compensation = match requested_action {
            "skip" => Some("skip"),
            "bad" | "reject" => Some("reject"),
            "accept" | "edit" => Some(
                if crate::normalizer::learning_text_key(requested_transcript)
                    == crate::normalizer::learning_text_key(served_transcript)
                {
                    "accept"
                } else {
                    "edit"
                },
            ),
            _ => None,
        };
        let valid_action_pair = if event.source == "couch_spot_check" {
            compensation_action == event.action && expected_compensation == Some(compensation_action)
        } else {
            match event.action.as_str() {
                "skip" => compensation_action == "skip" && expected_compensation == Some("skip"),
                "reject" => compensation_action == "reject" && expected_compensation == Some("reject"),
                // Corpus provenance may reclassify an unchanged earlier human correction as edit
                // while pay remains an accept, or an alternate ASR hypothesis as accept while pay
                // remains an edit. Both are deliberate writer outcomes; no other cross-pair is.
                "accept" | "edit" => {
                    matches!(compensation_action, "accept" | "edit")
                        && expected_compensation == Some(compensation_action)
                }
                _ => false,
            }
        };
        if review_action_basis_points(compensation_action).is_none() || !valid_action_pair {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} has invalid action/pay semantics"
            ));
        }
        if requested_transcript != crate::db::to_nfc(requested_transcript.trim())
            || served_transcript.is_empty()
            || served_transcript != crate::db::to_nfc(served_transcript.trim())
            || served_revision < 0
        {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} has invalid served/request evidence"
            ));
        }
        let operation_id = event.operation_id.as_deref().unwrap_or_default();
        if !is_canonical_lowercase_uuid(operation_id) || !operation_ids.insert(operation_id.to_string()) {
            return Err(format!(
                "database restore refused: post-cutoff Couch event {event_id} lacks a unique canonical lowercase UUID"
            ));
        }
        let operation_hash = event.operation_payload_hash.as_deref().unwrap_or_default();
        if !is_canonical_lowercase_64_hex(operation_hash) {
            return Err(format!(
                "database restore refused: post-cutoff Couch event {event_id} lacks a canonical payload hash"
            ));
        }
        if !event.duration_ms.is_some_and(|duration| duration > 0) {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} has no valid durable duration"
            ));
        }
        if event.source == "couch_spot_check" {
            let exact_results: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM spot_checks
                      WHERE segment_id = ?1 AND reviewer = ?2 COLLATE NOCASE AND action = ?3",
                    rusqlite::params![event.segment_id, event.reviewer, event.action],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target spot-check compensation evidence is unreadable: {error}"))?;
            if exact_results != 1 {
                return Err(format!(
                    "database restore refused: hidden review event {event_id} lacks its exact immutable spot-check result"
                ));
            }
        }
    }

    let mut ledger_statement = db
        .connection()
        .prepare(
            "SELECT id, entry_id, entry_key, review_event_id, canonical_work_id, canonical_identity_kind,
                    reviewer, segment_id, source, compensation_action, effective_decision,
                    decision_revision, duration_ms, rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                    corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id
               FROM review_compensation_ledger
              WHERE policy_version = ?1 ORDER BY id",
        )
        .map_err(|error| format!("restore target compensation ledger is unreadable: {error}"))?;
    let ledger_rows = ledger_statement
        .query_map([crate::db::REVIEW_PAY_POLICY_VERSION], |row| {
            Ok(Ledger {
                id: row.get(0)?,
                entry_id: row.get(1)?,
                entry_key: row.get(2)?,
                review_event_id: row.get(3)?,
                canonical_work_id: row.get(4)?,
                canonical_identity_kind: row.get(5)?,
                reviewer: row.get(6)?,
                segment_id: row.get(7)?,
                source: row.get(8)?,
                compensation_action: row.get(9)?,
                effective_decision: row.get(10)?,
                decision_revision: row.get(11)?,
                duration_ms: row.get(12)?,
                rate_basis_points: row.get(13)?,
                entitlement_micro_iqd: row.get(14)?,
                delta_micro_iqd: row.get(15)?,
                corrected_entitlement_ms: row.get(16)?,
                delta_corrected_ms: row.get(17)?,
                reverses_entry_id: row.get(18)?,
            })
        })
        .map_err(|error| format!("restore target compensation ledger is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target compensation ledger is unreadable: {error}"))?;
    drop(ledger_statement);

    let mut event_entry_counts = std::collections::HashMap::<i64, usize>::new();
    let mut entries = std::collections::HashMap::<String, Ledger>::new();
    let mut entry_keys = std::collections::HashSet::<String>::new();
    let mut reversed_entries = std::collections::HashSet::<String>::new();
    let mut balances = std::collections::HashMap::<String, i64>::new();
    let mut corrected_balances = std::collections::HashMap::<String, i64>::new();
    for ledger in &ledger_rows {
        if ledger.id <= 0
            || !is_canonical_lowercase_uuid(&ledger.entry_id)
            || !valid_compensation_reviewer(&ledger.reviewer)
            || !entry_keys.insert(ledger.entry_key.clone())
        {
            return Err(format!(
                "database restore refused: compensation ledger entry {} has invalid or duplicate durable identity",
                ledger.entry_id
            ));
        }
        let decision_revision = ledger.decision_revision.ok_or_else(|| {
            format!("database restore refused: compensation ledger entry {} has no decision revision", ledger.entry_id)
        })?;
        if ledger.canonical_identity_kind != "audio_content_hash+source_span"
            || !canonical_work_id_has_writer_shape(&ledger.canonical_work_id, &ledger.reviewer, ledger.duration_ms)
            || ledger.duration_ms <= 0
            || decision_revision < 0
        {
            return Err(format!(
                "database restore refused: compensation ledger entry {} disagrees with canonical segment/work identity",
                ledger.entry_id
            ));
        }
        if let Some((expected_work_id, segment_duration)) =
            canonical_compensation_work(db, &ledger.segment_id, &ledger.reviewer, decision_revision)?
        {
            if ledger.canonical_work_id != expected_work_id || ledger.duration_ms != segment_duration {
                return Err(format!(
                    "database restore refused: current compensation ledger entry {} disagrees with its segment identity",
                    ledger.entry_id
                ));
            }
        }
        let prior = *balances.get(&ledger.canonical_work_id).unwrap_or(&0);
        let prior_corrected = *corrected_balances.get(&ledger.canonical_work_id).unwrap_or(&0);

        if let Some(event_id) = ledger.review_event_id {
            *event_entry_counts.entry(event_id).or_default() += 1;
            let event = events.get(&event_id).ok_or_else(|| {
                format!(
                    "database restore refused: compensation ledger entry {} points outside the post-cutoff event range",
                    ledger.entry_id
                )
            })?;
            let expected_action = event
                .compensation_action
                .as_deref()
                .ok_or_else(|| format!("database restore refused: event {event_id} has no compensation action"))?;
            let event_duration = event
                .duration_ms
                .ok_or_else(|| format!("database restore refused: event {event_id} has no durable duration"))?;
            if ledger.compensation_action != expected_action
                || ledger.effective_decision != event.action
                || ledger.segment_id != event.segment_id
                || ledger.reviewer.trim().to_lowercase() != event.reviewer.trim().to_lowercase()
                || ledger.source != event.source
                || ledger.duration_ms != event_duration
                || ledger.entry_key != format!("review-event:{event_id}")
                || ledger.reverses_entry_id.is_some()
                || (event.source == "couch" && event.action != "skip" && decision_revision == 0)
                || ((event.source == "couch_spot_check" || event.action == "skip")
                    && event.served_revision != Some(decision_revision))
            {
                return Err(format!(
                    "database restore refused: compensation ledger entry {} disagrees with review event {event_id}",
                    ledger.entry_id
                ));
            }
            if event.action != "skip" && event_id > effect_event_frontier {
                let receipt_revision = event
                    .served_revision
                    .ok_or_else(|| format!("database restore refused: paid event {event_id} has no served revision"))?;
                if event.source == "couch" {
                    let (effect_count, prior_revision): (i64, Option<i64>) = db
                        .connection()
                        .query_row(
                            "SELECT COUNT(*), MIN(prior_revision)
                               FROM human_decision_effect_events
                              WHERE review_event_id = ?1 AND decision_revision = ?2",
                            rusqlite::params![event_id, decision_revision],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .map_err(|error| format!("restore target paid playback revision is unreadable: {error}"))?;
                    if effect_count != 1 {
                        return Err(format!(
                            "database restore refused: paid corpus event {event_id} has no unique decision effect for playback binding"
                        ));
                    }
                    let effect_prior_revision = prior_revision.ok_or_else(|| {
                        format!("database restore refused: paid corpus event {event_id} has no receipt revision")
                    })?;
                    if effect_prior_revision != receipt_revision {
                        return Err(format!(
                            "database restore refused: paid corpus event {event_id} served revision disagrees with its decision effect"
                        ));
                    }
                }
                let (content_hash, source_start_ms, source_end_ms) = canonical_work_audio_identity(
                    &ledger.canonical_work_id,
                    &ledger.reviewer,
                )
                .ok_or_else(|| {
                    format!(
                        "database restore refused: paid event {event_id} has no canonical content-hash/source-span work identity"
                    )
                })?;
                let retained_identity: Option<(Option<String>, i64, i64, Option<String>)> = db
                    .connection()
                    .query_row(
                        "SELECT audio_content_hash, duration_ms, COALESCE(review_revision, 0), alignment_json
                           FROM speech_segments WHERE id = ?1",
                        [&ledger.segment_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()
                    .map_err(|error| format!("restore target paid segment identity is unreadable: {error}"))?;
                let Some((retained_hash, retained_duration, retained_revision, retained_alignment)) = retained_identity
                else {
                    return Err(format!(
                        "database restore refused: post-v60 paid segment {} is missing; policy-3 evidence forbids reviewed-segment deletion",
                        ledger.segment_id
                    ));
                };
                let retained_span = crate::db::canonical_source_span(retained_alignment.as_deref());
                if retained_hash.as_deref() != Some(content_hash)
                    || retained_span != Some((source_start_ms, source_end_ms))
                    || retained_duration != ledger.duration_ms
                    || retained_revision < decision_revision
                {
                    return Err(format!(
                        "database restore refused: paid review event {event_id} disagrees with its retained BLAKE3/source-span/duration identity"
                    ));
                }
                let sufficient_receipts: i64 = db
                    .connection()
                    .query_row(
                        "SELECT COUNT(*)
                           FROM playback_receipts receipt
                          WHERE receipt.segment_id = ?1
                            AND receipt.reviewer = ?2 COLLATE NOCASE
                            AND receipt.segment_revision = ?3
                            AND receipt.audio_fingerprint = ?4
                            AND receipt.source_start_ms = ?5
                            AND receipt.source_end_ms = ?6
                            AND receipt.clip_duration_ms = ?7
                            AND receipt.policy_version = ?8
                            AND receipt.started_at_ms >= 0
                            AND receipt.played_ms >= 0
                            AND receipt.coverage_ratio >= ?9",
                        rusqlite::params![
                            ledger.segment_id,
                            ledger.reviewer,
                            receipt_revision,
                            content_hash,
                            source_start_ms,
                            source_end_ms,
                            ledger.duration_ms,
                            crate::db::PLAYBACK_POLICY_VERSION,
                            crate::db::MIN_PLAYBACK_COVERAGE,
                        ],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("restore target paid playback evidence is unreadable: {error}"))?;
                if sufficient_receipts == 0 {
                    return Err(format!(
                        "database restore refused: paid review event {event_id} has no exact sufficient policy-3 playback receipt"
                    ));
                }
            }
            let expected_bps = review_action_basis_points(expected_action).ok_or_else(|| {
                format!("database restore refused: ledger entry {} has an unsupported action", ledger.entry_id)
            })?;
            let entitlement = if expected_bps == 0 {
                if ledger.duration_ms <= 0 {
                    return Err(format!(
                        "database restore refused: ledger entry {} has non-positive duration",
                        ledger.entry_id
                    ));
                }
                0
            } else {
                exact_review_entitlement(ledger.duration_ms, expected_bps)?
            };
            let expected_delta = if expected_action == "skip" {
                0
            } else {
                entitlement.checked_sub(prior).ok_or_else(|| "review compensation delta overflow".to_string())?
            };
            let corrected_target = match expected_action {
                "edit" => ledger.duration_ms,
                "skip" => prior_corrected,
                "accept" | "reject" => 0,
                _ => return Err(format!("database restore refused: unsupported ledger action {expected_action}")),
            };
            let expected_corrected_delta = corrected_target
                .checked_sub(prior_corrected)
                .ok_or_else(|| "review corrected-entitlement delta overflow".to_string())?;
            if ledger.rate_basis_points != expected_bps
                || ledger.entitlement_micro_iqd != entitlement
                || ledger.delta_micro_iqd != expected_delta
                || ledger.corrected_entitlement_ms != corrected_target
                || ledger.delta_corrected_ms != expected_corrected_delta
            {
                return Err(format!(
                    "database restore refused: compensation rate/delta/corrected math is invalid at {}",
                    ledger.entry_id
                ));
            }
        } else if ledger.compensation_action == "undo" {
            if ledger.effective_decision != "undo"
                || ledger.source != "couch_undo"
                || ledger.rate_basis_points != 0
                || ledger.entitlement_micro_iqd != 0
            {
                return Err(format!(
                    "database restore refused: undo ledger entry {} has invalid fixed semantics",
                    ledger.entry_id
                ));
            }
            let reversed_id = ledger.reverses_entry_id.as_deref().ok_or_else(|| {
                format!("database restore refused: undo {} does not name an earlier entry", ledger.entry_id)
            })?;
            let reversed = entries.get(reversed_id).ok_or_else(|| {
                format!("database restore refused: undo {} references a missing or later entry", ledger.entry_id)
            })?;
            let latest_eligible = entries
                .values()
                .filter(|entry| {
                    entry.review_event_id.is_some()
                        && entry.compensation_action != "undo"
                        && entry.canonical_work_id == ledger.canonical_work_id
                        && entry.reviewer.trim().eq_ignore_ascii_case(ledger.reviewer.trim())
                        && !reversed_entries.contains(&entry.entry_id)
                })
                .max_by_key(|entry| entry.id)
                .map(|entry| entry.entry_id.as_str());
            if reversed.compensation_action == "undo"
                || reversed.canonical_work_id != ledger.canonical_work_id
                || reversed.segment_id != ledger.segment_id
                || reversed.reviewer.trim().to_lowercase() != ledger.reviewer.trim().to_lowercase()
                || reversed.duration_ms != ledger.duration_ms
                || reversed.decision_revision != ledger.decision_revision
                || latest_eligible != Some(reversed_id)
                || !reversed_entries.insert(reversed_id.to_string())
            {
                return Err(format!(
                    "database restore refused: undo {} does not exactly bind its earlier decision entry",
                    ledger.entry_id
                ));
            }
            let reversed_event_id = reversed.review_event_id.ok_or_else(|| {
                format!("database restore refused: undo {} does not reverse a production decision", ledger.entry_id)
            })?;
            let reversed_event = events.get(&reversed_event_id).ok_or_else(|| {
                format!("database restore refused: undo {} reverses an unknown event", ledger.entry_id)
            })?;
            let undo_operation = ledger.entry_key.strip_prefix("undo:").unwrap_or_default();
            if reversed.source != "couch"
                || reversed.effective_decision == "skip"
                || !is_canonical_lowercase_uuid(undo_operation)
                || reversed_event.operation_id.as_deref() != Some(undo_operation)
            {
                return Err(format!(
                    "database restore refused: undo {} has invalid operation/event linkage",
                    ledger.entry_id
                ));
            }
            let expected_delta = reversed
                .delta_micro_iqd
                .checked_neg()
                .ok_or_else(|| "review compensation undo overflow".to_string())?;
            let expected_corrected_delta = reversed
                .delta_corrected_ms
                .checked_neg()
                .ok_or_else(|| "review corrected-entitlement undo overflow".to_string())?;
            let expected_corrected_entitlement = prior_corrected
                .checked_add(expected_corrected_delta)
                .ok_or_else(|| "review corrected-entitlement undo balance overflow".to_string())?;
            if ledger.delta_micro_iqd != expected_delta
                || ledger.delta_corrected_ms != expected_corrected_delta
                || ledger.corrected_entitlement_ms != expected_corrected_entitlement
            {
                return Err(format!(
                    "database restore refused: undo compensation math is invalid at {}",
                    ledger.entry_id
                ));
            }
        } else {
            return Err(format!(
                "database restore refused: ledger entry {} has neither event nor undo semantics",
                ledger.entry_id
            ));
        }

        let balance = prior
            .checked_add(ledger.delta_micro_iqd)
            .ok_or_else(|| "review compensation running balance overflow".to_string())?;
        let corrected_balance = prior_corrected
            .checked_add(ledger.delta_corrected_ms)
            .ok_or_else(|| "review corrected-entitlement running balance overflow".to_string())?;
        if balance < 0 || corrected_balance < 0 {
            return Err(format!(
                "database restore refused: compensation entry {} creates a negative running balance",
                ledger.entry_id
            ));
        }
        balances.insert(ledger.canonical_work_id.clone(), balance);
        corrected_balances.insert(ledger.canonical_work_id.clone(), corrected_balance);
        if entries.insert(ledger.entry_id.clone(), ledger.clone()).is_some() {
            return Err("database restore refused: compensation ledger has duplicate entry identity".to_string());
        }
    }
    for event_id in events.keys() {
        if event_entry_counts.get(event_id).copied().unwrap_or(0) != 1 {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} does not have exactly one current-policy ledger entry"
            ));
        }
    }

    let mut settlement_statement = db
        .connection()
        .prepare(
            "SELECT settlement_id, reviewer, from_ledger_id_exclusive,
                    through_ledger_id_inclusive, allocated_micro_iqd, payout_reference
               FROM review_compensation_settlements
              WHERE policy_version = ?1
              ORDER BY reviewer COLLATE NOCASE, through_ledger_id_inclusive",
        )
        .map_err(|error| format!("restore target compensation settlements are unreadable: {error}"))?;
    let settlements = settlement_statement
        .query_map([crate::db::REVIEW_PAY_POLICY_VERSION], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|error| format!("restore target compensation settlements are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target compensation settlements are unreadable: {error}"))?;
    drop(settlement_statement);
    let maximum_ledger_id = ledger_rows.last().map(|row| row.id).unwrap_or(0);
    let mut boundaries = std::collections::HashMap::<String, i64>::new();
    let mut payout_references = std::collections::HashSet::<String>::new();
    for (settlement_id, reviewer, from, through, amount, payout_reference) in settlements {
        let reviewer_key = reviewer.trim().to_lowercase();
        let expected_from = boundaries.get(&reviewer_key).copied().unwrap_or(0);
        if !is_canonical_lowercase_uuid(&settlement_id)
            || !valid_compensation_reviewer(&reviewer)
            || from != expected_from
            || through <= from
            || through > maximum_ledger_id
        {
            return Err(format!(
                "database restore refused: settlement {settlement_id} has a non-contiguous or invalid ledger range"
            ));
        }
        let mut exact_amount = 0i64;
        let mut matching_rows = 0usize;
        for ledger in &ledger_rows {
            if ledger.reviewer.trim().to_lowercase() == reviewer_key && ledger.id > from && ledger.id <= through {
                exact_amount = exact_amount
                    .checked_add(ledger.delta_micro_iqd)
                    .ok_or_else(|| "review settlement amount overflow".to_string())?;
                matching_rows += 1;
            }
        }
        if matching_rows == 0 || exact_amount != amount {
            return Err(format!(
                "database restore refused: settlement {settlement_id} amount differs from its immutable ledger range"
            ));
        }
        let reference = payout_reference.trim().to_string();
        if reference.is_empty() || reference != payout_reference || !payout_references.insert(reference) {
            return Err(format!(
                "database restore refused: settlement {settlement_id} has an empty or duplicate payout reference"
            ));
        }
        boundaries.insert(reviewer_key, through);
    }
    Ok(())
}

/// Re-derive schema-v60 review-effect meaning from immutable authorities.  Triggers constrain new
/// writes, but a restored file may already contain rows created with triggers disabled; this pass
/// therefore cross-checks the complete event/effect/inverse graph before the database is published.
fn validate_review_effect_semantics(db: &crate::db::Database) -> Result<(), String> {
    use rusqlite::OptionalExtension;

    fn optional_text_is_blank(value: Option<&str>) -> bool {
        match value {
            Some(value) => value.trim().is_empty(),
            None => true,
        }
    }

    #[derive(Clone)]
    struct DecisionEffect {
        id: i64,
        review_event_id: Option<i64>,
        segment_id: String,
        reviewer: Option<String>,
        source: String,
        operation_id: Option<String>,
        operation_payload_hash: Option<String>,
        action: String,
        served_transcript: String,
        decision_transcript: Option<String>,
        decision_annotated_transcript: Option<String>,
        decision_verified: i64,
        decision_corrected_at: String,
        decision_rationale: Option<String>,
        requested_action: Option<String>,
        requested_transcript: Option<String>,
        requested_timestamp_ms: Option<i64>,
        prior_revision: i64,
        decision_revision: i64,
        prior_verified: i64,
        prior_annotated_transcript: Option<String>,
        prior_verdict: Option<String>,
        prior_verdict_transcript: Option<String>,
        prior_rationale: Option<String>,
        prior_escalated: i64,
        prior_human_decision: Option<String>,
        prior_corrected_at: Option<String>,
        prior_reviewed_by: Option<String>,
        reversal_operation: Option<String>,
    }

    #[derive(Clone)]
    struct FlagEffect {
        id: i64,
        operation_id: String,
        segment_id: String,
        prior_revision: i64,
        flag_revision: i64,
        prior_verdict: Option<String>,
        prior_rationale: Option<String>,
        flag_rationale: String,
        prior_escalated: i64,
        reversal_operation: Option<String>,
    }

    #[derive(Clone)]
    struct PostV60Event {
        id: i64,
        segment_id: String,
        reviewer: String,
        action: String,
        compensation_action: String,
        source: String,
        operation_id: String,
        operation_payload_hash: String,
        requested_action: String,
        requested_transcript: String,
        served_transcript: String,
        served_revision: i64,
    }

    #[derive(Clone)]
    enum ReviewMutation {
        Decision(Box<DecisionEffect>),
        Flag(FlagEffect),
    }

    #[derive(PartialEq, Eq)]
    struct DecisionOwnedState {
        verified: i64,
        annotated_transcript: Option<String>,
        verdict: Option<String>,
        verdict_transcript: Option<String>,
        escalated: i64,
        human_decision: Option<String>,
        corrected_at: Option<String>,
        reviewed_by: Option<String>,
    }

    #[derive(PartialEq, Eq)]
    struct FlagOwnedState {
        verdict: Option<String>,
        rationale: Option<String>,
        escalated: i64,
    }

    #[derive(Clone, PartialEq, Eq)]
    struct StableHumanState {
        verified: i64,
        annotated_transcript: Option<String>,
        verdict_transcript: Option<String>,
        human_decision: Option<String>,
        corrected_at: Option<String>,
        reviewed_by: Option<String>,
    }

    #[derive(Clone)]
    struct LegacyReviewedState {
        review_revision: i64,
        human_decision: Option<String>,
        verdict: Option<String>,
        verdict_transcript: Option<String>,
        annotated_transcript: Option<String>,
        verified: i64,
        reviewed_by: Option<String>,
        corrected_at: Option<String>,
        escalated: i64,
        is_gold: i64,
        rationale: Option<String>,
    }

    fn decision_terminal_state(effect: &DecisionEffect) -> DecisionOwnedState {
        if effect.reversal_operation.is_some() {
            DecisionOwnedState {
                verified: effect.prior_verified,
                annotated_transcript: effect.prior_annotated_transcript.clone(),
                verdict: effect.prior_verdict.clone(),
                verdict_transcript: effect.prior_verdict_transcript.clone(),
                escalated: effect.prior_escalated,
                human_decision: effect.prior_human_decision.clone(),
                corrected_at: effect.prior_corrected_at.clone(),
                reviewed_by: effect.prior_reviewed_by.clone(),
            }
        } else {
            DecisionOwnedState {
                verified: effect.decision_verified,
                annotated_transcript: effect.decision_annotated_transcript.clone(),
                verdict: Some(format!("human_{}", effect.action)),
                verdict_transcript: if effect.action == "reject" {
                    effect.prior_verdict_transcript.clone()
                } else {
                    effect.decision_transcript.clone()
                },
                escalated: 0,
                human_decision: Some(effect.action.clone()),
                corrected_at: Some(effect.decision_corrected_at.clone()),
                reviewed_by: effect.reviewer.clone(),
            }
        }
    }

    fn flag_terminal_state(effect: &FlagEffect) -> FlagOwnedState {
        if effect.reversal_operation.is_some() {
            FlagOwnedState {
                verdict: effect.prior_verdict.clone(),
                rationale: effect.prior_rationale.clone(),
                escalated: effect.prior_escalated,
            }
        } else {
            FlagOwnedState {
                verdict: Some("escalated".to_string()),
                rationale: Some(effect.flag_rationale.clone()),
                escalated: 1,
            }
        }
    }

    fn decision_prior_stable_state(effect: &DecisionEffect) -> StableHumanState {
        StableHumanState {
            verified: effect.prior_verified,
            annotated_transcript: effect.prior_annotated_transcript.clone(),
            verdict_transcript: effect.prior_verdict_transcript.clone(),
            human_decision: effect.prior_human_decision.clone(),
            corrected_at: effect.prior_corrected_at.clone(),
            reviewed_by: effect.prior_reviewed_by.clone(),
        }
    }

    fn decision_terminal_stable_state(effect: &DecisionEffect) -> StableHumanState {
        let terminal = decision_terminal_state(effect);
        StableHumanState {
            verified: terminal.verified,
            annotated_transcript: terminal.annotated_transcript,
            verdict_transcript: terminal.verdict_transcript,
            human_decision: terminal.human_decision,
            corrected_at: terminal.corrected_at,
            reviewed_by: terminal.reviewed_by,
        }
    }

    type CurrentReviewState = (
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    impl ReviewMutation {
        fn segment_id(&self) -> &str {
            match self {
                Self::Decision(effect) => &effect.segment_id,
                Self::Flag(effect) => &effect.segment_id,
            }
        }

        fn prior_revision(&self) -> i64 {
            match self {
                Self::Decision(effect) => effect.prior_revision,
                Self::Flag(effect) => effect.prior_revision,
            }
        }

        fn applied_revision(&self) -> i64 {
            match self {
                Self::Decision(effect) => effect.decision_revision,
                Self::Flag(effect) => effect.flag_revision,
            }
        }

        fn terminal_revision(&self) -> i64 {
            self.applied_revision()
                + match self {
                    Self::Decision(effect) => i64::from(effect.reversal_operation.is_some()),
                    Self::Flag(effect) => i64::from(effect.reversal_operation.is_some()),
                }
        }
    }

    let mut state_statement = db
        .connection()
        .prepare(
            "SELECT singleton_key, effective_after_review_event_id,
                    effective_after_ledger_id, created_at
               FROM review_effect_state ORDER BY singleton_key",
        )
        .map_err(|error| format!("restore target review-effect frontier is unreadable: {error}"))?;
    let states = state_statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?))
        })
        .map_err(|error| format!("restore target review-effect frontier is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target review-effect frontier is unreadable: {error}"))?;
    drop(state_statement);
    if states.len() != 1 || states[0].0 != 1 || states[0].1 < 0 || states[0].2 < 0 || states[0].3.trim().is_empty() {
        return Err("database restore refused: review_effect_state is not the one canonical schema-v60 frontier row"
            .to_string());
    }
    let event_frontier = states[0].1;
    let ledger_frontier = states[0].2;
    let maximum_event_id: i64 = db
        .connection()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("restore target review-event frontier cannot be verified: {error}"))?;
    let maximum_ledger_id: i64 = db
        .connection()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_compensation_ledger", [], |row| row.get(0))
        .map_err(|error| format!("restore target review-ledger frontier cannot be verified: {error}"))?;
    if event_frontier > maximum_event_id || ledger_frontier > maximum_ledger_id {
        return Err(format!(
            "database restore refused: review-effect frontiers ({event_frontier}, {ledger_frontier}) exceed retained history ({maximum_event_id}, {maximum_ledger_id})"
        ));
    }

    let mut event_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, reviewer, action, compensation_action, source, app_git_sha,
                    playback_guard_version, operation_id, operation_payload_hash,
                    requested_action, requested_transcript, served_transcript, served_revision
               FROM review_events WHERE id > ?1 ORDER BY id",
        )
        .map_err(|error| format!("restore target post-v60 review events are unreadable: {error}"))?;
    let post_v60_events = event_statement
        .query_map([event_frontier], |row| {
            Ok((
                PostV60Event {
                    id: row.get(0)?,
                    segment_id: row.get(1)?,
                    reviewer: row.get(2)?,
                    action: row.get(3)?,
                    compensation_action: row.get(4)?,
                    source: row.get(5)?,
                    operation_id: row.get(8)?,
                    operation_payload_hash: row.get(9)?,
                    requested_action: row.get(10)?,
                    requested_transcript: row.get(11)?,
                    served_transcript: row.get(12)?,
                    served_revision: row.get(13)?,
                },
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|error| format!("restore target post-v60 review events are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target post-v60 review events are unreadable: {error}"))?;
    drop(event_statement);

    let mut post_v60_events_by_id = std::collections::HashMap::<i64, PostV60Event>::new();
    for (event, git_sha, playback_guard) in &post_v60_events {
        let git_sha = git_sha.as_deref().unwrap_or_default();
        let request_text_is_canonical =
            crate::db::to_nfc(event.requested_transcript.trim()) == event.requested_transcript;
        let served_text_is_canonical = !event.served_transcript.is_empty()
            && crate::db::to_nfc(event.served_transcript.trim()) == event.served_transcript;
        let expected_payload_hash = crate::db::review_operation_payload_hash(
            &event.segment_id,
            &event.requested_action,
            &event.requested_transcript,
            &event.reviewer,
        );
        let request_classification_is_valid = match event.requested_action.as_str() {
            "skip" => event.action == "skip" && event.compensation_action == "skip",
            "bad" | "reject" => event.action == "reject" && event.compensation_action == "reject",
            "accept" | "edit" => {
                let expected_compensation = if crate::normalizer::learning_text_key(&event.requested_transcript)
                    == crate::normalizer::learning_text_key(&event.served_transcript)
                {
                    "accept"
                } else {
                    "edit"
                };
                matches!(event.action.as_str(), "accept" | "edit") && event.compensation_action == expected_compensation
            }
            _ => false,
        };
        if !matches!(event.source.as_str(), "couch" | "couch_spot_check")
            || !matches!(event.action.as_str(), "accept" | "edit" | "reject" | "skip")
            || !matches!(event.requested_action.as_str(), "accept" | "edit" | "reject" | "bad" | "skip")
            || !is_canonical_lowercase_uuid(&event.operation_id)
            || !is_canonical_lowercase_64_hex(&event.operation_payload_hash)
            || event.operation_payload_hash != expected_payload_hash
            || !request_text_is_canonical
            || !served_text_is_canonical
            || event.served_revision < 0
            || !request_classification_is_valid
            || git_sha.len() != 40
            || !git_sha.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || playback_guard.as_deref() != Some("content-hash-raw-counter-v3")
        {
            return Err(format!(
                "database restore refused: post-v60 review event {} lacks canonical Couch/build/playback provenance",
                event.id
            ));
        }
        post_v60_events_by_id.insert(event.id, event.clone());
        let total_effects: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM human_decision_effect_events WHERE review_event_id = ?1",
                [event.id],
                |row| row.get(0),
            )
            .map_err(|error| format!("restore target decision-effect linkage is unreadable: {error}"))?;
        if event.source == "couch" && event.action != "skip" {
            let exact_effects: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM human_decision_effect_events effect
                       JOIN review_compensation_ledger ledger
                         ON ledger.review_event_id = ?1
                        AND ledger.reverses_entry_id IS NULL
                      WHERE effect.review_event_id = ?1
                        AND effect.segment_id = ?2
                        AND effect.reviewer = ?3
                        AND effect.source = 'couch'
                        AND effect.action = ?4
                        AND ledger.segment_id = effect.segment_id
                        AND ledger.reviewer = effect.reviewer
                        AND ledger.source = effect.source
                        AND ledger.effective_decision = effect.action
                        AND ledger.decision_revision IS effect.decision_revision",
                    rusqlite::params![event.id, event.segment_id, event.reviewer, event.action],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target decision-effect linkage is unreadable: {error}"))?;
            if total_effects != 1 || exact_effects != 1 {
                return Err(format!(
                    "database restore refused: post-v60 Couch decision event {} does not have exactly one matching human/pay effect",
                    event.id
                ));
            }
        } else if total_effects != 0 {
            return Err(format!(
                "database restore refused: post-v60 {}/{} event {} must not create a human-decision effect",
                event.source, event.action, event.id
            ));
        }
    }

    let mut effect_statement = db
        .connection()
        .prepare(
            "SELECT effect.id, effect.review_event_id, effect.segment_id, effect.reviewer,
                    effect.source, effect.operation_id, effect.operation_payload_hash,
                    effect.action, effect.served_transcript, effect.decision_transcript,
                    effect.decision_annotated_transcript, effect.decision_verified,
                    effect.decision_corrected_at, effect.decision_rationale, effect.requested_action,
                    effect.requested_transcript, effect.requested_timestamp_ms,
                    effect.prior_revision, effect.decision_revision, effect.prior_verified,
                    effect.prior_annotated_transcript, effect.prior_verdict,
                    effect.prior_verdict_transcript, effect.prior_rationale, effect.prior_escalated,
                    effect.prior_human_decision, effect.prior_corrected_at,
                    effect.prior_reviewed_by, reversal.operation_id
               FROM human_decision_effect_events effect
               LEFT JOIN human_decision_effect_reversals reversal
                 ON reversal.effect_event_id = effect.id
              ORDER BY effect.id",
        )
        .map_err(|error| format!("restore target human-decision effects are unreadable: {error}"))?;
    let effects = effect_statement
        .query_map([], |row| {
            Ok(DecisionEffect {
                id: row.get(0)?,
                review_event_id: row.get(1)?,
                segment_id: row.get(2)?,
                reviewer: row.get(3)?,
                source: row.get(4)?,
                operation_id: row.get(5)?,
                operation_payload_hash: row.get(6)?,
                action: row.get(7)?,
                served_transcript: row.get(8)?,
                decision_transcript: row.get(9)?,
                decision_annotated_transcript: row.get(10)?,
                decision_verified: row.get(11)?,
                decision_corrected_at: row.get(12)?,
                decision_rationale: row.get(13)?,
                requested_action: row.get(14)?,
                requested_transcript: row.get(15)?,
                requested_timestamp_ms: row.get(16)?,
                prior_revision: row.get(17)?,
                decision_revision: row.get(18)?,
                prior_verified: row.get(19)?,
                prior_annotated_transcript: row.get(20)?,
                prior_verdict: row.get(21)?,
                prior_verdict_transcript: row.get(22)?,
                prior_rationale: row.get(23)?,
                prior_escalated: row.get(24)?,
                prior_human_decision: row.get(25)?,
                prior_corrected_at: row.get(26)?,
                prior_reviewed_by: row.get(27)?,
                reversal_operation: row.get(28)?,
            })
        })
        .map_err(|error| format!("restore target human-decision effects are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target human-decision effects are unreadable: {error}"))?;
    drop(effect_statement);

    let effects_by_id = effects.iter().map(|effect| (effect.id, effect)).collect::<std::collections::HashMap<_, _>>();
    for effect in &effects {
        if effect.id <= 0
            || effect.segment_id.trim().is_empty()
            || effect.decision_revision != effect.prior_revision + 1
            || !matches!(effect.action.as_str(), "accept" | "edit" | "reject")
            || !matches!(effect.decision_verified, 0 | 1)
            || !matches!(effect.prior_verified, 0 | 1)
            || !matches!(effect.prior_escalated, 0 | 1)
            || effect.decision_corrected_at.trim().is_empty()
            || effect.decision_rationale != effect.prior_rationale
            || effect.served_transcript.is_empty()
            || crate::db::to_nfc(effect.served_transcript.trim()) != effect.served_transcript
        {
            return Err(format!(
                "database restore refused: human-decision effect {} violates its immutable identity/revision boundary",
                effect.id
            ));
        }
        let canonical_decision_text = effect
            .decision_transcript
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty() && crate::db::to_nfc(text.trim()) == text);
        if (matches!(effect.action.as_str(), "accept" | "edit")
            && (!canonical_decision_text || effect.decision_annotated_transcript != effect.decision_transcript))
            || (effect.action == "reject" && effect.decision_transcript.is_some())
        {
            return Err(format!(
                "database restore refused: human-decision effect {} has no exact canonical post-decision transcript",
                effect.id
            ));
        }
        if let Some(event_id) = effect.review_event_id {
            let Some(event) = post_v60_events_by_id.get(&event_id) else {
                return Err(format!(
                    "database restore refused: phone decision effect {} names no post-v60 review event",
                    effect.id
                ));
            };
            let exact_link: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM review_events event
                       JOIN review_compensation_ledger ledger
                         ON ledger.review_event_id = event.id
                        AND ledger.reverses_entry_id IS NULL
                      WHERE event.id = ?1 AND event.id > ?2
                        AND event.segment_id = ?3
                        AND event.reviewer = ?4
                        AND event.source = 'couch'
                        AND event.action = ?5
                        AND ledger.segment_id = ?3
                        AND ledger.reviewer = ?4
                        AND ledger.source = 'couch'
                        AND ledger.effective_decision = ?5
                        AND ledger.decision_revision IS ?6",
                    rusqlite::params![
                        event_id,
                        event_frontier,
                        effect.segment_id,
                        effect.reviewer,
                        effect.action,
                        effect.decision_revision,
                    ],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target phone-effect linkage is unreadable: {error}"))?;
            if effect.source != "couch"
                || optional_text_is_blank(effect.reviewer.as_deref())
                || effect.operation_id.is_some()
                || effect.operation_payload_hash.is_some()
                || effect.requested_action.is_some()
                || effect.requested_transcript.is_some()
                || effect.requested_timestamp_ms.is_some()
                || event.segment_id != effect.segment_id
                || event.reviewer.as_str() != effect.reviewer.as_deref().unwrap_or_default()
                || event.action != effect.action
                || event.served_transcript != effect.served_transcript
                || event.served_revision != effect.prior_revision
                || exact_link != 1
            {
                return Err(format!(
                    "database restore refused: phone decision effect {} is not the exact post-v60 event/pay effect",
                    effect.id
                ));
            }
        } else {
            let desktop_request_ok = match (
                effect.operation_id.as_deref(),
                effect.operation_payload_hash.as_deref(),
                effect.requested_action.as_deref(),
                effect.requested_timestamp_ms,
            ) {
                (Some(operation_id), Some(payload_hash), Some(requested_action), Some(timestamp_ms)) => {
                    is_canonical_lowercase_uuid(operation_id)
                        && is_canonical_lowercase_64_hex(payload_hash)
                        && matches!(requested_action, "accept" | "edit" | "reject")
                        && timestamp_ms > 0
                        && effect
                            .requested_transcript
                            .as_deref()
                            .map_or(true, |text| crate::db::to_nfc(text.trim()) == text && !text.is_empty())
                        && crate::db::desktop_decision_payload_hash(
                            &effect.segment_id,
                            requested_action,
                            effect.requested_transcript.as_deref(),
                            Some(timestamp_ms),
                        ) == payload_hash
                }
                _ => false,
            };
            if effect.source != "desktop" || effect.reviewer.is_some() || !desktop_request_ok {
                return Err(format!(
                    "database restore refused: unlinked human-decision effect {} is outside the exact anonymous desktop operation boundary",
                    effect.id
                ));
            }
        }

        let original_reversal_count: i64 = if let Some(event_id) = effect.review_event_id {
            db.connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM review_compensation_ledger original
                       JOIN review_compensation_ledger reversal
                         ON reversal.reverses_entry_id = original.entry_id
                      WHERE original.review_event_id = ?1
                        AND original.reverses_entry_id IS NULL",
                    [event_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target effect reversal linkage is unreadable: {error}"))?
        } else {
            0
        };
        if let Some(operation_id) = effect.reversal_operation.as_deref() {
            if !is_canonical_lowercase_uuid(operation_id) {
                return Err(format!(
                    "database restore refused: human-decision reversal {} has no canonical operation UUID",
                    effect.id
                ));
            }
            if let Some(event_id) = effect.review_event_id {
                let exact_inverse: i64 = db
                    .connection()
                    .query_row(
                        "SELECT COUNT(*)
                           FROM review_events event
                           JOIN review_compensation_ledger original
                             ON original.review_event_id = event.id
                            AND original.reverses_entry_id IS NULL
                           JOIN review_compensation_ledger reversal
                             ON reversal.reverses_entry_id = original.entry_id
                          WHERE event.id = ?1
                            AND event.operation_id = ?2
                            AND reversal.id > ?3
                            AND reversal.entry_key = 'undo:' || ?2
                            AND reversal.policy_version = original.policy_version
                            AND reversal.canonical_work_id = original.canonical_work_id
                            AND reversal.canonical_identity_kind = original.canonical_identity_kind
                            AND reversal.reviewer = original.reviewer
                            AND reversal.segment_id = original.segment_id
                            AND reversal.source = 'couch_undo'
                            AND reversal.compensation_action = 'undo'
                            AND reversal.effective_decision = 'undo'
                            AND reversal.decision_revision IS original.decision_revision
                            AND reversal.duration_ms = original.duration_ms
                            AND reversal.rate_basis_points = 0
                            AND reversal.entitlement_micro_iqd = 0
                            AND reversal.delta_micro_iqd = -original.delta_micro_iqd
                            AND reversal.delta_corrected_ms = -original.delta_corrected_ms",
                        rusqlite::params![event_id, operation_id, ledger_frontier],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("restore target effect reversal linkage is unreadable: {error}"))?;
                if original_reversal_count != 1 || exact_inverse != 1 {
                    return Err(format!(
                        "database restore refused: phone decision reversal {} lacks its exact operation-bound compensation inverse",
                        effect.id
                    ));
                }
            } else {
                let conflicting_pay_inverse: i64 = db
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM review_compensation_ledger
                          WHERE entry_key = 'undo:' || ?1",
                        [operation_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("restore target desktop reversal identity is unreadable: {error}"))?;
                if conflicting_pay_inverse != 0 {
                    return Err(format!(
                        "database restore refused: desktop decision reversal {} reuses a paid-review inverse identity",
                        effect.id
                    ));
                }
            }
        } else if original_reversal_count != 0 {
            return Err(format!(
                "database restore refused: active phone decision effect {} already has a compensation inverse",
                effect.id
            ));
        }
    }

    let mut reversal_statement = db
        .connection()
        .prepare(
            "SELECT id FROM review_compensation_ledger
              WHERE id > ?1 AND reverses_entry_id IS NOT NULL ORDER BY id",
        )
        .map_err(|error| format!("restore target post-v60 compensation reversals are unreadable: {error}"))?;
    let post_v60_reversal_ids = reversal_statement
        .query_map([ledger_frontier], |row| row.get::<_, i64>(0))
        .map_err(|error| format!("restore target post-v60 compensation reversals are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target post-v60 compensation reversals are unreadable: {error}"))?;
    drop(reversal_statement);
    for reversal_id in post_v60_reversal_ids {
        let matching_effect_inverse: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*)
                   FROM review_compensation_ledger reversal
                   JOIN review_compensation_ledger original
                     ON original.entry_id = reversal.reverses_entry_id
                   JOIN human_decision_effect_events effect
                     ON effect.review_event_id = original.review_event_id
                   JOIN human_decision_effect_reversals effect_reversal
                     ON effect_reversal.effect_event_id = effect.id
                   JOIN review_events event ON event.id = effect.review_event_id
                  WHERE reversal.id = ?1
                    AND reversal.entry_key = 'undo:' || effect_reversal.operation_id
                    AND event.operation_id = effect_reversal.operation_id",
                [reversal_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("restore target post-v60 compensation reversal linkage is unreadable: {error}"))?;
        if matching_effect_inverse != 1 {
            return Err(format!(
                "database restore refused: post-v60 compensation reversal {reversal_id} is not owned by one exact human-effect reversal"
            ));
        }
    }

    let (legacy_example_columns, legacy_example_rows) =
        exact_query_rows(db, "legacy agent-example snapshot", "SELECT * FROM legacy_agent_examples_v60")?;
    let (raw_legacy_example_columns, raw_legacy_example_rows) = exact_query_rows(
        db,
        "raw legacy agent examples",
        "SELECT example.rowid AS original_rowid, example.id, example.segment_id,
                example.audio_features, example.wrong_transcript, example.human_fix,
                example.created_at, example.source, example.verified_by_human,
                example.corrector_model_id
           FROM agent_examples example
          WHERE example.effect_event_id IS NULL
            AND EXISTS (
                 SELECT 1 FROM legacy_agent_examples_v60 legacy
                  WHERE legacy.id = example.id
            )",
    )?;
    require_encoded_row_equality(
        "legacy agent-example snapshot versus retained raw rows",
        legacy_example_columns,
        legacy_example_rows,
        raw_legacy_example_columns,
        raw_legacy_example_rows,
    )?;
    let forged_unbound_human_examples: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*)
               FROM agent_examples example
              WHERE example.effect_event_id IS NULL
                AND (example.source = 'human' OR example.verified_by_human = 1)
                AND NOT EXISTS (
                     SELECT 1 FROM legacy_agent_examples_v60 legacy
                      WHERE legacy.id = example.id AND legacy.original_rowid = example.rowid
                )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("restore target unbound agent-example provenance is unreadable: {error}"))?;
    if forged_unbound_human_examples != 0 {
        return Err(
            "database restore refused: post-v60 unbound rows cannot claim human agent-example provenance".to_string()
        );
    }

    let (legacy_correction_columns, legacy_correction_rows) =
        exact_query_rows(db, "legacy correction snapshot", "SELECT * FROM legacy_corrections_v60")?;
    let (raw_legacy_correction_columns, raw_legacy_correction_rows) = exact_query_rows(
        db,
        "raw legacy corrections",
        "SELECT correction.rowid AS original_rowid, correction.id, correction.segment_id,
                correction.audio_content_hash, correction.raw_hypothesis,
                correction.ensemble_hyps_json, correction.agreement_score,
                correction.jury_verdict, correction.human_fix,
                correction.model_version_id, correction.adapter_id,
                correction.reviewer_id, correction.loop_applied, correction.decided_at
           FROM corrections correction
          WHERE correction.effect_event_id IS NULL",
    )?;
    require_encoded_row_equality(
        "legacy correction snapshot versus retained raw rows",
        legacy_correction_columns,
        legacy_correction_rows,
        raw_legacy_correction_columns,
        raw_legacy_correction_rows,
    )?;

    let mut example_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, wrong_transcript, human_fix, source,
                    verified_by_human, effect_event_id
               FROM agent_examples WHERE effect_event_id IS NOT NULL ORDER BY id",
        )
        .map_err(|error| format!("restore target effect-bound human examples are unreadable: {error}"))?;
    let examples = example_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|error| format!("restore target effect-bound human examples are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target effect-bound human examples are unreadable: {error}"))?;
    drop(example_statement);
    for (id, segment_id, wrong, fix, source, verified, effect_id) in examples {
        let Some(effect) = effects_by_id.get(&effect_id).copied() else {
            return Err(format!(
                "database restore refused: effect-bound agent example {id} names a missing decision effect"
            ));
        };
        let exact_correction_text: Option<(String, String)> = db
            .connection()
            .query_row(
                "SELECT raw_hypothesis, human_fix FROM corrections WHERE effect_event_id = ?1",
                [effect_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("restore target example/correction linkage is unreadable: {error}"))?;
        let retained_draft: Option<(Option<String>, String)> = db
            .connection()
            .query_row(
                "SELECT normalized_transcript, raw_transcript FROM speech_segments WHERE id = ?1",
                [&effect.segment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("restore target example wrong-side provenance is unreadable: {error}"))?;
        let expected_wrong = retained_draft.and_then(|(normalized, raw)| {
            crate::db::rejected_transcript_for_learning(
                &fix,
                &[
                    effect.prior_verdict_transcript.clone(),
                    effect.prior_annotated_transcript.clone(),
                    normalized,
                    Some(raw),
                ],
            )
        });
        if !is_canonical_lowercase_uuid(&id)
            || segment_id != effect.segment_id
            || effect.action != "edit"
            || source != "human"
            || verified != 1
            || wrong.trim().is_empty()
            || fix.trim().is_empty()
            || crate::normalizer::learning_text_key(&wrong) == crate::normalizer::learning_text_key(&fix)
            || effect.decision_transcript.as_deref() != Some(fix.as_str())
            || expected_wrong.as_deref() != Some(wrong.as_str())
            || exact_correction_text.as_ref() != Some(&(wrong.clone(), fix.clone()))
        {
            return Err(format!(
                "database restore refused: effect-bound agent example {id} is not one genuine human edit"
            ));
        }
    }

    let mut correction_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, audio_content_hash, raw_hypothesis, human_fix,
                    reviewer_id, effect_event_id
               FROM corrections WHERE effect_event_id IS NOT NULL ORDER BY id",
        )
        .map_err(|error| format!("restore target effect-bound corrections are unreadable: {error}"))?;
    let corrections = correction_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|error| format!("restore target effect-bound corrections are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target effect-bound corrections are unreadable: {error}"))?;
    drop(correction_statement);
    let mut correction_text_by_effect = std::collections::HashMap::<i64, (String, String)>::new();
    for (id, segment_id, audio_hash, wrong, fix, reviewer, effect_id) in corrections {
        let Some(effect) = effects_by_id.get(&effect_id).copied() else {
            return Err(format!(
                "database restore refused: effect-bound correction {id} names a missing decision effect"
            ));
        };
        let reviewer_matches = match (reviewer.as_deref(), effect.reviewer.as_deref()) {
            (None, None) => true,
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            _ => false,
        };
        let retained_segment_matches = match segment_id.as_deref() {
            Some(segment_id) if segment_id == effect.segment_id => {
                db.connection()
                    .query_row(
                        "SELECT audio_content_hash = ?2 FROM speech_segments WHERE id = ?1",
                        rusqlite::params![segment_id, audio_hash],
                        |row| row.get::<_, bool>(0),
                    )
                    .optional()
                    .map_err(|error| format!("restore target correction segment identity is unreadable: {error}"))?
                    == Some(true)
            }
            _ => false,
        };
        let retained_draft: Option<(Option<String>, String)> = db
            .connection()
            .query_row(
                "SELECT normalized_transcript, raw_transcript FROM speech_segments WHERE id = ?1",
                [&effect.segment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("restore target correction wrong-side provenance is unreadable: {error}"))?;
        let expected_wrong = retained_draft.map(|(normalized, raw)| {
            crate::db::rejected_transcript_for_learning(
                &fix,
                &[
                    effect.prior_verdict_transcript.clone(),
                    effect.prior_annotated_transcript.clone(),
                    normalized,
                    Some(raw.clone()),
                ],
            )
            .unwrap_or(raw)
        });
        if !is_canonical_lowercase_uuid(&id)
            || effect.action != "edit"
            || !retained_segment_matches
            || !reviewer_matches
            || !crate::db::is_canonical_audio_content_hash(&audio_hash)
            || wrong.trim().is_empty()
            || fix.trim().is_empty()
            || crate::normalizer::learning_text_key(&wrong) == crate::normalizer::learning_text_key(&fix)
            || effect.decision_transcript.as_deref() != Some(fix.as_str())
            || expected_wrong.as_deref() != Some(wrong.as_str())
        {
            return Err(format!(
                "database restore refused: effect-bound correction {id} violates edit/audio/reviewer identity"
            ));
        }
        if correction_text_by_effect.insert(effect_id, (wrong, fix)).is_some() {
            return Err(format!("database restore refused: decision effect {effect_id} owns more than one correction"));
        }
    }

    let mut memory_statement = db
        .connection()
        .prepare(
            "SELECT id, wrong_token, human_token, slot_key, phonetic_key, source_segment,
                    confidence, hit_count, last_fired_at, confirm_count, override_count,
                    legacy_seed
               FROM correction_memory ORDER BY id",
        )
        .map_err(|error| format!("restore target correction-memory identities are unreadable: {error}"))?;
    let memories = memory_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        })
        .map_err(|error| format!("restore target correction-memory identities are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target correction-memory identities are unreadable: {error}"))?;
    drop(memory_statement);
    let memory_ids = memories.iter().map(|memory| memory.0.as_str()).collect::<std::collections::HashSet<_>>();
    for (
        id,
        wrong,
        human,
        slot,
        _phonetic,
        source_segment,
        confidence,
        hit_count,
        last_fired_at,
        confirm_count,
        override_count,
        legacy_seed,
    ) in &memories
    {
        if *legacy_seed == 0 {
            let capture_count: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM correction_memory_contributions
                      WHERE memory_id = ?1 AND capture_delta = 1",
                    [id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target correction-memory capture lineage is unreadable: {error}"))?;
            let capture_origin_count: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM correction_memory_contributions contribution
                       JOIN human_decision_effect_events effect
                         ON effect.id = contribution.effect_event_id
                      WHERE contribution.memory_id = ?1
                        AND contribution.capture_delta = 1
                        AND (?2 IS NULL OR effect.segment_id = ?2)",
                    rusqlite::params![id, source_segment],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target correction-memory capture identity is unreadable: {error}"))?;
            if !is_canonical_lowercase_uuid(id)
                || wrong.trim().is_empty()
                || human.trim().is_empty()
                || slot.trim().is_empty()
                || crate::normalizer::learning_text_key(wrong) == crate::normalizer::learning_text_key(human)
                || !confidence.is_finite()
                || (*confidence - 0.5).abs() > f64::EPSILON
                || *hit_count != 0
                || *confirm_count != 0
                || *override_count != 0
                || last_fired_at.is_some()
                || capture_count == 0
                || capture_origin_count == 0
            {
                return Err(format!(
                    "database restore refused: post-v60 correction memory {id} lacks its zero-baseline capture identity"
                ));
            }
        } else if *legacy_seed != 1 {
            return Err(format!("database restore refused: correction memory {id} has an invalid legacy boundary"));
        }
    }

    let mut contribution_statement = db
        .connection()
        .prepare(
            "SELECT effect_event_id, memory_id, capture_delta, confirm_delta,
                    override_delta, fired_at
               FROM correction_memory_contributions ORDER BY effect_event_id, memory_id",
        )
        .map_err(|error| format!("restore target correction-memory contributions are unreadable: {error}"))?;
    let contributions = contribution_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|error| format!("restore target correction-memory contributions are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target correction-memory contributions are unreadable: {error}"))?;
    drop(contribution_statement);
    for (effect_id, memory_id, capture, confirm, override_delta, fired_at) in &contributions {
        let Some(effect) = effects_by_id.get(effect_id).copied() else {
            return Err(format!(
                "database restore refused: correction-memory contribution {effect_id}/{memory_id} names a missing effect"
            ));
        };
        let evidence_fired = confirm + override_delta > 0;
        if !memory_ids.contains(memory_id.as_str())
            || !matches!(effect.action.as_str(), "accept" | "edit")
            || !matches!(*capture, 0 | 1)
            || !matches!(*confirm, 0 | 1)
            || !matches!(*override_delta, 0 | 1)
            || capture + confirm + override_delta == 0
            || confirm + override_delta > 1
            || (*capture == 1 && effect.action != "edit")
            || evidence_fired != fired_at.as_deref().is_some_and(|value| !value.trim().is_empty())
        {
            return Err(format!(
                "database restore refused: correction-memory contribution {effect_id}/{memory_id} violates its action/evidence identity"
            ));
        }
    }

    // Re-derive every post-v60 memory capture from the exact immutable correction owned by the
    // same decision effect. Merely linking arbitrary tokens to an edit effect is not provenance:
    // those tokens feed the live corrector. The extracted substitution tuple (including phonetic
    // key) and the contribution set must be byte-exact.
    type MemoryNaturalKey = (String, String, String, String);
    let memory_by_id =
        memories.iter().map(|memory| (memory.0.as_str(), memory)).collect::<std::collections::HashMap<_, _>>();
    let mut capture_ids_by_effect = std::collections::HashMap::<i64, std::collections::BTreeSet<String>>::new();
    let mut first_capture_effect_by_memory = std::collections::HashMap::<String, i64>::new();
    for (effect_id, memory_id, capture, _, _, _) in &contributions {
        if *capture == 1 {
            capture_ids_by_effect.entry(*effect_id).or_default().insert(memory_id.clone());
            first_capture_effect_by_memory
                .entry(memory_id.clone())
                .and_modify(|existing| *existing = (*existing).min(*effect_id))
                .or_insert(*effect_id);
        }
    }
    let memory_id_by_natural_key = memories
        .iter()
        .map(|memory| ((memory.3.clone(), memory.1.clone(), memory.2.clone(), memory.4.clone()), memory.0.clone()))
        .collect::<std::collections::HashMap<MemoryNaturalKey, String>>();

    for memory in &memories {
        if memory.11 != 0 {
            continue;
        }
        let Some(first_effect_id) = first_capture_effect_by_memory.get(&memory.0) else {
            return Err(format!(
                "database restore refused: post-v60 correction memory {} has no first capture effect",
                memory.0
            ));
        };
        let Some(first_effect) = effects_by_id.get(first_effect_id).copied() else {
            return Err(format!(
                "database restore refused: post-v60 correction memory {} names a missing first capture effect",
                memory.0
            ));
        };
        if memory.5.as_deref() != Some(first_effect.segment_id.as_str()) {
            return Err(format!(
                "database restore refused: post-v60 correction memory {} source segment differs from its first capture",
                memory.0
            ));
        }
    }

    for effect in &effects {
        let segment_is_gold: bool = db
            .connection()
            .query_row("SELECT is_gold FROM speech_segments WHERE id = ?1", [&effect.segment_id], |row| row.get(0))
            .optional()
            .map_err(|error| format!("restore target correction-memory segment state is unreadable: {error}"))?
            .unwrap_or(false);
        let mut expected_capture_ids = std::collections::BTreeSet::<String>::new();
        if !segment_is_gold {
            if let Some((wrong, fix)) = correction_text_by_effect.get(&effect.id) {
                let mut seen = std::collections::HashSet::<MemoryNaturalKey>::new();
                for extracted in crate::corrections::extract_substitution_memories(wrong, fix) {
                    let natural_key =
                        (extracted.slot_key, extracted.wrong_token, extracted.human_token, extracted.phonetic_key);
                    if seen.insert(natural_key.clone()) {
                        let Some(memory_id) = memory_id_by_natural_key.get(&natural_key) else {
                            return Err(format!(
                                "database restore refused: decision effect {} is missing an exactly derived correction memory",
                                effect.id
                            ));
                        };
                        expected_capture_ids.insert(memory_id.clone());
                    }
                }
            }
        }
        let actual_capture_ids = capture_ids_by_effect.get(&effect.id).cloned().unwrap_or_default();
        if actual_capture_ids != expected_capture_ids {
            return Err(format!(
                "database restore refused: decision effect {} has arbitrary or incomplete correction-memory captures",
                effect.id
            ));
        }
    }

    for (effect_id, memory_id, _, confirm, override_delta, _) in &contributions {
        if confirm + override_delta == 0 {
            continue;
        }
        let effect = effects_by_id[effect_id];
        let memory = memory_by_id[memory_id.as_str()];
        let existed_before_effect = memory.11 == 1
            || first_capture_effect_by_memory
                .get(memory_id)
                .is_some_and(|capture_effect_id| *capture_effect_id < *effect_id);
        let Some(reference) = effect.decision_transcript.as_deref() else {
            return Err(format!(
                "database restore refused: memory outcome {effect_id}/{memory_id} has no accepted decision text"
            ));
        };
        let entry = crate::corrections::MemoryEntry {
            wrong_token: memory.1.clone(),
            human_token: memory.2.clone(),
            slot_key: memory.3.clone(),
            phonetic_key: memory.4.clone(),
            confidence: memory.6,
            hit_count: memory.7,
        };
        let expected_outcome = crate::corrections::classify_memory_outcome(
            &effect.served_transcript,
            reference,
            &entry,
            &crate::corrections::FiringConfig::default(),
        );
        let outcome_matches = match expected_outcome {
            crate::corrections::MemoryOutcome::Confirm => *confirm == 1 && *override_delta == 0,
            crate::corrections::MemoryOutcome::Override => *confirm == 0 && *override_delta == 1,
            crate::corrections::MemoryOutcome::Neutral => false,
        };
        if !existed_before_effect || !outcome_matches {
            return Err(format!(
                "database restore refused: correction-memory outcome {effect_id}/{memory_id} is not re-derived from the served/decision text"
            ));
        }
    }

    let mut flag_statement = db
        .connection()
        .prepare(
            "SELECT effect.id, effect.operation_id, effect.segment_id, effect.prior_revision,
                    effect.flag_revision, effect.prior_verdict, effect.prior_rationale,
                    effect.flag_rationale, effect.prior_escalated, reversal.operation_id
               FROM review_flag_effect_events effect
               LEFT JOIN review_flag_effect_reversals reversal
                 ON reversal.flag_effect_event_id = effect.id
              ORDER BY effect.id",
        )
        .map_err(|error| format!("restore target review-flag effects are unreadable: {error}"))?;
    let flags = flag_statement
        .query_map([], |row| {
            Ok(FlagEffect {
                id: row.get(0)?,
                operation_id: row.get(1)?,
                segment_id: row.get(2)?,
                prior_revision: row.get(3)?,
                flag_revision: row.get(4)?,
                prior_verdict: row.get(5)?,
                prior_rationale: row.get(6)?,
                flag_rationale: row.get(7)?,
                prior_escalated: row.get(8)?,
                reversal_operation: row.get(9)?,
            })
        })
        .map_err(|error| format!("restore target review-flag effects are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target review-flag effects are unreadable: {error}"))?;
    drop(flag_statement);
    for flag in &flags {
        if flag.id <= 0
            || !is_canonical_lowercase_uuid(&flag.operation_id)
            || flag.segment_id.trim().is_empty()
            || flag.flag_revision != flag.prior_revision + 1
            || flag.flag_rationale.trim().is_empty()
            || crate::db::to_nfc(flag.flag_rationale.trim()) != flag.flag_rationale
            || !matches!(flag.prior_escalated, 0 | 1)
            || flag.reversal_operation.as_deref().is_some_and(|operation| !is_canonical_lowercase_uuid(operation))
        {
            return Err(format!(
                "database restore refused: review-flag effect {} violates its immutable revision/operation identity",
                flag.id
            ));
        }
        let initial_collision_count: i64 = db
            .connection()
            .query_row(
                "SELECT
                     (SELECT COUNT(*) FROM review_events WHERE operation_id = ?1)
                   + (SELECT COUNT(*) FROM human_decision_effect_events WHERE operation_id = ?1)
                   + (SELECT COUNT(*) FROM human_decision_effect_reversals WHERE operation_id = ?1)
                   + (SELECT COUNT(*) FROM review_flag_effect_reversals WHERE operation_id = ?1)",
                [&flag.operation_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("restore target flag operation identity is unreadable: {error}"))?;
        if initial_collision_count != 0 {
            return Err(format!(
                "database restore refused: review-flag effect {} reuses another review operation identity",
                flag.id
            ));
        }
        if let Some(operation_id) = flag.reversal_operation.as_deref() {
            let collision_count: i64 = db
                .connection()
                .query_row(
                    "SELECT
                         (SELECT COUNT(*) FROM review_events WHERE operation_id = ?1)
                       + (SELECT COUNT(*) FROM human_decision_effect_events WHERE operation_id = ?1)
                       + (SELECT COUNT(*) FROM human_decision_effect_reversals WHERE operation_id = ?1)
                       + (SELECT COUNT(*) FROM review_flag_effect_events WHERE operation_id = ?1)",
                    [operation_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target flag-reversal identity is unreadable: {error}"))?;
            if collision_count != 0 {
                return Err(format!(
                    "database restore refused: review-flag reversal {} reuses another review operation identity",
                    flag.id
                ));
            }
        }
    }

    let mut expected_active_decisions = std::collections::BTreeMap::<String, i64>::new();
    for effect in &effects {
        if effect.reversal_operation.is_none() {
            expected_active_decisions.insert(effect.segment_id.clone(), effect.id);
        }
    }
    let mut actual_active_statement = db
        .connection()
        .prepare("SELECT segment_id, id FROM effective_human_decision_effects_v60 ORDER BY segment_id")
        .map_err(|error| format!("restore target effective decision projection is unreadable: {error}"))?;
    let actual_active_decisions = actual_active_statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|error| format!("restore target effective decision projection is unreadable: {error}"))?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        .map_err(|error| format!("restore target effective decision projection is unreadable: {error}"))?;
    drop(actual_active_statement);
    if actual_active_decisions != expected_active_decisions {
        return Err(
            "database restore refused: effective human-decision projection does not select the latest active effect"
                .to_string(),
        );
    }

    let mut expected_active_flags = std::collections::BTreeMap::<String, i64>::new();
    for flag in &flags {
        if flag.reversal_operation.is_none() {
            expected_active_flags.insert(flag.segment_id.clone(), flag.id);
        }
    }
    let mut actual_flag_statement = db
        .connection()
        .prepare("SELECT segment_id, id FROM effective_review_flag_effects_v60 ORDER BY segment_id")
        .map_err(|error| format!("restore target effective flag projection is unreadable: {error}"))?;
    let actual_active_flags = actual_flag_statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|error| format!("restore target effective flag projection is unreadable: {error}"))?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        .map_err(|error| format!("restore target effective flag projection is unreadable: {error}"))?;
    drop(actual_flag_statement);
    if actual_active_flags != expected_active_flags {
        return Err(
            "database restore refused: effective review-flag projection does not select the latest active effect"
                .to_string(),
        );
    }

    let mut legacy_reviewed_statement = db
        .connection()
        .prepare(
            "SELECT id, review_revision, human_decision, verdict, verdict_transcript,
                    annotated_transcript, verified, reviewed_by, corrected_at, escalated,
                    is_gold, rationale
               FROM legacy_reviewed_segments_v60 ORDER BY id",
        )
        .map_err(|error| format!("restore target legacy reviewed-segment authority is unreadable: {error}"))?;
    let legacy_reviewed_segments = legacy_reviewed_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                LegacyReviewedState {
                    review_revision: row.get(1)?,
                    human_decision: row.get(2)?,
                    verdict: row.get(3)?,
                    verdict_transcript: row.get(4)?,
                    annotated_transcript: row.get(5)?,
                    verified: row.get(6)?,
                    reviewed_by: row.get(7)?,
                    corrected_at: row.get(8)?,
                    escalated: row.get(9)?,
                    is_gold: row.get(10)?,
                    rationale: row.get(11)?,
                },
            ))
        })
        .map_err(|error| format!("restore target legacy reviewed-segment authority is unreadable: {error}"))?
        .collect::<Result<std::collections::HashMap<_, _>, _>>()
        .map_err(|error| format!("restore target legacy reviewed-segment authority is unreadable: {error}"))?;
    drop(legacy_reviewed_statement);

    let mut mutations_by_segment = std::collections::BTreeMap::<String, Vec<ReviewMutation>>::new();
    for effect in effects.iter().cloned() {
        mutations_by_segment
            .entry(effect.segment_id.clone())
            .or_default()
            .push(ReviewMutation::Decision(Box::new(effect)));
    }
    for flag in flags.iter().cloned() {
        mutations_by_segment.entry(flag.segment_id.clone()).or_default().push(ReviewMutation::Flag(flag));
    }

    for (segment_id, mutations) in &mut mutations_by_segment {
        mutations.sort_by_key(|mutation| (mutation.applied_revision(), mutation.prior_revision()));
        let first = mutations
            .first()
            .ok_or_else(|| format!("database restore refused: empty review-effect chain for {segment_id}"))?;

        // Flags deliberately do not copy or mutate the human transcript/verification fields.  Bind
        // those untouched fields to the first decision's immutable prior snapshot (when one follows
        // the flag), otherwise to the retained row, then replay every later decision across any
        // intervening flags.  This prevents a forged first flag from laundering an unbound verified
        // annotation merely because the exhaustive scan sees that an effect names the segment.
        let first_decision = mutations.iter().find_map(|mutation| match mutation {
            ReviewMutation::Decision(effect) => Some(effect),
            ReviewMutation::Flag(_) => None,
        });
        let (baseline_human_state, current_is_gold): (StableHumanState, i64) = if let Some(effect) = first_decision {
            let is_gold = db
                .connection()
                .query_row("SELECT is_gold FROM speech_segments WHERE id = ?1", [segment_id], |row| row.get(0))
                .optional()
                .map_err(|error| format!("restore target review baseline is unreadable: {error}"))?
                .ok_or_else(|| format!("database restore refused: review-effect segment {segment_id} is missing"))?;
            (decision_prior_stable_state(effect), is_gold)
        } else {
            db.connection()
                .query_row(
                    "SELECT verified, annotated_transcript, verdict_transcript,
                            human_decision, corrected_at, reviewed_by, is_gold
                       FROM speech_segments WHERE id = ?1",
                    [segment_id],
                    |row| {
                        Ok((
                            StableHumanState {
                                verified: row.get(0)?,
                                annotated_transcript: row.get(1)?,
                                verdict_transcript: row.get(2)?,
                                human_decision: row.get(3)?,
                                corrected_at: row.get(4)?,
                                reviewed_by: row.get(5)?,
                            },
                            row.get(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| format!("restore target review baseline is unreadable: {error}"))?
                .ok_or_else(|| format!("database restore refused: review-effect segment {segment_id} is missing"))?
        };
        if let Some(legacy) = legacy_reviewed_segments.get(segment_id) {
            let baseline_matches = match first {
                ReviewMutation::Decision(effect) => {
                    effect.prior_revision >= legacy.review_revision
                        && effect.prior_verified == legacy.verified
                        && effect.prior_annotated_transcript == legacy.annotated_transcript
                        && effect.prior_verdict == legacy.verdict
                        && effect.prior_verdict_transcript == legacy.verdict_transcript
                        && effect.prior_rationale == legacy.rationale
                        && effect.prior_escalated == legacy.escalated
                        && effect.prior_human_decision == legacy.human_decision
                        && effect.prior_corrected_at == legacy.corrected_at
                        && effect.prior_reviewed_by == legacy.reviewed_by
                }
                ReviewMutation::Flag(flag) => {
                    flag.prior_revision >= legacy.review_revision
                        && flag.prior_verdict == legacy.verdict
                        && flag.prior_rationale == legacy.rationale
                        && flag.prior_escalated == legacy.escalated
                }
            } && baseline_human_state.verified == legacy.verified
                && baseline_human_state.annotated_transcript == legacy.annotated_transcript
                && baseline_human_state.verdict_transcript == legacy.verdict_transcript
                && baseline_human_state.human_decision == legacy.human_decision
                && baseline_human_state.corrected_at == legacy.corrected_at
                && baseline_human_state.reviewed_by == legacy.reviewed_by
                && current_is_gold == legacy.is_gold;
            if !baseline_matches {
                return Err(format!(
                    "database restore refused: review-effect chain for segment {segment_id} does not start from its immutable pre-v60 reviewed state"
                ));
            }
        } else {
            let unbound_human_prior = baseline_human_state.verified != 0
                || !optional_text_is_blank(baseline_human_state.annotated_transcript.as_deref())
                || !optional_text_is_blank(baseline_human_state.human_decision.as_deref())
                || !optional_text_is_blank(baseline_human_state.reviewed_by.as_deref())
                || !optional_text_is_blank(baseline_human_state.corrected_at.as_deref())
                || current_is_gold != 0;
            let unbound_flag_prior = match first {
                ReviewMutation::Flag(flag) => {
                    flag.prior_escalated != 0
                        || flag
                            .prior_verdict
                            .as_deref()
                            .is_some_and(|value| value.starts_with("human_") || value == "escalated")
                }
                ReviewMutation::Decision(effect) => effect
                    .prior_verdict
                    .as_deref()
                    .is_some_and(|value| value.starts_with("human_") || value == "escalated"),
            };
            if unbound_human_prior || unbound_flag_prior {
                return Err(format!(
                    "database restore refused: review-effect chain for segment {segment_id} starts from unsnapshotted human review truth"
                ));
            }
        }

        let mut expected_stable_human_state = baseline_human_state;
        let mut expected_rationale = match first {
            ReviewMutation::Decision(effect) => effect.prior_rationale.clone(),
            ReviewMutation::Flag(effect) => effect.prior_rationale.clone(),
        };
        for mutation in mutations.iter() {
            match mutation {
                ReviewMutation::Decision(effect) => {
                    if decision_prior_stable_state(effect) != expected_stable_human_state {
                        return Err(format!(
                            "database restore refused: review effect chain for segment {segment_id} changes human transcript/verification fields across a flag without authority"
                        ));
                    }
                    if effect.prior_rationale != expected_rationale
                        || effect.decision_rationale != effect.prior_rationale
                    {
                        return Err(format!(
                            "database restore refused: review effect chain for segment {segment_id} changes rationale across a human decision"
                        ));
                    }
                    expected_stable_human_state = decision_terminal_stable_state(effect);
                    expected_rationale = effect.decision_rationale.clone();
                }
                ReviewMutation::Flag(effect) => {
                    if effect.prior_rationale != expected_rationale {
                        return Err(format!(
                            "database restore refused: review effect chain for segment {segment_id} has a forged flag rationale prior-state"
                        ));
                    }
                    expected_rationale = flag_terminal_state(effect).rationale;
                }
            }
        }
        for pair in mutations.windows(2) {
            if pair[1].applied_revision() <= pair[0].applied_revision()
                || pair[1].prior_revision() < pair[0].terminal_revision()
            {
                return Err(format!(
                    "database restore refused: review effects for segment {segment_id} overlap or reverse a shadowed mutation"
                ));
            }
            let prior_snapshot_continuous = match (&pair[0], &pair[1]) {
                (ReviewMutation::Decision(previous), ReviewMutation::Decision(next)) => {
                    decision_terminal_state(previous)
                        == DecisionOwnedState {
                            verified: next.prior_verified,
                            annotated_transcript: next.prior_annotated_transcript.clone(),
                            verdict: next.prior_verdict.clone(),
                            verdict_transcript: next.prior_verdict_transcript.clone(),
                            escalated: next.prior_escalated,
                            human_decision: next.prior_human_decision.clone(),
                            corrected_at: next.prior_corrected_at.clone(),
                            reviewed_by: next.prior_reviewed_by.clone(),
                        }
                }
                (ReviewMutation::Flag(previous), ReviewMutation::Flag(next)) => {
                    flag_terminal_state(previous)
                        == FlagOwnedState {
                            verdict: next.prior_verdict.clone(),
                            rationale: next.prior_rationale.clone(),
                            escalated: next.prior_escalated,
                        }
                }
                (ReviewMutation::Decision(previous), ReviewMutation::Flag(next)) => {
                    let terminal = decision_terminal_state(previous);
                    terminal.verdict == next.prior_verdict && terminal.escalated == next.prior_escalated
                }
                (ReviewMutation::Flag(previous), ReviewMutation::Decision(next)) => {
                    let terminal = flag_terminal_state(previous);
                    terminal.verdict == next.prior_verdict && terminal.escalated == next.prior_escalated
                }
            };
            if !prior_snapshot_continuous {
                return Err(format!(
                    "database restore refused: review effect chain for segment {segment_id} has a forged or discontinuous prior snapshot"
                ));
            }
        }

        let Some(latest) = mutations.last() else {
            continue;
        };
        debug_assert_eq!(latest.segment_id(), segment_id);
        let current: Option<CurrentReviewState> = db
            .connection()
            .query_row(
                "SELECT review_revision, verified, annotated_transcript, verdict,
                        verdict_transcript, escalated, human_decision, corrected_at,
                        reviewed_by, rationale
                   FROM speech_segments WHERE id = ?1",
                [segment_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("restore target current review-effect state is unreadable: {error}"))?;
        let Some((
            current_revision,
            current_verified,
            current_annotated,
            current_verdict,
            current_verdict_transcript,
            current_escalated,
            current_human_decision,
            current_corrected_at,
            current_reviewed_by,
            current_rationale,
        )) = current
        else {
            return Err(format!(
                "database restore refused: reviewed segment {segment_id} is missing while its immutable schema-v60 effect history remains"
            ));
        };
        if current_revision < latest.terminal_revision() {
            return Err(format!(
                "database restore refused: segment {segment_id} predates its latest review-effect revision"
            ));
        }
        let current_stable_human_state = StableHumanState {
            verified: current_verified,
            annotated_transcript: current_annotated.clone(),
            verdict_transcript: current_verdict_transcript.clone(),
            human_decision: current_human_decision.clone(),
            corrected_at: current_corrected_at.clone(),
            reviewed_by: current_reviewed_by.clone(),
        };
        if current_stable_human_state != expected_stable_human_state {
            return Err(format!(
                "database restore refused: segment {segment_id} has unbound human transcript/verification state outside its exact review-effect chain"
            ));
        }
        if current_rationale != expected_rationale {
            return Err(format!(
                "database restore refused: segment {segment_id} rationale disagrees with its exact mixed decision/flag effect chain"
            ));
        }

        match latest {
            ReviewMutation::Decision(effect) if effect.reversal_operation.is_none() => {
                let expected_verdict = format!("human_{}", effect.action);
                let expected_verdict_transcript = if effect.action == "reject" {
                    effect.prior_verdict_transcript.as_ref()
                } else {
                    effect.decision_transcript.as_ref()
                };
                if current_revision < effect.decision_revision
                    || current_human_decision.as_deref() != Some(effect.action.as_str())
                    || current_verdict.as_deref() != Some(expected_verdict.as_str())
                    || current_escalated != 0
                    || current_verified != effect.decision_verified
                    || current_annotated != effect.decision_annotated_transcript
                    || current_verdict_transcript.as_ref() != expected_verdict_transcript
                    || current_corrected_at.as_deref() != Some(effect.decision_corrected_at.as_str())
                    || current_reviewed_by != effect.reviewer
                {
                    return Err(format!(
                        "database restore refused: segment {segment_id} disagrees with its latest active human-decision effect {}",
                        effect.id
                    ));
                }
            }
            ReviewMutation::Decision(effect) => {
                let exact_inverse_revision = effect.decision_revision + 1;
                let exact_snapshot = current_verified == effect.prior_verified
                    && current_annotated == effect.prior_annotated_transcript
                    && current_verdict == effect.prior_verdict
                    && current_verdict_transcript == effect.prior_verdict_transcript
                    && current_escalated == effect.prior_escalated
                    && current_human_decision == effect.prior_human_decision
                    && current_corrected_at == effect.prior_corrected_at
                    && current_reviewed_by == effect.prior_reviewed_by;
                if current_revision < exact_inverse_revision || !exact_snapshot {
                    return Err(format!(
                        "database restore refused: segment {segment_id} does not reflect human-decision reversal {}",
                        effect.id
                    ));
                }
            }
            ReviewMutation::Flag(flag) if flag.reversal_operation.is_none() => {
                if current_revision < flag.flag_revision
                    || current_verdict.as_deref() != Some("escalated")
                    || current_escalated != 1
                    || current_human_decision.as_deref().is_some_and(|value| !value.trim().is_empty())
                    || current_rationale.as_deref() != Some(flag.flag_rationale.as_str())
                {
                    return Err(format!(
                        "database restore refused: segment {segment_id} disagrees with its latest active review-flag effect {}",
                        flag.id
                    ));
                }
            }
            ReviewMutation::Flag(flag) => {
                let exact_inverse_revision = flag.flag_revision + 1;
                let exact_snapshot = current_verdict == flag.prior_verdict
                    && current_rationale == flag.prior_rationale
                    && current_escalated == flag.prior_escalated
                    && optional_text_is_blank(current_human_decision.as_deref());
                if current_revision < exact_inverse_revision || !exact_snapshot {
                    return Err(format!(
                        "database restore refused: segment {segment_id} does not reflect review-flag reversal {}",
                        flag.id
                    ));
                }
            }
        }
    }

    // Exhaustive current-row coverage closes the renderer/staged-file bypass: every row that can
    // presently export or advertise human-reviewed truth must be explained either by the immutable
    // pre-v60 snapshot or by the validated schema-v60 mutation chain above. A target-added row is
    // not legitimate merely because no effect happens to name it.
    let mut current_reviewed_statement = db
        .connection()
        .prepare(
            "SELECT segment.id, segment.review_revision, segment.human_decision,
                    segment.verdict, segment.verdict_transcript, segment.annotated_transcript,
                    segment.verified, segment.reviewed_by, segment.corrected_at,
                    segment.escalated, segment.is_gold, segment.rationale
               FROM speech_segments segment
              WHERE segment.verified = 1
                 OR segment.is_gold = 1
                 OR segment.human_decision IS NOT NULL
                 OR segment.reviewed_by IS NOT NULL
                 OR segment.corrected_at IS NOT NULL
                 OR segment.escalated = 1
                 OR segment.verdict = 'escalated'
                 OR segment.verdict LIKE 'human_%'
                 OR EXISTS (
                      SELECT 1 FROM review_events event
                       WHERE event.segment_id = segment.id
                         AND event.source <> 'couch_spot_check'
                         AND event.action IN ('accept', 'edit', 'reject')
                 )
                 OR EXISTS (
                      SELECT 1 FROM review_compensation_ledger ledger
                       WHERE ledger.segment_id = segment.id
                         AND ledger.compensation_action = 'undo'
                 )
              ORDER BY segment.id",
        )
        .map_err(|error| format!("restore target current reviewed-row authority is unreadable: {error}"))?;
    let current_reviewed_rows = current_reviewed_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                LegacyReviewedState {
                    review_revision: row.get(1)?,
                    human_decision: row.get(2)?,
                    verdict: row.get(3)?,
                    verdict_transcript: row.get(4)?,
                    annotated_transcript: row.get(5)?,
                    verified: row.get(6)?,
                    reviewed_by: row.get(7)?,
                    corrected_at: row.get(8)?,
                    escalated: row.get(9)?,
                    is_gold: row.get(10)?,
                    rationale: row.get(11)?,
                },
            ))
        })
        .map_err(|error| format!("restore target current reviewed-row authority is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target current reviewed-row authority is unreadable: {error}"))?;
    drop(current_reviewed_statement);
    for (segment_id, current) in current_reviewed_rows {
        if mutations_by_segment.contains_key(&segment_id) {
            continue;
        }
        let Some(legacy) = legacy_reviewed_segments.get(&segment_id) else {
            return Err(format!(
                "database restore refused: current reviewed segment {segment_id} has neither immutable legacy authority nor a schema-v60 effect chain"
            ));
        };
        let exact_legacy_terminal = current.review_revision >= legacy.review_revision
            && current.human_decision == legacy.human_decision
            && current.verdict == legacy.verdict
            && current.verdict_transcript == legacy.verdict_transcript
            && current.annotated_transcript == legacy.annotated_transcript
            && current.verified == legacy.verified
            && current.reviewed_by == legacy.reviewed_by
            && current.corrected_at == legacy.corrected_at
            && current.escalated == legacy.escalated
            && current.is_gold == legacy.is_gold
            && current.rationale == legacy.rationale;
        if !exact_legacy_terminal {
            return Err(format!(
                "database restore refused: current reviewed segment {segment_id} disagrees with its immutable pre-v60 terminal state"
            ));
        }
    }

    Ok(())
}

/// Recompute every restored listening receipt from its integer media counters. A staged file can
/// carry rows that predate the current triggers/writer; trusting their stored REAL would let
/// `played_ms = 0, coverage_ratio = 1` become durable no-listen authority after a restore.
fn validate_playback_receipt_semantics(db: &crate::db::Database) -> Result<(), String> {
    use rusqlite::OptionalExtension;

    let mut statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, segment_revision, audio_fingerprint, played_ms,
                    clip_duration_ms, coverage_ratio, policy_version, started_at_ms,
                    source_start_ms, source_end_ms
               FROM playback_receipts ORDER BY id",
        )
        .map_err(|error| format!("restore target playback receipts are unreadable: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
            ))
        })
        .map_err(|error| format!("restore target playback receipts are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target playback receipts are unreadable: {error}"))?;
    drop(statement);

    for (
        id,
        segment_id,
        segment_revision,
        stored_audio_identity,
        played_ms,
        clip_duration_ms,
        coverage,
        policy_version,
        started_at_ms,
        source_start_ms,
        source_end_ms,
    ) in rows
    {
        let expected_coverage = if clip_duration_ms > 0 && played_ms >= 0 {
            (played_ms as f64 / clip_duration_ms as f64).min(1.0)
        } else {
            f64::NAN
        };
        let tolerance = 1e-12_f64.max(expected_coverage.abs() * f64::EPSILON * 8.0);
        if id <= 0
            || segment_id.trim().is_empty()
            || segment_revision < 0
            || stored_audio_identity.trim().is_empty()
            || played_ms < 0
            || started_at_ms < 0
            || clip_duration_ms <= 0
            || !coverage.is_finite()
            || !expected_coverage.is_finite()
            || (coverage - expected_coverage).abs() > tolerance
            || !matches!(policy_version, 1 | 2 | crate::db::PLAYBACK_POLICY_VERSION)
        {
            return Err(format!(
                "database restore refused: playback receipt {id} violates the canonical writer invariants"
            ));
        }

        let current: Option<(i64, Option<String>, i64, Option<String>)> = db
            .connection()
            .query_row(
                "SELECT COALESCE(review_revision, 0),
                        NULLIF(TRIM(COALESCE(audio_content_hash, '')), ''),
                        COALESCE(duration_ms, 0), alignment_json
                   FROM speech_segments WHERE id = ?1",
                [&segment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| format!("restore target playback segment identity is unreadable: {error}"))?;
        let Some((current_revision, current_content_hash, current_duration, current_alignment_json)) = current else {
            return Err(format!("database restore refused: playback receipt {id} points to a missing segment"));
        };
        // Production minting reads the current revision atomically; a future-revision receipt is
        // impossible and would become a pre-minted authorization after the next segment UPDATE.
        if segment_revision > current_revision {
            return Err(format!("database restore refused: playback receipt {id} is from a future segment revision"));
        }
        // Policy 1 stored the v50 64-bit spectral candidate in the legacy `audio_fingerprint`
        // receipt column. Preserve it as historical audit evidence; policy 2 stored decoded-PCM
        // BLAKE3 but predates source-span binding. Neither can authorize policy-3 decisions.
        if policy_version == 1 {
            if source_start_ms.is_some() || source_end_ms.is_some() {
                return Err(format!(
                    "database restore refused: legacy policy-1 playback receipt {id} claims a policy-3 source span"
                ));
            }
            continue;
        }
        if !crate::db::is_canonical_audio_content_hash(&stored_audio_identity) {
            return Err(format!(
                "database restore refused: content-hash playback receipt {id} lacks a canonical decoded-PCM BLAKE3 hash"
            ));
        }
        let receipt_source_span = match (source_start_ms, source_end_ms) {
            (Some(start), Some(end)) if start >= 0 && end > start => Some((start, end)),
            (None, None) if policy_version == 2 => None,
            _ => {
                return Err(format!(
                    "database restore refused: policy-{policy_version} playback receipt {id} has an invalid source span"
                ));
            }
        };
        if policy_version == 2 && receipt_source_span.is_some() {
            return Err(format!(
                "database restore refused: historical policy-2 playback receipt {id} claims a policy-3 source span"
            ));
        }
        if policy_version == crate::db::PLAYBACK_POLICY_VERSION
            && !receipt_source_span
                .is_some_and(|(start, end)| crate::db::source_span_matches_duration(start, end, clip_duration_ms))
        {
            return Err(format!(
                "database restore refused: policy-3 playback receipt {id} source span disagrees with decoded duration"
            ));
        }
        let Some(current_content_hash) =
            current_content_hash.filter(|value| crate::db::is_canonical_audio_content_hash(value))
        else {
            return Err(format!(
                "database restore refused: content-hash playback receipt {id} has no canonical server-derived segment BLAKE3 identity"
            ));
        };
        let current_source_span = crate::db::canonical_source_span(current_alignment_json.as_deref());
        if policy_version == crate::db::PLAYBACK_POLICY_VERSION
            && !current_source_span
                .is_some_and(|(start, end)| crate::db::source_span_matches_duration(start, end, current_duration))
        {
            return Err(format!(
                "database restore refused: policy-3 playback receipt {id} segment source span disagrees with decoded duration"
            ));
        }
        // Policy 3 freezes the segment's audio identity for the lifetime of the receipt.  Unrelated
        // metadata writes legitimately advance `review_revision`, so an older receipt revision is
        // expected, but its decoded-PCM BLAKE3, duration, and exact source window must still equal
        // the retained server row.  Checking only when revisions happened to be equal let a staged
        // database bump an unrelated column and then substitute a different valid-looking hash.
        let identity_must_match =
            policy_version == crate::db::PLAYBACK_POLICY_VERSION || segment_revision == current_revision;
        if identity_must_match
            && (stored_audio_identity != current_content_hash
                || clip_duration_ms != current_duration
                || current_duration <= 0
                || (policy_version == crate::db::PLAYBACK_POLICY_VERSION && receipt_source_span != current_source_span))
        {
            return Err(format!(
                "database restore refused: content-hash playback receipt {id} disagrees with its retained segment identity"
            ));
        }
    }
    Ok(())
}

fn validate_restore_target_semantics(db: &crate::db::Database) -> Result<(), String> {
    validate_review_compensation_semantics(db)?;
    validate_review_effect_semantics(db)?;
    validate_playback_receipt_semantics(db)?;
    crate::review_campaign::load(db)
        .map_err(|error| format!("database restore refused: sequential campaign authority is invalid: {error}"))?;
    crate::review_pool::load(db)
        .map_err(|error| format!("database restore refused: flexible review-pool authority is invalid: {error}"))?;
    Ok(())
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
    if has_durable_review_activity(db)? || has_durable_review_activity(&staged)? {
        return Err(
            "Bare database restore is refused when either the live or target generation contains durable review activity. Use a named recovery snapshot so database, pilot policy, and routing state restore as one verified generation."
                .to_string(),
        );
    }
    require_durable_review_history_superset(db, &staged)?;
    validate_restore_target_semantics(&staged)?;
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
    if let Some(pending) = load_named_restore_pending(data_dir)? {
        if pending.completed_selector.is_some() {
            return Err("the named restore already completed; only durable barrier cleanup may run".to_string());
        }
        if pending.source_selector != source_selector {
            return Err(format!(
                "an interrupted restore of '{}' is pending; retry that exact snapshot before selecting '{}'",
                pending.source_selector, source_selector
            ));
        }
        // The live connection may already contain the target (or a partial prior publication). The
        // only authoritative pre-restore floor is the original manifest-verified pin recorded before
        // the first swap; stage that pin independently and compare against it on every retry.
        let original_pin = crate::snapshot::resolve_snapshot_dir(data_dir, &pending.pre_restore_pin_selector)?;
        let original_source = original_pin.join("cortex-speech.db");
        let (floor_plan, floor) = prepare_named_restore_artifacts(&original_pin, &original_source, || {})
            .map_err(|error| format!("interrupted restore's original safety floor is unusable: {error}"))?;
        let floor_policy = explicit_snapshot_pilot_policy(&floor_plan.pilot, "original safety floor")?;
        require_durable_review_history_superset(&floor, &staged)?;
        require_active_pilot_policy_binding(&floor, floor_policy.as_ref(), &staged, &plan.pilot)?;
    } else {
        // No transaction has crossed its marker yet, so the locked live DB and its live policy are
        // the exact authoritative floor. Admission + the caller-owned DB mutex keep that floor fixed
        // through comparison, pin creation, marker commit, and page publication.
        let floor_policy = crate::review_pilot::load(data_dir)?;
        require_durable_review_history_superset(db, &staged)?;
        require_active_pilot_policy_binding(db, floor_policy.as_ref(), &staged, &plan.pilot)?;
    }
    validate_restore_target_semantics(&staged)?;
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
            let snapshot_schema: i64 = source
                .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |row| row.get(0))
                .map_err(|error| format!("snapshot pilot schema could not be verified: {error}"))?;
            if snapshot_schema < crate::review_pilot::REVIEW_PILOT_HIDDEN_KEYS_SCHEMA_VERSION {
                return Err(format!(
                    "policy-bearing snapshot schema {snapshot_schema} predates durable hidden-key authority v{}; restoring it could forget already-served paid QC keys",
                    crate::review_pilot::REVIEW_PILOT_HIDDEN_KEYS_SCHEMA_VERSION
                ));
            }
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

fn explicit_snapshot_pilot_policy(
    action: &SnapshotPilotPolicyRestore,
    context: &str,
) -> Result<Option<crate::review_pilot::ReviewPilotPolicy>, String> {
    match action {
        SnapshotPilotPolicyRestore::Install(bytes) => {
            let raw =
                std::str::from_utf8(bytes).map_err(|error| format!("{context} pilot policy is not UTF-8: {error}"))?;
            crate::review_pilot::parse(raw).map(Some)
        }
        SnapshotPilotPolicyRestore::ExplicitlyAbsent => Ok(None),
        SnapshotPilotPolicyRestore::PreserveLegacy => {
            Err(format!("{context} does not explicitly bind paid-review policy presence or absence"))
        }
    }
}

fn validate_pilot_hidden_structural_namespaces(db: &crate::db::Database, context: &str) -> Result<(), String> {
    let structural_violation: bool = db
        .connection()
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM (
                     SELECT after_review_event_id
                       FROM review_pilot_hidden_keys
                      GROUP BY after_review_event_id
                     HAVING COUNT(DISTINCT policy_sha256) > 1
                 )
                 UNION ALL
                 SELECT 1 FROM (
                     SELECT policy_sha256, after_review_event_id, reviewer
                       FROM review_pilot_hidden_keys
                      GROUP BY policy_sha256, after_review_event_id, reviewer COLLATE NOCASE
                     HAVING COUNT(*) > 2
                 )
                 UNION ALL
                 SELECT 1 FROM (
                     SELECT policy_sha256, after_review_event_id
                       FROM review_pilot_hidden_keys
                      GROUP BY policy_sha256, after_review_event_id
                     HAVING COUNT(*) > 4
                 )
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("{context} hidden-key structural quotas are unreadable: {error}"))?;
    if structural_violation {
        return Err(format!(
            "database restore refused: {context} contains a historical hidden-key namespace that violates one-policy-per-baseline or grant quotas"
        ));
    }
    Ok(())
}

fn validate_pilot_hidden_namespace(
    db: &crate::db::Database,
    policy: &crate::review_pilot::ReviewPilotPolicy,
    context: &str,
) -> Result<(), String> {
    let policy_sha256 = policy.policy_sha256()?;
    let baseline = policy.after_review_event_id;
    let maximum_event_id: i64 = db
        .connection()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("{context} pilot review history is unreadable: {error}"))?;
    if baseline > maximum_event_id {
        return Err(format!(
            "database restore refused: {context} pilot baseline {baseline} is ahead of review history maximum {maximum_event_id}"
        ));
    }
    let inconsistent_namespace: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM review_pilot_hidden_keys
              WHERE (policy_sha256 = ?1 OR after_review_event_id = ?2)
                AND NOT (policy_sha256 = ?1 AND after_review_event_id = ?2)",
            rusqlite::params![policy_sha256, baseline],
            |row| row.get(0),
        )
        .map_err(|error| format!("{context} pilot hidden-key namespace is unreadable: {error}"))?;
    if inconsistent_namespace != 0 {
        return Err(format!(
            "database restore refused: {context} has {inconsistent_namespace} hidden-key grant(s) inconsistent with its active policy SHA/baseline"
        ));
    }

    let mut statement = db
        .connection()
        .prepare(
            "SELECT reviewer, COUNT(*) FROM review_pilot_hidden_keys
              WHERE policy_sha256 = ?1 AND after_review_event_id = ?2
              GROUP BY reviewer COLLATE NOCASE",
        )
        .map_err(|error| format!("{context} pilot hidden-key roster is unreadable: {error}"))?;
    let reviewer_counts = statement
        .query_map(rusqlite::params![policy_sha256, baseline], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| format!("{context} pilot hidden-key roster is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} pilot hidden-key roster is unreadable: {error}"))?;
    let mut total = 0i64;
    for (reviewer, count) in reviewer_counts {
        if policy.cap_for(&reviewer).is_none() {
            return Err(format!(
                "database restore refused: {context} hidden-key namespace contains a reviewer outside its exact policy roster"
            ));
        }
        if count > crate::review_pilot::REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER {
            return Err(format!(
                "database restore refused: {context} hidden-key namespace exceeds the per-reviewer grant ceiling"
            ));
        }
        total += count;
    }
    if total > crate::review_pilot::REVIEW_PILOT_TOTAL_HIDDEN_QC {
        return Err(format!(
            "database restore refused: {context} hidden-key namespace exceeds the global grant ceiling"
        ));
    }
    Ok(())
}

fn validate_active_pilot_semantics(
    db: &crate::db::Database,
    policy: &crate::review_pilot::ReviewPilotPolicy,
    context: &str,
) -> Result<(), String> {
    use std::collections::{HashMap, HashSet};

    let policy_sha256 = policy.policy_sha256()?;
    let baseline = policy.after_review_event_id;
    let authorized =
        policy.reviewer_names().into_iter().map(|name| (name.to_ascii_lowercase(), name)).collect::<HashMap<_, _>>();
    let reviewer_key = |actual: &str| {
        let key = actual.trim().to_ascii_lowercase();
        authorized.contains_key(&key).then_some(key)
    };

    let mut grants = authorized.keys().map(|key| (key.clone(), HashSet::new())).collect::<HashMap<_, _>>();
    let mut grant_statement = db
        .connection()
        .prepare(
            "SELECT reviewer, segment_id FROM review_pilot_hidden_keys
              WHERE policy_sha256 = ?1 AND after_review_event_id = ?2
              ORDER BY reviewer COLLATE NOCASE, segment_id",
        )
        .map_err(|error| format!("{context} active pilot grants are unreadable: {error}"))?;
    let grant_rows = grant_statement
        .query_map(rusqlite::params![policy_sha256, baseline], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| format!("{context} active pilot grants are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} active pilot grants are unreadable: {error}"))?;
    drop(grant_statement);
    for (reviewer, segment_id) in grant_rows {
        let key = reviewer_key(&reviewer).ok_or_else(|| {
            format!("database restore refused: {context} active pilot grant has an unauthorized reviewer")
        })?;
        let reviewer_grants = grants
            .get_mut(&key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        if !reviewer_grants.insert(segment_id) {
            return Err(format!("database restore refused: {context} active pilot contains a duplicate grant"));
        }
    }

    let mut corpus_actions = authorized.keys().map(|key| (key.clone(), 0i64)).collect::<HashMap<_, _>>();
    let mut hidden_actions = authorized.keys().map(|key| (key.clone(), 0i64)).collect::<HashMap<_, _>>();
    let mut completed = authorized.keys().map(|key| (key.clone(), HashSet::new())).collect::<HashMap<_, _>>();
    let mut skipped = authorized.keys().map(|key| (key.clone(), HashSet::new())).collect::<HashMap<_, _>>();
    let mut hidden_event_actions = HashMap::<(String, String), String>::new();

    let mut event_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, reviewer, action, source FROM review_events
              WHERE id > ?1 AND source IN ('couch', 'couch_spot_check')
              ORDER BY id",
        )
        .map_err(|error| format!("{context} post-baseline pilot history is unreadable: {error}"))?;
    let events = event_statement
        .query_map([baseline], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| format!("{context} post-baseline pilot history is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} post-baseline pilot history is unreadable: {error}"))?;
    drop(event_statement);

    for (event_id, segment_id, reviewer, action, source) in events {
        let key = reviewer_key(&reviewer).ok_or_else(|| {
            format!(
                "database restore refused: {context} post-baseline pilot event {event_id} has an unauthorized reviewer"
            )
        })?;
        if !matches!(action.as_str(), "accept" | "edit" | "reject" | "skip") {
            return Err(format!(
                "database restore refused: {context} post-baseline pilot event {event_id} has an invalid action"
            ));
        }
        let is_grant = grants.get(&key).is_some_and(|segments| segments.contains(&segment_id));
        if source == "couch" {
            let corpus_count = corpus_actions
                .get_mut(&key)
                .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
            *corpus_count += 1;
            if is_grant {
                // Pre-v59/session-backed hidden skips were recorded as ordinary Couch skips. Keep
                // recognizing that exact history: it consumes a corpus slot and resolves the grant,
                // but any non-skip corpus finalization of a hidden key is corruption.
                if action != "skip" {
                    return Err(format!(
                        "database restore refused: {context} reserved hidden key was non-skip finalized through the corpus path"
                    ));
                }
                let reviewer_skips = skipped
                    .get_mut(&key)
                    .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
                if !reviewer_skips.insert(segment_id) {
                    return Err(format!(
                        "database restore refused: {context} reserved hidden key was resolved more than once"
                    ));
                }
            }
            continue;
        }

        if !is_grant {
            return Err(format!(
                "database restore refused: {context} hidden-check event {event_id} has no active durable grant"
            ));
        }
        if completed.get(&key).is_some_and(|segments| segments.contains(&segment_id))
            || skipped.get(&key).is_some_and(|segments| segments.contains(&segment_id))
        {
            return Err(format!("database restore refused: {context} reserved hidden key was resolved more than once"));
        }
        if hidden_event_actions.insert((key.clone(), segment_id.clone()), action.clone()).is_some() {
            return Err(format!("database restore refused: {context} reserved hidden key has duplicate hidden events"));
        }
        if action == "skip" {
            skipped
                .get_mut(&key)
                .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?
                .insert(segment_id);
        } else {
            completed
                .get_mut(&key)
                .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?
                .insert(segment_id);
        }
        let hidden_count = hidden_actions
            .get_mut(&key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        *hidden_count += 1;
    }

    let mut result_statement = db
        .connection()
        .prepare(
            "SELECT key.reviewer, key.segment_id, result.action,
                    result.submitted_transcript, result.expected_transcript,
                    result.noticed, result.cer
               FROM review_pilot_hidden_keys key
               JOIN spot_checks result
                 ON result.segment_id = key.segment_id
                AND result.reviewer = key.reviewer COLLATE NOCASE
              WHERE key.policy_sha256 = ?1 AND key.after_review_event_id = ?2
              ORDER BY key.reviewer COLLATE NOCASE, key.segment_id",
        )
        .map_err(|error| format!("{context} active pilot results are unreadable: {error}"))?;
    let result_rows = result_statement
        .query_map(rusqlite::params![policy_sha256, baseline], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, f64>(6)?,
            ))
        })
        .map_err(|error| format!("{context} active pilot results are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} active pilot results are unreadable: {error}"))?;
    drop(result_statement);
    let mut result_actions = HashMap::<(String, String), Vec<String>>::new();
    for (reviewer, segment_id, action, submitted, expected, noticed, cer) in result_rows {
        let key = reviewer_key(&reviewer).ok_or_else(|| {
            format!("database restore refused: {context} hidden-check result has an unauthorized reviewer")
        })?;
        let submitted = crate::db::to_nfc(submitted.trim());
        let expected = crate::db::to_nfc(expected.trim());
        let expected_noticed = action != "reject"
            && crate::normalizer::learning_text_key(&submitted) == crate::normalizer::learning_text_key(&expected);
        let expected_cer = crate::wer::compute_cer(&expected, &submitted);
        let cer_tolerance = 1e-12_f64.max(expected_cer.abs() * f64::EPSILON * 8.0);
        if !matches!(noticed, 0 | 1)
            || (noticed != 0) != expected_noticed
            || !cer.is_finite()
            || !expected_cer.is_finite()
            || (cer - expected_cer).abs() > cer_tolerance
        {
            return Err(format!(
                "database restore refused: {context} hidden-check result has impossible noticed/CER semantics"
            ));
        }
        result_actions.entry((key, segment_id)).or_default().push(action);
    }
    for (key, expected_action) in &hidden_event_actions {
        match result_actions.get(key) {
            Some(observed) if observed.len() == 1 && observed[0] == *expected_action => {}
            _ => {
                return Err(format!(
                    "database restore refused: {context} hidden-check event/result actions do not match exactly"
                ));
            }
        }
    }
    if result_actions.keys().any(|key| !hidden_event_actions.contains_key(key)) {
        return Err(format!(
            "database restore refused: {context} has an orphan hidden-check result without a matching event"
        ));
    }

    // A corpus verdict and its event/ledger are one database transaction in current builds. A
    // restored target may nevertheless contain pre-existing rows from an older half-write or a
    // crafted extra. For every CURRENT decision attributed to this active roster, require the latest
    // still-active campaign event to describe exactly that state. A fully reversed campaign chain is
    // allowed because atomic Undo deliberately restores the prior row snapshot.
    let reversed_entries = {
        let mut statement = db
            .connection()
            .prepare(
                "SELECT reverses_entry_id FROM review_compensation_ledger
                  WHERE policy_version = ?1 AND compensation_action = 'undo'
                    AND source = 'couch_undo' AND reverses_entry_id IS NOT NULL",
            )
            .map_err(|error| format!("{context} pilot undo ledger is unreadable: {error}"))?;
        let rows = statement
            .query_map([crate::db::REVIEW_PAY_POLICY_VERSION], |row| row.get::<_, String>(0))
            .map_err(|error| format!("{context} pilot undo ledger is unreadable: {error}"))?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(|error| format!("{context} pilot undo ledger is unreadable: {error}"))?;
        rows
    };
    let mut active_corpus = HashMap::<String, (i64, String, String, i64)>::new();
    let mut corpus_statement = db
        .connection()
        .prepare(
            "SELECT event.id, event.segment_id, event.reviewer, event.action,
                    (SELECT COUNT(*) FROM review_compensation_ledger ledger
                      WHERE ledger.policy_version = ?2 AND ledger.review_event_id = event.id),
                    (SELECT entry_id FROM review_compensation_ledger ledger
                      WHERE ledger.policy_version = ?2 AND ledger.review_event_id = event.id
                      ORDER BY ledger.id LIMIT 1),
                    (SELECT decision_revision FROM review_compensation_ledger ledger
                      WHERE ledger.policy_version = ?2 AND ledger.review_event_id = event.id
                      ORDER BY ledger.id LIMIT 1)
               FROM review_events event
              WHERE event.id > ?1 AND event.source = 'couch'
                AND event.action IN ('accept','edit','reject')
              ORDER BY event.id",
        )
        .map_err(|error| format!("{context} pilot corpus-state history is unreadable: {error}"))?;
    let corpus_rows = corpus_statement
        .query_map(rusqlite::params![baseline, crate::db::REVIEW_PAY_POLICY_VERSION], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(|error| format!("{context} pilot corpus-state history is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} pilot corpus-state history is unreadable: {error}"))?;
    drop(corpus_statement);
    for (event_id, segment_id, reviewer, action, ledger_count, entry_id, decision_revision) in corpus_rows {
        if ledger_count != 1 || entry_id.is_none() || decision_revision.is_none() {
            return Err(format!(
                "database restore refused: {context} corpus event {event_id} lacks one valid compensation ledger entry"
            ));
        }
        let entry_id = entry_id.ok_or_else(|| {
            format!("database restore refused: {context} corpus event {event_id} has no ledger identity")
        })?;
        if !reversed_entries.contains(&entry_id) {
            let decision_revision = decision_revision.ok_or_else(|| {
                format!("database restore refused: {context} corpus event {event_id} has no decision revision")
            })?;
            active_corpus.insert(segment_id, (event_id, reviewer, action, decision_revision));
        }
    }

    let mut current_statement = db
        .connection()
        .prepare(
            "SELECT id, reviewed_by, human_decision FROM speech_segments
              WHERE reviewed_by IS NOT NULL AND human_decision IN ('accept','edit','reject')",
        )
        .map_err(|error| format!("{context} current reviewed corpus state is unreadable: {error}"))?;
    let current_rows = current_statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .map_err(|error| format!("{context} current reviewed corpus state is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} current reviewed corpus state is unreadable: {error}"))?;
    drop(current_statement);
    for (segment_id, reviewer, decision) in current_rows {
        if reviewer_key(&reviewer).is_none() {
            continue;
        }
        match active_corpus.get(&segment_id) {
            Some((_, event_reviewer, event_action, _))
                if event_reviewer.trim().eq_ignore_ascii_case(reviewer.trim()) && event_action == &decision => {}
            None => {
                use rusqlite::OptionalExtension;
                let prior: Option<(String, String)> = db
                    .connection()
                    .query_row(
                        "SELECT reviewer, action FROM review_events
                          WHERE id <= ?1 AND segment_id = ?2 AND source = 'couch'
                            AND action IN ('accept','edit','reject')
                          ORDER BY id DESC LIMIT 1",
                        rusqlite::params![baseline, segment_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|error| format!("{context} pre-pilot corpus state is unreadable: {error}"))?;
                if !prior.is_some_and(|(prior_reviewer, prior_action)| {
                    prior_reviewer.trim().eq_ignore_ascii_case(reviewer.trim()) && prior_action == decision
                }) {
                    return Err(format!(
                        "database restore refused: {context} current reviewed segment {segment_id} has no matching active campaign event/ledger"
                    ));
                }
                // When all campaign entries were reversed, the exact prior event above proves the
                // reviewed row is the state atomic Undo restored. With no prior event, a normal Undo
                // restores an unreviewed row, which never enters this scan; any reviewed row is forged.
            }
            _ => {
                return Err(format!(
                    "database restore refused: {context} current reviewed segment {segment_id} has no matching active campaign event/ledger"
                ));
            }
        }
    }
    for (segment_id, (event_id, event_reviewer, event_action, decision_revision)) in &active_corpus {
        use rusqlite::OptionalExtension;
        let current: Option<(i64, Option<String>, Option<String>)> = db
            .connection()
            .query_row(
                "SELECT COALESCE(review_revision, 0), human_decision, reviewed_by
                   FROM speech_segments WHERE id = ?1",
                [segment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("{context} campaign segment state is unreadable: {error}"))?;
        if let Some((current_revision, current_decision, current_reviewer)) = current {
            if current_revision == *decision_revision
                && (current_decision.as_deref() != Some(event_action.as_str())
                    || !current_reviewer
                        .as_deref()
                        .is_some_and(|value| value.trim().eq_ignore_ascii_case(event_reviewer.trim())))
            {
                return Err(format!(
                    "database restore refused: {context} corpus event {event_id} has no matching current-revision segment state"
                ));
            }
        }
    }

    for key in authorized.keys() {
        let reviewer_completed = completed
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        let reviewer_skipped = skipped
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        if reviewer_completed.intersection(reviewer_skipped).next().is_some() {
            return Err(format!(
                "database restore refused: {context} hidden key has both completed and skipped resolution"
            ));
        }
        let corpus = *corpus_actions
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        let hidden = *hidden_actions
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        let reviewer = authorized
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        let corpus_cap = policy
            .cap_for(reviewer)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer cap is inconsistent"))?;
        if corpus > corpus_cap {
            return Err(format!("database restore refused: {context} exceeds the per-reviewer corpus-action ceiling"));
        }
        if hidden > crate::review_pilot::REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER {
            return Err(format!("database restore refused: {context} exceeds the per-reviewer hidden-action ceiling"));
        }
        if corpus + hidden
            > crate::review_pilot::REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER
                + crate::review_pilot::REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER
        {
            return Err(format!("database restore refused: {context} exceeds the per-reviewer UI-action ceiling"));
        }
    }
    let corpus_total: i64 = corpus_actions.values().sum();
    let hidden_total: i64 = hidden_actions.values().sum();
    if corpus_total > policy.max_total_corpus_actions {
        return Err(format!("database restore refused: {context} exceeds the global corpus-action ceiling"));
    }
    if hidden_total > crate::review_pilot::REVIEW_PILOT_TOTAL_HIDDEN_QC {
        return Err(format!("database restore refused: {context} exceeds the global hidden-action ceiling"));
    }
    if corpus_total + hidden_total > crate::review_pilot::REVIEW_PILOT_MAX_COMPENSATED_UI_ACTIONS {
        return Err(format!("database restore refused: {context} exceeds the global UI-action ceiling"));
    }
    Ok(())
}

/// If the authoritative floor has begun using its active controlled-pilot identity, the target must
/// carry that exact semantic policy. Baseline alone is insufficient: changing the roster at the same
/// event id would reinterpret grants and mint a fresh paid-action namespace.
fn require_active_pilot_policy_binding(
    floor: &crate::db::Database,
    floor_policy: Option<&crate::review_pilot::ReviewPilotPolicy>,
    target: &crate::db::Database,
    target_action: &SnapshotPilotPolicyRestore,
) -> Result<(), String> {
    validate_pilot_hidden_structural_namespaces(target, "target snapshot")?;
    let target_policy = match target_action {
        SnapshotPilotPolicyRestore::Install(bytes) => {
            let raw = std::str::from_utf8(bytes)
                .map_err(|error| format!("target snapshot pilot policy is not UTF-8: {error}"))?;
            Some(crate::review_pilot::parse(raw)?)
        }
        SnapshotPilotPolicyRestore::ExplicitlyAbsent | SnapshotPilotPolicyRestore::PreserveLegacy => None,
    };
    if let Some(policy) = target_policy.as_ref() {
        // Triggers constrain future INSERTs but cannot prove that pre-existing rows obeyed them.
        // Validate the migrated staged generation itself before it can replace one live page.
        validate_pilot_hidden_namespace(target, policy, "target snapshot")?;
        validate_active_pilot_semantics(target, policy, "target snapshot")?;
    }
    let Some(floor_policy) = floor_policy else {
        return Ok(());
    };
    validate_pilot_hidden_structural_namespaces(floor, "authoritative floor")?;
    validate_pilot_hidden_namespace(floor, floor_policy, "authoritative floor")?;
    let policy_sha256 = floor_policy.policy_sha256()?;
    let baseline = floor_policy.after_review_event_id;
    let grants: i64 = floor
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM review_pilot_hidden_keys
              WHERE policy_sha256 = ?1 AND after_review_event_id = ?2",
            rusqlite::params![policy_sha256, baseline],
            |row| row.get(0),
        )
        .map_err(|error| format!("authoritative pilot hidden-key grants are unreadable: {error}"))?;
    let activity: i64 = floor
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM review_events
              WHERE id > ?1 AND source IN ('couch', 'couch_spot_check')",
            [baseline],
            |row| row.get(0),
        )
        .map_err(|error| format!("authoritative pilot review activity is unreadable: {error}"))?;
    if grants == 0 && activity == 0 {
        return Ok(());
    }

    let Some(target_policy) = target_policy else {
        return Err(
            "database restore refused: the authoritative floor has policy-bound pilot grants/activity, but the target does not cryptographically bind that policy"
                .to_string(),
        );
    };
    let target_sha256 = target_policy.policy_sha256()?;
    if target_policy != *floor_policy || target_sha256 != policy_sha256 {
        return Err(
            "database restore refused: target pilot policy identity differs from the authoritative policy already used for grants/activity"
                .to_string(),
        );
    }
    Ok(())
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

fn publish_prepared_snapshot_generation_offline(
    data_dir: &Path,
    restore_plan: &SnapshotRestorePlan,
    staged: &crate::db::Database,
    live_controls: &AppSettings,
) -> Result<(), String> {
    validate_restore_target_semantics(staged)?;
    let db_path = data_dir.join("cortex-speech.db");
    let mut live = crate::db::Database::open_with_retry(db_path.to_string_lossy().as_ref())
        .map_err(|error| format!("could not open live database for recovery: {error}"))?;
    live.commit_staged_restore(staged)
        .map_err(|error| format!("could not publish recovered database generation: {error}"))?;
    let integrity =
        live.integrity_check().map_err(|error| format!("recovered live database could not be verified: {error}"))?;
    if integrity.trim() != "ok" {
        return Err(format!("recovered live database failed integrity_check: {integrity}"));
    }
    drop(live);
    install_snapshot_restore_plan(restore_plan, data_dir, live_controls)?;
    Ok(())
}

fn restore_snapshot_generation_offline(
    data_dir: &Path,
    selector: &str,
    live_controls: &AppSettings,
    authoritative_floor: &crate::db::Database,
    authoritative_policy: Option<&crate::review_pilot::ReviewPilotPolicy>,
) -> Result<(), String> {
    let snapshot_dir = crate::snapshot::resolve_snapshot_dir(data_dir, selector)?;
    let source = snapshot_dir.join("cortex-speech.db");
    let source_metadata = std::fs::symlink_metadata(&source)
        .map_err(|error| format!("snapshot '{selector}' has no readable database file: {error}"))?;
    if !source_metadata.file_type().is_file() || source_metadata.file_type().is_symlink() {
        return Err(format!("snapshot '{selector}' has no regular database file"));
    }
    let (restore_plan, staged) = prepare_named_restore_artifacts(&snapshot_dir, &source, || {})?;
    require_durable_review_history_superset(authoritative_floor, &staged)?;
    require_active_pilot_policy_binding(authoritative_floor, authoritative_policy, &staged, &restore_plan.pilot)?;
    publish_prepared_snapshot_generation_offline(data_dir, &restore_plan, &staged, live_controls)?;
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
    // Stage the recorded original pin once and keep that owned in-memory generation as the floor for
    // BOTH target retry and fallback. Never consult possibly-swapped live pages for admission, and do
    // not re-migrate the original twice (time-derived migration values could otherwise differ).
    let original_source = original_pin.join("cortex-speech.db");
    let (original_plan, original_floor) = prepare_named_restore_artifacts(&original_pin, &original_source, || {})?;
    if matches!(original_plan.pilot, SnapshotPilotPolicyRestore::PreserveLegacy)
        || original_plan
            .optional
            .iter()
            .any(|(_, action)| matches!(action, crate::snapshot::OptionalSnapshotRestore::PreserveLegacy))
    {
        return Err("interrupted restore's original safety pin does not explicitly bind every required config state"
            .to_string());
    }
    let original_policy = explicit_snapshot_pilot_policy(&original_plan.pilot, "original safety pin")?;

    let settings_path = data_dir.join("settings.json");
    let live_controls = strict_live_settings_for_restore(&settings_path)?;
    let target_result = restore_snapshot_generation_offline(
        data_dir,
        &pending.source_selector,
        &live_controls,
        &original_floor,
        original_policy.as_ref(),
    );
    let completed_selector = match target_result {
        Ok(()) => pending.source_selector.clone(),
        Err(target_error) => {
            tracing::error!(
                "interrupted target restore '{}' could not complete ({target_error}); rolling back verified original '{}'",
                pending.source_selector,
                pending.pre_restore_pin_selector
            );
            require_durable_review_history_superset(&original_floor, &original_floor)
                .and_then(|()| {
                    require_active_pilot_policy_binding(
                        &original_floor,
                        original_policy.as_ref(),
                        &original_floor,
                        &original_plan.pilot,
                    )
                })
                .and_then(|()| {
                    publish_prepared_snapshot_generation_offline(
                        data_dir,
                        &original_plan,
                        &original_floor,
                        &live_controls,
                    )
                })
                .map_err(|rollback_error| {
                    format!(
                        "interrupted restore could not complete target '{}' ({target_error}) and could not roll back verified original '{}' ({rollback_error}); normal startup is blocked",
                        pending.source_selector, pending.pre_restore_pin_selector
                    )
                })?;
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
    recover_interrupted_named_restore_with_admission(data_dir, RESTORE_ADMISSION.as_ref())
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
    // Schema v60 retired every machine-verdict writer: paid review truth now crosses only the
    // evidence-backed human-decision boundary. Continuing into the historical jury at v60+ would do
    // expensive work and then fail the import on its first forbidden write. Champion-only production
    // also has exactly one ASR, so a multi-ASR jury is meaningless even against an archival schema.
    // Keep the old jury executable only for explicit pre-v60 diagnostics and leave current drafts for
    // human review. This guard is deliberately at the shared core so directory, file, audiobook, and
    // direct-command callers cannot drift apart.
    let schema_version = crate::migrations::get_current_version(db).map_err(|error| error.to_string())?;
    if schema_version >= 60 || settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
        let reason = if schema_version >= 60 {
            "Schema v60+ sends machine drafts directly to the evidence-backed human review flow; machine jury writes are retired"
        } else {
            "Champion-only mode sends OmniASR 7B drafts directly to human review; auxiliary-ASR jury is not run"
        };
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
            "reason": reason
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

    #[test]
    fn a_post_batch_jury_failure_is_never_reported_as_a_completed_batch() {
        // The failure was log-only while the terminal `batch-progress` event still said
        // type:"completed" — the adjudication that decides what the review queue sees had failed and
        // nothing above the log said so.
        let jury = batch_terminal_halt_cause(None, Some("jury db unavailable".into()))
            .expect("a jury failure must produce a halt cause, not a clean completion");
        assert!(jury.contains("jury"), "{jury}");

        // A per-clip hard stop still wins and keeps its own cause.
        assert_eq!(
            batch_terminal_halt_cause(Some("segment x: decode failed".into()), Some("jury db unavailable".into())),
            Some("segment x: decode failed".to_string())
        );

        // And a genuinely clean run is still allowed to report completion.
        assert_eq!(batch_terminal_halt_cause(None, None), None);
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
            signal_anomaly_score: None,
            ..crate::db::SpeechSegment::default()
        }
    }

    fn legacy_machine_db() -> crate::db::Database {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&db, 7).unwrap(), vec![66, 65, 64, 63, 62, 61, 60]);
        db
    }

    fn copied_database(source: &crate::db::Database) -> crate::db::Database {
        let mut copy = crate::db::Database::open(":memory:").unwrap();
        copy.initialize().unwrap();
        copy.commit_staged_restore(source).unwrap();
        copy
    }

    fn insert_canonical_pay_segment(db: &crate::db::Database, id: &str) {
        db.insert_segment(&test_segment(id, &format!("{id}.wav"), "machine draft")).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?2,
                        audio_fingerprint = ?3,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}',
                        duration_ms = 1000
                  WHERE id = ?1",
                rusqlite::params![id, "a".repeat(64), 424_242_i64],
            )
            .unwrap();
    }

    fn canonical_operation(index: u64) -> String {
        format!("00000000-0000-4000-8000-{index:012x}")
    }

    fn canonical_phone_playback(
        db: &crate::db::Database,
        segment_id: &str,
        reviewer: &str,
    ) -> crate::db::PlaybackDecisionProof {
        let revision = db.segment_review_revision(segment_id).unwrap().unwrap();
        let audio_content_hash = db.segment_audio_content_hash(segment_id).unwrap().unwrap();
        let (source_start_ms, source_end_ms) = db.segment_source_span(segment_id).unwrap().unwrap();
        db.record_playback_receipt(&crate::db::PlaybackReceipt {
            segment_id: segment_id.to_string(),
            segment_revision: revision,
            audio_content_hash: audio_content_hash.clone(),
            reviewer: Some(reviewer.to_string()),
            session_id: None,
            started_at_ms: 1,
            played_ms: 1_000,
            clip_duration_ms: 1_000,
            source_start_ms: Some(source_start_ms),
            source_end_ms: Some(source_end_ms),
        })
        .unwrap();
        crate::db::PlaybackDecisionProof {
            segment_revision: revision,
            audio_content_hash,
            source_start_ms,
            source_end_ms,
        }
    }

    fn record_canonical_phone_edit(db: &crate::db::Database, segment_id: &str, operation_index: u64) -> (i64, String) {
        insert_canonical_pay_segment(db, segment_id);
        let proof = canonical_phone_playback(db, segment_id, "Reviewer");
        let operation = canonical_operation(operation_index);
        db.record_phone_human_decision_by_at_revision_with_operation_limit(
            segment_id,
            "edit",
            Some("machine truth"),
            "Reviewer",
            proof.segment_revision,
            &proof,
            &operation,
            &crate::db::review_operation_payload_hash(segment_id, "edit", "machine truth", "Reviewer"),
            "edit",
            "machine truth",
            None,
        )
        .unwrap()
        .unwrap();
        let effect_id = db.human_decision_effect_for_operation(&operation).unwrap().unwrap().0;
        (effect_id, operation)
    }

    fn record_canonical_skip(db: &crate::db::Database, segment_id: &str, reviewer: &str, index: u64) {
        db.record_review_event_with_operation(
            segment_id,
            reviewer,
            "skip",
            "couch",
            i64::try_from(index).unwrap(),
            &canonical_operation(index),
            &crate::db::review_operation_payload_hash(segment_id, "skip", "", reviewer),
        )
        .unwrap();
    }

    fn insert_test_compensation_ledger(db: &crate::db::Database) {
        db.connection()
            .execute(
                "INSERT INTO review_compensation_ledger
                    (id, entry_id, entry_key, policy_version, review_event_id,
                     canonical_work_id, canonical_identity_kind, reviewer, segment_id, source,
                     compensation_action, effective_decision, decision_revision, duration_ms,
                     rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                     corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id, created_at)
                 VALUES
                    (1, 'entry-1', 'key-1', ?1, NULL,
                     'work-1', 'test', 'Reviewer', 'ledger-segment', 'test',
                     'skip', 'skip', NULL, 1000,
                     0, 0, 0, 0, 0, NULL, '2026-08-22 00:00:00')",
                [crate::db::REVIEW_PAY_POLICY_VERSION],
            )
            .unwrap();
    }

    fn test_pilot_policy(
        after_review_event_id: i64,
        first: &str,
        second: &str,
    ) -> crate::review_pilot::ReviewPilotPolicy {
        crate::review_pilot::parse(
            &serde_json::json!({
                "schema_version": 1,
                "after_review_event_id": after_review_event_id,
                "max_total_corpus_actions": 20,
                "reviewers": [
                    { "name": first, "max_corpus_actions": 10 },
                    { "name": second, "max_corpus_actions": 10 }
                ]
            })
            .to_string(),
        )
        .unwrap()
    }

    fn pilot_restore_action(policy: &crate::review_pilot::ReviewPilotPolicy) -> SnapshotPilotPolicyRestore {
        SnapshotPilotPolicyRestore::Install(serde_json::to_vec(policy).unwrap())
    }

    fn insert_test_review_event(
        db: &crate::db::Database,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        source: &str,
        timestamp_ms: i64,
    ) {
        let paid_provenance = matches!(source, "couch" | "couch_spot_check");
        let requested_action = match action {
            "accept" | "edit" | "reject" | "skip" => action,
            _ => "accept",
        };
        let requested_transcript = if requested_action == "skip" { "" } else { "expected" };
        let operation_id = uuid::Uuid::new_v4().to_string();
        let operation_payload_hash =
            crate::db::review_operation_payload_hash(segment_id, requested_action, requested_transcript, reviewer);
        db.connection()
            .execute(
                "INSERT INTO review_events
                    (segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                     compensation_action, created_at, app_git_sha, playback_guard_version,
                     operation_id, operation_payload_hash, requested_action,
                     requested_transcript, served_transcript, served_revision)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1000, ?3, '2026-08-22 00:00:00', ?6, ?7,
                         ?8, ?9, ?10, ?11, 'expected', 0)",
                rusqlite::params![
                    segment_id,
                    reviewer,
                    action,
                    source,
                    timestamp_ms,
                    paid_provenance.then_some(crate::GIT_SHA),
                    paid_provenance.then_some("content-hash-raw-counter-v3"),
                    paid_provenance.then_some(operation_id),
                    paid_provenance.then_some(operation_payload_hash),
                    paid_provenance.then_some(requested_action),
                    paid_provenance.then_some(requested_transcript),
                ],
            )
            .unwrap();
    }

    fn insert_test_spot_result(db: &crate::db::Database, segment_id: &str, reviewer: &str, action: &str) {
        db.connection()
            .execute(
                "INSERT INTO spot_checks
                    (segment_id, reviewer, action, submitted_transcript, expected_transcript,
                     noticed, cer, created_at)
                 VALUES (?1, ?2, ?3, 'expected', 'expected', 1, 0.0,
                         '2026-08-22 00:00:00')",
                rusqlite::params![segment_id, reviewer, action],
            )
            .unwrap();
    }

    fn assert_floor_only_durable_row_rejected<Prepare, AddFloor>(
        expected_label: &str,
        prepare_shared: Prepare,
        add_floor_only: AddFloor,
    ) where
        Prepare: FnOnce(&crate::db::Database),
        AddFloor: FnOnce(&crate::db::Database),
    {
        let base = crate::db::Database::open(":memory:").unwrap();
        base.initialize().unwrap();
        prepare_shared(&base);
        let target = copied_database(&base);
        let floor = copied_database(&base);
        add_floor_only(&floor);
        let error = require_durable_review_history_superset(&floor, &target).unwrap_err();
        assert!(error.contains(expected_label), "expected {expected_label} rejection, got: {error}");
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
    fn durable_restore_floor_protects_all_pre_v60_authorities_by_exact_value() {
        assert_floor_only_durable_row_rejected(
            "review_pilot_hidden_keys",
            |_| {},
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'Reviewer', 'hidden-1')",
                        ["a".repeat(64)],
                    )
                    .unwrap();
            },
        );
        assert_floor_only_durable_row_rejected(
            "review_events",
            |_| {},
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO review_events
                            (id, segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                             compensation_action, created_at)
                         VALUES (1, 'event-1', 'Reviewer', 'skip', 'test', 1, 1000, 'skip',
                                 '2026-08-22 00:00:00')",
                        [],
                    )
                    .unwrap();
            },
        );
        assert_floor_only_durable_row_rejected(
            "spot_checks",
            |_| {},
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO spot_checks
                            (segment_id, reviewer, action, submitted_transcript, expected_transcript,
                             noticed, cer, created_at)
                         VALUES ('spot-1', 'Reviewer', 'edit', 'submitted', 'expected', 1, 0.0,
                                 '2026-08-22 00:00:00')",
                        [],
                    )
                    .unwrap();
            },
        );
        assert_floor_only_durable_row_rejected("review_compensation_ledger", |_| {}, insert_test_compensation_ledger);
        assert_floor_only_durable_row_rejected(
            "review_compensation_settlements",
            insert_test_compensation_ledger,
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO review_compensation_settlements
                            (id, settlement_id, policy_version, reviewer,
                             from_ledger_id_exclusive, through_ledger_id_inclusive,
                             allocated_micro_iqd, payout_reference, created_at)
                         VALUES (1, 'settlement-1', ?1, 'Reviewer', 0, 1, 0, 'payout-1',
                                 '2026-08-22 00:00:00')",
                        [crate::db::REVIEW_PAY_POLICY_VERSION],
                    )
                    .unwrap();
            },
        );
        assert_floor_only_durable_row_rejected(
            "review_compensation_policies",
            |_| {},
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO review_compensation_policies
                            (policy_version, effective_after_event_id, base_rate_micro_iqd_per_hour,
                             edit_basis_points, accept_basis_points, reject_basis_points,
                             skip_basis_points, created_at)
                         VALUES ('test-policy-v2', 0, 1, 10000, 1000, 1000, 0,
                                 '2026-08-22 00:00:00')",
                        [],
                    )
                    .unwrap();
            },
        );
        // A legacy correction has to exist before v60 snapshots the immutable frontier. Build that
        // floor first, then remove both the row and its snapshot from a trigger-disabled staged copy;
        // rolling only the floor through v60 after the target was copied would also recreate
        // review_effect_state with a different timestamp and test the wrong authority first.
        let correction_floor = crate::db::Database::open(":memory:").unwrap();
        correction_floor.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&correction_floor, 7).unwrap(), vec![66, 65, 64, 63, 62, 61, 60]);
        correction_floor
            .connection()
            .execute(
                "INSERT INTO corrections
                    (id, segment_id, audio_content_hash, raw_hypothesis, human_fix,
                     reviewer_id, decided_at)
                 VALUES ('correction-1', NULL, ?1, 'wrong', 'right', 'Reviewer',
                         '2026-08-22 00:00:00')",
                ["a".repeat(64)],
            )
            .unwrap();
        assert_eq!(crate::migrations::run_migrations(&correction_floor).unwrap(), vec![60, 61, 62, 63, 64, 65, 66]);
        let correction_target = copied_database(&correction_floor);
        correction_target
            .connection()
            .execute_batch(
                "DROP TRIGGER corrections_v60_effect_immutable_delete;
                 DROP TRIGGER legacy_corrections_v60_immutable_delete;
                 DELETE FROM corrections WHERE id = 'correction-1';
                 DELETE FROM legacy_corrections_v60 WHERE id = 'correction-1';",
            )
            .unwrap();
        let correction_error =
            require_durable_review_history_superset(&correction_floor, &correction_target).unwrap_err();
        assert!(correction_error.contains("corrections"), "expected corrections rejection, got: {correction_error}");
        assert_floor_only_durable_row_rejected(
            "playback_receipts",
            |base| {
                base.insert_segment(&test_segment("receipt-1", "receipt.wav", "draft")).unwrap();
            },
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO playback_receipts
                            (id, segment_id, segment_revision, audio_fingerprint, reviewer, session_id,
                             started_at_ms, played_ms, clip_duration_ms, coverage_ratio,
                             policy_version, created_at)
                         VALUES (1, 'receipt-1', 0, 'fingerprint', 'Reviewer', 'session',
                                 1, 1000, 1000, 1.0, 1, '2026-08-22 00:00:00')",
                        [],
                    )
                    .unwrap();
            },
        );

        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        floor
            .connection()
            .execute(
                "INSERT INTO review_events
                    (id, segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                     compensation_action, created_at)
                 VALUES (1, 'value-1', 'Reviewer', 'skip', 'test', 1, 1000, 'skip',
                         '2026-08-22 00:00:00')",
                [],
            )
            .unwrap();
        let target = copied_database(&floor);
        require_durable_review_history_superset(&floor, &target).unwrap();
        target.connection().execute("DROP TRIGGER review_events_v60_post_cutoff_immutable_update", []).unwrap();
        target.connection().execute("UPDATE review_events SET reviewer = 'Changed' WHERE id = 1", []).unwrap();
        let changed = require_durable_review_history_superset(&floor, &target).unwrap_err();
        assert!(changed.contains("review_events"), "same identity with changed values must fail: {changed}");

        let superset = copied_database(&floor);
        superset
            .connection()
            .execute(
                "INSERT INTO review_events
                    (id, segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                     compensation_action, created_at)
                 VALUES (2, 'value-2', 'Reviewer', 'skip', 'test', 2, 1000, 'skip',
                         '2026-08-22 00:00:01')",
                [],
            )
            .unwrap();
        require_durable_review_history_superset(&floor, &superset).unwrap();
    }

    #[test]
    fn durable_restore_floor_protects_every_v60_effect_authority() {
        fn assert_missing_is_refused(floor: &crate::db::Database, expected_label: &str, mutation_sql: &str) {
            let target = copied_database(floor);
            target.connection().execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
            target.connection().execute_batch(mutation_sql).unwrap();
            let error = require_durable_review_history_superset(floor, &target).unwrap_err();
            assert!(error.contains(expected_label), "expected {expected_label} refusal, got: {error}");
        }

        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        record_canonical_phone_edit(&floor, "durable-effect-edit", 201);

        insert_canonical_pay_segment(&floor, "durable-effect-undo");
        canonical_phone_playback(&floor, "durable-effect-undo", "Reviewer");
        let revision = floor.segment_review_revision("durable-effect-undo").unwrap().unwrap();
        let operation = canonical_operation(202);
        floor
            .record_phone_human_decision_by_at_revision_with_operation(
                "durable-effect-undo",
                "reject",
                None,
                "Reviewer",
                revision,
                &operation,
                &crate::db::review_operation_payload_hash("durable-effect-undo", "reject", "", "Reviewer"),
            )
            .unwrap()
            .unwrap();
        let effect_id = floor.human_decision_effect_for_operation(&operation).unwrap().unwrap().0;
        assert!(matches!(
            floor.undo_human_decision(effect_id, Some("Reviewer"), &operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));

        insert_canonical_pay_segment(&floor, "durable-flag-undo");
        let flag = floor
            .record_review_flag("durable-flag-undo", "durable flag", "00000000-0000-4000-8000-000000000801")
            .unwrap();
        assert!(matches!(
            floor.undo_review_flag(flag.effect_event_id, &canonical_operation(203)).unwrap(),
            crate::db::HumanFlagUndoOutcome::Applied { .. }
        ));

        for (table, predicate) in [
            ("human_decision_effect_events", "1=1"),
            ("human_decision_effect_reversals", "1=1"),
            ("review_flag_effect_events", "1=1"),
            ("review_flag_effect_reversals", "1=1"),
            ("correction_memory", "legacy_seed=0"),
            ("correction_memory_contributions", "1=1"),
            ("corrections", "effect_event_id IS NOT NULL"),
            ("agent_examples", "effect_event_id IS NOT NULL"),
        ] {
            let count: i64 = floor
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table} WHERE {predicate}"), [], |row| row.get(0))
                .unwrap();
            assert!(count > 0, "writer fixture must populate {table}");
        }
        validate_restore_target_semantics(&floor).unwrap();
        assert!(has_durable_review_activity(&floor).unwrap());

        assert_missing_is_refused(
            &floor,
            "review_effect_state",
            "DROP TRIGGER review_effect_state_immutable_delete;
             DELETE FROM review_effect_state;",
        );
        assert_missing_is_refused(
            &floor,
            "human_decision_effect_events",
            "DROP TRIGGER human_decision_effect_events_immutable_delete;
             DELETE FROM human_decision_effect_events
              WHERE id = (SELECT MIN(id) FROM human_decision_effect_events);",
        );
        assert_missing_is_refused(
            &floor,
            "human_decision_effect_reversals",
            "DROP TRIGGER human_decision_effect_reversals_immutable_delete;
             DELETE FROM human_decision_effect_reversals
              WHERE effect_event_id = (SELECT MIN(effect_event_id) FROM human_decision_effect_reversals);",
        );
        assert_missing_is_refused(
            &floor,
            "review_flag_effect_events",
            "DROP TRIGGER review_flag_effect_events_immutable_delete;
             DELETE FROM review_flag_effect_events
              WHERE id = (SELECT MIN(id) FROM review_flag_effect_events);",
        );
        assert_missing_is_refused(
            &floor,
            "review_flag_effect_reversals",
            "DROP TRIGGER review_flag_effect_reversals_immutable_delete;
             DELETE FROM review_flag_effect_reversals
              WHERE flag_effect_event_id = (SELECT MIN(flag_effect_event_id) FROM review_flag_effect_reversals);",
        );
        assert_missing_is_refused(
            &floor,
            "correction_memory",
            "DROP TRIGGER correction_memory_v60_immutable_delete;
             DELETE FROM correction_memory
              WHERE id = (SELECT MIN(id) FROM correction_memory WHERE legacy_seed=0);",
        );
        assert_missing_is_refused(
            &floor,
            "correction_memory_contributions",
            "DROP TRIGGER correction_memory_contributions_immutable_delete;
             DELETE FROM correction_memory_contributions
              WHERE rowid = (SELECT MIN(rowid) FROM correction_memory_contributions);",
        );
        assert_missing_is_refused(
            &floor,
            "corrections",
            "DROP TRIGGER corrections_v60_effect_immutable_delete;
             DELETE FROM corrections
              WHERE effect_event_id = (SELECT MIN(effect_event_id) FROM corrections WHERE effect_event_id IS NOT NULL);",
        );
        assert_missing_is_refused(
            &floor,
            "effect-bound agent examples",
            "DROP TRIGGER agent_examples_v60_effect_immutable_delete;
             DELETE FROM agent_examples
              WHERE effect_event_id = (SELECT MIN(effect_event_id) FROM agent_examples WHERE effect_event_id IS NOT NULL);",
        );
    }

    #[test]
    fn pristine_v60_state_is_not_activity_but_a_nonzero_frontier_is() {
        let pristine = crate::db::Database::open(":memory:").unwrap();
        pristine.initialize().unwrap();
        assert!(!has_durable_review_activity(&pristine).unwrap());

        pristine.connection().execute("DROP TRIGGER review_effect_state_immutable_update", []).unwrap();
        pristine
            .connection()
            .execute("UPDATE review_effect_state SET effective_after_review_event_id = 1", [])
            .unwrap();
        assert!(
            has_durable_review_activity(&pristine).unwrap(),
            "a captured pre-v60 frontier remains durable activity even if legacy rows are later missing"
        );
    }

    #[test]
    fn durable_restore_floor_protects_reviewed_segment_export_and_pay_identity_projection() {
        let base = crate::db::Database::open(":memory:").unwrap();
        base.initialize().unwrap();
        base.insert_segment(&test_segment("reviewed-1", "reviewed.wav", "machine draft")).unwrap();
        base.insert_segment(&test_segment("desktop-accept", "desktop.wav", "desktop draft")).unwrap();
        base.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?1,
                        audio_fingerprint = 123456,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}',
                        duration_ms = 1000,
                        human_decision = 'edit', verdict = 'human_corrected',
                        verdict_transcript = 'human truth', annotated_transcript = 'human truth',
                        verified = 1, reviewed_by = 'Reviewer',
                        corrected_at = '2026-08-22 00:00:00', escalated = 0
                  WHERE id = 'reviewed-1'",
                ["a".repeat(64)],
            )
            .unwrap();
        base.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?1, audio_fingerprint = 654321,
                        alignment_json = '{\"source_start_ms\":1000,\"source_end_ms\":2000}',
                        duration_ms = 1000, human_decision = 'accept',
                        verdict = 'human_verified', verdict_transcript = 'desktop truth',
                        annotated_transcript = 'desktop truth', verified = 1,
                        corrected_at = '2026-08-22 00:00:01', escalated = 0
                  WHERE id = 'desktop-accept'",
                ["b".repeat(64)],
            )
            .unwrap();
        base.connection()
            .execute(
                "INSERT INTO review_events
                    (id, segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                     compensation_action, created_at)
                 VALUES (1, 'reviewed-1', 'Reviewer', 'edit', 'legacy', 1, 1000, 'edit',
                         '2026-08-22 00:00:00')",
                [],
            )
            .unwrap();
        let floor = copied_database(&base);
        let equal = copied_database(&base);
        require_durable_review_history_superset(&floor, &equal).unwrap();

        let modified = copied_database(&base);
        modified
            .connection()
            .execute("UPDATE speech_segments SET duration_ms = 999 WHERE id = 'reviewed-1'", [])
            .unwrap();
        let error = require_durable_review_history_superset(&floor, &modified).unwrap_err();
        assert!(error.contains("reviewed speech-segment export projection"), "{error}");

        let missing = copied_database(&base);
        missing.connection().execute("DROP TRIGGER speech_segments_v60_review_authority_immutable_delete", []).unwrap();
        missing.delete_segment("reviewed-1").unwrap();
        let error = require_durable_review_history_superset(&floor, &missing).unwrap_err();
        assert!(error.contains("reviewed speech-segment export projection"), "{error}");

        let unaudited_desktop_regression = copied_database(&base);
        unaudited_desktop_regression
            .connection()
            .execute("DROP TRIGGER speech_segments_v60_review_authority_immutable_delete", [])
            .unwrap();
        unaudited_desktop_regression.delete_segment("desktop-accept").unwrap();
        let error = require_durable_review_history_superset(&floor, &unaudited_desktop_regression).unwrap_err();
        assert!(
            error.contains("reviewed speech-segment export projection"),
            "an unaudited desktop human decision must still be protected: {error}"
        );
    }

    #[test]
    fn staged_compensation_semantics_accept_writer_history_and_refuse_segment_deletion() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_canonical_pay_segment(&db, "pay-valid");
        record_canonical_skip(&db, "pay-valid", "Reviewer", 1);
        validate_review_compensation_semantics(&db).unwrap();

        let deletion = db.delete_segment("pay-valid").unwrap_err();
        assert!(matches!(deletion, crate::error::AppError::Validation(_)), "{deletion}");
        assert!(db.get_segment_by_id("pay-valid").unwrap().is_some());
        validate_review_compensation_semantics(&db)
            .expect("a refused deletion must preserve immutable pay/event snapshots and their clip");
    }

    #[test]
    fn staged_compensation_semantics_reject_forged_identity_source_and_entry_key() {
        let base = crate::db::Database::open(":memory:").unwrap();
        base.initialize().unwrap();
        insert_canonical_pay_segment(&base, "pay-forge");
        record_canonical_skip(&base, "pay-forge", "Reviewer", 2);

        let split_work = copied_database(&base);
        split_work.connection().execute("DROP TRIGGER review_compensation_ledger_immutable_update", []).unwrap();
        split_work
            .connection()
            .execute(
                "UPDATE review_compensation_ledger
                    SET canonical_work_id = ?1",
                [format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:0:1000", "c".repeat(64))],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&split_work).unwrap_err();
        assert!(error.contains("segment identity"), "forged work split must fail: {error}");

        let wrong_source = copied_database(&base);
        wrong_source.connection().execute("DROP TRIGGER review_compensation_ledger_immutable_update", []).unwrap();
        wrong_source.connection().execute("DROP TRIGGER review_events_v60_post_cutoff_immutable_update", []).unwrap();
        wrong_source.connection().execute("UPDATE review_events SET source = 'test'", []).unwrap();
        wrong_source.connection().execute("UPDATE review_compensation_ledger SET source = 'test'", []).unwrap();
        let error = validate_review_compensation_semantics(&wrong_source).unwrap_err();
        assert!(error.contains("production Couch action"), "nonproduction pay source must fail: {error}");

        let wrong_key = copied_database(&base);
        wrong_key.connection().execute("DROP TRIGGER review_compensation_ledger_immutable_update", []).unwrap();
        wrong_key
            .connection()
            .execute("UPDATE review_compensation_ledger SET entry_key = 'review-event:999'", [])
            .unwrap();
        let error = validate_review_compensation_semantics(&wrong_key).unwrap_err();
        assert!(error.contains("disagrees with review event"), "forged event key must fail: {error}");

        let duplicate_operation = crate::db::Database::open(":memory:").unwrap();
        duplicate_operation.initialize().unwrap();
        insert_canonical_pay_segment(&duplicate_operation, "duplicate-op-a");
        insert_canonical_pay_segment(&duplicate_operation, "duplicate-op-b");
        duplicate_operation.connection().execute("DROP INDEX idx_review_events_operation_id", []).unwrap();
        record_canonical_skip(&duplicate_operation, "duplicate-op-a", "Reviewer", 11);
        record_canonical_skip(&duplicate_operation, "duplicate-op-b", "Reviewer", 11);
        let error = validate_review_compensation_semantics(&duplicate_operation).unwrap_err();
        assert!(error.contains("unique canonical lowercase UUID"), "duplicate operation UUID must fail: {error}");
    }

    #[test]
    fn staged_compensation_semantics_validate_undo_linkage_and_settlement_math() {
        let undo_db = crate::db::Database::open(":memory:").unwrap();
        undo_db.initialize().unwrap();
        insert_canonical_pay_segment(&undo_db, "pay-undo");
        canonical_phone_playback(&undo_db, "pay-undo", "Reviewer");
        let served_revision = undo_db.segment_review_revision("pay-undo").unwrap().unwrap();
        let operation = canonical_operation(3);
        undo_db
            .record_phone_human_decision_by_at_revision_with_operation(
                "pay-undo",
                "reject",
                None,
                "Reviewer",
                served_revision,
                &operation,
                &crate::db::review_operation_payload_hash("pay-undo", "reject", "", "Reviewer"),
            )
            .unwrap()
            .unwrap();
        let effect_id = undo_db.human_decision_effect_for_operation(&operation).unwrap().unwrap().0;
        assert!(matches!(
            undo_db.undo_human_decision(effect_id, Some("Reviewer"), &operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        validate_review_compensation_semantics(&undo_db).unwrap();

        let wrong_undo = copied_database(&undo_db);
        wrong_undo.connection().execute("DROP TRIGGER review_compensation_ledger_immutable_update", []).unwrap();
        wrong_undo
            .connection()
            .execute(
                "UPDATE review_compensation_ledger SET entry_key = ?1
                  WHERE compensation_action = 'undo'",
                [format!("undo:{}", canonical_operation(4))],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&wrong_undo).unwrap_err();
        assert!(error.contains("operation/event linkage"), "wrong undo operation must fail: {error}");

        let settled = crate::db::Database::open(":memory:").unwrap();
        settled.initialize().unwrap();
        insert_canonical_pay_segment(&settled, "pay-settle");
        canonical_phone_playback(&settled, "pay-settle", "Reviewer");
        let revision = settled.segment_review_revision("pay-settle").unwrap().unwrap();
        settled
            .record_phone_human_decision_by_at_revision_with_operation(
                "pay-settle",
                "reject",
                None,
                "Reviewer",
                revision,
                &canonical_operation(5),
                &crate::db::review_operation_payload_hash("pay-settle", "reject", "", "Reviewer"),
            )
            .unwrap()
            .unwrap();
        let through: i64 = settled
            .connection()
            .query_row("SELECT MAX(id) FROM review_compensation_ledger", [], |row| row.get(0))
            .unwrap();
        settled.record_review_compensation_settlement("Reviewer", through, "payout-1").unwrap();
        validate_review_compensation_semantics(&settled).unwrap();

        let forged_settlement = copied_database(&settled);
        forged_settlement
            .connection()
            .execute("DROP TRIGGER review_compensation_settlement_immutable_update", [])
            .unwrap();
        forged_settlement
            .connection()
            .execute(
                "UPDATE review_compensation_settlements
                    SET allocated_micro_iqd = allocated_micro_iqd + 1",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&forged_settlement).unwrap_err();
        assert!(error.contains("amount differs"), "forged settlement amount must fail: {error}");
    }

    #[test]
    fn staged_playback_semantics_reject_no_listen_and_future_revision_receipts() {
        let valid = crate::db::Database::open(":memory:").unwrap();
        valid.initialize().unwrap();
        insert_canonical_pay_segment(&valid, "playback-valid");
        valid
            .record_playback_receipt(&crate::db::PlaybackReceipt {
                segment_id: "playback-valid".to_string(),
                segment_revision: 0,
                audio_content_hash: "f".repeat(64),
                reviewer: Some("Reviewer".to_string()),
                session_id: Some("session".to_string()),
                started_at_ms: 1,
                played_ms: 900,
                clip_duration_ms: 1000,
                source_start_ms: None,
                source_end_ms: None,
            })
            .unwrap();
        validate_playback_receipt_semantics(&valid).unwrap();

        // Speaker/quality metadata is allowed to advance the review revision after listening.  It
        // must not invalidate the immutable policy-3 audio identity the receipt actually proves.
        valid.set_speaker_change_score("playback-valid", 0.37).unwrap();
        validate_playback_receipt_semantics(&valid)
            .expect("an unrelated metadata revision bump must preserve exact policy-3 evidence");

        let mismatched_old_revision_hash = copied_database(&valid);
        mismatched_old_revision_hash
            .connection()
            .execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", [])
            .unwrap();
        mismatched_old_revision_hash
            .connection()
            .execute("UPDATE playback_receipts SET audio_fingerprint = ?1", ["b".repeat(64)])
            .unwrap();
        let error = validate_playback_receipt_semantics(&mismatched_old_revision_hash).unwrap_err();
        assert!(
            error.contains("retained segment identity"),
            "a metadata revision bump must not hide a forged historical BLAKE3 identity: {error}"
        );

        let wrong_span = copied_database(&valid);
        wrong_span.connection().execute("DROP TRIGGER speech_segments_review_revision", []).unwrap();
        wrong_span.connection().execute("DROP TRIGGER speech_segments_v60_paid_identity_immutable_update", []).unwrap();
        wrong_span
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET alignment_json = json_object(
                        'source_start_ms', 2000, 'source_end_ms', 3000,
                        'chunk_index', 0, 'chunk_count', 1
                    )
                  WHERE id = 'playback-valid'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&wrong_span).unwrap_err();
        assert!(
            error.contains("playback receipt") && error.contains("segment identity"),
            "same hash/revision/duration on a different source window must fail the actual restore gate: {error}"
        );

        let no_listen = copied_database(&valid);
        no_listen.connection().execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", []).unwrap();
        no_listen.connection().execute("UPDATE playback_receipts SET played_ms = 0, coverage_ratio = 1.0", []).unwrap();
        let error = validate_playback_receipt_semantics(&no_listen).unwrap_err();
        assert!(error.contains("writer invariants"), "forged no-listen receipt must fail: {error}");

        let future = copied_database(&valid);
        future.connection().execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", []).unwrap();
        future
            .connection()
            .execute(
                "UPDATE playback_receipts
                    SET segment_revision = (
                        SELECT review_revision + 1
                          FROM speech_segments
                         WHERE id = playback_receipts.segment_id
                    )",
                [],
            )
            .unwrap();
        let error = validate_playback_receipt_semantics(&future).unwrap_err();
        assert!(error.contains("future segment revision"), "pre-minted future receipt must fail: {error}");

        let no_content_hash = copied_database(&valid);
        no_content_hash
            .connection()
            .execute("DROP TRIGGER speech_segments_v60_paid_identity_immutable_update", [])
            .unwrap();
        no_content_hash
            .connection()
            .execute("UPDATE speech_segments SET audio_content_hash = NULL WHERE id = 'playback-valid'", [])
            .unwrap();
        let error = validate_playback_receipt_semantics(&no_content_hash).unwrap_err();
        assert!(
            error.contains("no canonical server-derived segment BLAKE3 identity"),
            "a restore must not invent audio identity from a segment id: {error}"
        );

        let legacy = copied_database(&valid);
        legacy.connection().execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", []).unwrap();
        legacy
            .connection()
            .execute(
                "UPDATE playback_receipts
                    SET policy_version = 1, audio_fingerprint = '424242',
                        source_start_ms = NULL, source_end_ms = NULL",
                [],
            )
            .unwrap();
        validate_playback_receipt_semantics(&legacy)
            .expect("policy-1 spectral receipts remain historical/readable but never authorize policy 3");
    }

    #[test]
    fn staged_review_effect_semantics_accept_every_current_writer_state() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        record_canonical_phone_edit(&db, "effect-active-edit", 101);

        insert_canonical_pay_segment(&db, "effect-undone-reject");
        let reject_revision = db.segment_review_revision("effect-undone-reject").unwrap().unwrap();
        canonical_phone_playback(&db, "effect-undone-reject", "Reviewer");
        let reject_operation = canonical_operation(102);
        db.record_phone_human_decision_by_at_revision_with_operation(
            "effect-undone-reject",
            "reject",
            None,
            "Reviewer",
            reject_revision,
            &reject_operation,
            &crate::db::review_operation_payload_hash("effect-undone-reject", "reject", "", "Reviewer"),
        )
        .unwrap()
        .unwrap();
        let reject_effect = db.human_decision_effect_for_operation(&reject_operation).unwrap().unwrap().0;
        assert!(matches!(
            db.undo_human_decision(reject_effect, Some("Reviewer"), &reject_operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));

        insert_canonical_pay_segment(&db, "effect-desktop-edit");
        db.finalize_human_review("effect-desktop-edit", "edit", Some("desktop truth"), None, None).unwrap();

        insert_canonical_pay_segment(&db, "effect-active-flag");
        db.record_review_flag("effect-active-flag", "needs another listen", "00000000-0000-4000-8000-000000000802")
            .unwrap();
        db.set_speaker_change_score("effect-active-edit", 0.41).unwrap();
        db.set_speaker_change_score("effect-active-flag", 0.42).unwrap();
        insert_canonical_pay_segment(&db, "effect-undone-flag");
        let undone_flag = db
            .record_review_flag("effect-undone-flag", "temporary concern", "00000000-0000-4000-8000-000000000803")
            .unwrap();
        db.set_speaker_change_score("effect-undone-flag", 0.43).unwrap();
        assert!(matches!(
            db.undo_review_flag(undone_flag.effect_event_id, &canonical_operation(103)).unwrap(),
            crate::db::HumanFlagUndoOutcome::Applied { .. }
        ));

        validate_restore_target_semantics(&db)
            .expect("every current phone/desktop/flag writer state must pass the actual restore gate");
    }

    #[test]
    fn staged_restore_rejects_orphaned_sequential_campaign_authority() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_campaign_registry
                    (campaign_id, focus_segment_count, focus_sha256, first_reviewer, second_reviewer,
                     after_review_event_id, activated_at_review_event_id)
                 VALUES('123e4567-e89b-42d3-a456-426614174000', 1, ?1, 'Rubar', 'Alle', 0, 0)",
                ["a".repeat(64)],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&db).unwrap_err();
        assert!(
            error.contains("campaign authority") && error.contains("without its base campaign policy"),
            "orphaned campaign authority must fail the actual restore gate: {error}"
        );
    }

    #[test]
    fn staged_restore_rejects_effect_correction_with_wrong_but_valid_audio_content_hash() {
        let base = crate::db::Database::open(":memory:").unwrap();
        base.initialize().unwrap();
        record_canonical_phone_edit(&base, "effect-wrong-content-hash", 104);
        validate_restore_target_semantics(&base).unwrap();

        let forged = copied_database(&base);
        forged.connection().execute("DROP TRIGGER corrections_v60_effect_immutable_update", []).unwrap();
        forged
            .connection()
            .execute(
                "UPDATE corrections SET audio_content_hash = ?1 WHERE effect_event_id IS NOT NULL",
                ["b".repeat(64)],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&forged).unwrap_err();
        assert!(
            error.contains("effect-bound correction") && error.contains("audio"),
            "a different canonical decoded-PCM BLAKE3 hash must fail the real restore gate: {error}"
        );
    }

    #[test]
    fn staged_restore_rejects_forged_effect_artifacts_current_state_and_inverse() {
        let active = crate::db::Database::open(":memory:").unwrap();
        active.initialize().unwrap();
        let (_effect_id, _operation) = record_canonical_phone_edit(&active, "effect-forgery", 105);
        validate_restore_target_semantics(&active).unwrap();

        let mismatched_example = copied_database(&active);
        mismatched_example.connection().execute("DROP TRIGGER agent_examples_v60_effect_immutable_update", []).unwrap();
        mismatched_example
            .connection()
            .execute("UPDATE agent_examples SET human_fix = 'forged truth' WHERE effect_event_id IS NOT NULL", [])
            .unwrap();
        let error = validate_restore_target_semantics(&mismatched_example).unwrap_err();
        assert!(error.contains("agent example"), "example/correction split must fail: {error}");

        let forged_memory = copied_database(&active);
        forged_memory.connection().execute("DROP TRIGGER correction_memory_v60_baseline_immutable_update", []).unwrap();
        forged_memory
            .connection()
            .execute("UPDATE correction_memory SET hit_count = 1 WHERE legacy_seed = 0", [])
            .unwrap();
        let error = validate_restore_target_semantics(&forged_memory).unwrap_err();
        assert!(error.contains("zero-baseline"), "mutable memory evidence must fail: {error}");

        let arbitrary_capture = copied_database(&active);
        let effect_id: i64 = arbitrary_capture
            .connection()
            .query_row("SELECT id FROM human_decision_effect_events WHERE segment_id = 'effect-forgery'", [], |row| {
                row.get(0)
            })
            .unwrap();
        arbitrary_capture
            .connection()
            .execute(
                "INSERT INTO correction_memory
                    (id, wrong_token, human_token, slot_key, phonetic_key, source_segment,
                     confidence, hit_count, confirm_count, override_count, legacy_seed)
                 VALUES ('00000000-0000-4000-8000-000000000806', 'forged-wrong',
                         'forged-fix', 'forged|slot', 'forged', 'effect-forgery',
                         0.5, 0, 0, 0, 0)",
                [],
            )
            .unwrap();
        arbitrary_capture
            .connection()
            .execute(
                "INSERT INTO correction_memory_contributions
                    (effect_event_id, memory_id, capture_delta, confirm_delta, override_delta)
                 VALUES (?1, '00000000-0000-4000-8000-000000000806', 1, 0, 0)",
                [effect_id],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&arbitrary_capture).unwrap_err();
        assert!(
            error.contains("arbitrary or incomplete correction-memory captures"),
            "an arbitrary effect-bound memory capture must fail: {error}"
        );

        let arbitrary_outcome = copied_database(&active);
        arbitrary_outcome
            .connection()
            .execute("DROP TRIGGER correction_memory_contributions_immutable_update", [])
            .unwrap();
        arbitrary_outcome
            .connection()
            .execute(
                "UPDATE correction_memory_contributions
                    SET confirm_delta = 1, fired_at = '2026-08-22 00:00:00'
                  WHERE capture_delta = 1",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&arbitrary_outcome).unwrap_err();
        assert!(
            error.contains("not re-derived from the served/decision text"),
            "a fabricated memory confirmation must fail: {error}"
        );

        let stale_current = copied_database(&active);
        stale_current
            .connection()
            .execute("UPDATE speech_segments SET verdict = 'human_reject' WHERE id = 'effect-forgery'", [])
            .unwrap();
        let error = validate_restore_target_semantics(&stale_current).unwrap_err();
        assert!(error.contains("latest active human-decision"), "forged current state must fail: {error}");

        let undone = crate::db::Database::open(":memory:").unwrap();
        undone.initialize().unwrap();
        let (effect_id, operation) = record_canonical_phone_edit(&undone, "effect-inverse", 106);
        assert!(matches!(
            undone.undo_human_decision(effect_id, Some("Reviewer"), &operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        validate_restore_target_semantics(&undone).unwrap();
        let forged_inverse = copied_database(&undone);
        forged_inverse
            .connection()
            .execute("DROP TRIGGER human_decision_effect_reversals_immutable_update", [])
            .unwrap();
        forged_inverse
            .connection()
            .execute("UPDATE human_decision_effect_reversals SET operation_id = ?1", [canonical_operation(107)])
            .unwrap();
        let error = validate_restore_target_semantics(&forged_inverse).unwrap_err();
        assert!(error.contains("operation-bound compensation inverse"), "wrong inverse identity must fail: {error}");
    }

    #[test]
    fn staged_restore_rejects_forged_flag_state_and_reversal_identity() {
        let active = crate::db::Database::open(":memory:").unwrap();
        active.initialize().unwrap();
        insert_canonical_pay_segment(&active, "flag-active-forgery");
        active
            .record_review_flag("flag-active-forgery", "listen again", "00000000-0000-4000-8000-000000000804")
            .unwrap();
        validate_restore_target_semantics(&active).unwrap();

        let laundered_human_truth = copied_database(&active);
        laundered_human_truth
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET verified = 1, annotated_transcript = 'forged unbound truth'
                  WHERE id = 'flag-active-forgery'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&laundered_human_truth).unwrap_err();
        assert!(
            error.contains("unsnapshotted human review truth")
                || error.contains("unbound human transcript/verification state"),
            "a flag effect must not launder unrelated verified/annotated truth: {error}"
        );

        active
            .connection()
            .execute("UPDATE speech_segments SET verdict = NULL WHERE id = 'flag-active-forgery'", [])
            .unwrap();
        let error = validate_restore_target_semantics(&active).unwrap_err();
        assert!(error.contains("latest active review-flag"), "forged active flag state must fail: {error}");

        let undone = crate::db::Database::open(":memory:").unwrap();
        undone.initialize().unwrap();
        insert_canonical_pay_segment(&undone, "flag-undo-forgery");
        let flag = undone
            .record_review_flag("flag-undo-forgery", "temporary flag", "00000000-0000-4000-8000-000000000805")
            .unwrap();
        let undo_operation = canonical_operation(110);
        assert!(matches!(
            undone.undo_review_flag(flag.effect_event_id, &undo_operation).unwrap(),
            crate::db::HumanFlagUndoOutcome::Applied { .. }
        ));
        validate_restore_target_semantics(&undone).unwrap();

        let wrong_identity = copied_database(&undone);
        wrong_identity.connection().execute("DROP TRIGGER review_flag_effect_reversals_immutable_update", []).unwrap();
        wrong_identity
            .connection()
            .execute("UPDATE review_flag_effect_reversals SET operation_id = 'not-a-uuid'", [])
            .unwrap();
        let error = validate_restore_target_semantics(&wrong_identity).unwrap_err();
        assert!(
            error.contains("review-flag effect") && error.contains("operation"),
            "forged flag undo must fail: {error}"
        );

        let stale_inverse = copied_database(&undone);
        stale_inverse
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET verdict = 'escalated', rationale = 'forged', escalated = 1
                  WHERE id = 'flag-undo-forgery'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&stale_inverse).unwrap_err();
        assert!(
            error.contains("review-flag reversal") || error.contains("exact mixed decision/flag effect chain"),
            "a stale flag after reversal must fail: {error}"
        );

        let legacy = crate::db::Database::open(":memory:").unwrap();
        legacy.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&legacy, 7).unwrap(), vec![66, 65, 64, 63, 62, 61, 60]);
        let mut legacy_segment = test_segment("flag-legacy-authority", "flag-legacy.wav", "machine draft");
        legacy_segment.verified = true;
        legacy_segment.annotated_transcript = Some("immutable legacy truth".into());
        legacy.insert_segment_full(&legacy_segment).unwrap();
        assert_eq!(crate::migrations::run_migrations(&legacy).unwrap(), vec![60, 61, 62, 63, 64, 65, 66]);
        legacy
            .record_review_flag("flag-legacy-authority", "legacy concern", "00000000-0000-4000-8000-000000000807")
            .unwrap();
        validate_restore_target_semantics(&legacy)
            .expect("an exact immutable pre-v60 reviewed baseline remains a valid first flag origin");
    }

    #[test]
    fn mixed_flag_decision_chains_preserve_exact_rationale_through_undo_and_restore() {
        let flag_then_decision = crate::db::Database::open(":memory:").unwrap();
        flag_then_decision.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&flag_then_decision, 7).unwrap(), vec![66, 65, 64, 63, 62, 61, 60]);
        insert_canonical_pay_segment(&flag_then_decision, "rationale-flag-decision");
        flag_then_decision
            .write_segment_verdict(
                "rationale-flag-decision",
                "jury_accept",
                Some("machine draft"),
                Some("machine rationale"),
                None,
                Some(0.9),
                false,
            )
            .unwrap();
        assert_eq!(crate::migrations::run_migrations(&flag_then_decision).unwrap(), vec![60, 61, 62, 63, 64, 65, 66]);
        flag_then_decision
            .record_review_flag("rationale-flag-decision", "flag rationale", "00000000-0000-4000-8000-000000000808")
            .unwrap();
        flag_then_decision.finalize_human_review("rationale-flag-decision", "accept", None, Some(1), None).unwrap();
        validate_restore_target_semantics(&flag_then_decision)
            .expect("flag-to-decision chain must retain the exact flag rationale");

        let forged_decision_prior = copied_database(&flag_then_decision);
        forged_decision_prior
            .connection()
            .execute("DROP TRIGGER human_decision_effect_events_immutable_update", [])
            .unwrap();
        forged_decision_prior
            .connection()
            .execute(
                "UPDATE human_decision_effect_events
                    SET prior_rationale = 'forged prior', decision_rationale = 'forged prior'
                  WHERE segment_id = 'rationale-flag-decision'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&forged_decision_prior).unwrap_err();
        assert!(error.contains("rationale"), "a decision must not invent the flag rationale it inherited: {error}");

        let decision_effect: i64 = flag_then_decision
            .connection()
            .query_row(
                "SELECT id FROM human_decision_effect_events
                  WHERE segment_id = 'rationale-flag-decision'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(matches!(
            flag_then_decision
                .undo_human_decision(decision_effect, None, "00000000-0000-4000-8000-000000000809")
                .unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        assert_eq!(
            flag_then_decision.get_segment_by_id("rationale-flag-decision").unwrap().unwrap().rationale.as_deref(),
            Some("flag rationale")
        );
        validate_restore_target_semantics(&flag_then_decision)
            .expect("decision Undo must restore the exact preceding active flag state");

        let decision_then_flag = crate::db::Database::open(":memory:").unwrap();
        decision_then_flag.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&decision_then_flag, 7).unwrap(), vec![66, 65, 64, 63, 62, 61, 60]);
        insert_canonical_pay_segment(&decision_then_flag, "rationale-decision-flag");
        decision_then_flag
            .write_segment_verdict(
                "rationale-decision-flag",
                "jury_accept",
                Some("machine draft"),
                Some("original rationale"),
                None,
                Some(0.9),
                false,
            )
            .unwrap();
        assert_eq!(crate::migrations::run_migrations(&decision_then_flag).unwrap(), vec![60, 61, 62, 63, 64, 65, 66]);
        decision_then_flag.finalize_human_review("rationale-decision-flag", "accept", None, Some(2), None).unwrap();
        let effect_id: i64 = decision_then_flag
            .connection()
            .query_row(
                "SELECT id FROM human_decision_effect_events
                  WHERE segment_id = 'rationale-decision-flag'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(matches!(
            decision_then_flag.undo_human_decision(effect_id, None, "00000000-0000-4000-8000-000000000810").unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        decision_then_flag
            .record_review_flag(
                "rationale-decision-flag",
                "later flag rationale",
                "00000000-0000-4000-8000-000000000811",
            )
            .unwrap();
        validate_restore_target_semantics(&decision_then_flag)
            .expect("a reversed decision followed by a flag must preserve the original rationale prior-state");

        let forged_flag_prior = copied_database(&decision_then_flag);
        forged_flag_prior.connection().execute("DROP TRIGGER review_flag_effect_events_immutable_update", []).unwrap();
        forged_flag_prior
            .connection()
            .execute(
                "UPDATE review_flag_effect_events
                    SET prior_rationale = 'forged prior'
                  WHERE segment_id = 'rationale-decision-flag'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&forged_flag_prior).unwrap_err();
        assert!(
            error.contains("rationale"),
            "a flag must not invent the rationale inherited from a decision/Undo chain: {error}"
        );
    }

    #[test]
    fn staged_review_effect_semantics_require_zero_effects_for_skip_and_spot_check() {
        for (source, action, operation_index) in [("couch", "skip", 108_u64), ("couch_spot_check", "edit", 109_u64)] {
            let db = crate::db::Database::open(":memory:").unwrap();
            db.initialize().unwrap();
            insert_canonical_pay_segment(&db, &format!("zero-effect-{source}"));
            let segment_id = format!("zero-effect-{source}");
            if source == "couch_spot_check" {
                db.record_spot_check(&segment_id, "Reviewer", action, "expected", "expected").unwrap();
                validate_review_effect_semantics(&db).unwrap();
            } else {
                db.record_review_event_with_operation(
                    &segment_id,
                    "Reviewer",
                    action,
                    source,
                    i64::try_from(operation_index).unwrap(),
                    &canonical_operation(operation_index),
                    &crate::db::review_operation_payload_hash(&segment_id, action, "", "Reviewer"),
                )
                .unwrap();
                validate_restore_target_semantics(&db).unwrap();
            }

            let event_id: i64 =
                db.connection().query_row("SELECT MAX(id) FROM review_events", [], |row| row.get(0)).unwrap();
            let prior_revision = db.segment_review_revision(&segment_id).unwrap().unwrap();
            db.connection()
                .execute("DROP TRIGGER human_decision_effect_events_validate_review_event_insert", [])
                .unwrap();
            db.connection()
                .execute(
                    "INSERT INTO human_decision_effect_events
                        (review_event_id, segment_id, reviewer, source, action,
                         served_transcript, decision_transcript, decision_annotated_transcript,
                         decision_verified, decision_corrected_at,
                         prior_revision, decision_revision, prior_verified,
                         prior_escalated)
                     VALUES (?1, ?2, 'Reviewer', 'couch', 'edit',
                             'served transcript', 'forged edit', 'forged edit', 1,
                             '2026-08-22 00:00:00',
                             ?3, ?3 + 1, 0, 0)",
                    rusqlite::params![event_id, segment_id, prior_revision],
                )
                .unwrap();
            let error = validate_review_effect_semantics(&db).unwrap_err();
            assert!(
                error.contains("must not create a human-decision effect"),
                "{source}/{action} forged effect must fail: {error}"
            );
        }
    }

    #[test]
    fn staged_restore_rejects_current_human_truth_without_legacy_or_effect_authority() {
        for (segment_id, reviewed_by) in
            [("forged-unbound-desktop", None), ("forged-unrostered-reviewer", Some("Mallory"))]
        {
            let db = crate::db::Database::open(":memory:").unwrap();
            db.initialize().unwrap();
            insert_canonical_pay_segment(&db, segment_id);
            db.connection()
                .execute(
                    "UPDATE speech_segments
                        SET human_decision = 'accept', verdict = 'human_accept',
                            verdict_transcript = raw_transcript,
                            annotated_transcript = raw_transcript, verified = 1,
                            reviewed_by = ?2, corrected_at = datetime('now')
                      WHERE id = ?1",
                    rusqlite::params![segment_id, reviewed_by],
                )
                .unwrap();
            let error = validate_restore_target_semantics(&db).unwrap_err();
            assert!(
                error.contains("neither immutable legacy authority nor a schema-v60 effect chain"),
                "unbound current human truth must fail for {segment_id}: {error}"
            );
        }
    }

    #[test]
    fn schema_v60_machine_only_dataset_merge_remains_restore_safe() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let first = vec![crate::db::SpeechSegment {
            id: "restore-machine-merge".to_string(),
            created_at: Some("2026-08-22 01:02:03".to_string()),
            audio_path: "restore-machine-merge.wav".to_string(),
            raw_transcript: "machine draft one".to_string(),
            normalized_transcript: Some("machine normalized one".to_string()),
            duration_ms: 1_000,
            confidence: Some(0.7),
            model_version_id: Some("omniasr-7b-champion".to_string()),
            confidence_source: Some("real_posterior".to_string()),
            ..Default::default()
        }];
        assert_eq!(db.merge_dataset_json(&serde_json::to_string(&first).unwrap()).unwrap(), (1, 0));
        validate_restore_target_semantics(&db).expect("machine-only inserted row is valid restore material");

        let replacement = vec![crate::db::SpeechSegment {
            id: "restore-machine-merge".to_string(),
            audio_path: "restore-machine-merge.wav".to_string(),
            raw_transcript: "machine draft two".to_string(),
            normalized_transcript: Some("machine normalized two".to_string()),
            duration_ms: 1_000,
            confidence: Some(0.8),
            model_version_id: Some("omniasr-7b-champion".to_string()),
            confidence_source: Some("real_posterior".to_string()),
            ..Default::default()
        }];
        assert_eq!(db.merge_dataset_json(&serde_json::to_string(&replacement).unwrap()).unwrap(), (0, 1));
        validate_restore_target_semantics(&db).expect("machine-only updated row remains valid restore material");
        let row = db.get_segment_by_id("restore-machine-merge").unwrap().unwrap();
        assert_eq!(row.raw_transcript, "machine draft two");
        assert!(row.annotated_transcript.is_none() && !row.verified && row.human_decision.is_none());
    }

    #[test]
    fn generic_machine_history_after_human_effect_preserves_exact_review_state_and_restore() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_canonical_pay_segment(&db, "restore-history-review");
        let original = db.get_segment_by_id("restore-history-review").unwrap().unwrap();
        let updated = crate::db::SpeechSegment {
            raw_transcript: "machine draft two".to_string(),
            speaker_id: Some("speaker-a".to_string()),
            ..original.clone()
        };
        db.insert_segment(&updated).unwrap();
        let history = crate::history::HistoryManager::new(10);
        history.record_segment_update(original, updated);

        db.finalize_human_review("restore-history-review", "accept", Some("machine draft two"), Some(123), None)
            .unwrap();
        let reviewed = db.get_segment_by_id("restore-history-review").unwrap().unwrap();
        validate_restore_target_semantics(&db).expect("decision state is restore-safe before generic history");

        history.undo(&db).expect("generic machine undo");
        let undone = db.get_segment_by_id("restore-history-review").unwrap().unwrap();
        assert_eq!(undone.raw_transcript, "machine draft");
        assert_eq!(undone.annotated_transcript, reviewed.annotated_transcript);
        assert_eq!(undone.verified, reviewed.verified);
        assert_eq!(undone.human_decision, reviewed.human_decision);
        assert_eq!(undone.verdict, reviewed.verdict);
        assert_eq!(undone.rationale, reviewed.rationale);
        assert_eq!(undone.reviewed_by, reviewed.reviewed_by);
        validate_restore_target_semantics(&db).expect("machine undo must preserve a valid exact effect graph");

        history.redo(&db).expect("generic machine redo");
        let redone = db.get_segment_by_id("restore-history-review").unwrap().unwrap();
        assert_eq!(redone.raw_transcript, "machine draft two");
        assert_eq!(redone.annotated_transcript, reviewed.annotated_transcript);
        assert_eq!(redone.verified, reviewed.verified);
        assert_eq!(redone.human_decision, reviewed.human_decision);
        assert_eq!(redone.verdict, reviewed.verdict);
        assert_eq!(redone.rationale, reviewed.rationale);
        assert_eq!(redone.reviewed_by, reviewed.reviewed_by);
        validate_restore_target_semantics(&db).expect("machine redo must preserve a valid exact effect graph");
    }

    #[test]
    fn staged_restore_rejects_a_forged_undo_of_a_shadowed_canonical_alias() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        insert_canonical_pay_segment(&db, "restore-alias-a");
        let proof_a = canonical_phone_playback(&db, "restore-alias-a", "Reviewer");
        let operation_a = canonical_operation(120);
        let commit_a = db
            .record_phone_human_decision_by_at_revision_with_operation_limit(
                "restore-alias-a",
                "accept",
                Some("machine draft"),
                "Reviewer",
                proof_a.segment_revision,
                &proof_a,
                &operation_a,
                &crate::db::review_operation_payload_hash("restore-alias-a", "accept", "machine draft", "Reviewer"),
                "accept",
                "machine draft",
                None,
            )
            .unwrap()
            .unwrap();

        let (_effect_b, _operation_b) = record_canonical_phone_edit(&db, "restore-alias-b", 121);
        validate_restore_target_semantics(&db).unwrap();

        // Model a trigger-disabled staged target that tries to retract A after B became the latest
        // entitlement mutation for the same reviewer/BLAKE3/source-span work identity.
        db.connection()
            .execute(
                "INSERT INTO review_compensation_ledger
                    (entry_id, entry_key, policy_version, canonical_work_id,
                     canonical_identity_kind, reviewer, segment_id, source,
                     compensation_action, effective_decision, decision_revision, duration_ms,
                     rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                     corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id)
                 SELECT '00000000-0000-4000-8000-000000000900',
                        'undo:' || ?2, original.policy_version, original.canonical_work_id,
                        original.canonical_identity_kind, original.reviewer,
                        original.segment_id, 'couch_undo', 'undo', 'undo',
                        original.decision_revision, original.duration_ms, 0, 0,
                        -original.delta_micro_iqd,
                        (SELECT COALESCE(SUM(delta_corrected_ms), 0)
                           FROM review_compensation_ledger
                          WHERE canonical_work_id = original.canonical_work_id)
                            - original.delta_corrected_ms,
                        -original.delta_corrected_ms, original.entry_id
                   FROM review_compensation_ledger original
                   JOIN human_decision_effect_events effect
                     ON effect.review_event_id = original.review_event_id
                  WHERE effect.id = ?1 AND original.reverses_entry_id IS NULL",
                rusqlite::params![commit_a.effect_event_id, operation_a],
            )
            .unwrap();
        db.connection()
            .execute(
                "INSERT INTO human_decision_effect_reversals (effect_event_id, operation_id)
                 VALUES (?1, ?2)",
                rusqlite::params![commit_a.effect_event_id, operation_a],
            )
            .unwrap();

        let error = validate_restore_target_semantics(&db).unwrap_err();
        assert!(
            error.contains("does not exactly bind its earlier decision entry"),
            "a shadowed alias reversal must fail the actual restore gate: {error}"
        );
    }

    #[test]
    fn staged_restore_exact_schema_contract_rejects_weakened_hidden_key_trigger() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("weakened-hidden-trigger.db");
        let candidate = crate::db::Database::open(path.to_string_lossy().as_ref()).unwrap();
        candidate.initialize().unwrap();
        candidate
            .connection()
            .execute_batch(
                "DROP TRIGGER review_pilot_hidden_keys_quota_insert;
                 CREATE TRIGGER review_pilot_hidden_keys_quota_insert
                 BEFORE INSERT ON review_pilot_hidden_keys
                 BEGIN SELECT 1; END;",
            )
            .unwrap();
        candidate.wal_checkpoint().unwrap();
        drop(candidate);

        let error = match crate::db::Database::stage_restore_source(&path) {
            Ok(_) => panic!("weakened hidden-key trigger must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("changed=") && error.contains("review_pilot_hidden_keys_quota_insert"),
            "staged restore must compare exact sqlite_schema SQL for hidden-key authority: {error}"
        );
    }

    #[test]
    fn staged_target_hidden_namespaces_and_used_policy_identity_are_fail_closed() {
        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        let policy = test_pilot_policy(0, "ReviewerA", "ReviewerB");
        let digest = policy.policy_sha256().unwrap();

        let valid = crate::db::Database::open(":memory:").unwrap();
        valid.initialize().unwrap();
        valid
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'reviewera', 'valid-1')", [&digest])
            .unwrap();
        require_active_pilot_policy_binding(&floor, None, &valid, &pilot_restore_action(&policy)).unwrap();

        let wrong_sha = crate::db::Database::open(":memory:").unwrap();
        wrong_sha.initialize().unwrap();
        wrong_sha
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'wrong-sha')", ["b".repeat(64)])
            .unwrap();
        let error =
            require_active_pilot_policy_binding(&floor, None, &wrong_sha, &pilot_restore_action(&policy)).unwrap_err();
        assert!(error.contains("SHA/baseline"), "{error}");

        let wrong_baseline = crate::db::Database::open(":memory:").unwrap();
        wrong_baseline.initialize().unwrap();
        wrong_baseline
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 1, 'ReviewerA', 'wrong-baseline')", [&digest])
            .unwrap();
        let error = require_active_pilot_policy_binding(&floor, None, &wrong_baseline, &pilot_restore_action(&policy))
            .unwrap_err();
        assert!(error.contains("SHA/baseline"), "{error}");

        let unauthorized = crate::db::Database::open(":memory:").unwrap();
        unauthorized.initialize().unwrap();
        unauthorized
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerC', 'unauthorized')", [&digest])
            .unwrap();
        let error = require_active_pilot_policy_binding(&floor, None, &unauthorized, &pilot_restore_action(&policy))
            .unwrap_err();
        assert!(error.contains("exact policy roster"), "{error}");

        let over_quota = crate::db::Database::open(":memory:").unwrap();
        over_quota.initialize().unwrap();
        over_quota.connection().execute("DROP TRIGGER review_pilot_hidden_keys_quota_insert", []).unwrap();
        for segment_id in ["over-1", "over-2", "over-3"] {
            over_quota
                .connection()
                .execute(
                    "INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', ?2)",
                    rusqlite::params![&digest, segment_id],
                )
                .unwrap();
        }
        let error =
            require_active_pilot_policy_binding(&floor, None, &over_quota, &pilot_restore_action(&policy)).unwrap_err();
        assert!(error.contains("structural") || error.contains("quota"), "{error}");

        let historical_conflict = crate::db::Database::open(":memory:").unwrap();
        historical_conflict.initialize().unwrap();
        historical_conflict.connection().execute("DROP TRIGGER review_pilot_hidden_keys_policy_insert", []).unwrap();
        historical_conflict
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 17, 'PastReviewer', 'past-1')", ["c".repeat(64)])
            .unwrap();
        historical_conflict
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 17, 'PastReviewer', 'past-2')", ["d".repeat(64)])
            .unwrap();
        let error = require_active_pilot_policy_binding(
            &floor,
            None,
            &historical_conflict,
            &SnapshotPilotPolicyRestore::ExplicitlyAbsent,
        )
        .unwrap_err();
        assert!(error.contains("one-policy-per-baseline"), "{error}");

        let used_floor = crate::db::Database::open(":memory:").unwrap();
        used_floor.initialize().unwrap();
        insert_test_review_event(&used_floor, "pilot-work", "ReviewerA", "skip", "couch", 1);
        let used_target = copied_database(&used_floor);
        require_active_pilot_policy_binding(&used_floor, Some(&policy), &used_target, &pilot_restore_action(&policy))
            .unwrap();
        let replacement = test_pilot_policy(0, "ReviewerA", "ReviewerC");
        let error = require_active_pilot_policy_binding(
            &used_floor,
            Some(&policy),
            &used_target,
            &pilot_restore_action(&replacement),
        )
        .unwrap_err();
        assert!(error.contains("identity differs"), "{error}");
    }

    #[test]
    fn staged_target_active_pilot_semantics_reject_corrupt_extras_and_keep_legacy_hidden_skip() {
        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        let policy = test_pilot_policy(0, "ReviewerA", "ReviewerB");
        let digest = policy.policy_sha256().unwrap();
        let action = pilot_restore_action(&policy);
        let validate =
            |target: &crate::db::Database| require_active_pilot_policy_binding(&floor, None, target, &action);

        let legacy_skip = crate::db::Database::open(":memory:").unwrap();
        legacy_skip.initialize().unwrap();
        legacy_skip
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'legacy-hidden')", [&digest])
            .unwrap();
        insert_test_review_event(&legacy_skip, "legacy-hidden", "ReviewerA", "skip", "couch", 1);
        validate(&legacy_skip).unwrap();

        let valid_hidden = crate::db::Database::open(":memory:").unwrap();
        valid_hidden.initialize().unwrap();
        valid_hidden
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-ok')", [&digest])
            .unwrap();
        insert_test_review_event(&valid_hidden, "hidden-ok", "ReviewerA", "edit", "couch_spot_check", 1);
        insert_test_spot_result(&valid_hidden, "hidden-ok", "reviewera", "edit");
        validate(&valid_hidden).unwrap();

        let unauthorized = crate::db::Database::open(":memory:").unwrap();
        unauthorized.initialize().unwrap();
        insert_test_review_event(&unauthorized, "work", "Intruder", "skip", "couch", 1);
        let error = validate(&unauthorized).unwrap_err();
        assert!(error.contains("unauthorized reviewer"), "{error}");

        let invalid_action = crate::db::Database::open(":memory:").unwrap();
        invalid_action.initialize().unwrap();
        insert_test_review_event(&invalid_action, "work", "ReviewerA", "approve", "couch", 1);
        let error = validate(&invalid_action).unwrap_err();
        assert!(error.contains("invalid action"), "{error}");

        let ungranted_hidden = crate::db::Database::open(":memory:").unwrap();
        ungranted_hidden.initialize().unwrap();
        insert_test_review_event(&ungranted_hidden, "not-granted", "ReviewerA", "edit", "couch_spot_check", 1);
        let error = validate(&ungranted_hidden).unwrap_err();
        assert!(error.contains("no active durable grant"), "{error}");

        let corpus_finalized_hidden = crate::db::Database::open(":memory:").unwrap();
        corpus_finalized_hidden.initialize().unwrap();
        corpus_finalized_hidden
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-corpus')", [&digest])
            .unwrap();
        insert_test_review_event(&corpus_finalized_hidden, "hidden-corpus", "ReviewerA", "accept", "couch", 1);
        let error = validate(&corpus_finalized_hidden).unwrap_err();
        assert!(error.contains("non-skip finalized"), "{error}");

        let duplicate_resolution = crate::db::Database::open(":memory:").unwrap();
        duplicate_resolution.initialize().unwrap();
        duplicate_resolution
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-duplicate')", [&digest])
            .unwrap();
        insert_test_review_event(&duplicate_resolution, "hidden-duplicate", "ReviewerA", "edit", "couch_spot_check", 1);
        insert_test_review_event(&duplicate_resolution, "hidden-duplicate", "ReviewerA", "edit", "couch_spot_check", 2);
        insert_test_spot_result(&duplicate_resolution, "hidden-duplicate", "ReviewerA", "edit");
        let error = validate(&duplicate_resolution).unwrap_err();
        assert!(error.contains("resolved more than once"), "{error}");

        let mismatched_result = crate::db::Database::open(":memory:").unwrap();
        mismatched_result.initialize().unwrap();
        mismatched_result
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-mismatch')", [&digest])
            .unwrap();
        insert_test_review_event(&mismatched_result, "hidden-mismatch", "ReviewerA", "edit", "couch_spot_check", 1);
        insert_test_spot_result(&mismatched_result, "hidden-mismatch", "ReviewerA", "accept");
        let error = validate(&mismatched_result).unwrap_err();
        assert!(error.contains("event/result actions"), "{error}");

        let impossible_score = crate::db::Database::open(":memory:").unwrap();
        impossible_score.initialize().unwrap();
        impossible_score
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-impossible')", [&digest])
            .unwrap();
        insert_test_review_event(&impossible_score, "hidden-impossible", "ReviewerA", "edit", "couch_spot_check", 1);
        insert_test_spot_result(&impossible_score, "hidden-impossible", "ReviewerA", "edit");
        impossible_score
            .connection()
            .execute(
                "UPDATE spot_checks SET submitted_transcript = 'different', noticed = 1, cer = 0.0
                  WHERE segment_id = 'hidden-impossible'",
                [],
            )
            .unwrap();
        let error = validate(&impossible_score).unwrap_err();
        assert!(error.contains("impossible noticed/CER"), "{error}");

        let orphan_result = crate::db::Database::open(":memory:").unwrap();
        orphan_result.initialize().unwrap();
        orphan_result
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-orphan')", [&digest])
            .unwrap();
        insert_test_spot_result(&orphan_result, "hidden-orphan", "ReviewerA", "edit");
        let error = validate(&orphan_result).unwrap_err();
        assert!(error.contains("orphan hidden-check result"), "{error}");

        let over_corpus_cap = crate::db::Database::open(":memory:").unwrap();
        over_corpus_cap.initialize().unwrap();
        for index in 0..=crate::review_pilot::REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER {
            insert_test_review_event(&over_corpus_cap, &format!("work-{index}"), "ReviewerA", "skip", "couch", index);
        }
        let error = validate(&over_corpus_cap).unwrap_err();
        assert!(error.contains("per-reviewer corpus-action ceiling"), "{error}");

        let half_written = crate::db::Database::open(":memory:").unwrap();
        half_written.initialize().unwrap();
        half_written.insert_segment(&test_segment("half-written", "half.wav", "draft")).unwrap();
        half_written
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET human_decision = 'accept', reviewed_by = 'ReviewerA', verified = 1
                  WHERE id = 'half-written'",
                [],
            )
            .unwrap();
        let error = validate(&half_written).unwrap_err();
        assert!(error.contains("no matching active campaign event/ledger"), "{error}");

        let missing_current_state = crate::db::Database::open(":memory:").unwrap();
        missing_current_state.initialize().unwrap();
        insert_canonical_pay_segment(&missing_current_state, "event-without-state");
        canonical_phone_playback(&missing_current_state, "event-without-state", "ReviewerA");
        let revision = missing_current_state.segment_review_revision("event-without-state").unwrap().unwrap();
        missing_current_state
            .record_phone_human_decision_by_at_revision_with_operation(
                "event-without-state",
                "reject",
                None,
                "ReviewerA",
                revision,
                &canonical_operation(20),
                &crate::db::review_operation_payload_hash("event-without-state", "reject", "", "ReviewerA"),
            )
            .unwrap()
            .unwrap();
        // Model a self-consistent staged file whose event+ledger survived but whose same-revision
        // corpus state did not. Recreate the exact trigger so only semantics—not schema drift—refuse.
        missing_current_state.connection().execute("DROP TRIGGER speech_segments_review_revision", []).unwrap();
        missing_current_state
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET human_decision = NULL, reviewed_by = NULL, verified = 0
                  WHERE id = 'event-without-state'",
                [],
            )
            .unwrap();
        missing_current_state
            .connection()
            .execute_batch(
                "CREATE TRIGGER speech_segments_review_revision
                 AFTER UPDATE ON speech_segments
                 WHEN new.review_revision = old.review_revision
                 BEGIN
                     UPDATE speech_segments
                     SET review_revision = old.review_revision + 1
                     WHERE id = old.id;
                 END;",
            )
            .unwrap();
        let error = validate(&missing_current_state).unwrap_err();
        assert!(error.contains("no matching current-revision segment state"), "{error}");

        let pre_pilot = crate::db::Database::open(":memory:").unwrap();
        pre_pilot.initialize().unwrap();
        insert_canonical_pay_segment(&pre_pilot, "pre-pilot-state");
        canonical_phone_playback(&pre_pilot, "pre-pilot-state", "ReviewerA");
        let revision = pre_pilot.segment_review_revision("pre-pilot-state").unwrap().unwrap();
        pre_pilot
            .record_phone_human_decision_by_at_revision_with_operation(
                "pre-pilot-state",
                "reject",
                None,
                "ReviewerA",
                revision,
                &canonical_operation(21),
                &crate::db::review_operation_payload_hash("pre-pilot-state", "reject", "", "ReviewerA"),
            )
            .unwrap()
            .unwrap();
        let pre_pilot_policy = test_pilot_policy(1, "ReviewerA", "ReviewerB");
        validate_active_pilot_semantics(&pre_pilot, &pre_pilot_policy, "pre-pilot regression")
            .expect("an exact same-reviewer decision at the baseline is legitimate prior state");

        let forged_after_undo = crate::db::Database::open(":memory:").unwrap();
        forged_after_undo.initialize().unwrap();
        insert_canonical_pay_segment(&forged_after_undo, "forged-after-undo");
        canonical_phone_playback(&forged_after_undo, "forged-after-undo", "ReviewerA");
        let revision = forged_after_undo.segment_review_revision("forged-after-undo").unwrap().unwrap();
        let operation = canonical_operation(22);
        forged_after_undo
            .record_phone_human_decision_by_at_revision_with_operation(
                "forged-after-undo",
                "reject",
                None,
                "ReviewerA",
                revision,
                &operation,
                &crate::db::review_operation_payload_hash("forged-after-undo", "reject", "", "ReviewerA"),
            )
            .unwrap()
            .unwrap();
        let effect_id = forged_after_undo.human_decision_effect_for_operation(&operation).unwrap().unwrap().0;
        assert!(matches!(
            forged_after_undo.undo_human_decision(effect_id, Some("ReviewerA"), &operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        forged_after_undo.connection().execute("DROP TRIGGER speech_segments_review_revision", []).unwrap();
        forged_after_undo
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET human_decision = 'accept', reviewed_by = 'ReviewerA', verified = 1
                  WHERE id = 'forged-after-undo'",
                [],
            )
            .unwrap();
        forged_after_undo
            .connection()
            .execute_batch(
                "CREATE TRIGGER speech_segments_review_revision
                 AFTER UPDATE ON speech_segments
                 WHEN new.review_revision = old.review_revision
                 BEGIN
                     UPDATE speech_segments
                     SET review_revision = old.review_revision + 1
                     WHERE id = old.id;
                 END;",
            )
            .unwrap();
        let error = validate(&forged_after_undo).unwrap_err();
        assert!(error.contains("no matching active campaign event/ledger"), "{error}");
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

        // Derive the source from live so immutable compensation-policy provenance (including its
        // database-owned timestamp) is truly the same generation; independent initialize() calls can
        // straddle a one-second clock boundary and are not a legitimate restore-superset fixture.
        live.backup(&source_path).unwrap();
        let source = crate::db::Database::open(source_path.to_string_lossy().as_ref()).unwrap();
        source.delete_segment("old-live").unwrap();
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
    fn bare_restore_refuses_durable_review_activity_before_pin_or_swap() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("app-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let live_path = temp.path().join("live.db");
        let source_path = temp.path().join("source.db");
        let mut live = crate::db::Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "reviewed");
        live.backup(&source_path).unwrap();
        live.record_review_event("reviewed", "Reviewer", "skip", "test", 1).unwrap();

        let error = restore_with_mandatory_snapshot(&reservation, &mut live, &data_dir, &source_path).unwrap_err();
        assert!(error.contains("Bare database restore is refused"), "{error}");
        let events: i64 =
            live.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(events, 1, "the live audit row must remain in place");
        assert!(!data_dir.join("snapshots").join("pinned").exists(), "refusal must happen before safety-pin I/O");
    }

    #[test]
    fn bare_restore_refuses_target_only_durable_review_activity_before_pin_or_swap() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("app-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let live_path = temp.path().join("live.db");
        let source_path = temp.path().join("source.db");
        let mut live = crate::db::Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "target-reviewed");
        live.backup(&source_path).unwrap();
        let target = crate::db::Database::open(source_path.to_string_lossy().as_ref()).unwrap();
        target.record_review_event("target-reviewed", "Reviewer", "skip", "test", 1).unwrap();
        target.wal_checkpoint().unwrap();
        drop(target);

        let error = restore_with_mandatory_snapshot(&reservation, &mut live, &data_dir, &source_path).unwrap_err();
        assert!(error.contains("either the live or target generation"), "{error}");
        let live_events: i64 =
            live.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(live_events, 0, "target-only audit history must not be imported through the bare path");
        assert!(!data_dir.join("snapshots").join("pinned").exists());
    }

    #[test]
    fn named_restore_rejects_missing_durable_rows_before_pin_marker_or_swap() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        let mut live = crate::db::Database::open(":memory:").unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "reviewed");
        let source_dir = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 1000).unwrap().unwrap();
        let source = source_dir.join("cortex-speech.db");
        live.record_review_event("reviewed", "Reviewer", "skip", "test", 1).unwrap();

        let error = prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &source_dir,
            &source,
            "snapshot_0000001000",
        )
        .unwrap_err();
        assert!(error.contains("review_events"), "{error}");
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let pins = data_dir.join("snapshots").join("pinned");
        assert!(!pins.exists() || std::fs::read_dir(pins).unwrap().next().is_none());
        let events: i64 =
            live.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(events, 1, "the live generation must not be swapped on floor regression");
    }

    #[test]
    fn named_restore_rejects_target_only_half_written_pilot_state_before_pin_or_marker() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        let mut live = crate::db::Database::open(":memory:").unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("half-target", "half.wav", "draft")).unwrap();

        let target = copied_database(&live);
        target
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET human_decision = 'accept', reviewed_by = 'Hawzhin', verified = 1
                  WHERE id = 'half-target'",
                [],
            )
            .unwrap();
        // Snapshot creation must keep enforcing the real controlled-pilot authority contract. Use
        // its exact roster so this fixture passes snapshot admission and reaches the independent
        // named-restore semantic gate for the deliberately half-written corpus row below.
        let policy = test_pilot_policy(0, "Hawzhin", "Pavel");
        crate::review_pilot::install_test_focus(data_dir, ["half-target"]);
        std::fs::write(
            data_dir.join(crate::review_pilot::REVIEW_PILOT_FILE),
            serde_json::to_vec_pretty(&policy).unwrap(),
        )
        .unwrap();
        let snapshot = crate::snapshot::take_snapshot_at(&target, data_dir, 5, 6000).unwrap().unwrap();
        let source = snapshot.join("cortex-speech.db");
        let selector = snapshot.file_name().unwrap().to_string_lossy().to_string();

        let error =
            prepare_and_restore_named_transaction(&reservation, &mut live, data_dir, &snapshot, &source, &selector)
                .unwrap_err();
        assert!(error.contains("no matching active campaign event/ledger"), "{error}");
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let pins = data_dir.join("snapshots").join("pinned");
        assert!(!pins.exists() || std::fs::read_dir(pins).unwrap().next().is_none());
        let unchanged = live.get_segment_by_id("half-target").unwrap().unwrap();
        assert!(unchanged.human_decision.is_none() && unchanged.reviewed_by.is_none());
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
    fn interrupted_recovery_uses_original_pin_floor_not_possibly_swapped_live() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let mut live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "original");
        live.insert_segment(&test_segment("target-only", "target.wav", "target generation")).unwrap();
        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 2000).unwrap().unwrap();

        live.delete_segment("target-only").unwrap();
        record_canonical_skip(&live, "original", "Reviewer", 30);
        let original_pin = crate::snapshot::take_pinned_snapshot_at(&live, data_dir, "history-floor", 3, 3000).unwrap();

        // Model a crash after target page publication: live now lacks the original floor's audit/pay
        // rows. A broken implementation that compares against live would accept target again.
        let staged_target = crate::db::Database::stage_restore_source(target.join("cortex-speech.db")).unwrap();
        live.commit_staged_restore(&staged_target).unwrap();
        drop(staged_target);
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

        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        let events: i64 = recovered
            .connection()
            .query_row("SELECT COUNT(*) FROM review_events WHERE segment_id = 'original'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(events, 1, "fallback must restore the original pin's durable review floor");
        assert!(recovered.get_segment_by_id("original").unwrap().is_some());
        assert!(
            recovered.get_segment_by_id("target-only").unwrap().is_none(),
            "the regressive target must be rejected and the original floor published"
        );
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
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
        let db = legacy_machine_db();
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
    fn current_schema_retires_machine_jury_even_for_an_auxiliary_diagnostic_selection() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let segment = test_segment("current-schema-no-jury", "/audio/diagnostic.wav", "diagnostic draft");
        db.insert_segment(&segment).unwrap();

        let settings = crate::settings::AppSettings {
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            multi_engine_hypotheses: true,
            jury_cloud_opt_in: true,
            llm_api_key: "must-not-be-used".to_string(),
            ..crate::settings::AppSettings::default()
        };
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["mode"], "not_required");
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert!(
            report["reason"].as_str().unwrap_or("").contains("Schema v60+"),
            "the handoff must explain the current trust boundary: {report}"
        );
        assert!(fresh.verdict.is_none(), "the retired machine jury must not author review truth");
        assert!(!fresh.escalated, "the retired machine jury must not rewrite queue state");
    }

    #[test]
    fn autonomy_dial_governs_every_machine_commit_stage_not_just_t0() {
        // Round-24 hunt #1 (HIGH): under Observe/Propose the dial was enforced only inside
        // run_t0_gate — the SAME pipeline run then machine-committed 'jury_accept' via the
        // reference-selection stage (and T2), silently removing from the human queue the very
        // segments the dial promised to stage.

        // ── Propose: a committable reference selection must STAGE, never commit. ──
        let db = legacy_machine_db();
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
        let db2 = legacy_machine_db();
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
        let db = legacy_machine_db();
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
        let db = legacy_machine_db();
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
        let db = legacy_machine_db();
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
        let db = legacy_machine_db();
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
        let db = legacy_machine_db();
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
        let db = legacy_machine_db();
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
        let db = legacy_machine_db();
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
        let db = legacy_machine_db();
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
