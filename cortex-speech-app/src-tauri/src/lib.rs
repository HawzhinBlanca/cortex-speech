// Production code must handle errors explicitly: `.unwrap()`/`.expect()` are denied
// outside of tests. Reviewed, infallible exceptions are grandfathered with a local
// `#[allow(clippy::unwrap_used)]` plus justification (see e.g. `normalizer.rs`).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod agentic;
pub mod aligner;
pub mod api_keys;
pub mod asr;
pub mod atomic_file;
pub mod audio;
pub mod audio_quality;
pub mod cache;
pub mod cancel;
pub mod champion_promotion;
pub mod chunking;
pub mod commands;
pub mod constrained_decode;
pub mod corrections;
pub mod couch;
pub mod crash;
pub mod db;
pub mod denoiser;
pub mod deployment;
pub mod dialect;
pub mod diarization;
pub mod diff;
pub mod dpapi;
pub mod engine_runtime;
pub mod engine_supervisor;
pub mod error;
pub mod eval;
pub mod export;
pub mod export_audio;
pub mod export_bundle;
// REMOVED (iteration 231): `features` — an 80-bin mel-filterbank extractor, 473 lines, and the sole
// user of the `rustfft` dependency. Its production consumer was the fbank diarization fallback, deleted
// earlier for not being speaker-discriminative; after that its ONLY caller was an #[ignore]d test that
// tested FbankExtractor itself. A module whose reason to exist is a test of the module is not coverage.
// sherpa-onnx computes its own features for every model this app runs.
pub mod fingerprint;
pub mod flock;
pub mod gemini_api;
pub mod health;
pub mod history;
pub mod http;
pub mod inference;
pub mod integration_runner;
pub mod jobs;
pub mod jury;
pub mod llm_refiner;
pub mod media;
pub mod migrations;
pub mod models;
pub mod normalizer;
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
pub mod snapshot;
pub mod source_provenance;
pub mod stats;
pub mod telemetry;
pub mod throttle;
pub mod transcript_export;
pub mod validation;
pub mod wav2vec2_asr;
pub mod wer;

// M0.6 / P0.2: Git SHA baked at compile time by build.rs (via rustc-env GIT_SHA). Exposed to the
// frontend via the `app_git_sha` IPC command, and embedded as a greppable, contiguous rodata marker
// (below) so `scripts/check_exe_freshness.py` can extract it from the binary WITHOUT running the exe
// and assert the running binary matches HEAD — closing the stale-exe trap (deep-audit F4).
pub const GIT_SHA: &str = env!("GIT_SHA");

