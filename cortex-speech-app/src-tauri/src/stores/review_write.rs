//! Serialized human-review effect writes that already expose a stable database-domain contract.

use crate::database_runtime::{DatabaseRuntime, MutationGuard, RestoreGeneration};
#[cfg(test)]
use crate::db::DesktopReviewUndoAuthority;
use crate::db::{
    DesktopHumanDecisionUndoAuthority, DesktopPlaybackInterval, DesktopPlaybackReceipt, DesktopPlaybackSession,
    DesktopReviewFlagUndoAuthority, DesktopReviewUndoAvailability, HumanDecisionCommit, HumanDecisionUndoOutcome,
    HumanFlagCommit, HumanFlagUndoOutcome, PlaybackDecisionProof, PLAYBACK_EVIDENCE_CHANGED,
};
use crate::error::{AppError, AppResult};
use crate::technical_audio_probe::{
    acquire_technical_audio_source_lease, probe_technical_audio_failure, TechnicalAudioFailureEvidence,
    TechnicalAudioProbeObservation,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReviewCommitError {
    #[error("the review segment no longer exists")]
    SegmentNotFound,
    #[error("the review revision is stale; current revision is {current_revision}")]
    StaleRevision { current_revision: i64 },
    #[error(transparent)]
    Backend(#[from] AppError),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReviewFlagCommitError {
    #[error("the review segment no longer exists")]
    SegmentNotFound,
    #[error("the review revision is stale; current revision is {current_revision}")]
    StaleRevision { current_revision: i64 },
    #[error(transparent)]
    Backend(#[from] AppError),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TechnicalUnusableCommitError {
    #[error("the review segment no longer exists")]
    SegmentNotFound,
    #[error("the review revision is stale; current revision is {current_revision}")]
    StaleRevision { current_revision: i64 },
    #[error("the segment already has human transcript truth")]
    AlreadyHumanReviewed,
    #[error("the segment audio source changed while its failure was being verified")]
    SourceChanged,
    #[error("a missing path cannot be bound to an immutable audio-source lease")]
    MissingFileUnleaseable,
    #[error("technical audio verification is at its strict concurrency limit")]
    ProbeBusy,
    #[error("the declared audio failure was not reproduced (declared {declared_reason}, observed {observed})")]
    FailureNotReproduced { declared_reason: String, observed: String },
    #[error(transparent)]
    Backend(#[from] AppError),
}

const TECHNICAL_PROBE_MAX_CONCURRENCY: usize = 2;
const TECHNICAL_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const TECHNICAL_PROBE_WAIT: Duration = Duration::from_secs(16);

#[derive(Default)]
struct TechnicalProbeFlightState {
    result: Option<TechnicalAudioFailureEvidence>,
}

#[derive(Default)]
struct TechnicalProbeFlight {
    state: Mutex<TechnicalProbeFlightState>,
    complete: Condvar,
}

#[derive(Default)]
struct TechnicalProbeRegistry {
    active: HashMap<String, Arc<TechnicalProbeFlight>>,
    #[cfg(test)]
    max_active_observed: usize,
}

static TECHNICAL_PROBE_REGISTRY: LazyLock<Mutex<TechnicalProbeRegistry>> =
    LazyLock::new(|| Mutex::new(TechnicalProbeRegistry::default()));

fn lock_probe_registry() -> std::sync::MutexGuard<'static, TechnicalProbeRegistry> {
    TECHNICAL_PROBE_REGISTRY.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned technical-audio probe registry");
        poisoned.into_inner()
    })
}

/// Coalesce concurrent probes of one canonical source and reject excess distinct sources before a
/// decoder is opened. Followers receive the leader's immutable observation; every caller still
/// rechecks the exact source bytes before any database mutation.
fn probe_technical_audio_failure_single_flight_with<F>(
    source_path_sha256: &str,
    audio_content_hash: Option<&str>,
    path: &Path,
    probe: F,
) -> Result<TechnicalAudioFailureEvidence, TechnicalUnusableCommitError>
where
    F: FnOnce(std::path::PathBuf, Instant) -> TechnicalAudioFailureEvidence + Send + 'static,
{
    let flight_key = format!("{source_path_sha256}:{}", audio_content_hash.unwrap_or("none"));
    let (flight, is_leader) = {
        let mut registry = lock_probe_registry();
        if let Some(flight) = registry.active.get(&flight_key) {
            (Arc::clone(flight), false)
        } else {
            if registry.active.len() >= TECHNICAL_PROBE_MAX_CONCURRENCY {
                return Err(TechnicalUnusableCommitError::ProbeBusy);
            }
            let flight = Arc::new(TechnicalProbeFlight::default());
            registry.active.insert(flight_key.clone(), Arc::clone(&flight));
            #[cfg(test)]
            {
                registry.max_active_observed = registry.max_active_observed.max(registry.active.len());
            }
            (flight, true)
        }
    };

    if is_leader {
        let worker_flight = Arc::clone(&flight);
        let worker_key = flight_key.clone();
        let worker_path = path.to_path_buf();
        if let Err(error) = std::thread::Builder::new().name("technical-audio-probe".into()).spawn(move || {
            let deadline = Instant::now() + TECHNICAL_PROBE_TIMEOUT;
            let evidence = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe(worker_path, deadline)))
                .unwrap_or(TechnicalAudioFailureEvidence {
                    observation: TechnicalAudioProbeObservation::Inconclusive,
                    source_blake3: None,
                });
            {
                let mut state = worker_flight.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                state.result = Some(evidence);
                worker_flight.complete.notify_all();
            }
            let mut registry = lock_probe_registry();
            if registry.active.get(&worker_key).is_some_and(|active| Arc::ptr_eq(active, &worker_flight)) {
                registry.active.remove(&worker_key);
            }
        }) {
            let mut registry = lock_probe_registry();
            if registry.active.get(&flight_key).is_some_and(|active| Arc::ptr_eq(active, &flight)) {
                registry.active.remove(&flight_key);
            }
            return Err(TechnicalUnusableCommitError::Backend(AppError::Other(format!(
                "could not start bounded technical-audio probe: {error}"
            ))));
        }
    }

    let state = flight.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (state, timeout) = flight
        .complete
        .wait_timeout_while(state, TECHNICAL_PROBE_WAIT, |state| state.result.is_none())
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if timeout.timed_out() || state.result.is_none() {
        return Err(TechnicalUnusableCommitError::ProbeBusy);
    }
    state.result.clone().ok_or(TechnicalUnusableCommitError::ProbeBusy)
}

fn probe_technical_audio_failure_single_flight(
    source_path_sha256: &str,
    audio_content_hash: Option<&str>,
    path: &Path,
) -> Result<TechnicalAudioFailureEvidence, TechnicalUnusableCommitError> {
    probe_technical_audio_failure_single_flight_with(source_path_sha256, audio_content_hash, path, |path, deadline| {
        probe_technical_audio_failure(&path, deadline)
    })
}

#[derive(Clone)]
pub(crate) struct ReviewWriteStore {
    runtime: DatabaseRuntime,
}

