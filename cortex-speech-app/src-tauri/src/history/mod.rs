use crate::db::{SpeakerAssignmentChange, SpeechSegment};
use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;

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
        /// Full snapshots immediately before and after the batch's durable ASR writes. Both endpoints
        /// are required for compare-and-set Undo and deterministic Redo.
        previous_segments: Vec<SpeechSegment>,
        current_segments: Vec<SpeechSegment>,
    },
    SpeakerAssignment {
        changes: Vec<SpeakerAssignmentChange>,
    },
}

/// Stable, locale-neutral identity of a reversible action. Human-readable copy belongs to the
/// renderer's typed i18n catalog; backend implementation strings must never cross IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryAction {
    UpdateSegment,
    DeleteSegments,
    BatchTranscribe,
    SpeakerAssignment,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    command: Command,
    estimated_bytes: usize,
}

impl Command {
    pub fn action(&self) -> HistoryAction {
        match self {
            Command::UpdateSegment { .. } => HistoryAction::UpdateSegment,
            Command::DeleteSegments { .. } => HistoryAction::DeleteSegments,
            Command::BatchTranscribe { .. } => HistoryAction::BatchTranscribe,
            Command::SpeakerAssignment { .. } => HistoryAction::SpeakerAssignment,
        }
    }
}

pub struct HistoryManager {
    undo_stack: Mutex<VecDeque<HistoryEntry>>,
    redo_stack: Mutex<VecDeque<HistoryEntry>>,
    max_history: usize,
    max_bytes: usize,
}

impl HistoryManager {
    pub fn new(max_history: usize) -> Self {
        Self::with_limits(max_history, MAX_HISTORY_BYTES)
    }

    fn with_limits(max_history: usize, max_bytes: usize) -> Self {
        Self {
            undo_stack: Mutex::new(VecDeque::with_capacity(max_history.min(256))),
            redo_stack: Mutex::new(VecDeque::new()),
            max_history,
            max_bytes,
        }
    }

