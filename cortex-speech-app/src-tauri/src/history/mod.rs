use crate::db::SpeechSegment;
use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

/// Represents a reversible change to the dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    UpdateSegment {
        segment_id: String,
        previous: Box<SpeechSegment>,
        current: Box<SpeechSegment>,
    },
    DeleteSegments {
        segments: Vec<SpeechSegment>,
    },
    BatchTranscribe {
        /// Full snapshots of each segment *before* transcription ran.
        /// Used to fully restore raw_transcript, normalized_transcript,
        /// annotated_transcript, and confidence on undo.
        previous_segments: Vec<SpeechSegment>,
    },
}

impl Command {
    pub fn description(&self) -> &str {
        match self {
            Command::UpdateSegment { .. } => "Update segment",
            Command::DeleteSegments { .. } => "Delete segments",
            Command::BatchTranscribe { .. } => "Batch transcribe",
        }
    }
}

pub struct HistoryManager {
    undo_stack: Mutex<VecDeque<Command>>,
    redo_stack: Mutex<VecDeque<Command>>,
    max_history: usize,
}

impl HistoryManager {
    pub fn new(max_history: usize) -> Self {
        Self {
            undo_stack: Mutex::new(VecDeque::with_capacity(max_history.min(256))),
            redo_stack: Mutex::new(VecDeque::new()),
            max_history,
        }
    }

