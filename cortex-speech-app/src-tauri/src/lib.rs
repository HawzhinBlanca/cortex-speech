// Production code must handle errors explicitly: `.unwrap()`/`.expect()` are denied
// outside of tests. Reviewed, infallible exceptions are grandfathered with a local
// `#[allow(clippy::unwrap_used)]` plus justification (see e.g. `normalizer.rs`).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod agentic;
pub mod align_text;
pub mod api_keys;
pub mod aligner;
pub mod asr;
pub mod atomic_file;
pub mod audio;
pub mod audio_quality;
pub mod cache;
pub mod cancel;
pub mod chunking;
pub mod commands;
pub mod corrections;
pub mod crash;
pub mod db;
pub mod denoiser;
pub mod diarization;
pub mod diff;
pub mod error;
pub mod eval;
pub mod export;
pub mod export_audio;
pub mod export_bundle;
pub mod features;
pub mod fingerprint;
pub mod flock;
pub mod gemini_api;
pub mod health;
pub mod history;
pub mod http;
pub mod inference;
pub mod integration_runner;
pub mod jury;
pub mod llm_refiner;
pub mod media;
pub mod migrations;
pub mod models;
pub mod normalizer;
pub mod observer;
pub mod perf;
pub mod pipeline;
pub mod quality;
pub mod registry;
pub mod runs;
pub mod scorecard;
pub mod scribe_api;
pub mod secret_redaction;
pub mod session;
pub mod settings;
pub mod significance;
pub mod stats;
pub mod telemetry;
pub mod throttle;
pub mod validation;
pub mod wer;