/// Contiguous, prefixed marker forced into the binary so a static (non-executing) check can recover
/// the baked SHA. `#[used]` keeps the linker from stripping it; `concat!` guarantees the prefix and
/// SHA are one literal in rodata. Grep pattern: `CORTEX_BUILD_SHA:<40 hex>`.
#[used]
static GIT_SHA_MARKER: &str = concat!("CORTEX_BUILD_SHA:", env!("GIT_SHA"));

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
    // Arc so a slow command can clone the handle and move DB work into `spawn_blocking` (off the
    // main/UI thread) without borrowing `State` across an await. lock_db() still returns a guard.
    pub db: Arc<Mutex<Database>>,
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

    /// A clonable handle to the DB, for moving blocking work into `spawn_blocking`. Lock it the same
    /// poison-tolerant way `lock_db` does: `db.lock().unwrap_or_else(|p| p.into_inner())`.
    pub(crate) fn db_arc(&self) -> Arc<Mutex<Database>> {
        Arc::clone(&self.db)
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
            // Reuse a LIVE token (so an in-flight cancel stays in effect), but NEVER hand back a
            // cancelled one. finish_batch clears the slot under a separate lock AFTER it flips the
            // state gate to Idle, so a re-clicked batch can start while a just-cancelled token still
            // lingers here; returning it would make the new batch's first is_cancelled() check fire
            // and silently no-op the whole run (round-15 TOCTOU). Replace a cancelled token instead.
            if !token.is_cancelled() {
                return Ok(token.clone());
            }
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
        // P1.3b: refuse to start while a DB restore is reserved. Checked UNDER the import_state lock (and
        // set-Running is under the same lock) so it can't race prepare_restore's writers_active() read.
        if crate::commands::restore_pending() {
            return Err(crate::commands::RESTORE_IN_PROGRESS_MSG.into());
        }
        if *import == ImportState::Running {
            return Err("Import already in progress".into());
        }
        *import = ImportState::Running;
        Ok(())
    }

    pub fn finish_import(&self) {
        // Clear the (possibly-cancelled) token BEFORE opening the gate — the same round-15 TOCTOU
        // ordering finish_batch documents. The OLD order (Idle first, then clear) let a new import
        // start the instant this call flipped the state to Idle, arm its OWN token via
        // start_cancel_token, and then have THIS call's second statement wipe that fresh token from
        // the slot — leaving the new import running but with an empty cancel slot, so
        // cancel_current_operation could never stop it. Opening the gate LAST means a new import can
        // only begin after this call has fully finished, so it can never lose its token to us.
        *self.lock_import_cancel_token() = None;
        *self.lock_import_state() = ImportState::Idle;
    }

    pub fn try_start_batch(&self) -> Result<(), String> {
        let mut batch = self.lock_batch_state();
        // P1.3b: refuse to start a batch while a DB restore is reserved (checked under the batch_state lock).
        if crate::commands::restore_pending() {
            return Err(crate::commands::RESTORE_IN_PROGRESS_MSG.into());
        }
        if *batch == BatchState::Running {
            return Err("Batch operation already in progress".into());
        }
        *batch = BatchState::Running;
        Ok(())
    }

    pub fn finish_batch(&self) {
        // Clear the (possibly-cancelled) token BEFORE opening the gate. A new batch can only start
        // once batch_state is Idle, and the mutex release/acquire chain guarantees that any thread
        // observing Idle also observes the token already cleared — so it can never inherit the stale
        // token through the gap between these two statements (round-15 TOCTOU). ensure_cancel_token is
        // additionally hardened to never return a cancelled token, as belt-and-suspenders.
        *self.lock_batch_cancel_token() = None;
        *self.lock_batch_state() = BatchState::Idle;
    }

    pub fn update_pipeline_settings(&self, settings: AppSettings) {
        self.lock_pipeline().update_settings(settings);
    }

    /// Stop cloud egress for any opt-in `next` turns OFF, without waiting for the save to succeed.
    /// Withdrawals only — see `ProcessingPipeline::revoke_consent_now`.
    pub fn revoke_pipeline_consent_now(&self, next: &AppSettings) {
        self.lock_pipeline().revoke_consent_now(next);
    }

    /// True while an import or batch worker may be WRITING. The restore gate (true-10 audit
    /// 2026-07-09): restoring mid-import let the worker keep upserting pre-restore segments into the
    /// just-restored library through its own pipeline connection, and a batch finishing AFTER
    /// restore pushed a stale undo command into the freshly-cleared history — pressing Ctrl+Z then
    /// replayed pre-restore rows into the restored dataset.
    pub fn writers_active(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        *self.lock_import_state() == ImportState::Running
            || *self.lock_batch_state() == BatchState::Running
            // The WSL-7B refinement loop is a background DB WRITER too (update_asr_transcript_if_unreviewed),
            // tracked by its own atomic — restoring a snapshot while it writes tears the DB (round-26 hunt).
            || crate::commands::WSL_REFINE_RUNNING.load(SeqCst)
            // R3: the OTHER background writers that use dedicated connections (or land a write after a
            // lock-free cloud window) and so also escape the db-Mutex serialization the restore relies on:
            //   - Scribe vote batches (add_scribe_votes) — SCRIBE_VOTES_IN_FLIGHT,
            //   - every dedicated-connection background writer that registers a BgDbWriterGuard: the jury
            //     pipeline / T2 / DPO / post-import adjudication writers AND the detached background-
            //     alignment thread (which outlives the import guard) — one BG_DB_WRITERS counter,
            //   - the Couch phone-review server — a running server can persist a decision on submit.
            // Each was a real "restore mixes late writes into the just-restored library" hole. New
            // dedicated-connection writers register a BgDbWriterGuard rather than growing this chain.
            || crate::commands::SCRIBE_VOTES_IN_FLIGHT.load(SeqCst)
            || crate::commands::bg_db_writers_active()
            || crate::couch::is_running()
    }
}

