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
use crate::history::HistoryManager;
use crate::ipc_contract::{
    CommandErrorV1, CommitReviewRequestV1, CommittedReviewV1, ReviewDecisionV1, ReviewDraftV1, SuggestedActionV1,
};
use crate::stores::PlaybackObservation;
use crate::validation::input as validate;
use crate::AppState;
use tauri::State;

const UNBOUND_REVIEW_FIELD_MUTATION_DISABLED: &str =
    "generic review-owned field mutation is disabled at schema v60; use the evidence-bound review decision/flag flow";

const WHOLE_ROW_SEGMENT_WRITE_RETIRED: &str =
    "the whole-row segment writer is retired; use update_segment_fields or the review decision/flag flow";

fn schema_uses_effect_bound_human_truth(db: &crate::db::Database) -> Result<bool, String> {
    crate::migrations::get_current_version(db).map(|version| version >= 60).map_err(|error| error.to_string())
}

/// RETIRED (deep audit 2026-08-25) — the legacy whole-row segment write.
///
/// It had no callers left: curation autosave moved to `update_segment_fields` and human truth moved to
/// the evidence-bound decision/flag flow. What it still carried was every retired write hazard on one
/// endpoint — a BLANK `raw_transcript` overwriting a good champion draft, renderer-supplied STALE
/// machine fields, and resurrection of a row deleted mid-edit through `insert_segment`'s ON CONFLICT
/// upsert. Hardening a caller-less endpoint only preserves the surface, so the write is gone; the
/// refusal stays here (rather than inline in the command) so a test can prove it without a Tauri
/// `State`. Same shape as `restore_segment_snapshot` above it.
#[allow(dead_code)]
fn persist_whole_segment_update_on(
    db: &crate::db::Database,
    history: &HistoryManager,
    segment: &SpeechSegment,
) -> Result<(), String> {
    let _ = (db, history, segment);
    Err(WHOLE_ROW_SEGMENT_WRITE_RETIRED.into())
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
    state.rights_store().declare_recording(&validated, &rights).map_err(|error| error.to_string())
}

/// Record a withdrawal of consent for a recording. Irreversible from this API by design.
///
/// Once stamped, every export path drops these clips — the local JSON/JSONL/CSV/Parquet tables as
/// well as the redistribution ones. A withdrawal that only blocked publishing would not be one.
#[tauri::command]
pub fn revoke_recording_consent(audio_path: String, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("revoke_recording_consent")?;
    let validated = validate::validate_file_path(&audio_path)?;
    let n = state.rights_store().revoke_recording(&validated).map_err(|error| error.to_string())?;
    tracing::warn!("consent withdrawn for {validated}: {n} segment(s) excluded from every export");
    Ok(n)
}

