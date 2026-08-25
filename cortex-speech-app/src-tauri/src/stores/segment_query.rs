//! Query-only segment, library and review access.

use crate::database_runtime::DatabaseRuntime;
use crate::db::{AudioHealth, SegmentsPage, SpeechSegment};
use crate::error::AppResult;
use crate::quality;
use std::collections::{HashMap, HashSet};

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
        assert!(store.active_learning_queue(0.1, 0.95, 10).unwrap().is_empty());
        let page = store.get_segments_page(Some(false), None, "oldest", 10, None, None).unwrap();
        assert_eq!(page.total, 0);
        assert!(page.items.is_empty());
    }
}