/// Where panic crash dumps are written — set once the app data dir exists (see `run`).
static CRASH_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// How many daily log files to keep under `<data_dir>/logs`. See the appender below.
const LOG_RETENTION_DAYS: usize = 60;

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

    // Compute the data dir FIRST so tracing can ALSO write to a rolling log file there. The release GUI
    // runs with windows_subsystem="windows", which discards stdout — without a file sink EVERY non-panic
    // warning/error is invisible and the owner has nothing to inspect after a bad import/snapshot/batch.
    let data_dir = get_app_data_dir();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("Failed to create app data directory at {data_dir:?}: {e}");
        fatal_app_error(format!("Failed to create app data directory at {:?}: {e}", data_dir));
    }

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // Rolling daily log under <data_dir>/logs (non-blocking writer). The WorkerGuard must outlive the
    // process or buffered lines are dropped on exit, so it is leaked intentionally (one per process).
    let log_dir = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    // Clear the orderly-exit marker on every start, so its presence means exactly one thing: the last
    // run reached RunEvent::Exit. Absent while the app is not running = it died without getting there.
    // Until this existed the two were indistinguishable — 16 log files across a month contain ZERO
    // shutdown lines of any kind, so the watchdog's "session expected but app not running" (5 times in
    // the last week) could not be read as either "the owner closed it" or "it crashed".
    let exit_marker = log_dir.join("last-exit.txt");
    let _ = std::fs::remove_file(&exit_marker);
    // Daily rotation WITH retention. `rolling::daily` rotates but never deletes, so the log
    // directory grew without any bound at all — 23 MB across 30+ files by 2026-08-17, on a machine
    // whose C: drive already runs close to full. Sixty days is far longer than any incident
    // investigation here has needed and still bounds the directory. Falls back to the unbounded
    // appender if the builder ever rejects the config, because losing logs entirely would be worse
    // than keeping too many.
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("cortex.log")
        .max_log_files(LOG_RETENTION_DAYS)
        .build(&log_dir)
        .unwrap_or_else(|e| {
            eprintln!("log retention could not be configured ({e}); falling back to unbounded daily logs");
            tracing_appender::rolling::daily(&log_dir, "cortex.log")
        });
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
    let _ = Box::leak(Box::new(guard));
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_target(true).with_thread_ids(true))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_writer(file_writer),
            )
            .init();
    }

    models::init_ort_dylib_path();
    // Say so AT STARTUP if the runtime cannot load, rather than letting the first transcription
    // freeze with no message. A missing or corrupt onnxruntime library makes `ort` block forever
    // instead of failing, so without this the symptom is an app that simply stops responding
    // mid-import. Logged rather than fatal: everything that does not touch ONNX (review, export,
    // search, the whole DB side) still works, and taking the app away entirely would be a worse
    // answer than telling the owner what is broken.
    if let Err(error) = models::ensure_ort_runtime_loadable() {
        tracing::error!("{error}");
    }

    // The data dir exists and tracing now has a file sink — let the panic hook write crash dumps there.
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
    // PRE-MIGRATION pinned snapshot (true-10 audit 2026-07-09): a semantically-buggy migration (a
    // wrong UPDATE predicate, a lossy rewrite) commits cleanly — the all-or-nothing transaction only
    // protects against SQL errors — and the post-migration startup snapshot then rotates every
    // pre-upgrade copy out within ~90 minutes of first launch, exactly when the user is most
    // exposed. Before running any pending migration on a NON-empty library, pin a rotation-exempt
    // copy under snapshots/pinned/. Best-effort: a pin failure warns, never blocks startup.
    {
        let current = crate::migrations::get_current_version(&db).unwrap_or(0);
        let max_known = crate::migrations::max_supported_version();
        if current > 0 && current < max_known {
            match crate::snapshot::take_pinned_snapshot(
                &db,
                &data_dir,
                &format!("premigration_v{current}_to_v{max_known}"),
                3,
            ) {
                Ok(path) => tracing::info!("pre-migration snapshot pinned at {}", path.display()),
                Err(e) => tracing::warn!("pre-migration snapshot failed (continuing): {e}"),
            }
        }
    }
    if let Err(e) = db.initialize() {
        fatal_app_error(format!("Failed to initialize database schema: {e}"));
    }

    // P5.2: mirror the registry's champions to <data_dir>/champion.json at startup so external
    // consumers (the WSL 7B server) resolve the CURRENT champion — promotion is no longer a no-op
    // at its final step. Best-effort: a pointer-write failure never blocks startup.
    if let Err(e) = crate::registry::sync_champion_pointer(&db, &data_dir) {
        tracing::warn!("champion pointer sync failed: {e}");
    }

    // P0 #3 Job Supervisor: any durable job still `running` at startup is a crash residue (a clean run
    // always reaches a terminal state) — reap it to failed/INTERRUPTED so the activity surface shows the
    // honest "interrupted", never a ghost that spins forever. Best-effort: never blocks startup.
    match db.mark_orphaned_running_jobs_failed() {
        Ok(0) => {}
        Ok(n) => tracing::info!("reaped {n} interrupted job(s) from a previous crash"),
        Err(e) => tracing::warn!("startup job reaper failed: {e}"),
    }

    // P3.1/M0.4b: rotating auto-snapshots of the DB + config state. One on startup (so a corruption is
    // recoverable from the moment the app runs), then every 10 minutes — protecting the marathon's
    // irreplaceable review labor without any user action. Skipped in headless test modes.
    const SNAPSHOT_KEEP: usize = 10;
    const SNAPSHOT_INTERVAL_SECS: u64 = 600;
    if !smoke_test {
        match crate::snapshot::take_snapshot(&db, &data_dir, SNAPSHOT_KEEP) {
            Ok(Some(_)) => {}
            Ok(None) => tracing::warn!("startup DB snapshot skipped by the empty-DB guard"),
            Err(e) => tracing::warn!("startup DB snapshot failed: {e}"),
        }
        let snap_db_path = db_path.clone();
        let snap_data_dir = data_dir.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(SNAPSHOT_INTERVAL_SECS));
            // catch_unwind (true-10 audit 2026-07-09): a panic in the loop body silently killed the
            // safety-net thread for the rest of the session — the failure counter only saw Err, not
            // panics. A panic now counts as a failure (health surfaces it) and the loop survives.
            let iteration = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // A fresh read connection avoids holding the app's DB mutex for the backup's duration.
                match Database::open(snap_db_path.to_string_lossy().as_ref()) {
                    Ok(snap_db) => {
                        match crate::snapshot::take_snapshot(&snap_db, &snap_data_dir, SNAPSHOT_KEEP) {
                            Ok(_) => {}
                            Err(e) => tracing::warn!("periodic DB snapshot failed: {e}"),
                        }
                        // Second-directory backup (Week-2): re-read settings.json each interval so the
                        // owner can point backups at another drive without a restart. Failure here is
                        // warn-only — it must never break the primary snapshot safety net above.
                        let second = AppSettings::load(&snap_data_dir.join("settings.json")).backup_second_dir;
                        if !second.trim().is_empty() {
                            // Quarantine files live in the PRIMARY data dir — thread it in so the
                            // off-drive tree's prune-pin and accumulation cap see the corruption too
                            // (its own parent never holds *.corrupt.* files).
                            // take_offsite_snapshot (NOT ..._with_quarantine_source): the off-drive tree
                            // must not touch the shared health counters, or its success masks a failing
                            // primary snapshot tree and health_check reads a false green (round-25 hunt).
                            match crate::snapshot::take_offsite_snapshot(
                                &snap_db,
                                std::path::Path::new(second.trim()),
                                &snap_data_dir,
                                SNAPSHOT_KEEP,
                            ) {
                                Ok(_) => {}
                                Err(e) => tracing::warn!("second-directory snapshot failed ({second}): {e}"),
                            }
                        }
                    }
                    Err(e) => tracing::warn!("periodic snapshot: could not open db: {e}"),
                }
            }));
            if iteration.is_err() {
                crate::snapshot::record_snapshot_panic();
                tracing::error!("periodic snapshot iteration PANICKED — counted as a failure; loop continues");
            }
        });
    }

    let settings_path = data_dir.join("settings.json");
    let mut settings = AppSettings::load(&settings_path);
    // Test-only override: a release process must never be able to downgrade the champion through an
    // inherited environment variable. Integration binaries are debug builds and still get their
    // explicitly requested local fixture engine.
    #[cfg(debug_assertions)]
    if std::env::var("CORTEX_INTEGRATION_TEST").ok().as_deref() == Some("1") {
        settings.asr_model_size = crate::settings::AsrModelSize::CTC300M;
    }

    let normalizer = Arc::new(SoraniNormalizer::new());
    let cache = Arc::new(TranscriptCache::new(1000));
    let fingerprint = Arc::new(AudioFingerprint::new());
    // v50: rehydrate the dedup map from the library BEFORE the pipeline is built, so the very first
    // import of a session already knows every recording seen in previous ones. Without this the map
    // started empty every launch and cross-session duplicate detection did not exist (external review
    // 2026-08-06 #4).
    //
    // Best-effort by design: a failed read must not block startup. It costs this session cross-run
    // dedup — exactly the behaviour that shipped before v50 — and says so loudly rather than degrading
    // silently. Rows predating v50 have a NULL fingerprint and are simply absent until backfilled.
    //
    // v51: a row that has a spectral bucket but no content hash (imported between v50 and v51) IS
    // loaded, but can never reject an import, because a value that cannot distinguish content must not
    // discard a legitimate recording. The count is reported separately so the gap is visible in the log
    // rather than implied by silence.
    match db.load_audio_identities() {
        Ok(known) => {
            let unhashed = known.iter().filter(|k| k.content.is_none()).count();
            let n = fingerprint.rehydrate(known);
            tracing::info!("Audio dedup: rehydrated {n} recording identity/identities from the library");
            if unhashed > 0 {
                tracing::warn!(
                    "Audio dedup: {unhashed} recording(s) predate v51 and have no content hash — they \
                     cannot prove a duplicate and will never reject an import. Run \
                     `backfill_fingerprints --apply` to close the gap."
                );
            }
        }
        Err(e) => {
            tracing::warn!("Audio dedup: could not rehydrate identities ({e}) — within-run dedup only");
        }
    }

    let pipeline = ProcessingPipeline::new(
        db_path.to_string_lossy().to_string(),
        Arc::clone(&normalizer),
        Arc::clone(&cache),
        Arc::clone(&fingerprint),
        Arc::new(settings.clone()),
        Arc::new(ModelManager::new(data_dir.join("models"))),
    );

    let history = HistoryManager::new(500);
    let mut session = SessionManager::new(data_dir.join("session"));

    let model_manager = ModelManager::new(data_dir.join("models"));
    if let Err(e) = model_manager.ensure_dir() {
        tracing::warn!("Could not create models directory: {e}");
    }

    if !model_manager.all_models_present_for(&settings.asr_model_size) {
        let missing = model_manager.missing_required_model_names_for(&settings.asr_model_size);
        tracing::warn!("Models required by the selected ASR engine are missing: {:?}", missing);
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
            db: Arc::new(Mutex::new(db)),
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
            commands::take_last_crash,
            commands::app_git_sha,
            commands::open_audio_file,
            commands::import_directory,
            commands::get_interrupted_import,
            commands::resume_interrupted_import,
            commands::discard_interrupted_import,
            commands::import_audio_file,
            commands::transcribe_segment,
            commands::transcribe_segment_constrained,
            commands::transcribe_segment_finetuned,
            commands::verify_finetuned_model_integrity,
            commands::batch_transcribe,
            commands::normalize_text,
            commands::align_segment,
            commands::get_segment_consensus,
            commands::get_segment,
            commands::get_segments_page,
            commands::get_segment_ids_for_view,
            commands::get_signal_anomaly_segments,
            commands::update_segment,
            commands::update_segment_fields,
            commands::restore_segment_snapshot,
            commands::delete_segment,
            commands::delete_segments_batch,
            commands::merge_dataset_json,
            commands::export_dataset,
            commands::export_transcript,
            commands::get_jobs,
            commands::get_champion_engine_status,
            commands::start_champion_engine,
            commands::export_dataset_bundle,
            commands::export_huggingface_dataset,
            commands::list_agent_import_reports,
            commands::list_agent_stage_events,
            commands::list_model_versions,
            commands::import_model_checkpoint,
            commands::import_model_deployment,
            commands::bootstrap_legacy_champion,
            commands::create_gold_from_file,
            commands::import_verified_segments_as_gold,
            commands::export_gold_eval_set,
            commands::export_finetune_pack,
            commands::get_configured_providers,
            commands::set_api_key,
            commands::start_couch_review,
            commands::stop_couch_review,
            commands::revoke_couch_reviewer,
            commands::couch_review_status,
            commands::spot_check_report,
            commands::reviewer_throughput,
            commands::export_agreement_sample,
            commands::transcribe_audio_with_scribe,
            commands::add_scribe_votes,
            commands::register_media_asset,
            commands::get_media_asset_url,
            commands::check_agentic_readiness,
            commands::rediarize_segments,
            commands::rename_speaker,
            commands::get_audio_duration,
            commands::get_waveform,
            commands::get_dataset_stats,
            commands::get_speakers,
            commands::get_dataset_quality,
            commands::get_training_grade_breakdown,
            commands::set_recording_rights,
            commands::revoke_recording_consent,
            commands::list_recording_rights,
            commands::get_settings,
            commands::update_settings,
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
            commands::db_restore,
            commands::db_vacuum,
            commands::get_quarantine_notice,
            commands::acknowledge_quarantine,
            commands::get_intelligence_report,
            commands::list_db_snapshots,
            commands::restore_db_from_snapshot,
            commands::get_audio_health,
            commands::relink_audio,
            commands::models_status,
            commands::models_download,
            commands::models_download_all,
            commands::cancel_operation,
            commands::get_inference_stats,
            commands::run_wsl_refinement,
            commands::cancel_wsl_refinement,
            commands::compute_acoustic_scores,
            commands::get_dataset_certificate,
            commands::compute_signal_anomaly_scores,
            commands::get_active_learning_queue,
            // Phase 1 — Gold-Set Eval Harness
            commands::import_gold_segments,
            commands::run_gold_eval,
            commands::run_gold_eval_local,
            commands::run_gold_eval_asr,
            commands::build_scorecard,
            commands::list_eval_runs,
            commands::get_label_quality_lift,
            commands::list_gold_segments,
            // Phase 2 — T0 Gate + Jury
            commands::run_t0_gate,
            commands::get_escalation_queue,
            commands::record_human_decision,
            commands::clear_human_decision,
            commands::clear_escalation,
            commands::write_segment_verdict,
            commands::get_few_shot_examples,
            commands::get_escalation_rate_trend,
            commands::run_dpo_update,
            // Phase 1+2: Full pipeline + T2 direct
            commands::run_jury_pipeline,
            commands::run_t2_for_segment,
        ])
        .setup(|app| {
            use tauri::Manager;
            // Authorize the asset protocol to read the media-cache directory the registry actually
            // writes into. The static `$APPDATA/media-cache/**` scope in tauri.conf.json resolves
            // (Tauri v2) to the bundle-identifier-qualified app-data dir
            // (%APPDATA%\com.cortex.kurdish-speech\media-cache), which is NOT where get_app_data_dir()
            // writes (%APPDATA%\cortex-speech\media-cache). Without this runtime grant every
            // convertFileSrc(asset://) playback URL is refused (403) and no imported clip can be
            // played in the review UI — the core listen-and-approve step. Grant the REAL directory,
            // derived from the same data_dir source of truth via media::media_cache_dir, so playback
            // works regardless of how the static scope token would resolve.
            if let Some(data_dir) = app.state::<AppState>().lock_data_dir().clone() {
                let media_cache = crate::media::media_cache_dir(&data_dir);
                if let Err(e) = std::fs::create_dir_all(&media_cache) {
                    tracing::warn!("Could not create media cache dir {}: {e}", media_cache.display());
                }
                match app.asset_protocol_scope().allow_directory(&media_cache, true) {
                    Ok(()) => {
                        tracing::info!("Asset protocol scope authorized for media cache: {}", media_cache.display())
                    }
                    Err(e) => tracing::warn!("Failed to authorize media cache dir in asset scope: {e}"),
                }
            }

            // Bring back Couch Review if the app was closed without pressing Stop, with the SAME
            // links. Without this, closing the app killed every reviewer's URL and using the phone at
            // all meant returning to this desktop for a fresh one — the single thing that stopped
            // remote review being usable on the owner's own terms. Pressing Stop still revokes.
            //
            // Deliberately after the data-dir work above (the session file lives there) and
            // best-effort: `resume` never propagates an error, because a port already in use must not
            // stop the app from opening.
            {
                let state = app.state::<AppState>();
                let data_dir = state.lock_data_dir().clone();
                let db_path = state.lock_db().path().to_string();
                if let Some(dir) = data_dir {
                    crate::couch::resume(db_path, &dir);
                }
            }

            // Champion 7B supervision loop (engine_runtime): idles until the owner enables
            // champion_supervision_enabled; re-reads the setting every tick.
            crate::engine_runtime::spawn_supervision_loop(app.handle().clone());

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
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| fatal_app_error(format!("Tauri application runtime error: {e}")))
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                // Written SYNCHRONOUSLY, not through `tracing`: the file layer is a
                // `tracing_appender::non_blocking` whose WorkerGuard is deliberately leaked above, so
                // nothing flushes it at exit and a line emitted here can be lost with the process. A
                // diagnostic that is sometimes missing is worse than none, because then "absent" no
                // longer means "crashed".
                let _ = std::fs::write(&exit_marker, format!("orderly exit {}\n", chrono::Utc::now().to_rfc3339()));
                // Best-effort on ORDERLY exit: refuse further supervised starts, then tree-kill the
                // held champion child. An abnormal app death never reaches this handler — that orphan
                // case is documented (and adopted-on-next-launch) in engine_runtime's module doc.
                crate::engine_runtime::begin_shutdown();
            }
        });
}

