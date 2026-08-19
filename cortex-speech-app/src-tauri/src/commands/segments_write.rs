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

/// Declare rights for one source RECORDING — every segment cut from it (migration v49, audit #6).
///
/// The unit of consent is the recording, so this takes an audio path rather than a segment id, and
/// returns how many segments it actually covered.
///
/// Until a recording is declared its clips are rights-UNKNOWN, which today still EXPORTS: the gates
/// enforce withdrawal everywhere, and `RecordingRights::permits_redistribution` is available (and
/// tested) for a caller that genuinely publishes, but wiring it as a hard refusal would block an
/// entire undeclared library the moment the migration lands. That is an owner decision, deliberately
/// not smuggled in with a schema change.
#[tauri::command]
pub fn set_recording_rights(
    audio_path: String,
    rights: crate::db::RecordingRights,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("set_recording_rights")?;
    let validated = validate::validate_file_path(&audio_path)?;
    for (name, value) in [
        ("Licence", &rights.license),
        ("Consent basis", &rights.consent_basis),
        ("Permitted use", &rights.permitted_use),
        ("Attribution", &rights.attribution),
        ("Provenance", &rights.source),
    ] {
        if let Some(v) = value {
            validate::validate_text(v, 2000, name)?;
        }
    }
    // `revoked_at` is deliberately NOT settable here: withdrawal has its own command, so a rights
    // edit can never quietly un-revoke a recording by omitting the field.
    let db = state.lock_db();
    db.set_recording_rights(&validated, &rights).map_err(|e| e.to_string())
}

/// Record a withdrawal of consent for a recording. Irreversible from this API by design.
///
/// Once stamped, every export path drops these clips — the local JSON/JSONL/CSV/Parquet tables as
/// well as the redistribution ones. A withdrawal that only blocked publishing would not be one.
#[tauri::command]
pub fn revoke_recording_consent(audio_path: String, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("revoke_recording_consent")?;
    let validated = validate::validate_file_path(&audio_path)?;
    let db = state.lock_db();
    let n = db.revoke_recording(&validated).map_err(|e| e.to_string())?;
    tracing::warn!("consent withdrawn for {validated}: {n} segment(s) excluded from every export");
    Ok(n)
}

/// Every distinct source recording, its clip count, and its declared rights.
#[tauri::command]
pub fn list_recording_rights(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    RATE_LIMITER.check("list_recording_rights")?;
    let db = state.lock_db();
    let rows = db.list_recording_rights().map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(path, count, rights)| {
            serde_json::json!({
                "audioPath": path,
                "segmentCount": count,
                "disposition": format!("{:?}", rights.disposition()),
                "rights": rights,
            })
        })
        .collect())
}

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
    // P1.1: the sibling update_segment validates a changed audio_path; this whole-row restore must too,
    // or a compromised renderer could repoint an existing row's audio_path to a UNC share (NTLM leak on
    // the next decode/exists). Syntactic UNC guard — no canonicalize, since the snapshot's path is
    // written back as-is and may point at audio that was legitimately moved since it was captured.
    validate::reject_unc_path(&segment.audio_path)?;
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

/// Record that a reviewer actually HEARD a clip, so a verdict on it can be more than a guess.
///
/// The renderer reports how much MEDIA time it advanced; the backend derives coverage and stamps the
/// policy version. It is not a trust boundary on its own — a scripted client can post whatever it
/// likes — which is exactly why `require_playback_evidence` binds the receipt to the segment, the
/// revision AND the audio fingerprint: a fabricated receipt still has to name the bytes on file for
/// the clip being decided, at the revision being decided.
#[tauri::command]
// A receipt is a wide but flat record — segment, revision, fingerprint, timings, who and when.
// Collapsing it into a struct would only move the same fields behind one name, and Tauri would
// still take them as a flat payload from the renderer.
#[allow(clippy::too_many_arguments)]
pub fn record_playback_receipt(
    state: State<'_, AppState>,
    segment_id: String,
    segment_revision: i64,
    audio_fingerprint: String,
    played_ms: i64,
    clip_duration_ms: i64,
    reviewer: Option<String>,
    session_id: Option<String>,
    started_at_ms: i64,
) -> Result<(), String> {
    RATE_LIMITER.check("record_playback_receipt")?;
    validate::validate_identifier(&segment_id)?;
    validate::validate_text(&audio_fingerprint, 256, "Audio fingerprint")?;
    if let Some(name) = reviewer.as_deref() {
        validate::validate_text(name, 128, "Reviewer")?;
    }
    if played_ms < 0 || clip_duration_ms < 0 || segment_revision < 0 {
        return Err("playback receipt fields must not be negative".to_string());
    }
    let db = state.lock_db();
    db.record_playback_receipt(&crate::db::PlaybackReceipt {
        segment_id,
        segment_revision,
        audio_fingerprint,
        reviewer,
        session_id,
        started_at_ms,
        played_ms,
        clip_duration_ms,
    })
    .map_err(|e| e.to_string())
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
    agreement_score: Option<f64>,
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
    // Evidence is arbitrary structured JSON, not alignment metadata; only parse and size-bound it.
    if let Some(ej) = evidence_json.as_deref() {
        validate::validate_json_blob(ej)?;
    }
    let db = state.lock_db();
    db.write_segment_verdict(
        &segment_id,
        &verdict,
        transcript.as_deref(),
        rationale.as_deref(),
        evidence_json.as_deref(),
        agreement_score,
        escalated,
    )
    .map_err(|e| e.to_string())
}
