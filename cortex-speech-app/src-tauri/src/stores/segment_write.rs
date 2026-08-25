//! Durable segment deletion, undo-history capture and speaker rename boundaries.

use crate::database_runtime::{begin_mutation, DatabaseRuntime, MutationGuard};
use crate::db::SpeechSegment;
use crate::error::{AppError, AppResult};
use crate::history::{Command, HistoryManager};
use crate::validation::input as validate;
use std::sync::{Arc, Mutex, MutexGuard};

const UNBOUND_REVIEW_FIELD_MUTATION_DISABLED: &str =
    "generic review-owned field mutation is disabled at schema v60; use the evidence-bound review decision/flag flow";

fn schema_uses_effect_bound_human_truth(database: &crate::db::Database) -> AppResult<bool> {
    crate::migrations::get_current_version(database).map(|version| version >= 60)
}

/// Apply only the fields owned by curation autosave. Unknown keys and wrong value types are loud
/// errors, and the caller persists only after the complete payload has validated.
fn apply_curation_fields(
    segment: &mut SpeechSegment,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    fn optional_string(key: &str, value: &serde_json::Value) -> Result<Option<String>, String> {
        if value.is_null() {
            Ok(None)
        } else {
            value.as_str().map(str::to_string).map(Some).ok_or_else(|| format!("{key} must be a string or null"))
        }
    }

    for (key, value) in fields {
        match key.as_str() {
            "annotatedTranscript" => {
                let value = optional_string(key, value)?;
                if let Some(ref transcript) = value {
                    validate::validate_text(transcript, 100_000, "Annotated transcript")?;
                }
                segment.annotated_transcript = value;
            }
            "speakerId" => {
                let value = optional_string(key, value)?;
                if let Some(ref speaker) = value {
                    if !speaker.is_empty() {
                        validate::validate_text(speaker, 256, "Speaker ID")?;
                    }
                }
                segment.speaker_id = value;
            }
            "alignmentJson" => {
                let value = optional_string(key, value)?;
                if let Some(ref alignment) = value {
                    validate::validate_alignment_json(alignment)?;
                }
                segment.alignment_json = value;
            }
            "verified" => {
                segment.verified = value.as_bool().ok_or_else(|| format!("{key} must be a boolean"))?;
            }
            other => {
                return Err(format!(
                    "update_segment_fields: unsupported field '{other}' — only curation fields \
                     (annotatedTranscript, speakerId, alignmentJson, verified) may be partially updated"
                ));
            }
        }
    }
    Ok(())
}

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

    pub(crate) fn update_fields(
        &self,
        segment_id: &str,
        fields: &serde_json::Map<String, serde_json::Value>,
    ) -> AppResult<(bool, SegmentMutation)> {
        let admission = begin_mutation().map_err(AppError::Other)?;
        let database = self.lock_database("update_segment_fields");
        if schema_uses_effect_bound_human_truth(&database)?
            && fields.keys().any(|key| matches!(key.as_str(), "verified" | "annotatedTranscript"))
        {
            return Err(AppError::Other(UNBOUND_REVIEW_FIELD_MUTATION_DISABLED.into()));
        }
        let Some(mut segment) = database.get_segment_by_id(segment_id)? else {
            return Ok((false, SegmentMutation { _admission: admission }));
        };
        apply_curation_fields(&mut segment, fields)?;
        let history = self.lock_history("update_segment_fields");
        HistoryManager::persist_segment_update(&database, &history, &segment)?;
        drop(history);
        drop(database);
        Ok((true, SegmentMutation { _admission: admission }))
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
    use crate::db::Database;

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
    fn apply_curation_fields_touches_only_whitelisted_fields_and_rejects_unknown_keys() {
        let mut segment = SpeechSegment {
            id: "curation".into(),
            audio_path: "curation.wav".into(),
            raw_transcript: "raw text".into(),
            verified: true,
            confidence: Some(0.42),
            ..SpeechSegment::default()
        };
        let before = segment.clone();
        let fields = serde_json::json!({ "annotatedTranscript": "دەق", "speakerId": "SPEAKER_01" });
        apply_curation_fields(&mut segment, fields.as_object().unwrap()).unwrap();
        assert_eq!(segment.annotated_transcript.as_deref(), Some("دەق"));
        assert_eq!(segment.speaker_id.as_deref(), Some("SPEAKER_01"));
        assert_eq!(segment.verified, before.verified);
        assert_eq!(segment.confidence, before.confidence);
        assert_eq!(segment.raw_transcript, before.raw_transcript);
        assert_eq!(segment.audio_path, before.audio_path);
        assert_eq!(segment.alignment_json, before.alignment_json);

        let clear = serde_json::json!({ "speakerId": null });
        apply_curation_fields(&mut segment, clear.as_object().unwrap()).unwrap();
        assert_eq!(segment.speaker_id, None);

        let verified = serde_json::json!({ "verified": false });
        apply_curation_fields(&mut segment, verified.as_object().unwrap()).unwrap();
        assert!(!segment.verified);
        assert_eq!(segment.raw_transcript, before.raw_transcript);
        let bad_verified = serde_json::json!({ "verified": "yes" });
        assert!(apply_curation_fields(&mut segment, bad_verified.as_object().unwrap()).is_err());

        let unsupported = serde_json::json!({ "confidence": 0.9 });
        let error = apply_curation_fields(&mut segment, unsupported.as_object().unwrap()).unwrap_err();
        assert!(error.contains("unsupported field 'confidence'"), "{error}");
        assert_eq!(segment.confidence, before.confidence);
        let wrong_type = serde_json::json!({ "annotatedTranscript": 7 });
        assert!(apply_curation_fields(&mut segment, wrong_type.as_object().unwrap()).is_err());
    }

    #[test]
    fn schema_v60_field_writer_refuses_unbound_review_fields_atomically() {
        let (_directory, store, runtime, _history) = store_with_segments();
        let allowed = serde_json::json!({
            "speakerId": "speaker-z",
            "alignmentJson": r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1}"#
        });
        let (changed, admission) = store.update_fields("one", allowed.as_object().unwrap()).unwrap();
        assert!(changed);
        drop(admission);
        assert_eq!(
            runtime.open_read().unwrap().get_segment_by_id("one").unwrap().unwrap().speaker_id.as_deref(),
            Some("speaker-z")
        );

        for restricted in [
            serde_json::json!({ "verified": true }),
            serde_json::json!({ "annotatedTranscript": "unbound human truth" }),
        ] {
            let error =
                store.update_fields("one", restricted.as_object().unwrap()).err().expect("restricted field refused");
            assert!(error.to_string().contains("disabled at schema v60"), "{error}");
        }

        let mixed = serde_json::json!({ "speakerId": "must-not-commit", "verified": true });
        let error = store.update_fields("one", mixed.as_object().unwrap()).err().expect("mixed field payload refused");
        assert!(error.to_string().contains("disabled at schema v60"), "{error}");
        let retained = runtime.open_read().unwrap().get_segment_by_id("one").unwrap().unwrap();
        assert_eq!(retained.speaker_id.as_deref(), Some("speaker-z"));
        assert!(!retained.verified && retained.annotated_transcript.is_none());
    }
}
