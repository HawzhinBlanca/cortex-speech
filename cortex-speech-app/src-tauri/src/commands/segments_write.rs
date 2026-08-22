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

const UNBOUND_REVIEW_FIELD_MUTATION_DISABLED: &str =
    "generic review-owned field mutation is disabled at schema v60; use the evidence-bound review decision/flag flow";

fn schema_uses_effect_bound_human_truth(db: &crate::db::Database) -> Result<bool, String> {
    crate::migrations::get_current_version(db).map(|version| version >= 60).map_err(|error| error.to_string())
}

fn persist_whole_segment_update_on(
    db: &crate::db::Database,
    history: &HistoryManager,
    segment: &SpeechSegment,
) -> Result<(), String> {
    let existing = db.get_segment_by_id(&segment.id).map_err(|error| error.to_string())?;
    if schema_uses_effect_bound_human_truth(db)? {
        let mutates_review_truth = existing.as_ref().map_or_else(
            || !crate::db::review_owned_projection_matches(segment, &SpeechSegment::default()),
            |current| !crate::db::review_owned_projection_matches(current, segment),
        );
        if mutates_review_truth {
            return Err(UNBOUND_REVIEW_FIELD_MUTATION_DISABLED.into());
        }
    }
    HistoryManager::persist_segment_update(db, history, segment).map_err(|error| error.to_string())
}

fn persist_segment_fields_on(
    db: &crate::db::Database,
    history: &HistoryManager,
    segment_id: &str,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<bool, String> {
    if schema_uses_effect_bound_human_truth(db)?
        && fields.keys().any(|key| matches!(key.as_str(), "verified" | "annotatedTranscript"))
    {
        return Err(UNBOUND_REVIEW_FIELD_MUTATION_DISABLED.into());
    }
    let Some(mut segment) = db.get_segment_by_id(segment_id).map_err(|error| error.to_string())? else {
        return Ok(false);
    };
    apply_curation_fields(&mut segment, fields)?;
    HistoryManager::persist_segment_update(db, history, &segment).map_err(|error| error.to_string())?;
    Ok(true)
}

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
    let _mutation = super::begin_mutation()?;
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
    persist_whole_segment_update_on(&db, &history, &segment)?;
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
    let _ = (segment, state);
    Err("Renderer-owned whole-row restore is disabled; review undo requires an immutable server-owned effect id".into())
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
    let history = state.lock_history();
    let changed = persist_segment_fields_on(&db, &history, &segment_id, obj)?;
    drop(history);
    drop(db);

    if changed {
        state.session_auto_save();
    }
    Ok(changed)
}

#[tauri::command]
pub fn delete_segment(id: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("delete_segment")?;
    validate::validate_identifier(&id)?;
    let _mutation = super::begin_mutation()?;
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
    let _mutation = super::begin_mutation()?;
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

/// The desktop half of the listening bar, extracted so it can be tested without a Tauri `State`.
///
/// The desktop was the last way to write a verdict on a clip nobody heard: `ReviewMode` posted a
/// receipt on accept/edit but not on reject, `ReviewInbox` posted none at all, and the command never
/// asked for one — so the comment in ReviewMode claiming "the backend refuses a verdict without
/// sufficient evidence" described an intent that was never implemented. Both surfaces post now, and
/// this is the check that makes it mean something.
fn require_listened(db: &crate::db::Database, segment_id: &str) -> Result<crate::db::PlaybackDecisionProof, String> {
    let content_hash = db
        .segment_audio_content_hash(segment_id)
        .map_err(|error| format!("playback identity lookup failed: {error}"))?
        .ok_or_else(|| {
            format!("E_NO_AUDIO_CONTENT_HASH: segment {segment_id} has no server-derived audio content hash")
        })?;
    let revision = db
        .segment_review_revision(segment_id)
        .map_err(|error| format!("playback revision lookup failed: {error}"))?
        .unwrap_or(0);
    let (source_start_ms, source_end_ms) = db
        .segment_source_span(segment_id)
        .map_err(|error| format!("playback source-span lookup failed: {error}"))?
        .ok_or_else(|| format!("E_NO_AUDIO_SOURCE_SPAN: segment {segment_id} has no canonical server source span"))?;
    match db.has_sufficient_playback_evidence(segment_id, revision, &content_hash, None) {
        Ok(true) => Ok(crate::db::PlaybackDecisionProof {
            segment_revision: revision,
            audio_content_hash: content_hash,
            source_start_ms,
            source_end_ms,
        }),
        Ok(false) => {
            tracing::warn!(
                "PLAYBACK_EVIDENCE_V3_CONTENT_HASH_RAW_COUNTER_REFUSED: {segment_id} on the desktop at revision {revision}"
            );
            Err(db
                .require_playback_evidence(segment_id, revision, &content_hash, None)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "E_NO_PLAYBACK_EVIDENCE".to_string()))
        }
        // Not a verdict about the reviewer: an unwell database cannot answer the question, and
        // telling someone who listened that they did not is both false and unactionable.
        Err(e) => Err(format!("playback evidence check failed: {e}")),
    }
}