    fn lock_undo_stack(&self) -> MutexGuard<'_, VecDeque<HistoryEntry>> {
        self.undo_stack.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned undo history stack");
            poisoned.into_inner()
        })
    }

    fn lock_redo_stack(&self) -> MutexGuard<'_, VecDeque<HistoryEntry>> {
        self.redo_stack.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned redo history stack");
            poisoned.into_inner()
        })
    }

    pub fn push(&self, cmd: Command) {
        {
            let mut stack = self.lock_undo_stack();
            // Serialized size is a conservative, schema-aware proxy for retained heap. Measure once
            // when the command enters history; large batch inverses must not multiply into gigabytes
            // merely because the count-based history limit is 500.
            let estimated_bytes = serde_json::to_vec(&cmd).map_or(usize::MAX, |bytes| bytes.len());
            stack.push_back(HistoryEntry { command: cmd, estimated_bytes });
            // VecDeque::pop_front is O(1); was O(N) with Vec::remove(0).
            let mut retained_bytes =
                stack.iter().fold(0usize, |total, entry| total.saturating_add(entry.estimated_bytes));
            while stack.len() > self.max_history || (stack.len() > 1 && retained_bytes > self.max_bytes) {
                if let Some(removed) = stack.pop_front() {
                    retained_bytes = retained_bytes.saturating_sub(removed.estimated_bytes);
                }
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
        let effect_bound_schema = crate::migrations::get_current_version(db)? >= 60;
        if let Some(previous) = db.get_segment_by_id(&segment.id)? {
            if effect_bound_schema {
                db.persist_machine_segment_snapshot(&previous, segment)?;
            } else {
                db.insert_segment(segment)?;
            }
            let current = db.get_segment_by_id(&segment.id)?.ok_or_else(|| {
                crate::error::AppError::Other(format!("segment {} disappeared after its update", segment.id))
            })?;
            history.record_segment_update(previous, current);
        } else if effect_bound_schema {
            db.insert_machine_segment_snapshot(segment)?;
        } else {
            db.insert_segment(segment)?;
        }
        Ok(())
    }

    pub fn undo(&self, db: &crate::db::Database) -> AppResult<Option<HistoryAction>> {
        let cmd = {
            let mut stack = self.lock_undo_stack();
            stack.pop_back()
        };
        match cmd {
            Some(entry) => {
                let action = entry.command.action();
                // Apply BEFORE moving the command to the redo stack, and on failure put it BACK on the
                // undo stack it came from — never drop it from BOTH stacks. Popping first and pushing only
                // on success means a failing apply (e.g. a DB error) would destroy the command and desync
                // the stacks, corrupting history and mis-ordering future undo/redo.
                match self.apply_undo(db, &entry.command) {
                    Ok(()) => {
                        self.lock_redo_stack().push_back(entry);
                        Ok(Some(action))
                    }
                    Err(e) => {
                        self.lock_undo_stack().push_back(entry);
                        Err(e)
                    }
                }
            }
            None => Ok(None),
        }
    }

    pub fn redo(&self, db: &crate::db::Database) -> AppResult<Option<HistoryAction>> {
        let cmd = {
            let mut stack = self.lock_redo_stack();
            stack.pop_back()
        };
        match cmd {
            Some(entry) => {
                let action = entry.command.action();
                // Same invariant as undo: only move the command to the undo stack if the redo actually
                // applied, and keep it on the redo stack if apply_redo fails. An unsupported redo
                // (Command::BatchTranscribe returns Err) would otherwise DESTROY the popped command,
                // leaving can_redo()=false and the DB stranded in the undone state with no recovery, and
                // corrupting the stacks. Re-pushing on failure preserves the entry so the user is never
                // silently stranded.
                match self.apply_redo(db, &entry.command) {
                    Ok(()) => {
                        self.lock_undo_stack().push_back(entry);
                        Ok(Some(action))
                    }
                    Err(e) => {
                        self.lock_redo_stack().push_back(entry);
                        Err(e)
                    }
                }
            }
            None => Ok(None),
        }
    }

    fn apply_undo(&self, db: &crate::db::Database, cmd: &Command) -> AppResult<()> {
        match cmd {
            Command::UpdateSegment { previous, current, .. } => {
                // The atomic compare-and-set refuses missing or stale rows and leaves the command on
                // the undo stack on failure; a no-op can never be reported as a successful Undo.
                db.apply_history_machine_snapshot_atomic(current, previous)?;
            }
            Command::DeleteSegments { segments } => {
                // Validate and restore every row inside one writer-held savepoint. A trigger, disk
                // error, stale id, or commit failure must restore zero rows rather than a prefix.
                db.apply_deleted_segments_history(segments, false)?;
            }
            Command::BatchTranscribe { previous_segments, current_segments } => {
                db.apply_batch_transcription_history(previous_segments, current_segments, false)?;
            }
            Command::SpeakerAssignment { changes } => {
                db.apply_speaker_assignment_history(changes, false)?;
            }
        }
        Ok(())
    }

    fn apply_redo(&self, db: &crate::db::Database, cmd: &Command) -> AppResult<()> {
        match cmd {
            Command::UpdateSegment { previous, current, .. } => {
                db.apply_history_machine_snapshot_atomic(previous, current)?;
            }
            Command::DeleteSegments { segments } => {
                // Redo is compare-and-set against the exact restored snapshots. If any row was
                // edited, removed, or gained authority after Undo, delete none of them.
                db.apply_deleted_segments_history(segments, true)?;
            }
            Command::BatchTranscribe { previous_segments, current_segments } => {
                db.apply_batch_transcription_history(previous_segments, current_segments, true)?;
            }
            Command::SpeakerAssignment { changes } => {
                db.apply_speaker_assignment_history(changes, true)?;
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

    pub fn undo_action(&self) -> Option<HistoryAction> {
        self.lock_undo_stack().back().map(|entry| entry.command.action())
    }

    pub fn redo_action(&self) -> Option<HistoryAction> {
        self.lock_redo_stack().back().map(|entry| entry.command.action())
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
            signal_anomaly_score: None,
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
    fn stale_machine_history_preserves_a_later_human_effect_on_undo_and_redo() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        let original = make_segment("history-after-review", "machine before");
        db.insert_segment(&original).unwrap();
        let updated = SpeechSegment {
            raw_transcript: "machine after".to_string(),
            speaker_id: Some("speaker-a".to_string()),
            ..original.clone()
        };
        db.insert_segment(&updated).unwrap();
        history.record_segment_update(original, updated);

        db.finalize_human_review("history-after-review", "accept", Some("machine after"), Some(10), None)
            .expect("server-owned decision effect");
        let reviewed = db.get_segment_by_id("history-after-review").unwrap().unwrap();
        let effect_count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id='history-after-review'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(effect_count, 1);

        history.undo(&db).expect("machine undo after review");
        let undone = db.get_segment_by_id("history-after-review").unwrap().unwrap();
        assert_eq!(undone.raw_transcript, "machine before");
        assert!(crate::db::review_owned_projection_matches(&undone, &reviewed));
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id='history-after-review'",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            1,
            "generic undo must not rewrite or duplicate the decision effect"
        );

        history.redo(&db).expect("machine redo after review");
        let redone = db.get_segment_by_id("history-after-review").unwrap().unwrap();
        assert_eq!(redone.raw_transcript, "machine after");
        assert_eq!(redone.speaker_id.as_deref(), Some("speaker-a"));
        assert!(crate::db::review_owned_projection_matches(&redone, &reviewed));
    }

    #[test]
    fn history_refuses_review_or_source_identity_endpoints_without_mutation() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        let original = make_segment("history-protected", "machine before");
        db.insert_segment(&original).unwrap();
        let machine_current = SpeechSegment { raw_transcript: "machine after".to_string(), ..original.clone() };
        db.insert_segment(&machine_current).unwrap();

        let mut forged_review_endpoint = machine_current.clone();
        forged_review_endpoint.annotated_transcript = Some("stale annotation".to_string());
        forged_review_endpoint.verified = true;
        history.record_segment_update(forged_review_endpoint, machine_current.clone());
        let review_error = history.undo(&db).unwrap_err();
        assert!(review_error.to_string().contains("review-owned truth"), "unexpected refusal: {review_error}");
        let retained = db.get_segment_by_id("history-protected").unwrap().unwrap();
        assert_eq!(retained.raw_transcript, "machine after");
        assert!(retained.annotated_transcript.is_none() && !retained.verified);

        history.clear();
        let mut different_source = original.clone();
        different_source.audio_path = "other-source.wav".to_string();
        history.record_segment_update(different_source, machine_current);
        let source_error = history.undo(&db).unwrap_err();
        assert!(source_error.to_string().contains("protected source identity"), "unexpected refusal: {source_error}");
        assert_eq!(db.get_segment_by_id("history-protected").unwrap().unwrap().raw_transcript, "machine after");
    }

    #[test]
    fn undo_of_an_update_fails_when_the_segment_was_deleted_rather_than_silently_succeeding() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        let original = make_segment("gone1", "hello world");
        db.insert_segment(&original).unwrap();
        let updated = SpeechSegment { raw_transcript: "hello universe".to_string(), ..original.clone() };
        db.insert_segment(&updated).unwrap();
        history.push(Command::UpdateSegment {
            segment_id: updated.id.clone(),
            previous: Box::new(original.clone()),
            current: Box::new(updated.clone()),
        });

        // A divergent/external path deletes the segment after the edit was recorded.
        db.delete_segment("gone1").unwrap();

        // Undo must FAIL honestly (not silently report success and push a no-op onto the redo stack), and
        // the failed command must stay on the undo stack so the user can retry — and must not resurrect.
        assert!(history.undo(&db).is_err(), "undo of an edit on a deleted segment must fail, not no-op-succeed");
        assert!(history.can_undo(), "a failed undo must leave the command on the undo stack");
        assert!(db.get_segment_by_id("gone1").unwrap().is_none(), "the failed undo must not resurrect the segment");
    }

    #[test]
    fn reviewed_gold_state_cannot_enter_delete_undo_history() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        // A reviewed, gold segment carrying jury/human state.
        let mut seg = make_segment("g1", "کوردستان");
        seg.verified = true;
        seg.verdict = Some("human_accept".to_string());
        seg.human_decision = Some("human_edit".to_string());
        seg.is_gold = true;
        seg.agreement_score = Some(0.9);
        seg.rationale = Some("reviewed".to_string());
        db.insert_legacy_segment_fixture(&seg).unwrap();

        let err = db.delete_segment("g1").expect_err("reviewed/gold authority must be append-only");
        assert!(err.to_string().contains("durable review authority"), "unexpected refusal: {err}");
        assert!(!history.can_undo(), "a refused delete must never create a fictitious history command");

        let retained = db.get_segment_by_id("g1").unwrap().unwrap();
        assert_eq!(retained.verdict.as_deref(), Some("human_accept"));
        assert_eq!(retained.human_decision.as_deref(), Some("human_edit"));
        assert!(retained.is_gold);
        assert_eq!(retained.agreement_score, Some(0.9));
        assert_eq!(retained.rationale.as_deref(), Some("reviewed"));
    }

    #[test]
    fn deleting_a_clip_must_not_erase_the_record_of_how_reviewers_scored_on_it() {
        // A spot-check row is not data ABOUT the clip, it is the record of what a REVIEWER did —
        // whether they listened or blind-accepted. `spot_checks` was created with ON DELETE CASCADE,
        // so deleting the clip silently deletes that record, and undo (which restores only the
        // segment row) cannot bring it back.
        //
        // Two consequences, both quiet. A reviewer's score changes retroactively when unrelated
        // clips are tidied up — a number that moves when you delete something else is not a record.
        // And an ordinary delete+undo, a supported operation this module goes to lengths to make
        // lossless, destroys it outright.
        //
        // `review_events` (the very next migration, for the same audit purpose) deliberately has NO
        // foreign key so the trail survives deletion of the audited row. This asserts spot_checks
        // holds the same line.
        let db = setup_db();
        let mut seg = make_segment("sc1", "دەقی هەڵە");
        seg.verified = true;
        seg.human_decision = Some("edit".to_string());
        seg.verdict = Some("human_edit".to_string());
        seg.verdict_transcript = Some("دەقی ڕاست".to_string());
        db.insert_legacy_segment_fixture(&seg).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?1,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}'
                  WHERE id = 'sc1'",
                [blake3::hash(b"history-spot-check-sc1").to_hex().to_string()],
            )
            .unwrap();
        db.record_spot_check("sc1", "Sara", "edit", "دەقی ڕاست", "دەقی ڕاست").unwrap();
        assert_eq!(db.spot_check_report().unwrap().len(), 1, "the score exists before the delete");

        let error = db.delete_segment("sc1").unwrap_err();
        assert!(
            error.to_string().contains("FOREIGN KEY") || error.to_string().contains("review"),
            "durable reviewer evidence must make deletion fail closed: {error}"
        );

        assert!(db.get_segment_by_id("sc1").unwrap().is_some(), "the refused deletion preserves the clip");
        let report = db.spot_check_report().unwrap();
        assert_eq!(
            report.len(),
            1,
            "a reviewer's spot-check record must survive deleting the clip it was measured on — \
             deleting data must never rewrite the history of who reviewed honestly"
        );
        assert_eq!(report[0].reviewer, "Sara");
        assert_eq!(report[0].checks, 1);
    }

    #[test]
    fn undoing_a_delete_brings_back_the_speaker_change_flag_with_the_clip() {
        // A restore runs as a FRESH insert (the row was physically removed), so every column
        // `insert_segment_full` omits silently reverts to its schema default. For
        // `speaker_change_score` that default is NULL — which the phone reads as "not measured" and
        // shows no badge for. So a delete+undo would quietly UN-FLAG a two-speaker clip and hand it
        // back to the queue looking like ordinary work, with no error anywhere.
        //
        // Measuring it again costs a full CAM++ pass over the whole library, and nothing would tell
        // the owner it was needed.
        let db = setup_db();
        let history = HistoryManager::new(100);
        let mut seg = make_segment("mx1", "دەق");
        // 0.4121: a clip the owner heard as turn-taking in the blind listening pass.
        seg.speaker_change_score = Some(0.4121);
        db.insert_legacy_segment_fixture(&seg).unwrap();

        let snapshot = db.get_segment_by_id("mx1").unwrap().unwrap();
        assert_eq!(snapshot.speaker_change_score, Some(0.4121), "the score is stored before the delete");
        db.delete_segment("mx1").unwrap();
        history.push(Command::DeleteSegments { segments: vec![snapshot] });
        history.undo(&db).unwrap();

        let restored = db.get_segment_by_id("mx1").unwrap().expect("the clip comes back");
        assert_eq!(
            restored.speaker_change_score,
            Some(0.4121),
            "restoring a clip must restore the measurement that flags it as holding two speakers"
        );
    }

    #[test]
    fn batch_transcribe_redo_reapplies_the_exact_recorded_endpoint() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        let prev = make_segment("b1", "old");
        db.insert_segment(&prev).unwrap();
        let previous = db.get_segment_by_id("b1").unwrap().unwrap();
        let mut updated = previous.clone();
        updated.raw_transcript = "champion draft".into();
        db.insert_segment(&updated).unwrap();
        let current = db.get_segment_by_id("b1").unwrap().unwrap();
        history.push(Command::BatchTranscribe { previous_segments: vec![previous], current_segments: vec![current] });
        history.undo(&db).unwrap(); // moves it to the redo stack
        assert!(history.can_redo());
        assert_eq!(history.redo(&db).unwrap(), Some(HistoryAction::BatchTranscribe));
        assert_eq!(db.get_segment_by_id("b1").unwrap().unwrap().raw_transcript, "champion draft");
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
    fn reviewed_jury_gold_and_created_at_survive_a_refused_delete() {
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
            agreement_score: Some(0.91),
            escalated: true,
            human_decision: Some("edit".to_string()),
            corrected_at: Some("2020-01-03 09:00:00".to_string()),
            is_gold: true,
            ..SpeechSegment::default()
        };
        // Persist the fully-provenanced row, then read it back as the snapshot the delete would capture.
        db.insert_legacy_segment_fixture(&seg).unwrap();
        let snapshot = db.get_segment_by_id("prov1").unwrap().unwrap();
        assert_eq!(snapshot.verdict.as_deref(), Some("human_edit"));
        assert_eq!(snapshot.created_at.as_deref(), Some("2020-01-02 03:04:05"));

        let err = db.delete_segment("prov1").expect_err("reviewed/gold authority must refuse deletion");
        assert!(err.to_string().contains("durable review authority"), "unexpected refusal: {err}");
        assert!(!history.can_undo(), "a refused delete must not add an undo entry");

        let retained = db.get_segment_by_id("prov1").unwrap().expect("authoritative row retained");
        assert_eq!(retained.verdict.as_deref(), Some("human_edit"), "verdict must survive refusal");
        assert_eq!(
            retained.verdict_transcript.as_deref(),
            Some("dîtina rast a mirov"),
            "human-corrected transcript must survive refusal"
        );
        assert_eq!(retained.human_decision.as_deref(), Some("edit"));
        assert_eq!(retained.corrected_at.as_deref(), Some("2020-01-03 09:00:00"));
        assert!(retained.is_gold);
        assert!(retained.escalated);
        assert_eq!(retained.agreement_score, Some(0.91));
        assert_eq!(retained.rationale.as_deref(), Some("human corrected the failed ASR"));
        assert_eq!(retained.evidence_json.as_deref(), Some("{\"src\":\"human\"}"));
        assert_eq!(
            retained.created_at.as_deref(),
            Some("2020-01-02 03:04:05"),
            "created_at must remain unchanged because it orders every export"
        );
    }

    #[test]
    fn failed_redo_keeps_the_command_on_the_redo_stack() {
        // A stale compare-and-set failure must keep the command so the user is never silently stranded.
        let db = setup_db();
        let history = HistoryManager::new(100);
        let seg = make_segment("bt1", "before");
        db.insert_segment(&seg).unwrap();
        let previous = db.get_segment_by_id("bt1").unwrap().unwrap();
        let mut updated = previous.clone();
        updated.raw_transcript = "batch endpoint".into();
        db.insert_segment(&updated).unwrap();
        let current = db.get_segment_by_id("bt1").unwrap().unwrap();
        history.push(Command::BatchTranscribe { previous_segments: vec![previous], current_segments: vec![current] });
        history.undo(&db).unwrap(); // moves the command onto the redo stack
        assert!(history.can_redo(), "redo is available after undo");

        let mut later = db.get_segment_by_id("bt1").unwrap().unwrap();
        later.raw_transcript = "later machine edit".into();
        db.insert_segment(&later).unwrap();
        assert!(history.redo(&db).is_err(), "stale batch Redo must fail closed");
        assert!(history.can_redo(), "a failed redo must NOT drop the command from the redo stack");
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
    fn history_memory_budget_evicts_old_batches_but_keeps_the_latest_action_recoverable() {
        let history = HistoryManager::with_limits(100, 1);
        history.push(Command::SpeakerAssignment {
            changes: vec![SpeakerAssignmentChange {
                segment_id: "older".into(),
                previous_speaker_id: Some("a".into()),
                current_speaker_id: Some("b".into()),
            }],
        });
        history.push(Command::SpeakerAssignment {
            changes: vec![SpeakerAssignmentChange {
                segment_id: "latest".into(),
                previous_speaker_id: Some("b".into()),
                current_speaker_id: Some("c".into()),
            }],
        });

        let stack = history.undo_stack.lock().unwrap();
        assert_eq!(stack.len(), 1, "the byte budget must evict the older retained batch");
        assert!(matches!(
            stack.back().map(|entry| &entry.command),
            Some(Command::SpeakerAssignment { changes }) if changes[0].segment_id == "latest"
        ));
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
        assert_eq!(history.undo_action(), Some(HistoryAction::DeleteSegments));
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
        assert_eq!(history.undo_action(), Some(HistoryAction::UpdateSegment));

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
        let updated = db.get_segment_by_id("bt1").unwrap().unwrap();

        // Record undo with full snapshot of original.
        history.push(Command::BatchTranscribe {
            previous_segments: vec![original.clone()],
            current_segments: vec![updated],
        });

        // Undo — should restore ALL fields.
        let desc = history.undo(&db).unwrap();
        assert!(desc.is_some());
        let restored = db.get_segment_by_id("bt1").unwrap().unwrap();
        assert_eq!(restored.raw_transcript, "old raw");
        assert_eq!(restored.normalized_transcript.as_deref(), Some("old normalized"));
        assert!((restored.confidence.unwrap() - 0.9).abs() < 1e-9, "confidence not fully restored");
    }

    #[test]
    fn multi_row_delete_undo_rolls_back_the_complete_restore_on_late_failure() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        for id in ["restore-a", "restore-b"] {
            db.insert_segment(&make_segment(id, id)).unwrap();
        }
        let ids = vec!["restore-a".to_string(), "restore-b".to_string()];
        let snapshots = db.get_segments_by_ids(&ids).unwrap();
        db.delete_segments_batch(&ids).unwrap();
        history.push(Command::DeleteSegments { segments: snapshots });
        db.connection()
            .execute_batch(
                "CREATE TRIGGER fail_second_history_restore
                 BEFORE INSERT ON speech_segments
                 WHEN NEW.id = 'restore-b'
                 BEGIN SELECT RAISE(ABORT, 'injected history restore failure'); END;",
            )
            .unwrap();

        assert!(history.undo(&db).is_err());
        assert!(db.get_segment_by_id("restore-a").unwrap().is_none());
        assert!(db.get_segment_by_id("restore-b").unwrap().is_none());
        assert!(history.can_undo(), "the failed atomic inverse must remain retryable");

        db.connection().execute_batch("DROP TRIGGER fail_second_history_restore;").unwrap();
        assert_eq!(history.undo(&db).unwrap(), Some(HistoryAction::DeleteSegments));
        assert!(db.get_segment_by_id("restore-a").unwrap().is_some());
        assert!(db.get_segment_by_id("restore-b").unwrap().is_some());
    }

    #[test]
    fn delete_redo_refuses_the_complete_batch_when_one_restored_row_changed() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        for id in ["redo-a", "redo-b"] {
            db.insert_segment(&make_segment(id, id)).unwrap();
        }
        let ids = vec!["redo-a".to_string(), "redo-b".to_string()];
        let snapshots = db.get_segments_by_ids(&ids).unwrap();
        db.delete_segments_batch(&ids).unwrap();
        history.push(Command::DeleteSegments { segments: snapshots });
        history.undo(&db).unwrap();
        assert!(db.update_speaker_id("redo-b", Some("later-human-label")).unwrap());

        assert!(history.redo(&db).is_err());
        assert!(db.get_segment_by_id("redo-a").unwrap().is_some());
        assert!(db.get_segment_by_id("redo-b").unwrap().is_some());
        assert!(history.can_redo(), "a stale redo must remain available after its honest refusal");
    }

    #[test]
    fn multi_row_batch_transcription_undo_rolls_back_on_late_failure() {
        let db = setup_db();
        let history = HistoryManager::new(100);
        for id in ["batch-a", "batch-b"] {
            db.insert_segment(&make_segment(id, &format!("old-{id}"))).unwrap();
        }
        let ids = vec!["batch-a".to_string(), "batch-b".to_string()];
        let previous = db.get_segments_by_ids(&ids).unwrap();
        for id in &ids {
            let mut updated = db.get_segment_by_id(id).unwrap().unwrap();
            updated.raw_transcript = format!("new-{id}");
            db.insert_segment(&updated).unwrap();
        }
        let current = db.get_segments_by_ids(&ids).unwrap();
        history.push(Command::BatchTranscribe { previous_segments: previous, current_segments: current });
        db.connection()
            .execute_batch(
                "CREATE TRIGGER fail_second_batch_history_restore
                 BEFORE UPDATE ON speech_segments
                 WHEN OLD.id = 'batch-b' AND NEW.raw_transcript = 'old-batch-b'
                 BEGIN SELECT RAISE(ABORT, 'injected batch history failure'); END;",
            )
            .unwrap();

        assert!(history.undo(&db).is_err());
        assert_eq!(db.get_segment_by_id("batch-a").unwrap().unwrap().raw_transcript, "new-batch-a");
        assert_eq!(db.get_segment_by_id("batch-b").unwrap().unwrap().raw_transcript, "new-batch-b");
        assert!(history.can_undo());

        db.connection().execute_batch("DROP TRIGGER fail_second_batch_history_restore;").unwrap();
        assert_eq!(history.undo(&db).unwrap(), Some(HistoryAction::BatchTranscribe));
        assert_eq!(db.get_segment_by_id("batch-a").unwrap().unwrap().raw_transcript, "old-batch-a");
        assert_eq!(db.get_segment_by_id("batch-b").unwrap().unwrap().raw_transcript, "old-batch-b");
    }
}
