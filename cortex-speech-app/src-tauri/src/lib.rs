// Production code must handle errors explicitly: `.unwrap()`/`.expect()` are denied
// outside of tests. Reviewed, infallible exceptions are grandfathered with a local
// `#[allow(clippy::unwrap_used)]` plus justification (see e.g. `normalizer.rs`).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

// Cargo's `rustc-link-arg-tests` reaches declared integration-test targets but not this package's
// library unit-test harness. Link the compiled RT_MANIFEST resource from the test-configured crate
// itself. Normal binaries compile the library without `cfg(test)`, so they cannot receive this
// second manifest resource.
#[cfg(all(test, target_os = "windows"))]
#[link(name = "windows-test-common-controls-object-archive", kind = "static", modifiers = "+whole-archive")]
unsafe extern "C" {}

pub mod agentic;
pub mod aligner;
pub mod api_keys;
pub mod asr;
pub mod atomic_file;
pub mod audio;
pub mod audio_quality;
mod backup_service;
pub mod cache;
pub mod cancel;
pub mod champion_promotion;
pub mod champion_promotion_runtime;
pub mod chunking;
pub mod commands;
pub mod constrained_decode;
pub mod corrections;
pub mod couch;
pub mod crash;
mod database_runtime;
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
pub mod voice_focus;
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
pub mod ipc_contract;
pub mod jobs;
pub mod jury;
pub mod llm_refiner;
pub mod media;
pub mod media_materialization_worker;
pub mod migrations;
pub mod models;
pub mod normalizer;
pub mod pipeline;
pub mod production_dataset;
pub mod quality;
mod recovery;
pub mod registry;
mod restore_service;
pub mod review_campaign;
pub mod review_pilot;
pub mod review_pool;
pub mod review_pool_export;
pub mod runs;
pub mod scorecard;
pub mod secret_redaction;
pub mod session;
pub mod settings;
pub mod significance;
pub mod snapshot;
pub mod source_provenance;
pub mod stats;
mod stores;
pub mod technical_audio_probe;
pub mod telemetry;
#[cfg(test)]
pub(crate) mod test_support;
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
use database_runtime::DatabaseRuntime;
use db::Database;
use fingerprint::AudioFingerprint;
use history::HistoryManager;
use media::MediaRegistry;
use models::ModelManager;
use normalizer::SoraniNormalizer;
use pipeline::ProcessingPipeline;
use session::SessionManager;
use settings::AppSettings;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

// A nine-minute monotonic schedule leaves one minute for capture and ordinary scheduler jitter while
// preserving the private-production RPO <= 10 minutes. Advancing from the prior deadline (rather than
// sleeping after each completed backup) prevents snapshot duration from accumulating as cadence drift.
const SNAPSHOT_TARGET_RPO_SECS: u64 = 10 * 60;
const SNAPSHOT_CAPTURE_JITTER_MARGIN_SECS: u64 = 60;
const SNAPSHOT_INTERVAL_SECS: u64 = SNAPSHOT_TARGET_RPO_SECS - SNAPSHOT_CAPTURE_JITTER_MARGIN_SECS;

fn next_snapshot_deadline(previous_deadline: Instant, interval: Duration, now: Instant) -> Instant {
    let mut next = previous_deadline + interval;
    while next <= now {
        next += interval;
    }
    next
}

/// Clonable access to the AppState database for blocking worker tasks.
///
/// Unlike exposing the raw `Arc<Mutex<Database>>`, every lock acquisition passes through the
/// restore admission gate. This matters for work queued before a restore: it must not acquire the
/// database between the mandatory safety snapshot / page swap and the snapshot restore's final
/// configuration + history updates.
#[derive(Clone)]
pub(crate) struct AppDatabaseHandle {
    inner: DatabaseRuntime,
}

impl AppDatabaseHandle {
    pub(crate) fn lock(&self) -> std::sync::LockResult<MutexGuard<'_, Database>> {
        self.inner.lock()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportState {
    Idle,
    Running,
}

/// Exact, renderer-queryable admission state for one caller-generated import run identity.
///
/// The import command response can be lost after the worker has already been admitted. Keeping this
/// authority beside the single-flight gate lets the renderer distinguish that ambiguous transport
/// failure from a definite pre-admission refusal without guessing from progress events. Terminal
/// identities are retained only for the short reconciliation window; UUID collision/replay outside
/// the window remains cryptographically negligible and every live identity is always exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportRunAdmission {
    Running,
    Settled,
    Rejected,
    Unknown,
}

const IMPORT_RUN_TERMINAL_HISTORY: usize = 64;

#[derive(Debug, Default)]
struct ImportRunTracker {
    active: Option<String>,
    terminal: VecDeque<(String, ImportRunAdmission)>,
}

impl ImportRunTracker {
    fn terminal_status(&self, run_id: &str) -> ImportRunAdmission {
        self.terminal
            .iter()
            .rev()
            .find_map(|(known_id, status)| (known_id == run_id).then_some(*status))
            .unwrap_or(ImportRunAdmission::Unknown)
    }

    fn status(&self, run_id: &str) -> ImportRunAdmission {
        if self.active.as_deref() == Some(run_id) {
            ImportRunAdmission::Running
        } else {
            self.terminal_status(run_id)
        }
    }

    fn remember_terminal(&mut self, run_id: String, status: ImportRunAdmission) {
        self.terminal.retain(|(known_id, _)| known_id != &run_id);
        self.terminal.push_back((run_id, status));
        while self.terminal.len() > IMPORT_RUN_TERMINAL_HISTORY {
            self.terminal.pop_front();
        }
    }
}

/// Holds the import single-flight mutex while an interrupted-import journal is inspected or
/// mutated. Recovery commands must keep this guard alive for the complete database operation: a
/// check followed by an unlocked read/delete lets a new worker enter between the two and turns its
/// live journal into something the renderer can discard.
#[must_use = "dropping the admission reopens the import gate"]
pub(crate) struct ImportRecoveryAdmission<'a> {
    _state: MutexGuard<'a, ImportState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchState {
    Idle,
    Running,
}

/// Exact operation kind attached to a caller-generated batch identity. Keeping the kind in the
/// backend authority prevents a delayed event or status response for one batch domain from being
/// mistaken for another merely because both share the process-wide single-flight gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchOperation {
    Transcribe,
    Normalize,
}

/// Renderer-queryable admission truth for an exact batch operation identity. Like import-run
/// admission, this only proves whether native work was admitted and has stopped; refreshed database
/// reads and terminal events remain authoritative for the work's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchRunAdmission {
    Running,
    Settled,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchRunDisposition {
    Completed,
    Halted,
    Cancelled,
    Panicked,
}

/// Bounded terminal result retained beside admission truth. Desktop events are best-effort; this
/// snapshot makes a lost terminal event distinguishable from a clean completion and makes a worker
/// panic an explicit hard stop rather than an apparently successful settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchRunOutcome {
    pub disposition: BatchRunDisposition,
    pub total: usize,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
    pub abandoned: u32,
    pub cancelled: bool,
    pub error_code: Option<String>,
}

impl BatchRunOutcome {
    fn panicked(total: usize) -> Self {
        Self {
            disposition: BatchRunDisposition::Panicked,
            total,
            succeeded: 0,
            failed: 0,
            skipped: 0,
            abandoned: total as u32,
            cancelled: false,
            error_code: Some("BATCH_WORKER_PANICKED".into()),
        }
    }
}

/// Pre-worker batch admission claim. If OS thread creation or any setup after gate acquisition
/// fails, Drop records the exact operation as rejected and reopens the single-flight gate.
#[must_use = "disarm only after the batch worker has been created"]
pub(crate) struct ClaimedBatchStart<'a> {
    state: &'a AppState,
    operation_id: &'a str,
    operation: BatchOperation,
    armed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchStartCommitError {
    Cancelled,
    AuthorityLost,
}

/// Short linearization guard for a cancellation check/start transition. Never hold it across the
/// streamed durable admission; the UI-facing Cancel control must stay responsive while up to
/// 100,000 journal items are inserted. Commands acquire it once before admission and once around
/// the final OS spawn, checking the atomic token between those phases.
#[must_use = "hold this guard only across one short batch-start transition"]
pub(crate) struct BatchStartCommit<'a> {
    _cancel_slot: MutexGuard<'a, Option<CancellationToken>>,
}