#[tauri::command]
pub fn record_human_decision(
    state: State<'_, AppState>,
    segment_id: String,
    decision: String,
    corrected_transcript: Option<String>,
    timestamp_ms: Option<i64>,
    operation_id: String,
) -> Result<crate::db::HumanDecisionCommit, String> {
    RATE_LIMITER.check("record_human_decision")?;
    // Round-22 #4: validate the id and bound the free text, matching every other write command.
    validate::validate_identifier(&segment_id)?;
    validate::validate_identifier(&operation_id)?;
    if let Some(t) = corrected_transcript.as_deref() {
        validate::validate_text(t, 100_000, "Corrected transcript")?;
    }
    let db = state.lock_db();
    let commit = record_human_decision_on(
        &db,
        &segment_id,
        &decision,
        corrected_transcript.as_deref(),
        timestamp_ms,
        &operation_id,
    )?;

    // M2.6: Update session with current review segment for cursor persistence on restart.
    let mut session = state.lock_session();
    session.set_current_segment(&segment_id);
    let _ = session.save(&db);

    Ok(commit)
}

fn record_human_decision_on(
    db: &crate::db::Database,
    segment_id: &str,
    decision: &str,
    corrected_transcript: Option<&str>,
    timestamp_ms: Option<i64>,
    operation_id: &str,
) -> Result<crate::db::HumanDecisionCommit, String> {
    // A successful first attempt advanced the revision, so its old playback receipt is correctly no
    // longer current. Resolve an exact lost-response replay by immutable operation identity before
    // asking for fresh evidence; the writer repeats this check under BEGIN IMMEDIATE for races.
    if let Some(commit) = db
        .replay_desktop_human_decision(segment_id, decision, corrected_transcript, timestamp_ms, operation_id)
        .map_err(|error| error.to_string())?
    {
        return Ok(commit);
    }

    let playback = require_listened(db, segment_id)?;

    // ONE commit: decision, transcript, attribution and `verified` together. Two writes left nine
    // rows decided-but-pending on the live library — ReviewMode's second `update_segment_fields`
    // covered it, ReviewInbox never made that call, so its decisions never reached the corpus.
    db.finalize_human_review_with_playback(
        segment_id,
        decision,
        corrected_transcript,
        timestamp_ms,
        &playback,
        operation_id,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn undo_human_decision(
    state: State<'_, AppState>,
    effect_event_id: i64,
    operation_id: String,
) -> Result<crate::db::HumanDecisionUndoOutcome, String> {
    STRICT_RATE_LIMITER.check("undo_human_decision")?;
    let db = state.lock_db();
    db.undo_human_decision(effect_event_id, None, &operation_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn record_review_flag(
    state: State<'_, AppState>,
    segment_id: String,
    rationale: String,
    operation_id: String,
) -> Result<crate::db::HumanFlagCommit, String> {
    RATE_LIMITER.check("record_review_flag")?;
    validate::validate_identifier(&segment_id)?;
    validate::validate_text(&rationale, 10_000, "Review flag rationale")?;
    let db = state.lock_db();
    record_review_flag_on(&db, &segment_id, &rationale, &operation_id)
}

fn record_review_flag_on(
    db: &crate::db::Database,
    segment_id: &str,
    rationale: &str,
    operation_id: &str,
) -> Result<crate::db::HumanFlagCommit, String> {
    db.record_review_flag(segment_id, rationale, operation_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn undo_review_flag(
    state: State<'_, AppState>,
    effect_event_id: i64,
    operation_id: String,
) -> Result<crate::db::HumanFlagUndoOutcome, String> {
    STRICT_RATE_LIMITER.check("undo_review_flag")?;
    let db = state.lock_db();
    db.undo_review_flag(effect_event_id, &operation_id).map_err(|error| error.to_string())
}

/// Record that a reviewer actually HEARD a clip, so a verdict on it can be more than a guess.
///
/// The renderer reports how much MEDIA time it advanced; the backend derives coverage and stamps the
/// policy version. It is not a trust boundary on its own — a scripted client can post whatever it
/// likes — which is exactly why `require_playback_evidence` binds the receipt to the segment, the
/// revision AND the decoded-PCM content hash: a fabricated receipt still has to name the bytes on file for
/// the clip being decided, at the revision being decided.
#[tauri::command]
// A receipt is a wide but flat record — segment, revision, content hash, timings, who and when.
// Collapsing it into a struct would only move the same fields behind one name, and Tauri would
// still take them as a flat payload from the renderer.
#[allow(clippy::too_many_arguments)]
pub fn record_playback_receipt(
    state: State<'_, AppState>,
    segment_id: String,
    played_ms: i64,
    clip_duration_ms: i64,
    reviewer: Option<String>,
    session_id: Option<String>,
    started_at_ms: i64,
) -> Result<(), String> {
    RATE_LIMITER.check("record_playback_receipt")?;
    validate::validate_identifier(&segment_id)?;
    if let Some(name) = reviewer.as_deref() {
        validate::validate_text(name, 128, "Reviewer")?;
    }
    if played_ms < 0 || clip_duration_ms < 0 {
        return Err("playback receipt durations must not be negative".to_string());
    }
    let db = state.lock_db();

    // The REVISION and CONTENT HASH are resolved here, from the row itself — never accepted from the
    // renderer. A client that could name them could mint a receipt for a revision it never heard, or
    // for audio that has since been replaced; then the guard would be comparing the client's claim
    // with the client's claim. Resolved server-side, the only thing a caller can assert is how much
    // time it played, and that assertion is still bound to the clip actually on file.
    let content_hash = db.segment_audio_content_hash(&segment_id).map_err(|e| e.to_string())?.ok_or_else(|| {
        format!("cannot mint playback evidence for segment {segment_id} without a server-derived audio content hash")
    })?;
    let revision = db.segment_review_revision(&segment_id).map_err(|e| e.to_string())?.unwrap_or(0);

    db.record_playback_receipt(&crate::db::PlaybackReceipt {
        segment_id,
        segment_revision: revision,
        audio_content_hash: content_hash,
        reviewer,
        session_id,
        started_at_ms,
        played_ms,
        clip_duration_ms,
        source_start_ms: None,
        source_end_ms: None,
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

#[cfg(test)]
mod tests {
    use super::{
        persist_segment_fields_on, persist_whole_segment_update_on, record_human_decision_on, record_review_flag_on,
        require_listened,
    };
    use crate::db::{Database, PlaybackReceipt, SpeechSegment};
    use crate::history::HistoryManager;

    fn db_with_clip(dir: &std::path::Path, id: &str) -> Database {
        let db = Database::open(dir.join("t.db").to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        db.insert_segment(&SpeechSegment {
            id: id.into(),
            audio_path: dir.join(format!("{id}.wav")).to_string_lossy().into_owned(),
            raw_transcript: "دەق".into(),
            duration_ms: 10_000,
            alignment_json: Some(
                r#"{"source_start_ms":0,"source_end_ms":10000,"chunk_index":0,"chunk_count":1}"#.into(),
            ),
            ..SpeechSegment::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash = ?2 WHERE id = ?1",
                rusqlite::params![id, "a".repeat(64)],
            )
            .unwrap();
        db
    }

    fn receipt(db: &Database, id: &str, played: i64) {
        let content_hash = db.segment_audio_content_hash(id).unwrap().expect("fixture has exact audio content hash");
        let revision = db.segment_review_revision(id).unwrap().unwrap_or(0);
        db.record_playback_receipt(&PlaybackReceipt {
            segment_id: id.into(),
            segment_revision: revision,
            audio_content_hash: content_hash,
            reviewer: None,
            session_id: None,
            started_at_ms: 0,
            played_ms: played,
            clip_duration_ms: 10_000,
            source_start_ms: None,
            source_end_ms: None,
        })
        .unwrap();
    }

    #[test]
    fn schema_v60_registered_segment_writers_refuse_unbound_review_fields_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "curation-v60");
        let history = HistoryManager::new(20);

        let allowed = serde_json::json!({
            "speakerId": "speaker-a",
            "alignmentJson": r#"{"source_start_ms":0,"source_end_ms":10000,"chunk_index":0,"chunk_count":1}"#
        });
        assert!(persist_segment_fields_on(&db, &history, "curation-v60", allowed.as_object().unwrap()).unwrap());
        let allowed_row = db.get_segment_by_id("curation-v60").unwrap().unwrap();
        assert_eq!(allowed_row.speaker_id.as_deref(), Some("speaker-a"));

        for restricted in [
            serde_json::json!({ "verified": true }),
            serde_json::json!({ "annotatedTranscript": "unbound human truth" }),
        ] {
            let error =
                persist_segment_fields_on(&db, &history, "curation-v60", restricted.as_object().unwrap()).unwrap_err();
            assert!(error.contains("disabled at schema v60"), "{error}");
        }

        let mixed = serde_json::json!({ "speakerId": "must-not-commit", "verified": true });
        let error = persist_segment_fields_on(&db, &history, "curation-v60", mixed.as_object().unwrap()).unwrap_err();
        assert!(error.contains("disabled at schema v60"), "{error}");
        let after_mixed = db.get_segment_by_id("curation-v60").unwrap().unwrap();
        assert_eq!(after_mixed.speaker_id.as_deref(), Some("speaker-a"), "mixed refusal must be atomic");
        assert!(!after_mixed.verified && after_mixed.annotated_transcript.is_none());

        let mut whole_allowed = after_mixed.clone();
        whole_allowed.speaker_id = Some("speaker-b".into());
        persist_whole_segment_update_on(&db, &history, &whole_allowed).unwrap();
        assert_eq!(db.get_segment_by_id("curation-v60").unwrap().unwrap().speaker_id.as_deref(), Some("speaker-b"));

        let mut whole_verified = db.get_segment_by_id("curation-v60").unwrap().unwrap();
        whole_verified.verified = true;
        whole_verified.speaker_id = Some("must-not-commit".into());
        let error = persist_whole_segment_update_on(&db, &history, &whole_verified).unwrap_err();
        assert!(error.contains("disabled at schema v60"), "{error}");
        let after_whole_verified = db.get_segment_by_id("curation-v60").unwrap().unwrap();
        assert!(!after_whole_verified.verified);
        assert_eq!(after_whole_verified.speaker_id.as_deref(), Some("speaker-b"));

        let mut whole_annotated = after_whole_verified.clone();
        whole_annotated.annotated_transcript = Some("unbound annotation".into());
        let error = persist_whole_segment_update_on(&db, &history, &whole_annotated).unwrap_err();
        assert!(error.contains("disabled at schema v60"), "{error}");
        assert!(db.get_segment_by_id("curation-v60").unwrap().unwrap().annotated_transcript.is_none());

        for (field, mutate) in [
            ("human_decision", 0_u8),
            ("verdict", 1),
            ("reviewed_by", 2),
            ("corrected_at", 3),
            ("escalated", 4),
            ("rationale", 5),
            ("is_gold", 6),
        ] {
            let mut forged = db.get_segment_by_id("curation-v60").unwrap().unwrap();
            forged.speaker_id = Some("must-not-commit".into());
            match mutate {
                0 => forged.human_decision = Some("accept".into()),
                1 => forged.verdict = Some("human_accept".into()),
                2 => forged.reviewed_by = Some("forged reviewer".into()),
                3 => forged.corrected_at = Some("2026-08-22 00:00:00".into()),
                4 => forged.escalated = true,
                5 => forged.rationale = Some("forged flag rationale".into()),
                6 => forged.is_gold = true,
                _ => unreachable!(),
            }
            let error = persist_whole_segment_update_on(&db, &history, &forged).unwrap_err();
            assert!(error.contains("disabled at schema v60"), "{field}: {error}");
            let retained = db.get_segment_by_id("curation-v60").unwrap().unwrap();
            assert_eq!(retained.speaker_id.as_deref(), Some("speaker-b"), "{field} mixed refusal must be atomic");
            assert!(retained.human_decision.is_none() && retained.verdict.is_none());
            assert!(retained.reviewed_by.is_none() && retained.corrected_at.is_none());
            assert!(!retained.escalated && retained.rationale.is_none() && !retained.is_gold);
        }

        let mut new_unbound = SpeechSegment {
            id: "curation-new-unbound".into(),
            audio_path: tmp.path().join("new.wav").to_string_lossy().into_owned(),
            raw_transcript: "machine".into(),
            duration_ms: 1_000,
            ..SpeechSegment::default()
        };
        new_unbound.human_decision = Some("accept".into());
        new_unbound.verdict = Some("human_accept".into());
        new_unbound.reviewed_by = Some("forged reviewer".into());
        let error = persist_whole_segment_update_on(&db, &history, &new_unbound).unwrap_err();
        assert!(error.contains("disabled at schema v60"), "{error}");
        assert!(db.get_segment_by_id("curation-new-unbound").unwrap().is_none());
    }

    /// The desktop must hold the same bar as the phone, or it is simply the easier way in.
    ///
    /// Until 2026-08-19 it had none: `ReviewInbox` posted no receipt at all and `ReviewMode`'s reject
    /// skipped the one its own accept path posted, while this command never asked. The corpus could
    /// be written from the desktop by anyone who never pressed play.
    #[test]
    fn the_desktop_refuses_a_verdict_on_a_clip_that_was_not_heard() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "d1");

        let refused = require_listened(&db, "d1").expect_err("no receipt at all must be refused");
        assert!(refused.contains("E_NO_PLAYBACK_EVIDENCE"), "the reason must be legible: {refused}");

        receipt(&db, "d1", 3_000); // 30% of a 10s clip
        let refused = require_listened(&db, "d1").expect_err("a third of a clip is not a listen");
        assert!(refused.contains("E_NO_PLAYBACK_EVIDENCE"), "{refused}");

        receipt(&db, "d1", 9_000); // 90%, clear of the 0.85 bar
        require_listened(&db, "d1").expect("a clip heard to the bar must be decidable");
    }

    #[test]
    fn desktop_decision_rechecks_the_exact_playback_proof_inside_its_write_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "desktop-race");
        receipt(&db, "desktop-race", 9_000);
        let proof = require_listened(&db, "desktop-race").expect("preflight sees valid playback");

        // Model the race window between command preflight and finalization: the served row moves to
        // different decoded audio in a trigger-disabled staged database. Production schema blocks
        // this mutation too; the transactional check remains defense in depth for corrupted input.
        db.connection().execute("DROP TRIGGER speech_segments_v60_paid_identity_immutable_update", []).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash = ?2 WHERE id = ?1",
                rusqlite::params!["desktop-race", "b".repeat(64)],
            )
            .unwrap();
        let error = db
            .finalize_human_review_with_playback(
                "desktop-race",
                "accept",
                None,
                Some(1_700_000_000_001),
                &proof,
                "11111111-1111-4111-8111-111111111111",
            )
            .expect_err("a stale desktop proof must not authorize the decision");
        assert!(error.to_string().contains("E_PLAYBACK_EVIDENCE_CHANGED"), "{error}");
        let row = db.get_segment_by_id("desktop-race").unwrap().unwrap();
        assert!(row.human_decision.is_none() && !row.verified);
    }

    #[test]
    fn desktop_command_replays_a_committed_operation_before_current_playback_preflight() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "desktop-command-replay");
        receipt(&db, "desktop-command-replay", 9_000);
        let operation_id = "22222222-2222-4222-8222-222222222222";
        let first = record_human_decision_on(
            &db,
            "desktop-command-replay",
            "accept",
            Some("دەق"),
            Some(1_700_000_000_001),
            operation_id,
        )
        .unwrap();

        let current_revision = db.segment_review_revision("desktop-command-replay").unwrap().unwrap();
        let content_hash = db.segment_audio_content_hash("desktop-command-replay").unwrap().unwrap();
        assert!(
            !db.has_sufficient_playback_evidence("desktop-command-replay", current_revision, &content_hash, None)
                .unwrap(),
            "the original receipt is correctly stale after the decision advanced the revision"
        );
        let replay = record_human_decision_on(
            &db,
            "desktop-command-replay",
            "accept",
            Some("دەق"),
            Some(1_700_000_000_001),
            operation_id,
        )
        .expect("a response-loss retry must resolve before require_listened sees the stale revision");
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(replay.decided_revision, first.decided_revision);
    }

    #[test]
    fn desktop_flag_command_replays_an_exact_committed_operation_after_response_loss() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "desktop-flag-replay");
        let operation_id = "33333333-3333-4333-8333-333333333333";
        let first = record_review_flag_on(&db, "desktop-flag-replay", "Needs a second listen", operation_id).unwrap();
        let replay = record_review_flag_on(&db, "desktop-flag-replay", "Needs a second listen", operation_id)
            .expect("an exact retry must return the original flag commit");
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(replay.flag_revision, first.flag_revision);

        let conflict = record_review_flag_on(&db, "desktop-flag-replay", "Different request", operation_id)
            .expect_err("one operation UUID cannot authorize a different flag request");
        assert!(conflict.contains("different request"), "{conflict}");
        let effect_count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id = ?1",
                rusqlite::params!["desktop-flag-replay"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(effect_count, 1, "response-loss retry must not create a second flag effect");
    }

    /// A clip the library has never seen cannot have been heard, and must not pass by defaulting.
    #[test]
    fn an_unknown_segment_is_refused_rather_than_waved_through() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "d2");
        assert!(require_listened(&db, "nonexistent").is_err(), "an unknown clip must not pass the guard");
    }

    #[test]
    fn a_segment_without_an_audio_content_hash_cannot_mint_or_authorize_playback() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "blank-fingerprint");
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash = NULL WHERE id = 'blank-fingerprint'", [])
            .unwrap();

        let refused =
            require_listened(&db, "blank-fingerprint").expect_err("an identifier fallback is not exact audio identity");
        assert!(refused.contains("E_NO_AUDIO_CONTENT_HASH"), "{refused}");

        let error = db
            .record_playback_receipt(&PlaybackReceipt {
                segment_id: "blank-fingerprint".into(),
                segment_revision: 0,
                audio_content_hash: "f".repeat(64),
                reviewer: None,
                session_id: None,
                started_at_ms: 0,
                played_ms: 10_000,
                clip_duration_ms: 10_000,
                source_start_ms: None,
                source_end_ms: None,
            })
            .expect_err("the canonical writer must not replace missing identity with a client claim");
        assert!(error.to_string().contains("server-derived audio content hash"));
    }
}