impl ReviewWriteStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    fn lock(&self, operation: &str) -> std::sync::MutexGuard<'_, crate::db::Database> {
        self.runtime.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during a review write");
            poisoned.into_inner()
        })
    }

    fn lock_after_mutation(
        &self,
        operation: &str,
        mutation: &MutationGuard<'_>,
    ) -> std::sync::MutexGuard<'_, crate::db::Database> {
        self.runtime.lock_after_mutation(mutation).unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during an admitted review write");
            poisoned.into_inner()
        })
    }

    /// Human-review writes must enter this runtime's restore admission before waiting for the DB
    /// mutex. Acquiring only the mutex lets restore reserve behind a live writer and erase the write
    /// immediately after it reports success.
    fn begin_mutation(&self, operation: &str) -> AppResult<MutationGuard<'_>> {
        self.runtime.begin_mutation().map_err(|error| {
            tracing::warn!(operation, %error, "Review write refused by restore admission");
            AppError::Other(error)
        })
    }

    fn begin_mutation_at_generation(
        &self,
        generation: RestoreGeneration,
        operation: &str,
    ) -> AppResult<MutationGuard<'_>> {
        self.runtime.begin_mutation_at_restore_generation_serial(generation.serial()).map_err(|error| {
            tracing::warn!(operation, %error, "Review write crossed a restore generation");
            AppError::Other(error)
        })
    }

    pub(crate) fn capture_restore_generation(&self) -> Result<RestoreGeneration, String> {
        self.runtime.capture_restore_generation()
    }

    pub(crate) fn begin_mutation_at_restore_generation_serial(
        &self,
        expected_generation: u64,
    ) -> Result<MutationGuard<'_>, String> {
        self.runtime.begin_mutation_at_restore_generation_serial(expected_generation)
    }

    #[cfg(test)]
    pub(crate) fn advance_restore_generation_for_test(&self) -> Result<(), String> {
        self.runtime.advance_restore_generation_for_test()
    }

    /// Resolve the server-owned live media grant behind a playback receipt without leaking a raw
    /// database connection into the command layer. The command may use this id only to borrow the
    /// registry's already-verified source lease; the typed commit repeats the exact receipt/source
    /// checks under the serialized write boundary before changing human truth.
    pub(crate) fn desktop_playback_media_grant_id(&self, playback_receipt_id: &str) -> AppResult<Option<String>> {
        self.lock("desktop_playback_media_grant_id").desktop_playback_media_grant_id(playback_receipt_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_desktop_playback_session_at_generation_v1(
        &self,
        segment_id: &str,
        expected_revision: i64,
        media_grant_id: &str,
        client_attempt_id: &str,
        grant_source_path: &Path,
        grant_audio_content_hash: &str,
        reviewer: Option<&str>,
        restore_generation: RestoreGeneration,
    ) -> AppResult<DesktopPlaybackSession> {
        let mutation = self.begin_mutation_at_generation(restore_generation, "begin_desktop_playback_session_v1")?;
        self.lock_after_mutation("begin_desktop_playback_session_v1", &mutation).begin_desktop_playback_session_v1(
            segment_id,
            expected_revision,
            media_grant_id,
            client_attempt_id,
            grant_source_path,
            grant_audio_content_hash,
            reviewer,
        )
    }

    pub(crate) fn cancel_desktop_playback_session_v1(
        &self,
        playback_receipt_id: &str,
        client_attempt_id: &str,
    ) -> AppResult<bool> {
        let mutation = self.begin_mutation("cancel_desktop_playback_session_v1")?;
        self.lock_after_mutation("cancel_desktop_playback_session_v1", &mutation)
            .cancel_desktop_playback_session_v1(playback_receipt_id, client_attempt_id)
    }

    pub(crate) fn replay_finalized_desktop_playback_receipt_v1(
        &self,
        playback_receipt_id: &str,
        media_grant_id: &str,
        intervals: &[DesktopPlaybackInterval],
    ) -> AppResult<Option<DesktopPlaybackReceipt>> {
        self.lock("replay_finalized_desktop_playback_receipt_v1").replay_finalized_desktop_playback_receipt_v1(
            playback_receipt_id,
            media_grant_id,
            intervals,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_desktop_playback_session_at_generation_v1(
        &self,
        playback_receipt_id: &str,
        media_grant_id: &str,
        grant_source_path: &Path,
        grant_audio_content_hash: &str,
        intervals: &[DesktopPlaybackInterval],
        restore_generation: u64,
    ) -> AppResult<DesktopPlaybackReceipt> {
        let mutation =
            self.runtime.begin_mutation_at_restore_generation_serial(restore_generation).map_err(AppError::Other)?;
        self.lock_after_mutation("finalize_desktop_playback_session_v1", &mutation)
            .finalize_desktop_playback_session_v1(
                playback_receipt_id,
                media_grant_id,
                grant_source_path,
                grant_audio_content_hash,
                intervals,
            )
    }

    /// Legacy desktop compatibility boundary. Exact operation replay is resolved before current
    /// playback preflight because the first successful decision advances the review revision.
    #[cfg(test)]
    pub(crate) fn commit_legacy_decision(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        operation_id: &str,
    ) -> AppResult<HumanDecisionCommit> {
        let mutation = self.begin_mutation("record_human_decision")?;
        let database = self.lock_after_mutation("record_human_decision", &mutation);
        if let Some(commit) = database.replay_desktop_human_decision(
            segment_id,
            decision,
            corrected_transcript,
            timestamp_ms,
            operation_id,
        )? {
            return Ok(commit);
        }
        let playback = require_listened(&database, segment_id)?;
        database.finalize_human_review_with_playback(
            segment_id,
            decision,
            corrected_transcript,
            timestamp_ms,
            &playback,
            operation_id,
        )
    }

    /// Revision-bound typed desktop commit. Draft clearing remains inside the database transaction;
    /// a replay may clear only the draft for the original base revision.
    #[cfg(test)]
    pub(crate) fn commit_typed_decision(
        &self,
        segment_id: &str,
        base_revision: i64,
        decision: &str,
        transcript: Option<&str>,
        playback_receipt_id: &str,
        operation_id: &str,
    ) -> Result<HumanDecisionCommit, ReviewCommitError> {
        self.commit_typed_decision_with_source_lease(
            segment_id,
            base_revision,
            decision,
            transcript,
            playback_receipt_id,
            operation_id,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub(crate) fn commit_typed_decision_with_source_lease(
        &self,
        segment_id: &str,
        base_revision: i64,
        decision: &str,
        transcript: Option<&str>,
        playback_receipt_id: &str,
        operation_id: &str,
        source_lease: Option<crate::media::VerifiedMediaSourceLease>,
    ) -> Result<HumanDecisionCommit, ReviewCommitError> {
        let restore_generation = self.runtime.capture_restore_generation().map_err(AppError::Other)?;
        self.commit_typed_decision_with_source_lease_at_generation(
            segment_id,
            base_revision,
            decision,
            transcript,
            playback_receipt_id,
            operation_id,
            source_lease,
            restore_generation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_typed_decision_with_source_lease_at_generation(
        &self,
        segment_id: &str,
        base_revision: i64,
        decision: &str,
        transcript: Option<&str>,
        playback_receipt_id: &str,
        operation_id: &str,
        source_lease: Option<crate::media::VerifiedMediaSourceLease>,
        restore_generation: RestoreGeneration,
    ) -> Result<HumanDecisionCommit, ReviewCommitError> {
        if let Some(source_lease) = source_lease {
            return self.commit_typed_decision_with_verified_source_lease(
                segment_id,
                base_revision,
                decision,
                transcript,
                playback_receipt_id,
                operation_id,
                source_lease,
                restore_generation,
            );
        }

        // A live media grant normally supplies an already-verified immutable source lease.  A
        // receipt may legitimately outlive that in-memory grant (for example after a renderer or
        // desktop restart), so recover the server-owned source identity under the serialized
        // database lock, release that lock while decoding/hash-verifying the source, then rerun
        // every replay/revision/evidence check under the lock before committing.  This prevents a
        // long media decode from blocking unrelated durable writes without weakening the final
        // compare-and-swap boundary.
        let (source_path, audio_content_hash) = {
            let mutation = self.begin_mutation_at_generation(restore_generation, "commit_review_v1_source_recovery")?;
            let database = self.lock_after_mutation("commit_review_v1_source_recovery", &mutation);
            if let Some(commit) = database.replay_desktop_review_v1_and_clear_draft(
                segment_id,
                base_revision,
                decision,
                transcript,
                playback_receipt_id,
                operation_id,
            )? {
                return Ok(commit);
            }

            let Some((_segment, current_revision)) = database.get_segment_by_id_with_revision(segment_id)? else {
                return Err(ReviewCommitError::SegmentNotFound);
            };
            if current_revision != base_revision {
                return Err(ReviewCommitError::StaleRevision { current_revision });
            }

            database
                .desktop_playback_recovery_source_identity(segment_id, base_revision, playback_receipt_id)?
                .ok_or_else(|| {
                    ReviewCommitError::Backend(AppError::Validation(format!(
                        "{PLAYBACK_EVIDENCE_CHANGED}: the receipt no longer resolves to the current segment source"
                    )))
                })?
        };
        let source_lease =
            crate::media::verify_current_source_lease(&source_path, &audio_content_hash).map_err(|error| {
                ReviewCommitError::Backend(AppError::Validation(format!("{PLAYBACK_EVIDENCE_CHANGED}: {error}")))
            })?;

        self.commit_typed_decision_with_verified_source_lease(
            segment_id,
            base_revision,
            decision,
            transcript,
            playback_receipt_id,
            operation_id,
            source_lease,
            restore_generation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_typed_decision_with_verified_source_lease(
        &self,
        segment_id: &str,
        base_revision: i64,
        decision: &str,
        transcript: Option<&str>,
        playback_receipt_id: &str,
        operation_id: &str,
        source_lease: crate::media::VerifiedMediaSourceLease,
        restore_generation: RestoreGeneration,
    ) -> Result<HumanDecisionCommit, ReviewCommitError> {
        let mutation = self.begin_mutation_at_generation(restore_generation, "commit_review_v1")?;
        let database = self.lock_after_mutation("commit_review_v1", &mutation);
        if let Some(commit) = database.replay_desktop_review_v1_and_clear_draft(
            segment_id,
            base_revision,
            decision,
            transcript,
            playback_receipt_id,
            operation_id,
        )? {
            return Ok(commit);
        }

        let Some((_segment, current_revision)) = database.get_segment_by_id_with_revision(segment_id)? else {
            return Err(ReviewCommitError::SegmentNotFound);
        };
        if current_revision != base_revision {
            return Err(ReviewCommitError::StaleRevision { current_revision });
        }

        let playback =
            require_listened_v4_with_source_lease(&database, segment_id, playback_receipt_id, Some(source_lease))?;
        database
            .finalize_desktop_review_v1_with_playback(
                segment_id,
                base_revision,
                decision,
                transcript,
                &playback,
                operation_id,
            )
            .map_err(ReviewCommitError::from)
    }

    pub(crate) fn desktop_review_undo_availability(&self) -> AppResult<DesktopReviewUndoAvailability> {
        self.runtime.open_read()?.desktop_review_undo_availability()
    }

    pub(crate) fn undo_latest_desktop_human_decision(
        &self,
        authority: &DesktopHumanDecisionUndoAuthority,
        operation_id: &str,
    ) -> AppResult<HumanDecisionUndoOutcome> {
        let mutation = self.begin_mutation("undo_latest_desktop_human_decision")?;
        self.lock_after_mutation("undo_latest_desktop_human_decision", &mutation)
            .undo_latest_desktop_human_decision(authority, operation_id)
    }

    pub(crate) fn record_flag(
        &self,
        segment_id: &str,
        base_revision: i64,
        rationale: &str,
        operation_id: &str,
    ) -> Result<HumanFlagCommit, ReviewFlagCommitError> {
        let mutation = self.begin_mutation("record_review_flag")?;
        self.lock_after_mutation("record_review_flag", &mutation)
            .record_review_flag(segment_id, base_revision, rationale, operation_id)
            .map_err(|error| match error {
                AppError::Validation(message) if message == "E_REVIEW_FLAG_SEGMENT_NOT_FOUND" => {
                    ReviewFlagCommitError::SegmentNotFound
                }
                AppError::Validation(message) if message.starts_with("E_STALE_REVIEW_FLAG_REVISION:") => {
                    let current_revision = message
                        .split_once(':')
                        .and_then(|(_, revision)| revision.parse::<i64>().ok())
                        .unwrap_or(base_revision);
                    ReviewFlagCommitError::StaleRevision { current_revision }
                }
                error => ReviewFlagCommitError::Backend(error),
            })
    }

    /// Technical audio failure is intentionally independent from playback proof: playback is the
    /// impossible operation being reported. The database still owns the revision CAS, immutable
    /// effect, idempotency replay and exact draft deletion in one FULL-sync transaction.
    pub(crate) fn mark_technically_unusable(
        &self,
        segment_id: &str,
        base_revision: i64,
        reason: &str,
        operation_id: &str,
    ) -> Result<HumanFlagCommit, TechnicalUnusableCommitError> {
        self.mark_technically_unusable_after_probe(segment_id, base_revision, reason, operation_id, || {})
    }

    fn mark_technically_unusable_after_probe(
        &self,
        segment_id: &str,
        base_revision: i64,
        reason: &str,
        operation_id: &str,
        after_probe: impl FnOnce(),
    ) -> Result<HumanFlagCommit, TechnicalUnusableCommitError> {
        self.mark_technically_unusable_with_hooks(segment_id, base_revision, reason, operation_id, after_probe, || {})
    }

    fn mark_technically_unusable_with_hooks(
        &self,
        segment_id: &str,
        base_revision: i64,
        reason: &str,
        operation_id: &str,
        after_probe: impl FnOnce(),
        after_lease: impl FnOnce(),
    ) -> Result<HumanFlagCommit, TechnicalUnusableCommitError> {
        let restore_generation = self.runtime.capture_restore_generation().map_err(AppError::Other)?;
        let (audio_path, source_path_sha256, audio_content_hash) = {
            let mutation = self.begin_mutation_at_generation(restore_generation, "mark_segment_unusable_v1_replay")?;
            let database = self.lock_after_mutation("mark_segment_unusable_v1_replay", &mutation);
            if let Some(replay) = database
                .replay_segment_technically_unusable(segment_id, base_revision, reason, operation_id)
                .map_err(TechnicalUnusableCommitError::Backend)?
            {
                return Ok(replay);
            }
            // A negative directory entry has no file object that Windows can lease. An immediate
            // NotFound recheck would still leave a creation window before commit, so new missing-file
            // effects are disabled. Exact replays above remain available because they add no truth.
            if reason == TechnicalAudioProbeObservation::MissingFile.code() {
                return Err(TechnicalUnusableCommitError::MissingFileUnleaseable);
            }
            let Some(snapshot) = database
                .technical_unusable_source_snapshot(segment_id)
                .map_err(TechnicalUnusableCommitError::Backend)?
            else {
                return Err(TechnicalUnusableCommitError::SegmentNotFound);
            };
            let crate::db::TechnicalUnusableSourceSnapshot {
                segment,
                review_revision: current_revision,
                source_path_sha256,
                audio_content_hash,
            } = snapshot;
            if current_revision != base_revision {
                return Err(TechnicalUnusableCommitError::StaleRevision { current_revision });
            }
            if segment.human_decision.as_deref().is_some_and(|decision| !decision.trim().is_empty()) {
                return Err(TechnicalUnusableCommitError::AlreadyHumanReviewed);
            }
            (segment.audio_path, source_path_sha256, audio_content_hash)
        };

        let evidence = probe_technical_audio_failure_single_flight(
            &source_path_sha256,
            audio_content_hash.as_deref(),
            Path::new(&audio_path),
        )?;
        if evidence.observation.code() != reason {
            return Err(TechnicalUnusableCommitError::FailureNotReproduced {
                declared_reason: reason.to_string(),
                observed: evidence.observation.code().to_string(),
            });
        }
        after_probe();
        // Admission must recheck after every potentially long probe and immediately before taking
        // write authority. Readable existing-file failures additionally acquire a Windows handle
        // that denies write/delete sharing; `source_lease` remains live until the explicit drop below,
        // after the FULL-sync transaction returns. Missing files were rejected above because no such
        // lease can exist for a negative directory entry.
        let source_lease = acquire_technical_audio_source_lease(
            Path::new(&audio_path),
            &evidence,
            Instant::now() + TECHNICAL_PROBE_TIMEOUT,
        )
        .map_err(|error| {
            tracing::warn!(%error, "Technical-unusable commit could not acquire current source authority");
            TechnicalUnusableCommitError::SourceChanged
        })?;
        after_lease();

        let mutation = self.begin_mutation_at_generation(restore_generation, "mark_segment_unusable_v1_commit")?;
        let database = self.lock_after_mutation("mark_segment_unusable_v1_commit", &mutation);
        let result = match database.mark_segment_technically_unusable_after_verified_failure(
            segment_id,
            base_revision,
            reason,
            &source_path_sha256,
            audio_content_hash.as_deref(),
            operation_id,
        ) {
            Ok(commit) => Ok(commit),
            Err(AppError::Validation(message)) if message == "E_TECHNICAL_UNUSABLE_SEGMENT_NOT_FOUND" => {
                Err(TechnicalUnusableCommitError::SegmentNotFound)
            }
            Err(AppError::Validation(message)) if message == "E_TECHNICAL_UNUSABLE_ALREADY_HUMAN_REVIEWED" => {
                Err(TechnicalUnusableCommitError::AlreadyHumanReviewed)
            }
            Err(AppError::Validation(message)) if message == "E_TECHNICAL_UNUSABLE_SOURCE_CHANGED" => {
                Err(TechnicalUnusableCommitError::SourceChanged)
            }
            Err(AppError::Validation(message)) if message == "E_TECHNICAL_UNUSABLE_MISSING_FILE_UNLEASEABLE" => {
                Err(TechnicalUnusableCommitError::MissingFileUnleaseable)
            }
            Err(AppError::Validation(message)) if message.starts_with("E_STALE_TECHNICAL_UNUSABLE_REVISION:") => {
                let current_revision = message
                    .split_once(':')
                    .and_then(|(_, revision)| revision.parse::<i64>().ok())
                    .unwrap_or_else(|| {
                        database
                            .get_segment_by_id_with_revision(segment_id)
                            .ok()
                            .flatten()
                            .map(|(_, revision)| revision)
                            .unwrap_or(base_revision)
                    });
                Err(TechnicalUnusableCommitError::StaleRevision { current_revision })
            }
            Err(error) => Err(TechnicalUnusableCommitError::Backend(error)),
        };
        // Do not rely on non-lexical-lifetime inference for this security boundary: the named lease
        // is explicitly dropped only after SQLite's FULL-sync transaction has returned.
        drop(source_lease);
        result
    }

    pub(crate) fn undo_latest_desktop_review_flag(
        &self,
        authority: &DesktopReviewFlagUndoAuthority,
        operation_id: &str,
    ) -> AppResult<HumanFlagUndoOutcome> {
        let mutation = self.begin_mutation("undo_latest_desktop_review_flag")?;
        self.lock_after_mutation("undo_latest_desktop_review_flag", &mutation)
            .undo_latest_desktop_review_flag(authority, operation_id)
    }

    #[cfg(test)]
    pub(crate) fn undo_flag(&self, effect_event_id: i64, operation_id: &str) -> AppResult<HumanFlagUndoOutcome> {
        let mutation = self.begin_mutation("undo_review_flag")?;
        self.lock_after_mutation("undo_review_flag", &mutation).undo_review_flag(effect_event_id, operation_id)
    }

    pub(crate) fn clear_legacy_decision(&self, segment_id: &str) -> AppResult<()> {
        self.lock("clear_human_decision").clear_human_decision(segment_id)
    }
}

/// Server-authoritative playback preflight shared by both desktop review contracts. The caller can
/// supply only a segment identity; revision, decoded-audio hash, canonical source span and the
/// evidence verdict are all resolved from the database under the serialized writer lock.
#[cfg(test)]
pub(crate) fn require_listened(database: &crate::db::Database, segment_id: &str) -> AppResult<PlaybackDecisionProof> {
    let audio_content_hash = database
        .segment_audio_content_hash(segment_id)
        .map_err(|error| AppError::Other(format!("playback identity lookup failed: {error}")))?
        .ok_or_else(|| {
            AppError::Other(format!(
                "E_NO_AUDIO_CONTENT_HASH: segment {segment_id} has no server-derived audio content hash"
            ))
        })?;
    let segment_revision = database
        .segment_review_revision(segment_id)
        .map_err(|error| AppError::Other(format!("playback revision lookup failed: {error}")))?
        .unwrap_or(0);
    let (source_start_ms, source_end_ms) = database
        .segment_source_span(segment_id)
        .map_err(|error| AppError::Other(format!("playback source-span lookup failed: {error}")))?
        .ok_or_else(|| {
            AppError::Other(format!("E_NO_AUDIO_SOURCE_SPAN: segment {segment_id} has no canonical server source span"))
        })?;
    match database.has_sufficient_playback_evidence(segment_id, segment_revision, &audio_content_hash, None) {
        Ok(true) => Ok(PlaybackDecisionProof {
            segment_revision,
            audio_content_hash,
            source_start_ms,
            source_end_ms,
            authority_session_id: None,
            source_lease: None,
        }),
        Ok(false) => {
            tracing::warn!(
                "PLAYBACK_EVIDENCE_V3_CONTENT_HASH_RAW_COUNTER_REFUSED: {segment_id} on the desktop at revision {segment_revision}"
            );
            Err(AppError::Other(
                database
                    .require_playback_evidence(segment_id, segment_revision, &audio_content_hash, None)
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "E_NO_PLAYBACK_EVIDENCE".to_string()),
            ))
        }
        // A database fault is not evidence that the reviewer failed to listen.
        Err(error) => Err(AppError::Other(format!("playback evidence check failed: {error}"))),
    }
}

/// Typed desktop commits require the exact policy-4 receipt created for the visible audio attempt.
/// A different anonymous receipt for the same clip is deliberately insufficient: the request's
/// authority is an immutable capability, not a best-match query over ambient evidence. The caller
/// must hold the verified source lease through the decision transaction.
pub(crate) fn require_listened_v4_with_source_lease(
    database: &crate::db::Database,
    segment_id: &str,
    playback_receipt_id: &str,
    source_lease: Option<crate::media::VerifiedMediaSourceLease>,
) -> AppResult<PlaybackDecisionProof> {
    let audio_content_hash = database
        .segment_audio_content_hash(segment_id)
        .map_err(|error| AppError::Other(format!("playback identity lookup failed: {error}")))?
        .ok_or_else(|| {
            AppError::Other(format!(
                "E_NO_AUDIO_CONTENT_HASH: segment {segment_id} has no server-derived audio content hash"
            ))
        })?;
    let segment_revision = database
        .segment_review_revision(segment_id)
        .map_err(|error| AppError::Other(format!("playback revision lookup failed: {error}")))?
        .unwrap_or(0);
    database
        .desktop_playback_proof_v4(
            segment_id,
            segment_revision,
            &audio_content_hash,
            playback_receipt_id,
            source_lease,
        )?
        .ok_or_else(|| {
            AppError::Other(format!(
                "E_NO_PLAYBACK_EVIDENCE: policy-4 receipt {playback_receipt_id} does not record sufficient canonical-media traversal for segment {segment_id} at revision {segment_revision}"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        Database, HumanDecisionUndoOutcome, HumanFlagUndoOutcome, PlaybackDecisionProof, PlaybackReceipt, SpeechSegment,
    };

    static TECHNICAL_PROBE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_technical_probe_tests() -> std::sync::MutexGuard<'static, ()> {
        TECHNICAL_PROBE_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn store_with_clip() -> (tempfile::TempDir, ReviewWriteStore, DatabaseRuntime) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reviews.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database
            .insert_segment(&SpeechSegment {
                id: "clip".into(),
                audio_path: directory.path().join("clip.wav").to_string_lossy().into_owned(),
                raw_transcript: "دەق".into(),
                duration_ms: 10_000,
                alignment_json: Some(
                    r#"{"source_start_ms":0,"source_end_ms":10000,"chunk_index":0,"chunk_count":1}"#.into(),
                ),
                ..SpeechSegment::default()
            })
            .unwrap();
        database
            .connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash = ?2 WHERE id = ?1",
                rusqlite::params!["clip", "a".repeat(64)],
            )
            .unwrap();
        let runtime = DatabaseRuntime::isolated_for_test(database);
        (directory, ReviewWriteStore::new(runtime.clone()), runtime)
    }

    fn write_wav(path: &Path, sample_rate: u32, sample_count: usize) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int },
        )
        .unwrap();
        for index in 0..sample_count {
            writer.write_sample::<i16>((index % 257) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    struct DesktopPlaybackFixture {
        _directory: tempfile::TempDir,
        store: ReviewWriteStore,
        runtime: DatabaseRuntime,
        source: std::path::PathBuf,
        content_hash: String,
        base_revision: i64,
    }

    impl DesktopPlaybackFixture {
        fn begin(&self, media_grant_id: &str, client_attempt_id: &str) -> DesktopPlaybackSession {
            let generation = self.store.capture_restore_generation().unwrap();
            self.store
                .begin_desktop_playback_session_at_generation_v1(
                    "clip",
                    self.base_revision,
                    media_grant_id,
                    client_attempt_id,
                    &self.source,
                    &self.content_hash,
                    None,
                    generation,
                )
                .unwrap()
        }

        fn set_clock(&self, wall_ms: i64, active_ms: u64) {
            self.runtime.lock().unwrap().set_playback_test_clock(wall_ms, active_ms);
        }
    }

    fn desktop_playback_fixture() -> DesktopPlaybackFixture {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("playback.wav");
        write_wav(&source, 16_000, 6_400);
        let content_hash = crate::export_bundle::current_canonical_pcm_blake3(&source).unwrap();
        let database = Database::open(directory.path().join("playback-restore.db").to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database
            .insert_segment(&SpeechSegment {
                id: "clip".into(),
                audio_path: source.to_string_lossy().into_owned(),
                raw_transcript: "دەق".into(),
                duration_ms: 400,
                alignment_json: Some(
                    r#"{"source_start_ms":0,"source_end_ms":400,"chunk_index":0,"chunk_count":1}"#.into(),
                ),
                ..SpeechSegment::default()
            })
            .unwrap();
        database
            .connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params!["clip", content_hash],
            )
            .unwrap();
        let base_revision = database.segment_review_revision("clip").unwrap().unwrap();
        database.set_playback_test_clock(1_000_000, 10_000);
        let runtime = DatabaseRuntime::isolated_for_test(database);
        DesktopPlaybackFixture {
            _directory: directory,
            store: ReviewWriteStore::new(runtime.clone()),
            runtime,
            source,
            content_hash,
            base_revision,
        }
    }

    fn desktop_playback_counts(runtime: &DatabaseRuntime) -> (i64, i64, i64) {
        runtime
            .lock()
            .unwrap()
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM desktop_playback_sessions_v4),
                    (SELECT COUNT(*) FROM desktop_playback_intervals_v4),
                    (SELECT COUNT(*) FROM playback_receipts WHERE policy_version=4)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn desktop_playback_session_exists(runtime: &DatabaseRuntime, playback_receipt_id: &str) -> bool {
        runtime
            .lock()
            .unwrap()
            .connection()
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM desktop_playback_sessions_v4 WHERE playback_receipt_id=?1
                )",
                [playback_receipt_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn store_with_listened_clip(
    ) -> (tempfile::TempDir, ReviewWriteStore, DatabaseRuntime, i64, String, crate::media::VerifiedMediaSourceLease)
    {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("listened.wav");
        write_wav(&source, 16_000, 6_400);
        let content_hash = crate::export_bundle::current_canonical_pcm_blake3(&source).unwrap();
        let path = directory.path().join("listened-reviews.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database
            .insert_segment(&SpeechSegment {
                id: "clip".into(),
                audio_path: source.to_string_lossy().into_owned(),
                raw_transcript: "دەق".into(),
                duration_ms: 400,
                alignment_json: Some(
                    r#"{"source_start_ms":0,"source_end_ms":400,"chunk_index":0,"chunk_count":1}"#.into(),
                ),
                ..SpeechSegment::default()
            })
            .unwrap();
        database
            .connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash = ?2 WHERE id = ?1",
                rusqlite::params!["clip", content_hash],
            )
            .unwrap();
        let base_revision = database.segment_review_revision("clip").unwrap().unwrap();
        let media_grant_id = "10000000-0000-4000-8000-000000000001";
        let client_attempt_id = "10000000-0000-4000-8000-000000000002";
        database.set_playback_test_clock(1_000_000, 10_000);
        let session = database
            .begin_desktop_playback_session_v1(
                "clip",
                base_revision,
                media_grant_id,
                client_attempt_id,
                &source,
                &content_hash,
                None,
            )
            .unwrap();
        database.set_playback_test_clock(1_000_400, 10_400);
        database
            .finalize_desktop_playback_session_v1(
                &session.playback_receipt_id,
                media_grant_id,
                &source,
                &content_hash,
                &[DesktopPlaybackInterval { start_ms: 0, end_ms: 360 }],
            )
            .unwrap();
        let source_lease = crate::media::verify_current_source_lease(&source, &content_hash).unwrap();
        let runtime = DatabaseRuntime::isolated_for_test(database);
        (
            directory,
            ReviewWriteStore::new(runtime.clone()),
            runtime,
            base_revision,
            session.playback_receipt_id,
            source_lease,
        )
    }

    fn wait_for_store_mutation(runtime: &DatabaseRuntime) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !runtime.mutation_active_for_test() {
            assert!(Instant::now() < deadline, "the real store writer never entered restore mutation admission");
            std::thread::yield_now();
        }
    }

    fn run_while_restore_is_reserved<T: Send + 'static>(
        runtime: &DatabaseRuntime,
        operation: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let reservation = runtime.try_reserve_restore_for_test().expect("reserve the isolated runtime restore");
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let value = operation();
            let _ = result_tx.send(value);
        });
        let value = match result_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(value) => value,
            Err(error) => {
                // Release admission before joining so a regression that waited on the DB lock fails
                // cleanly instead of hanging the complete Rust harness forever.
                drop(reservation);
                let _ = worker.join();
                panic!("store write did not fail before a reserved restore: {error}");
            }
        };
        drop(reservation);
        worker.join().expect("store writer must not panic");
        value
    }

    fn effect_counts(runtime: &DatabaseRuntime) -> (i64, i64) {
        runtime
            .lock()
            .unwrap()
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id='clip'),
                    (SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id='clip')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn restore_first_refuses_desktop_playback_begin_finalize_and_cancel_without_mutation() {
        let fixture = desktop_playback_fixture();
        let media_grant_id = "30000000-0000-4000-8000-000000000001";
        let client_attempt_id = "30000000-0000-4000-8000-000000000002";

        let begin_generation = fixture.store.capture_restore_generation().unwrap();
        let begin_store = fixture.store.clone();
        let begin_source = fixture.source.clone();
        let begin_hash = fixture.content_hash.clone();
        let begin_result = run_while_restore_is_reserved(&fixture.runtime, move || {
            begin_store.begin_desktop_playback_session_at_generation_v1(
                "clip",
                0,
                media_grant_id,
                client_attempt_id,
                &begin_source,
                &begin_hash,
                None,
                begin_generation,
            )
        });
        assert!(begin_result.is_err(), "restore-first begin must fail closed");
        assert_eq!(desktop_playback_counts(&fixture.runtime), (0, 0, 0));

        let session = fixture.begin(media_grant_id, client_attempt_id);
        fixture.set_clock(1_000_400, 10_400);
        let intervals = vec![DesktopPlaybackInterval { start_ms: 0, end_ms: 360 }];
        let finalize_generation = fixture.store.capture_restore_generation().unwrap().serial();
        let finalize_store = fixture.store.clone();
        let finalize_receipt_id = session.playback_receipt_id.clone();
        let finalize_source = fixture.source.clone();
        let finalize_hash = fixture.content_hash.clone();
        let finalize_result = run_while_restore_is_reserved(&fixture.runtime, move || {
            finalize_store.finalize_desktop_playback_session_at_generation_v1(
                &finalize_receipt_id,
                media_grant_id,
                &finalize_source,
                &finalize_hash,
                &intervals,
                finalize_generation,
            )
        });
        assert!(finalize_result.is_err(), "restore-first finalization must fail closed");
        assert_eq!(desktop_playback_counts(&fixture.runtime), (1, 0, 0));

        let cancel_store = fixture.store.clone();
        let cancel_receipt_id = session.playback_receipt_id.clone();
        let cancel_result = run_while_restore_is_reserved(&fixture.runtime, move || {
            cancel_store.cancel_desktop_playback_session_v1(&cancel_receipt_id, client_attempt_id)
        });
        assert!(cancel_result.is_err(), "restore-first cancellation must fail closed");
        assert_eq!(desktop_playback_counts(&fixture.runtime), (1, 0, 0));
        assert!(fixture
            .store
            .cancel_desktop_playback_session_v1(&session.playback_receipt_id, client_attempt_id)
            .unwrap());
        assert_eq!(desktop_playback_counts(&fixture.runtime), (0, 0, 0));
    }

    #[test]
    fn desktop_playback_mutations_block_restore_until_begin_finalize_and_cancel_commit() {
        let fixture = desktop_playback_fixture();
        let media_grant_id = "30000000-0000-4000-8000-000000000003";
        let client_attempt_id = "30000000-0000-4000-8000-000000000004";
        let begin_generation = fixture.store.capture_restore_generation().unwrap();
        let writer_blocker = fixture.runtime.lock().unwrap();
        let begin_store = fixture.store.clone();
        let begin_base_revision = fixture.base_revision;
        let begin_source = fixture.source.clone();
        let begin_hash = fixture.content_hash.clone();
        let (begin_tx, begin_rx) = std::sync::mpsc::channel();
        let begin_worker = std::thread::spawn(move || {
            let result = begin_store.begin_desktop_playback_session_at_generation_v1(
                "clip",
                begin_base_revision,
                media_grant_id,
                client_attempt_id,
                &begin_source,
                &begin_hash,
                None,
                begin_generation,
            );
            let _ = begin_tx.send(result);
        });
        wait_for_store_mutation(&fixture.runtime);
        let before_begin: i64 = writer_blocker
            .connection()
            .query_row("SELECT COUNT(*) FROM desktop_playback_sessions_v4", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before_begin, 0, "a blocked begin must expose no partial session");
        assert!(fixture.runtime.try_reserve_restore_for_test().is_err());
        drop(writer_blocker);
        let session = begin_rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        begin_worker.join().unwrap();
        assert_eq!(desktop_playback_counts(&fixture.runtime), (1, 0, 0));

        fixture.set_clock(1_000_400, 10_400);
        let finalize_generation = fixture.store.capture_restore_generation().unwrap().serial();
        let writer_blocker = fixture.runtime.lock().unwrap();
        let finalize_store = fixture.store.clone();
        let finalize_receipt_id = session.playback_receipt_id.clone();
        let finalize_source = fixture.source.clone();
        let finalize_hash = fixture.content_hash.clone();
        let (finalize_tx, finalize_rx) = std::sync::mpsc::channel();
        let finalize_worker = std::thread::spawn(move || {
            let result = finalize_store.finalize_desktop_playback_session_at_generation_v1(
                &finalize_receipt_id,
                media_grant_id,
                &finalize_source,
                &finalize_hash,
                &[DesktopPlaybackInterval { start_ms: 0, end_ms: 360 }],
                finalize_generation,
            );
            let _ = finalize_tx.send(result);
        });
        wait_for_store_mutation(&fixture.runtime);
        let before_finalize: (i64, i64) = writer_blocker
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM desktop_playback_intervals_v4),
                    (SELECT COUNT(*) FROM playback_receipts WHERE policy_version=4)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(before_finalize, (0, 0), "a blocked finalization must expose no partial receipt");
        assert!(fixture.runtime.try_reserve_restore_for_test().is_err());
        drop(writer_blocker);
        finalize_rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        finalize_worker.join().unwrap();
        assert_eq!(desktop_playback_counts(&fixture.runtime), (1, 1, 1));

        let second_grant_id = "30000000-0000-4000-8000-000000000005";
        let second_attempt_id = "30000000-0000-4000-8000-000000000006";
        let second = fixture.begin(second_grant_id, second_attempt_id);
        let writer_blocker = fixture.runtime.lock().unwrap();
        let cancel_store = fixture.store.clone();
        let cancel_receipt_id = second.playback_receipt_id.clone();
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel();
        let cancel_worker = std::thread::spawn(move || {
            let result = cancel_store.cancel_desktop_playback_session_v1(&cancel_receipt_id, second_attempt_id);
            let _ = cancel_tx.send(result);
        });
        wait_for_store_mutation(&fixture.runtime);
        let second_exists: bool = writer_blocker
            .connection()
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM desktop_playback_sessions_v4 WHERE playback_receipt_id=?1
                )",
                [&second.playback_receipt_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(second_exists);
        assert!(fixture.runtime.try_reserve_restore_for_test().is_err());
        drop(writer_blocker);
        assert!(cancel_rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap());
        cancel_worker.join().unwrap();
        assert!(!desktop_playback_session_exists(&fixture.runtime, &second.playback_receipt_id));
        assert_eq!(desktop_playback_counts(&fixture.runtime), (1, 1, 1));
    }

    #[test]
    fn restored_generation_cannot_finalize_or_replay_an_unfinalized_desktop_session() {
        let fixture = desktop_playback_fixture();
        let stale_grant_id = "30000000-0000-4000-8000-000000000007";
        let stale_attempt_id = "30000000-0000-4000-8000-000000000008";
        let stale = fixture.begin(stale_grant_id, stale_attempt_id);
        let stale_generation = fixture.store.capture_restore_generation().unwrap().serial();

        let restore = fixture.runtime.try_reserve_restore_for_test().unwrap();
        restore.arm_named_restore().unwrap();
        fixture
            .runtime
            .with_restore_writer(&restore, |_database| Ok(()))
            .expect("the test restore must reopen the runtime writer");
        restore.commit_named_restore().unwrap();
        drop(restore);
        fixture.set_clock(1_000_400, 10_400);

        let intervals = [DesktopPlaybackInterval { start_ms: 0, end_ms: 360 }];
        let stale_error = fixture
            .store
            .finalize_desktop_playback_session_at_generation_v1(
                &stale.playback_receipt_id,
                stale_grant_id,
                &fixture.source,
                &fixture.content_hash,
                &intervals,
                stale_generation,
            )
            .expect_err("the pre-restore generation must never finalize afterward");
        assert!(stale_error.to_string().contains("generation changed"), "{stale_error}");
        assert_eq!(desktop_playback_counts(&fixture.runtime), (1, 0, 0));

        let current_generation = fixture.store.capture_restore_generation().unwrap().serial();
        let reopened_error = fixture
            .store
            .finalize_desktop_playback_session_at_generation_v1(
                &stale.playback_receipt_id,
                stale_grant_id,
                &fixture.source,
                &fixture.content_hash,
                &intervals,
                current_generation,
            )
            .expect_err("writer reopen must discard the process-local active-time authority");
        assert!(reopened_error.to_string().contains("no live active-time authority"), "{reopened_error}");
        assert_eq!(desktop_playback_counts(&fixture.runtime), (1, 0, 0));
        assert!(
            fixture
                .store
                .replay_finalized_desktop_playback_receipt_v1(&stale.playback_receipt_id, stale_grant_id, &intervals,)
                .unwrap()
                .is_none(),
            "an unfinalized pre-restore session must never appear as replayable receipt evidence"
        );

        let current = fixture.begin(stale_grant_id, stale_attempt_id);
        assert_ne!(
            current.playback_receipt_id, stale.playback_receipt_id,
            "an exact post-restore retry must issue fresh authority, never replay the inert pre-restore session"
        );
        assert!(!desktop_playback_session_exists(&fixture.runtime, &stale.playback_receipt_id));
        assert!(desktop_playback_session_exists(&fixture.runtime, &current.playback_receipt_id));
        assert_eq!(desktop_playback_counts(&fixture.runtime), (1, 0, 0));
    }

    #[test]
    fn typed_decision_admission_precedes_writer_lock_and_refuses_restore_without_losing_exact_replay() {
        let (_directory, store, runtime, base_revision, receipt_id, source_lease) = store_with_listened_clip();
        let operation_id = "20000000-0000-4000-8000-000000000001";
        let writer_blocker = runtime.lock().unwrap();
        let worker_store = store.clone();
        let worker_receipt = receipt_id.clone();
        let worker_lease = source_lease.clone();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = worker_store.commit_typed_decision_with_source_lease(
                "clip",
                base_revision,
                "accept",
                Some("دەق"),
                &worker_receipt,
                operation_id,
                Some(worker_lease),
            );
            let _ = result_tx.send(result);
        });

        wait_for_store_mutation(&runtime);
        let before: i64 = writer_blocker
            .connection()
            .query_row("SELECT COUNT(*) FROM human_decision_effect_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 0, "the blocked writer must not expose partial decision truth");
        let refusal = runtime
            .try_reserve_restore_for_test()
            .err()
            .expect("restore must refuse an admitted typed decision instead of queuing behind it");
        assert!(refusal.contains("mutation is already in progress"), "{refusal}");
        drop(writer_blocker);

        let first = result_rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        worker.join().unwrap();
        let replay = store
            .commit_typed_decision_with_source_lease(
                "clip",
                base_revision,
                "accept",
                Some("دەق"),
                &receipt_id,
                operation_id,
                Some(source_lease),
            )
            .unwrap();
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(replay.decided_revision, first.decided_revision);
        assert_eq!(effect_counts(&runtime), (1, 0), "an exact replay must not duplicate durable truth");
    }

    #[test]
    fn reserved_restore_refuses_typed_decision_before_mutation_and_does_not_consume_operation_identity() {
        let (_directory, store, runtime, base_revision, receipt_id, source_lease) = store_with_listened_clip();
        let operation_id = "20000000-0000-4000-8000-000000000002";
        let refused_store = store.clone();
        let refused_receipt = receipt_id.clone();
        let refused_lease = source_lease.clone();
        let error = run_while_restore_is_reserved(&runtime, move || {
            refused_store.commit_typed_decision_with_source_lease(
                "clip",
                base_revision,
                "accept",
                Some("دەق"),
                &refused_receipt,
                operation_id,
                Some(refused_lease),
            )
        })
        .expect_err("a restore-first typed decision must fail closed");
        assert!(error.to_string().contains("restore is in progress"), "{error}");
        assert_eq!(effect_counts(&runtime), (0, 0));

        let first = store
            .commit_typed_decision_with_source_lease(
                "clip",
                base_revision,
                "accept",
                Some("دەق"),
                &receipt_id,
                operation_id,
                Some(source_lease.clone()),
            )
            .unwrap();
        let replay = store
            .commit_typed_decision_with_source_lease(
                "clip",
                base_revision,
                "accept",
                Some("دەق"),
                &receipt_id,
                operation_id,
                Some(source_lease),
            )
            .unwrap();
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(effect_counts(&runtime), (1, 0));
    }

    #[test]
    fn typed_decision_final_boundary_rejects_pre_restore_media_authority() {
        let (_directory, store, runtime, base_revision, receipt_id, source_lease) = store_with_listened_clip();
        let pre_restore_generation = store.capture_restore_generation().unwrap();
        runtime.advance_restore_generation_for_test().unwrap();
        let error = store
            .commit_typed_decision_with_verified_source_lease(
                "clip",
                base_revision,
                "accept",
                Some("دەق"),
                &receipt_id,
                "20000000-0000-4000-8000-000000000008",
                source_lease,
                pre_restore_generation,
            )
            .expect_err("a media lease verified for an older restore generation must not commit afterward");
        assert!(error.to_string().contains("generation changed"), "{error}");
        assert_eq!(effect_counts(&runtime), (0, 0));
    }

    #[test]
    fn flag_admission_precedes_writer_lock_and_restore_first_refusal_is_zero_mutation() {
        let (_directory, store, runtime) = store_with_clip();
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let operation_id = "20000000-0000-4000-8000-000000000003";
        let writer_blocker = runtime.lock().unwrap();
        let worker_store = store.clone();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = result_tx.send(worker_store.record_flag("clip", base_revision, "owner flag", operation_id));
        });
        wait_for_store_mutation(&runtime);
        let before: i64 = writer_blocker
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 0);
        assert!(runtime.try_reserve_restore_for_test().is_err());
        drop(writer_blocker);
        let first = result_rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        worker.join().unwrap();
        assert_eq!(effect_counts(&runtime), (0, 1));
        assert_eq!(
            store.record_flag("clip", base_revision, "owner flag", operation_id).unwrap().effect_event_id,
            first.effect_event_id
        );

        let (_directory, store, runtime) = store_with_clip();
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let restore_first_operation = "20000000-0000-4000-8000-000000000004";
        let refused_store = store.clone();
        let error = run_while_restore_is_reserved(&runtime, move || {
            refused_store.record_flag("clip", base_revision, "restore-first flag", restore_first_operation)
        })
        .expect_err("a restore-first flag must fail closed");
        assert!(error.to_string().contains("restore is in progress"), "{error}");
        assert_eq!(effect_counts(&runtime), (0, 0));
        let first = store.record_flag("clip", base_revision, "restore-first flag", restore_first_operation).unwrap();
        let replay = store.record_flag("clip", base_revision, "restore-first flag", restore_first_operation).unwrap();
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(effect_counts(&runtime), (0, 1));
    }

    #[test]
    fn technical_unusable_final_admission_precedes_writer_lock_and_restore_first_is_zero_mutation() {
        let _probe_test = lock_technical_probe_tests();
        let (directory, store, runtime) = store_with_clip();
        let source = directory.path().join("clip.wav");
        write_wav(&source, 16_000, 16_000);
        let original_len = std::fs::metadata(&source).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&source).unwrap().set_len(original_len - 1).unwrap();
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let operation_id = "20000000-0000-4000-8000-000000000005";
        let (leased_tx, leased_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_store = store.clone();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = worker_store.mark_technically_unusable_with_hooks(
                "clip",
                base_revision,
                "decodeFailed",
                operation_id,
                || {},
                move || {
                    leased_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            );
            let _ = result_tx.send(result);
        });
        leased_rx.recv_timeout(Duration::from_secs(3)).expect("technical source lease must complete");
        let writer_blocker = runtime.lock().unwrap();
        release_tx.send(()).unwrap();
        wait_for_store_mutation(&runtime);
        let before: i64 = writer_blocker
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 0);
        assert!(runtime.try_reserve_restore_for_test().is_err());
        drop(writer_blocker);
        let first = result_rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        worker.join().unwrap();
        assert_eq!(effect_counts(&runtime), (0, 1));
        assert_eq!(
            store
                .mark_technically_unusable("clip", base_revision, "decodeFailed", operation_id)
                .unwrap()
                .effect_event_id,
            first.effect_event_id
        );

        let (directory, store, runtime) = store_with_clip();
        let source = directory.path().join("clip.wav");
        write_wav(&source, 16_000, 16_000);
        let original_len = std::fs::metadata(&source).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&source).unwrap().set_len(original_len - 1).unwrap();
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let restore_first_operation = "20000000-0000-4000-8000-000000000006";
        let refused_store = store.clone();
        let error = run_while_restore_is_reserved(&runtime, move || {
            refused_store.mark_technically_unusable("clip", base_revision, "decodeFailed", restore_first_operation)
        })
        .expect_err("a restore-first technical flag must fail before probing or mutation");
        assert!(error.to_string().contains("restore is in progress"), "{error}");
        assert_eq!(effect_counts(&runtime), (0, 0));
        let first =
            store.mark_technically_unusable("clip", base_revision, "decodeFailed", restore_first_operation).unwrap();
        let replay =
            store.mark_technically_unusable("clip", base_revision, "decodeFailed", restore_first_operation).unwrap();
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(effect_counts(&runtime), (0, 1));
    }

    #[test]
    fn restore_generation_change_during_technical_probe_cannot_land_pre_restore_truth() {
        let _probe_test = lock_technical_probe_tests();
        let (directory, store, runtime) = store_with_clip();
        let source = directory.path().join("clip.wav");
        write_wav(&source, 16_000, 16_000);
        let original_len = std::fs::metadata(&source).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&source).unwrap().set_len(original_len - 1).unwrap();
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let generation_runtime = runtime.clone();
        let error = store
            .mark_technically_unusable_after_probe(
                "clip",
                base_revision,
                "decodeFailed",
                "20000000-0000-4000-8000-000000000007",
                move || generation_runtime.advance_restore_generation_for_test().unwrap(),
            )
            .expect_err("pre-restore technical evidence must not authorize a post-restore generation");
        assert!(error.to_string().contains("generation changed"), "{error}");
        assert_eq!(effect_counts(&runtime), (0, 0));
        let row = runtime.lock().unwrap().get_segment_by_id("clip").unwrap().unwrap();
        assert!(row.verdict.is_none() && row.rationale.is_none() && !row.escalated);
    }

    #[test]
    fn flag_write_replays_exactly_and_undo_is_effect_bound_and_idempotent() {
        let (_directory, store, runtime) = store_with_clip();
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let flag_operation = "11111111-1111-4111-8111-111111111111";
        let first = store.record_flag("clip", base_revision, "Needs another listen", flag_operation).unwrap();
        let replay = store.record_flag("clip", base_revision, "Needs another listen", flag_operation).unwrap();
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(replay.flag_revision, first.flag_revision);

        let conflict = store
            .record_flag("clip", base_revision, "Different request", flag_operation)
            .expect_err("one operation identity cannot authorize another flag payload");
        assert!(conflict.to_string().contains("different request"), "{conflict}");

        let undo_operation = "22222222-2222-4222-8222-222222222222";
        assert!(matches!(
            store.undo_flag(first.effect_event_id, undo_operation).unwrap(),
            HumanFlagUndoOutcome::Applied { .. }
        ));
        assert!(matches!(
            store.undo_flag(first.effect_event_id, undo_operation).unwrap(),
            HumanFlagUndoOutcome::AlreadyApplied
        ));

        let database = runtime.lock().unwrap();
        let effects: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_events", [], |row| row.get(0))
            .unwrap();
        let reversals: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_reversals", [], |row| row.get(0))
            .unwrap();
        assert_eq!((effects, reversals), (1, 1));
    }

    #[test]
    fn active_generic_flag_rejects_a_new_operation_until_exact_undo() {
        let (_directory, store, runtime) = store_with_clip();
        let first_operation = "33333333-3333-4333-8333-333333333331";
        let next_operation = "33333333-3333-4333-8333-333333333332";
        let undo_operation = "33333333-3333-4333-8333-333333333333";

        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let first = store.record_flag("clip", base_revision, "Needs another listen", first_operation).unwrap();
        let exact_replay = store.record_flag("clip", base_revision, "Needs another listen", first_operation).unwrap();
        assert_eq!(exact_replay.effect_event_id, first.effect_event_id);
        assert_eq!(exact_replay.flag_revision, first.flag_revision);

        let before_rejection_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let before_rejection_counts = effect_counts(&runtime);
        let rejection = store
            .record_flag("clip", first.flag_revision, "Needs another listen", next_operation)
            .expect_err("a different operation must not stack another flag on the active immutable effect");
        assert!(rejection.to_string().contains("active immutable effect"), "{rejection}");
        assert_eq!(
            runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap(),
            before_rejection_revision,
            "rejecting the duplicate flag must not advance review truth"
        );
        assert_eq!(effect_counts(&runtime), before_rejection_counts, "rejecting the duplicate flag must add no effect");

        let restored_revision = match store.undo_flag(first.effect_event_id, undo_operation).unwrap() {
            HumanFlagUndoOutcome::Applied { restored_revision, .. } => restored_revision,
            other => panic!("the exact active flag must be reversible, got {other:?}"),
        };
        assert!(matches!(
            store.undo_flag(first.effect_event_id, undo_operation).unwrap(),
            HumanFlagUndoOutcome::AlreadyApplied
        ));

        let second = store.record_flag("clip", restored_revision, "Needs another listen", next_operation).unwrap();
        assert_eq!(second.prior_revision, restored_revision);
        assert_eq!(second.flag_revision, restored_revision + 1);
        let second_replay =
            store.record_flag("clip", restored_revision, "Needs another listen", next_operation).unwrap();
        assert_eq!(second_replay.effect_event_id, second.effect_event_id);
        assert_eq!(second_replay.flag_revision, second.flag_revision);

        let database = runtime.lock().unwrap();
        let (revision, effects, reversals): (i64, i64, i64) = database
            .connection()
            .query_row(
                "SELECT
                    review_revision,
                    (SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id='clip'),
                    (SELECT COUNT(*) FROM review_flag_effect_reversals)
                   FROM speech_segments
                  WHERE id='clip'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(revision, second.flag_revision);
        assert_eq!((effects, reversals), (2, 1), "only the initial flag and the post-undo flag may exist");
    }

    #[test]
    fn desktop_decision_undo_uses_only_the_immutable_effect_identity() {
        let (_directory, store, runtime) = store_with_clip();
        let effect_event_id = {
            let database = runtime.lock().unwrap();
            let audio_content_hash = database.segment_audio_content_hash("clip").unwrap().unwrap();
            let revision = database.segment_review_revision("clip").unwrap().unwrap_or(0);
            let (source_start_ms, source_end_ms) = database.segment_source_span("clip").unwrap().unwrap();
            database
                .record_playback_receipt(&PlaybackReceipt {
                    segment_id: "clip".into(),
                    segment_revision: revision,
                    audio_content_hash: audio_content_hash.clone(),
                    reviewer: None,
                    session_id: None,
                    started_at_ms: 1,
                    played_ms: 10_000,
                    clip_duration_ms: 10_000,
                    source_start_ms: None,
                    source_end_ms: None,
                })
                .unwrap();
            database
                .finalize_human_review_with_playback(
                    "clip",
                    "accept",
                    None,
                    Some(1_700_000_000_001),
                    &PlaybackDecisionProof {
                        segment_revision: revision,
                        audio_content_hash,
                        source_start_ms,
                        source_end_ms,
                        authority_session_id: None,
                        source_lease: None,
                    },
                    "33333333-3333-4333-8333-333333333333",
                )
                .unwrap()
                .effect_event_id
        };

        let operation_id = "44444444-4444-4444-8444-444444444444";
        let DesktopReviewUndoAvailability::Available(DesktopReviewUndoAuthority::Decision(authority)) =
            store.desktop_review_undo_availability().unwrap()
        else {
            panic!("store fixture must expose typed desktop Undo authority");
        };
        assert_eq!(authority.effect_event_id, effect_event_id);
        let outcome = store.undo_latest_desktop_human_decision(&authority, operation_id).unwrap();
        assert!(matches!(outcome, HumanDecisionUndoOutcome::Applied { .. }));
        let replay = store.undo_latest_desktop_human_decision(&authority, operation_id).unwrap();
        assert!(matches!(replay, HumanDecisionUndoOutcome::AlreadyApplied { .. }));

        let database = runtime.lock().unwrap();
        let segment = database.get_segment_by_id("clip").unwrap().unwrap();
        assert!(!segment.verified);
        assert!(segment.human_decision.is_none());
    }

    #[test]
    fn retired_identity_free_clear_remains_fail_closed_through_the_store() {
        let (_directory, store, runtime) = store_with_clip();
        let error = store.clear_legacy_decision("clip").expect_err("identity-free clear must stay retired");
        assert!(error.to_string().contains("immutable decision effect id"), "{error}");
        let database = runtime.lock().unwrap();
        assert!(database.get_segment_by_id("clip").unwrap().is_some());
    }

    #[test]
    fn finalized_receipt_cannot_commit_after_same_path_audio_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("same-path.wav");
        write_wav(&source, 16_000, 160_000);
        let database = Database::open(directory.path().join("reviews.db").to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database
            .insert_segment(&SpeechSegment {
                id: "same-path".into(),
                audio_path: source.to_string_lossy().into_owned(),
                raw_transcript: "دەق".into(),
                duration_ms: 10_000,
                alignment_json: Some(
                    r#"{"source_start_ms":0,"source_end_ms":10000,"chunk_index":0,"chunk_count":1}"#.into(),
                ),
                ..SpeechSegment::default()
            })
            .unwrap();
        let original_hash = crate::export_bundle::current_canonical_pcm_blake3(&source).unwrap();
        database
            .connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params!["same-path", original_hash],
            )
            .unwrap();
        let base_revision = database.segment_review_revision("same-path").unwrap().unwrap();
        database
            .connection()
            .execute(
                "INSERT INTO review_drafts(segment_id,base_revision,text,updated_at)
                 VALUES('same-path',?1,'retained correction','2026-08-26T00:00:00Z')",
                [base_revision],
            )
            .unwrap();
        let media_grant_id = uuid::Uuid::new_v4().to_string();
        let client_attempt_id = uuid::Uuid::new_v4().to_string();
        database.set_playback_test_clock(1_000_000, 10_000);
        let session = database
            .begin_desktop_playback_session_v1(
                "same-path",
                base_revision,
                &media_grant_id,
                &client_attempt_id,
                &source,
                &original_hash,
                None,
            )
            .unwrap();
        database.set_playback_test_clock(1_005_000, 15_000);
        database
            .finalize_desktop_playback_session_v1(
                &session.playback_receipt_id,
                &media_grant_id,
                &source,
                &original_hash,
                &[crate::db::DesktopPlaybackInterval { start_ms: 0, end_ms: 8_500 }],
            )
            .unwrap();
        let runtime = DatabaseRuntime::new(database);
        let store = ReviewWriteStore::new(runtime.clone());

        let mut replacement = hound::WavWriter::create(
            &source,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..160_000 {
            replacement.write_sample::<i16>(-1_200).unwrap();
        }
        replacement.finalize().unwrap();
        assert_ne!(crate::export_bundle::current_canonical_pcm_blake3(&source).unwrap(), original_hash);

        let error = store
            .commit_typed_decision(
                "same-path",
                base_revision,
                "edit",
                Some("retained correction"),
                &session.playback_receipt_id,
                "99999999-9999-4999-8999-999999999999",
            )
            .expect_err("a finalized receipt for replaced bytes must fail before human truth changes");
        assert!(error.to_string().contains(PLAYBACK_EVIDENCE_CHANGED), "{error}");

        let database = runtime.lock().unwrap();
        let row = database.get_segment_by_id("same-path").unwrap().unwrap();
        assert!(row.human_decision.is_none() && !row.verified);
        let state: (i64, i64, String) = database
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id='same-path'),
                    (SELECT COUNT(*) FROM review_drafts WHERE segment_id='same-path'),
                    (SELECT text FROM review_drafts WHERE segment_id='same-path')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (0, 1, "retained correction".to_string()));
    }

    #[test]
    fn technical_unusable_commit_rejects_same_revision_source_swap_after_probe() {
        let _probe_test = lock_technical_probe_tests();
        let (directory, store, runtime) = store_with_clip();
        std::fs::write(directory.path().join("clip.wav"), b"not an audio container").unwrap();
        let replacement = directory.path().join("healthy-replacement.wav");
        write_wav(&replacement, 16_000, 16_000);
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let update_runtime = runtime.clone();
        let error = store
            .mark_technically_unusable_after_probe(
                "clip",
                base_revision,
                "corruptContainer",
                "55555555-5555-4555-8555-555555555555",
                || {
                    let database = update_runtime.lock().unwrap();
                    database.connection().execute_batch("DROP TRIGGER speech_segments_review_revision;").unwrap();
                    database
                        .connection()
                        .execute(
                            "UPDATE speech_segments
                                SET audio_path = ?2, audio_content_hash = ?3
                              WHERE id = ?1",
                            rusqlite::params!["clip", replacement.to_string_lossy(), "b".repeat(64)],
                        )
                        .unwrap();
                },
            )
            .expect_err("the verified corrupt source must not authorize a healthy replacement");
        assert!(matches!(error, TechnicalUnusableCommitError::SourceChanged), "{error}");

        let database = runtime.lock().unwrap();
        let row = database.get_segment_by_id("clip").unwrap().unwrap();
        assert_eq!(database.segment_review_revision("clip").unwrap(), Some(base_revision));
        assert_eq!(row.audio_path, replacement.to_string_lossy());
        assert!(row.verdict.is_none() && row.rationale.is_none() && !row.escalated);
        let effects: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id='clip'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(effects, 0, "a source swap must leave no partial technical effect");
    }

    #[test]
    fn missing_file_technical_unusable_is_rejected_without_any_mutation() {
        let (_directory, store, runtime) = store_with_clip();
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        runtime
            .lock()
            .unwrap()
            .connection()
            .execute(
                "INSERT INTO review_drafts(segment_id,base_revision,text,updated_at)
                 VALUES('clip',?1,'must remain',datetime('now'))",
                [base_revision],
            )
            .unwrap();

        let error = store
            .mark_technically_unusable_after_probe(
                "clip",
                base_revision,
                "missingFile",
                "56565656-5656-4656-8656-565656565656",
                || panic!("missing-file refusal must happen before the technical probe completes"),
            )
            .expect_err("an absent path cannot mint technical-unusable truth");
        assert!(matches!(error, TechnicalUnusableCommitError::MissingFileUnleaseable), "{error}");

        let database = runtime.lock().unwrap();
        let row = database.get_segment_by_id("clip").unwrap().unwrap();
        assert_eq!(database.segment_review_revision("clip").unwrap(), Some(base_revision));
        assert!(row.verdict.is_none() && row.rationale.is_none() && !row.escalated);
        let snapshot = database.technical_unusable_source_snapshot("clip").unwrap().unwrap();
        let bypass_error = database
            .mark_segment_technically_unusable_after_verified_failure(
                "clip",
                base_revision,
                "missingFile",
                &snapshot.source_path_sha256,
                snapshot.audio_content_hash.as_deref(),
                "58585858-5858-4858-8858-585858585858",
            )
            .expect_err("the persistence boundary must reject a future store bypass too");
        assert!(bypass_error.to_string().contains("E_TECHNICAL_UNUSABLE_MISSING_FILE_UNLEASEABLE"));
        let state: (i64, i64, String) = database
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id='clip'),
                    (SELECT COUNT(*) FROM review_drafts WHERE segment_id='clip'),
                    (SELECT text FROM review_drafts WHERE segment_id='clip')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (0, 1, "must remain".to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn technical_unusable_existing_file_lease_blocks_competing_replacement_through_commit() {
        let _probe_test = lock_technical_probe_tests();
        let (directory, store, runtime) = store_with_clip();
        let source = directory.path().join("clip.wav");
        write_wav(&source, 16_000, 16_000);
        let original_len = std::fs::metadata(&source).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&source).unwrap().set_len(original_len - 1).unwrap();

        let healthy_replacement = directory.path().join("healthy-replacement.wav");
        write_wav(&healthy_replacement, 16_000, 16_000);
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let attempts = Arc::new(Mutex::new(None));
        let attempts_during_lease = Arc::clone(&attempts);
        let source_during_lease = source.clone();

        let commit = store
            .mark_technically_unusable_with_hooks(
                "clip",
                base_revision,
                "decodeFailed",
                "57575757-5757-4757-8757-575757575757",
                || {},
                move || {
                    let competitor = std::thread::spawn(move || {
                        let write_blocked = std::fs::OpenOptions::new().write(true).open(&source_during_lease).is_err();
                        let delete_blocked = std::fs::remove_file(&source_during_lease).is_err();
                        (write_blocked, delete_blocked)
                    });
                    *attempts_during_lease.lock().unwrap() = Some(competitor.join().unwrap());
                },
            )
            .expect("the sealed, still-corrupt source should authorize exactly one technical effect");
        assert_eq!(commit.flag_revision, base_revision + 1);
        assert_eq!(
            *attempts.lock().unwrap(),
            Some((true, true)),
            "a competing writer and the delete step of same-path replacement must both be denied while the lease is held"
        );

        {
            let database = runtime.lock().unwrap();
            let row = database.get_segment_by_id("clip").unwrap().unwrap();
            assert!(crate::quality::is_technically_unusable(&row));
            let effects: i64 = database
                .connection()
                .query_row("SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id='clip'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(effects, 1);
        }

        // The same replacement succeeds once the API returns, proving the failures above came from
        // the transaction-scoped source lease rather than ambient directory permissions.
        std::fs::remove_file(&source).unwrap();
        std::fs::rename(&healthy_replacement, &source).unwrap();
        assert!(source.is_file());
    }

    #[test]
    fn technical_probe_strictly_limits_distinct_blocking_sources() {
        let _probe_test = lock_technical_probe_tests();
        let started = Arc::new(std::sync::Barrier::new(3));
        let release = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for index in 0..2 {
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            workers.push(std::thread::spawn(move || {
                let source_hash = if index == 0 { "c".repeat(64) } else { "d".repeat(64) };
                probe_technical_audio_failure_single_flight_with(
                    &source_hash,
                    Some(&"e".repeat(64)),
                    Path::new("unused"),
                    move |_path, _deadline| {
                        started.wait();
                        release.wait();
                        TechnicalAudioFailureEvidence {
                            observation: TechnicalAudioProbeObservation::Healthy,
                            source_blake3: None,
                        }
                    },
                )
            }));
        }
        started.wait();
        let third = probe_technical_audio_failure_single_flight_with(
            &"f".repeat(64),
            Some(&"1".repeat(64)),
            Path::new("unused"),
            |_path, _deadline| TechnicalAudioFailureEvidence {
                observation: TechnicalAudioProbeObservation::Healthy,
                source_blake3: None,
            },
        )
        .expect_err("a third distinct decoder must be refused before it starts");
        assert!(matches!(third, TechnicalUnusableCommitError::ProbeBusy));
        release.wait();
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap().observation, TechnicalAudioProbeObservation::Healthy);
        }
        assert!(lock_probe_registry().active.is_empty());
        assert!(lock_probe_registry().max_active_observed <= TECHNICAL_PROBE_MAX_CONCURRENCY);
    }

    #[test]
    fn concurrent_long_healthy_audio_claims_create_zero_effects() {
        let _probe_test = lock_technical_probe_tests();
        let (directory, store, runtime) = store_with_clip();
        // Sixteen minutes at 8 kHz is deliberately much larger than a review clip. The probe walks
        // packets without accumulating PCM; same-source callers share at most one active flight.
        write_wav(&directory.path().join("clip.wav"), 8_000, 16 * 60 * 8_000);
        let base_revision = runtime.lock().unwrap().segment_review_revision("clip").unwrap().unwrap();
        let start = Arc::new(std::sync::Barrier::new(9));
        let mut workers = Vec::new();
        for index in 0..8 {
            let store = store.clone();
            let start = Arc::clone(&start);
            workers.push(std::thread::spawn(move || {
                start.wait();
                store.mark_technically_unusable(
                    "clip",
                    base_revision,
                    "decodeFailed",
                    &format!("00000000-0000-4000-8000-{index:012}"),
                )
            }));
        }
        start.wait();
        for worker in workers {
            let error = worker.join().unwrap().expect_err("healthy long audio is never technical-failure authority");
            assert!(matches!(
                error,
                TechnicalUnusableCommitError::FailureNotReproduced { .. } | TechnicalUnusableCommitError::ProbeBusy
            ));
        }

        let database = runtime.lock().unwrap();
        assert_eq!(database.segment_review_revision("clip").unwrap(), Some(base_revision));
        let effects: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id='clip'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(effects, 0);
        assert!(lock_probe_registry().max_active_observed <= TECHNICAL_PROBE_MAX_CONCURRENCY);
    }

    #[test]
    fn truncated_audio_tail_is_never_accepted_as_clean_eof() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("truncated-tail.wav");
        write_wav(&path, 16_000, 16_000);
        let original_len = std::fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(original_len - 1).unwrap();

        let evidence = probe_technical_audio_failure(&path, Instant::now() + Duration::from_secs(5));
        assert_eq!(
            evidence.observation,
            TechnicalAudioProbeObservation::DecodeFailed,
            "only Symphonia Ok(None) may prove a clean end of stream"
        );
        assert!(evidence.source_blake3.is_some(), "the failure must bind the exact truncated bytes");
    }
}