impl<'a> ClaimedBatchStart<'a> {
    pub(crate) fn new(state: &'a AppState, operation_id: &'a str, operation: BatchOperation) -> Self {
        Self { state, operation_id, operation, armed: true }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ClaimedBatchStart<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.state.abort_batch_start(self.operation_id, self.operation);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveBatchRun {
    operation_id: String,
    operation: BatchOperation,
    total: usize,
    phase: BatchRunPhase,
    outcome: Option<BatchRunOutcome>,
    renderer_acknowledged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchRunPhase {
    Starting,
    Durable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalBatchRun {
    operation_id: String,
    operation: BatchOperation,
    admission: BatchRunAdmission,
    outcome: Option<BatchRunOutcome>,
    renderer_acknowledged: bool,
}

const BATCH_RUN_TERMINAL_HISTORY: usize = 64;

#[derive(Debug, Default)]
struct BatchRunTracker {
    active: Option<ActiveBatchRun>,
    terminal: VecDeque<TerminalBatchRun>,
}

impl BatchRunTracker {
    fn status(&self, operation_id: &str) -> (BatchRunAdmission, Option<BatchOperation>, Option<BatchRunOutcome>) {
        if let Some(active) = self.active.as_ref().filter(|active| active.operation_id == operation_id) {
            return (BatchRunAdmission::Running, Some(active.operation), active.outcome.clone());
        }
        self.terminal
            .iter()
            .rev()
            .find_map(|known| {
                (known.operation_id == operation_id).then_some((
                    known.admission,
                    Some(known.operation),
                    known.outcome.clone(),
                ))
            })
            .unwrap_or((BatchRunAdmission::Unknown, None, None))
    }

    fn remember_terminal(
        &mut self,
        operation_id: String,
        operation: BatchOperation,
        admission: BatchRunAdmission,
        outcome: Option<BatchRunOutcome>,
        renderer_acknowledged: bool,
    ) {
        self.terminal.retain(|known| known.operation_id != operation_id);
        let renderer_acknowledged =
            renderer_acknowledged || admission != BatchRunAdmission::Settled || outcome.is_none();
        self.terminal.push_back(TerminalBatchRun {
            operation_id,
            operation,
            admission,
            outcome,
            renderer_acknowledged,
        });
        while self.terminal.len() > BATCH_RUN_TERMINAL_HISTORY {
            self.terminal.pop_front();
        }
    }

    /// Exact process-local identity eligible for renderer adoption. The active run remains eligible
    /// even if its durable header terminalized between two discovery reads; a settled result remains
    /// eligible until the renderer explicitly acknowledges presenting it.
    fn adoptable_identity(&self) -> Option<(String, BatchOperation)> {
        self.active.as_ref().map(|active| (active.operation_id.clone(), active.operation)).or_else(|| {
            self.terminal.iter().rev().find_map(|known| {
                (!known.renderer_acknowledged
                    && known.admission == BatchRunAdmission::Settled
                    && known.outcome.is_some())
                .then_some((known.operation_id.clone(), known.operation))
            })
        })
    }

    fn acknowledge_renderer(&mut self, operation_id: &str) -> bool {
        if let Some(active) = self.active.as_mut().filter(|active| active.operation_id == operation_id) {
            active.renderer_acknowledged = true;
            return true;
        }
        let Some(known) = self.terminal.iter_mut().rev().find(|known| {
            known.operation_id == operation_id
                && known.admission == BatchRunAdmission::Settled
                && known.outcome.is_some()
        }) else {
            return false;
        };
        known.renderer_acknowledged = true;
        true
    }
}

/// Stable machine code returned by every production audio-import entry point when the cross-run
/// duplicate index could not be proved authoritative at startup.
pub const DEDUP_INDEX_UNAVAILABLE_CODE: &str = "DEDUP_INDEX_UNAVAILABLE";

/// Keep this message stable while import commands still use their legacy string-error adapter. The
/// leading machine code is deliberately separate from the human action text so callers never need to
/// classify a database error or a localized sentence.
pub const DEDUP_INDEX_UNAVAILABLE_MESSAGE: &str = "DEDUP_INDEX_UNAVAILABLE: Audio import is disabled because the cross-run duplicate index could not be verified. Repair or backfill audio identities, then restart Cortex.";
pub const INTERRUPTED_IMPORT_RECOVERY_REQUIRED_MESSAGE: &str =
    "IMPORT_RECOVERY_REQUIRED: Resume or discard the interrupted import before starting another import.";
pub const IMPORT_RECOVERY_AUTHORITY_UNAVAILABLE_MESSAGE: &str = "IMPORT_RECOVERY_AUTHORITY_UNAVAILABLE: Cortex could not verify interrupted-import recovery state. Retry the recovery check before importing.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupUnavailableReason {
    IdentityReadFailed,
    IncompleteAudioIdentities { recordings: usize },
}

/// Startup-owned import admission state. An unavailable index degrades only audio import; the app is
/// still allowed to open the library, review existing work, recover data, and export eligible rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupReadiness {
    Ready { rehydrated_recordings: usize },
    Unavailable(DedupUnavailableReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedupIndexUnavailable;

impl DedupIndexUnavailable {
    pub const fn code(self) -> &'static str {
        DEDUP_INDEX_UNAVAILABLE_CODE
    }
}

impl std::fmt::Display for DedupIndexUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(DEDUP_INDEX_UNAVAILABLE_MESSAGE)
    }
}

impl std::error::Error for DedupIndexUnavailable {}

impl DedupReadiness {
    pub fn require_import_ready(&self) -> Result<(), DedupIndexUnavailable> {
        match self {
            Self::Ready { .. } => Ok(()),
            Self::Unavailable(_) => Err(DedupIndexUnavailable),
        }
    }
}

/// Rehydrate only after one snapshot has proved that every active recording has both durable identity
/// tiers. A read fault or even one incomplete recording leaves the cache empty and import fail-closed.
pub fn rehydrate_dedup_index(db: &Database, fingerprint: &AudioFingerprint) -> DedupReadiness {
    // This function is intentionally replacement, not merge, semantics. Startup passes a fresh map,
    // while a future lifecycle caller may not; retaining identities from another database generation
    // would create false duplicate refusals just as surely as omitting current identities creates
    // false admissions.
    fingerprint.clear();
    match db.load_audio_identity_inventory() {
        Ok((_known, incomplete_recordings)) if incomplete_recordings > 0 => {
            tracing::error!(
                incomplete_recordings,
                dedup_error_code = DEDUP_INDEX_UNAVAILABLE_CODE,
                "Audio import disabled: active recordings have incomplete durable identities"
            );
            DedupReadiness::Unavailable(DedupUnavailableReason::IncompleteAudioIdentities {
                recordings: incomplete_recordings,
            })
        }
        Ok((known, _)) => {
            let rehydrated_recordings = fingerprint.rehydrate(known);
            tracing::info!(
                rehydrated_recordings,
                "Audio dedup: rehydrated authoritative recording identities from the library"
            );
            DedupReadiness::Ready { rehydrated_recordings }
        }
        Err(error) => {
            tracing::error!(
                dedup_error_code = DEDUP_INDEX_UNAVAILABLE_CODE,
                %error,
                "Audio import disabled: durable recording identities could not be read"
            );
            DedupReadiness::Unavailable(DedupUnavailableReason::IdentityReadFailed)
        }
    }
}

pub struct AppState {
    // Arc so a slow command can clone the handle and move DB work into `spawn_blocking` (off the
    // main/UI thread) without borrowing `State` across an await. lock_db() still returns a guard.
    pub(crate) db: DatabaseRuntime,
    pub pipeline: Mutex<ProcessingPipeline>,
    pub normalizer: Arc<SoraniNormalizer>,
    pub cache: Arc<TranscriptCache>,
    pub fingerprint: Arc<AudioFingerprint>,
    pub(crate) dedup_readiness: DedupReadiness,
    pub history: HistKeyMgr,
    pub session: Mutex<SessionManager>,
    pub settings: Mutex<AppSettings>,
    /// Serializes the complete compare/save/publish settings transaction. `settings` alone cannot
    /// cover the pipeline update without violating the pipeline-before-settings lock hierarchy;
    /// this operation gate preserves writer order while each inner lock remains short-lived.
    settings_write: Mutex<()>,
    pub data_dir: Mutex<Option<PathBuf>>,
    pub model_manager: Mutex<ModelManager>,
    /// Separate cancellation slots per operation kind. Native file pickers have no import-run
    /// identity yet, so they need their own slot; imports and batches each retain their existing
    /// gates. The single Cancel control signals every slot and therefore cannot miss a picker whose
    /// callback was lost by the Windows dialog bridge.
    pub file_picker_cancel_token: Mutex<Option<CancellationToken>>,
    pub import_cancel_token: Mutex<Option<CancellationToken>>,
    pub batch_cancel_token: Mutex<Option<CancellationToken>>,
    pub import_state: Mutex<ImportState>,
    import_run_tracker: Mutex<ImportRunTracker>,
    pub batch_state: Mutex<BatchState>,
    batch_run_tracker: Mutex<BatchRunTracker>,
    pub media_registry: Arc<Mutex<MediaRegistry>>,
    pub(crate) media_materializer: Arc<crate::media::MediaMaterializationCoordinator>,
}

type HistKeyMgr = Arc<Mutex<HistoryManager>>;

impl AppState {
    fn lock_import_state(&self) -> MutexGuard<'_, ImportState> {
        self.import_state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned import state lock");
            poisoned.into_inner()
        })
    }

    fn lock_import_run_tracker(&self) -> MutexGuard<'_, ImportRunTracker> {
        self.import_run_tracker.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned import run tracker lock");
            poisoned.into_inner()
        })
    }

    pub(crate) fn remember_import_rejection(&self, run_id: &str) {
        let mut tracker = self.lock_import_run_tracker();
        if tracker.status(run_id) == ImportRunAdmission::Unknown {
            tracker.remember_terminal(run_id.to_string(), ImportRunAdmission::Rejected);
        }
    }

    fn lock_import_cancel_token(&self) -> MutexGuard<'_, Option<CancellationToken>> {
        self.import_cancel_token.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned import cancellation token lock");
            poisoned.into_inner()
        })
    }

    fn lock_file_picker_cancel_token(&self) -> MutexGuard<'_, Option<CancellationToken>> {
        self.file_picker_cancel_token.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned file-picker cancellation token lock");
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

    fn lock_batch_run_tracker(&self) -> MutexGuard<'_, BatchRunTracker> {
        self.batch_run_tracker.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned batch run tracker lock");
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

    pub(crate) fn lock_settings_write(&self) -> MutexGuard<'_, ()> {
        self.settings_write.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned settings write gate");
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

    /// Raw history access for restore publication only. A restore worker must clear old-generation
    /// undo/redo entries before its reservation can leave that worker: if the async IPC future is
    /// cancelled after SQLite publication, command-local cleanup will never run.
    pub(crate) fn history_arc_for_restore(&self) -> Arc<Mutex<HistoryManager>> {
        Arc::clone(&self.history)
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

    /// A clonable, restore-gated DB handle for moving blocking work into `spawn_blocking`.
    pub(crate) fn db_arc(&self) -> AppDatabaseHandle {
        AppDatabaseHandle { inner: self.db.clone() }
    }

    /// Bounded query-only connection authority for read-heavy blocking work.
    pub(crate) fn db_runtime(&self) -> DatabaseRuntime {
        self.db.clone()
    }

    /// Query-domain store for segment/library/review reads. Command handlers retain validation and
    /// DTO mapping but do not receive a raw connection for this migrated domain.
    pub(crate) fn segment_queries(&self) -> crate::stores::SegmentQueryStore {
        crate::stores::SegmentQueryStore::new(self.db.clone())
    }

    pub(crate) fn review_drafts(&self) -> crate::stores::ReviewDraftStore {
        crate::stores::ReviewDraftStore::new(self.db.clone())
    }

    /// Serialized human-review writer for desktop decisions, exact undo and review flags.
    pub(crate) fn review_writes(&self) -> crate::stores::ReviewWriteStore {
        crate::stores::ReviewWriteStore::new(self.db.clone())
    }

    /// Recording-scoped rights, consent withdrawal and provenance query boundary.
    pub(crate) fn rights_store(&self) -> crate::stores::RightsStore {
        crate::stores::RightsStore::new(self.db.clone())
    }

    /// Durable job-center and interrupted-import query/write boundary.
    pub(crate) fn job_store(&self) -> crate::stores::JobStore {
        crate::stores::JobStore::new(self.db.clone())
    }

    /// Reviewer-compensation overview and immutable-settlement boundary (owner desktop only).
    pub(crate) fn compensation_store(&self) -> crate::stores::CompensationStore {
        crate::stores::CompensationStore::new(self.db.clone())
    }

    /// Durable batch admission, execution and recovery boundary.  The returned store can create a
    /// lease that keeps restore exclusion alive across long-running inference.
    pub(crate) fn batch_store(&self) -> crate::stores::BatchStore {
        crate::stores::BatchStore::new(self.db.clone())
    }

    /// Segment deletion/history and speaker-rename mutation boundary.
    pub(crate) fn segment_writes(&self) -> crate::stores::SegmentWriteStore {
        crate::stores::SegmentWriteStore::new(self.db.clone(), Arc::clone(&self.history))
    }

    pub(crate) fn save_session_view_state(
        &self,
        search_query: String,
        sort_order: String,
        filter_verified: Option<bool>,
    ) -> crate::error::AppResult<()> {
        let mutation = self.db.begin_mutation().map_err(crate::error::AppError::Other)?;
        let db = self.db.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned database lock during session view-state save");
            poisoned.into_inner()
        });
        let mut session = self.lock_session();
        session.set_view_state(search_query, sort_order, filter_verified);
        session.save(&db)
    }

    pub fn session_save(&self) {
        let mutation = match self.db.begin_mutation() {
            Ok(mutation) => mutation,
            Err(error) => {
                tracing::warn!(%error, "Session save refused by database restore admission");
                return;
            }
        };
        let db = self.db.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned database lock during session save");
            poisoned.into_inner()
        });
        if let Err(error) = self.lock_session().save(&db) {
            tracing::error!("Session save failed: {error}");
        }
    }

    /// Best-effort navigation breadcrumb after durable review truth commits. Commands do not need
    /// raw database authority merely to keep restart position current.
    pub(crate) fn persist_review_cursor(&self, segment_id: &str) {
        let mutation = match self.db.begin_mutation() {
            Ok(mutation) => mutation,
            Err(error) => {
                tracing::warn!(%error, segment_id, "Review cursor save refused by database restore admission");
                return;
            }
        };
        let db = self.db.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned database lock during review cursor save");
            poisoned.into_inner()
        });
        let mut session = self.lock_session();
        session.set_current_segment(segment_id);
        if let Err(error) = session.save(&db) {
            tracing::warn!("Review cursor save failed after durable commit: {error}");
        }
    }

    pub fn session_auto_save(&self) {
        let mutation = match self.db.begin_mutation() {
            Ok(mutation) => mutation,
            Err(error) => {
                tracing::warn!(%error, "Session autosave refused by database restore admission");
                return;
            }
        };
        let db = self.db.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned database lock during session autosave");
            poisoned.into_inner()
        });
        if let Err(error) = self.lock_session().auto_save(&db) {
            tracing::error!("Session autosave failed: {error}");
        }
    }

    /// Regression helper for cancellation-slot poisoning and import/batch isolation tests. Real
    /// batches receive a fresh exact token atomically from `try_start_batch_for_run`.
    #[cfg(test)]
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

    /// Claim the one native file-picker slot and arm it for the shared Cancel control. A second
    /// backend caller is rejected even if it bypasses the renderer's synchronous re-entry guard.
    pub(crate) fn try_start_file_picker(&self) -> Result<CancellationToken, String> {
        let mut guard = self.lock_file_picker_cancel_token();
        if guard.as_ref().is_some_and(|token| !token.is_cancelled()) {
            return Err("E_FILE_PICKER_BUSY".into());
        }
        let token = CancellationToken::new();
        *guard = Some(token.clone());
        Ok(token)
    }

    /// Clear only the picker that owns `token`. Exact identity prevents a late command drop from
    /// erasing the cancellation authority of a newer picker.
    pub(crate) fn finish_file_picker(&self, token: &CancellationToken) {
        let mut guard = self.lock_file_picker_cancel_token();
        if guard.as_ref().is_some_and(|current| current.same_instance(token)) {
            *guard = None;
        }
    }

    /// Cancel every running operation. Both slots are signalled so the single Cancel control reliably
    /// stops a running import AND a running batch, regardless of which started last.
    pub fn cancel_current_operation(&self) -> bool {
        let mut cancelled_any = false;
        if let Some(token) = self.lock_file_picker_cancel_token().as_ref() {
            token.cancel();
            cancelled_any = true;
        }
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
        self.lock_file_picker_cancel_token().as_ref().is_some_and(|t| t.is_cancelled())
            || self.lock_import_cancel_token().as_ref().is_some_and(|t| t.is_cancelled())
            || self.lock_batch_cancel_token().as_ref().is_some_and(|t| t.is_cancelled())
    }

    pub(crate) fn require_audio_import_ready(&self) -> Result<(), DedupIndexUnavailable> {
        self.dedup_readiness.require_import_ready()
    }

    /// Admit one recovery-journal operation only while no import worker can be live. The returned
    /// guard deliberately owns the same mutex used by `try_start_import`, closing the entire
    /// check/read-or-delete/start race instead of exposing a racy `is_import_active()` snapshot.
    pub(crate) fn try_import_recovery_admission(&self) -> Option<ImportRecoveryAdmission<'_>> {
        let state = self.lock_import_state();
        if *state == ImportState::Running {
            return None;
        }
        Some(ImportRecoveryAdmission { _state: state })
    }

    pub fn try_start_import(&self) -> Result<(), String> {
        self.try_start_import_for_run(&uuid::Uuid::new_v4().to_string())
    }

    pub(crate) fn try_start_import_for_run(&self, run_id: &str) -> Result<(), String> {
        self.try_start_import_for_run_with_recovery(run_id, false)
    }

    pub(crate) fn try_start_import_for_recovery_run(&self, run_id: &str) -> Result<(), String> {
        self.try_start_import_for_run_with_recovery(run_id, true)
    }

    fn try_start_import_for_run_with_recovery(&self, run_id: &str, recovering: bool) -> Result<(), String> {
        // Import alone degrades when startup could not prove the durable cross-run identity index.
        // This check precedes the Running transition, cancellation-token creation, worker spawn,
        // decoding, and durable import-journal creation for every desktop audio-import command.
        if let Err(error) = self.require_audio_import_ready() {
            self.remember_import_rejection(run_id);
            return Err(error.to_string());
        }
        let mut import = self.lock_import_state();
        // P1.3b: refuse to start while a DB restore is reserved. Checked UNDER the import_state lock (and
        // set-Running is under the same lock) so it can't race prepare_restore's writers_active() read.
        if crate::database_runtime::restore_pending() {
            self.remember_import_rejection(run_id);
            return Err(crate::database_runtime::RESTORE_IN_PROGRESS_MSG.into());
        }
        if *import == ImportState::Running {
            self.remember_import_rejection(run_id);
            return Err("Import already in progress".into());
        }
        if !recovering {
            match self.job_store().find_interrupted_import() {
                Ok(Some(_)) => {
                    self.remember_import_rejection(run_id);
                    return Err(INTERRUPTED_IMPORT_RECOVERY_REQUIRED_MESSAGE.into());
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(%error, "Import refused because interrupted-import authority could not be read");
                    self.remember_import_rejection(run_id);
                    return Err(IMPORT_RECOVERY_AUTHORITY_UNAVAILABLE_MESSAGE.into());
                }
            }
        }
        let mut tracker = self.lock_import_run_tracker();
        if tracker.status(run_id) != ImportRunAdmission::Unknown {
            return Err("Import run identity already used".into());
        }
        tracker.active = Some(run_id.to_string());
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
        let mut import = self.lock_import_state();
        let mut tracker = self.lock_import_run_tracker();
        if let Some(run_id) = tracker.active.take() {
            tracker.remember_terminal(run_id, ImportRunAdmission::Settled);
        }
        *import = ImportState::Idle;
    }

    /// Undo a claimed import gate when no worker was started. This is deliberately distinct from
    /// `finish_import`: the renderer may safely surface the original command error only for a
    /// definitively rejected run, while a settled run proves that admission and execution occurred.
    pub(crate) fn abort_import_start(&self, run_id: &str) {
        *self.lock_import_cancel_token() = None;
        let mut import = self.lock_import_state();
        let mut tracker = self.lock_import_run_tracker();
        if tracker.active.as_deref() == Some(run_id) {
            tracker.active = None;
            tracker.remember_terminal(run_id.to_string(), ImportRunAdmission::Rejected);
            *import = ImportState::Idle;
        } else {
            tracing::error!(run_id, "Refusing to abort a different or untracked import run");
        }
    }

    pub(crate) fn import_run_admission(&self, run_id: &str) -> ImportRunAdmission {
        self.lock_import_run_tracker().status(run_id)
    }

    pub(crate) fn remember_batch_rejection(&self, operation_id: &str, operation: BatchOperation) {
        let mut tracker = self.lock_batch_run_tracker();
        if tracker.status(operation_id).0 == BatchRunAdmission::Unknown {
            tracker.remember_terminal(operation_id.to_string(), operation, BatchRunAdmission::Rejected, None, true);
        }
    }

    pub(crate) fn try_start_batch_for_run(
        &self,
        operation_id: &str,
        operation: BatchOperation,
        total: usize,
    ) -> Result<CancellationToken, String> {
        let mut batch = self.lock_batch_state();
        let mut tracker = self.lock_batch_run_tracker();
        if tracker.status(operation_id).0 != BatchRunAdmission::Unknown {
            return Err("Batch operation identity already used".into());
        }
        // P1.3b: refuse to start a batch while a DB restore is reserved (checked under the batch_state lock).
        if crate::database_runtime::restore_pending() {
            tracker.remember_terminal(operation_id.to_string(), operation, BatchRunAdmission::Rejected, None, true);
            return Err(crate::database_runtime::RESTORE_IN_PROGRESS_MSG.into());
        }
        if *batch == BatchState::Running || tracker.active.is_some() {
            tracker.remember_terminal(operation_id.to_string(), operation, BatchRunAdmission::Rejected, None, true);
            return Err("Batch operation already in progress".into());
        }
        tracker.active = Some(ActiveBatchRun {
            operation_id: operation_id.to_string(),
            operation,
            total,
            phase: BatchRunPhase::Starting,
            outcome: None,
            renderer_acknowledged: false,
        });
        // Arm cancellation before opening any preflight/worker-start window. This assignment occurs
        // only after every refusal check, so a rejected second caller cannot detach the live run.
        let token = CancellationToken::new();
        *self.lock_batch_cancel_token() = Some(token.clone());
        *batch = BatchState::Running;
        Ok(token)
    }

    /// Briefly validate exact ownership and serialize one cancellation check. The returned guard is
    /// intentionally cancel-slot-only: callers must drop it before streamed database admission, then
    /// reacquire it for the short final thread spawn.
    pub(crate) fn commit_batch_start<'a>(
        &'a self,
        operation_id: &str,
        operation: BatchOperation,
        token: &CancellationToken,
    ) -> Result<BatchStartCommit<'a>, BatchStartCommitError> {
        let batch = self.lock_batch_state();
        let tracker = self.lock_batch_run_tracker();
        let owns_gate = *batch == BatchState::Running
            && tracker
                .active
                .as_ref()
                .is_some_and(|active| active.operation_id == operation_id && active.operation == operation);
        if !owns_gate {
            return Err(BatchStartCommitError::AuthorityLost);
        }

        let cancel_slot = self.lock_batch_cancel_token();
        if !cancel_slot.as_ref().is_some_and(|current| current.same_instance(token)) {
            return Err(BatchStartCommitError::AuthorityLost);
        }
        if token.is_cancelled() {
            return Err(BatchStartCommitError::Cancelled);
        }
        drop(tracker);
        drop(batch);
        Ok(BatchStartCommit { _cancel_slot: cancel_slot })
    }

    /// Record the completed journal admission without holding cancellation or batch-state locks.
    pub(crate) fn mark_batch_durable_admitted(&self, operation_id: &str, operation: BatchOperation) -> bool {
        let mut tracker = self.lock_batch_run_tracker();
        let Some(active) = tracker.active.as_mut() else {
            return false;
        };
        if active.operation_id != operation_id
            || active.operation != operation
            || active.phase != BatchRunPhase::Starting
        {
            return false;
        }
        active.phase = BatchRunPhase::Durable;
        true
    }

    /// Exact process-local authority for the pre-journal preflight window. This is intentionally
    /// separate from durable running truth so status code can never mistake a missing journal after
    /// admission for a harmless start phase.
    pub(crate) fn starting_batch_run(&self, operation_id: &str) -> Option<(BatchOperation, usize)> {
        self.lock_batch_run_tracker().active.as_ref().and_then(|active| {
            (active.operation_id == operation_id && active.phase == BatchRunPhase::Starting)
                .then_some((active.operation, active.total))
        })
    }

    /// Persist terminal truth before publishing the best-effort terminal event. A second/different
    /// result for the same live identity is refused so competing worker paths cannot rewrite the
    /// outcome that response-loss reconciliation will later expose.
    pub(crate) fn record_batch_outcome(
        &self,
        operation_id: &str,
        operation: BatchOperation,
        outcome: BatchRunOutcome,
    ) -> bool {
        let mut tracker = self.lock_batch_run_tracker();
        let Some(active) = tracker.active.as_mut() else {
            tracing::error!(operation_id, ?operation, "Refusing outcome for an untracked batch run");
            return false;
        };
        if active.operation_id != operation_id || active.operation != operation || active.total != outcome.total {
            tracing::error!(operation_id, ?operation, "Refusing mismatched batch terminal outcome");
            return false;
        }
        if active.outcome.is_some() {
            tracing::error!(operation_id, ?operation, "Refusing duplicate batch terminal outcome");
            return false;
        }
        active.outcome = Some(outcome);
        true
    }

    /// Release only the exact worker that owns the gate. The state/tracker locks remain held while
    /// the cancellation slot is cleared and the terminal record is committed, so a newer batch can
    /// neither enter nor lose its token to a delayed old guard.
    pub(crate) fn finish_batch_for_run(&self, operation_id: &str, operation: BatchOperation) -> bool {
        let mut batch = self.lock_batch_state();
        let mut tracker = self.lock_batch_run_tracker();
        let owns_gate = tracker
            .active
            .as_ref()
            .is_some_and(|active| active.operation_id == operation_id && active.operation == operation);
        if !owns_gate {
            tracing::error!(operation_id, ?operation, "Refusing to finish a different or untracked batch run");
            return false;
        }

        let Some(active) = tracker.active.take() else {
            tracing::error!(operation_id, ?operation, "Batch ownership disappeared during exact settlement");
            return false;
        };
        *self.lock_batch_cancel_token() = None;
        let outcome = active.outcome.or_else(|| Some(BatchRunOutcome::panicked(active.total)));
        tracker.remember_terminal(
            operation_id.to_string(),
            operation,
            BatchRunAdmission::Settled,
            outcome,
            active.renderer_acknowledged,
        );
        *batch = BatchState::Idle;
        true
    }

    /// Undo a claimed batch gate when worker creation fails. This records a definitive rejection so
    /// a renderer whose command channel failed can reconcile without guessing that native work ran.
    pub(crate) fn abort_batch_start(&self, operation_id: &str, operation: BatchOperation) {
        let mut batch = self.lock_batch_state();
        let mut tracker = self.lock_batch_run_tracker();
        let owns_gate = tracker
            .active
            .as_ref()
            .is_some_and(|active| active.operation_id == operation_id && active.operation == operation);
        if !owns_gate {
            tracing::error!(operation_id, ?operation, "Refusing to abort a different or untracked batch run");
            return;
        }

        *self.lock_batch_cancel_token() = None;
        tracker.active = None;
        tracker.remember_terminal(operation_id.to_string(), operation, BatchRunAdmission::Rejected, None, true);
        *batch = BatchState::Idle;
    }

    pub(crate) fn batch_run_admission(
        &self,
        operation_id: &str,
    ) -> (BatchRunAdmission, Option<BatchOperation>, Option<BatchRunOutcome>) {
        self.lock_batch_run_tracker().status(operation_id)
    }

    pub(crate) fn adoptable_batch_run_identity(&self) -> Option<(String, BatchOperation)> {
        self.lock_batch_run_tracker().adoptable_identity()
    }

    pub(crate) fn acknowledge_batch_run_renderer(&self, operation_id: &str) -> bool {
        self.lock_batch_run_tracker().acknowledge_renderer(operation_id)
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
            // R3: other background writers can escape the db-Mutex serialization the restore relies
            // on. Dedicated jury/alignment writers register one shared BgDbWriterGuard counter; the
            // Couch phone-review server is fenced separately because it can persist on submit.
            // Each was a real "restore mixes late writes into the just-restored library" hole. New
            // dedicated-connection writers register a BgDbWriterGuard rather than growing this chain.
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

    // A named snapshot restore spans SQLite plus several dataset-coupled files. If a crash or a
    // post-page-swap error left its durable marker, recover that exact transaction synchronously
    // under the single-instance lock BEFORE schema initialization, the startup job reaper, snapshots,
    // Couch resume, or any background writer can observe/mutate a mixed generation. The recovery path
    // retries the recorded target and falls back to the verified full pre-restore pin; uncertainty is
    // fatal rather than permission to start normally.
    match crate::commands::recover_interrupted_named_restore_at_startup(&data_dir) {
        Ok(true) => tracing::warn!("interrupted database/config restore was recovered before startup"),
        Ok(false) => {}
        Err(error) => {
            fatal_app_error(format!("Recovery-required database restore could not be completed safely: {error}"))
        }
    }

    let db_path = data_dir.join("cortex-speech.db");
    let db = match Database::open_with_retry(db_path.to_string_lossy().as_ref()) {
        Ok(db) => db,
        Err(e) => fatal_app_error(format!("Failed to open database at {:?}: {e}", db_path)),
    };
    // Any pending migration on an established profile is allowed only after a rotation-exempt copy
    // of the exact pre-upgrade DB has been promoted successfully. This is fail-closed because a
    // semantically wrong migration can commit cleanly; SQL atomicity cannot make that data recoverable.
    match crate::snapshot::initialize_with_required_pre_migration_pin(&db, &data_dir) {
        Ok(Some(path)) => tracing::info!("pre-migration snapshot pinned at {}", path.display()),
        Ok(None) => {}
        Err(e) => fatal_app_error(format!("Failed to safely initialize database schema: {e}")),
    }

    // A live schema-68 batch belongs to a process that no longer exists.  Reconcile it from its
    // immutable item ledger before the generic reaper, snapshots, dedup rehydration, or any worker
    // can observe the database.  This owner-only release deliberately hard-stops interrupted work;
    // it preserves applied items and marks every pending item abandoned instead of guessing how to
    // reconstruct result-affecting runtime configuration after a restart.
    let recovered_batch_history_token = match db.recover_active_batch_job_v1() {
        Ok(Some(status)) => {
            tracing::warn!(
                operation_id = %status.operation_id,
                kind = ?status.kind,
                state = ?status.state,
                "reconciled an interrupted durable batch before startup"
            );
            match db.batch_execution_history_token_v1(&status.operation_id) {
                Ok(token) => token,
                Err(error) => fatal_app_error(format!(
                    "Recovered batch effects could not be bound to exact Undo authority: {error}"
                )),
            }
        }
        Ok(None) => None,
        Err(e) => {
            fatal_app_error(format!("Interrupted batch evidence could not be validated and reconciled safely: {e}"))
        }
    };

    // P0 #3 Job Supervisor: any durable job still `running` at startup is a crash residue (a clean run
    // always reaches a terminal state) — reap it to failed/INTERRUPTED so the activity surface shows the
    // honest "interrupted", never a ghost that spins forever. Best-effort: never blocks startup.
    match db.mark_orphaned_running_jobs_failed() {
        Ok(0) => {}
        Ok(n) => tracing::info!("reaped {n} interrupted job(s) from a previous crash"),
        Err(e) => tracing::warn!("startup job reaper failed: {e}"),
    }

    // P3.1/M0.4b: rotating auto-snapshots of the DB + config state. One on startup (so a corruption is
    // recoverable from the moment the app runs), then on a fixed nine-minute monotonic cadence. The
    // one-minute capture/jitter margin keeps the measured recovery point within the ten-minute target
    // without cumulative drift. Skipped in headless test modes.
    const SNAPSHOT_KEEP: usize = 10;
    if !smoke_test {
        match crate::snapshot::take_snapshot(&db, &data_dir, SNAPSHOT_KEEP) {
            Ok(Some(_)) => {}
            Ok(None) => tracing::warn!("startup DB snapshot skipped by the empty-DB guard"),
            Err(e) => tracing::warn!("startup DB snapshot failed: {e}"),
        }
        let snap_db_path = db_path.clone();
        let snap_data_dir = data_dir.clone();
        std::thread::spawn(move || {
            let interval = Duration::from_secs(SNAPSHOT_INTERVAL_SECS);
            let mut deadline = Instant::now() + interval;
            loop {
                if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
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
                deadline = next_snapshot_deadline(deadline, interval, Instant::now());
            }
        });
    }

    let settings_path = data_dir.join("settings.json");
    // Clamp stale/on-disk alternatives before any warm-up or pipeline construction. The debug-only
    // integration override below remains the one explicit desktop diagnostic escape hatch.
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut settings = AppSettings::load_production(&settings_path);
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
    // Import is a degradable capability, not a startup prerequisite. A read fault or legacy unhashed
    // recording keeps review/library/export available but makes every audio import fail closed with a
    // stable code before it can decode, journal, or publish anything.
    let dedup_readiness = rehydrate_dedup_index(&db, &fingerprint);

    let database_runtime = DatabaseRuntime::new(db);
    let pipeline = ProcessingPipeline::new_with_runtime(
        db_path.to_string_lossy().to_string(),
        Arc::clone(&normalizer),
        Arc::clone(&cache),
        Arc::clone(&fingerprint),
        Arc::new(settings.clone()),
        Arc::new(ModelManager::new(data_dir.join("models"))),
        database_runtime.clone(),
    );

    let history = HistoryManager::new(500);
    if let Some(token) = recovered_batch_history_token {
        if let Err(error) = history.record_batch_token(token) {
            fatal_app_error(format!("Recovered batch Undo authority could not be installed: {error}"));
        }
    }
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

    let media_registry = Arc::new(Mutex::new(MediaRegistry::default()));
    let protocol_media_registry = Arc::clone(&media_registry);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol(
            crate::media::MEDIA_PROTOCOL_SCHEME,
            move |_context, request, responder| {
                let Some(permit) = crate::media::try_acquire_media_protocol_worker() else {
                    responder.respond(crate::media::media_protocol_busy_response());
                    return;
                };
                let registry = Arc::clone(&protocol_media_registry);
                tauri::async_runtime::spawn_blocking(move || {
                    let _permit = permit;
                    responder.respond(crate::media::serve_media_protocol_request(&registry, request));
                });
            },
        )
        .manage(AppState {
            db: database_runtime,
            pipeline: Mutex::new(pipeline),
            normalizer,
            cache,
            fingerprint,
            dedup_readiness,
            history: Arc::new(Mutex::new(history)),
            session: Mutex::new(session),
            settings: Mutex::new(settings),
            settings_write: Mutex::new(()),
            data_dir: Mutex::new(Some(data_dir)),
            model_manager: Mutex::new(model_manager),
            file_picker_cancel_token: Mutex::new(None),
            import_cancel_token: Mutex::new(None),
            batch_cancel_token: Mutex::new(None),
            import_state: Mutex::new(ImportState::Idle),
            import_run_tracker: Mutex::new(ImportRunTracker::default()),
            batch_state: Mutex::new(BatchState::Idle),
            batch_run_tracker: Mutex::new(BatchRunTracker::default()),
            media_registry,
            media_materializer: Arc::new(crate::media::MediaMaterializationCoordinator::default()),
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_health,
            commands::take_last_crash,
            commands::app_git_sha,
            commands::get_review_compensation_overview_v1,
            commands::record_review_compensation_settlement_v1,
            commands::open_audio_file,
            commands::import_directory,
            commands::get_import_run_status,
            commands::get_batch_run_status,
            commands::get_active_batch_run,
            commands::acknowledge_batch_run,
            commands::get_interrupted_import,
            commands::resume_interrupted_import,
            commands::discard_interrupted_import,
            commands::import_audio_file,
            commands::transcribe_segment,
            commands::batch_transcribe,
            commands::normalize_text,
            commands::align_segment,
            commands::get_segment_consensus,
            commands::get_segment,
            commands::get_segments_page,
            commands::get_segment_ids_for_view,
            commands::get_signal_anomaly_segments,
            commands::update_segment,
            commands::update_segment_metadata_v1,
            commands::delete_segments_v1,
            commands::rename_speaker_v1,
            commands::merge_dataset_json,
            commands::export_dataset,
            commands::export_transcript,
            commands::get_jobs,
            commands::get_champion_engine_status,
            commands::start_champion_engine,
            commands::export_dataset_bundle,
            commands::export_huggingface_dataset,
            commands::list_agent_import_reports,
            commands::get_agent_import_report_by_run_id,
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
            commands::register_media_asset,
            commands::register_review_media_asset,
            commands::get_media_asset_url,
            commands::check_agentic_readiness,
            commands::rediarize_segments,
            commands::get_audio_duration,
            commands::get_waveform,
            commands::get_dataset_stats,
            commands::get_speaker_inventory_v1,
            commands::get_dataset_quality,
            commands::get_training_grade_breakdown,
            commands::set_recording_rights,
            commands::revoke_recording_consent,
            commands::list_recording_rights,
            commands::get_settings,
            commands::update_settings,
            commands::get_settings_v1,
            commands::patch_settings_v1,
            commands::set_cloud_consent_v1,
            commands::get_fingerprint_count,
            commands::undo,
            commands::redo,
            commands::get_history_status_v1,
            commands::compute_diff,
            commands::validate_dataset_cmd,
            commands::export_audio,
            commands::batch_verify,
            commands::assign_speakers_v1,
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
            commands::run_gold_eval_asr,
            commands::build_scorecard,
            commands::list_eval_runs,
            commands::get_label_quality_lift,
            commands::list_gold_segments,
            // Phase 2 — T0 Gate + Jury
            commands::run_t0_gate,
            commands::get_escalation_queue,
            commands::get_active_voice_focus_v1,
            commands::get_review_page_v1,
            commands::record_human_decision,
            commands::commit_review_v1,
            commands::mark_segment_unusable_v1,
            commands::get_review_draft_v1,
            commands::reserve_review_draft_write_v1,
            commands::save_review_draft_v1,
            commands::delete_review_draft_v1,
            commands::get_desktop_review_undo_target_v1,
            commands::undo_desktop_review_action_v1,
            commands::record_review_flag,
            commands::begin_desktop_playback_session_v1,
            commands::cancel_desktop_playback_session_v1,
            commands::finalize_desktop_playback_session_v1,
            commands::record_playback_receipt,
            commands::get_few_shot_examples,
            commands::get_escalation_rate_trend,
            commands::run_dpo_update,
            // Phase 1+2: Full pipeline + T2 direct
            commands::run_jury_pipeline,
            commands::run_t2_for_segment,
        ])
        .setup(|app| {
            use tauri::Manager;
            // The renderer receives no filesystem scope. Startup owns private-cache maintenance;
            // playable bytes leave only through the live UUID-bound `cortex-media` protocol.
            if let Some(data_dir) = app.state::<AppState>().lock_data_dir().clone() {
                let media_cache = crate::media::media_cache_dir(&data_dir);
                if let Err(e) = std::fs::create_dir_all(&media_cache) {
                    tracing::warn!("Could not create media cache dir {}: {e}", media_cache.display());
                } else {
                    // No media command can run before setup completes. This is the only safe point
                    // for directory-wide orphan cleanup; runtime builders prune only exact grants so
                    // one parallel materialization can never delete another's unpublished file.
                    crate::media::prune_media_cache_on_startup(&media_cache);
                }
            }

            // Promotion recovery MUST precede ordinary pointer publication and supervision. The job's
            // durable saga payload is the only source that can say whether a crash left the candidate
            // or incumbent authoritative. Publishing the current DB row first could overwrite that
            // evidence boundary and start a half-promoted model. Recovery uses independent DB
            // connections and never holds AppState's mutex across restart/warm-up/canary work.
            {
                let state = app.state::<AppState>();
                let data_dir = state.lock_data_dir().clone();
                let db_path = state.lock_db().path().to_string();
                let running = {
                    let db = state.lock_db();
                    crate::champion_promotion::running_promotions(&db)
                };
                match (data_dir, running) {
                    (Some(dir), Ok(jobs)) if jobs.is_empty() => {
                        let db = state.lock_db();
                        match crate::registry::sync_champion_pointer(&db, &dir) {
                            Ok(_) => crate::engine_runtime::set_promotion_recovery_blocked(false),
                            Err(error) => {
                                crate::engine_runtime::set_promotion_recovery_blocked(true);
                                tracing::error!("champion pointer publication failed at startup: {error}");
                            }
                        }
                    }
                    (Some(dir), Ok(_)) => {
                        // A 30-GB worker can need minutes to restart. Keep setup responsive, but fence
                        // every engine start until the background recovery and final pointer sync pass.
                        crate::engine_runtime::set_promotion_recovery_blocked(true);
                        let recovery_app = app.handle().clone();
                        match crate::database_runtime::begin_mutation() {
                            Ok(mutation) => {
                                std::thread::spawn(move || {
                                    let _mutation = mutation;
                                    match crate::champion_promotion_runtime::recover_running_promotions(
                                        &recovery_app,
                                        db_path.clone(),
                                        dir.clone(),
                                    ) {
                                        Ok(recovered) => match Database::open(&db_path)
                                            .and_then(|db| crate::registry::sync_champion_pointer(&db, &dir))
                                        {
                                            Ok(_) => {
                                                tracing::info!(
                                                    "recovered {recovered} interrupted champion promotion"
                                                );
                                                crate::engine_runtime::set_promotion_recovery_blocked(false);
                                            }
                                            Err(error) => tracing::error!(
                                                "champion pointer publication failed after recovery; engine remains blocked: {error}"
                                            ),
                                        },
                                        Err(error) => tracing::error!(
                                            "champion promotion recovery is unverified; engine remains blocked: {error}"
                                        ),
                                    }
                                });
                            }
                            Err(error) => tracing::error!(
                                "champion promotion recovery could not acquire the mutation fence; engine remains blocked: {error}"
                            ),
                        }
                    }
                    (None, _) => {
                        crate::engine_runtime::set_promotion_recovery_blocked(true);
                        tracing::error!("champion promotion recovery cannot run: application data directory is missing");
                    }
                    (_, Err(error)) => {
                        crate::engine_runtime::set_promotion_recovery_blocked(true);
                        tracing::error!("champion promotion inventory failed; engine remains blocked: {error}");
                    }
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

/// The explicit `CORTEX_APP_DATA_DIR` override, if set. Shared with supported `bin/*` CLI tools so
/// the app and its batch utilities resolve to the same data dir and single-instance lock. Pure for testing.
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

    #[test]
    fn periodic_snapshot_deadlines_do_not_accumulate_capture_time_or_missed_ticks() {
        let interval = Duration::from_secs(SNAPSHOT_INTERVAL_SECS);
        let first = Instant::now() + interval;

        let after_fast_capture = first + Duration::from_secs(7);
        assert_eq!(
            next_snapshot_deadline(first, interval, after_fast_capture),
            first + interval,
            "a completed capture must advance from the scheduled deadline, not its completion time"
        );

        let after_two_missed_ticks = first + interval * 2 + Duration::from_secs(7);
        assert_eq!(
            next_snapshot_deadline(first, interval, after_two_missed_ticks),
            first + interval * 3,
            "a suspended or overrun process must skip expired ticks without introducing cadence drift"
        );
        assert_eq!(SNAPSHOT_INTERVAL_SECS, 9 * 60);
        assert_eq!(SNAPSHOT_TARGET_RPO_SECS, 10 * 60);
    }

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

    #[cfg(windows)]
    #[test]
    fn every_windows_rust_test_harness_requests_common_controls_v6() {
        // This test can only execute if the just-linked harness itself passed Windows loader
        // activation. Keep a source-level assertion too, so a build-script cleanup cannot silently
        // remove the dependency and make the next clean test run die before reporting any test.
        let build_script = include_str!("../build.rs");
        for required in [
            "embed_resource::compile_for_tests",
            "cc::windows_registry::find_tool",
            "cvtres.exe",
            "windows-test-common-controls-object-archive.lib",
            "cargo:rustc-link-search=native=",
            "windows-test-common-controls.manifest",
            "windows-test-common-controls.rc",
        ] {
            assert!(build_script.contains(required), "Windows test linker lost required binding: {required}");
        }
        for forbidden in ["cargo:rustc-link-arg=/MANIFEST", "cargo:rustc-link-arg=/MANIFESTINPUT"] {
            assert!(
                !build_script.contains(forbidden),
                "a package-wide manifest directive would corrupt normal Tauri binaries: {forbidden}"
            );
        }
        let library_source = include_str!("lib.rs");
        for required in [
            "cfg(all(test, target_os = \"windows\"))",
            "link(name = \"windows-test-common-controls-object-archive\", kind = \"static\", modifiers = \"+whole-archive\")",
        ] {
            assert!(library_source.contains(required), "unit-test harness lost scoped resource linkage: {required}");
        }
        let resource = include_str!("../windows-test-common-controls.rc");
        assert!(
            resource.contains("1 24 \"windows-test-common-controls.manifest\""),
            "Windows test resource must embed numeric RT_MANIFEST type 24 at ID 1"
        );
        let manifest = include_str!("../windows-test-common-controls.manifest");
        for required in
            ["Microsoft.Windows.Common-Controls", "version=\"6.0.0.0\"", "publicKeyToken=\"6595b64144ccf1df\""]
        {
            assert!(manifest.contains(required), "Windows test manifest lost required binding: {required}");
        }
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

        // DatabaseRuntime::open_read creates an independent connection. A SQLite `:memory:` database
        // is private to one connection, so recovery-authority reads would falsely see "no schema".
        // Use a disposable file to characterize the same multi-connection behavior as production.
        let db = Database::open(data_dir.join("app-state.db").to_string_lossy().as_ref()).unwrap();
        db.initialize().unwrap();

        AppState {
            db: DatabaseRuntime::new(db),
            pipeline: Mutex::new(pipeline),
            normalizer,
            cache,
            fingerprint,
            dedup_readiness: DedupReadiness::Ready { rehydrated_recordings: 0 },
            history: Arc::new(Mutex::new(HistoryManager::new(10))),
            session: Mutex::new(SessionManager::new(data_dir.join("session"))),
            settings: Mutex::new(settings),
            settings_write: Mutex::new(()),
            data_dir: Mutex::new(Some(data_dir)),
            model_manager: Mutex::new(model_manager),
            file_picker_cancel_token: Mutex::new(None),
            import_cancel_token: Mutex::new(None),
            batch_cancel_token: Mutex::new(None),
            import_state: Mutex::new(ImportState::Idle),
            import_run_tracker: Mutex::new(ImportRunTracker::default()),
            batch_state: Mutex::new(BatchState::Idle),
            batch_run_tracker: Mutex::new(BatchRunTracker::default()),
            media_registry: Arc::new(Mutex::new(MediaRegistry::default())),
            media_materializer: Arc::new(crate::media::MediaMaterializationCoordinator::default()),
        }
    }

    fn admit_test_normalize_guard(
        state: &Arc<AppState>,
        operation_id: &str,
        segment_ids: &[String],
    ) -> crate::commands::DurableBatchWorkerGuard {
        {
            let db = state.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for segment_id in segment_ids {
                db.insert_segment(&crate::db::SpeechSegment {
                    id: segment_id.clone(),
                    audio_path: format!("C:/fixtures/{segment_id}.wav"),
                    raw_transcript: format!("raw-{segment_id}"),
                    ..Default::default()
                })
                .expect("insert batch-guard fixture");
            }
        }
        let cancel = state
            .try_start_batch_for_run(operation_id, BatchOperation::Normalize, segment_ids.len())
            .expect("claim exact batch process gate");
        let mut claimed = ClaimedBatchStart::new(state.as_ref(), operation_id, BatchOperation::Normalize);
        let restore_generation =
            crate::database_runtime::capture_restore_generation().expect("capture restore generation");
        let (lease, admitted) = state
            .batch_store()
            .admit(crate::stores::BatchAdmissionV1 {
                operation_id,
                kind: crate::db::BatchJobKindV1::Normalize,
                segment_ids,
                config_sha256: &"a".repeat(64),
                executor: crate::commands::new_batch_executor_identity(),
                cancel: cancel.as_atomic(),
                restore_generation,
            })
            .expect("durably admit guard fixture");
        assert_eq!(usize::try_from(admitted.total).unwrap(), segment_ids.len());
        let guard = crate::commands::DurableBatchWorkerGuard::new_for_test(
            Arc::clone(state),
            operation_id.to_string(),
            BatchOperation::Normalize,
            lease,
        );
        assert!(state.mark_batch_durable_admitted(operation_id, BatchOperation::Normalize));
        claimed.disarm();
        guard
    }

    #[test]
    fn dedup_identity_read_failure_degrades_import_only_with_the_stable_code() {
        // An open connection without schema is a deterministic identity-query failure. Startup must
        // retain that as typed readiness state instead of silently treating the library as empty.
        let db = Database::open(":memory:").unwrap();
        let fingerprint = AudioFingerprint::new();
        let readiness = rehydrate_dedup_index(&db, &fingerprint);
        assert_eq!(readiness, DedupReadiness::Unavailable(DedupUnavailableReason::IdentityReadFailed));
        assert_eq!(fingerprint.count(), 0, "a failed inventory read must leave no partially trusted cache");

        let dir = tempfile::tempdir().unwrap();
        let mut state = test_app_state(dir.path().to_path_buf());
        state.dedup_readiness = readiness;
        assert_eq!(
            state.try_start_import(),
            Err(DEDUP_INDEX_UNAVAILABLE_MESSAGE.to_string()),
            "all desktop import entry points share this pre-worker gate"
        );
        assert_eq!(DedupIndexUnavailable.code(), DEDUP_INDEX_UNAVAILABLE_CODE);
        assert_eq!(*state.lock_import_state(), ImportState::Idle, "a refusal must not claim the import gate");
        assert!(state.lock_import_cancel_token().is_none(), "a refusal must not arm a worker cancellation token");

        // The degraded state is deliberately capability-scoped: the application remains usable for
        // existing-library work. Batch state is an independent gate and is representative of the app
        // not entering a global fatal/maintenance mode merely because new audio cannot be admitted.
        let operation_id = uuid::Uuid::from_u128(0x1001).to_string();
        state
            .try_start_batch_for_run(&operation_id, BatchOperation::Normalize, 1)
            .expect("non-import capability remains available");
        assert!(state.finish_batch_for_run(&operation_id, BatchOperation::Normalize));
    }

    #[test]
    fn active_incomplete_recording_identity_blocks_before_any_rehydration() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("dedup-readiness.db");
        let db = Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        crate::snapshot::initialize_with_required_pre_migration_pin(&db, dir.path()).unwrap();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "legacy-unhashed".into(),
            audio_path: "C:/audio/legacy.wav".into(),
            raw_transcript: "دەنگ".into(),
            ..Default::default()
        })
        .unwrap();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "degenerate-identity".into(),
            audio_path: "C:/audio/degenerate.wav".into(),
            raw_transcript: "دەنگ".into(),
            ..Default::default()
        })
        .unwrap();
        db.set_audio_identity(
            "C:/audio/degenerate.wav",
            &crate::fingerprint::AudioIdentity { spectral: 0, content: "a".repeat(64) },
        )
        .unwrap();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "malformed-hash".into(),
            audio_path: "C:/audio/malformed.wav".into(),
            raw_transcript: "دەنگ".into(),
            ..Default::default()
        })
        .unwrap();
        db.set_audio_identity(
            "C:/audio/malformed.wav",
            &crate::fingerprint::AudioIdentity { spectral: 42, content: "not-a-canonical-hash".into() },
        )
        .unwrap();

        let fingerprint = AudioFingerprint::new();
        assert_eq!(
            rehydrate_dedup_index(&db, &fingerprint),
            DedupReadiness::Unavailable(DedupUnavailableReason::IncompleteAudioIdentities { recordings: 3 })
        );
        assert_eq!(
            fingerprint.count(),
            0,
            "complete neighbors must not create a falsely authoritative partial index while one recording is unhashed"
        );
    }

    #[test]
    fn data_dir_override_matches_the_cli_tools() {
        // The app must honor CORTEX_APP_DATA_DIR exactly as bin/*.rs does, or setting it splits the app
        // and the batch importer onto different databases (they share the live DB + single-instance lock).
        assert_eq!(data_dir_override(Some("D:/cortex-data".into())), Some(PathBuf::from("D:/cortex-data")));
        assert_eq!(data_dir_override(None), None, "unset -> fall through to headless/platform resolution");
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
        let operation_id = uuid::Uuid::from_u128(0x2002).to_string();
        state.try_start_batch_for_run(&operation_id, BatchOperation::Normalize, 1).expect("start batch 2");
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
    fn file_picker_is_exclusive_cancellable_and_only_its_owner_can_clear_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let picker = state.try_start_file_picker().expect("claim picker");
        assert_eq!(state.try_start_file_picker().err().as_deref(), Some("E_FILE_PICKER_BUSY"));
        assert!(state.cancel_current_operation(), "the shared control must reach a native picker");
        assert!(picker.is_cancelled());

        let unrelated = CancellationToken::new();
        state.finish_file_picker(&unrelated);
        assert!(state.cancel_current_operation(), "an unrelated finisher cannot erase the slot");
        state.finish_file_picker(&picker);
        assert!(!state.cancel_current_operation(), "the exact owner releases the picker slot");

        let next = state.try_start_file_picker().expect("a fresh picker is admitted");
        assert!(!next.is_cancelled(), "a new picker must not inherit cancellation");
        state.finish_file_picker(&next);
    }

    #[test]
    fn second_batch_gets_a_fresh_token_after_cancel() {
        // Round-2 audit HIGH: ensure_cancel_token is reuse-or-create, so a cancelled batch left a
        // permanently-cancelled token in the slot and every later batch inherited it and no-op'd.
        // finish_batch now clears the slot so the next batch gets a live token.
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let first = uuid::Uuid::from_u128(0x3001).to_string();
        state.try_start_batch_for_run(&first, BatchOperation::Normalize, 1).expect("start batch 1");
        let t1 = state.ensure_cancel_token().expect("batch token 1");
        assert!(state.cancel_current_operation());
        assert!(t1.is_cancelled());
        assert!(state.finish_batch_for_run(&first, BatchOperation::Normalize));

        let second = uuid::Uuid::from_u128(0x3002).to_string();
        state.try_start_batch_for_run(&second, BatchOperation::Normalize, 1).expect("start batch 2");
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
    fn import_run_tracker_distinguishes_running_settled_rejected_and_unknown_exactly() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let completed = "00000000-0000-4000-8000-000000000001";
        let rejected = "00000000-0000-4000-8000-000000000002";
        let unknown = "00000000-0000-4000-8000-000000000003";

        assert_eq!(state.import_run_admission(unknown), ImportRunAdmission::Unknown);
        state.try_start_import_for_run(completed).expect("admit exact run");
        assert_eq!(state.import_run_admission(completed), ImportRunAdmission::Running);
        assert_eq!(state.import_run_admission(unknown), ImportRunAdmission::Unknown);
        state.finish_import();
        assert_eq!(state.import_run_admission(completed), ImportRunAdmission::Settled);

        state.try_start_import_for_run(rejected).expect("admit second exact run");
        let picker_cancel = state.start_cancel_token();
        assert!(state.cancel_current_operation(), "claimed picker must be cancellable");
        assert!(picker_cancel.is_cancelled());
        state.abort_import_start(rejected);
        assert_eq!(state.import_run_admission(rejected), ImportRunAdmission::Rejected);
        assert_eq!(*state.lock_import_state(), ImportState::Idle);
        assert!(!state.cancel_current_operation(), "aborting picker admission must clear its token");
        assert_eq!(
            state.try_start_import_for_run(rejected),
            Err("Import run identity already used".to_string()),
            "a recently terminal identity cannot be replayed"
        );
    }

    #[test]
    fn import_run_tracker_bounds_terminal_reconciliation_history() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let first = uuid::Uuid::from_u128(1).to_string();
        let last = uuid::Uuid::from_u128((IMPORT_RUN_TERMINAL_HISTORY + 1) as u128).to_string();

        for value in 1..=(IMPORT_RUN_TERMINAL_HISTORY + 1) {
            let run_id = uuid::Uuid::from_u128(value as u128).to_string();
            state.try_start_import_for_run(&run_id).expect("admit retained run");
            state.finish_import();
        }

        assert_eq!(state.import_run_admission(&first), ImportRunAdmission::Unknown);
        assert_eq!(state.import_run_admission(&last), ImportRunAdmission::Settled);
        assert_eq!(state.lock_import_run_tracker().terminal.len(), IMPORT_RUN_TERMINAL_HISTORY);
    }

    #[test]
    fn batch_run_tracker_is_exact_kind_bound_and_late_guards_cannot_release_a_live_worker() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let active = uuid::Uuid::from_u128(0x5001).to_string();
        let refused = uuid::Uuid::from_u128(0x5002).to_string();

        assert_eq!(state.batch_run_admission(&active), (BatchRunAdmission::Unknown, None, None));
        state.try_start_batch_for_run(&active, BatchOperation::Transcribe, 2).expect("admit exact batch");
        let live_token = state.ensure_cancel_token().expect("arm exact batch token");
        assert_eq!(
            state.batch_run_admission(&active),
            (BatchRunAdmission::Running, Some(BatchOperation::Transcribe), None)
        );

        assert_eq!(
            state.try_start_batch_for_run(&refused, BatchOperation::Normalize, 2).err().as_deref(),
            Some("Batch operation already in progress")
        );
        assert_eq!(
            state.batch_run_admission(&refused),
            (BatchRunAdmission::Rejected, Some(BatchOperation::Normalize), None)
        );

        assert!(
            !state.finish_batch_for_run(&active, BatchOperation::Normalize),
            "a wrong-kind delayed guard must fail closed"
        );
        assert_eq!(*state.lock_batch_state(), BatchState::Running);
        assert!(
            state.lock_batch_cancel_token().as_ref().is_some_and(|token| token.same_instance(&live_token)),
            "a wrong guard must not erase the live worker's cancellation authority"
        );

        assert!(state.finish_batch_for_run(&active, BatchOperation::Transcribe));
        assert_eq!(*state.lock_batch_state(), BatchState::Idle);
        assert!(state.lock_batch_cancel_token().is_none());
        let (admission, operation, outcome) = state.batch_run_admission(&active);
        assert_eq!(admission, BatchRunAdmission::Settled);
        assert_eq!(operation, Some(BatchOperation::Transcribe));
        assert_eq!(outcome.expect("missing terminal outcome").disposition, BatchRunDisposition::Panicked);
        assert_eq!(
            state.try_start_batch_for_run(&active, BatchOperation::Transcribe, 2).err().as_deref(),
            Some("Batch operation identity already used"),
            "a terminal operation identity must not be replayable"
        );
    }

    #[test]
    fn claimed_batch_start_records_rejection_when_worker_creation_does_not_complete() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let operation_id = uuid::Uuid::from_u128(0x5101).to_string();

        state.try_start_batch_for_run(&operation_id, BatchOperation::Normalize, 1).expect("claim pre-worker gate");
        let token = state.ensure_cancel_token().expect("arm pre-worker token");
        {
            let _claim = ClaimedBatchStart::new(&state, &operation_id, BatchOperation::Normalize);
        }

        assert_eq!(*state.lock_batch_state(), BatchState::Idle);
        assert!(state.lock_batch_cancel_token().is_none());
        assert!(!token.is_cancelled(), "rejection releases rather than falsely reporting cancellation");
        assert_eq!(
            state.batch_run_admission(&operation_id),
            (BatchRunAdmission::Rejected, Some(BatchOperation::Normalize), None)
        );
    }

    #[test]
    fn batch_claim_arms_exact_cancellation_before_preflight_and_cancel_refuses_commit() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let operation_id = uuid::Uuid::from_u128(0x5102).to_string();

        let token = state
            .try_start_batch_for_run(&operation_id, BatchOperation::Transcribe, 3)
            .expect("claim and arm preflight");
        let claim = ClaimedBatchStart::new(&state, &operation_id, BatchOperation::Transcribe);
        assert!(
            state.lock_batch_cancel_token().as_ref().is_some_and(|current| current.same_instance(&token)),
            "the exact cancellation authority must exist in the same completed admission call"
        );
        assert_eq!(state.starting_batch_run(&operation_id), Some((BatchOperation::Transcribe, 3)));

        assert!(state.cancel_current_operation(), "Cancel must reach a still-preflighting batch");
        assert!(token.is_cancelled());
        assert_eq!(
            state.commit_batch_start(&operation_id, BatchOperation::Transcribe, &token).err(),
            Some(BatchStartCommitError::Cancelled),
            "cancel-before-commit must prevent durable admission and worker start"
        );

        drop(claim);
        assert_eq!(
            state.batch_run_admission(&operation_id),
            (BatchRunAdmission::Rejected, Some(BatchOperation::Transcribe), None),
            "a cancelled preflight remains a definite rejected run for response-loss reconciliation"
        );
        assert!(state.lock_batch_cancel_token().is_none());
    }

    #[test]
    fn batch_start_commit_serializes_one_cancel_check_at_the_spawn_boundary() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let operation_id = uuid::Uuid::from_u128(0x5103).to_string();
        let token = state
            .try_start_batch_for_run(&operation_id, BatchOperation::Normalize, 1)
            .expect("claim normalization preflight");
        let claim = ClaimedBatchStart::new(&state, &operation_id, BatchOperation::Normalize);
        let commit = state
            .commit_batch_start(&operation_id, BatchOperation::Normalize, &token)
            .expect("commit exact start authority");
        assert!(
            state.batch_cancel_token.try_lock().is_err(),
            "one short start check must own the exact cancel slot until its boundary is released"
        );
        drop(commit);
        assert!(state.cancel_current_operation());

        assert!(token.is_cancelled(), "the same token reaches the worker immediately after handoff");
        drop(claim);
    }

    #[test]
    fn streamed_admission_keeps_cancel_and_status_locks_responsive() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let operation_id = uuid::Uuid::from_u128(0x5105).to_string();
        let token = state
            .try_start_batch_for_run(&operation_id, BatchOperation::Normalize, 100_000)
            .expect("claim large batch");
        let claim = ClaimedBatchStart::new(&state, &operation_id, BatchOperation::Normalize);

        let admission_check = state
            .commit_batch_start(&operation_id, BatchOperation::Normalize, &token)
            .expect("enter admission boundary");
        drop(admission_check);
        assert!(state.batch_run_tracker.try_lock().is_ok(), "status must not block on streamed admission");
        assert!(state.cancel_current_operation(), "Cancel must remain responsive during streamed admission");
        assert_eq!(
            state.commit_batch_start(&operation_id, BatchOperation::Normalize, &token).err(),
            Some(BatchStartCommitError::Cancelled),
            "a cancellation that wins during admission must prevent worker spawn"
        );
        drop(claim);
    }

    #[test]
    fn starting_authority_is_one_way_and_disappears_after_durable_admission() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let operation_id = uuid::Uuid::from_u128(0x5104).to_string();
        let token = state
            .try_start_batch_for_run(&operation_id, BatchOperation::Normalize, 4)
            .expect("claim starting identity");
        assert_eq!(state.starting_batch_run(&operation_id), Some((BatchOperation::Normalize, 4)));

        let commit =
            state.commit_batch_start(&operation_id, BatchOperation::Normalize, &token).expect("commit exact start");
        drop(commit);
        assert!(state.mark_batch_durable_admitted(&operation_id, BatchOperation::Normalize));
        assert_eq!(state.starting_batch_run(&operation_id), None);
        assert!(state.finish_batch_for_run(&operation_id, BatchOperation::Normalize));
    }

    #[test]
    fn durable_batch_guard_drop_before_worker_entry_records_start_failure_and_reopens_exact_gate() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = Arc::new(test_app_state(dir.path().to_path_buf()));
        let operation_id = uuid::Uuid::from_u128(0x5301).to_string();
        let segment_ids = vec!["guard-start-failure".to_string()];
        let guard = admit_test_normalize_guard(&state, &operation_id, &segment_ids);

        // This is the exact ownership shape of an OS `spawn` refusal: the captured closure (and
        // therefore its not-yet-entered guard) is dropped synchronously on the caller thread.
        drop(guard);

        let status = state.batch_store().status(&operation_id).unwrap().expect("durable terminal status");
        assert_eq!(status.state, crate::db::BatchJobLifecycleV1::Failed);
        assert_eq!(status.error_code.as_deref(), Some("BATCH_WORKER_START_FAILED"));
        assert_eq!(status.counts.abandoned, 1);
        let (admission, operation, outcome) = state.batch_run_admission(&operation_id);
        assert_eq!(admission, BatchRunAdmission::Settled);
        assert_eq!(operation, Some(BatchOperation::Normalize));
        let outcome = outcome.expect("exact durable outcome retained");
        assert_eq!(outcome.disposition, BatchRunDisposition::Halted);
        assert_eq!(outcome.error_code.as_deref(), Some("BATCH_WORKER_START_FAILED"));
        assert_eq!(*state.lock_batch_state(), BatchState::Idle);
        assert!(state.lock_batch_cancel_token().is_none());
        assert!(!state.lock_history().can_undo(), "a zero-effect start refusal must not mint an undo action");

        let next = uuid::Uuid::from_u128(0x5302).to_string();
        state
            .try_start_batch_for_run(&next, BatchOperation::Normalize, 1)
            .expect("verified terminal settlement reopens the single-flight gate");
        state.abort_batch_start(&next, BatchOperation::Normalize);
    }

    #[test]
    fn durable_batch_guard_drop_after_worker_entry_records_panic_and_exact_prefix_undo_redo() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = Arc::new(test_app_state(dir.path().to_path_buf()));
        let operation_id = uuid::Uuid::from_u128(0x5303).to_string();
        let segment_ids = vec!["guard-panic-applied".to_string(), "guard-panic-pending".to_string()];
        let mut guard = admit_test_normalize_guard(&state, &operation_id, &segment_ids);
        guard.mark_worker_entered();
        let page = guard.lease().unwrap().pending_page(None).expect("read exact pending page");
        assert_eq!(page.len(), 2);
        assert!(matches!(
            guard
                .lease()
                .unwrap()
                .commit_normalization(page[0].ordinal, "normalized-applied", "guard-test-v1")
                .expect("commit first exact item"),
            crate::db::BatchItemCommitOutcomeV1::Applied { .. }
        ));

        // Simulate an unwind before the worker's normal finish/outcome/event path.
        drop(guard);

        let status = state.batch_store().status(&operation_id).unwrap().expect("durable panic terminal");
        assert_eq!(status.state, crate::db::BatchJobLifecycleV1::Failed);
        assert_eq!(status.error_code.as_deref(), Some("BATCH_WORKER_PANICKED"));
        assert_eq!(status.counts.applied, 1);
        assert_eq!(status.counts.abandoned, 1);
        let (_, _, outcome) = state.batch_run_admission(&operation_id);
        let outcome = outcome.expect("panic outcome retained from durable counts");
        assert_eq!(outcome.disposition, BatchRunDisposition::Panicked);
        assert_eq!(outcome.succeeded, 1);
        assert_eq!(outcome.abandoned, 1);
        assert!(state.lock_history().can_undo(), "an applied panic prefix must publish exact undo authority");

        {
            let db = state.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let history = state.lock_history();
            assert_eq!(history.undo(&db).unwrap(), Some(crate::history::HistoryAction::BatchNormalize));
            assert_eq!(db.get_segment_by_id(&segment_ids[0]).unwrap().unwrap().normalized_transcript, None);
            assert_eq!(db.get_segment_by_id(&segment_ids[1]).unwrap().unwrap().normalized_transcript, None);
            assert_eq!(history.redo(&db).unwrap(), Some(crate::history::HistoryAction::BatchNormalize));
            assert_eq!(
                db.get_segment_by_id(&segment_ids[0]).unwrap().unwrap().normalized_transcript.as_deref(),
                Some("normalized-applied")
            );
            assert_eq!(
                db.get_segment_by_id(&segment_ids[1]).unwrap().unwrap().normalized_transcript,
                None,
                "redo must not touch the item that was pending when the worker panicked"
            );
        }
    }

    #[test]
    fn durable_batch_guard_retains_process_gate_when_terminal_database_proof_is_unreadable() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = Arc::new(test_app_state(dir.path().to_path_buf()));
        let operation_id = uuid::Uuid::from_u128(0x5304).to_string();
        let segment_ids = vec!["guard-corrupt-journal".to_string()];
        let guard = admit_test_normalize_guard(&state, &operation_id, &segment_ids);
        state
            .db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .connection()
            .execute_batch("DROP TABLE batch_job_items_v1")
            .expect("inject unreadable durable journal");

        drop(guard);

        assert!(state.batch_store().status(&operation_id).is_err(), "terminal evidence must remain unreadable");
        assert_eq!(
            state.batch_run_admission(&operation_id),
            (BatchRunAdmission::Running, Some(BatchOperation::Normalize), None),
            "RAM must not invent a terminal outcome or acknowledge an unreadable durable journal"
        );
        assert_eq!(*state.lock_batch_state(), BatchState::Running);
        assert!(state.lock_batch_cancel_token().is_some());
        let refused = uuid::Uuid::from_u128(0x5305).to_string();
        assert_eq!(
            state.try_start_batch_for_run(&refused, BatchOperation::Normalize, 1).err().as_deref(),
            Some("Batch operation already in progress"),
            "a second batch must remain blocked until restart recovery can validate durable truth"
        );
    }

    #[test]
    fn terminal_batch_publication_and_ack_wait_for_exact_guard_settlement() {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(test_app_state(directory.path().to_path_buf()));
        let operation_id = uuid::Uuid::from_u128(0x4f02).to_string();
        let segment_ids = vec!["guard-terminal-publication".to_string()];
        let mut guard = admit_test_normalize_guard(&state, &operation_id, &segment_ids);
        guard.mark_worker_entered();
        let item = guard.lease().unwrap().pending_page(None).unwrap().pop().expect("one pending publication fixture");
        assert!(matches!(
            guard
                .lease()
                .unwrap()
                .commit_normalization(item.ordinal, "normalized-publication", "guard-test-v1")
                .unwrap(),
            crate::db::BatchItemCommitOutcomeV1::Applied { .. }
        ));
        let stale_running = state.batch_store().status(&operation_id).unwrap().expect("running status snapshot");
        assert_eq!(stale_running.state, crate::db::BatchJobLifecycleV1::Running);
        let terminal = guard.finish(crate::db::BatchTerminalIntentV1::Succeeded).unwrap();
        assert_eq!(terminal.state, crate::db::BatchJobLifecycleV1::Succeeded);
        assert!(state.lock_history().can_undo(), "exact history must exist before process settlement");
        assert_eq!(state.batch_run_admission(&operation_id).0, BatchRunAdmission::Running);

        // Exact regression: a renderer poll after the durable header commit but before Guard::drop
        // must not observe or acknowledge terminal success. The guard still owns the physical gate.
        let finalizing = crate::commands::get_batch_run_status_blocking_for_test(operation_id.clone(), &state)
            .expect("read process-aware finalizing state");
        assert_eq!(finalizing.status, crate::ipc_contract::BatchRunStatusV1::Running);
        assert_eq!(finalizing.outcome, None);
        let early_ack = crate::commands::acknowledge_batch_run_blocking_for_test(operation_id.clone(), &state)
            .expect_err("terminal durable truth alone must not be acknowledgeable");
        assert_eq!(early_ack.code, "BATCH_RUN_NOT_SETTLED");
        assert_eq!(state.batch_run_admission(&operation_id).0, BatchRunAdmission::Running);

        drop(guard);
        let raced = crate::commands::publishable_durable_batch_status(&state, stale_running)
            .expect("a pre-terminal DB snapshot must refresh against exact settled process truth");
        assert_eq!(raced.status, crate::ipc_contract::BatchRunStatusV1::Settled);
        assert!(raced.outcome.is_some());
        let settled = crate::commands::get_batch_run_status_blocking_for_test(operation_id.clone(), &state)
            .expect("read exactly settled state");
        assert_eq!(settled.status, crate::ipc_contract::BatchRunStatusV1::Settled);
        assert!(settled.outcome.is_some());
        assert!(crate::commands::acknowledge_batch_run_blocking_for_test(operation_id.clone(), &state)
            .expect("acknowledge exact settled result"));

        // A new process has no RAM tracker entry. Its immutable terminal journal remains the status
        // authority, while ACK correctly stays process-local and non-adoptable.
        let restarted = test_app_state(directory.path().to_path_buf());
        let recovered = crate::commands::get_batch_run_status_blocking_for_test(operation_id.clone(), &restarted)
            .expect("read terminal journal without a process tracker");
        assert_eq!(recovered.status, crate::ipc_contract::BatchRunStatusV1::Settled);
        assert!(recovered.outcome.is_some());
        let restart_ack = crate::commands::acknowledge_batch_run_blocking_for_test(operation_id, &restarted)
            .expect_err("an untracked historical result is not awaiting renderer acknowledgment");
        assert_eq!(restart_ack.code, "BATCH_RUN_NOT_ADOPTABLE");
    }

    #[test]
    fn batch_terminal_outcome_is_single_assignment_and_survives_event_loss() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let operation_id = uuid::Uuid::from_u128(0x5201).to_string();
        state
            .try_start_batch_for_run(&operation_id, BatchOperation::Transcribe, 3)
            .expect("admit outcome-bearing batch");
        assert_eq!(state.adoptable_batch_run_identity(), Some((operation_id.clone(), BatchOperation::Transcribe)));

        let outcome = BatchRunOutcome {
            disposition: BatchRunDisposition::Halted,
            total: 3,
            succeeded: 1,
            failed: 1,
            skipped: 0,
            abandoned: 1,
            cancelled: false,
            error_code: Some("BATCH_TRANSCRIPT_WRITE_FAILED".into()),
        };
        assert!(state.record_batch_outcome(&operation_id, BatchOperation::Transcribe, outcome.clone()));
        assert!(
            !state.record_batch_outcome(&operation_id, BatchOperation::Transcribe, outcome.clone()),
            "terminal truth is immutable once recorded"
        );
        assert!(state.finish_batch_for_run(&operation_id, BatchOperation::Transcribe));

        let (admission, operation, retained) = state.batch_run_admission(&operation_id);
        assert_eq!(admission, BatchRunAdmission::Settled);
        assert_eq!(operation, Some(BatchOperation::Transcribe));
        assert_eq!(retained, Some(outcome));
        assert_eq!(
            state.adoptable_batch_run_identity(),
            Some((operation_id.clone(), BatchOperation::Transcribe)),
            "terminalization between reload discovery calls must not erase the result"
        );
        assert!(state.acknowledge_batch_run_renderer(&operation_id));
        assert!(state.acknowledge_batch_run_renderer(&operation_id), "acknowledgment replay must be idempotent");
        assert_eq!(state.adoptable_batch_run_identity(), None);
    }

    #[test]
    fn batch_run_tracker_bounds_terminal_reconciliation_history() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let first = uuid::Uuid::from_u128(0x6000).to_string();
        let last = uuid::Uuid::from_u128(0x6000 + BATCH_RUN_TERMINAL_HISTORY as u128).to_string();

        for value in 0..=BATCH_RUN_TERMINAL_HISTORY {
            let operation_id = uuid::Uuid::from_u128(0x6000 + value as u128).to_string();
            state
                .try_start_batch_for_run(&operation_id, BatchOperation::Normalize, 1)
                .expect("admit retained batch run");
            assert!(state.finish_batch_for_run(&operation_id, BatchOperation::Normalize));
        }

        assert_eq!(state.batch_run_admission(&first), (BatchRunAdmission::Unknown, None, None));
        let (last_status, last_operation, last_outcome) = state.batch_run_admission(&last);
        assert_eq!(last_status, BatchRunAdmission::Settled);
        assert_eq!(last_operation, Some(BatchOperation::Normalize));
        assert_eq!(last_outcome.expect("missing retained outcome").disposition, BatchRunDisposition::Panicked);
        assert_eq!(state.lock_batch_run_tracker().terminal.len(), BATCH_RUN_TERMINAL_HISTORY);
    }

    #[test]
    fn ordinary_import_cannot_supersede_an_interrupted_journal_but_exact_resume_can() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        state.job_store().begin_import("C:/owner/audio", 3).expect("create interrupted journal");
        let ordinary = "00000000-0000-4000-8000-000000000011";
        let recovery = "00000000-0000-4000-8000-000000000012";

        assert_eq!(
            state.try_start_import_for_run(ordinary),
            Err(INTERRUPTED_IMPORT_RECOVERY_REQUIRED_MESSAGE.to_string())
        );
        assert_eq!(state.import_run_admission(ordinary), ImportRunAdmission::Rejected);
        assert_eq!(*state.lock_import_state(), ImportState::Idle);

        state.try_start_import_for_recovery_run(recovery).expect("resume path may claim the existing journal");
        assert_eq!(state.import_run_admission(recovery), ImportRunAdmission::Running);
        state.finish_import();
    }

    #[test]
    fn import_recovery_admission_is_mutually_exclusive_with_a_live_worker() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        state.try_start_import().expect("start live import");
        assert!(
            state.try_import_recovery_admission().is_none(),
            "a live worker's journal must not be exposed as interrupted"
        );
        state.finish_import();

        let admission = state.try_import_recovery_admission().expect("idle recovery admission");
        assert!(
            state.import_state.try_lock().is_err(),
            "the recovery admission must hold the exact mutex used by import startup"
        );
        drop(admission);
        assert!(state.import_state.try_lock().is_ok(), "dropping recovery admission reopens startup");
    }

    #[test]
    fn app_state_batch_state_recovers_poisoned_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.batch_state.lock().expect("lock batch state");
            panic!("poison batch state");
        }));

        let first = uuid::Uuid::from_u128(0x4001).to_string();
        state.try_start_batch_for_run(&first, BatchOperation::Transcribe, 1).expect("recover and start batch");
        assert_eq!(*state.lock_batch_state(), BatchState::Running);
        let second = uuid::Uuid::from_u128(0x4002).to_string();
        assert_eq!(
            state.try_start_batch_for_run(&second, BatchOperation::Normalize, 1).err().as_deref(),
            Some("Batch operation already in progress")
        );
        assert!(state.finish_batch_for_run(&first, BatchOperation::Transcribe));
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
    fn session_view_save_and_restore_admission_are_linearized_before_the_writer_lock() {
        let directory = tempfile::TempDir::new().unwrap();
        let mut state = test_app_state(directory.path().to_path_buf());
        let isolated_path = directory.path().join("isolated-session.db");
        let database = Database::open(isolated_path.to_string_lossy().as_ref()).unwrap();
        database.initialize().unwrap();
        state.db = DatabaseRuntime::isolated_for_test(database);
        let state = Arc::new(state);
        let runtime = state.db_runtime();

        let held_writer = runtime.lock().unwrap();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            worker_state.save_session_view_state("writer first".into(), "oldest".into(), Some(true))
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !runtime.mutation_active_for_test() {
            assert!(Instant::now() < deadline, "session writer never entered restore admission");
            std::thread::yield_now();
        }
        assert!(
            runtime.try_reserve_restore_for_test().is_err(),
            "restore must refuse an admitted session writer even while it waits for SQLite"
        );
        drop(held_writer);
        worker.join().unwrap().unwrap();
        let saved = state.lock_session().load().expect("writer-first session is durable");
        assert_eq!(saved.search_query, "writer first");
        assert_eq!(saved.sort_order, "oldest");
        assert_eq!(saved.filter_verified, Some(true));

        let restore = runtime.try_reserve_restore_for_test().unwrap();
        let error = state
            .save_session_view_state("must not land".into(), "newest".into(), Some(false))
            .expect_err("restore-first admission must refuse the session write");
        assert!(error.to_string().contains("restore"), "unexpected admission error: {error}");
        let retained = state.lock_session().load().expect("the prior session remains authoritative");
        assert_eq!(retained.search_query, "writer first");
        drop(restore);

        state.save_session_view_state("after restore".into(), "newest".into(), None).unwrap();
        let resumed = state.lock_session().load().expect("session writes resume after restore admission releases");
        assert_eq!(resumed.search_query, "after restore");
        assert_eq!(resumed.sort_order, "newest");
        assert_eq!(resumed.filter_verified, None);
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

    // ---- Wave-3 coverage: AppState accessors, run trackers, session plumbing ----

    #[test]
    fn batch_run_outcome_panicked_marks_every_item_abandoned() {
        // A worker panic must read as a hard stop: nothing succeeded, everything abandoned, and the
        // stable machine code names the cause. Never a quiet "completed".
        let outcome = BatchRunOutcome::panicked(5);
        assert_eq!(outcome.disposition, BatchRunDisposition::Panicked);
        assert_eq!(outcome.total, 5);
        assert_eq!(outcome.abandoned, 5);
        assert_eq!((outcome.succeeded, outcome.failed, outcome.skipped), (0, 0, 0));
        assert!(!outcome.cancelled);
        assert_eq!(outcome.error_code.as_deref(), Some("BATCH_WORKER_PANICKED"));
    }

    #[test]
    fn dedup_ready_state_admits_import_and_the_unavailable_error_displays_the_stable_message() {
        assert!(DedupReadiness::Ready { rehydrated_recordings: 7 }.require_import_ready().is_ok());
        assert_eq!(DedupIndexUnavailable.to_string(), DEDUP_INDEX_UNAVAILABLE_MESSAGE);
        assert!(
            DEDUP_INDEX_UNAVAILABLE_MESSAGE.starts_with(DEDUP_INDEX_UNAVAILABLE_CODE),
            "callers classify on the leading machine code, so it must stay the message prefix"
        );
    }

    #[test]
    fn review_cursor_and_session_save_persist_durable_view_state() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        state.persist_review_cursor("seg-cursor-9");
        let saved = state.lock_session().load().expect("cursor save is durable");
        assert_eq!(saved.selected_segment_id.as_deref(), Some("seg-cursor-9"));

        state.lock_session().set_view_state("گەڕان".into(), "oldest".into(), Some(false));
        state.session_save();
        let saved = state.lock_session().load().expect("session_save is durable");
        assert_eq!(saved.search_query, "گەڕان");
        assert_eq!(saved.sort_order, "oldest");
        assert_eq!(saved.filter_verified, Some(false));
        assert_eq!(saved.selected_segment_id.as_deref(), Some("seg-cursor-9"), "a later full save keeps the cursor");

        // last_save starts at 0, so the first auto_save is past its interval and must also persist.
        state.lock_session().set_view_state("دووەم".into(), "newest".into(), None);
        state.session_auto_save();
        let saved = state.lock_session().load().expect("auto save is durable");
        assert_eq!(saved.search_query, "دووەم");
    }

    #[test]
    fn history_and_db_handles_reach_the_live_state_authorities() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        assert!(
            Arc::ptr_eq(&state.history_arc_for_restore(), &state.history),
            "restore publication must clear the SAME history the commands mutate, not a copy"
        );
        let handle = state.db_arc();
        let db = handle.lock().expect("restore-gated handle reaches the database");
        assert_eq!(db.integrity_check().unwrap(), "ok");
    }

    #[test]
    fn settings_write_gate_recovers_poisoned_lock_and_releases() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.settings_write.lock().expect("lock settings write gate");
            panic!("poison settings write gate");
        }));

        {
            let _gate = state.lock_settings_write();
            assert!(
                matches!(state.settings_write.try_lock(), Err(std::sync::TryLockError::WouldBlock)),
                "the recovered gate is genuinely held"
            );
        }
        // The std poison flag survives recovery by design (a bare try_lock keeps answering
        // Poisoned even when free); what must reopen is acquirability through the recovering
        // accessor every production caller uses.
        let _reacquired = state.lock_settings_write();
    }

    #[test]
    fn abort_import_start_refuses_a_different_run_and_keeps_the_live_worker() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let live = uuid::Uuid::from_u128(0x7001).to_string();
        let stranger = uuid::Uuid::from_u128(0x7002).to_string();

        state.try_start_import_for_run(&live).expect("admit live import");
        state.abort_import_start(&stranger);
        assert_eq!(*state.lock_import_state(), ImportState::Running, "a wrong-run abort must not free the gate");
        assert_eq!(state.import_run_admission(&live), ImportRunAdmission::Running);
        assert_eq!(
            state.import_run_admission(&stranger),
            ImportRunAdmission::Unknown,
            "a refused abort must not fabricate terminal truth for an unadmitted identity"
        );
        state.finish_import();
        assert_eq!(state.import_run_admission(&live), ImportRunAdmission::Settled);
    }

    #[test]
    fn import_rejection_memory_is_exact_and_never_downgrades_a_settled_run() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let settled = uuid::Uuid::from_u128(0x7003).to_string();
        let refused = uuid::Uuid::from_u128(0x7004).to_string();

        state.try_start_import_for_run(&settled).expect("admit run");
        state.finish_import();
        state.remember_import_rejection(&settled);
        assert_eq!(
            state.import_run_admission(&settled),
            ImportRunAdmission::Settled,
            "a late rejection writer must not rewrite settled admission truth"
        );

        state.remember_import_rejection(&refused);
        assert_eq!(state.import_run_admission(&refused), ImportRunAdmission::Rejected);
    }

    #[test]
    fn batch_rejection_memory_blocks_identity_replay_and_never_downgrades_settlement() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let refused = uuid::Uuid::from_u128(0x7005).to_string();
        let settled = uuid::Uuid::from_u128(0x7006).to_string();

        state.remember_batch_rejection(&refused, BatchOperation::Normalize);
        assert_eq!(
            state.batch_run_admission(&refused),
            (BatchRunAdmission::Rejected, Some(BatchOperation::Normalize), None)
        );
        assert_eq!(
            state.try_start_batch_for_run(&refused, BatchOperation::Normalize, 1).err().as_deref(),
            Some("Batch operation identity already used"),
            "a rejected identity must not be replayable into a live worker"
        );

        state.try_start_batch_for_run(&settled, BatchOperation::Transcribe, 1).expect("admit batch");
        assert!(state.finish_batch_for_run(&settled, BatchOperation::Transcribe));
        state.remember_batch_rejection(&settled, BatchOperation::Transcribe);
        assert_eq!(
            state.batch_run_admission(&settled).0,
            BatchRunAdmission::Settled,
            "a late rejection writer must not rewrite settled batch truth"
        );
    }

    #[test]
    fn mark_batch_durable_admitted_is_exact_and_one_way() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let live = uuid::Uuid::from_u128(0x7007).to_string();
        let stranger = uuid::Uuid::from_u128(0x7008).to_string();

        assert!(!state.mark_batch_durable_admitted(&live, BatchOperation::Normalize), "no active run: refuse");
        state.try_start_batch_for_run(&live, BatchOperation::Normalize, 2).expect("admit batch");
        assert!(
            !state.mark_batch_durable_admitted(&stranger, BatchOperation::Normalize),
            "a different identity must not flip the live run's phase"
        );
        assert!(
            !state.mark_batch_durable_admitted(&live, BatchOperation::Transcribe),
            "a wrong-kind writer must not flip the live run's phase"
        );
        assert!(state.mark_batch_durable_admitted(&live, BatchOperation::Normalize));
        assert!(
            !state.mark_batch_durable_admitted(&live, BatchOperation::Normalize),
            "the Starting->Durable transition is one-way and single-shot"
        );
        assert!(state.finish_batch_for_run(&live, BatchOperation::Normalize));
    }

    #[test]
    fn record_batch_outcome_refuses_untracked_and_mismatched_runs() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_app_state(dir.path().to_path_buf());
        let live = uuid::Uuid::from_u128(0x7009).to_string();
        let stranger = uuid::Uuid::from_u128(0x700a).to_string();
        let outcome = BatchRunOutcome {
            disposition: BatchRunDisposition::Completed,
            total: 2,
            succeeded: 2,
            failed: 0,
            skipped: 0,
            abandoned: 0,
            cancelled: false,
            error_code: None,
        };

        assert!(
            !state.record_batch_outcome(&stranger, BatchOperation::Transcribe, outcome.clone()),
            "an untracked run has no outcome slot"
        );
        state.try_start_batch_for_run(&live, BatchOperation::Transcribe, 2).expect("admit batch");
        assert!(
            !state.record_batch_outcome(
                &live,
                BatchOperation::Transcribe,
                BatchRunOutcome { total: 3, ..outcome.clone() }
            ),
            "a mismatched total is a different run's result and must be refused"
        );
        assert!(
            !state.record_batch_outcome(&live, BatchOperation::Normalize, outcome.clone()),
            "a wrong-kind result must be refused"
        );
        assert!(state.record_batch_outcome(&live, BatchOperation::Transcribe, outcome.clone()));
        assert!(state.finish_batch_for_run(&live, BatchOperation::Transcribe));
        let (_, _, retained) = state.batch_run_admission(&live);
        assert_eq!(retained, Some(outcome), "only the exact-match outcome survives to settlement");
    }
}