fn fatal_app_error(message: String) -> ! {
    tracing::error!("CORTEX_STARTUP_FAIL: {message}");
    eprintln!("CORTEX_STARTUP_FAIL: {message}");
    // P1.2: the release GUI runs with windows_subsystem="windows", so stdout/stderr are DISCARDED and
    // the tracing file sink may not exist yet (data-dir create is itself a fatal path) — every fatal
    // startup cause (instance lock held, unopenable/newer-schema DB, data-dir create failure) would
    // otherwise present as "double-click the icon, nothing happens". Show a native modal so the user
    // sees the real reason. show_fatal_message_box self-suppresses under headless/CDP automation.
    #[cfg(windows)]
    show_fatal_message_box(&message);
    std::process::exit(1);
}

/// UTF-16 encode with a trailing NUL terminator, for the `*W` Win32 APIs (which read until the NUL).
#[cfg(windows)]
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// True when the app is being driven by an automated CDP / remote-debugging session — the e2e drivers
/// (e2e_real_app.cjs, e2e_*_ipc.cjs, e2e_7b_connect.cjs) all spawn the exe with
/// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<n>`. Pure for testing.
#[cfg(windows)]
fn is_cdp_remote_debug(webview2_browser_args: Option<&str>) -> bool {
    webview2_browser_args.is_some_and(|v| v.contains("--remote-debugging-port"))
}

