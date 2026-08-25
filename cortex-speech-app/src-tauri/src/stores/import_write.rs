//! Durable import publication, rollback and background-metadata write boundaries.

use crate::database_runtime::{begin_mutation, DatabaseRuntime};
use crate::db::SpeechSegment;
use crate::error::{AppError, AppResult};
use std::sync::MutexGuard;

#[derive(Clone)]
pub(crate) struct ImportWriteStore {
    runtime: DatabaseRuntime,
}

impl ImportWriteStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    fn lock(&self, operation: &str) -> MutexGuard<'_, crate::db::Database> {
        self.runtime.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during an import write");
            poisoned.into_inner()
        })
    }

    pub(crate) fn publish_segments(&self, segments: &[SpeechSegment]) -> AppResult<()> {
        let _mutation = begin_mutation().map_err(AppError::Other)?;
        self.lock("publish_import_segments").insert_segments_batch(segments)
    }

    pub(crate) fn rollback_segments(&self, segment_ids: &[String]) -> AppResult<()> {
        let _mutation = begin_mutation().map_err(AppError::Other)?;
        self.lock("rollback_import_segments").delete_segments_batch(segment_ids)
    }

    pub(crate) fn update_alignment_if_unchanged(
        &self,
        segment_id: &str,
        expected_alignment: Option<&str>,
        alignment_json: &str,
        quality: &str,
    ) -> AppResult<bool> {
        let _mutation = begin_mutation().map_err(AppError::Other)?;
        self.lock("update_import_alignment").update_segment_alignment_if_unchanged(
            segment_id,
            expected_alignment,
            alignment_json,
            quality,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn store() -> (tempfile::TempDir, ImportWriteStore, DatabaseRuntime) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("imports.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        let runtime = DatabaseRuntime::new(database);
        let store = ImportWriteStore::new(runtime.clone());
        (directory, store, runtime)
    }

    fn segment(id: &str, alignment_json: Option<String>) -> SpeechSegment {
        SpeechSegment {
            id: id.into(),
            audio_path: format!("C:/recordings/{id}.wav"),
            raw_transcript: format!("draft-{id}"),
            duration_ms: 1_000,
            alignment_json,
            ..SpeechSegment::default()
        }
    }

    #[test]
    fn import_publication_and_rollback_are_atomic_through_the_store() {
        let (_directory, store, runtime) = store();
        let mut forged = segment("forged", None);
        forged.annotated_transcript = Some("unbound human truth".into());
        assert!(store.publish_segments(&[segment("must-not-land", None), forged]).is_err());
        assert_eq!(runtime.open_read().unwrap().segment_count().unwrap(), 0);

        let segments = [segment("one", None), segment("two", None)];
        store.publish_segments(&segments).unwrap();
        assert_eq!(runtime.open_read().unwrap().segment_count().unwrap(), 2);
        store.rollback_segments(&["one".into(), "two".into()]).unwrap();
        assert_eq!(runtime.open_read().unwrap().segment_count().unwrap(), 0);
    }

    #[test]
    fn background_alignment_cannot_clobber_metadata_that_changed_after_inference_started() {
        let (_directory, store, runtime) = store();
        let original = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1}"#;
        let first = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1,"words":[]}"#;
        let newer = r#"{"source_start_ms":100,"source_end_ms":900,"chunk_index":0,"chunk_count":1}"#;
        store.publish_segments(&[segment("aligned", Some(original.into()))]).unwrap();

        assert!(store.update_alignment_if_unchanged("aligned", Some(original), first, "ctc_forced").unwrap());
        runtime.lock().unwrap().update_segment_alignment("aligned", newer, "energy_heuristic").unwrap();
        assert!(!store.update_alignment_if_unchanged("aligned", Some(first), original, "ctc_forced").unwrap());

        let retained = runtime.open_read().unwrap().get_segment_by_id("aligned").unwrap().unwrap();
        assert_eq!(retained.alignment_json.as_deref(), Some(newer));
        assert_eq!(retained.alignment_quality.as_deref(), Some("energy_heuristic"));
    }
}