use cache::TranscriptCache;
use cancel::CancellationToken;
use db::Database;
use fingerprint::AudioFingerprint;
use history::HistoryManager;
use media::MediaRegistry;
use models::ModelManager;
use normalizer::SoraniNormalizer;
use pipeline::ProcessingPipeline;
use session::SessionManager;
use settings::AppSettings;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportState {
    Idle,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchState {
    Idle,
    Running,
}

pub struct AppState {
    pub db: Mutex<Database>,
    pub pipeline: Mutex<ProcessingPipeline>,
    pub normalizer: Arc<SoraniNormalizer>,
    pub cache: Arc<TranscriptCache>,
    pub fingerprint: Arc<AudioFingerprint>,
    pub history: HistKeyMgr,
    pub session: Mutex<SessionManager>,
    pub settings: Mutex<AppSettings>,
    pub data_dir: Mutex<Option<PathBuf>>,
    pub model_manager: Mutex<ModelManager>,
    /// Separate cancellation slots per long-running operation kind. Imports (start_cancel_token) and
    /// batches (ensure_cancel_token) run under independent gates, so sharing ONE slot let starting one
    /// detach the other's token — a Cancel could then miss a still-running operation or hit the wrong
    /// one. With a slot each, cancel_current_operation cancels BOTH, so the single Cancel control
    /// reliably stops everything that is running.
    pub import_cancel_token: Mutex<Option<CancellationToken>>,
    pub batch_cancel_token: Mutex<Option<CancellationToken>>,
    pub import_state: Mutex<ImportState>,
    pub batch_state: Mutex<BatchState>,
    pub media_registry: Mutex<MediaRegistry>,
}

type HistKeyMgr = Mutex<HistoryManager>;

impl AppState {
    fn lock_import_state(&self) -> MutexGuard<'_, ImportState> {
        self.import_state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned import state lock");
            poisoned.into_inner()
        })
    }

    fn lock_import_cancel_token(&self) -> MutexGuard<'_, Option<CancellationToken>> {
        self.import_cancel_token.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned import cancellation token lock");
            poisoned.into_inner()
        })
    }

    fn lock_batch_cancel_token(&self) -> MutexGuard<'_, Option<CancellationToken>> {
        self.batch_cancel_token.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned batch cancellation token lock");
            poisoned.into_inner()
        })
    }

    fn lock_batch_state(&self) -> MutexGuard<'_, BatchState> {
        self.batch_state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned batch state lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn lock_pipeline(&self) -> MutexGuard<'_, ProcessingPipeline> {
        self.pipeline.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned processing pipeline lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn lock_settings(&self) -> MutexGuard<'_, AppSettings> {
        self.settings.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned settings lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn lock_model_manager(&self) -> MutexGuard<'_, ModelManager> {
        self.model_manager.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned model manager lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn lock_data_dir(&self) -> MutexGuard<'_, Option<PathBuf>> {
        self.data_dir.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned data directory lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn lock_media_registry(&self) -> MutexGuard<'_, MediaRegistry> {
        self.media_registry.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned media registry lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn lock_history(&self) -> MutexGuard<'_, HistoryManager> {
        self.history.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned history lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn lock_session(&self) -> MutexGuard<'_, SessionManager> {
        self.session.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned session lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn lock_db(&self) -> MutexGuard<'_, Database> {
        self.db.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned database lock");
            poisoned.into_inner()
        })
    }

    pub fn session_save(&self) {
        let db = self.lock_db();
        if let Err(error) = self.lock_session().save(&db) {
            tracing::error!("Session save failed: {error}");
        }
    }

    pub fn session_auto_save(&self) {
        let db = self.lock_db();
        if let Err(error) = self.lock_session().auto_save(&db) {
            tracing::error!("Session autosave failed: {error}");
        }
    }

    /// Returns the active BATCH cancellation token, creating one if none exists. Batches reuse a
    /// token for the operation's duration (so a cancel request stays in effect), hence reuse-or-create.
    pub fn ensure_cancel_token(&self) -> Result<CancellationToken, String> {
        let mut guard = self.lock_batch_cancel_token();
        if let Some(token) = guard.as_ref() {
            return Ok(token.clone());
        }
        let token = CancellationToken::new();
        *guard = Some(token.clone());
        Ok(token)
    }

    /// Arms a fresh IMPORT cancellation token (imports always get a new one per run).
    pub fn start_cancel_token(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.lock_import_cancel_token() = Some(token.clone());
        token
    }

    /// Cancel every running operation. Both slots are signalled so the single Cancel control reliably
    /// stops a running import AND a running batch, regardless of which started last.
    pub fn cancel_current_operation(&self) -> bool {
        let mut cancelled_any = false;
        if let Some(token) = self.lock_import_cancel_token().as_ref() {
            token.cancel();
            cancelled_any = true;
        }
        if let Some(token) = self.lock_batch_cancel_token().as_ref() {
            token.cancel();
            cancelled_any = true;
        }
        cancelled_any
    }

    pub fn is_cancelled(&self) -> bool {
        self.lock_import_cancel_token().as_ref().is_some_and(|t| t.is_cancelled())
            || self.lock_batch_cancel_token().as_ref().is_some_and(|t| t.is_cancelled())
    }

    pub fn try_start_import(&self) -> Result<(), String> {
        let mut import = self.lock_import_state();
        if *import == ImportState::Running {
            return Err("Import already in progress".into());
        }
        *import = ImportState::Running;
        Ok(())
    }

    pub fn finish_import(&self) {
        *self.lock_import_state() = ImportState::Idle;
        // Drop the (possibly-cancelled) token so the next import never inherits a stale one.
        *self.lock_import_cancel_token() = None;
    }

    pub fn try_start_batch(&self) -> Result<(), String> {
        let mut batch = self.lock_batch_state();
        if *batch == BatchState::Running {
            return Err("Batch operation already in progress".into());
        }
        *batch = BatchState::Running;
        Ok(())
    }

    pub fn finish_batch(&self) {
        *self.lock_batch_state() = BatchState::Idle;
        // Drop the token here: ensure_cancel_token is reuse-or-create, so without clearing, a batch
        // that was cancelled would leave a permanently-cancelled token in the slot and EVERY later
        // batch would inherit it and no-op immediately.
        *self.lock_batch_cancel_token() = None;
    }

    pub fn update_pipeline_settings(&self, settings: AppSettings) {
        self.lock_pipeline().update_settings(settings);
    }
}

