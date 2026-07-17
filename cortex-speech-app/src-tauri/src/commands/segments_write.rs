//! Segment mutation IPC commands — slice 9 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use
//! segments_write::*;`), so `lib.rs`'s invoke_handler still names `commands::update_segment` and the
//! frontend invokes are untouched. Same functions, only relocated; the re-export also keeps them
//! callable by bare name from commands.rs.
//!
//! These are fast single-row/batch DB writes (edit, delete, speaker rename, human decision, verdict,
//! bounds) — they run on the caller thread (no run_blocking) exactly as before, since a single indexed
//! write is not a UI-freeze risk.

use super::{apply_curation_fields, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::db::SpeechSegment;
use crate::history::{Command, HistoryManager};
use crate::validation::input as validate;
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn update_segment(segment: SpeechSegment, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("update_segment")?;
    validate::validate_identifier(&segment.id)?;
    if let Some(ref aj) = segment.alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    let db = state.lock_db();
    let path_changed = match db.get_segment_by_id(&segment.id) {
        Ok(Some(existing)) => existing.audio_path != segment.audio_path,
        Ok(None) => true,
        Err(e) => return Err(e.to_string()),
    };
    drop(db);
    if path_changed {
        validate::validate_file_path(&segment.audio_path)?;
    }
    validate::validate_text(&segment.raw_transcript, 100000, "Raw transcript")?;
    if let Some(ref t) = segment.normalized_transcript {
        validate::validate_text(t, 100000, "Normalized transcript")?;
    }
    if let Some(ref t) = segment.annotated_transcript {
        validate::validate_text(t, 100000, "Annotated transcript")?;
    }
    if let Some(ref s) = segment.speaker_id {
        if !s.is_empty() {
            validate::validate_text(s, 256, "Speaker ID")?;
        }
    }
    let db = state.lock_db();
    let history = state.lock_history();
    HistoryManager::persist_segment_update(&db, &history, &segment).map_err(|e| e.to_string())?;
    drop(history);
    drop(db);

    state.session_auto_save();
    Ok(())
}

/// Lossless snapshot restore — the review-undo IPC. `update_segment` deliberately omits the
/// jury/decision columns (anti-clobber for ordinary edits), so ReviewMode's undo — clear decision +
/// re-upsert the pre-save snapshot — silently NULLed a PRIOR human_decision when undoing a
/// REdecision (reproduced in db::tests::redecision_undo_...; observed live 2026-07-14 as the owner's
/// morning review vanishing). This command writes the WHOLE snapshot through the same lossless path
/// the delete-undo uses (`restore_segment` / `insert_segment_full`), so an undo returns the row to
/// its exact pre-decision state — decision, verdict fields, escalation, gold flag and all.
#[tauri::command]
pub fn restore_segment_snapshot(segment: SpeechSegment, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("restore_segment_snapshot")?;
    validate::validate_identifier(&segment.id)?;
    validate::validate_text(&segment.raw_transcript, 100000, "Raw transcript")?;
    if let Some(ref t) = segment.annotated_transcript {
        validate::validate_text(t, 100000, "Annotated transcript")?;
    }
    if let Some(ref aj) = segment.alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    let db = state.lock_db();
    // Restore only over an EXISTING row: this is an undo of an edit/decision, never a resurrection
    // path (delete-undo has its own history command with its own guards).
    if db.get_segment_by_id(&segment.id).map_err(|e| e.to_string())?.is_none() {
        return Err(format!("Cannot restore snapshot of segment {}: it no longer exists", segment.id));
    }
    db.insert_segment_full(&segment).map_err(|e| e.to_string())?;
    drop(db);
    state.session_auto_save();
    Ok(())
}

/// F10 root fix — the partial-update IPC the debounced curation autosave calls instead of
/// whole-row `update_segment`.
///
/// The old path merged the user's field edits into the FRONTEND STORE row and upserted the whole row;
/// during a minutes-long batch the store is stale (it reloads only on batch completion), so that
/// upsert could silently revert concurrently-written columns (verify stamps, alignment quality,
/// confidence...). Here the FRESH row is read from the DB and the whitelisted fields applied under the
/// SAME held lock with no await in between (this command is sync), then persisted through the same
/// history path as `update_segment` — undo/redo still works, and nothing else in the row can be
/// clobbered by construction. A row deleted mid-debounce returns `Ok(false)` (no-op) rather than being
/// resurrected by the upsert.
#[tauri::command]
pub fn update_segment_fields(
    segment_id: String,
    fields: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    STRICT_RATE_LIMITER.check("update_segment_fields")?;
    validate::validate_identifier(&segment_id)?;
    let obj = fields.as_object().ok_or("update_segment_fields: fields must be a JSON object")?;
    if obj.is_empty() {
        return Ok(false); // nothing to apply
    }

    let db = state.lock_db();
    let Some(mut segment) = db.get_segment_by_id(&segment_id).map_err(|e| e.to_string())? else {
        return Ok(false); // deleted mid-debounce -> no-op, never resurrect
    };
    apply_curation_fields(&mut segment, obj)?;
    let history = state.lock_history();
    HistoryManager::persist_segment_update(&db, &history, &segment).map_err(|e| e.to_string())?;
    drop(history);
    drop(db);

    state.session_auto_save();
    Ok(true)
}

#[tauri::command]
pub fn delete_segment(id: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("delete_segment")?;
    validate::validate_identifier(&id)?;
    let db = state.lock_db();
    let previous = db.get_segment_by_id(&id).map_err(|e| e.to_string())?;
    db.delete_segment(&id).map_err(|e| e.to_string())?;
    drop(db);

    if let Some(seg) = previous {
        let history = state.lock_history();
        history.push(Command::DeleteSegments { segments: vec![seg] });
    }

    state.session_auto_save();
    Ok(())
}

#[tauri::command]
pub fn delete_segments_batch(ids: Vec<String>, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("delete_segments_batch")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    let db = state.lock_db();
    // Single batch-SELECT instead of N individual get_segment_by_id calls (O(1) SQL round trip).
    let segments = db.get_segments_by_ids(&ids).map_err(|e| e.to_string())?;
    db.delete_segments_batch(&ids).map_err(|e| e.to_string())?;
    drop(db);

    if !segments.is_empty() {
        let history = state.lock_history();
        history.push(Command::DeleteSegments { segments });
    }

    state.session_auto_save();
    Ok(())
}

#[tauri::command]
pub fn rename_speaker(old_id: String, new_id: String, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("rename_speaker")?;
    validate::validate_identifier(&new_id)?;
    let db = state.lock_db();
    db.rename_speaker(&old_id, &new_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn record_human_decision(
    state: State<'_, AppState>,
    segment_id: String,
    decision: String,
    corrected_transcript: Option<String>,
    timestamp_ms: Option<i64>,
) -> Result<(), String> {
    RATE_LIMITER.check("record_human_decision")?;
    // Round-22 #4: validate the id and bound the free text, matching every other write command.
    validate::validate_identifier(&segment_id)?;
    if let Some(t) = corrected_transcript.as_deref() {
        validate::validate_text(t, 100_000, "Corrected transcript")?;
    }
    let db = state.lock_db();
    db.record_human_decision(&segment_id, &decision, corrected_transcript.as_deref(), timestamp_ms)
        .map_err(|e| e.to_string())?;

    // M2.6: Update session with current review segment for cursor persistence on restart.
    let mut session = state.lock_session();
    session.set_current_segment(&segment_id);
    let _ = session.save(&db);

    Ok(())
}

/// P3-3: Revert a segment back to unreviewed state (NULL human_decision).
/// This is the correct undo operation — avoids incorrectly re-setting to 'accept'.
#[tauri::command]
pub fn clear_human_decision(state: State<'_, AppState>, segment_id: String) -> Result<(), String> {
    RATE_LIMITER.check("clear_human_decision")?;
    validate::validate_identifier(&segment_id)?; // round-22 #4
    let db = state.lock_db();
    db.clear_human_decision(&segment_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn write_segment_verdict(
    state: State<'_, AppState>,
    segment_id: String,
    verdict: String,
    transcript: Option<String>,
    rationale: Option<String>,
    evidence_json: Option<String>,
    agent_confidence: Option<f64>,
    escalated: bool,
) -> Result<(), String> {
    RATE_LIMITER.check("write_segment_verdict")?;
    // Round-22 #4: validate the id and bound every free-text field, matching the other write commands.
    validate::validate_identifier(&segment_id)?;
    if let Some(t) = transcript.as_deref() {
        validate::validate_text(t, 100_000, "Verdict transcript")?;
    }
    if let Some(r) = rationale.as_deref() {
        validate::validate_text(r, 100_000, "Verdict rationale")?;
    }
    // evidence_json is always serialized JSON; validate_alignment_json both confirms it parses as JSON
    // and bounds it (max 500KB), which is stricter and more apt than a plain length cap.
    if let Some(ej) = evidence_json.as_deref() {
        validate::validate_alignment_json(ej)?;
    }
    let db = state.lock_db();
    db.write_segment_verdict(
        &segment_id,
        &verdict,
        transcript.as_deref(),
        rationale.as_deref(),
        evidence_json.as_deref(),
        agent_confidence,
        escalated,
    )
    .map_err(|e| e.to_string())
}

/// `update_segment_bounds` — updates the start and end timestamps (in milliseconds)
/// of a speech segment in the database, adjusting duration and alignment metadata.
#[tauri::command]
pub fn update_segment_bounds(id: String, start_ms: i64, end_ms: i64, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("update_segment_bounds")?;
    validate::validate_identifier(&id)?;

    // Upper cap at u32::MAX ms (~49.7 days) matches the export/diarization slicer's offset guard
    // (chunking.rs) — a bound beyond that is garbage the slicer would reject downstream anyway, so
    // reject it at the IPC boundary instead of storing an absurd duration/offset from the webview.
    if start_ms < 0 || end_ms < 0 || start_ms >= end_ms || end_ms > u32::MAX as i64 {
        return Err("Invalid segment bounds".to_string());
    }

    let db = state.lock_db();
    let mut segment =
        db.get_segment_by_id(&id).map_err(|e| e.to_string())?.ok_or_else(|| format!("Segment not found: {id}"))?;

    let mut meta = if let Some(ref alignment_str) = segment.alignment_json {
        crate::chunking::SegmentSourceMeta::from_alignment_json(alignment_str).unwrap_or(
            crate::chunking::SegmentSourceMeta {
                source_start_ms: start_ms,
                source_end_ms: end_ms,
                chunk_index: 0,
                chunk_count: 1,
            },
        )
    } else {
        crate::chunking::SegmentSourceMeta {
            source_start_ms: start_ms,
            source_end_ms: end_ms,
            chunk_index: 0,
            chunk_count: 1,
        }
    };

    meta.source_start_ms = start_ms;
    meta.source_end_ms = end_ms;

    segment.alignment_json = Some(meta.to_alignment_json());
    segment.duration_ms = end_ms - start_ms;

    let history = state.lock_history();
    crate::history::HistoryManager::persist_segment_update(&db, &history, &segment).map_err(|e| e.to_string())?;
    drop(history);
    drop(db);

    state.session_auto_save();
    Ok(())
}
