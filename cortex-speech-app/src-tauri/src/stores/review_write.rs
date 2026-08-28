//! Serialized human-review effect writes that already expose a stable database-domain contract.

use crate::database_runtime::DatabaseRuntime;
use crate::db::{
    HumanDecisionCommit, HumanDecisionUndoOutcome, HumanFlagCommit, HumanFlagUndoOutcome, PlaybackDecisionProof,
    PLAYBACK_EVIDENCE_CHANGED,
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

    /// Resolve the server-owned live media grant behind a playback receipt without leaking a raw
    /// database connection into the command layer. The command may use this id only to borrow the
    /// registry's already-verified source lease; the typed commit repeats the exact receipt/source
    /// checks under the serialized write boundary before changing human truth.
    pub(crate) fn desktop_playback_media_grant_id(&self, playback_receipt_id: &str) -> AppResult<Option<String>> {
        self.lock("desktop_playback_media_grant_id").desktop_playback_media_grant_id(playback_receipt_id)
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
        let database = self.lock("record_human_decision");
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
        if let Some(source_lease) = source_lease {
            return self.commit_typed_decision_with_verified_source_lease(
                segment_id,
                base_revision,
                decision,
                transcript,
                playback_receipt_id,
                operation_id,
                source_lease,
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
            let database = self.lock("commit_review_v1_source_recovery");
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
    ) -> Result<HumanDecisionCommit, ReviewCommitError> {
        let database = self.lock("commit_review_v1");
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

    pub(crate) fn undo_human_decision(
        &self,
        effect_event_id: i64,
        actor: Option<&str>,
        operation_id: &str,
    ) -> AppResult<HumanDecisionUndoOutcome> {
        self.lock("undo_human_decision").undo_human_decision(effect_event_id, actor, operation_id)
    }

    pub(crate) fn record_flag(
        &self,
        segment_id: &str,
        rationale: &str,
        operation_id: &str,
    ) -> AppResult<HumanFlagCommit> {
        self.lock("record_review_flag").record_review_flag(segment_id, rationale, operation_id)
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
        let (audio_path, source_path_sha256, audio_content_hash) = {
            let database = self.lock("mark_segment_unusable_v1_replay");
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

        let database = self.lock("mark_segment_unusable_v1_commit");
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

    pub(crate) fn undo_flag(&self, effect_event_id: i64, operation_id: &str) -> AppResult<HumanFlagUndoOutcome> {
        self.lock("undo_review_flag").undo_review_flag(effect_event_id, operation_id)
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
        let runtime = DatabaseRuntime::new(database);
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

    #[test]
    fn flag_write_replays_exactly_and_undo_is_effect_bound_and_idempotent() {
        let (_directory, store, runtime) = store_with_clip();
        let flag_operation = "11111111-1111-4111-8111-111111111111";
        let first = store.record_flag("clip", "Needs another listen", flag_operation).unwrap();
        let replay = store.record_flag("clip", "Needs another listen", flag_operation).unwrap();
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(replay.flag_revision, first.flag_revision);

        let conflict = store
            .record_flag("clip", "Different request", flag_operation)
            .expect_err("one operation identity cannot authorize another flag payload");
        assert!(conflict.to_string().contains("different request"), "{conflict}");

        let undo_operation = "22222222-2222-4222-8222-222222222222";
        assert!(matches!(
            store.undo_flag(first.effect_event_id, undo_operation).unwrap(),
            HumanFlagUndoOutcome::Applied { .. }
        ));
        assert!(matches!(
            store.undo_flag(first.effect_event_id, undo_operation).unwrap(),
            HumanFlagUndoOutcome::AlreadyApplied { .. }
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
        let outcome = store.undo_human_decision(effect_event_id, None, operation_id).unwrap();
        assert!(matches!(outcome, HumanDecisionUndoOutcome::Applied { .. }));
        let replay = store.undo_human_decision(effect_event_id, None, operation_id).unwrap();
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

    /// The technical-probe registry is a process-global static with a hard concurrency cap of
    /// TECHNICAL_PROBE_MAX_CONCURRENCY, and `cargo test` runs these tests in parallel. Without this
    /// lock a sibling test can hold a slot while
    /// `technical_probe_strictly_limits_distinct_blocking_sources` is mid-setup: its second worker is
    /// then refused with ProbeBusy and returns WITHOUT reaching `started.wait()`, so its 3-party
    /// Barrier waits forever for a party that will never arrive. A std Barrier has no timeout, so the
    /// whole suite hangs — which is exactly how the Rust coverage prerequisite burned its full 7200s
    /// budget on CI while reporting nothing. Same idiom as GLOBAL_SESSION_LOCK in couch.rs.
    static PROBE_REGISTRY_SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn technical_probe_strictly_limits_distinct_blocking_sources() {
        let _serial = PROBE_REGISTRY_SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let _serial = PROBE_REGISTRY_SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let _serial = PROBE_REGISTRY_SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