/// Every distinct source recording, its clip count, and its declared rights.
#[tauri::command]
pub fn list_recording_rights(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    RATE_LIMITER.check("list_recording_rights")?;
    let rows = state.rights_store().list_recordings().map_err(|error| error.to_string())?;
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

/// RETIRED. The IPC name stays registered so `lib.rs`'s invoke_handler keeps compiling and any stray
/// caller gets a legible refusal instead of a silent whole-row write — see
/// `persist_whole_segment_update_on` above for what this endpoint used to be able to do.
#[tauri::command]
pub fn update_segment(segment: SpeechSegment, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("update_segment")?;
    let _ = (segment, state);
    Err(WHOLE_ROW_SEGMENT_WRITE_RETIRED.into())
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
    let _mutation = state.segment_writes().delete_one(&id).map_err(|error| error.to_string())?;
    state.session_auto_save();
    Ok(())
}

#[tauri::command]
pub fn delete_segments_batch(ids: Vec<String>, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("delete_segments_batch")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    let _mutation = state.segment_writes().delete_batch(&ids).map_err(|error| error.to_string())?;
    state.session_auto_save();
    Ok(())
}

#[tauri::command]
pub fn rename_speaker(old_id: String, new_id: String, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("rename_speaker")?;
    validate::validate_identifier(&new_id)?;
    state.segment_writes().rename_speaker(&old_id, &new_id).map_err(|error| error.to_string())
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
    let commit = record_human_decision_on(
        &state.review_writes(),
        &segment_id,
        &decision,
        corrected_transcript.as_deref(),
        timestamp_ms,
        &operation_id,
    )?;

    state.persist_review_cursor(&segment_id);
    Ok(commit)
}

fn public_review_error(error: &str, operation_id: &str) -> CommandErrorV1 {
    if error.contains("E_NO_PLAYBACK_EVIDENCE") {
        CommandErrorV1::new("NO_PLAYBACK_EVIDENCE", "Listen to this clip before committing a decision.", true)
            .operation(operation_id)
            .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("E_PLAYBACK_EVIDENCE_CHANGED") {
        CommandErrorV1::new(
            "PLAYBACK_EVIDENCE_CHANGED",
            "The clip changed while it was being saved. Reload it and listen again.",
            true,
        )
        .operation(operation_id)
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("operation UUID was already used") {
        CommandErrorV1::new(
            "OPERATION_ID_CONFLICT",
            "This save identity is already bound to a different review request.",
            false,
        )
        .operation(operation_id)
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.to_ascii_lowercase().contains("database is locked")
        || error.to_ascii_lowercase().contains("database is busy")
    {
        CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry this save.", true)
            .operation(operation_id)
            .suggested(SuggestedActionV1::Retry)
    } else {
        CommandErrorV1::new("REVIEW_COMMIT_FAILED", "The decision was not committed. Your draft is unchanged.", false)
            .operation(operation_id)
            .suggested(SuggestedActionV1::OpenHealth)
    }
}

fn public_draft_error(error: &str, action: &str) -> CommandErrorV1 {
    if error.contains("E_STALE_REVIEW_DRAFT") {
        CommandErrorV1::new(
            "STALE_DRAFT_REVISION",
            "The clip changed while this draft was being saved. Reload it before continuing.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("E_REVIEW_DRAFT_SEGMENT_NOT_FOUND") {
        CommandErrorV1::new("SEGMENT_NOT_FOUND", "This clip no longer exists.", false)
            .suggested(SuggestedActionV1::ReloadClip)
    } else if error.to_ascii_lowercase().contains("database is locked")
        || error.to_ascii_lowercase().contains("database is busy")
    {
        CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry this draft action.", true)
            .suggested(SuggestedActionV1::Retry)
    } else {
        let message = match action {
            "loaded" => "The review draft could not be loaded.",
            "saved" => "The review draft could not be saved.",
            "deleted" => "The review draft could not be deleted.",
            _ => "The review draft operation failed.",
        };
        CommandErrorV1::new("REVIEW_DRAFT_FAILED", message, false).suggested(SuggestedActionV1::OpenHealth)
    }
}

fn review_draft_v1(record: crate::stores::ReviewDraftRecord) -> ReviewDraftV1 {
    ReviewDraftV1 {
        segment_id: record.segment_id,
        base_revision: record.base_revision,
        text: record.text,
        updated_at: record.updated_at,
    }
}

/// Load the non-authoritative desktop draft for one clip. Draft text never participates in review
/// truth, exports, evaluation, readiness, compensation, or serving queries.
#[tauri::command]
#[specta::specta]
pub fn get_review_draft_v1(
    state: State<'_, AppState>,
    segment_id: String,
) -> Result<Option<ReviewDraftV1>, CommandErrorV1> {
    RATE_LIMITER.check("get_review_draft_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many draft reads. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    validate::validate_identifier(&segment_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "The clip identity is invalid.", false))?;
    state
        .review_drafts()
        .get(&segment_id)
        .map(|draft| draft.map(review_draft_v1))
        .map_err(|error| public_draft_error(&error.to_string(), "loaded"))
}

/// Durably replace one clip's desktop draft. The server owns the timestamp; the renderer supplies
/// the exact review revision so a later decision cannot erase a draft for a newer clip state.
#[tauri::command]
#[specta::specta]
pub fn save_review_draft_v1(
    state: State<'_, AppState>,
    segment_id: String,
    base_revision: i64,
    text: String,
) -> Result<ReviewDraftV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("save_review_draft_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many draft saves. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    validate::validate_identifier(&segment_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "The clip identity is invalid.", false))?;
    if base_revision < 0 {
        return Err(CommandErrorV1::new("INVALID_REVIEW_REVISION", "The clip revision must be non-negative.", false));
    }
    validate::validate_text(&text, 100_000, "Review draft")
        .map_err(|_| CommandErrorV1::new("INVALID_REVIEW_DRAFT", "The draft is invalid or too long.", false))?;
    state
        .review_drafts()
        .save(&segment_id, base_revision, &text)
        .map(review_draft_v1)
        .map_err(|error| public_draft_error(&error.to_string(), "saved"))
}

/// Delete only a draft bound to the supplied review revision. A stale renderer cannot erase work
/// saved against a newer server state.
#[tauri::command]
#[specta::specta]
pub fn delete_review_draft_v1(
    state: State<'_, AppState>,
    segment_id: String,
    base_revision: i64,
) -> Result<bool, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("delete_review_draft_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many draft deletes. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    validate::validate_identifier(&segment_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "The clip identity is invalid.", false))?;
    if base_revision < 0 {
        return Err(CommandErrorV1::new("INVALID_REVIEW_REVISION", "The clip revision must be non-negative.", false));
    }
    state
        .review_drafts()
        .delete_if_revision(&segment_id, base_revision)
        .map_err(|error| public_draft_error(&error.to_string(), "deleted"))
}

fn committed_review_v1(commit: crate::db::HumanDecisionCommit) -> CommittedReviewV1 {
    let authoritative_transcript = commit
        .segment
        .verdict_transcript
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| commit.segment.annotated_transcript.as_deref().filter(|text| !text.trim().is_empty()))
        .unwrap_or(&commit.segment.raw_transcript)
        .to_string();
    CommittedReviewV1 {
        segment_id: commit.segment_id,
        committed_revision: commit.decided_revision,
        authoritative_transcript,
        decision_id: format!("effect:{}", commit.effect_event_id),
    }
}

fn commit_review_v1_on(
    store: &crate::stores::ReviewWriteStore,
    request: &CommitReviewRequestV1,
) -> Result<CommittedReviewV1, CommandErrorV1> {
    let invalid =
        |message: &str| CommandErrorV1::new("INVALID_REVIEW_REQUEST", message, false).operation(&request.operation_id);
    validate::validate_identifier(&request.segment_id).map_err(|_| invalid("The clip identity is invalid."))?;
    validate::validate_identifier(&request.operation_id).map_err(|_| invalid("The operation identity is invalid."))?;
    if request.base_revision < 0 {
        return Err(invalid("The clip revision must be non-negative."));
    }
    if let Some(transcript) = request.transcript.as_deref() {
        validate::validate_text(transcript, 100_000, "Transcript")
            .map_err(|_| invalid("The transcript is invalid or too long."))?;
    }
    if request.reason_code.is_some() {
        return Err(CommandErrorV1::new(
            "REASON_CODE_NOT_SUPPORTED",
            "This release cannot yet persist a structured unusable-audio reason.",
            false,
        )
        .operation(&request.operation_id));
    }
    if request.playback_receipt_id.is_some() {
        return Err(CommandErrorV1::new(
            "PLAYBACK_RECEIPT_ID_NOT_SUPPORTED",
            "This release verifies the current server-owned playback proof instead of accepting a receipt identity.",
            false,
        )
        .operation(&request.operation_id)
        .suggested(SuggestedActionV1::ReloadClip));
    }

    let (decision, transcript) = match request.decision {
        ReviewDecisionV1::Accept => ("accept", request.transcript.as_deref()),
        ReviewDecisionV1::Edit => {
            let transcript = request
                .transcript
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| invalid("A correction must contain a non-blank transcript."))?;
            ("edit", Some(transcript))
        }
        ReviewDecisionV1::Reject => {
            if request.transcript.as_deref().is_some_and(|text| !text.trim().is_empty()) {
                return Err(invalid("A rejection cannot silently discard a submitted transcript."));
            }
            ("reject", None)
        }
        ReviewDecisionV1::Skip => {
            return Err(CommandErrorV1::new(
                "SKIP_NOT_A_COMMIT",
                "Skip changes navigation only and cannot create review truth.",
                false,
            )
            .operation(&request.operation_id));
        }
    };

    let commit = store
        .commit_typed_decision(&request.segment_id, request.base_revision, decision, transcript, &request.operation_id)
        .map_err(|error| match error {
            crate::stores::ReviewCommitError::SegmentNotFound => {
                CommandErrorV1::new("SEGMENT_NOT_FOUND", "This clip no longer exists.", false)
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
            }
            crate::stores::ReviewCommitError::StaleRevision { current_revision } => {
                CommandErrorV1::new("STALE_REVISION", "This clip changed; reload it before saving.", false)
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
                    .detail("expectedRevision", request.base_revision)
                    .detail("currentRevision", current_revision)
            }
            crate::stores::ReviewCommitError::Backend(source) => {
                public_review_error(&source.to_string(), &request.operation_id)
            }
        })?;
    Ok(committed_review_v1(commit))
}

/// Versioned, compare-and-swap desktop review boundary. Legacy callers remain registered while the
/// renderer migrates one review domain at a time.
#[tauri::command]
#[specta::specta]
pub fn commit_review_v1(
    state: State<'_, AppState>,
    request: CommitReviewRequestV1,
) -> Result<CommittedReviewV1, CommandErrorV1> {
    RATE_LIMITER.check("commit_review_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many review saves. Retry in a moment.", true)
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::Retry)
    })?;
    let commit = commit_review_v1_on(&state.review_writes(), &request)?;
    state.persist_review_cursor(&request.segment_id);
    Ok(commit)
}

fn record_human_decision_on(
    store: &crate::stores::ReviewWriteStore,
    segment_id: &str,
    decision: &str,
    corrected_transcript: Option<&str>,
    timestamp_ms: Option<i64>,
    operation_id: &str,
) -> Result<crate::db::HumanDecisionCommit, String> {
    store
        .commit_legacy_decision(segment_id, decision, corrected_transcript, timestamp_ms, operation_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn undo_human_decision(
    state: State<'_, AppState>,
    effect_event_id: i64,
    operation_id: String,
) -> Result<crate::db::HumanDecisionUndoOutcome, String> {
    STRICT_RATE_LIMITER.check("undo_human_decision")?;
    state.review_writes().undo_human_decision(effect_event_id, None, &operation_id).map_err(|error| error.to_string())
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
    state.review_writes().record_flag(&segment_id, &rationale, &operation_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn undo_review_flag(
    state: State<'_, AppState>,
    effect_event_id: i64,
    operation_id: String,
) -> Result<crate::db::HumanFlagUndoOutcome, String> {
    STRICT_RATE_LIMITER.check("undo_review_flag")?;
    state.review_writes().undo_flag(effect_event_id, &operation_id).map_err(|error| error.to_string())
}

/// Bound BOTH renderer-supplied identity strings on a playback receipt. `session_id` is stored on the
/// receipt exactly like `reviewer` is, but was the one string in this module that arrived unbounded —
/// every other write command bounds its free text. Extracted so the gate can prove the bound without a
/// Tauri `State`.
fn validate_playback_receipt_identity(reviewer: Option<&str>, session_id: Option<&str>) -> Result<(), String> {
    if let Some(name) = reviewer {
        validate::validate_text(name, 128, "Reviewer")?;
    }
    if let Some(session) = session_id {
        validate::validate_text(session, 128, "Session")?;
    }
    Ok(())
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
    validate_playback_receipt_identity(reviewer.as_deref(), session_id.as_deref())?;
    if played_ms < 0 || clip_duration_ms < 0 {
        return Err("playback receipt durations must not be negative".to_string());
    }
    state
        .playback_writes()
        .record_observation(PlaybackObservation {
            segment_id,
            reviewer,
            session_id,
            started_at_ms,
            played_ms,
            claimed_clip_duration_ms: clip_duration_ms,
        })
        .map_err(|e| e.to_string())
}

/// P3-3: Revert a segment back to unreviewed state (NULL human_decision).
/// This is the correct undo operation — avoids incorrectly re-setting to 'accept'.
#[tauri::command]
pub fn clear_human_decision(state: State<'_, AppState>, segment_id: String) -> Result<(), String> {
    RATE_LIMITER.check("clear_human_decision")?;
    validate::validate_identifier(&segment_id)?; // round-22 #4
    state.review_writes().clear_legacy_decision(&segment_id).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        commit_review_v1_on, persist_segment_fields_on, persist_whole_segment_update_on, record_human_decision_on,
        validate_playback_receipt_identity,
    };
    use crate::database_runtime::DatabaseRuntime;
    use crate::db::{Database, PlaybackReceipt, SpeechSegment};
    use crate::history::HistoryManager;
    use crate::ipc_contract::{CommitReviewRequestV1, ReviewDecisionV1};
    use crate::stores::{require_listened, ReviewWriteStore};

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

    fn review_store(database: &Database) -> ReviewWriteStore {
        let writer = Database::open(database.path()).expect("open independent serialized review writer");
        ReviewWriteStore::new(DatabaseRuntime::new(writer))
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

        // The whole-row writer is RETIRED (deep audit 2026-08-25), so it now refuses EVERY request —
        // including one that only touches an allowed curation field, which it used to commit.
        let mut whole_curation_only = after_mixed.clone();
        whole_curation_only.speaker_id = Some("must-not-commit".into());
        let error = persist_whole_segment_update_on(&db, &history, &whole_curation_only).unwrap_err();
        assert!(error.contains("retired"), "{error}");
        assert_eq!(
            db.get_segment_by_id("curation-v60").unwrap().unwrap().speaker_id.as_deref(),
            Some("speaker-a"),
            "a retired writer must not commit even an allowed field"
        );

        let mut whole_verified = db.get_segment_by_id("curation-v60").unwrap().unwrap();
        whole_verified.verified = true;
        whole_verified.speaker_id = Some("must-not-commit".into());
        let error = persist_whole_segment_update_on(&db, &history, &whole_verified).unwrap_err();
        assert!(error.contains("retired"), "{error}");
        let after_whole_verified = db.get_segment_by_id("curation-v60").unwrap().unwrap();
        assert!(!after_whole_verified.verified);
        assert_eq!(after_whole_verified.speaker_id.as_deref(), Some("speaker-a"));

        let mut whole_annotated = after_whole_verified.clone();
        whole_annotated.annotated_transcript = Some("unbound annotation".into());
        let error = persist_whole_segment_update_on(&db, &history, &whole_annotated).unwrap_err();
        assert!(error.contains("retired"), "{error}");
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
            assert!(error.contains("retired"), "{field}: {error}");
            let retained = db.get_segment_by_id("curation-v60").unwrap().unwrap();
            assert_eq!(retained.speaker_id.as_deref(), Some("speaker-a"), "{field} mixed refusal must be atomic");
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
        assert!(error.contains("retired"), "{error}");
        assert!(db.get_segment_by_id("curation-new-unbound").unwrap().is_none());
    }

    /// The three hazards the caller-less whole-row IPC still carried, proved dead on a real row.
    ///
    /// It could blank a good champion draft (the recurring "blank transcript overwrites good" class),
    /// write renderer-supplied STALE machine fields over fresher ones, and RESURRECT a row deleted
    /// mid-edit through `insert_segment`'s ON CONFLICT upsert. Retirement kills all three at once, so
    /// this pins that none of them can write anything.
    #[test]
    fn the_retired_whole_row_writer_cannot_blank_a_draft_write_stale_machine_text_or_resurrect_a_row() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "retired-whole-row");
        let history = HistoryManager::new(20);
        let row = db.get_segment_by_id("retired-whole-row").unwrap().unwrap();

        let mut blanked = row.clone();
        blanked.raw_transcript = String::new();
        assert!(persist_whole_segment_update_on(&db, &history, &blanked).is_err());
        assert_eq!(
            db.get_segment_by_id("retired-whole-row").unwrap().unwrap().raw_transcript,
            row.raw_transcript,
            "a blank draft must never reach the row"
        );

        let mut stale = row.clone();
        stale.raw_transcript = "stale renderer copy".into();
        stale.normalized_transcript = Some("stale normalized".into());
        assert!(persist_whole_segment_update_on(&db, &history, &stale).is_err());
        assert_eq!(db.get_segment_by_id("retired-whole-row").unwrap().unwrap().raw_transcript, row.raw_transcript);

        db.delete_segment("retired-whole-row").unwrap();
        assert!(persist_whole_segment_update_on(&db, &history, &row).is_err());
        assert!(
            db.get_segment_by_id("retired-whole-row").unwrap().is_none(),
            "a deleted row must not be resurrected by a whole-row upsert"
        );
    }

    #[test]
    fn a_playback_receipt_bounds_both_renderer_supplied_identity_strings() {
        // `session_id` is persisted on the receipt exactly like `reviewer`, but arrived unbounded —
        // the one free-text field in this module that skipped the command's own bounding convention.
        assert!(validate_playback_receipt_identity(Some("Sara"), Some("session-1")).is_ok());
        assert!(validate_playback_receipt_identity(None, None).is_ok());
        assert!(validate_playback_receipt_identity(Some(&"r".repeat(129)), None).is_err());
        let long_session = "s".repeat(129);
        let error = validate_playback_receipt_identity(None, Some(&long_session)).expect_err("must be bounded");
        assert!(error.contains("Session"), "{error}");
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
        assert!(refused.to_string().contains("E_NO_PLAYBACK_EVIDENCE"), "the reason must be legible: {refused}");

        receipt(&db, "d1", 3_000); // 30% of a 10s clip
        let refused = require_listened(&db, "d1").expect_err("a third of a clip is not a listen");
        assert!(refused.to_string().contains("E_NO_PLAYBACK_EVIDENCE"), "{refused}");

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
        let store = review_store(&db);
        let first = record_human_decision_on(
            &store,
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
            &store,
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
    fn typed_review_commit_is_revision_bound_idempotent_and_returns_server_truth() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "typed-review");
        receipt(&db, "typed-review", 9_000);
        let base_revision = db.segment_review_revision("typed-review").unwrap().unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'unfinished', datetime('now'))",
                rusqlite::params!["typed-review", base_revision],
            )
            .unwrap();
        let request = CommitReviewRequestV1 {
            operation_id: "44444444-4444-4444-8444-444444444444".into(),
            segment_id: "typed-review".into(),
            base_revision,
            decision: ReviewDecisionV1::Accept,
            transcript: Some("دەق".into()),
            reason_code: None,
            playback_receipt_id: None,
        };
        let store = review_store(&db);
        let first = commit_review_v1_on(&store, &request).expect("typed commit");
        assert_eq!(first.segment_id, "typed-review");
        assert_eq!(first.authoritative_transcript, "دەق");
        assert!(first.decision_id.starts_with("effect:"));
        let draft_count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_drafts WHERE segment_id = 'typed-review'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(draft_count, 0, "the matching draft clears with the durable decision");

        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'response-loss copy', datetime('now'))",
                rusqlite::params!["typed-review", base_revision],
            )
            .unwrap();

        let replay = commit_review_v1_on(&store, &request).expect("lost-response replay");
        assert_eq!(replay, first, "an exact typed retry returns the original effect");
        let draft_count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_drafts WHERE segment_id = 'typed-review'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(draft_count, 0, "an exact replay clears only its old-revision draft");

        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'newer work', datetime('now'))",
                rusqlite::params!["typed-review", first.committed_revision],
            )
            .unwrap();
        let replay = commit_review_v1_on(&store, &request).expect("repeat lost-response replay");
        assert_eq!(replay, first);
        let retained_revision: i64 = db
            .connection()
            .query_row("SELECT base_revision FROM review_drafts WHERE segment_id = 'typed-review'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained_revision, first.committed_revision, "old replay must preserve newer work");

        let stale = CommitReviewRequestV1 { operation_id: "55555555-5555-4555-8555-555555555555".into(), ..request };
        let error = commit_review_v1_on(&store, &stale).expect_err("a new operation cannot reuse the old revision");
        assert_eq!(error.code, "STALE_REVISION");
        assert!(!error.retryable);
        assert_eq!(error.details.get("expectedRevision"), Some(&base_revision.into()));
    }

    #[test]
    fn typed_review_truth_rolls_back_if_matching_draft_cannot_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "draft-atomicity");
        receipt(&db, "draft-atomicity", 9_000);
        let base_revision = db.segment_review_revision("draft-atomicity").unwrap().unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'must remain', datetime('now'))",
                rusqlite::params!["draft-atomicity", base_revision],
            )
            .unwrap();
        db.connection()
            .execute_batch(
                "CREATE TRIGGER test_refuse_draft_clear BEFORE DELETE ON review_drafts
                 BEGIN SELECT RAISE(ABORT, 'injected draft clear failure'); END;",
            )
            .unwrap();
        let request = CommitReviewRequestV1 {
            operation_id: "66666666-6666-4666-8666-666666666666".into(),
            segment_id: "draft-atomicity".into(),
            base_revision,
            decision: ReviewDecisionV1::Accept,
            transcript: Some("دەق".into()),
            reason_code: None,
            playback_receipt_id: None,
        };
        let store = review_store(&db);
        let error = commit_review_v1_on(&store, &request).expect_err("draft-clear failure must abort review truth");
        assert_eq!(error.code, "REVIEW_COMMIT_FAILED");
        let row = db.get_segment_by_id("draft-atomicity").unwrap().unwrap();
        assert!(row.human_decision.is_none() && !row.verified, "human truth must roll back with draft clear");
        let draft_count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_drafts WHERE segment_id = 'draft-atomicity'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(draft_count, 1);
    }

    #[test]
    fn desktop_flag_command_replays_an_exact_committed_operation_after_response_loss() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "desktop-flag-replay");
        let operation_id = "33333333-3333-4333-8333-333333333333";
        let first = db.record_review_flag("desktop-flag-replay", "Needs a second listen", operation_id).unwrap();
        let replay = db
            .record_review_flag("desktop-flag-replay", "Needs a second listen", operation_id)
            .expect("an exact retry must return the original flag commit");
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(replay.flag_revision, first.flag_revision);

        let conflict = db
            .record_review_flag("desktop-flag-replay", "Different request", operation_id)
            .expect_err("one operation UUID cannot authorize a different flag request");
        assert!(conflict.to_string().contains("different request"), "{conflict}");
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
        assert!(refused.to_string().contains("E_NO_AUDIO_CONTENT_HASH"), "{refused}");

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
