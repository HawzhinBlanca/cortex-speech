//! Durable segment deletion, undo-history capture and speaker rename boundaries.

use crate::database_runtime::{begin_mutation, DatabaseRuntime, MutationGuard};
use crate::error::{AppError, AppResult};
use crate::history::{Command, HistoryManager};
use crate::validation::input as validate;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SegmentMetadataChange {
    SpeakerId { expected: Option<String>, value: Option<String> },
    AlignmentJson { expected: Option<String>, value: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdatedSegmentMetadata {
    pub(crate) segment_id: String,
    pub(crate) speaker_id: Option<String>,
    pub(crate) alignment_json: Option<String>,
    pub(crate) changed: bool,
}

#[derive(Debug)]
pub(crate) enum SegmentMetadataUpdateError {
    Application(AppError),
    Missing,
    Conflict(&'static str),
}

impl From<AppError> for SegmentMetadataUpdateError {
    fn from(error: AppError) -> Self {
        Self::Application(error)
    }
}

impl From<String> for SegmentMetadataUpdateError {
    fn from(error: String) -> Self {
        Self::Application(AppError::Validation(error))
    }
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

    /// Atomically compare-and-set the nullable metadata fields owned by the library workspace.
    ///
    /// An exact lost-response replay is a success when the current value already equals `value`.
    /// Otherwise every edited field must still equal the renderer's `expected` value; all conflicts
    /// are detected before any field is changed, so a mixed request can never partially commit.
    pub(crate) fn update_metadata_v1(
        &self,
        segment_id: &str,
        changes: &[SegmentMetadataChange],
    ) -> Result<(UpdatedSegmentMetadata, SegmentMutation), SegmentMetadataUpdateError> {
        let admission = begin_mutation().map_err(AppError::Other)?;
        if changes.is_empty() {
            return Err(AppError::Validation("segment metadata update requires at least one change".into()).into());
        }
        if changes.len() > 2 {
            return Err(AppError::Validation("segment metadata update accepts at most two changes".into()).into());
        }

        let mut speaker_change: Option<(&Option<String>, &Option<String>)> = None;
        let mut alignment_change: Option<(&Option<String>, &Option<String>)> = None;
        for change in changes {
            match change {
                SegmentMetadataChange::SpeakerId { expected, value } => {
                    if speaker_change.replace((expected, value)).is_some() {
                        return Err(AppError::Validation(
                            "speakerId appears more than once in one metadata update".into(),
                        )
                        .into());
                    }
                    if let Some(speaker) = value {
                        if !speaker.is_empty() {
                            validate::validate_text(speaker, 256, "Speaker ID")?;
                        }
                    }
                }
                SegmentMetadataChange::AlignmentJson { expected, value } => {
                    if alignment_change.replace((expected, value)).is_some() {
                        return Err(AppError::Validation(
                            "alignmentJson appears more than once in one metadata update".into(),
                        )
                        .into());
                    }
                    if let Some(alignment) = value {
                        validate::validate_alignment_json(alignment)?;
                    }
                }
            }
        }

        let database = self.lock_database("update_segment_metadata_v1");
        let Some(mut segment) = database.get_segment_by_id(segment_id)? else {
            return Err(SegmentMetadataUpdateError::Missing);
        };

        if let Some((expected, value)) = speaker_change {
            if segment.speaker_id != *expected && segment.speaker_id != *value {
                return Err(SegmentMetadataUpdateError::Conflict("speakerId"));
            }
        }
        if let Some((expected, value)) = alignment_change {
            if segment.alignment_json != *expected && segment.alignment_json != *value {
                return Err(SegmentMetadataUpdateError::Conflict("alignmentJson"));
            }
        }

        let previous_speaker = segment.speaker_id.clone();
        let previous_alignment = segment.alignment_json.clone();
        if let Some((_, value)) = speaker_change {
            segment.speaker_id.clone_from(value);
        }
        if let Some((_, value)) = alignment_change {
            segment.alignment_json.clone_from(value);
        }
        let changed = segment.speaker_id != previous_speaker || segment.alignment_json != previous_alignment;
        if changed {
            let history = self.lock_history("update_segment_metadata_v1");
            HistoryManager::persist_segment_update(&database, &history, &segment)?;
        }
        let result = UpdatedSegmentMetadata {
            segment_id: segment.id,
            speaker_id: segment.speaker_id,
            alignment_json: segment.alignment_json,
            changed,
        };
        drop(database);
        Ok((result, SegmentMutation { _admission: admission }))
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

    #[test]
    fn metadata_compare_and_set_refuses_a_stale_same_field_write() {
        let (_directory, store, runtime, _history) = store_with_segments();
        let first =
            [SegmentMetadataChange::SpeakerId { expected: Some("speaker-a".into()), value: Some("speaker-z".into()) }];
        let (updated, admission) = store.update_metadata_v1("one", &first).unwrap();
        drop(admission);
        assert!(updated.changed);
        assert_eq!(updated.speaker_id.as_deref(), Some("speaker-z"));

        let stale = [SegmentMetadataChange::SpeakerId {
            expected: Some("speaker-a".into()),
            value: Some("must-not-clobber".into()),
        }];
        assert!(matches!(
            store.update_metadata_v1("one", &stale),
            Err(SegmentMetadataUpdateError::Conflict("speakerId"))
        ));
        let retained = runtime.open_read().unwrap().get_segment_by_id("one").unwrap().unwrap();
        assert_eq!(retained.speaker_id.as_deref(), Some("speaker-z"));
        assert_eq!(retained.raw_transcript, "draft-one");
    }

    #[test]
    fn metadata_update_is_atomic_and_an_exact_lost_response_replay_is_idempotent() {
        let (_directory, store, runtime, _history) = store_with_segments();
        let alignment = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1}"#.to_string();
        let request = [
            SegmentMetadataChange::SpeakerId { expected: Some("speaker-a".into()), value: Some("speaker-z".into()) },
            SegmentMetadataChange::AlignmentJson { expected: None, value: Some(alignment.clone()) },
        ];
        let (first, admission) = store.update_metadata_v1("one", &request).unwrap();
        drop(admission);
        assert!(first.changed);
        assert_eq!(first.alignment_json.as_deref(), Some(alignment.as_str()));

        let (replay, admission) = store.update_metadata_v1("one", &request).unwrap();
        drop(admission);
        assert!(!replay.changed, "an exact replay must acknowledge the existing server value");

        let conflict = [
            SegmentMetadataChange::SpeakerId {
                expected: Some("speaker-a".into()),
                value: Some("must-not-commit".into()),
            },
            SegmentMetadataChange::AlignmentJson { expected: Some(alignment.clone()), value: None },
        ];
        assert!(matches!(
            store.update_metadata_v1("one", &conflict),
            Err(SegmentMetadataUpdateError::Conflict("speakerId"))
        ));
        let retained = runtime.open_read().unwrap().get_segment_by_id("one").unwrap().unwrap();
        assert_eq!(retained.speaker_id.as_deref(), Some("speaker-z"));
        assert_eq!(retained.alignment_json.as_deref(), Some(alignment.as_str()));
    }
}