/// Where panic crash dumps are written — set once the app data dir exists (see `run`).
static CRASH_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            *s
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.as_str()
        } else {
            "Box<Any>"
        };
        tracing::error!("PANIC at {}: {}", location, message);
        // Persist a crash dump before the process dies, so the panic is diagnosable, not silent.
        if let Some(dir) = CRASH_DIR.get() {
            let ts = chrono::Utc::now().to_rfc3339();
            if let Some(path) = crash::write_crash_report(dir, &location, message, &ts) {
                eprintln!("Crash report written to {}", path.display());
            }
        }
    }));

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .with_thread_ids(true)
        .init();

    models::init_ort_dylib_path();

    let data_dir = get_app_data_dir();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        fatal_app_error(format!("Failed to create app data directory at {:?}: {e}", data_dir));
    }
    // The data dir now exists — let the panic hook write crash dumps there.
    let _ = CRASH_DIR.set(data_dir.clone());

    models::init_user_models_dir(data_dir.join("models"));

    let smoke_test = is_headless_mode();

    let _lock = if smoke_test {
        None
    } else {
        match crate::flock::InstanceLock::try_lock(&data_dir) {
            Ok(lock) => Some(lock),
            Err(e) => fatal_app_error(e.to_string()),
        }
    };

    let db_path = data_dir.join("cortex-speech.db");
    let db = match Database::open_with_retry(db_path.to_string_lossy().as_ref()) {
        Ok(db) => db,
        Err(e) => fatal_app_error(format!("Failed to open database at {:?}: {e}", db_path)),
    };
    if let Err(e) = db.initialize() {
        fatal_app_error(format!("Failed to initialize database schema: {e}"));
    }

    let settings_path = data_dir.join("settings.json");
    let settings = AppSettings::load(&settings_path);

    let normalizer = Arc::new(SoraniNormalizer::new());
    let cache = Arc::new(TranscriptCache::new(1000));
    let fingerprint = Arc::new(AudioFingerprint::new());

    let pipeline = ProcessingPipeline::new(
        db_path.to_string_lossy().to_string(),
        Arc::clone(&normalizer),
        Arc::clone(&cache),
        Arc::clone(&fingerprint),
        Arc::new(settings.clone()),
        Arc::new(ModelManager::new(data_dir.join("models"))),
    );

    let history = HistoryManager::new(500);
    let session = SessionManager::new(data_dir.join("session"));

    let model_manager = ModelManager::new(data_dir.join("models"));
    if let Err(e) = model_manager.ensure_dir() {
        tracing::warn!("Could not create models directory: {e}");
    }

    if !model_manager.all_models_present() {
        let missing = model_manager.missing_required_model_names();
        tracing::warn!("Essential models missing: {:?}", missing);
    } else {
        tracing::info!("Required models present, warming up...");
        let warmup_start = std::time::Instant::now();
        if let Err(e) = model_manager.warmup() {
            tracing::warn!("Model warm-up error: {e}");
        }
        if let Err(e) = pipeline.warmup_asr() {
            tracing::warn!("ASR pool warm-up error: {e}");
        }
        crate::inference::INFERENCE_METRICS.set_model_load_time_ms(warmup_start.elapsed().as_secs_f64() * 1000.0);
    }

    // Restore previous session
    if let Ok(Some(state)) = session.restore() {
        tracing::info!("Session restored: {} segments, {} verified", state.segment_count, state.verified_count);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            db: Mutex::new(db),
            pipeline: Mutex::new(pipeline),
            normalizer,
            cache,
            fingerprint,
            history: Mutex::new(history),
            session: Mutex::new(session),
            settings: Mutex::new(settings),
            data_dir: Mutex::new(Some(data_dir)),
            model_manager: Mutex::new(model_manager),
            import_cancel_token: Mutex::new(None),
            batch_cancel_token: Mutex::new(None),
            import_state: Mutex::new(ImportState::Idle),
            batch_state: Mutex::new(BatchState::Idle),
            media_registry: Mutex::new(MediaRegistry::default()),
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_health,
            commands::open_audio_file,
            commands::import_directory,
            commands::import_audio_file,
            commands::transcribe_segment,
            commands::batch_transcribe,
            commands::normalize_text,
            commands::align_segment,
            commands::get_segments,
            commands::search_segments,
            commands::update_segment,
            commands::delete_segment,
            commands::delete_segments_batch,
            commands::merge_dataset_json,
            commands::export_dataset,
            commands::export_dataset_bundle,
            commands::export_huggingface_dataset,
            commands::create_dataset_run,
            commands::list_dataset_runs,
            commands::list_agent_import_reports,
            commands::list_agent_stage_events,
            commands::start_job,
            commands::get_job_status,
            commands::cancel_job,
            commands::list_model_versions,
            commands::get_champion_model,
            commands::import_model_checkpoint,
            commands::create_gold_from_file,
            commands::get_configured_providers,
            commands::transcribe_audio_with_scribe,
            commands::add_scribe_votes,
            commands::get_blocking_validation_issues,
            commands::register_media_asset,
            commands::get_media_asset_url,
            commands::check_external_provider,
            commands::check_agentic_readiness,
            commands::rediarize_segments,
            commands::rename_speaker,
            commands::get_audio_duration,
            commands::get_waveform,
            commands::get_import_status,
            commands::get_dataset_stats,
            commands::get_speakers,
            commands::get_dataset_quality,
            commands::get_settings,
            commands::update_settings,
            commands::get_cache_info,
            commands::clear_cache,
            commands::get_fingerprint_count,
            commands::undo,
            commands::redo,
            commands::can_undo,
            commands::can_redo,
            commands::compute_diff,
            commands::validate_dataset_cmd,
            commands::export_audio,
            commands::batch_verify,
            commands::batch_assign_speaker,
            commands::batch_normalize,
            commands::get_tracing_stats,
            commands::get_recent_spans,
            commands::clear_tracing_spans,
            commands::save_session,
            commands::restore_session,
            commands::check_audio,
            commands::db_info,
            commands::db_backup,
            commands::db_vacuum,
            commands::db_wal_checkpoint,
            commands::models_status,
            commands::models_download,
            commands::models_download_all,
            commands::start_operation,
            commands::cancel_operation,
            commands::get_inference_stats,
            commands::run_wsl_refinement,
            commands::cancel_wsl_refinement,
            commands::add_segment_hypothesis,
            commands::run_consensus_refinery,
            commands::compute_acoustic_scores,
            commands::get_dataset_certificate,
            commands::compute_ood_scores,
            commands::get_active_learning_queue,
            // Phase 1 — Gold-Set Eval Harness
            commands::import_gold_segments,
            commands::run_gold_eval,
            commands::run_gold_eval_local,
            commands::run_gold_eval_asr,
            commands::build_scorecard,
            commands::compute_annotation_drift_scorecard,
            commands::list_eval_runs,
            commands::list_gold_segments,
            // Phase 2 — T0 Gate + Jury
            commands::run_t0_gate,
            commands::get_escalation_queue,
            commands::record_human_decision,
            commands::clear_human_decision,
            commands::write_segment_verdict,
            commands::get_few_shot_examples,
            commands::get_escalation_rate_trend,
            commands::run_dpo_update,
            // Phase 1+2: Full pipeline + T2 direct
            commands::run_jury_pipeline,
            commands::run_t2_for_segment,
            commands::update_segment_bounds,
        ])
        .setup(|app| {
            use tauri::Manager;
            for (label, _window) in app.webview_windows() {
                tracing::info!("Found webview window: {label}");
                // `open_devtools` only exists in debug builds, so in release the binding is
                // otherwise unused — the underscore keeps the release build warning-clean
                // while still allowing the debug-only devtools call below.
                #[cfg(debug_assertions)]
                _window.open_devtools();
            }

            if std::env::var("CORTEX_INTEGRATION_TEST").ok().as_deref() == Some("1")
                || std::env::var("CORTEX_AUDIOBOOK_PIPELINE").ok().as_deref() == Some("1")
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    match crate::integration_runner::run(&handle) {
                        Ok(()) => handle.exit(0),
                        Err(e) => {
                            eprintln!("CORTEX_INTEGRATION_FAIL: {e}");
                            handle.exit(1);
                        }
                    }
                });
            } else if std::env::var("CORTEX_SMOKE_TEST").ok().as_deref() == Some("1") {
                tracing::info!("CORTEX_SMOKE_TEST: shell initialized, exiting");
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    handle.exit(0);
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| fatal_app_error(format!("Tauri application runtime error: {e}")));
}

