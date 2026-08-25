//! Durable segment deletion, undo-history capture and speaker rename boundaries.

use crate::database_runtime::{begin_mutation, DatabaseRuntime, MutationGuard};
use crate::error::{AppError, AppResult};
use crate::history::{Command, HistoryManager};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub(crate) struct SegmentWriteStore {
    runtime: DatabaseRuntime,
    history: Arc<Mutex<HistoryManager>>,
}

/// Keeps restore admission fenced until the command has persisted its matching session state.
pub(crate) struct SegmentMutation {
    _admission: MutationGuard<'static>,
}

impl SegmentWriteStore {
    pub(crate) fn new(runtime: DatabaseRuntime, history: Arc<Mutex<HistoryManager>>) -> Self {
        Self { runtime, history }
    }

    fn lock_database(&self, operation: &str) -> MutexGuard<'_, crate::db::Database> {
        self.runtime.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during a segment write");
            poisoned.into_inner()
        })
    }

    fn lock_history(&self, operation: &str) -> MutexGuard<'_, HistoryManager> {
        self.history.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned history lock during a segment write");
            poisoned.into_inner()
        })
    }

    pub(crate) fn delete_one(&self, id: &str) -> AppResult<SegmentMutation> {
        let admission = begin_mutation().map_err(AppError::Other)?;
        let database = self.lock_database("delete_segment");
        let previous = database.get_segment_by_id(id)?;
        database.delete_segment(id)?;
        drop(database);

        if let Some(segment) = previous {
            self.lock_history("delete_segment").push(Command::DeleteSegments { segments: vec![segment] });
        }
        Ok(SegmentMutation { _admission: admission })
    }

    pub(crate) fn delete_batch(&self, ids: &[String]) -> AppResult<SegmentMutation> {
        let admission = begin_mutation().map_err(AppError::Other)?;
        let database = self.lock_database("delete_segments_batch");
        let segments = database.get_segments_by_ids(ids)?;
        database.delete_segments_batch(ids)?;
        drop(database);

        if !segments.is_empty() {
            self.lock_history("delete_segments_batch").push(Command::DeleteSegments { segments });
        }
        Ok(SegmentMutation { _admission: admission })
    }

    pub(crate) fn rename_speaker(&self, old_id: &str, new_id: &str) -> AppResult<usize> {
        self.lock_database("rename_speaker").rename_speaker(old_id, new_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, SpeechSegment};

    fn store_with_segments() -> (tempfile::TempDir, SegmentWriteStore, DatabaseRuntime, Arc<Mutex<HistoryManager>>) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("segments.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        for (id, speaker) in [("one", "speaker-a"), ("two", "speaker-a"), ("three", "speaker-b")] {
            database
                .insert_segment(&SpeechSegment {
                    id: id.into(),
                    audio_path: directory.path().join(format!("{id}.wav")).to_string_lossy().into_owned(),
                    raw_transcript: format!("draft-{id}"),
                    speaker_id: Some(speaker.into()),
                    duration_ms: 1_000,
                    ..SpeechSegment::default()
                })
                .unwrap();
        }
        let runtime = DatabaseRuntime::new(database);
        let history = Arc::new(Mutex::new(HistoryManager::new(20)));
        let store = SegmentWriteStore::new(runtime.clone(), Arc::clone(&history));
        (directory, store, runtime, history)
    }

    #[test]
    fn deletion_captures_the_server_row_and_exact_undo_restores_it() {
        let (_directory, store, runtime, history) = store_with_segments();
        store.delete_one("one").unwrap();
        assert!(runtime.open_read().unwrap().get_segment_by_id("one").unwrap().is_none());

        let database = runtime.lock().unwrap();
        let history = history.lock().unwrap();
        assert_eq!(history.undo(&database).unwrap().as_deref(), Some("Delete segments"));
        let restored = database.get_segment_by_id("one").unwrap().unwrap();
        assert_eq!(restored.raw_transcript, "draft-one");
        assert_eq!(restored.speaker_id.as_deref(), Some("speaker-a"));
    }

    #[test]
    fn batch_delete_and_speaker_rename_share_the_serialized_store_boundary() {
        let (_directory, store, runtime, _history) = store_with_segments();
        assert_eq!(store.rename_speaker("speaker-a", "speaker-z").unwrap(), 2);
        let renamed = runtime.open_read().unwrap().get_segments_by_ids(&["one".into(), "two".into()]).unwrap();
        assert!(renamed.iter().all(|segment| segment.speaker_id.as_deref() == Some("speaker-z")));

        store.delete_batch(&["one".into(), "three".into()]).unwrap();
        let remaining =
            runtime.open_read().unwrap().get_segments_by_ids(&["one".into(), "two".into(), "three".into()]).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "two");
    }
}
