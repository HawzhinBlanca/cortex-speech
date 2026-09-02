//! Durable segment deletion, undo-history capture and speaker rename boundaries.

use crate::database_runtime::{begin_mutation, DatabaseRuntime, MutationGuard};
use crate::error::AppError;
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

#[derive(Debug)]
pub(crate) enum SegmentDeleteError {
    Invalid,
    Authority,
    Busy,
    Application,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenamedSpeaker {
    pub(crate) source_speaker_id: Option<String>,
    pub(crate) target_speaker_id: String,
    pub(crate) renamed_count: usize,
    pub(crate) target_count: usize,
    pub(crate) merged: bool,
}

#[derive(Debug)]
pub(crate) enum SpeakerRenameError {
    Invalid,
    Stale { source_count: usize, target_count: usize },
    Busy,
    Application,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssignedSpeakers {
    pub(crate) requested_count: usize,
    pub(crate) changed_count: usize,
}

#[derive(Debug)]
pub(crate) enum SpeakerAssignmentError {
    Invalid,
    Stale,
    Busy,
    Application,
}

impl From<AppError> for SpeakerAssignmentError {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Validation(message)
                if message.contains("no longer exists")
                    || message.contains("changed concurrently")
                    || message.contains("disappeared") =>
            {
                Self::Stale
            }
            AppError::Validation(_) => Self::Invalid,
            AppError::Database(rusqlite::Error::SqliteFailure(code, _))
                if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) =>
            {
                Self::Busy
            }
            _ => Self::Application,
        }
    }
}

impl From<AppError> for SpeakerRenameError {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Validation(_) => Self::Invalid,
            AppError::Database(rusqlite::Error::SqliteFailure(code, _))
                if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) =>
            {
                Self::Busy
            }
            _ => Self::Application,
        }
    }
}

impl From<AppError> for SegmentDeleteError {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Validation(message) if message.contains("durable review authority") => Self::Authority,
            AppError::Validation(_) => Self::Invalid,
            AppError::Database(rusqlite::Error::SqliteFailure(code, _))
                if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) =>
            {
                Self::Busy
            }
            _ => Self::Application,
        }
    }
}

impl SegmentWriteStore {
    pub(crate) fn new(runtime: DatabaseRuntime, history: Arc<Mutex<HistoryManager>>) -> Self {
        Self { runtime, history }
    }

