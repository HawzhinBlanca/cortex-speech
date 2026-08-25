//! Serialized human-review effect writes that already expose a stable database-domain contract.

use crate::database_runtime::DatabaseRuntime;
use crate::db::{
    HumanDecisionCommit, HumanDecisionUndoOutcome, HumanFlagCommit, HumanFlagUndoOutcome, PlaybackDecisionProof,
};
use crate::error::{AppError, AppResult};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReviewCommitError {
    #[error("the review segment no longer exists")]
    SegmentNotFound,
    #[error("the review revision is stale; current revision is {current_revision}")]
    StaleRevision { current_revision: i64 },
    #[error(transparent)]
    Backend(#[from] AppError),
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

    /// Legacy desktop compatibility boundary. Exact operation replay is resolved before current
    /// playback preflight because the first successful decision advances the review revision.
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
    pub(crate) fn commit_typed_decision(
        &self,
        segment_id: &str,
        base_revision: i64,
        decision: &str,
        transcript: Option<&str>,
        operation_id: &str,
    ) -> Result<HumanDecisionCommit, ReviewCommitError> {
        let database = self.lock("commit_review_v1");
        if let Some(commit) = database.replay_desktop_review_v1_and_clear_draft(
            segment_id,
            base_revision,
            decision,
            transcript,
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

        let playback = require_listened(&database, segment_id)?;
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
        Ok(true) => Ok(PlaybackDecisionProof { segment_revision, audio_content_hash, source_start_ms, source_end_ms }),
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
}