    fn lock_undo_stack(&self) -> MutexGuard<'_, VecDeque<Command>> {
        self.undo_stack.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned undo history stack");
            poisoned.into_inner()
        })
    }

    fn lock_redo_stack(&self) -> MutexGuard<'_, VecDeque<Command>> {
        self.redo_stack.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned redo history stack");
            poisoned.into_inner()
        })
    }

    pub fn push(&self, cmd: Command) {
        {
            let mut stack = self.lock_undo_stack();
            stack.push_back(cmd);
            // VecDeque::pop_front is O(1); was O(N) with Vec::remove(0).
            while stack.len() > self.max_history {
                stack.pop_front();
            }
        }
        // Clear redo stack on new action.
        self.lock_redo_stack().clear();
    }

    /// Records an update to an existing segment for undo/redo.
    pub fn record_segment_update(&self, previous: SpeechSegment, current: SpeechSegment) {
        self.push(Command::UpdateSegment {
            segment_id: current.id.clone(),
            previous: Box::new(previous),
            current: Box::new(current),
        });
    }

    /// Persists a segment update and records history when updating an existing row.
    pub fn persist_segment_update(
        db: &crate::db::Database,
        history: &HistoryManager,
        segment: &SpeechSegment,
    ) -> AppResult<()> {
        if let Some(previous) = db.get_segment_by_id(&segment.id)? {
            db.insert_segment(segment)?;
            history.record_segment_update(previous, segment.clone());
        } else {
            db.insert_segment(segment)?;
        }
        Ok(())
    }

    pub fn undo(&self, db: &crate::db::Database) -> AppResult<Option<String>> {
        let cmd = {
            let mut stack = self.lock_undo_stack();
            stack.pop_back()
        };
        match cmd {
            Some(cmd) => {
                let description = cmd.description().to_string();
                self.apply_undo(db, &cmd)?;
                self.lock_redo_stack().push_back(cmd);
                Ok(Some(description))
            }
            None => Ok(None),
        }
    }

    pub fn redo(&self, db: &crate::db::Database) -> AppResult<Option<String>> {
        let cmd = {
            let mut stack = self.lock_redo_stack();
            stack.pop_back()
        };
        match cmd {
            Some(cmd) => {
                let description = cmd.description().to_string();
                self.apply_redo(db, &cmd)?;
                self.lock_undo_stack().push_back(cmd);
                Ok(Some(description))
            }
            None => Ok(None),
        }
    }

    fn apply_undo(&self, db: &crate::db::Database, cmd: &Command) -> AppResult<()> {
        match cmd {
            Command::UpdateSegment { previous, .. } => {
                if db.get_segment_by_id(&previous.id)?.is_some() {
                    db.insert_segment(previous)?;
                }
            }
            Command::DeleteSegments { segments } => {
                for seg in segments {
                    if db.get_segment_by_id(&seg.id)?.is_none() {
                        // The row was HARD-deleted, so this is a fresh INSERT — use the full-column
                        // restore so jury verdict / human decision / gold status / created_at survive.
                        // insert_segment would drop them (it only writes 17 columns), silently wiping
                        // curated provenance and re-stamping created_at to now.
                        db.insert_segment_full(seg)?;
                    }
                }
            }
            Command::BatchTranscribe { previous_segments } => {
                for prev in previous_segments {
                    // Restore the full pre-transcription state — not just raw_transcript.
                    // This ensures normalized_transcript, annotated_transcript, and
                    // confidence are also reverted, not silently left dirty.
                    if db.get_segment_by_id(&prev.id)?.is_some() {
                        db.insert_segment(prev)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_redo(&self, db: &crate::db::Database, cmd: &Command) -> AppResult<()> {
        match cmd {
            Command::UpdateSegment { current, .. } => {
                if db.get_segment_by_id(&current.id)?.is_some() {
                    db.insert_segment(current)?;
                }
            }
            Command::DeleteSegments { segments } => {
                for seg in segments {
                    if db.get_segment_by_id(&seg.id)?.is_some() {
                        db.delete_segment(&seg.id)?;
                    }
                }
            }
            Command::BatchTranscribe { .. } => {
                // BatchTranscribe redo is not supported — the original ASR result
                // would need to be stored in the command for deterministic replay.
                return Err(crate::error::AppError::Other("BatchTranscribe redo is not supported".into()));
            }
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.lock_undo_stack().is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.lock_redo_stack().is_empty()
    }

    pub fn clear(&self) {
        self.lock_undo_stack().clear();
        self.lock_redo_stack().clear();
    }

    pub fn undo_description(&self) -> Option<String> {
        self.lock_undo_stack().back().map(|c| c.description().to_string())
    }

    pub fn redo_description(&self) -> Option<String> {
        self.lock_redo_stack().back().map(|c| c.description().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db
    }

    fn make_segment(id: &str, text: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: format!("{id}.wav"),
            raw_transcript: text.to_string(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 1000,
            speaker_id: None,
            verified: false,
            confidence: None,
            ctc_score: None,
            clipping_ratio: None,
            rms_db: None,
            snr_db: None,
            split: None,
            ood_score: None,
            ..SpeechSegment::default()
        }
    }

    #[test]
    fn test_undo_redo_update() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        let original = make_segment("test1", "hello world");
        db.insert_segment(&original).unwrap();

        // Perform update in DB, then record command for undo
        let updated = SpeechSegment { raw_transcript: "hello universe".to_string(), ..original.clone() };
        db.insert_segment(&updated).unwrap();
        let cmd = Command::UpdateSegment {
            segment_id: updated.id.clone(),
            previous: Box::new(original.clone()),
            current: Box::new(updated.clone()),
        };
        history.push(cmd);

        // Verify updated state
        let current = db.get_segment_by_id("test1").unwrap().unwrap();
        assert_eq!(current.raw_transcript, "hello universe");

        // Undo
        let desc = history.undo(&db).unwrap();
        assert!(desc.is_some());
        let restored = db.get_segment_by_id("test1").unwrap().unwrap();
        assert_eq!(restored.raw_transcript, "hello world");

        // Redo
        let desc = history.redo(&db).unwrap();
        assert!(desc.is_some());
        let redone = db.get_segment_by_id("test1").unwrap().unwrap();
        assert_eq!(redone.raw_transcript, "hello universe");
    }

    #[test]
    fn test_undo_delete() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        let seg = make_segment("del1", "to delete");
        db.insert_segment(&seg).unwrap();

        let cmd = Command::DeleteSegments { segments: vec![seg.clone()] };

        // Execute delete
        db.delete_segment("del1").unwrap();
        assert!(db.get_segment_by_id("del1").unwrap().is_none());

        // Push and undo the deletion
        history.push(cmd);
        let desc = history.undo(&db).unwrap();
        assert!(desc.is_some());
        let restored = db.get_segment_by_id("del1").unwrap();
        assert!(restored.is_some());
        assert_eq!(restored.unwrap().raw_transcript, "to delete");
    }

    // Delete-then-undo must restore the FULL row, not just raw_transcript. A curator can run the jury
    // and/or a human review (verdict, verdict_transcript, human_decision, corrected_at), mark the clip
    // gold (is_gold), then delete and undo. Because delete is a hard DELETE, undo is a fresh INSERT —
    // insert_segment would drop every jury/gold/created_at column to its default, silently wiping the
    // curated decision and gold-anchor status. This pins insert_segment_full so the restore is lossless.
    #[test]
    fn test_undo_delete_preserves_jury_gold_and_created_at() {
        let db = setup_db();
        let history = HistoryManager::new(100);

        let seg = SpeechSegment {
            id: "prov1".to_string(),
            created_at: Some("2020-01-02 03:04:05".to_string()),
            audio_path: "prov1.wav".to_string(),
            raw_transcript: "[Pending WSL 7B ASR]".to_string(),
            normalized_transcript: Some("dîtina rast".to_string()),
            duration_ms: 1500,
            verified: true,
            verdict: Some("human_edit".to_string()),
            verdict_transcript: Some("dîtina rast a mirov".to_string()),
            rationale: Some("human corrected the failed ASR".to_string()),
            evidence_json: Some("{\"src\":\"human\"}".to_string()),
            agent_confidence: Some(0.91),
            escalated: true,
            human_decision: Some("edit".to_string()),
            corrected_at: Some("2020-01-03 09:00:00".to_string()),
            is_gold: true,
            ..SpeechSegment::default()
        };
        // Persist the fully-provenanced row, then read it back as the snapshot the delete would capture.
        db.insert_segment_full(&seg).unwrap();
        let snapshot = db.get_segment_by_id("prov1").unwrap().unwrap();
        assert_eq!(snapshot.verdict.as_deref(), Some("human_edit"));
        assert_eq!(snapshot.created_at.as_deref(), Some("2020-01-02 03:04:05"));

        // Hard-delete, then undo via the command holding the full snapshot.
        db.delete_segment("prov1").unwrap();
        assert!(db.get_segment_by_id("prov1").unwrap().is_none());
        history.push(Command::DeleteSegments { segments: vec![snapshot] });
        history.undo(&db).unwrap();

        let restored = db.get_segment_by_id("prov1").unwrap().expect("row restored");
        assert_eq!(restored.verdict.as_deref(), Some("human_edit"), "verdict must survive undo");
        assert_eq!(
            restored.verdict_transcript.as_deref(),
            Some("dîtina rast a mirov"),
            "human-corrected transcript must survive undo"
        );
        assert_eq!(restored.human_decision.as_deref(), Some("edit"), "human_decision must survive undo");
        assert_eq!(restored.corrected_at.as_deref(), Some("2020-01-03 09:00:00"));
        assert!(restored.is_gold, "gold-anchor status must survive undo");
        assert!(restored.escalated, "escalated flag must survive undo");
        assert_eq!(restored.agent_confidence, Some(0.91));
        assert_eq!(restored.rationale.as_deref(), Some("human corrected the failed ASR"));
        assert_eq!(restored.evidence_json.as_deref(), Some("{\"src\":\"human\"}"));
        assert_eq!(
            restored.created_at.as_deref(),
            Some("2020-01-02 03:04:05"),
            "created_at must be preserved, not re-stamped to now() (it orders every export)"
        );
    }

    #[test]
    fn test_max_history() {
        let history = HistoryManager::new(3);
        for i in 0..5 {
            history.push(Command::DeleteSegments { segments: vec![make_segment(&format!("seg{i}"), "")] });
        }
        // VecDeque evicts oldest (front) entries; should have exactly max_history items.
        assert_eq!(history.undo_stack.lock().unwrap().len(), 3);
    }

    #[test]
    fn history_operations_recover_poisoned_stacks() {
        let history = HistoryManager::new(3);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = history.undo_stack.lock().expect("lock undo stack");
            panic!("poison undo stack");
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = history.redo_stack.lock().expect("lock redo stack");
            panic!("poison redo stack");
        }));

        history.push(Command::DeleteSegments { segments: vec![make_segment("poisoned", "")] });

        assert!(history.can_undo());
        assert_eq!(history.undo_description(), Some("Delete segments".to_string()));
        history.clear();
        assert!(!history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn test_persist_segment_update_pushes_history() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        let original = make_segment("persist1", "before");
        db.insert_segment(&original).unwrap();
        assert!(!history.can_undo());

        let mut updated = original.clone();
        updated.raw_transcript = "after".to_string();
        HistoryManager::persist_segment_update(&db, &history, &updated).unwrap();

        assert!(history.can_undo());
        assert_eq!(history.undo_description(), Some("Update segment".to_string()));

        history.undo(&db).unwrap();
        let restored = db.get_segment_by_id("persist1").unwrap().unwrap();
        assert_eq!(restored.raw_transcript, "before");
    }

    #[test]
    fn test_persist_segment_update_skips_history_for_insert() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        let segment = make_segment("persist-new", "fresh");

        HistoryManager::persist_segment_update(&db, &history, &segment).unwrap();

        assert!(!history.can_undo());
        let stored = db.get_segment_by_id("persist-new").unwrap().unwrap();
        assert_eq!(stored.raw_transcript, "fresh");
    }

    #[test]
    fn test_batch_transcribe_undo_restores_all_fields() {
        // Verifies that undoing a batch transcription restores not just raw_transcript
        // but also normalized_transcript and confidence — the complete previous state.
        let db = setup_db();
        let history = HistoryManager::new(100);

        // Insert segment with pre-existing normalized transcript and confidence.
        let mut original = make_segment("bt1", "old raw");
        original.normalized_transcript = Some("old normalized".to_string());
        original.confidence = Some(0.9);
        db.insert_segment(&original).unwrap();

        // Simulate batch transcription: update all three fields.
        let mut updated = original.clone();
        updated.raw_transcript = "new raw".to_string();
        updated.normalized_transcript = Some("new normalized".to_string());
        updated.confidence = Some(0.5);
        db.insert_segment(&updated).unwrap();

        // Record undo with full snapshot of original.
        history.push(Command::BatchTranscribe { previous_segments: vec![original.clone()] });

        // Undo — should restore ALL fields.
        let desc = history.undo(&db).unwrap();
        assert!(desc.is_some());
        let restored = db.get_segment_by_id("bt1").unwrap().unwrap();
        assert_eq!(restored.raw_transcript, "old raw");
        assert_eq!(restored.normalized_transcript.as_deref(), Some("old normalized"));
        assert!((restored.confidence.unwrap() - 0.9).abs() < 1e-9, "confidence not fully restored");
    }
}
