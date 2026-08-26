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

use super::{RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::db::SpeechSegment;
use crate::history::HistoryManager;
use crate::ipc_contract::{
    CommandErrorV1, CommitReviewRequestV1, CommittedReviewV1, DesktopPlaybackReceiptV1, DesktopPlaybackSessionV1,
    MarkSegmentUnusableRequestV1, MarkedSegmentUnusableV1, PlaybackIntervalV1, ReviewDecisionV1, ReviewDraftV1,
    SuggestedActionV1,
};
use crate::validation::input as validate;
use crate::AppState;
use tauri::State;

const WHOLE_ROW_SEGMENT_WRITE_RETIRED: &str =
    "the whole-row segment writer is retired; use update_segment_fields or the review decision/flag flow";

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

    let (changed, _mutation) =
        state.segment_writes().update_fields(&segment_id, obj).map_err(|error| error.to_string())?;

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
    _state: State<'_, AppState>,
    segment_id: String,
    decision: String,
    corrected_transcript: Option<String>,
    timestamp_ms: Option<i64>,
    operation_id: String,
) -> Result<crate::db::HumanDecisionCommit, String> {
    RATE_LIMITER.check("record_human_decision")?;
    let _ = (segment_id, decision, corrected_transcript, timestamp_ms, operation_id);
    Err(retired_legacy_decision_error())
}

fn retired_legacy_decision_error() -> String {
    "TYPED_REVIEW_REQUIRED: record_human_decision is retired; use commit_review_v1 with an exact policy-4 playback receipt"
        .to_string()
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
        CommandErrorV1::new(
            "COMMIT_OUTCOME_UNKNOWN",
            "The save outcome could not be confirmed. Your exact retry identity and draft were retained; retry to reconcile it safely.",
            true,
        )
            .operation(operation_id)
            .suggested(SuggestedActionV1::Retry)
    }
}

