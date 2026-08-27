//! Query-only segment, library and review access.

use crate::database_runtime::DatabaseRuntime;
use crate::db::{AudioHealth, SegmentsPage, SpeechSegment};
use crate::error::AppResult;
use crate::quality;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SpeakerInventoryItem {
    pub(crate) speaker_id: Option<String>,
    pub(crate) segment_count: usize,
    pub(crate) total_duration_seconds: f64,
}

#[derive(Clone)]
pub(crate) struct SegmentQueryStore {
    runtime: DatabaseRuntime,
}

impl SegmentQueryStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    pub(crate) fn get_segment(&self, segment_id: &str) -> AppResult<Option<SpeechSegment>> {
        self.runtime.open_read()?.get_segment_by_id(segment_id)
    }

    /// Resolve the target for one-off transcription without permitting an arbitrary first row when
    /// multiple segments share the same source recording.
    pub(crate) fn resolve_transcription_segment(
        &self,
        audio_path: &str,
        alignment_json: Option<&str>,
    ) -> AppResult<Option<String>> {
        let database = self.runtime.open_read()?;
        if let Some(alignment_json) = alignment_json {
            return database.get_segment_id_by_audio_alignment(audio_path, alignment_json).map_err(|error| {
                crate::error::AppError::Other(format!("transcribe: segment lookup by alignment failed: {error}"))
            });
        }

        let ids = database.segment_ids_for_audio_path(audio_path).map_err(|error| {
            crate::error::AppError::Other(format!("transcribe: segment lookup by audio path failed: {error}"))
        })?;
        if ids.len() > 1 {
            return Err(crate::error::AppError::Validation(format!(
                "transcribe: {} segments share this audio file; pass an explicit segment_id (or alignment_json) to choose which one to transcribe",
                ids.len()
            )));
        }
        Ok(ids.into_iter().next())
    }

    pub(crate) fn get_segments(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        self.runtime.open_read()?.get_segments(verified)
    }

    pub(crate) fn get_segments_suspect_first(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        self.runtime.open_read()?.get_segments_suspect_first(verified)
    }

    pub(crate) fn search_segments(&self, query: &str) -> AppResult<Vec<SpeechSegment>> {
        self.runtime.open_read()?.search_segments(query)
    }

    pub(crate) fn get_segments_page(
        &self,
        verified: Option<bool>,
        query: Option<&str>,
        sort: &str,
        limit: usize,
        cursor: Option<&str>,
        focus: Option<&HashSet<String>>,
    ) -> AppResult<SegmentsPage> {
        self.runtime.open_read()?.get_segments_page_focused(verified, query, sort, limit, cursor, focus)
    }

    pub(crate) fn get_escalation_review_page(
        &self,
        limit: usize,
        cursor: Option<&str>,
        focus: Option<&HashSet<String>>,
    ) -> AppResult<SegmentsPage> {
        self.runtime.open_read()?.get_escalation_review_page(limit, cursor, focus)
    }

    pub(crate) fn get_segment_ids_for_view(
        &self,
        verified: Option<bool>,
        query: Option<&str>,
        transcript_state: &str,
    ) -> AppResult<Vec<String>> {
        self.runtime.open_read()?.get_segment_ids_for_view(verified, query, transcript_state)
    }

    pub(crate) fn get_signal_anomaly_segments(&self, limit: usize) -> AppResult<Vec<SpeechSegment>> {
        self.runtime.open_read()?.get_signal_anomaly_segments(limit)
    }

    pub(crate) fn audio_health(&self) -> AppResult<AudioHealth> {
        self.runtime.open_read()?.audio_health()
    }

    /// Return every speaker group without collapsing SQL NULL into a user-chosen string. This is
    /// the inventory authority used by the compare-and-set rename flow, not the truncated dashboard
    /// summary.
    pub(crate) fn speaker_inventory(&self) -> AppResult<Vec<SpeakerInventoryItem>> {
        let database = self.runtime.open_read()?;
        let mut statement = database.connection().prepare(
            "SELECT speaker_id, COUNT(*), COALESCE(SUM(duration_ms), 0)
             FROM speech_segments
             GROUP BY speaker_id",
        )?;
        let mut speakers: Vec<SpeakerInventoryItem> = statement
            .query_map([], |row| {
                Ok(SpeakerInventoryItem {
                    speaker_id: row.get(0)?,
                    segment_count: row.get::<_, i64>(1)? as usize,
                    total_duration_seconds: row.get::<_, i64>(2)? as f64 / 1000.0,
                })
            })?
            .collect::<Result<_, _>>()?;
        speakers.sort_by(|left, right| {
            right.segment_count.cmp(&left.segment_count).then_with(|| left.speaker_id.cmp(&right.speaker_id))
        });
        Ok(speakers)
    }

    /// Preserve the established active-learning selection rule while keeping its scan, tally and
    /// hydration on one stable query snapshot. The global-threshold ranking remains intentionally
    /// naive until a frozen Gold Marathon calibration split can support a separate evidence-backed
    /// selection-policy change.
    pub(crate) fn active_learning_queue(
        &self,
        target_error: f64,
        confidence_level: f64,
        limit: usize,
    ) -> AppResult<Vec<SpeechSegment>> {
        let db = self.runtime.open_read()?;
        let mut tally = quality::conformal::ConformalTally::default();
        let mut scored: Vec<(String, f64)> = Vec::new();
        db.for_each_segment(None, |segment| {
            if !segment.verified {
                scored.push((segment.id.clone(), quality::conformal::compute_nonconformity_score(&segment)));
            }
            tally.push(&segment);
        })?;
        let threshold = tally.finish(target_error, confidence_level).threshold;

        scored.sort_by(|left, right| {
            let (left_uncertainty, right_uncertainty) = (-(left.1 - threshold).abs(), -(right.1 - threshold).abs());
            right_uncertainty.partial_cmp(&left_uncertainty).unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);

        let ids: Vec<String> = scored.iter().map(|(id, _)| id.clone()).collect();
        let rows = db.get_segments_by_ids(&ids)?;
        let by_id: HashMap<&str, &SpeechSegment> = rows.iter().map(|segment| (segment.id.as_str(), segment)).collect();
        Ok(ids.iter().filter_map(|id| by_id.get(id.as_str()).map(|segment| (*segment).clone())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn empty_initialized_store_supports_every_migrated_query_shape() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        let store = SegmentQueryStore::new(DatabaseRuntime::new(database));

        assert!(store.get_segments(None).unwrap().is_empty());
        assert!(store.get_segments_suspect_first(None).unwrap().is_empty());
        assert!(store.search_segments("missing").unwrap().is_empty());
        assert!(store.get_segment_ids_for_view(None, None, "any").unwrap().is_empty());
        assert!(store.get_signal_anomaly_segments(10).unwrap().is_empty());
        assert!(store.speaker_inventory().unwrap().is_empty());
        assert!(store.active_learning_queue(0.1, 0.95, 10).unwrap().is_empty());
        assert_eq!(store.resolve_transcription_segment("missing.wav", None).unwrap(), None);
        let page = store.get_segments_page(Some(false), None, "oldest", 10, None, None).unwrap();
        assert_eq!(page.total, 0);
        assert!(page.items.is_empty());
    }

    #[test]
    fn speaker_inventory_keeps_unassigned_distinct_from_literal_unknown_and_returns_every_group() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("speakers.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        for index in 0..12 {
            let speaker_id = match index {
                0 => None,
                1 => Some("unknown".to_string()),
                _ => Some(format!("speaker-{index}")),
            };
            database
                .insert_segment(&SpeechSegment {
                    id: format!("segment-{index}"),
                    audio_path: directory.path().join(format!("{index}.wav")).to_string_lossy().into_owned(),
                    speaker_id,
                    duration_ms: 1_000,
                    ..SpeechSegment::default()
                })
                .unwrap();
        }

        let store = SegmentQueryStore::new(DatabaseRuntime::new(database));
        let inventory = store.speaker_inventory().unwrap();
        assert_eq!(inventory.len(), 12, "the inventory must not truncate or merge speaker groups");
        assert!(inventory.iter().any(|speaker| speaker.speaker_id.is_none()));
        assert!(inventory.iter().any(|speaker| speaker.speaker_id.as_deref() == Some("unknown")));
    }

    #[test]
    fn transcription_lookup_is_exact_ambiguity_safe_and_error_preserving() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lookup.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        for (id, alignment) in [
            ("first", r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":2}"#),
            ("second", r#"{"source_start_ms":1000,"source_end_ms":2000,"chunk_index":1,"chunk_count":2}"#),
        ] {
            database
                .insert_segment(&SpeechSegment {
                    id: id.into(),
                    audio_path: "shared.wav".into(),
                    alignment_json: Some(alignment.into()),
                    ..Default::default()
                })
                .unwrap();
        }
        let store = SegmentQueryStore::new(DatabaseRuntime::new(database));
        assert_eq!(
            store
                .resolve_transcription_segment(
                    "shared.wav",
                    Some(r#"{"source_start_ms":1000,"source_end_ms":2000,"chunk_index":1,"chunk_count":2}"#),
                )
                .unwrap(),
            Some("second".into())
        );
        assert_eq!(store.resolve_transcription_segment("shared.wav", Some(r#"{"missing":1}"#)).unwrap(), None);
        assert!(store.resolve_transcription_segment("shared.wav", None).is_err());

        let bare_path = directory.path().join("bare.db");
        let bare = Database::open(bare_path.to_str().unwrap()).unwrap();
        let bare_store = SegmentQueryStore::new(DatabaseRuntime::new(bare));
        let error = bare_store
            .resolve_transcription_segment("shared.wav", Some("{}"))
            .expect_err("a missing schema must not masquerade as an absent segment");
        assert!(error.to_string().contains("segment lookup by alignment failed"), "{error}");
    }
}