/// Show a native modal error box for a fatal startup failure (no Tauri/webview exists this early, so
/// tauri-plugin-dialog cannot be used — this is a raw Win32 MessageBoxW). Best-effort and side-effect
/// only; the caller exits regardless of the user's click.
#[cfg(windows)]
fn show_fatal_message_box(message: &str) {
    // No human is waiting on a modal when the app is driven headlessly (smoke/integration/audiobook) or
    // over a CDP/remote-debugging automation session. Popping a topmost modal there blocks the desktop
    // and hangs the driver's ~90s connect poll (e.g. the InstanceLock fatal fires when the owner's app
    // is already open and an e2e driver launches a second instance). Suppress it — the eprintln/tracing
    // above still record the reason. Only a genuine interactive double-click reaches the MessageBox.
    if is_headless_mode() || is_cdp_remote_debug(std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").ok().as_deref())
    {
        return;
    }
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST};
    let caption = to_wide_null("Cortex Speech — cannot start");
    let body = to_wide_null(message);
    // SAFETY: hwnd is null (no parent window exists before the event loop); `body`/`caption` are valid,
    // NUL-terminated UTF-16 buffers that outlive the call. MessageBoxW runs its own modal loop and needs
    // no pre-existing message pump. The return value (which button) is irrelevant — we exit either way.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

fn is_headless_mode() -> bool {
    matches!(std::env::var("CORTEX_SMOKE_TEST").ok().as_deref(), Some("1"))
        || matches!(std::env::var("CORTEX_INTEGRATION_TEST").ok().as_deref(), Some("1"))
        || matches!(std::env::var("CORTEX_AUDIOBOOK_PIPELINE").ok().as_deref(), Some("1"))
}

/// The explicit `CORTEX_APP_DATA_DIR` override, if set. Shared with the `bin/*` CLI tools
/// (batch_importer/batch_processor/download_model/test_file) so the app and its batch utilities ALWAYS
/// resolve to the SAME data dir — they share the live DB and the single-instance lock. Pure for testing.
fn data_dir_override(env_val: Option<std::ffi::OsString>) -> Option<PathBuf> {
    env_val.map(PathBuf::from) // matches the bin/*.rs override exactly (no empty-string filtering)
}

fn get_app_data_dir() -> PathBuf {
    // The CLI tools honored CORTEX_APP_DATA_DIR but the app did not, so setting it (e.g. to relocate the
    // library to another drive) silently split them: the batch importer wrote to the override dir while
    // the app kept reading APPDATA\cortex-speech. Honor it here too, at top priority, so they never diverge.
    if let Some(dir) = data_dir_override(std::env::var_os("CORTEX_APP_DATA_DIR")) {
        return dir;
    }
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

    #[cfg(windows)]
    #[test]
    fn to_wide_null_is_nul_terminated_utf16() {
        // P1.2: the fatal-dialog marshalling. A `*W` Win32 API reads until a NUL, so a MISSING
        // terminator would over-read past the buffer (garbage title/body or a crash). Assert the NUL is
        // present, is the ONLY interior-free terminator for plain ASCII, and that non-ASCII (Sorani)
        // round-trips through UTF-16.
        assert_eq!(to_wide_null("Hi"), vec![0x48, 0x69, 0x00], "ASCII encodes + NUL-terminates");
        assert_eq!(to_wide_null(""), vec![0x00], "empty string is a single NUL");
        let w = to_wide_null("کوردی");
        assert_eq!(*w.last().unwrap(), 0, "must end in a NUL terminator");
        assert_eq!(String::from_utf16(&w[..w.len() - 1]).unwrap(), "کوردی", "Sorani round-trips via UTF-16");
        assert!(w[..w.len() - 1].iter().all(|&u| u != 0), "no interior NUL before the terminator");
    }

    #[test]
    fn writers_active_fences_background_db_writers() {
        // P1.3 (audit R3): the restore fence covered only import/batch/WSL, so a background writer on its
        // OWN connection (jury pipeline/T2/DPO/post-import adjudication, or the detached alignment thread)
        // could land a late write in a just-restored library. A held BgDbWriterGuard must now report
        // writers_active()==true so prepare_restore refuses. Uses the counter, so it is robust to any
        // concurrently-held guard (asserts the delta, not an absolute rest state).
        let tmp = tempfile::tempdir().unwrap();
        let state = test_app_state(tmp.path().to_path_buf());
        let before = state.writers_active();
        {
            let _writer = crate::commands::BgDbWriterGuard::new();
            assert!(state.writers_active(), "an in-flight background DB writer must arm the restore fence");
        }
        assert_eq!(state.writers_active(), before, "the fence clears when the background writer finishes");
    }

    #[cfg(windows)]
    #[test]
    fn cdp_remote_debug_suppresses_the_fatal_dialog() {
        // P1.2 regression: the fatal modal must be suppressed under CDP automation, or a fatal startup
        // (e.g. InstanceLock held while the owner's app is open) pops a topmost box that hangs the e2e
        // driver's connect poll. All e2e drivers set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS with the port.
        assert!(is_cdp_remote_debug(Some("--remote-debugging-port=9222")), "the CDP port arg suppresses");
        assert!(is_cdp_remote_debug(Some("--foo --remote-debugging-port=9222 --bar")), "embedded arg detected");
        assert!(!is_cdp_remote_debug(Some("--disable-gpu")), "unrelated webview2 args do not suppress");
        assert!(!is_cdp_remote_debug(None), "a real interactive launch (no such env) shows the dialog");
    }

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
            db: Arc::new(Mutex::new(Database::open(":memory:").unwrap())),
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
    fn data_dir_override_matches_the_cli_tools() {
        // The app must honor CORTEX_APP_DATA_DIR exactly as bin/*.rs does, or setting it splits the app
        // and the batch importer onto different databases (they share the live DB + single-instance lock).
        assert_eq!(data_dir_override(Some("D:/cortex-data".into())), Some(PathBuf::from("D:/cortex-data")));
        assert_eq!(data_dir_override(None), None, "unset -> fall through to headless/platform resolution");
    }

    #[test]
    fn scribe_commands_require_cloud_stt_consent() {
        // Regression: the Scribe IPC commands (transcribe_audio_with_scribe, add_scribe_votes) upload
        // raw audio (biometric) to ElevenLabs. They MUST refuse without explicit cloud-STT opt-in —
        // this guards the shared gate both call before any key load or network request.
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        // Default settings: cloud_stt_opt_in = false → consent gate refuses.
        let denied = commands::require_cloud_stt_consent(&state);
        assert!(denied.is_err(), "biometric audio egress must be refused without opt-in");
        assert!(denied.unwrap_err().contains("opt-in"), "error should name the missing consent");
        // After explicit opt-in, the gate allows it.
        state.settings.lock().unwrap().cloud_stt_opt_in = true;
        assert!(commands::require_cloud_stt_consent(&state).is_ok(), "opt-in permits Scribe egress");
    }

    #[test]
    fn cloud_llm_channels_require_opt_in() {
        // Regression: run_dpo_update POSTs private transcript-derived preference pairs outbound, so it
        // must refuse without explicit cloud-LLM opt-in (the endpoint allow-list is not consent).
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        assert!(
            commands::require_cloud_llm_consent(&state).is_err(),
            "cloud-LLM data egress must be refused without opt-in"
        );
        state.settings.lock().unwrap().cloud_llm_opt_in = true;
        assert!(commands::require_cloud_llm_consent(&state).is_ok(), "opt-in permits cloud-LLM egress");
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
        // ensure_cancel_token recovers the poisoned lock (does not panic) AND, per the round-15
        // hardening, hands out a FRESH non-cancelled token rather than the lingering cancelled one.
        assert!(!state.ensure_cancel_token().expect("recover cancel token").is_cancelled());
    }

    #[test]
    fn batch_token_is_fresh_even_when_finish_batch_left_the_slot_torn() {
        // Round-15 TOCTOU: finish_batch flips the gate to Idle and clears the cancel token under a
        // SEPARATE lock, so a re-clicked batch can start (gate Idle) while a just-cancelled token still
        // lingers in the slot. Reproduce that torn window directly: a cancelled token sits in the slot
        // while the gate reads Idle. ensure_cancel_token must NOT hand that cancelled token to the new
        // batch — doing so would trip its first is_cancelled() check and silently no-op the whole run.
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        // Batch 1 ran and was cancelled; its token is still in the slot.
        let t1 = state.ensure_cancel_token().expect("batch token 1");
        assert!(state.cancel_current_operation());
        assert!(t1.is_cancelled());
        // Simulate ONLY finish_batch's gate flip (the torn window before the token is cleared).
        *state.lock_batch_state() = BatchState::Idle;

        // Batch 2 starts in that window and requests its token.
        state.try_start_batch().expect("start batch 2");
        let t2 = state.ensure_cancel_token().expect("batch token 2");
        assert!(!t2.is_cancelled(), "a new batch must never inherit a cancelled token from a torn slot");
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