fn fatal_app_error(message: String) -> ! {
    tracing::error!("CORTEX_STARTUP_FAIL: {message}");
    eprintln!("CORTEX_STARTUP_FAIL: {message}");
    std::process::exit(1);
}

fn is_headless_mode() -> bool {
    matches!(std::env::var("CORTEX_SMOKE_TEST").ok().as_deref(), Some("1"))
        || matches!(std::env::var("CORTEX_INTEGRATION_TEST").ok().as_deref(), Some("1"))
        || matches!(std::env::var("CORTEX_AUDIOBOOK_PIPELINE").ok().as_deref(), Some("1"))
}

fn get_app_data_dir() -> PathBuf {
    if is_headless_mode() {
        let suffix = if std::env::var("CORTEX_AUDIOBOOK_PIPELINE").ok().as_deref() == Some("1") {
            "cortex-audiobook"
        } else if std::env::var("CORTEX_INTEGRATION_TEST").ok().as_deref() == Some("1") {
            "cortex-integration"
        } else {
            "cortex-smoke"
        };
        return std::env::temp_dir().join(format!("{suffix}-{}", std::process::id()));
    }
    let base = if cfg!(target_os = "windows") {
        std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
    } else if cfg!(target_os = "macos") {
        dirs_fallback("Library/Application Support")
    } else {
        dirs_fallback(".local/share")
    };
    base.join("cortex-speech")
}

