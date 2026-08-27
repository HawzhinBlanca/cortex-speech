//! Serialized playback-evidence writes.
//!
//! The renderer reports only an observation. The database remains the sole authority for review
//! revision, decoded-audio content hash, canonical source span and coverage denominator.

use crate::database_runtime::DatabaseRuntime;
use crate::db::PlaybackReceiptObservation;
use crate::error::AppResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaybackObservation {
    pub(crate) segment_id: String,
    pub(crate) reviewer: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) started_at_ms: i64,
    pub(crate) played_ms: i64,
    /// Retained for wire compatibility and non-negative validation only. The database replaces this
    /// claim with the canonical decoded clip duration before calculating or storing coverage.
    pub(crate) claimed_clip_duration_ms: i64,
}

#[derive(Clone)]
pub(crate) struct PlaybackWriteStore {
    runtime: DatabaseRuntime,
}

impl PlaybackWriteStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    pub(crate) fn record_observation(&self, observation: PlaybackObservation) -> AppResult<()> {
        let database = self.runtime.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned database lock while recording playback evidence");
            poisoned.into_inner()
        });

        database.record_playback_observation(PlaybackReceiptObservation {
            segment_id: observation.segment_id,
            reviewer: observation.reviewer,
            session_id: observation.session_id,
            started_at_ms: observation.started_at_ms,
            played_ms: observation.played_ms,
            claimed_clip_duration_ms: observation.claimed_clip_duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, SpeechSegment};

    fn store_with_clip() -> (tempfile::TempDir, PlaybackWriteStore, DatabaseRuntime) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("playback.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database
            .insert_segment(&SpeechSegment {
                id: "clip".into(),
                audio_path: directory.path().join("clip.wav").to_string_lossy().into_owned(),
                raw_transcript: "دەق".into(),
                duration_ms: 10_000,
                alignment_json: Some(
                    r#"{"source_start_ms":2000,"source_end_ms":12000,"chunk_index":0,"chunk_count":1}"#.into(),
                ),
                ..SpeechSegment::default()
            })
            .unwrap();
        database
            .connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash = ?2, review_revision = 7 WHERE id = ?1",
                rusqlite::params!["clip", "a".repeat(64)],
            )
            .unwrap();
        let runtime = DatabaseRuntime::new(database);
        (directory, PlaybackWriteStore::new(runtime.clone()), runtime)
    }

    fn observation() -> PlaybackObservation {
        PlaybackObservation {
            segment_id: "clip".into(),
            reviewer: Some("owner".into()),
            session_id: Some("session".into()),
            started_at_ms: 100,
            played_ms: 9_000,
            claimed_clip_duration_ms: 1,
        }
    }

    #[test]
    fn renderer_duration_cannot_shrink_coverage_or_supply_review_identity() {
        let (_directory, store, runtime) = store_with_clip();
        store.record_observation(observation()).unwrap();

        let database = runtime.lock().unwrap();
        let stored: (i64, String, i64, i64, i64, f64) = database
            .connection()
            .query_row(
                "SELECT segment_revision, audio_fingerprint, clip_duration_ms,
                        source_start_ms, source_end_ms, coverage_ratio
                   FROM playback_receipts WHERE segment_id = 'clip'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
        assert_eq!(stored.0, 7);
        assert_eq!(stored.1, "a".repeat(64));
        assert_eq!((stored.2, stored.3, stored.4), (10_000, 2_000, 12_000));
        assert!((stored.5 - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn missing_server_audio_identity_fails_without_a_partial_receipt() {
        let (_directory, store, runtime) = store_with_clip();
        {
            let database = runtime.lock().unwrap();
            database
                .connection()
                .execute("UPDATE speech_segments SET audio_content_hash = NULL WHERE id = 'clip'", [])
                .unwrap();
        }

        let error = store.record_observation(observation()).expect_err("missing server identity must fail closed");
        assert!(error.to_string().contains("server-derived audio content hash"), "{error}");
        let database = runtime.lock().unwrap();
        let receipts: i64 =
            database.connection().query_row("SELECT COUNT(*) FROM playback_receipts", [], |row| row.get(0)).unwrap();
        assert_eq!(receipts, 0);
    }

    #[test]
    fn invalid_observation_timing_fails_before_any_receipt_is_written() {
        let (_directory, store, runtime) = store_with_clip();
        let mut invalid = observation();
        invalid.started_at_ms = -1;
        assert!(store.record_observation(invalid).is_err());

        let database = runtime.lock().unwrap();
        let receipts: i64 =
            database.connection().query_row("SELECT COUNT(*) FROM playback_receipts", [], |row| row.get(0)).unwrap();
        assert_eq!(receipts, 0);
    }
}