    fn lock_database_after_mutation(
        &self,
        operation: &str,
        mutation: &MutationGuard<'_>,
    ) -> MutexGuard<'_, crate::db::Database> {
        self.runtime.lock_after_mutation(mutation).unwrap_or_else(|poisoned| {
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

        let database = self.lock_database_after_mutation("update_segment_metadata_v1", &admission);
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

    pub(crate) fn delete_batch(&self, ids: &[String]) -> Result<(usize, SegmentMutation), SegmentDeleteError> {
        let admission = begin_mutation().map_err(AppError::Other).map_err(SegmentDeleteError::from)?;
        let database = self.lock_database_after_mutation("delete_segments_batch", &admission);
        let segments = database.get_segments_by_ids(ids).map_err(SegmentDeleteError::from)?;
        database.delete_segments_batch(ids).map_err(SegmentDeleteError::from)?;
        drop(database);

        let deleted_count = segments.len();
        if deleted_count > 0 {
            self.lock_history("delete_segments_batch").push(Command::DeleteSegments { segments });
        }
        Ok((deleted_count, SegmentMutation { _admission: admission }))
    }

    pub(crate) fn rename_speaker_v1(
        &self,
        old_id: Option<&str>,
        new_id: &str,
        expected_source_count: usize,
        expected_target_count: usize,
    ) -> Result<(RenamedSpeaker, SegmentMutation), SpeakerRenameError> {
        let admission = begin_mutation().map_err(AppError::Other).map_err(SpeakerRenameError::from)?;
        if expected_source_count == 0 || old_id == Some(new_id) {
            return Err(SpeakerRenameError::Invalid);
        }
        if let Some(old_id) = old_id {
            validate::validate_text(old_id, 256, "Source speaker label").map_err(|_| SpeakerRenameError::Invalid)?;
        }
        validate::validate_speaker_label(new_id).map_err(|_| SpeakerRenameError::Invalid)?;

        let database = self.lock_database_after_mutation("rename_speaker_v1", &admission);
        let history_changes = database
            .rename_speaker_with_inventory(old_id, new_id, expected_source_count, expected_target_count)
            .map_err(SpeakerRenameError::from)?;
        let Some(history_changes) = history_changes else {
            let (source_count, target_count) =
                database.speaker_counts(old_id, new_id).map_err(SpeakerRenameError::from)?;
            return Err(SpeakerRenameError::Stale { source_count, target_count });
        };
        let renamed_count = history_changes.len();
        debug_assert_eq!(renamed_count, expected_source_count);
        drop(database);
        self.lock_history("rename_speaker_v1").push(Command::SpeakerAssignment { changes: history_changes });

        Ok((
            RenamedSpeaker {
                source_speaker_id: old_id.map(str::to_owned),
                target_speaker_id: new_id.to_owned(),
                renamed_count,
                target_count: expected_target_count + renamed_count,
                merged: expected_target_count > 0,
            },
            SegmentMutation { _admission: admission },
        ))
    }

    pub(crate) fn assign_speaker_batch_v1(
        &self,
        ids: &[String],
        target_speaker_id: Option<&str>,
    ) -> Result<(AssignedSpeakers, SegmentMutation), SpeakerAssignmentError> {
        let admission = begin_mutation().map_err(AppError::Other).map_err(SpeakerAssignmentError::from)?;
        if ids.is_empty() || ids.len() > 100_000 {
            return Err(SpeakerAssignmentError::Invalid);
        }
        let mut unique_ids = std::collections::HashSet::with_capacity(ids.len());
        for id in ids {
            validate::validate_identifier(id).map_err(|_| SpeakerAssignmentError::Invalid)?;
            if !unique_ids.insert(id.as_str()) {
                return Err(SpeakerAssignmentError::Invalid);
            }
        }
        if let Some(speaker_id) = target_speaker_id {
            validate::validate_speaker_label(speaker_id).map_err(|_| SpeakerAssignmentError::Invalid)?;
        }

        let database = self.lock_database_after_mutation("assign_speaker_batch_v1", &admission);
        let changes =
            database.assign_speaker_batch_atomic(ids, target_speaker_id).map_err(SpeakerAssignmentError::from)?;
        let changed_count = changes.len();
        drop(database);
        if !changes.is_empty() {
            self.lock_history("assign_speaker_batch_v1").push(Command::SpeakerAssignment { changes });
        }
        Ok((AssignedSpeakers { requested_count: ids.len(), changed_count }, SegmentMutation { _admission: admission }))
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
        let (_, admission) = store.delete_batch(&["one".into()]).unwrap();
        drop(admission);
        assert!(runtime.open_read().unwrap().get_segment_by_id("one").unwrap().is_none());

        let database = runtime.lock().unwrap();
        let history = history.lock().unwrap();
        assert_eq!(history.undo(&database).unwrap(), Some(crate::history::HistoryAction::DeleteSegments));
        let restored = database.get_segment_by_id("one").unwrap().unwrap();
        assert_eq!(restored.raw_transcript, "draft-one");
        assert_eq!(restored.speaker_id.as_deref(), Some("speaker-a"));
    }

    #[test]
    fn batch_delete_and_speaker_rename_share_the_serialized_store_boundary() {
        let (_directory, store, runtime, _history) = store_with_segments();
        let (renamed, admission) = store.rename_speaker_v1(Some("speaker-a"), "speaker-z", 2, 0).unwrap();
        drop(admission);
        assert_eq!(renamed.renamed_count, 2);
        assert!(!renamed.merged);
        let renamed = runtime.open_read().unwrap().get_segments_by_ids(&["one".into(), "two".into()]).unwrap();
        assert!(renamed.iter().all(|segment| segment.speaker_id.as_deref() == Some("speaker-z")));

        let (deleted, admission) = store.delete_batch(&["one".into(), "three".into()]).unwrap();
        drop(admission);
        assert_eq!(deleted, 2);
        let remaining =
            runtime.open_read().unwrap().get_segments_by_ids(&["one".into(), "two".into(), "three".into()]).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "two");
    }

    #[test]
    fn speaker_rename_refuses_stale_source_or_target_inventory_without_any_partial_write() {
        let (_directory, store, runtime, _history) = store_with_segments();
        let stale_source = match store.rename_speaker_v1(Some("speaker-a"), "speaker-z", 1, 0) {
            Err(error) => error,
            Ok(_) => panic!("a stale source count must not rename any segment"),
        };
        assert!(matches!(stale_source, SpeakerRenameError::Stale { source_count: 2, target_count: 0 }));
        assert!(runtime
            .open_read()
            .unwrap()
            .get_segments_by_ids(&["one".into(), "two".into()])
            .unwrap()
            .iter()
            .all(|segment| segment.speaker_id.as_deref() == Some("speaker-a")));

        let stale_target = match store.rename_speaker_v1(Some("speaker-a"), "speaker-b", 2, 0) {
            Err(error) => error,
            Ok(_) => panic!("an unconfirmed target group must not be merged"),
        };
        assert!(matches!(stale_target, SpeakerRenameError::Stale { source_count: 2, target_count: 1 }));
        let retained =
            runtime.open_read().unwrap().get_segments_by_ids(&["one".into(), "two".into(), "three".into()]).unwrap();
        assert_eq!(retained.iter().filter(|segment| segment.speaker_id.as_deref() == Some("speaker-a")).count(), 2);
        assert_eq!(retained.iter().filter(|segment| segment.speaker_id.as_deref() == Some("speaker-b")).count(), 1);
    }

    #[test]
    fn speaker_rename_handles_sql_null_without_touching_literal_unknown_and_rejects_same_id_noops() {
        let (_directory, store, runtime, _history) = store_with_segments();
        {
            let database = runtime.lock().unwrap();
            database
                .insert_segment(&SpeechSegment {
                    id: "unassigned".into(),
                    audio_path: "unassigned.wav".into(),
                    speaker_id: None,
                    ..SpeechSegment::default()
                })
                .unwrap();
            database
                .insert_segment(&SpeechSegment {
                    id: "literal-unknown".into(),
                    audio_path: "literal-unknown.wav".into(),
                    speaker_id: Some("unknown".into()),
                    ..SpeechSegment::default()
                })
                .unwrap();
        }

        let (renamed, admission) = store.rename_speaker_v1(None, "assigned", 1, 0).unwrap();
        drop(admission);
        assert_eq!(renamed.renamed_count, 1);
        let read = runtime.open_read().unwrap();
        assert_eq!(read.get_segment_by_id("unassigned").unwrap().unwrap().speaker_id.as_deref(), Some("assigned"));
        assert_eq!(read.get_segment_by_id("literal-unknown").unwrap().unwrap().speaker_id.as_deref(), Some("unknown"));
        drop(read);

        assert!(matches!(
            store.rename_speaker_v1(Some("speaker-a"), "speaker-a", 2, 2),
            Err(SpeakerRenameError::Invalid)
        ));
    }

    #[test]
    fn speaker_rename_and_merge_have_exact_server_owned_undo_and_redo() {
        let (_directory, store, runtime, history) = store_with_segments();
        let (renamed, admission) = store.rename_speaker_v1(Some("speaker-a"), "speaker-b", 2, 1).unwrap();
        drop(admission);
        assert!(renamed.merged);
        assert_eq!(renamed.target_count, 3);

        {
            let database = runtime.lock().unwrap();
            let history = history.lock().unwrap();
            assert_eq!(history.undo(&database).unwrap(), Some(crate::history::HistoryAction::SpeakerAssignment));
        }
        let restored =
            runtime.open_read().unwrap().get_segments_by_ids(&["one".into(), "two".into(), "three".into()]).unwrap();
        assert_eq!(restored.iter().filter(|segment| segment.speaker_id.as_deref() == Some("speaker-a")).count(), 2);
        assert_eq!(restored.iter().filter(|segment| segment.speaker_id.as_deref() == Some("speaker-b")).count(), 1);

        {
            let database = runtime.lock().unwrap();
            let history = history.lock().unwrap();
            assert_eq!(history.redo(&database).unwrap(), Some(crate::history::HistoryAction::SpeakerAssignment));
        }
        let redone =
            runtime.open_read().unwrap().get_segments_by_ids(&["one".into(), "two".into(), "three".into()]).unwrap();
        assert!(redone.iter().all(|segment| segment.speaker_id.as_deref() == Some("speaker-b")));
    }

    #[test]
    fn batch_speaker_assignment_is_atomic_replay_safe_and_exactly_undoable() {
        let (_directory, store, runtime, history) = store_with_segments();
        let ids = ["one".to_string(), "three".to_string()];
        let (assigned, admission) = store.assign_speaker_batch_v1(&ids, Some("Shara Karim")).unwrap();
        drop(admission);
        assert_eq!(assigned.requested_count, 2);
        assert_eq!(assigned.changed_count, 2);

        let (replay, admission) = store.assign_speaker_batch_v1(&ids, Some("Shara Karim")).unwrap();
        drop(admission);
        assert_eq!(replay.changed_count, 0, "an exact replay must not rewrite timestamps or history");

        {
            let database = runtime.lock().unwrap();
            let history = history.lock().unwrap();
            assert_eq!(history.undo(&database).unwrap(), Some(crate::history::HistoryAction::SpeakerAssignment));
            assert!(!history.can_undo(), "the no-op replay must not add a second history entry");
        }
        let restored = runtime.open_read().unwrap().get_segments_by_ids(&ids).unwrap();
        assert_eq!(
            restored.iter().find(|segment| segment.id == "one").unwrap().speaker_id.as_deref(),
            Some("speaker-a")
        );
        assert_eq!(
            restored.iter().find(|segment| segment.id == "three").unwrap().speaker_id.as_deref(),
            Some("speaker-b")
        );
    }

    #[test]
    fn batch_speaker_assignment_rolls_back_every_row_on_mid_batch_database_failure() {
        let (_directory, store, runtime, history) = store_with_segments();
        runtime
            .lock()
            .unwrap()
            .connection()
            .execute_batch(
                "CREATE TRIGGER test_fail_second_speaker
                 BEFORE UPDATE OF speaker_id ON speech_segments
                 WHEN old.id = 'two'
                 BEGIN SELECT RAISE(ABORT, 'forced speaker failure'); END;",
            )
            .unwrap();
        let ids = ["one".to_string(), "two".to_string()];
        assert!(matches!(
            store.assign_speaker_batch_v1(&ids, Some("speaker-z")),
            Err(SpeakerAssignmentError::Application)
        ));
        let retained = runtime.open_read().unwrap().get_segments_by_ids(&ids).unwrap();
        assert!(retained.iter().all(|segment| segment.speaker_id.as_deref() == Some("speaker-a")));
        let database = runtime.lock().unwrap();
        let history = history.lock().unwrap();
        assert!(!history.can_undo(), "a rolled-back assignment must not create history");
        drop(history);
        database.connection().execute("DROP TRIGGER test_fail_second_speaker", []).unwrap();
    }

    #[test]
    fn batch_speaker_assignment_refuses_missing_or_duplicate_ids_before_mutation() {
        let (_directory, store, runtime, history) = store_with_segments();
        assert!(matches!(
            store.assign_speaker_batch_v1(&["one".into(), "missing".into()], Some("speaker-z")),
            Err(SpeakerAssignmentError::Stale)
        ));
        assert!(matches!(
            store.assign_speaker_batch_v1(&["one".into(), "one".into()], Some("speaker-z")),
            Err(SpeakerAssignmentError::Invalid)
        ));
        assert_eq!(
            runtime.open_read().unwrap().get_segment_by_id("one").unwrap().unwrap().speaker_id.as_deref(),
            Some("speaker-a")
        );
        let database = runtime.lock().unwrap();
        let history = history.lock().unwrap();
        assert!(!history.can_undo());
        assert!(database.get_segment_by_id("missing").unwrap().is_none());
    }

    #[test]
    fn stale_batch_speaker_undo_rolls_back_earlier_inverse_rows_and_stays_retryable() {
        let (_directory, store, runtime, history) = store_with_segments();
        let ids = ["one".to_string(), "three".to_string()];
        let (_, admission) = store.assign_speaker_batch_v1(&ids, Some("speaker-z")).unwrap();
        drop(admission);
        runtime.lock().unwrap().update_speaker_id("three", Some("later-owner-edit")).unwrap();

        {
            let database = runtime.lock().unwrap();
            let history = history.lock().unwrap();
            assert!(history.undo(&database).is_err());
            assert!(history.can_undo(), "a failed atomic undo must remain available for recovery");
        }
        let retained = runtime.open_read().unwrap().get_segments_by_ids(&ids).unwrap();
        assert_eq!(
            retained.iter().find(|segment| segment.id == "one").unwrap().speaker_id.as_deref(),
            Some("speaker-z"),
            "the first inverse must roll back when a later row is stale"
        );
        assert_eq!(
            retained.iter().find(|segment| segment.id == "three").unwrap().speaker_id.as_deref(),
            Some("later-owner-edit")
        );
    }

    #[test]
    fn duplicate_batch_ids_are_refused_before_delete_or_history_mutation() {
        let (_directory, store, runtime, history) = store_with_segments();
        let error = match store.delete_batch(&["one".into(), "one".into()]) {
            Err(error) => error,
            Ok(_) => panic!("duplicate ids must fail before deletion"),
        };
        assert!(matches!(error, SegmentDeleteError::Invalid));
        assert!(runtime.open_read().unwrap().get_segment_by_id("one").unwrap().is_some());
        let database = runtime.lock().unwrap();
        let history = history.lock().unwrap();
        assert!(history.undo(&database).unwrap().is_none());
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