fn public_playback_error(error: &str) -> CommandErrorV1 {
    if error.contains("E_PLAYBACK_COVERAGE_INSUFFICIENT") {
        CommandErrorV1::new(
            "PLAYBACK_COVERAGE_INSUFFICIENT",
            "Continue listening to this clip before saving a decision.",
            true,
        )
        .suggested(SuggestedActionV1::Retry)
    } else if error.contains("E_NO_PLAYBACK_EVIDENCE") {
        CommandErrorV1::new("NO_PLAYBACK_EVIDENCE", "Listen to this clip before saving a decision.", true)
            .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("E_PLAYBACK_TIME_IMPLAUSIBLE") {
        CommandErrorV1::new(
            "PLAYBACK_TIME_IMPLAUSIBLE",
            "The playback proof did not match elapsed listening time. Reload the clip and listen again.",
            true,
        )
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("E_PLAYBACK_REVISION_CHANGED") {
        CommandErrorV1::new(
            "PLAYBACK_REVISION_CHANGED",
            "The clip changed before playback began. Reload it before listening.",
            true,
        )
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("E_PLAYBACK_EVIDENCE_CHANGED") {
        CommandErrorV1::new(
            "PLAYBACK_EVIDENCE_CHANGED",
            "The clip changed after playback began. Reload it and listen again.",
            true,
        )
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("missing or expired") || error.contains("expired") || error.contains("active-time") {
        CommandErrorV1::new(
            "PLAYBACK_SESSION_EXPIRED",
            "This playback session expired. Reload the clip and listen again.",
            true,
        )
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("E_PLAYBACK_SESSION_LIMIT") {
        CommandErrorV1::new(
            "PLAYBACK_SESSION_LIMIT",
            "Too many playback attempts are open. Finish or reload the current clip.",
            true,
        )
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("E_PLAYBACK_SESSION_FINALIZED") {
        CommandErrorV1::new(
            "PLAYBACK_SESSION_FINALIZED",
            "This playback receipt is already immutable and cannot be cancelled.",
            false,
        )
        .suggested(SuggestedActionV1::Retry)
    } else if error.contains("E_PLAYBACK_CANCEL_IDENTITY_MISMATCH") {
        CommandErrorV1::new(
            "PLAYBACK_CANCEL_IDENTITY_MISMATCH",
            "This playback cancellation belongs to a different attempt.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("different imported source") || error.contains("different interval union") {
        CommandErrorV1::new("PLAYBACK_AUTHORITY_MISMATCH", "The playback proof does not belong to this clip.", false)
            .suggested(SuggestedActionV1::ReloadClip)
    } else if error.to_ascii_lowercase().contains("database is locked")
        || error.to_ascii_lowercase().contains("database is busy")
    {
        CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry playback proof.", true)
            .suggested(SuggestedActionV1::Retry)
    } else {
        CommandErrorV1::new(
            "PLAYBACK_PROOF_FAILED",
            "Playback proof could not be recorded. Reload the clip and try again.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    }
}

/// Map a media-registry refusal on the new-receipt finalization path.
///
/// The caller invokes this only after an exact durable-receipt replay returned `None` and before
/// opening the finalization transaction.  That ordering is important: unlike a transport or
/// post-write error, this outcome proves that no receipt was committed, so the renderer may retire
/// its frozen retry attempt and request fresh playback authority.
fn public_precommit_playback_binding_error(_error: &str) -> CommandErrorV1 {
    CommandErrorV1::new(
        "PLAYBACK_MEDIA_GRANT_UNAVAILABLE",
        "The playback media grant is no longer available. Reload the clip and listen again.",
        true,
    )
    .suggested(SuggestedActionV1::ReloadClip)
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

#[cfg(test)]
fn commit_review_v1_on(
    store: &crate::stores::ReviewWriteStore,
    request: &CommitReviewRequestV1,
) -> Result<CommittedReviewV1, CommandErrorV1> {
    commit_review_v1_on_with_source_lease(store, request, None)
}

fn commit_review_v1_on_with_source_lease(
    store: &crate::stores::ReviewWriteStore,
    request: &CommitReviewRequestV1,
    source_lease: Option<crate::media::VerifiedMediaSourceLease>,
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
    let playback_receipt_id = request.playback_receipt_id.as_str();
    validate::validate_identifier(playback_receipt_id).map_err(|_| {
        CommandErrorV1::new("INVALID_PLAYBACK_RECEIPT", "The playback proof identity is invalid.", false)
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::ReloadClip)
    })?;

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
        .commit_typed_decision_with_source_lease(
            &request.segment_id,
            request.base_revision,
            decision,
            transcript,
            playback_receipt_id,
            &request.operation_id,
            source_lease,
        )
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

/// Versioned, compare-and-swap desktop review boundary. The legacy command name remains registered
/// only to return `TYPED_REVIEW_REQUIRED`; it has no production write path.
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
    let store = state.review_writes();
    let media_grant_id = store
        .desktop_playback_media_grant_id(&request.playback_receipt_id)
        .map_err(|error| public_review_error(&error.to_string(), &request.operation_id))?;
    let source_lease = media_grant_id.and_then(|grant_id| {
        let mut registry = state.lock_media_registry();
        registry.playback_binding(&grant_id).ok().map(|binding| binding.source_lease())
    });
    let commit = commit_review_v1_on_with_source_lease(&store, &request, source_lease)?;
    state.persist_review_cursor(&request.segment_id);
    Ok(commit)
}

fn marked_segment_unusable_v1(
    commit: crate::db::HumanFlagCommit,
    reason: crate::ipc_contract::TechnicalUnusableReasonV1,
) -> MarkedSegmentUnusableV1 {
    MarkedSegmentUnusableV1 {
        segment_id: commit.segment_id,
        committed_revision: commit.flag_revision,
        reason,
        effect_id: format!("flag-effect:{}", commit.effect_event_id),
    }
}

fn mark_segment_unusable_v1_on(
    store: &crate::stores::ReviewWriteStore,
    request: &MarkSegmentUnusableRequestV1,
) -> Result<MarkedSegmentUnusableV1, CommandErrorV1> {
    let invalid = |message: &str| {
        CommandErrorV1::new("INVALID_MARK_UNUSABLE_REQUEST", message, false).operation(&request.operation_id)
    };
    validate::validate_identifier(&request.segment_id).map_err(|_| invalid("The clip identity is invalid."))?;
    validate::validate_identifier(&request.operation_id).map_err(|_| invalid("The operation identity is invalid."))?;
    if request.base_revision < 0 {
        return Err(invalid("The clip revision must be non-negative."));
    }

    let reason = request.reason;
    let commit = store
        .mark_technically_unusable(&request.segment_id, request.base_revision, reason.as_code(), &request.operation_id)
        .map_err(|error| match error {
            crate::stores::TechnicalUnusableCommitError::SegmentNotFound => {
                CommandErrorV1::new("SEGMENT_NOT_FOUND", "This clip no longer exists.", false)
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
            }
            crate::stores::TechnicalUnusableCommitError::StaleRevision { current_revision } => CommandErrorV1::new(
                "STALE_REVISION",
                "This clip changed; reload it before marking the audio unusable.",
                false,
            )
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::ReloadClip)
            .detail("expectedRevision", request.base_revision)
            .detail("currentRevision", current_revision),
            crate::stores::TechnicalUnusableCommitError::AlreadyHumanReviewed => CommandErrorV1::new(
                "HUMAN_TRUTH_ALREADY_COMMITTED",
                "This clip already has a human decision. Reload it; technical failure cannot replace human truth.",
                false,
            )
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::ReloadClip),
            crate::stores::TechnicalUnusableCommitError::SourceChanged => CommandErrorV1::new(
                "AUDIO_SOURCE_CHANGED",
                "This clip's audio source changed while the failure was checked. Reload it before retrying.",
                false,
            )
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::ReloadClip),
            crate::stores::TechnicalUnusableCommitError::MissingFileUnleaseable => CommandErrorV1::new(
                "MISSING_AUDIO_REQUIRES_RELINK",
                "A missing path cannot be sealed as technical evidence. Open Health to restore or relink the audio; the clip and its draft were not changed.",
                false,
            )
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::OpenHealth),
            crate::stores::TechnicalUnusableCommitError::ProbeBusy => CommandErrorV1::new(
                "AUDIO_PROBE_BUSY",
                "Technical audio verification is busy. Retry in a moment.",
                true,
            )
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::Retry),
            crate::stores::TechnicalUnusableCommitError::FailureNotReproduced { declared_reason, observed } => {
                CommandErrorV1::new(
                    "AUDIO_FAILURE_NOT_REPRODUCED",
                    "The backend could not reproduce that technical audio failure. The clip was not changed.",
                    true,
                )
                .operation(&request.operation_id)
                .suggested(SuggestedActionV1::Retry)
                .detail("declaredReason", declared_reason)
                .detail("observed", observed)
            }
            crate::stores::TechnicalUnusableCommitError::Backend(source) => {
                let message = source.to_string();
                if message.contains("operation UUID was already used") {
                    CommandErrorV1::new(
                        "OPERATION_ID_CONFLICT",
                        "This action identity is already bound to a different unusable-audio request.",
                        false,
                    )
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
                } else if message.to_ascii_lowercase().contains("database is locked")
                    || message.to_ascii_lowercase().contains("database is busy")
                {
                    CommandErrorV1::new(
                        "DATABASE_BUSY",
                        "The workspace is busy. Retry marking this audio unusable.",
                        true,
                    )
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::Retry)
                } else {
                    CommandErrorV1::new(
                        "MARK_UNUSABLE_FAILED",
                        "The technical audio failure was not saved. This clip and its draft are unchanged.",
                        false,
                    )
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::OpenHealth)
                }
            }
        })?;
    Ok(marked_segment_unusable_v1(commit, reason))
}

/// Record a technical inability to review this clip. This is not a transcript decision and therefore
/// neither requires nor creates playback evidence, reviewer compensation or human truth.
#[tauri::command]
#[specta::specta]
pub fn mark_segment_unusable_v1(
    state: State<'_, AppState>,
    request: MarkSegmentUnusableRequestV1,
) -> Result<MarkedSegmentUnusableV1, CommandErrorV1> {
    RATE_LIMITER.check("mark_segment_unusable_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many technical-failure saves. Retry in a moment.", true)
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::Retry)
    })?;
    let marked = mark_segment_unusable_v1_on(&state.review_writes(), &request)?;
    state.persist_review_cursor(&request.segment_id);
    Ok(marked)
}