fn dirs_fallback(suffix: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app_state(data_dir: PathBuf) -> AppState {
        let normalizer = Arc::new(SoraniNormalizer::new());
        let cache = Arc::new(TranscriptCache::new(10));
        let fingerprint = Arc::new(AudioFingerprint::new());
        let settings = AppSettings::default();
        let model_manager = ModelManager::new(data_dir.join("models"));
        let pipeline = ProcessingPipeline::new(
            ":memory:".to_string(),
            Arc::clone(&normalizer),
            Arc::clone(&cache),
            Arc::clone(&fingerprint),
            Arc::new(settings.clone()),
            Arc::new(ModelManager::new(data_dir.join("models"))),
        );

        AppState {
            db: Mutex::new(Database::open(":memory:").unwrap()),
            pipeline: Mutex::new(pipeline),
            normalizer,
            cache,
            fingerprint,
            history: Mutex::new(HistoryManager::new(10)),
            session: Mutex::new(SessionManager::new(data_dir.join("session"))),
            settings: Mutex::new(settings),
            data_dir: Mutex::new(Some(data_dir)),
            model_manager: Mutex::new(model_manager),
            import_cancel_token: Mutex::new(None),
            batch_cancel_token: Mutex::new(None),
            import_state: Mutex::new(ImportState::Idle),
            batch_state: Mutex::new(BatchState::Idle),
            media_registry: Mutex::new(MediaRegistry::default()),
        }
    }

    #[test]
    fn app_state_cancel_token_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let token = state.ensure_cancel_token().expect("create cancel token");
        token.cancel();

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.batch_cancel_token.lock().expect("lock cancel token");
            panic!("poison cancel token");
        }));

        assert!(state.is_cancelled());
        assert!(state.ensure_cancel_token().expect("recover cancel token").is_cancelled());
    }

    #[test]
    fn app_state_start_and_cancel_recover_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.import_cancel_token.lock().expect("lock cancel token");
            panic!("poison cancel token");
        }));

        let token = state.start_cancel_token();
        assert!(!token.is_cancelled());
        assert!(state.cancel_current_operation());
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_reaches_both_a_running_import_and_batch() {
        // Hardening-audit MEDIUM: an import (start_cancel_token) and a batch (ensure_cancel_token) run
        // under separate gates and used to SHARE one cancel slot, so starting one detached the other's
        // token and the single Cancel control could miss a still-running operation. With independent
        // slots, cancel_current_operation signals BOTH.
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let batch = state.ensure_cancel_token().expect("batch token"); // batch slot
        let import = state.start_cancel_token(); // import slot — must NOT detach the batch token
        assert!(!batch.is_cancelled() && !import.is_cancelled(), "both live before cancel");

        assert!(state.cancel_current_operation());
        assert!(batch.is_cancelled(), "the batch is cancelled (its token was lost before the fix)");
        assert!(import.is_cancelled(), "the import is cancelled too");
    }

    #[test]
    fn second_batch_gets_a_fresh_token_after_cancel() {
        // Round-2 audit HIGH: ensure_cancel_token is reuse-or-create, so a cancelled batch left a
        // permanently-cancelled token in the slot and every later batch inherited it and no-op'd.
        // finish_batch now clears the slot so the next batch gets a live token.
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        state.try_start_batch().expect("start batch 1");
        let t1 = state.ensure_cancel_token().expect("batch token 1");
        assert!(state.cancel_current_operation());
        assert!(t1.is_cancelled());
        state.finish_batch(); // Drop-guard equivalent — must clear the cancelled token

        state.try_start_batch().expect("start batch 2");
        let t2 = state.ensure_cancel_token().expect("batch token 2");
        assert!(!t2.is_cancelled(), "the second batch must NOT inherit the cancelled token");
    }

    #[test]
    fn app_state_import_state_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.import_state.lock().expect("lock import state");
            panic!("poison import state");
        }));

        state.try_start_import().expect("recover and start import");
        assert_eq!(*state.lock_import_state(), ImportState::Running);
        assert_eq!(state.try_start_import(), Err("Import already in progress".to_string()));
        state.finish_import();
        assert_eq!(*state.lock_import_state(), ImportState::Idle);
    }

    #[test]
    fn app_state_batch_state_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.batch_state.lock().expect("lock batch state");
            panic!("poison batch state");
        }));

        state.try_start_batch().expect("recover and start batch");
        assert_eq!(*state.lock_batch_state(), BatchState::Running);
        assert_eq!(state.try_start_batch(), Err("Batch operation already in progress".to_string()));
        state.finish_batch();
        assert_eq!(*state.lock_batch_state(), BatchState::Idle);
    }

    #[test]
    fn app_state_settings_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        state.lock_settings().vad_threshold = 0.61;

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.settings.lock().expect("lock settings");
            panic!("poison settings");
        }));

        assert_eq!(state.lock_settings().vad_threshold, 0.61);
    }

    #[test]
    fn app_state_model_manager_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.model_manager.lock().expect("lock model manager");
            panic!("poison model manager");
        }));

        assert_eq!(state.lock_model_manager().models_dir, dir.path().join("models"));
    }

    #[test]
    fn app_state_data_dir_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.data_dir.lock().expect("lock data directory");
            panic!("poison data directory");
        }));

        assert_eq!(*state.lock_data_dir(), Some(dir.path().to_path_buf()));
    }

    #[test]
    fn app_state_media_registry_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.media_registry.lock().expect("lock media registry");
            panic!("poison media registry");
        }));

        assert_eq!(
            state.lock_media_registry().resolve("missing"),
            Err("Media grant is missing or expired".to_string())
        );
    }

    #[test]
    fn app_state_history_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.history.lock().expect("lock history");
            panic!("poison history");
        }));

        assert!(!state.lock_history().can_undo());
        assert!(!state.lock_history().can_redo());
    }

    #[test]
    fn app_state_session_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.session.lock().expect("lock session");
            panic!("poison session");
        }));

        assert_eq!(state.lock_session().save_path(), dir.path().join("session").join("session.json"));
    }

    #[test]
    fn app_state_db_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.db.lock().expect("lock database");
            panic!("poison database");
        }));

        assert_eq!(state.lock_db().integrity_check().unwrap(), "ok");
    }

    #[test]
    fn app_state_pipeline_settings_update_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.pipeline.lock().expect("lock processing pipeline");
            panic!("poison processing pipeline");
        }));

        let settings = AppSettings { vad_threshold: 0.73, max_segment_duration_ms: 42_000, ..AppSettings::default() };
        state.update_pipeline_settings(settings.clone());
        let live_settings = state.lock_pipeline().settings_snapshot();
        assert_eq!(live_settings.vad_threshold, settings.vad_threshold);
        assert_eq!(live_settings.max_segment_duration_ms, settings.max_segment_duration_ms);
    }
}