#[cfg(test)]
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

/// Start a policy-4 desktop playback attempt bound to one live media grant and the current immutable
/// clip identity.  The server creates the receipt capability before the media element can play; the
/// capability alone never authorizes a decision.
#[tauri::command]
#[specta::specta]
pub fn begin_desktop_playback_session_v1(
    state: State<'_, AppState>,
    segment_id: String,
    media_grant_id: String,
    expected_revision: i64,
    client_attempt_id: String,
) -> Result<DesktopPlaybackSessionV1, CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("begin_desktop_playback_session_v1")
        .map_err(|_| CommandErrorV1::new("RATE_LIMITED", "Too many playback attempts. Retry in a moment.", true))?;
    validate::validate_identifier(&segment_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "The clip identity is invalid.", false))?;
    validate::validate_identifier(&media_grant_id)
        .map_err(|_| CommandErrorV1::new("INVALID_MEDIA_GRANT", "The media grant identity is invalid.", false))?;
    validate::validate_identifier(&client_attempt_id).map_err(|_| {
        CommandErrorV1::new("INVALID_PLAYBACK_ATTEMPT", "The playback attempt identity is invalid.", false)
    })?;
    if expected_revision < 0 {
        return Err(CommandErrorV1::new("INVALID_REVIEW_REVISION", "The clip revision must be non-negative.", false));
    }

    // Clone a read-only media lease under the registry lock, then release that lock before taking
    // the serialized DB writer. A grant may spend minutes copying/verifying a large source; waiting
    // for that mutex while holding DB would freeze every unrelated query. The cloned OS handle keeps
    // the exact cached bytes sealed through the transaction without nested locks.
    let binding = {
        let mut registry = state.lock_media_registry();
        registry.playback_binding(&media_grant_id).map_err(|error| public_playback_error(&error))?
    };
    let database = state.lock_db();
    let session = database
        .begin_desktop_playback_session_v1(
            &segment_id,
            expected_revision,
            &media_grant_id,
            &client_attempt_id,
            &binding.source_path,
            &binding.audio_content_hash,
            None,
        )
        .map_err(|error| public_playback_error(&error.to_string()))?;
    Ok(DesktopPlaybackSessionV1 {
        playback_receipt_id: session.playback_receipt_id,
        segment_id: session.segment_id,
        segment_revision: session.segment_revision,
        clip_duration_ms: session.clip_duration_ms,
        expires_at_ms: session.expires_at_ms,
    })
}

/// Retire a superseded policy-4 authority only while it is still an unfinalized renderer attempt.
/// The exact receipt/client-attempt pair prevents an old AudioPlayer instance from cancelling a
/// newer one. Replays after successful retirement are harmless; finalized evidence fails closed.
#[tauri::command]
#[specta::specta]
pub fn cancel_desktop_playback_session_v1(
    state: State<'_, AppState>,
    playback_receipt_id: String,
    client_attempt_id: String,
) -> Result<bool, CommandErrorV1> {
    RATE_LIMITER.check("cancel_desktop_playback_session_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many playback cancellations. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    validate::validate_identifier(&playback_receipt_id)
        .map_err(|_| CommandErrorV1::new("INVALID_PLAYBACK_RECEIPT", "The playback identity is invalid.", false))?;
    validate::validate_identifier(&client_attempt_id)
        .map_err(|_| CommandErrorV1::new("INVALID_PLAYBACK_ATTEMPT", "The playback attempt is invalid.", false))?;
    state
        .lock_db()
        .cancel_desktop_playback_session_v1(&playback_receipt_id, &client_attempt_id)
        .map_err(|error| public_playback_error(&error.to_string()))
}

/// Finalize the exact interval union observed under one server-issued playback session.  The
/// database stores the interval rows and policy-4 receipt atomically; a retry with the same union is
/// idempotent, while any altered replay fails closed.
#[tauri::command]
#[specta::specta]
pub fn finalize_desktop_playback_session_v1(
    state: State<'_, AppState>,
    playback_receipt_id: String,
    media_grant_id: String,
    intervals: Vec<PlaybackIntervalV1>,
) -> Result<DesktopPlaybackReceiptV1, CommandErrorV1> {
    RATE_LIMITER
        .check("finalize_desktop_playback_session_v1")
        .map_err(|_| CommandErrorV1::new("RATE_LIMITED", "Too many playback proofs. Retry in a moment.", true))?;
    validate::validate_identifier(&playback_receipt_id)
        .map_err(|_| CommandErrorV1::new("INVALID_PLAYBACK_RECEIPT", "The playback identity is invalid.", false))?;
    validate::validate_identifier(&media_grant_id)
        .map_err(|_| CommandErrorV1::new("INVALID_MEDIA_GRANT", "The media grant identity is invalid.", false))?;
    let intervals = intervals
        .into_iter()
        .map(|interval| crate::db::DesktopPlaybackInterval { start_ms: interval.start_ms, end_ms: interval.end_ms })
        .collect::<Vec<_>>();

    // Exact lost-response replay is recovery, not evidence minting. Once the immutable receipt row
    // exists, requiring its short-lived media-cache grant would strand a durable decision after a
    // suspend/TTL expiry. Release the DB lock before touching the registry on the new-receipt path.
    if let Some(receipt) = {
        let database = state.lock_db();
        database
            .replay_finalized_desktop_playback_receipt_v1(&playback_receipt_id, &media_grant_id, &intervals)
            .map_err(|error| public_playback_error(&error.to_string()))?
    } {
        return Ok(DesktopPlaybackReceiptV1 {
            playback_receipt_id: receipt.playback_receipt_id,
            segment_id: receipt.segment_id,
            segment_revision: receipt.segment_revision,
            unique_played_ms: receipt.unique_played_ms,
            clip_duration_ms: receipt.clip_duration_ms,
            coverage_ratio: receipt.coverage_ratio,
        });
    }

    let binding = {
        let mut registry = state.lock_media_registry();
        registry.playback_binding(&media_grant_id).map_err(|error| public_precommit_playback_binding_error(&error))?
    };
    let database = state.lock_db();
    let receipt = database
        .finalize_desktop_playback_session_v1(
            &playback_receipt_id,
            &media_grant_id,
            &binding.source_path,
            &binding.audio_content_hash,
            &intervals,
        )
        .map_err(|error| public_playback_error(&error.to_string()))?;
    Ok(DesktopPlaybackReceiptV1 {
        playback_receipt_id: receipt.playback_receipt_id,
        segment_id: receipt.segment_id,
        segment_revision: receipt.segment_revision,
        unique_played_ms: receipt.unique_played_ms,
        clip_duration_ms: receipt.clip_duration_ms,
        coverage_ratio: receipt.coverage_ratio,
    })
}

/// Retained only as a fail-closed rolling-compatibility endpoint.  A raw scalar is not listening
/// authority and can never mint a policy-4 desktop receipt.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn record_playback_receipt(
    _state: State<'_, AppState>,
    segment_id: String,
    played_ms: i64,
    clip_duration_ms: i64,
    reviewer: Option<String>,
    session_id: Option<String>,
    _started_at_ms: i64,
) -> Result<(), String> {
    RATE_LIMITER.check("record_playback_receipt")?;
    validate::validate_identifier(&segment_id)?;
    validate_playback_receipt_identity(reviewer.as_deref(), session_id.as_deref())?;
    if played_ms < 0 || clip_duration_ms < 0 {
        return Err("playback receipt durations must not be negative".to_string());
    }
    Err("PLAYBACK_SESSION_REQUIRED: scalar desktop playback receipts are retired; reload the clip".into())
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
        commit_review_v1_on, mark_segment_unusable_v1_on, persist_whole_segment_update_on,
        public_precommit_playback_binding_error, record_human_decision_on, retired_legacy_decision_error,
        validate_playback_receipt_identity,
    };
    use crate::database_runtime::DatabaseRuntime;
    use crate::db::{Database, PlaybackReceipt, SpeechSegment};
    use crate::history::HistoryManager;
    use crate::ipc_contract::{
        CommitReviewRequestV1, MarkSegmentUnusableRequestV1, ReviewDecisionV1, TechnicalUnusableReasonV1,
    };
    use crate::stores::{require_listened, ReviewWriteStore};
    use sha2::{Digest, Sha256};

    #[test]
    fn finalization_media_binding_refusal_is_a_typed_proven_non_commit() {
        let error = public_precommit_playback_binding_error("Cached media file is missing");
        assert_eq!(error.schema, 1);
        assert_eq!(error.code, "PLAYBACK_MEDIA_GRANT_UNAVAILABLE");
        assert!(error.retryable);
        assert_eq!(error.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::ReloadClip));
    }

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

    fn write_test_wav(path: &std::path::Path, sample_count: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for sample in 0..sample_count {
            writer.write_sample(((sample % 127) as i16) - 63).unwrap();
        }
        writer.finalize().unwrap();
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

    fn exact_policy4_receipt(db: &Database, id: &str, played: i64) -> String {
        // Policy-4 source recovery now proves the current immutable source bytes. This fixture must
        // therefore create real audio and persist its canonical decoded-PCM identity instead of the
        // pre-policy-4 dummy digest used by older receipt-only tests.
        let source_path = std::path::PathBuf::from(db.get_segment_by_id(id).unwrap().unwrap().audio_path);
        write_test_wav(&source_path, 160_000);
        let content_hash = crate::export_bundle::current_canonical_pcm_blake3(&source_path).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params![id, content_hash],
            )
            .unwrap();
        let content_hash = db.segment_audio_content_hash(id).unwrap().expect("fixture has exact audio content hash");
        let revision = db.segment_review_revision(id).unwrap().unwrap_or(0);
        let playback_receipt_id = uuid::Uuid::new_v4().to_string();
        let media_grant_id = uuid::Uuid::new_v4().to_string();
        let client_attempt_id = uuid::Uuid::new_v4().to_string();
        let canonical_source = std::fs::canonicalize(&source_path).unwrap();
        let mut canonical_source_text = canonical_source.to_string_lossy().replace('\\', "/");
        if cfg!(windows) {
            canonical_source_text.make_ascii_lowercase();
        }
        let mut source_path_hash = Sha256::new();
        source_path_hash.update(b"cortex-desktop-playback-grant-path-v1\0");
        source_path_hash.update((canonical_source_text.len() as u64).to_le_bytes());
        source_path_hash.update(canonical_source_text.as_bytes());
        let source_path_hash = source_path_hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let now = chrono::Utc::now().timestamp_millis().max(20_000);
        let issued_at_ms = now - 10_000;
        db.connection()
            .execute(
                "INSERT INTO desktop_playback_sessions_v4
                    (playback_receipt_id,media_grant_id,client_attempt_id,surface,session_binding_sha256,
                     grant_source_path_sha256,segment_id,
                     segment_revision,audio_content_hash,reviewer,clip_duration_ms,source_start_ms,
                     source_end_ms,issued_at_ms,expires_at_ms)
                 VALUES (?1,?2,?3,'desktop',NULL,?4,?5,?6,?7,NULL,10000,0,10000,?8,?9)",
                rusqlite::params![
                    playback_receipt_id,
                    media_grant_id,
                    client_attempt_id,
                    source_path_hash,
                    id,
                    revision,
                    content_hash,
                    issued_at_ms,
                    now + 60_000,
                ],
            )
            .unwrap();
        let mut interval_hash = Sha256::new();
        interval_hash.update(b"cortex-desktop-playback-interval-union-v1\0");
        interval_hash.update(1_u64.to_le_bytes());
        interval_hash.update(0_i64.to_le_bytes());
        interval_hash.update(played.to_le_bytes());
        let interval_hash = interval_hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        db.connection()
            .execute(
                "INSERT INTO desktop_playback_intervals_v4
                    (playback_receipt_id,ordinal,start_ms,end_ms,observed_at_ms)
                 VALUES (?1,0,0,?2,?3)",
                rusqlite::params![playback_receipt_id, played, now],
            )
            .unwrap();
        db.connection()
            .execute(
                "INSERT INTO playback_receipts
                    (segment_id,segment_revision,audio_fingerprint,reviewer,session_id,
                     started_at_ms,played_ms,clip_duration_ms,coverage_ratio,policy_version,
                     source_start_ms,source_end_ms,authority_session_id,interval_union_sha256)
                 VALUES (?1,?2,?3,NULL,?4,?5,?6,10000,CAST(?6 AS REAL)/10000.0,4,
                         0,10000,?4,?7)",
                rusqlite::params![
                    id,
                    revision,
                    content_hash,
                    playback_receipt_id,
                    issued_at_ms,
                    played,
                    interval_hash,
                ],
            )
            .unwrap();
        playback_receipt_id
    }

    fn schema_trigger_sql(db: &Database, name: &str) -> String {
        db.connection()
            .query_row("SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1", [name], |row| row.get(0))
            .unwrap()
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
    fn retired_legacy_command_cannot_spend_an_existing_policy3_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "legacy-policy3-bypass");
        receipt(&db, "legacy-policy3-bypass", 9_000);
        require_listened(&db, "legacy-policy3-bypass").expect("fixture has sufficient legacy ambient evidence");

        let error = retired_legacy_decision_error();
        assert!(error.starts_with("TYPED_REVIEW_REQUIRED:"), "{error}");
        let row = db.get_segment_by_id("legacy-policy3-bypass").unwrap().unwrap();
        assert!(
            row.human_decision.is_none() && !row.verified,
            "retiring the public boundary must leave even a policy-3-authorized row untouched",
        );
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
        let playback_receipt_id = exact_policy4_receipt(&db, "typed-review", 9_000);
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
            playback_receipt_id,
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

        // Historical startup proof binds the effect to its immutable prior revision. The current
        // speech row is already at base_revision+1 after the decision, so reusing the live precommit
        // predicate here would brick every successful typed-review database on its next restart.
        let database_path = db.path().to_string();
        drop(store);
        drop(db);
        let reopened = Database::open(&database_path).unwrap();
        reopened.initialize().expect("a committed policy-4 decision must survive restart validation");

        // The semantic validator must re-derive the exact interval hash, not merely accept a
        // canonical-looking 64-character value. Model a trigger-disabled staged/restore clone; the
        // normal production schema still prevents this update before semantic validation runs.
        let receipt_trigger_sql = schema_trigger_sql(&reopened, "playback_receipts_v67_policy4_immutable_update");
        reopened.connection().execute_batch("DROP TRIGGER playback_receipts_v67_policy4_immutable_update;").unwrap();
        reopened
            .connection()
            .execute("UPDATE playback_receipts SET interval_union_sha256=?1 WHERE policy_version=4", ["f".repeat(64)])
            .unwrap();
        reopened.connection().execute_batch(&format!("{receipt_trigger_sql};")).unwrap();
        drop(reopened);
        let corrupted = Database::open(&database_path).unwrap();
        let semantic_error = corrupted
            .initialize()
            .expect_err("a shaped but non-derived interval digest must fail restore/startup semantic proof");
        assert!(semantic_error.to_string().contains("does not match its exact interval authority"));
    }

    #[test]
    fn startup_rejects_a_tampered_unbound_policy4_interval_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "unbound-digest");
        let authority_id = exact_policy4_receipt(&db, "unbound-digest", 9_000);
        let database_path = db.path().to_string();
        let trigger_name = "playback_receipts_v67_policy4_immutable_update";
        let trigger_sql = schema_trigger_sql(&db, trigger_name);
        db.connection().execute_batch(&format!("DROP TRIGGER {trigger_name};")).unwrap();
        db.connection()
            .execute(
                "UPDATE playback_receipts SET interval_union_sha256=?2 WHERE authority_session_id=?1",
                rusqlite::params![authority_id, "f".repeat(64)],
            )
            .unwrap();
        db.connection().execute_batch(&format!("{trigger_sql};")).unwrap();
        drop(db);

        let reopened = Database::open(&database_path).unwrap();
        let error = reopened
            .initialize()
            .expect_err("every finalized policy-4 receipt is validated even before it is consumed by a decision");
        assert!(error.to_string().contains("does not match its exact interval authority"), "{error}");
    }

    #[test]
    fn startup_rejects_tampered_unbound_policy4_interval_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "unbound-interval");
        let authority_id = exact_policy4_receipt(&db, "unbound-interval", 9_000);
        let database_path = db.path().to_string();
        let trigger_name = "desktop_playback_intervals_v4_immutable_update";
        let trigger_sql = schema_trigger_sql(&db, trigger_name);
        db.connection().execute_batch(&format!("DROP TRIGGER {trigger_name};")).unwrap();
        db.connection()
            .execute(
                "UPDATE desktop_playback_intervals_v4 SET end_ms=end_ms-1 WHERE playback_receipt_id=?1",
                [authority_id],
            )
            .unwrap();
        db.connection().execute_batch(&format!("{trigger_sql};")).unwrap();
        drop(db);

        let reopened = Database::open(&database_path).unwrap();
        let error = reopened
            .initialize()
            .expect_err("startup must re-derive receipt counters and hashes from ordered immutable interval rows");
        assert!(error.to_string().contains("does not match its exact interval authority"), "{error}");
    }

    #[test]
    fn typed_review_truth_rolls_back_if_matching_draft_cannot_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "draft-atomicity");
        let playback_receipt_id = exact_policy4_receipt(&db, "draft-atomicity", 9_000);
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
            playback_receipt_id,
        };
        let store = review_store(&db);
        let error = commit_review_v1_on(&store, &request).expect_err("draft-clear failure must abort review truth");
        assert_eq!(error.code, "COMMIT_OUTCOME_UNKNOWN");
        let row = db.get_segment_by_id("draft-atomicity").unwrap().unwrap();
        assert!(row.human_decision.is_none() && !row.verified, "human truth must roll back with draft clear");
        let draft_count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_drafts WHERE segment_id = 'draft-atomicity'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(draft_count, 1);
    }

    #[test]
    fn technical_unusable_mark_is_cas_bound_idempotent_non_human_and_export_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "technical-unusable");
        std::fs::write(tmp.path().join("technical-unusable.wav"), b"not an audio container").unwrap();
        let base_revision = db.segment_review_revision("technical-unusable").unwrap().unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'preserve until commit', datetime('now'))",
                rusqlite::params!["technical-unusable", base_revision],
            )
            .unwrap();
        let request = MarkSegmentUnusableRequestV1 {
            operation_id: "77777777-7777-4777-8777-777777777777".into(),
            segment_id: "technical-unusable".into(),
            base_revision,
            reason: TechnicalUnusableReasonV1::CorruptContainer,
        };
        let store = review_store(&db);

        let first = mark_segment_unusable_v1_on(&store, &request).expect("technical mark");
        assert_eq!(first.segment_id, request.segment_id);
        assert_eq!(first.committed_revision, base_revision + 1);
        assert_eq!(first.reason, TechnicalUnusableReasonV1::CorruptContainer);
        assert!(first.effect_id.starts_with("flag-effect:"));

        let row = db.get_segment_by_id("technical-unusable").unwrap().unwrap();
        assert_eq!(crate::quality::technical_unusable_reason(&row), Some("corruptContainer"));
        assert_eq!(row.verdict.as_deref(), Some("escalated"));
        assert!(row.escalated);
        assert!(row.human_decision.is_none());
        assert!(row.annotated_transcript.is_none());
        assert!(!row.verified);
        assert!(!crate::quality::is_human_rejected(&row));
        assert!(crate::quality::is_technically_unusable(&row));
        assert!(!crate::quality::training_grade_for_segment(&row).training_ready);
        assert!(crate::export::exclude_unexportable_segments(&db, vec![row.clone()]).unwrap().is_empty());

        let counts: (i64, i64, i64, i64, i64) = db
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id=?1),
                    (SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id=?1),
                    (SELECT COUNT(*) FROM review_events WHERE segment_id=?1),
                    (SELECT COUNT(*) FROM review_compensation_ledger WHERE segment_id=?1),
                    (SELECT COUNT(*) FROM playback_receipts WHERE segment_id=?1)",
                rusqlite::params!["technical-unusable"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 0, 0, 0, 0), "technical failure must create only its immutable flag effect");
        let draft_count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_drafts WHERE segment_id='technical-unusable'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(draft_count, 0, "exact-revision draft clears only with the durable technical mark");

        // Repair the source before retrying. Exact operation replay must resolve from the immutable
        // effect before probing current bytes, or a lost success response becomes ambiguous as soon
        // as the owner restores the file.
        write_test_wav(&tmp.path().join("technical-unusable.wav"), 1_600);
        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'autosave raced behind success', datetime('now'))",
                rusqlite::params!["technical-unusable", base_revision],
            )
            .unwrap();
        let replay = mark_segment_unusable_v1_on(&store, &request).expect("lost-response retry");
        assert_eq!(replay, first);
        let stale_draft_count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_drafts WHERE segment_id=?1 AND base_revision=?2",
                rusqlite::params!["technical-unusable", base_revision],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale_draft_count, 0, "an exact replay clears a raced old-revision autosave");

        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'newer work must survive', datetime('now'))",
                rusqlite::params!["technical-unusable", first.committed_revision],
            )
            .unwrap();
        assert_eq!(mark_segment_unusable_v1_on(&store, &request).unwrap(), first);
        let retained_revision: i64 = db
            .connection()
            .query_row("SELECT base_revision FROM review_drafts WHERE segment_id='technical-unusable'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained_revision, first.committed_revision, "old replay must preserve newer-revision work");
        let flag_count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id='technical-unusable'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(flag_count, 1, "idempotent retry must not create a second effect");

        let conflicting_reason =
            MarkSegmentUnusableRequestV1 { reason: TechnicalUnusableReasonV1::DecodeFailed, ..request.clone() };
        let conflict = mark_segment_unusable_v1_on(&store, &conflicting_reason)
            .expect_err("one operation UUID cannot authorize a different reason");
        assert_eq!(conflict.code, "OPERATION_ID_CONFLICT");

        let stale =
            MarkSegmentUnusableRequestV1 { operation_id: "88888888-8888-4888-8888-888888888888".into(), ..request };
        let stale_error = mark_segment_unusable_v1_on(&store, &stale).expect_err("old revision must be refused");
        assert_eq!(stale_error.code, "STALE_REVISION");
        assert_eq!(stale_error.details.get("currentRevision"), Some(&first.committed_revision.into()));
    }

    #[test]
    fn missing_file_reason_returns_actionable_error_without_truth_effect_revision_or_draft_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "missing-unleaseable");
        let base_revision = db.segment_review_revision("missing-unleaseable").unwrap().unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_drafts(segment_id,base_revision,text,updated_at)
                 VALUES(?1,?2,'must remain',datetime('now'))",
                rusqlite::params!["missing-unleaseable", base_revision],
            )
            .unwrap();
        let request = MarkSegmentUnusableRequestV1 {
            operation_id: "78787878-7878-4878-8878-787878787878".into(),
            segment_id: "missing-unleaseable".into(),
            base_revision,
            reason: TechnicalUnusableReasonV1::MissingFile,
        };

        let error = mark_segment_unusable_v1_on(&review_store(&db), &request)
            .expect_err("missing paths cannot be bound to immutable technical evidence");
        assert_eq!(error.code, "MISSING_AUDIO_REQUIRES_RELINK");
        assert!(!error.retryable);
        assert_eq!(error.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::OpenHealth));
        assert_eq!(error.operation_id.as_deref(), Some(request.operation_id.as_str()));

        let row = db.get_segment_by_id(&request.segment_id).unwrap().unwrap();
        assert_eq!(db.segment_review_revision(&request.segment_id).unwrap(), Some(base_revision));
        assert!(row.verdict.is_none() && row.rationale.is_none() && !row.escalated);
        assert!(!crate::quality::is_technically_unusable(&row));
        let state: (i64, i64, String) = db
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id=?1),
                    (SELECT COUNT(*) FROM review_drafts WHERE segment_id=?1),
                    (SELECT text FROM review_drafts WHERE segment_id=?1)",
                [&request.segment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (0, 1, "must remain".to_string()));
    }

    #[test]
    fn technical_unusable_mark_and_draft_clear_are_one_atomic_effect() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "technical-unusable-atomic");
        std::fs::write(tmp.path().join("technical-unusable-atomic.wav"), b"not an audio container").unwrap();
        let base_revision = db.segment_review_revision("technical-unusable-atomic").unwrap().unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'must survive failure', datetime('now'))",
                rusqlite::params!["technical-unusable-atomic", base_revision],
            )
            .unwrap();
        db.connection()
            .execute_batch(
                "CREATE TRIGGER test_refuse_unusable_draft_clear BEFORE DELETE ON review_drafts
                 BEGIN SELECT RAISE(ABORT, 'injected unusable draft clear failure'); END;",
            )
            .unwrap();
        let request = MarkSegmentUnusableRequestV1 {
            operation_id: "99999999-9999-4999-8999-999999999999".into(),
            segment_id: "technical-unusable-atomic".into(),
            base_revision,
            reason: TechnicalUnusableReasonV1::CorruptContainer,
        };
        let store = review_store(&db);
        let error = mark_segment_unusable_v1_on(&store, &request)
            .expect_err("draft-clear failure must abort the entire technical mark");
        assert_eq!(error.code, "MARK_UNUSABLE_FAILED");

        let row = db.get_segment_by_id("technical-unusable-atomic").unwrap().unwrap();
        assert!(!crate::quality::is_technically_unusable(&row));
        assert!(row.verdict.is_none() && row.rationale.is_none() && !row.escalated);
        let counts: (i64, i64) = db
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id=?1),
                    (SELECT COUNT(*) FROM review_drafts WHERE segment_id=?1)",
                rusqlite::params!["technical-unusable-atomic"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 1), "failed mark leaves neither partial effect nor lost draft");
    }

    #[test]
    fn healthy_audio_direct_invocation_is_refused_without_any_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "healthy-unusable-refusal");
        write_test_wav(&tmp.path().join("healthy-unusable-refusal.wav"), 16_000);
        let base_revision = db.segment_review_revision("healthy-unusable-refusal").unwrap().unwrap();
        let request = MarkSegmentUnusableRequestV1 {
            operation_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            segment_id: "healthy-unusable-refusal".into(),
            base_revision,
            reason: TechnicalUnusableReasonV1::DecodeFailed,
        };
        let store = review_store(&db);
        let error = mark_segment_unusable_v1_on(&store, &request)
            .expect_err("renderer claims are not authority over a healthy backend-decodable clip");
        assert_eq!(error.code, "AUDIO_FAILURE_NOT_REPRODUCED");
        assert_eq!(error.details.get("declaredReason"), Some(&"decodeFailed".into()));
        assert_eq!(error.details.get("observed"), Some(&"healthy".into()));

        let row = db.get_segment_by_id("healthy-unusable-refusal").unwrap().unwrap();
        assert_eq!(db.segment_review_revision(&row.id).unwrap(), Some(base_revision));
        assert!(row.verdict.is_none() && row.rationale.is_none() && !row.escalated);
        let effect_count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id=?1", [&row.id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(effect_count, 0);
    }

    #[test]
    fn backend_distinguishes_container_failure_from_post_probe_decode_failure() {
        let tmp = tempfile::tempdir().unwrap();

        let corrupt = db_with_clip(tmp.path(), "corrupt-authority");
        std::fs::write(tmp.path().join("corrupt-authority.wav"), b"definitely not wav").unwrap();
        let corrupt_revision = corrupt.segment_review_revision("corrupt-authority").unwrap().unwrap();
        let corrupt_store = review_store(&corrupt);
        let corrupt_mark = mark_segment_unusable_v1_on(
            &corrupt_store,
            &MarkSegmentUnusableRequestV1 {
                operation_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
                segment_id: "corrupt-authority".into(),
                base_revision: corrupt_revision,
                reason: TechnicalUnusableReasonV1::CorruptContainer,
            },
        )
        .expect("backend reproduced corrupt container");
        assert_eq!(corrupt_mark.reason, TechnicalUnusableReasonV1::CorruptContainer);

        // A structurally valid zero-frame WAV passes the container probe but fails the real decoder
        // with EmptyBuffer, which is the closed decodeFailed reason rather than corruptContainer.
        let decode_dir = tempfile::tempdir().unwrap();
        let decode = db_with_clip(decode_dir.path(), "decode-authority");
        write_test_wav(&decode_dir.path().join("decode-authority.wav"), 0);
        let decode_revision = decode.segment_review_revision("decode-authority").unwrap().unwrap();
        let decode_store = review_store(&decode);
        let decode_mark = mark_segment_unusable_v1_on(
            &decode_store,
            &MarkSegmentUnusableRequestV1 {
                operation_id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc".into(),
                segment_id: "decode-authority".into(),
                base_revision: decode_revision,
                reason: TechnicalUnusableReasonV1::DecodeFailed,
            },
        )
        .expect("backend reproduced post-probe decode failure");
        assert_eq!(decode_mark.reason, TechnicalUnusableReasonV1::DecodeFailed);
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
