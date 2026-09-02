//! Segment mutation IPC commands — slice 9 of the Week-4 `commands.rs` decomposition.
//!
//! `commands.rs` re-exports this module (`pub use segments_write::*;`). Legacy whole-row writes stay
//! retired; library metadata uses the generated compare-and-set v1 contract.
//!
//! These are fast single-row/batch DB writes (edit, delete, speaker rename, human decision, verdict,
//! bounds) — they run on the caller thread (no run_blocking) exactly as before, since a single indexed
//! write is not a UI-freeze risk.

use super::{RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::db::SpeechSegment;
use crate::history::HistoryManager;
use crate::ipc_contract::{
    CommandErrorV1, CommitReviewRequestV1, CommittedReviewV1, DeleteSegmentsRequestV1, DeletedSegmentsV1,
    DesktopHumanDecisionV1, DesktopPlaybackReceiptV1, DesktopPlaybackSessionV1, DesktopReviewFlagKindV1,
    DesktopReviewUndoAvailabilityV1, DesktopReviewUndoBlockReasonV1, DesktopReviewUndoOutcomeV1,
    DesktopReviewUndoTargetV1, MarkSegmentUnusableRequestV1, MarkedSegmentUnusableV1, PlaybackIntervalV1,
    RecordReviewFlagRequestV1, RecordedReviewFlagV1, RenameSpeakerRequestV1, RenamedSpeakerV1, ReviewDecisionV1,
    ReviewDraftV1, SegmentMetadataChangeV1, SuggestedActionV1, UndoDesktopReviewRequestV1,
    UpdateSegmentMetadataRequestV1, UpdatedSegmentMetadataV1,
};
use crate::stores::{SegmentDeleteError, SegmentMetadataChange, SegmentMetadataUpdateError, SpeakerRenameError};
use crate::validation::input as validate;
use crate::AppState;
use tauri::{Manager, State};

const WHOLE_ROW_SEGMENT_WRITE_RETIRED: &str =
    "the whole-row segment writer is retired; use update_segment_metadata_v1 or the review decision/flag flow";
const MAX_SEGMENT_DELETE_IDS: usize = 100_000;

/// RETIRED (deep audit 2026-08-25) — the legacy whole-row segment write.
///
/// It had no callers left: curation autosave moved to `update_segment_metadata_v1` and human truth moved to
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
fn public_segment_metadata_error(error: SegmentMetadataUpdateError) -> CommandErrorV1 {
    match error {
        SegmentMetadataUpdateError::Missing => CommandErrorV1::new(
            "SEGMENT_NOT_FOUND",
            "The selected segment no longer exists. Reload the library.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip),
        SegmentMetadataUpdateError::Conflict(field) => CommandErrorV1::new(
            "STALE_SEGMENT_METADATA",
            "This metadata changed in another operation. Reload the segment before choosing which value to keep.",
            false,
        )
        .detail("field", field)
        .suggested(SuggestedActionV1::ReloadClip),
        SegmentMetadataUpdateError::Application(crate::error::AppError::Validation(_)) => {
            CommandErrorV1::new("INVALID_SEGMENT_METADATA", "The segment metadata is invalid and was not saved.", false)
        }
        SegmentMetadataUpdateError::Application(error) => {
            let normalized = error.to_string().to_ascii_lowercase();
            if normalized.contains("database is locked") || normalized.contains("database is busy") {
                CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry saving this metadata.", true)
                    .suggested(SuggestedActionV1::Retry)
            } else {
                CommandErrorV1::new(
                    "SEGMENT_METADATA_SAVE_FAILED",
                    "The metadata could not be saved. Open Health before retrying.",
                    false,
                )
                .suggested(SuggestedActionV1::OpenHealth)
            }
        }
    }
}

/// Versioned per-field compare-and-set for library-owned metadata. Unlike the retired generic JSON
/// writer, every nullable field is explicit and bound to the exact last server value the renderer
/// observed. A stale save therefore conflicts instead of overwriting newer metadata, while an exact
/// lost-response replay remains an idempotent success.
#[tauri::command]
#[specta::specta]
pub fn update_segment_metadata_v1(
    request: UpdateSegmentMetadataRequestV1,
    state: State<'_, AppState>,
) -> Result<UpdatedSegmentMetadataV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("update_segment_metadata_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many metadata saves. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    validate::validate_identifier(&request.segment_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "The selected segment identity is invalid.", false))?;
    let changes = request
        .changes
        .into_iter()
        .map(|change| match change {
            SegmentMetadataChangeV1::SpeakerId { expected, value } => {
                SegmentMetadataChange::SpeakerId { expected, value }
            }
            SegmentMetadataChangeV1::AlignmentJson { expected, value } => {
                SegmentMetadataChange::AlignmentJson { expected, value }
            }
        })
        .collect::<Vec<_>>();
    let (updated, _mutation) = state
        .segment_writes()
        .update_metadata_v1(&request.segment_id, &changes)
        .map_err(public_segment_metadata_error)?;
    if updated.changed {
        state.session_auto_save();
    }
    Ok(UpdatedSegmentMetadataV1 {
        segment_id: updated.segment_id,
        speaker_id: updated.speaker_id,
        alignment_json: updated.alignment_json,
        changed: updated.changed,
    })
}

fn public_segment_delete_error(error: SegmentDeleteError) -> CommandErrorV1 {
    match error {
        SegmentDeleteError::Authority => CommandErrorV1::new(
            "SEGMENT_DELETE_BLOCKED",
            "Reviewed segments and their evidence are append-only and cannot be deleted.",
            false,
        ),
        SegmentDeleteError::Invalid => {
            CommandErrorV1::new("INVALID_DELETE_REQUEST", "The segment deletion request is invalid.", false)
        }
        SegmentDeleteError::Busy => {
            CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry the deletion.", true)
                .suggested(SuggestedActionV1::Retry)
        }
        SegmentDeleteError::Application => CommandErrorV1::new(
            "SEGMENT_DELETE_FAILED",
            "The segments could not be deleted. Open Health before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth),
    }
}

/// One generated, idempotent deletion boundary for both single and batch UI actions. Duplicate ids
/// are refused by the shared database boundary before evidence archival, and reviewed authority
/// remains append-only. Replaying after response loss succeeds with `deleted_count = 0`.
#[tauri::command]
#[specta::specta]
pub fn delete_segments_v1(
    request: DeleteSegmentsRequestV1,
    state: State<'_, AppState>,
) -> Result<DeletedSegmentsV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("delete_segments_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many deletion requests. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    if request.ids.is_empty() || request.ids.len() > MAX_SEGMENT_DELETE_IDS {
        return Err(CommandErrorV1::new(
            "INVALID_DELETE_REQUEST",
            "Delete between one and 100,000 segments at a time.",
            false,
        ));
    }
    for id in &request.ids {
        validate::validate_identifier(id)
            .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "A segment identity is invalid.", false))?;
    }
    let requested_count = request.ids.len();
    let (deleted_count, _mutation) =
        state.segment_writes().delete_batch(&request.ids).map_err(public_segment_delete_error)?;
    if deleted_count > 0 {
        state.session_auto_save();
    }
    Ok(DeletedSegmentsV1 { requested_count, deleted_count })
}

fn public_speaker_rename_error(error: SpeakerRenameError) -> CommandErrorV1 {
    match error {
        SpeakerRenameError::Invalid => CommandErrorV1::new(
            "INVALID_SPEAKER_RENAME",
            "The speaker rename request is invalid and was not applied.",
            false,
        ),
        SpeakerRenameError::Stale { source_count, target_count } => CommandErrorV1::new(
            "STALE_SPEAKER_INVENTORY",
            "The speaker inventory changed. Review the refreshed counts before confirming again.",
            false,
        )
        .detail("sourceCount", source_count as i64)
        .detail("targetCount", target_count as i64),
        SpeakerRenameError::Busy => {
            CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry the speaker rename.", true)
                .suggested(SuggestedActionV1::Retry)
        }
        SpeakerRenameError::Application => CommandErrorV1::new(
            "SPEAKER_RENAME_FAILED",
            "The speaker could not be renamed. Open Health before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth),
    }
}

/// Atomic compare-and-set speaker rename. Source and target counts make an earlier merge
/// confirmation expire if either group changes before the write reaches SQLite.
#[tauri::command]
#[specta::specta]
pub async fn rename_speaker_v1(
    request: RenameSpeakerRequestV1,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<RenamedSpeakerV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("rename_speaker_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many speaker rename requests. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    if let Some(source_speaker_id) = &request.source_speaker_id {
        validate::validate_text(source_speaker_id, 256, "Source speaker label")
            .map_err(|_| CommandErrorV1::new("INVALID_SPEAKER_ID", "The source speaker identity is invalid.", false))?;
    }
    validate::validate_speaker_label(&request.target_speaker_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SPEAKER_ID", "The target speaker identity is invalid.", false))?;

    let segment_writes = state.segment_writes();
    let source_speaker_id = request.source_speaker_id;
    let target_speaker_id = request.target_speaker_id;
    let expected_source_count = request.expected_source_count;
    let expected_target_count = request.expected_target_count;
    let worker_app = app.clone();
    let (renamed, _mutation) = tokio::task::spawn_blocking(move || {
        let result = segment_writes.rename_speaker_v1(
            source_speaker_id.as_deref(),
            &target_speaker_id,
            expected_source_count,
            expected_target_count,
        )?;
        if let Some(app_state) = worker_app.try_state::<AppState>() {
            app_state.session_auto_save();
        }
        Ok::<_, crate::stores::SpeakerRenameError>(result)
    })
    .await
    .map_err(|_| {
        CommandErrorV1::new(
            "SPEAKER_RENAME_FAILED",
            "The speaker rename worker stopped unexpectedly. Retry the operation.",
            true,
        )
        .suggested(SuggestedActionV1::Retry)
    })?
    .map_err(public_speaker_rename_error)?;
    Ok(RenamedSpeakerV1 {
        source_speaker_id: renamed.source_speaker_id,
        target_speaker_id: renamed.target_speaker_id,
        renamed_count: renamed.renamed_count,
        target_count: renamed.target_count,
        merged: renamed.merged,
    })
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
    if error.contains("E_STALE_REVIEW_DRAFT_WRITE") {
        CommandErrorV1::new(
            "DRAFT_WRITE_SUPERSEDED",
            "A newer draft action replaced this one. Retry the current edit.",
            true,
        )
        .suggested(SuggestedActionV1::Retry)
    } else if error.to_ascii_lowercase().contains("restore generation changed")
        || error.to_ascii_lowercase().contains("database restore is in progress")
    {
        CommandErrorV1::new(
            "DRAFT_WRITE_GENERATION_CHANGED",
            "The workspace was restored while this draft action was pending. Reload the clip and retry.",
            true,
        )
        .suggested(SuggestedActionV1::ReloadClip)
    } else if error.contains("E_STALE_REVIEW_DRAFT") {
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

/// Reserve the exact next draft mutation before starting a possibly slow native write. A later
/// reservation fences an older invocation whose renderer response timed out or whose surface was
/// replaced, while a write already at its commit point completes before this reservation returns.
#[tauri::command]
#[specta::specta]
pub fn reserve_review_draft_write_v1(
    state: State<'_, AppState>,
    segment_id: String,
    operation_id: String,
) -> Result<(), CommandErrorV1> {
    STRICT_RATE_LIMITER.check("reserve_review_draft_write_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many draft actions. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    validate::validate_identifier(&segment_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "The clip identity is invalid.", false))?;
    validate::validate_identifier(&operation_id)
        .map_err(|_| CommandErrorV1::new("INVALID_OPERATION_ID", "The draft operation identity is invalid.", false))?;
    state
        .review_drafts()
        .reserve_write(&segment_id, &operation_id)
        .map_err(|error| public_draft_error(&error.to_string(), "reserved"))
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
    operation_id: String,
) -> Result<ReviewDraftV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("save_review_draft_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many draft saves. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    validate::validate_identifier(&segment_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "The clip identity is invalid.", false))?;
    validate::validate_identifier(&operation_id)
        .map_err(|_| CommandErrorV1::new("INVALID_OPERATION_ID", "The draft operation identity is invalid.", false))?;
    if base_revision < 0 {
        return Err(CommandErrorV1::new("INVALID_REVIEW_REVISION", "The clip revision must be non-negative.", false));
    }
    validate::validate_text(&text, 100_000, "Review draft")
        .map_err(|_| CommandErrorV1::new("INVALID_REVIEW_DRAFT", "The draft is invalid or too long.", false))?;
    state
        .review_drafts()
        .save(&segment_id, base_revision, &text, &operation_id)
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
    operation_id: String,
) -> Result<bool, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("delete_review_draft_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many draft deletes. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    validate::validate_identifier(&segment_id)
        .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "The clip identity is invalid.", false))?;
    validate::validate_identifier(&operation_id)
        .map_err(|_| CommandErrorV1::new("INVALID_OPERATION_ID", "The draft operation identity is invalid.", false))?;
    if base_revision < 0 {
        return Err(CommandErrorV1::new("INVALID_REVIEW_REVISION", "The clip revision must be non-negative.", false));
    }
    state
        .review_drafts()
        .delete_if_revision(&segment_id, base_revision, &operation_id)
        .map_err(|error| public_draft_error(&error.to_string(), "deleted"))
}

fn committed_review_v1(commit: crate::db::HumanDecisionCommit) -> CommittedReviewV1 {
    // Return the same product authority every review/export surface uses. In particular, a reject
    // must not relabel a retained historical machine-jury proposal as the server's authoritative
    // transcript merely because that proposal still occupies `verdict_transcript`.
    let authoritative_transcript = crate::quality::effective_transcript(&commit.segment).to_string();
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

#[cfg(test)]
fn commit_review_v1_on_with_source_lease(
    store: &crate::stores::ReviewWriteStore,
    request: &CommitReviewRequestV1,
    source_lease: Option<crate::media::VerifiedMediaSourceLease>,
) -> Result<CommittedReviewV1, CommandErrorV1> {
    let restore_generation =
        store.capture_restore_generation().map_err(|error| public_review_error(&error, &request.operation_id))?;
    commit_review_v1_on_with_source_lease_at_generation(store, request, source_lease, restore_generation)
}

fn commit_review_v1_on_with_source_lease_at_generation(
    store: &crate::stores::ReviewWriteStore,
    request: &CommitReviewRequestV1,
    source_lease: Option<crate::media::VerifiedMediaSourceLease>,
    restore_generation: crate::database_runtime::RestoreGeneration,
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
        ReviewDecisionV1::Accept => {
            // Accept must name the exact text the renderer showed. Falling back inside the database
            // lets a direct/stale client accidentally bless a legacy machine verdict that was never
            // served under the Verbatim Law.
            let transcript = request
                .transcript
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| invalid("An acceptance must identify the non-blank transcript being approved."))?;
            ("accept", Some(transcript))
        }
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
    };

    let commit = store
        .commit_typed_decision_with_source_lease_at_generation(
            &request.segment_id,
            request.base_revision,
            decision,
            transcript,
            playback_receipt_id,
            &request.operation_id,
            source_lease,
            restore_generation,
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
    let restore_generation =
        store.capture_restore_generation().map_err(|error| public_review_error(&error, &request.operation_id))?;
    let media_grant_id = store
        .desktop_playback_media_grant_id(&request.playback_receipt_id)
        .map_err(|error| public_review_error(&error.to_string(), &request.operation_id))?;
    let source_lease = media_grant_id.and_then(|grant_id| {
        let mut registry = state.lock_media_registry();
        registry.playback_binding(&grant_id).ok().map(|binding| binding.source_lease())
    });
    let commit =
        commit_review_v1_on_with_source_lease_at_generation(&store, &request, source_lease, restore_generation)?;
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
    let _ = (state, effect_event_id, operation_id);
    Err(
        "TYPED_UNDO_REQUIRED: undo_human_decision is retired; reload review truth and use undo_desktop_review_action_v1"
            .into(),
    )
}

fn public_desktop_undo_target_error(error: &crate::error::AppError) -> CommandErrorV1 {
    let normalized = error.to_string().to_ascii_lowercase();
    if error.is_database_busy() || normalized.contains("restore") || normalized.contains("read capacity") {
        CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry loading Undo.", true)
            .suggested(SuggestedActionV1::Retry)
    } else {
        CommandErrorV1::new(
            "UNDO_TARGET_UNAVAILABLE",
            "The restart-safe Undo target could not be verified. Review data was not changed.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    }
}

fn public_desktop_undo_error(error: &crate::error::AppError, operation_id: &str) -> CommandErrorV1 {
    if error.is_database_busy() {
        CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry this exact Undo.", true)
            .operation(operation_id)
            .suggested(SuggestedActionV1::Retry)
    } else if matches!(error, crate::error::AppError::Validation(_))
        && (error.to_string().contains("globally current")
            || error.to_string().contains("same immutable decision")
            || error.to_string().contains("same immutable review flag"))
    {
        CommandErrorV1::new(
            "STALE_UNDO_TARGET",
            "A newer review action or database generation replaced this Undo target. Reload review truth.",
            false,
        )
        .operation(operation_id)
        .suggested(SuggestedActionV1::ReloadClip)
    } else if matches!(error, crate::error::AppError::Validation(_)) {
        CommandErrorV1::new(
            "INVALID_UNDO_REQUEST",
            "This Undo identity is invalid or no longer belongs to a reversible desktop review action.",
            false,
        )
        .operation(operation_id)
        .suggested(SuggestedActionV1::ReloadClip)
    } else {
        CommandErrorV1::new(
            "UNDO_REVIEW_FAILED",
            "The exact Undo outcome could not be verified. Retry with the retained operation identity or open Health.",
            true,
        )
        .operation(operation_id)
        .suggested(SuggestedActionV1::OpenHealth)
    }
}

fn public_desktop_undo_generation_error(error: &str, operation_id: &str) -> CommandErrorV1 {
    if error.contains("generation changed") {
        CommandErrorV1::new(
            "STALE_UNDO_TARGET",
            "The database was restored after this Undo target was loaded. Reload review truth.",
            false,
        )
        .operation(operation_id)
        .suggested(SuggestedActionV1::ReloadClip)
    } else {
        CommandErrorV1::new("DATABASE_BUSY", "Database recovery is busy. Retry after it completes.", true)
            .operation(operation_id)
            .suggested(SuggestedActionV1::Retry)
    }
}

fn get_desktop_review_undo_target_v1_on(
    store: &crate::stores::ReviewWriteStore,
) -> Result<DesktopReviewUndoAvailabilityV1, CommandErrorV1> {
    let before = store
        .capture_restore_generation()
        .map_err(|error| public_desktop_undo_target_error(&crate::error::AppError::Other(error)))?;
    let availability =
        store.desktop_review_undo_availability().map_err(|error| public_desktop_undo_target_error(&error))?;
    let after = store
        .capture_restore_generation()
        .map_err(|error| public_desktop_undo_target_error(&crate::error::AppError::Other(error)))?;
    if before != after {
        return Err(CommandErrorV1::new(
            "STALE_UNDO_TARGET",
            "The database changed while Undo history was loading. Retry from current review truth.",
            true,
        )
        .suggested(SuggestedActionV1::Retry));
    }
    match availability {
        crate::db::DesktopReviewUndoAvailability::NoHistory => Ok(DesktopReviewUndoAvailabilityV1::None),
        crate::db::DesktopReviewUndoAvailability::Blocked(reason) => {
            let reason = match reason {
                crate::db::DesktopReviewUndoBlockReason::LegacyHistory => DesktopReviewUndoBlockReasonV1::LegacyHistory,
                crate::db::DesktopReviewUndoBlockReason::LatestDecisionUndone => {
                    DesktopReviewUndoBlockReasonV1::LatestDecisionUndone
                }
                crate::db::DesktopReviewUndoBlockReason::LatestFlagUndone => {
                    DesktopReviewUndoBlockReasonV1::LatestFlagUndone
                }
                crate::db::DesktopReviewUndoBlockReason::DecisionShadowed => {
                    DesktopReviewUndoBlockReasonV1::DecisionShadowed
                }
                crate::db::DesktopReviewUndoBlockReason::FlagShadowed => DesktopReviewUndoBlockReasonV1::FlagShadowed,
            };
            Ok(DesktopReviewUndoAvailabilityV1::Blocked { reason })
        }
        crate::db::DesktopReviewUndoAvailability::Available(authority) => {
            let target = match authority {
                crate::db::DesktopReviewUndoAuthority::Decision(authority) => {
                    let decision = match authority.action.as_str() {
                        "accept" => DesktopHumanDecisionV1::Accept,
                        "edit" => DesktopHumanDecisionV1::Edit,
                        "reject" => DesktopHumanDecisionV1::Reject,
                        _ => {
                            return Err(CommandErrorV1::new(
                                "UNDO_TARGET_INVALID",
                                "The stored Undo target failed integrity validation. Review data was not changed.",
                                false,
                            )
                            .suggested(SuggestedActionV1::OpenHealth));
                        }
                    };
                    DesktopReviewUndoTargetV1::Decision {
                        effect_event_id: authority.effect_event_id,
                        segment_id: authority.segment_id,
                        decision,
                        source_operation_id: authority.decision_operation_id,
                        source_payload_hash: authority.decision_payload_hash,
                        database_generation: before.serial(),
                    }
                }
                crate::db::DesktopReviewUndoAuthority::Flag(authority) => {
                    let flag_kind = match authority.flag_kind {
                        crate::db::DesktopReviewFlagKind::Generic => DesktopReviewFlagKindV1::Generic,
                        crate::db::DesktopReviewFlagKind::TechnicalUnusable(reason) => {
                            let reason = match reason.as_str() {
                                "decodeFailed" => crate::ipc_contract::TechnicalUnusableReasonV1::DecodeFailed,
                                "missingFile" => crate::ipc_contract::TechnicalUnusableReasonV1::MissingFile,
                                "permissionDenied" => crate::ipc_contract::TechnicalUnusableReasonV1::PermissionDenied,
                                "corruptContainer" => crate::ipc_contract::TechnicalUnusableReasonV1::CorruptContainer,
                                _ => {
                                    return Err(CommandErrorV1::new(
                                        "UNDO_TARGET_INVALID",
                                        "The stored technical Undo target failed integrity validation. Review data was not changed.",
                                        false,
                                    )
                                    .suggested(SuggestedActionV1::OpenHealth));
                                }
                            };
                            DesktopReviewFlagKindV1::TechnicalUnusable { reason }
                        }
                    };
                    DesktopReviewUndoTargetV1::Flag {
                        effect_event_id: authority.effect_event_id,
                        segment_id: authority.segment_id,
                        source_operation_id: authority.flag_operation_id,
                        source_payload_hash: authority.flag_payload_hash,
                        prior_revision: authority.prior_revision,
                        flag_revision: authority.flag_revision,
                        flag_kind,
                        database_generation: before.serial(),
                    }
                }
            };
            Ok(DesktopReviewUndoAvailabilityV1::Available { target })
        }
    }
}

fn undo_desktop_review_action_v1_on(
    store: &crate::stores::ReviewWriteStore,
    request: &UndoDesktopReviewRequestV1,
) -> Result<DesktopReviewUndoOutcomeV1, CommandErrorV1> {
    let database_generation = match &request.target {
        DesktopReviewUndoTargetV1::Decision { database_generation, .. }
        | DesktopReviewUndoTargetV1::Flag { database_generation, .. } => *database_generation,
    };
    let _mutation = store
        .begin_mutation_at_restore_generation_serial(database_generation)
        .map_err(|error| public_desktop_undo_generation_error(&error, &request.operation_id))?;
    match &request.target {
        DesktopReviewUndoTargetV1::Decision {
            effect_event_id,
            segment_id,
            decision,
            source_operation_id,
            source_payload_hash,
            ..
        } => {
            let action = match decision {
                DesktopHumanDecisionV1::Accept => "accept",
                DesktopHumanDecisionV1::Edit => "edit",
                DesktopHumanDecisionV1::Reject => "reject",
            };
            let authority = crate::db::DesktopHumanDecisionUndoAuthority {
                effect_event_id: *effect_event_id,
                segment_id: segment_id.clone(),
                action: action.into(),
                decision_operation_id: source_operation_id.clone(),
                decision_payload_hash: source_payload_hash.clone(),
            };
            store
                .undo_latest_desktop_human_decision(&authority, &request.operation_id)
                .map(|outcome| DesktopReviewUndoOutcomeV1::from_decision_database(*effect_event_id, outcome))
                .map_err(|error| public_desktop_undo_error(&error, &request.operation_id))
        }
        DesktopReviewUndoTargetV1::Flag {
            effect_event_id,
            segment_id,
            source_operation_id,
            source_payload_hash,
            prior_revision,
            flag_revision,
            flag_kind,
            ..
        } => {
            let flag_kind = match flag_kind {
                DesktopReviewFlagKindV1::Generic => crate::db::DesktopReviewFlagKind::Generic,
                DesktopReviewFlagKindV1::TechnicalUnusable { reason } => {
                    crate::db::DesktopReviewFlagKind::TechnicalUnusable(reason.as_code().into())
                }
            };
            let authority = crate::db::DesktopReviewFlagUndoAuthority {
                effect_event_id: *effect_event_id,
                segment_id: segment_id.clone(),
                flag_operation_id: source_operation_id.clone(),
                prior_revision: *prior_revision,
                flag_revision: *flag_revision,
                flag_payload_hash: source_payload_hash.clone(),
                flag_kind,
            };
            store
                .undo_latest_desktop_review_flag(&authority, &request.operation_id)
                .map(|outcome| DesktopReviewUndoOutcomeV1::from_flag_database(*effect_event_id, outcome))
                .map_err(|error| public_desktop_undo_error(&error, &request.operation_id))
        }
    }
}

/// Discover the globally latest database-proven active desktop decision or flag. The tagged target
/// is restore-generation-bound and carries only server-derived immutable authority, so Backspace
/// cannot cross-route colliding decision/flag effect ids after restart.
#[tauri::command]
#[specta::specta]
pub async fn get_desktop_review_undo_target_v1(
    state: State<'_, AppState>,
) -> Result<DesktopReviewUndoAvailabilityV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("get_desktop_review_undo_target_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many Undo-history reads. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    let store = state.review_writes();
    tokio::task::spawn_blocking(move || get_desktop_review_undo_target_v1_on(&store)).await.map_err(|_| {
        CommandErrorV1::new(
            "UNDO_TARGET_WORKER_FAILED",
            "The Undo-history worker stopped unexpectedly. Review data was not changed.",
            true,
        )
        .suggested(SuggestedActionV1::Retry)
    })?
}

/// Reverse one server-owned desktop decision or flag snapshot with a stable idempotency UUID. The
/// renderer supplies only the tagged immutable target; it cannot provide or overwrite restored row
/// truth or private technical-flag rationale.
#[tauri::command]
#[specta::specta]
pub async fn undo_desktop_review_action_v1(
    state: State<'_, AppState>,
    request: UndoDesktopReviewRequestV1,
) -> Result<DesktopReviewUndoOutcomeV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("undo_desktop_review_action_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many Undo attempts. Retry this exact Undo in a moment.", true)
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::Retry)
    })?;
    let store = state.review_writes();
    let operation_id = request.operation_id.clone();
    tokio::task::spawn_blocking(move || undo_desktop_review_action_v1_on(&store, &request)).await.map_err(|_| {
        CommandErrorV1::new(
            "UNDO_REVIEW_WORKER_FAILED",
            "The Undo worker stopped unexpectedly. Retry with the retained operation identity.",
            true,
        )
        .operation(&operation_id)
        .suggested(SuggestedActionV1::Retry)
    })?
}

fn recorded_review_flag_v1(commit: crate::db::HumanFlagCommit) -> RecordedReviewFlagV1 {
    RecordedReviewFlagV1 {
        effect_event_id: commit.effect_event_id,
        segment_id: commit.segment_id,
        prior_revision: commit.prior_revision,
        flag_revision: commit.flag_revision,
        segment: commit.segment,
    }
}

fn record_review_flag_on(
    store: &crate::stores::ReviewWriteStore,
    request: &RecordReviewFlagRequestV1,
) -> Result<RecordedReviewFlagV1, CommandErrorV1> {
    let invalid = |message: &str| {
        CommandErrorV1::new("INVALID_REVIEW_FLAG_REQUEST", message, false).operation(&request.operation_id)
    };
    validate::validate_identifier(&request.segment_id).map_err(|_| invalid("The clip identity is invalid."))?;
    validate::validate_identifier(&request.operation_id).map_err(|_| invalid("The operation identity is invalid."))?;
    if !uuid::Uuid::parse_str(&request.operation_id)
        .is_ok_and(|parsed| parsed.hyphenated().to_string() == request.operation_id)
    {
        return Err(invalid("The operation identity is invalid."));
    }
    if request.base_revision < 0 {
        return Err(invalid("The clip revision must be non-negative."));
    }
    validate::validate_text(&request.rationale, 10_000, "Review flag rationale")
        .map_err(|_| invalid("The review flag rationale is invalid."))?;

    store
        .record_flag(&request.segment_id, request.base_revision, &request.rationale, &request.operation_id)
        .map(recorded_review_flag_v1)
        .map_err(|error| match error {
            crate::stores::ReviewFlagCommitError::SegmentNotFound => {
                CommandErrorV1::new("SEGMENT_NOT_FOUND", "This clip no longer exists.", false)
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
            }
            crate::stores::ReviewFlagCommitError::StaleRevision { current_revision } => {
                CommandErrorV1::new("STALE_REVISION", "This clip changed; reload it before flagging it.", false)
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
                    .detail("expectedRevision", request.base_revision)
                    .detail("currentRevision", current_revision)
            }
            crate::stores::ReviewFlagCommitError::Backend(source) => {
                let message = source.to_string();
                if message.contains("operation UUID was already used")
                    || message.contains("operation UUID is already bound")
                {
                    CommandErrorV1::new(
                        "OPERATION_ID_CONFLICT",
                        "This action identity is already bound to a different review flag request.",
                        false,
                    )
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
                } else if message.contains("already has an active immutable effect") {
                    CommandErrorV1::new(
                        "REVIEW_FLAG_ALREADY_ACTIVE",
                        "This clip already has an active review flag. Reload it before taking another action.",
                        false,
                    )
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
                } else if message.to_ascii_lowercase().contains("database is locked")
                    || message.to_ascii_lowercase().contains("database is busy")
                {
                    CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry this exact flag.", true)
                        .operation(&request.operation_id)
                        .suggested(SuggestedActionV1::Retry)
                } else if matches!(source, crate::error::AppError::Validation(_)) {
                    CommandErrorV1::new(
                        "REVIEW_FLAG_REFUSED",
                        "The review flag was refused and no clip truth was changed. Reload the clip before retrying.",
                        false,
                    )
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::ReloadClip)
                } else {
                    CommandErrorV1::new(
                        "COMMIT_OUTCOME_UNKNOWN",
                        "The review flag outcome could not be confirmed. Restart Cortex to reconcile database truth.",
                        true,
                    )
                    .operation(&request.operation_id)
                    .suggested(SuggestedActionV1::Retry)
                }
            }
        })
}

/// Compare-and-swap generic owner review flag. The exact renderer-observed revision is part of the
/// idempotency payload, so stale UI state cannot flag a newer clip state.
#[tauri::command]
#[specta::specta]
pub fn record_review_flag(
    state: State<'_, AppState>,
    request: RecordReviewFlagRequestV1,
) -> Result<RecordedReviewFlagV1, CommandErrorV1> {
    RATE_LIMITER.check("record_review_flag").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many review flag attempts. Retry in a moment.", true)
            .operation(&request.operation_id)
            .suggested(SuggestedActionV1::Retry)
    })?;
    let committed = record_review_flag_on(&state.review_writes(), &request)?;
    state.persist_review_cursor(&request.segment_id);
    Ok(committed)
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

    let store = state.review_writes();
    let restore_generation = store.capture_restore_generation().map_err(|error| public_playback_error(&error))?;
    // Clone a read-only media lease under the registry lock, then release that lock before taking
    // the serialized DB writer. A grant may spend minutes copying/verifying a large source; waiting
    // for that mutex while holding DB would freeze every unrelated query. The cloned OS handle keeps
    // the exact cached bytes sealed through the transaction without nested locks. The generation
    // captured above prevents this pre-restore binding from minting authority after a restore.
    let binding = {
        let mut registry = state.lock_media_registry();
        registry.playback_binding(&media_grant_id).map_err(|error| public_playback_error(&error))?
    };
    let session = store
        .begin_desktop_playback_session_at_generation_v1(
            &segment_id,
            expected_revision,
            &media_grant_id,
            &client_attempt_id,
            &binding.source_path,
            &binding.audio_content_hash,
            None,
            restore_generation,
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
        .review_writes()
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
    let store = state.review_writes();
    let restore_generation =
        store.capture_restore_generation().map_err(|error| public_playback_error(&error))?.serial();

    // Exact lost-response replay is recovery, not evidence minting. Once the immutable receipt row
    // exists, requiring its short-lived media-cache grant would strand a durable decision after a
    // suspend/TTL expiry. Release the DB lock before touching the registry on the new-receipt path.
    if let Some(receipt) = {
        store
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
    let receipt = store
        .finalize_desktop_playback_session_at_generation_v1(
            &playback_receipt_id,
            &media_grant_id,
            &binding.source_path,
            &binding.audio_content_hash,
            &intervals,
            restore_generation,
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

    use std::time::{Duration, Instant};

    /// Bound for retrying a direct technical mark that was refused `AUDIO_PROBE_BUSY`.
    const PROBE_BUSY_RETRY_BUDGET: Duration = Duration::from_secs(5);

    /// Retry `attempt` while it answers `AUDIO_PROBE_BUSY`, for at most `budget`; any other answer
    /// returns at once. The technical-audio probe registry caps ACTIVE flights process-wide
    /// (`TECHNICAL_PROBE_MAX_CONCURRENCY` = 2), and every `#[test]` in this binary probes through the
    /// same registry, so under full-suite parallelism a direct mark with a unique key can be refused
    /// while unrelated tests hold both slots -- measured 2026-09-02, one failure in a 2561-test run,
    /// 3/3 green standalone. BUSY is `retryable: true` ("Retry in a moment") and a client retries;
    /// so do the tests. Production behaviour is untouched.
    fn retry_while_probe_busy<T>(
        budget: Duration,
        mut attempt: impl FnMut() -> Result<T, crate::ipc_contract::CommandErrorV1>,
    ) -> Result<T, crate::ipc_contract::CommandErrorV1> {
        let started = Instant::now();
        loop {
            match attempt() {
                Err(error) if error.code == "AUDIO_PROBE_BUSY" && started.elapsed() < budget => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                other => return other,
            }
        }
    }

    fn mark_unusable_retrying_busy(
        store: &crate::stores::ReviewWriteStore,
        request: &MarkSegmentUnusableRequestV1,
    ) -> Result<crate::ipc_contract::MarkedSegmentUnusableV1, crate::ipc_contract::CommandErrorV1> {
        retry_while_probe_busy(PROBE_BUSY_RETRY_BUDGET, || mark_segment_unusable_v1_on(store, request))
    }

    #[test]
    fn retry_while_probe_busy_retries_only_busy_and_gives_up_at_the_budget() {
        let busy = || crate::ipc_contract::CommandErrorV1::new("AUDIO_PROBE_BUSY", "busy", true);
        let mut calls = 0;
        let answered = retry_while_probe_busy(Duration::from_secs(5), || {
            calls += 1;
            if calls < 3 {
                Err(busy())
            } else {
                Ok(42)
            }
        });
        assert_eq!(answered.map_err(|e| e.code), Ok(42), "the first non-busy answer is returned");
        assert_eq!(calls, 3);

        let mut calls = 0;
        let refused = retry_while_probe_busy(Duration::from_secs(5), || {
            calls += 1;
            Err::<(), _>(crate::ipc_contract::CommandErrorV1::new("INVALID_REQUEST", "no", false))
        });
        assert_eq!(refused.map_err(|e| e.code), Err("INVALID_REQUEST".to_string()), "a refusal is not retried");
        assert_eq!(calls, 1);

        let mut calls = 0;
        let started = Instant::now();
        let gave_up = retry_while_probe_busy(Duration::from_millis(120), || {
            calls += 1;
            Err::<(), _>(busy())
        });
        assert_eq!(
            gave_up.map_err(|e| e.code),
            Err("AUDIO_PROBE_BUSY".to_string()),
            "busy past the budget is reported as busy"
        );
        assert!(calls >= 2 && started.elapsed() >= Duration::from_millis(120), "it kept trying until the budget");
    }
    use super::{
        commit_review_v1_on, get_desktop_review_undo_target_v1_on, mark_segment_unusable_v1_on,
        persist_whole_segment_update_on, public_desktop_undo_error, public_precommit_playback_binding_error,
        public_segment_delete_error, public_segment_metadata_error, public_speaker_rename_error,
        record_human_decision_on, record_review_flag_on, retired_legacy_decision_error,
        undo_desktop_review_action_v1_on, validate_playback_receipt_identity,
    };
    use crate::database_runtime::DatabaseRuntime;
    use crate::db::{Database, PlaybackReceipt, SpeechSegment};
    use crate::history::HistoryManager;
    use crate::ipc_contract::{
        CommitReviewRequestV1, DesktopHumanDecisionV1, DesktopReviewUndoAvailabilityV1, DesktopReviewUndoBlockReasonV1,
        DesktopReviewUndoOutcomeV1, DesktopReviewUndoTargetV1, MarkSegmentUnusableRequestV1, RecordReviewFlagRequestV1,
        ReviewDecisionV1, TechnicalUnusableReasonV1, UndoDesktopReviewRequestV1,
    };
    use crate::stores::{
        require_listened, ReviewWriteStore, SegmentDeleteError, SegmentMetadataUpdateError, SpeakerRenameError,
    };
    use sha2::{Digest, Sha256};

    fn available_undo_target(availability: DesktopReviewUndoAvailabilityV1) -> DesktopReviewUndoTargetV1 {
        match availability {
            DesktopReviewUndoAvailabilityV1::Available { target } => target,
            other => panic!("expected an available restart-safe Undo target, got {other:?}"),
        }
    }

    fn exact_undo_request(target: &DesktopReviewUndoTargetV1, operation_id: &str) -> UndoDesktopReviewRequestV1 {
        UndoDesktopReviewRequestV1 { target: target.clone(), operation_id: operation_id.into() }
    }

    #[test]
    fn finalization_media_binding_refusal_is_a_typed_proven_non_commit() {
        let error = public_precommit_playback_binding_error("Cached media file is missing");
        assert_eq!(error.schema, 1);
        assert_eq!(error.code, "PLAYBACK_MEDIA_GRANT_UNAVAILABLE");
        assert!(error.retryable);
        assert_eq!(error.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::ReloadClip));
    }

    #[test]
    fn metadata_refusals_are_typed_and_scrub_backend_details() {
        let conflict = public_segment_metadata_error(SegmentMetadataUpdateError::Conflict("speakerId"));
        assert_eq!(conflict.code, "STALE_SEGMENT_METADATA");
        assert_eq!(conflict.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::ReloadClip));
        assert_eq!(
            conflict.details.get("field"),
            Some(&crate::ipc_contract::CommandErrorDetailV1::String("speakerId".into()))
        );

        let internal = public_segment_metadata_error(SegmentMetadataUpdateError::Application(
            crate::error::AppError::Other("SQL failed at C:\\private\\owner.db with secret token".into()),
        ));
        let public = serde_json::to_string(&internal).unwrap();
        assert_eq!(internal.code, "SEGMENT_METADATA_SAVE_FAILED");
        assert!(!public.contains("owner.db") && !public.contains("secret token"), "{public}");
    }

    #[test]
    fn deletion_refusals_are_typed_and_scrub_backend_details() {
        let blocked = public_segment_delete_error(SegmentDeleteError::Authority);
        assert_eq!(blocked.code, "SEGMENT_DELETE_BLOCKED");
        assert!(!blocked.retryable);

        let invalid = public_segment_delete_error(SegmentDeleteError::Invalid);
        assert_eq!(invalid.code, "INVALID_DELETE_REQUEST");

        let internal = public_segment_delete_error(SegmentDeleteError::Application);
        let public = serde_json::to_string(&internal).unwrap();
        assert_eq!(internal.code, "SEGMENT_DELETE_FAILED");
        assert!(!public.contains("owner.db") && !public.contains("secret token"), "{public}");
    }

    #[test]
    fn speaker_rename_refusals_are_typed_bounded_and_actionable() {
        let stale = public_speaker_rename_error(SpeakerRenameError::Stale { source_count: 7, target_count: 3 });
        assert_eq!(stale.code, "STALE_SPEAKER_INVENTORY");
        assert!(!stale.retryable);
        assert_eq!(stale.details.get("sourceCount"), Some(&crate::ipc_contract::CommandErrorDetailV1::Number(7.0)));
        assert_eq!(stale.details.get("targetCount"), Some(&crate::ipc_contract::CommandErrorDetailV1::Number(3.0)));

        let busy = public_speaker_rename_error(SpeakerRenameError::Busy);
        assert_eq!(busy.code, "DATABASE_BUSY");
        assert!(busy.retryable);

        let internal = public_speaker_rename_error(SpeakerRenameError::Application);
        let public = serde_json::to_string(&internal).unwrap();
        assert_eq!(internal.code, "SPEAKER_RENAME_FAILED");
        assert!(!public.contains("owner.db") && !public.contains("secret token"), "{public}");
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
        ReviewWriteStore::new(DatabaseRuntime::isolated_for_test(writer))
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
    fn typed_accept_without_the_served_transcript_fails_before_any_write() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "typed-accept-missing-text");
        let base_revision = db.segment_review_revision("typed-accept-missing-text").unwrap().unwrap();
        let request = CommitReviewRequestV1 {
            operation_id: "77777777-7777-4777-8777-777777777777".into(),
            segment_id: "typed-accept-missing-text".into(),
            base_revision,
            decision: ReviewDecisionV1::Accept,
            transcript: None,
            reason_code: None,
            playback_receipt_id: "88888888-8888-4888-8888-888888888888".into(),
        };

        let error = commit_review_v1_on(&review_store(&db), &request)
            .expect_err("a typed accept may never infer which transcript was approved");
        assert_eq!(error.code, "INVALID_REVIEW_REQUEST");
        assert!(error.message.contains("transcript being approved"));
        let row = db.get_segment_by_id("typed-accept-missing-text").unwrap().unwrap();
        assert!(row.human_decision.is_none() && row.verdict_transcript.is_none() && !row.verified);
    }

    #[test]
    fn typed_reject_response_never_promotes_a_retained_machine_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "typed-reject-machine-proposal");
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET verdict='jury_edit',
                        verdict_transcript='machine proposal',
                        jury_transcript='machine proposal'
                  WHERE id='typed-reject-machine-proposal'",
                [],
            )
            .unwrap();
        let playback_receipt_id = exact_policy4_receipt(&db, "typed-reject-machine-proposal", 9_000);
        let base_revision = db.segment_review_revision("typed-reject-machine-proposal").unwrap().unwrap();
        let request = CommitReviewRequestV1 {
            operation_id: "99999999-9999-4999-8999-999999999999".into(),
            segment_id: "typed-reject-machine-proposal".into(),
            base_revision,
            decision: ReviewDecisionV1::Reject,
            transcript: None,
            reason_code: None,
            playback_receipt_id,
        };

        let committed = commit_review_v1_on(&review_store(&db), &request).expect("typed reject");
        assert_eq!(committed.authoritative_transcript, "دەق");
        assert_ne!(committed.authoritative_transcript, "machine proposal");
        let row = db.get_segment_by_id("typed-reject-machine-proposal").unwrap().unwrap();
        assert_eq!(row.human_decision.as_deref(), Some("reject"));
        assert_eq!(crate::quality::effective_transcript(&row), "دەق");
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

        let first = mark_unusable_retrying_busy(&store, &request).expect("technical mark");
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
        assert_eq!(draft_count, 1, "technical classification must retain non-authoritative owner work");

        // Repair the source before retrying. Exact operation replay must resolve from the immutable
        // effect before probing current bytes, or a lost success response becomes ambiguous as soon
        // as the owner restores the file.
        write_test_wav(&tmp.path().join("technical-unusable.wav"), 1_600);
        let replay = mark_unusable_retrying_busy(&store, &request).expect("lost-response retry");
        assert_eq!(replay, first);
        let stale_draft_count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_drafts WHERE segment_id=?1 AND base_revision=?2",
                rusqlite::params!["technical-unusable", base_revision],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale_draft_count, 1, "an exact replay must retain old-revision human work");

        db.connection()
            .execute(
                "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, 'newer work must survive', datetime('now'))
                 ON CONFLICT(segment_id) DO UPDATE SET
                    base_revision=excluded.base_revision,
                    text=excluded.text,
                    updated_at=excluded.updated_at",
                rusqlite::params!["technical-unusable", first.committed_revision],
            )
            .unwrap();
        assert_eq!(mark_unusable_retrying_busy(&store, &request).unwrap(), first);
        let mut retained_revisions_query = db
            .connection()
            .prepare(
                "SELECT base_revision FROM review_drafts WHERE segment_id='technical-unusable' ORDER BY base_revision",
            )
            .unwrap();
        let retained_revisions = retained_revisions_query
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            retained_revisions,
            vec![first.committed_revision],
            "old replay must preserve the single newer-revision owner draft"
        );
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
        let conflict = mark_unusable_retrying_busy(&store, &conflicting_reason)
            .expect_err("one operation UUID cannot authorize a different reason");
        assert_eq!(conflict.code, "OPERATION_ID_CONFLICT");

        let stale =
            MarkSegmentUnusableRequestV1 { operation_id: "88888888-8888-4888-8888-888888888888".into(), ..request };
        let stale_error = mark_unusable_retrying_busy(&store, &stale).expect_err("old revision must be refused");
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

        let error = mark_unusable_retrying_busy(&review_store(&db), &request)
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
    fn technical_unusable_mark_never_deletes_a_saved_draft() {
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
        mark_unusable_retrying_busy(&store, &request)
            .expect("technical mark must not attempt to delete non-authoritative draft work");

        let row = db.get_segment_by_id("technical-unusable-atomic").unwrap().unwrap();
        assert!(crate::quality::is_technically_unusable(&row));
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
        assert_eq!(counts, (1, 1), "technical mark is durable while the saved draft remains intact");
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
        let error = mark_unusable_retrying_busy(&store, &request)
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
        let corrupt_mark = mark_unusable_retrying_busy(
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
        let decode_mark = mark_unusable_retrying_busy(
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
        let base_revision = db.segment_review_revision("desktop-flag-replay").unwrap().unwrap();
        let first =
            db.record_review_flag("desktop-flag-replay", base_revision, "Needs a second listen", operation_id).unwrap();
        let replay = db
            .record_review_flag("desktop-flag-replay", base_revision, "Needs a second listen", operation_id)
            .expect("an exact retry must return the original flag commit");
        assert_eq!(replay.effect_event_id, first.effect_event_id);
        assert_eq!(replay.flag_revision, first.flag_revision);

        let conflict = db
            .record_review_flag("desktop-flag-replay", base_revision, "Different request", operation_id)
            .expect_err("one operation UUID cannot authorize a different flag request");
        assert!(conflict.to_string().contains("different request"), "{conflict}");
        let revision_conflict = db
            .record_review_flag("desktop-flag-replay", first.flag_revision, "Needs a second listen", operation_id)
            .expect_err("one operation UUID cannot be replayed with a different base revision");
        assert!(revision_conflict.to_string().contains("different request"), "{revision_conflict}");
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

    #[test]
    fn stale_desktop_flag_is_typed_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "desktop-flag-stale");
        db.connection()
            .execute(
                "UPDATE speech_segments SET review_revision = 5 WHERE id = ?1",
                rusqlite::params!["desktop-flag-stale"],
            )
            .unwrap();
        // The renderer still holds r5 while an independent owner-path mutation advances truth to r6.
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET normalized_transcript = 'server truth at r6', review_revision = 6
                  WHERE id = ?1 AND review_revision = 5",
                rusqlite::params!["desktop-flag-stale"],
            )
            .unwrap();
        let store = review_store(&db);
        let operation_id = "34343434-3434-4434-8434-343434343434";
        let error = record_review_flag_on(
            &store,
            &RecordReviewFlagRequestV1 {
                operation_id: operation_id.into(),
                segment_id: "desktop-flag-stale".into(),
                base_revision: 5,
                rationale: "Needs a second listen".into(),
            },
        )
        .expect_err("an r5 renderer must not flag server truth at r6");
        assert_eq!(error.code, "STALE_REVISION");
        assert_eq!(error.operation_id.as_deref(), Some(operation_id));
        assert_eq!(
            error.details.get("expectedRevision"),
            Some(&crate::ipc_contract::CommandErrorDetailV1::Number(5.0))
        );
        assert_eq!(error.details.get("currentRevision"), Some(&crate::ipc_contract::CommandErrorDetailV1::Number(6.0)));
        let unchanged = db.get_segment_by_id("desktop-flag-stale").unwrap().unwrap();
        assert_eq!(db.segment_review_revision("desktop-flag-stale").unwrap(), Some(6));
        assert_eq!(unchanged.normalized_transcript.as_deref(), Some("server truth at r6"));
        assert_eq!(unchanged.verdict, None);
        assert_eq!(unchanged.rationale, None);
        assert!(!unchanged.escalated);
        let effect_count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id = ?1",
                rusqlite::params!["desktop-flag-stale"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(effect_count, 0, "a stale flag must not create partial effect evidence");
    }

    #[test]
    fn typed_desktop_undo_survives_restart_rejects_forgery_and_replays_exactly() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "restart-undo");
        let database_path = db.path().to_string();
        let playback_receipt_id = exact_policy4_receipt(&db, "restart-undo", 9_000);
        let base_revision = db.segment_review_revision("restart-undo").unwrap().unwrap();
        let request = CommitReviewRequestV1 {
            operation_id: "dddddddd-dddd-4ddd-8ddd-dddddddddddd".into(),
            segment_id: "restart-undo".into(),
            base_revision,
            decision: ReviewDecisionV1::Accept,
            transcript: Some("دەق".into()),
            reason_code: None,
            playback_receipt_id,
        };
        let store = review_store(&db);
        let committed = commit_review_v1_on(&store, &request).expect("typed desktop decision commits");
        drop(store);
        drop(db);

        let reopened = Database::open(&database_path).expect("reopen committed workspace");
        let reopened_store = review_store(&reopened);
        let target = available_undo_target(
            get_desktop_review_undo_target_v1_on(&reopened_store).expect("restart-safe target read"),
        );
        let target_effect_event_id = match &target {
            DesktopReviewUndoTargetV1::Decision { effect_event_id, segment_id, decision, .. } => {
                assert_eq!(segment_id, "restart-undo");
                assert_eq!(*decision, DesktopHumanDecisionV1::Accept);
                *effect_event_id
            }
            other => panic!("expected a decision Undo target, got {other:?}"),
        };
        assert_eq!(target_effect_event_id.to_string(), committed.decision_id.trim_start_matches("effect:"));

        let forged = exact_undo_request(&target, "not-a-uuid");
        let refused = undo_desktop_review_action_v1_on(&reopened_store, &forged)
            .expect_err("renderer forgery must not mutate review truth");
        assert_eq!(refused.code, "INVALID_UNDO_REQUEST");
        assert!(reopened.get_segment_by_id("restart-undo").unwrap().unwrap().verified);

        let undo_request = exact_undo_request(&target, "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee");
        let applied =
            undo_desktop_review_action_v1_on(&reopened_store, &undo_request).expect("exact restart-safe undo applies");
        let restored_revision = match applied {
            DesktopReviewUndoOutcomeV1::Applied { effect_event_id, restored_revision, ref segment, .. } => {
                assert_eq!(effect_event_id, target_effect_event_id);
                assert_eq!(segment.id, "restart-undo");
                assert!(!segment.verified);
                restored_revision
            }
            _ => panic!("first exact inverse must report applied"),
        };
        assert!(
            matches!(
                get_desktop_review_undo_target_v1_on(&reopened_store).unwrap(),
                DesktopReviewUndoAvailabilityV1::Blocked {
                    reason: DesktopReviewUndoBlockReasonV1::LatestDecisionUndone
                }
            ),
            "a reversed effect must never reappear as a restart target"
        );
        reopened
            .insert_segment(&SpeechSegment {
                id: "newer-action-after-lost-response".into(),
                audio_path: tmp.path().join("newer-action.wav").to_string_lossy().into_owned(),
                raw_transcript: "دەق".into(),
                duration_ms: 10_000,
                ..SpeechSegment::default()
            })
            .unwrap();
        let newer_action_revision =
            reopened.segment_review_revision("newer-action-after-lost-response").unwrap().unwrap();
        reopened_store
            .record_flag(
                "newer-action-after-lost-response",
                newer_action_revision,
                "Later action after the owner did not receive the Undo response",
                "abababab-abab-4aba-8aba-abababababab",
            )
            .unwrap();
        let replay = undo_desktop_review_action_v1_on(&reopened_store, &undo_request)
            .expect("lost-response retry resolves idempotently after a later review action");
        let replay_json = serde_json::to_string(&replay).unwrap();
        assert!(
            !replay_json.contains("segment") && !replay_json.contains("restoredRevision"),
            "an idempotent retry must not mislabel newer mutable truth as the old Undo result: {replay_json}"
        );
        match replay {
            DesktopReviewUndoOutcomeV1::AlreadyApplied { effect_event_id, .. } => {
                assert_eq!(effect_event_id, target_effect_event_id);
                assert_eq!(
                    reopened.segment_review_revision("restart-undo").unwrap(),
                    Some(restored_revision),
                    "the test workspace remains at the applied inverse revision"
                );
            }
            _ => panic!("exact inverse replay must report alreadyApplied"),
        }
    }

    #[test]
    fn typed_desktop_undo_refuses_a_target_from_before_restore_generation_without_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "stale-generation-undo");
        let playback_receipt_id = exact_policy4_receipt(&db, "stale-generation-undo", 9_000);
        let base_revision = db.segment_review_revision("stale-generation-undo").unwrap().unwrap();
        let store = review_store(&db);
        commit_review_v1_on(
            &store,
            &CommitReviewRequestV1 {
                operation_id: "10101010-1010-4010-8010-101010101010".into(),
                segment_id: "stale-generation-undo".into(),
                base_revision,
                decision: ReviewDecisionV1::Accept,
                transcript: Some("دەق".into()),
                reason_code: None,
                playback_receipt_id,
            },
        )
        .unwrap();
        let target = available_undo_target(get_desktop_review_undo_target_v1_on(&store).unwrap());
        let request = exact_undo_request(&target, "20202020-2020-4020-8020-202020202020");
        let generation_before = db.restore_generation_sha256().unwrap();
        let reversal_count_before: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM human_decision_effect_reversals", [], |row| row.get(0))
            .unwrap();
        let journal_count_before: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get(0))
            .unwrap();

        store.advance_restore_generation_for_test().unwrap();

        let error = undo_desktop_review_action_v1_on(&store, &request)
            .expect_err("a target fetched before a committed restore generation must be stale");
        assert_eq!(error.code, "STALE_UNDO_TARGET");
        assert_eq!(db.restore_generation_sha256().unwrap(), generation_before);
        assert_eq!(
            db.connection()
                .query_row("SELECT COUNT(*) FROM human_decision_effect_reversals", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            reversal_count_before
        );
        assert_eq!(
            db.connection()
                .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            journal_count_before
        );
        assert!(db.get_segment_by_id("stale-generation-undo").unwrap().unwrap().verified);
    }

    #[test]
    fn typed_flag_undo_refuses_a_target_from_before_restore_generation_without_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "stale-generation-flag-undo");
        let store = review_store(&db);
        let base_revision = db.segment_review_revision("stale-generation-flag-undo").unwrap().unwrap();
        store
            .record_flag(
                "stale-generation-flag-undo",
                base_revision,
                "Restore generation must dominate replay acknowledgement",
                "30303030-3030-4030-8030-303030303030",
            )
            .unwrap();
        let target = available_undo_target(get_desktop_review_undo_target_v1_on(&store).unwrap());
        let request = exact_undo_request(&target, "40404040-4040-4040-8040-404040404040");
        let revision_before = db.segment_review_revision("stale-generation-flag-undo").unwrap();
        let reversal_count_before: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_reversals", [], |row| row.get(0))
            .unwrap();
        let journal_count_before: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get(0))
            .unwrap();

        store.advance_restore_generation_for_test().unwrap();
        let error = undo_desktop_review_action_v1_on(&store, &request)
            .expect_err("a flag target fetched before restore generation changes must be stale");
        assert_eq!(error.code, "STALE_UNDO_TARGET");
        assert_eq!(db.segment_review_revision("stale-generation-flag-undo").unwrap(), revision_before);
        assert!(db.get_segment_by_id("stale-generation-flag-undo").unwrap().unwrap().escalated);
        assert_eq!(
            db.connection()
                .query_row("SELECT COUNT(*) FROM review_flag_effect_reversals", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            reversal_count_before
        );
        assert_eq!(
            db.connection()
                .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            journal_count_before
        );
    }

    #[test]
    fn generic_flag_survives_restart_and_exact_undo_leaves_a_crash_barrier() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "decision-before-flag");
        let database_path = db.path().to_string();
        let playback_receipt_id = exact_policy4_receipt(&db, "decision-before-flag", 9_000);
        let base_revision = db.segment_review_revision("decision-before-flag").unwrap().unwrap();
        let store = review_store(&db);
        commit_review_v1_on(
            &store,
            &CommitReviewRequestV1 {
                operation_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into(),
                segment_id: "decision-before-flag".into(),
                base_revision,
                decision: ReviewDecisionV1::Accept,
                transcript: Some("دەق".into()),
                reason_code: None,
                playback_receipt_id,
            },
        )
        .unwrap();
        db.insert_segment(&SpeechSegment {
            id: "newer-flag".into(),
            audio_path: tmp.path().join("newer-flag.wav").to_string_lossy().into_owned(),
            raw_transcript: "دەق".into(),
            duration_ms: 10_000,
            ..SpeechSegment::default()
        })
        .unwrap();
        let newer_flag_revision = db.segment_review_revision("newer-flag").unwrap().unwrap();
        let flag = store
            .record_flag(
                "newer-flag",
                newer_flag_revision,
                "Needs a second independent listen",
                "ffffffff-ffff-4fff-8fff-ffffffffffff",
            )
            .unwrap();
        drop(store);
        drop(db);

        let reopened = Database::open(&database_path).unwrap();
        let reopened_store = review_store(&reopened);
        let target = available_undo_target(get_desktop_review_undo_target_v1_on(&reopened_store).unwrap());
        match &target {
            DesktopReviewUndoTargetV1::Flag {
                effect_event_id,
                segment_id,
                prior_revision,
                flag_revision,
                flag_kind,
                ..
            } => {
                assert_eq!(*effect_event_id, flag.effect_event_id);
                assert_eq!(segment_id, "newer-flag");
                assert_eq!(*prior_revision, newer_flag_revision);
                assert_eq!(*flag_revision, newer_flag_revision + 1);
                assert_eq!(*flag_kind, crate::ipc_contract::DesktopReviewFlagKindV1::Generic);
            }
            other => panic!("expected generic flag Undo target after restart, got {other:?}"),
        }
        let request = exact_undo_request(&target, "99999999-9999-4999-8999-999999999999");
        assert!(matches!(
            undo_desktop_review_action_v1_on(&reopened_store, &request).unwrap(),
            DesktopReviewUndoOutcomeV1::Applied {
                effect_kind: crate::ipc_contract::DesktopReviewUndoEffectKindV1::Flag,
                effect_event_id,
                ..
            } if effect_event_id == flag.effect_event_id
        ));
        assert!(
            matches!(
                get_desktop_review_undo_target_v1_on(&reopened_store).unwrap(),
                DesktopReviewUndoAvailabilityV1::Blocked { reason: DesktopReviewUndoBlockReasonV1::LatestFlagUndone }
            ),
            "a flag plus its inverse remains a crash barrier; Undo never falls through to an older decision"
        );

        reopened
            .insert_segment(&SpeechSegment {
                id: "later-action-after-flag-undo".into(),
                audio_path: tmp.path().join("later-action-after-flag-undo.wav").to_string_lossy().into_owned(),
                raw_transcript: "دەق".into(),
                duration_ms: 10_000,
                ..SpeechSegment::default()
            })
            .unwrap();
        let later_revision = reopened.segment_review_revision("later-action-after-flag-undo").unwrap().unwrap();
        reopened_store
            .record_flag(
                "later-action-after-flag-undo",
                later_revision,
                "Later action after a lost flag-Undo response",
                "abababab-cdcd-4efe-8a8a-abababababab",
            )
            .unwrap();
        let reversals_before: i64 = reopened
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_reversals", [], |row| row.get(0))
            .unwrap();
        let journal_before: i64 = reopened
            .connection()
            .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get(0))
            .unwrap();
        assert!(matches!(
            undo_desktop_review_action_v1_on(&reopened_store, &request).unwrap(),
            DesktopReviewUndoOutcomeV1::AlreadyApplied {
                effect_kind: crate::ipc_contract::DesktopReviewUndoEffectKindV1::Flag,
                effect_event_id,
            } if effect_event_id == flag.effect_event_id
        ));
        let conflicting_replay = exact_undo_request(&target, "88888888-7777-4666-8555-444444444444");
        assert!(matches!(
            undo_desktop_review_action_v1_on(&reopened_store, &conflicting_replay).unwrap(),
            DesktopReviewUndoOutcomeV1::Conflict {
                effect_kind: crate::ipc_contract::DesktopReviewUndoEffectKindV1::Flag,
                effect_event_id,
            } if effect_event_id == flag.effect_event_id
        ));
        assert_eq!(
            reopened
                .connection()
                .query_row("SELECT COUNT(*) FROM review_flag_effect_reversals", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            reversals_before,
            "exact replay and a different inverse UUID must not append another reversal"
        );
        assert_eq!(
            reopened
                .connection()
                .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            journal_before,
            "replay outcomes must not append another action-journal record"
        );
    }

    #[test]
    fn technical_flag_with_saved_draft_survives_restart_and_exact_undo_preserves_owner_work() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "technical-flag-undo");
        std::fs::write(tmp.path().join("technical-flag-undo.wav"), b"not an audio container").unwrap();
        let database_path = db.path().to_string();
        let base_revision = db.segment_review_revision("technical-flag-undo").unwrap().unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_drafts(segment_id,base_revision,text,updated_at)
                 VALUES(?1,?2,'owner correction must survive',datetime('now'))",
                rusqlite::params!["technical-flag-undo", base_revision],
            )
            .unwrap();
        let request = MarkSegmentUnusableRequestV1 {
            operation_id: "12345678-1234-4234-8234-123456789abc".into(),
            segment_id: "technical-flag-undo".into(),
            base_revision,
            reason: TechnicalUnusableReasonV1::CorruptContainer,
        };
        let store = review_store(&db);
        let marked = mark_unusable_retrying_busy(&store, &request).unwrap();
        assert_eq!(marked.committed_revision, base_revision + 1);
        let private_rationale = db
            .get_segment_by_id("technical-flag-undo")
            .unwrap()
            .unwrap()
            .rationale
            .expect("technical evidence has a canonical private rationale");
        drop(store);
        drop(db);

        let reopened = Database::open(&database_path).unwrap();
        let reopened_store = review_store(&reopened);
        let target = available_undo_target(get_desktop_review_undo_target_v1_on(&reopened_store).unwrap());
        let target_json = serde_json::to_string(&target).unwrap();
        let effect_event_id = match &target {
            DesktopReviewUndoTargetV1::Flag {
                effect_event_id,
                segment_id,
                source_operation_id,
                prior_revision,
                flag_revision,
                flag_kind,
                ..
            } => {
                assert_eq!(segment_id, "technical-flag-undo");
                assert_eq!(source_operation_id, &request.operation_id);
                assert_eq!(*prior_revision, base_revision);
                assert_eq!(*flag_revision, base_revision + 1);
                assert_eq!(
                    *flag_kind,
                    crate::ipc_contract::DesktopReviewFlagKindV1::TechnicalUnusable {
                        reason: TechnicalUnusableReasonV1::CorruptContainer
                    }
                );
                *effect_event_id
            }
            other => panic!("expected technical flag Undo target after restart, got {other:?}"),
        };
        assert!(
            !target_json.contains(&private_rationale)
                && !target_json.contains("path=")
                && !target_json.contains("audio="),
            "renderer target must not expose private technical rationale or source hashes: {target_json}"
        );
        let retained_before: (i64, String) = reopened
            .connection()
            .query_row(
                "SELECT base_revision,text FROM review_drafts WHERE segment_id=?1",
                ["technical-flag-undo"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(retained_before, (base_revision, "owner correction must survive".into()));

        let undo_request = exact_undo_request(&target, "abcdefab-cdef-4abc-8def-abcdefabcdef");
        let outcome = undo_desktop_review_action_v1_on(&reopened_store, &undo_request).unwrap();
        assert!(matches!(
            outcome,
            DesktopReviewUndoOutcomeV1::Applied {
                effect_kind: crate::ipc_contract::DesktopReviewUndoEffectKindV1::Flag,
                effect_event_id: applied_effect,
                ref segment,
                ..
            } if applied_effect == effect_event_id
                && segment.id == "technical-flag-undo"
                && segment.verdict.is_none()
                && segment.rationale.is_none()
                && !segment.escalated
        ));
        let retained_after: (i64, String) = reopened
            .connection()
            .query_row(
                "SELECT base_revision,text FROM review_drafts WHERE segment_id=?1",
                ["technical-flag-undo"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(retained_after, retained_before, "flag Undo must not delete or synthesize transcript work");
        drop(reopened_store);
        drop(reopened);

        let restarted_again = Database::open(&database_path).unwrap();
        let restarted_store = review_store(&restarted_again);
        assert!(matches!(
            get_desktop_review_undo_target_v1_on(&restarted_store).unwrap(),
            DesktopReviewUndoAvailabilityV1::Blocked { reason: DesktopReviewUndoBlockReasonV1::LatestFlagUndone }
        ));
        let retained_after_restart: (i64, String) = restarted_again
            .connection()
            .query_row(
                "SELECT base_revision,text FROM review_drafts WHERE segment_id=?1",
                ["technical-flag-undo"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(retained_after_restart, retained_before);
    }

    #[test]
    fn typed_flag_undo_refuses_every_forged_authority_field_without_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "forged-flag-undo");
        let store = review_store(&db);
        let base_revision = db.segment_review_revision("forged-flag-undo").unwrap().unwrap();
        store
            .record_flag(
                "forged-flag-undo",
                base_revision,
                "Exact immutable flag authority",
                "11111111-2222-4333-8444-555555555555",
            )
            .unwrap();
        let target = available_undo_target(get_desktop_review_undo_target_v1_on(&store).unwrap());
        let pristine = db.get_segment_by_id("forged-flag-undo").unwrap().unwrap();
        let pristine_revision = db.segment_review_revision("forged-flag-undo").unwrap().unwrap();
        let mutations = [
            ("effectEventId", serde_json::json!(9_999), "INVALID_UNDO_REQUEST"),
            ("segmentId", serde_json::json!("another-segment"), "STALE_UNDO_TARGET"),
            ("sourceOperationId", serde_json::json!("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"), "STALE_UNDO_TARGET"),
            ("sourcePayloadHash", serde_json::json!("f".repeat(64)), "STALE_UNDO_TARGET"),
            ("priorRevision", serde_json::json!(base_revision + 7), "STALE_UNDO_TARGET"),
            ("flagRevision", serde_json::json!(base_revision + 8), "STALE_UNDO_TARGET"),
            ("flagKind", serde_json::json!({"kind":"technicalUnusable","reason":"decodeFailed"}), "STALE_UNDO_TARGET"),
            ("databaseGeneration", serde_json::json!(9_999), "STALE_UNDO_TARGET"),
        ];
        let inverse_operations = [
            "00000000-0000-4000-8000-000000000101",
            "00000000-0000-4000-8000-000000000102",
            "00000000-0000-4000-8000-000000000103",
            "00000000-0000-4000-8000-000000000104",
            "00000000-0000-4000-8000-000000000105",
            "00000000-0000-4000-8000-000000000106",
            "00000000-0000-4000-8000-000000000107",
            "00000000-0000-4000-8000-000000000108",
        ];
        let journal_before: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get(0))
            .unwrap();

        for ((field, value, expected_code), operation_id) in mutations.into_iter().zip(inverse_operations) {
            let mut forged_json = serde_json::to_value(&target).unwrap();
            forged_json.as_object_mut().unwrap().insert(field.into(), value);
            let forged: DesktopReviewUndoTargetV1 = serde_json::from_value(forged_json).unwrap();
            let error = undo_desktop_review_action_v1_on(&store, &exact_undo_request(&forged, operation_id))
                .expect_err("forged flag authority must fail before mutation");
            assert_eq!(error.code, expected_code, "wrong public error for forged field {field}");
            let current = db.get_segment_by_id("forged-flag-undo").unwrap().unwrap();
            assert_eq!(
                db.segment_review_revision("forged-flag-undo").unwrap(),
                Some(pristine_revision),
                "{field} changed revision"
            );
            assert_eq!(current.verdict, pristine.verdict, "{field} changed verdict");
            assert_eq!(current.rationale, pristine.rationale, "{field} changed rationale");
            assert_eq!(current.escalated, pristine.escalated, "{field} changed escalation");
            let reversal_count: i64 = db
                .connection()
                .query_row("SELECT COUNT(*) FROM review_flag_effect_reversals", [], |row| row.get(0))
                .unwrap();
            assert_eq!(reversal_count, 0, "{field} wrote a reversal");
            assert_eq!(
                db.connection()
                    .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                journal_before,
                "{field} appended a journal record"
            );
        }
    }

    #[test]
    fn stale_flag_inverse_cannot_jump_over_a_newer_desktop_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "older-flag");
        db.insert_segment(&SpeechSegment {
            id: "newer-decision".into(),
            audio_path: tmp.path().join("newer-decision.wav").to_string_lossy().into_owned(),
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
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params!["newer-decision", "b".repeat(64)],
            )
            .unwrap();
        let store = review_store(&db);
        let older_flag_revision = db.segment_review_revision("older-flag").unwrap().unwrap();
        let older = store
            .record_flag(
                "older-flag",
                older_flag_revision,
                "First global review action must not jump a newer decision",
                "12121212-1212-4212-8212-121212121212",
            )
            .unwrap();
        let stale_target = available_undo_target(get_desktop_review_undo_target_v1_on(&store).unwrap());
        assert!(matches!(
            &stale_target,
            DesktopReviewUndoTargetV1::Flag { effect_event_id, .. } if *effect_event_id == older.effect_event_id
        ));
        let playback_receipt_id = exact_policy4_receipt(&db, "newer-decision", 9_000);
        let base_revision = db.segment_review_revision("newer-decision").unwrap().unwrap();
        commit_review_v1_on(
            &store,
            &CommitReviewRequestV1 {
                operation_id: "34343434-3434-4434-8434-343434343434".into(),
                segment_id: "newer-decision".into(),
                base_revision,
                decision: ReviewDecisionV1::Accept,
                transcript: Some("دەق".into()),
                reason_code: None,
                playback_receipt_id,
            },
        )
        .unwrap();

        let reversals_before: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_flag_effect_reversals", [], |row| row.get(0))
            .unwrap();
        let journal_before: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get(0))
            .unwrap();
        let refused = undo_desktop_review_action_v1_on(
            &store,
            &exact_undo_request(&stale_target, "56565656-5656-4656-8656-565656565656"),
        )
        .expect_err("global LIFO authority must reject a stale typed flag inverse");
        assert_eq!(refused.code, "STALE_UNDO_TARGET");
        let still_flagged = db.get_segment_by_id("older-flag").unwrap().unwrap();
        assert!(still_flagged.escalated, "the stale inverse must leave the older flag untouched");
        assert_eq!(
            db.connection()
                .query_row("SELECT COUNT(*) FROM review_flag_effect_reversals", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            reversals_before
        );
        assert_eq!(
            db.connection()
                .query_row("SELECT COUNT(*) FROM desktop_review_action_events_v1", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            journal_before
        );
    }

    #[test]
    fn generic_internal_undo_cannot_bypass_typed_desktop_authority() {
        let tmp = tempfile::tempdir().unwrap();
        let db = db_with_clip(tmp.path(), "typed-only-desktop-undo");
        let playback_receipt_id = exact_policy4_receipt(&db, "typed-only-desktop-undo", 9_000);
        let base_revision = db.segment_review_revision("typed-only-desktop-undo").unwrap().unwrap();
        let store = review_store(&db);
        let committed = commit_review_v1_on(
            &store,
            &CommitReviewRequestV1 {
                operation_id: "78787878-7878-4878-8878-787878787878".into(),
                segment_id: "typed-only-desktop-undo".into(),
                base_revision,
                decision: ReviewDecisionV1::Accept,
                transcript: Some("دەق".into()),
                reason_code: None,
                playback_receipt_id,
            },
        )
        .unwrap();
        let effect_event_id = committed
            .decision_id
            .strip_prefix("effect:")
            .and_then(|value| value.parse::<i64>().ok())
            .expect("typed commit returns its opaque effect identity");
        let refused = db
            .undo_human_decision(effect_event_id, None, "90909090-9090-4090-8090-909090909090")
            .expect_err("a caller without typed immutable authority must not undo a desktop decision");
        assert!(refused.to_string().contains("does not own"), "unexpected refusal: {refused}");
        assert!(db.get_segment_by_id("typed-only-desktop-undo").unwrap().unwrap().verified);
    }

    #[test]
    fn typed_desktop_undo_errors_never_expose_private_backend_details() {
        let stale_flag = public_desktop_undo_error(
            &crate::error::AppError::Validation(
                "desktop Undo authority no longer identifies the same immutable review flag".into(),
            ),
            "11111111-1111-4111-8111-111111111111",
        );
        assert_eq!(stale_flag.code, "STALE_UNDO_TARGET");
        assert_eq!(stale_flag.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::ReloadClip));

        let error = public_desktop_undo_error(
            &crate::error::AppError::Other(
                "SQL failed at C:\\private\\owner.db with secret token and raw statement".into(),
            ),
            "11111111-1111-4111-8111-111111111111",
        );
        let public = serde_json::to_string(&error).unwrap();
        assert_eq!(error.code, "UNDO_REVIEW_FAILED");
        assert!(!public.contains("owner.db") && !public.contains("secret token") && !public.contains("statement"));
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

    /// Wave-4 state-boundary coverage: the `#[tauri::command]` wrappers above, invoked through a
    /// genuine managed `State<'_, AppState>` (`crate::test_support::managed_app_state`), so the
    /// limiter/validation/store-mapping closures run exactly as production IPC runs them. The `_on`
    /// helpers already carry the deep semantics in the parent module; these tests own the wrappers.
    mod state_command_surface_tests {
        use super::super::{
            begin_desktop_playback_session_v1, cancel_desktop_playback_session_v1, clear_human_decision,
            commit_review_v1, delete_review_draft_v1, delete_segments_v1, finalize_desktop_playback_session_v1,
            get_desktop_review_undo_target_v1, get_review_draft_v1, list_recording_rights, mark_segment_unusable_v1,
            record_human_decision, record_playback_receipt, record_review_flag, reserve_review_draft_write_v1,
            restore_segment_snapshot, revoke_recording_consent, save_review_draft_v1, set_recording_rights,
            undo_desktop_review_action_v1, undo_human_decision, update_segment, update_segment_metadata_v1,
        };
        use super::{available_undo_target, exact_policy4_receipt, exact_undo_request};
        use crate::db::SpeechSegment;
        use crate::ipc_contract::{
            CommitReviewRequestV1, DeleteSegmentsRequestV1, DesktopReviewUndoAvailabilityV1,
            DesktopReviewUndoOutcomeV1, MarkSegmentUnusableRequestV1, PlaybackIntervalV1, RecordReviewFlagRequestV1,
            ReviewDecisionV1, SegmentMetadataChangeV1, TechnicalUnusableReasonV1, UpdateSegmentMetadataRequestV1,
        };
        use crate::test_support::managed_app_state;
        use crate::validation::input as validate;
        use crate::AppState;
        use tauri::Manager;

        type MockApp = tauri::App<tauri::test::MockRuntime>;

        /// Seed one real clip into the MANAGED state's database — the same shape as the parent
        /// module's `db_with_clip`, but writing through the state so the commands under test read it.
        fn seed_state_clip(app: &MockApp, dir: &std::path::Path, id: &str) -> i64 {
            let state = app.state::<AppState>();
            let db = state.lock_db();
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
            db.segment_review_revision(id).unwrap().unwrap_or(0)
        }

        fn owner_rights(license: Option<String>) -> crate::db::RecordingRights {
            crate::db::RecordingRights {
                license,
                consent_basis: Some("explicit_consent".into()),
                permitted_use: Some("train".into()),
                attribution: None,
                source: Some("owner supplied".into()),
                revoked_at: None,
            }
        }

        #[test]
        fn recording_rights_commands_declare_revoke_and_list_through_state() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            let raw_path = tmp.path().join("rights-recording.wav");
            std::fs::write(&raw_path, b"recording bytes").unwrap();
            // The command canonicalizes before matching rows, so the seeded clip must carry the
            // exact validated form of the path or the declaration would cover zero segments.
            let validated = validate::validate_file_path(raw_path.to_string_lossy().as_ref()).unwrap();
            app.state::<AppState>()
                .lock_db()
                .insert_segment(&SpeechSegment {
                    id: "rights-seg".into(),
                    audio_path: validated.clone(),
                    raw_transcript: "دەق".into(),
                    ..SpeechSegment::default()
                })
                .unwrap();

            let missing = set_recording_rights(
                tmp.path().join("never-created.wav").to_string_lossy().into_owned(),
                owner_rights(Some("owner-full-rights".into())),
                app.state(),
            )
            .expect_err("a nonexistent recording path must fail validation");
            assert!(missing.contains("Invalid path"), "{missing}");

            let oversized = set_recording_rights(
                raw_path.to_string_lossy().into_owned(),
                owner_rights(Some("l".repeat(2001))),
                app.state(),
            )
            .expect_err("an unbounded licence field must be refused");
            assert!(oversized.contains("Licence"), "{oversized}");

            let covered = set_recording_rights(
                raw_path.to_string_lossy().into_owned(),
                owner_rights(Some("owner-full-rights".into())),
                app.state(),
            )
            .expect("declare rights for a real recording");
            assert_eq!(covered, 1, "the one segment cut from this recording is covered");

            let revoked = revoke_recording_consent(raw_path.to_string_lossy().into_owned(), app.state())
                .expect("withdrawal stamps every clip of the recording");
            assert_eq!(revoked, 1);

            let rows = list_recording_rights(app.state()).expect("list recordings");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["audioPath"], serde_json::json!(validated));
            assert_eq!(rows[0]["segmentCount"], serde_json::json!(1));
            assert_eq!(rows[0]["disposition"], serde_json::json!("Revoked"), "revocation outranks the declaration");
        }

        #[test]
        fn retired_segment_write_endpoints_refuse_through_state_without_mutation() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            seed_state_clip(&app, tmp.path(), "retired-state");

            let whole_row =
                update_segment(SpeechSegment { id: "retired-state".into(), ..SpeechSegment::default() }, app.state())
                    .expect_err("the whole-row writer stays retired at the IPC boundary");
            assert!(whole_row.contains("retired"), "{whole_row}");

            let restore = restore_segment_snapshot(
                SpeechSegment { id: "retired-state".into(), ..SpeechSegment::default() },
                app.state(),
            )
            .expect_err("renderer-owned whole-row restore is disabled");
            assert!(restore.contains("disabled"), "{restore}");

            let legacy = record_human_decision(
                app.state(),
                "retired-state".into(),
                "accept".into(),
                Some("دەق".into()),
                Some(1_700_000_000_001),
                "eeee0000-0000-4000-8000-000000000001".into(),
            )
            .expect_err("the legacy decision boundary is retired");
            assert!(legacy.starts_with("TYPED_REVIEW_REQUIRED:"), "{legacy}");

            let undo = undo_human_decision(app.state(), 1, "eeee0000-0000-4000-8000-000000000002".into())
                .expect_err("the identity-free undo boundary is retired");
            assert!(undo.starts_with("TYPED_UNDO_REQUIRED:"), "{undo}");

            // The retained scalar playback endpoint still validates and bounds before it refuses.
            let negative = record_playback_receipt(app.state(), "retired-state".into(), -1, 10_000, None, None, 0)
                .expect_err("negative playback durations are refused");
            assert!(negative.contains("must not be negative"), "{negative}");
            let unbounded = record_playback_receipt(
                app.state(),
                "retired-state".into(),
                1_000,
                10_000,
                None,
                Some("s".repeat(129)),
                0,
            )
            .expect_err("the receipt session identity must stay bounded");
            assert!(unbounded.contains("Session"), "{unbounded}");
            let refused = record_playback_receipt(
                app.state(),
                "retired-state".into(),
                9_000,
                10_000,
                Some("reviewer-a".into()),
                Some("session-1".into()),
                0,
            )
            .expect_err("a raw scalar can never mint policy-4 evidence");
            assert!(refused.starts_with("PLAYBACK_SESSION_REQUIRED:"), "{refused}");

            let invalid = clear_human_decision(app.state(), "bad id!".into()).expect_err("identifier gate first");
            assert_eq!(invalid, "Identifier must be alphanumeric (underscore, hyphen, dot allowed)");
            let disabled = clear_human_decision(app.state(), "retired-state".into())
                .expect_err("identity-free decision clearing is disabled in the database boundary");
            assert!(disabled.contains("clear_human_decision is disabled"), "{disabled}");

            let row = app.state::<AppState>().lock_db().get_segment_by_id("retired-state").unwrap().unwrap();
            assert_eq!(row.raw_transcript, "دەق");
            assert!(row.human_decision.is_none() && !row.verified, "no retired endpoint may have written truth");
        }

        #[test]
        fn update_segment_metadata_v1_refuses_invalid_missing_and_stale_then_saves() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            seed_state_clip(&app, tmp.path(), "meta-clip");

            let invalid = update_segment_metadata_v1(
                UpdateSegmentMetadataRequestV1 { segment_id: "bad id!".into(), changes: Vec::new() },
                app.state(),
            )
            .expect_err("identity gate first");
            assert_eq!(invalid.code, "INVALID_SEGMENT_ID");

            let empty = update_segment_metadata_v1(
                UpdateSegmentMetadataRequestV1 { segment_id: "meta-clip".into(), changes: Vec::new() },
                app.state(),
            )
            .expect_err("a change-free request is invalid");
            assert_eq!(empty.code, "INVALID_SEGMENT_METADATA");

            let missing = update_segment_metadata_v1(
                UpdateSegmentMetadataRequestV1 {
                    segment_id: "meta-missing".into(),
                    changes: vec![SegmentMetadataChangeV1::SpeakerId {
                        expected: None,
                        value: Some("spk-state".into()),
                    }],
                },
                app.state(),
            )
            .expect_err("an unknown segment refuses");
            assert_eq!(missing.code, "SEGMENT_NOT_FOUND");

            let saved = update_segment_metadata_v1(
                UpdateSegmentMetadataRequestV1 {
                    segment_id: "meta-clip".into(),
                    changes: vec![SegmentMetadataChangeV1::SpeakerId {
                        expected: None,
                        value: Some("spk-state".into()),
                    }],
                },
                app.state(),
            )
            .expect("compare-and-set save");
            assert!(saved.changed);
            assert_eq!(saved.segment_id, "meta-clip");
            assert_eq!(saved.speaker_id.as_deref(), Some("spk-state"));

            let stale = update_segment_metadata_v1(
                UpdateSegmentMetadataRequestV1 {
                    segment_id: "meta-clip".into(),
                    changes: vec![SegmentMetadataChangeV1::SpeakerId {
                        expected: None,
                        value: Some("must-not-clobber".into()),
                    }],
                },
                app.state(),
            )
            .expect_err("a stale expectation conflicts instead of overwriting");
            assert_eq!(stale.code, "STALE_SEGMENT_METADATA");
            assert_eq!(
                app.state::<AppState>()
                    .lock_db()
                    .get_segment_by_id("meta-clip")
                    .unwrap()
                    .unwrap()
                    .speaker_id
                    .as_deref(),
                Some("spk-state"),
                "the conflicting save must leave the newer value in place"
            );
        }

        #[test]
        fn delete_segments_v1_bounds_ids_deletes_and_replays_idempotently() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            seed_state_clip(&app, tmp.path(), "del-a");
            seed_state_clip(&app, tmp.path(), "del-b");

            let empty = delete_segments_v1(DeleteSegmentsRequestV1 { ids: Vec::new() }, app.state())
                .expect_err("zero ids is invalid");
            assert_eq!(empty.code, "INVALID_DELETE_REQUEST");

            let invalid = delete_segments_v1(DeleteSegmentsRequestV1 { ids: vec!["bad id!".into()] }, app.state())
                .expect_err("identifier gate");
            assert_eq!(invalid.code, "INVALID_SEGMENT_ID");

            let deleted =
                delete_segments_v1(DeleteSegmentsRequestV1 { ids: vec!["del-a".into(), "del-b".into()] }, app.state())
                    .expect("batch delete");
            assert_eq!(deleted.requested_count, 2);
            assert_eq!(deleted.deleted_count, 2);
            assert!(app.state::<AppState>().lock_db().get_segment_by_id("del-a").unwrap().is_none());

            let replay =
                delete_segments_v1(DeleteSegmentsRequestV1 { ids: vec!["del-a".into(), "del-b".into()] }, app.state())
                    .expect("a lost-response replay proves the requested final state");
            assert_eq!(replay.requested_count, 2);
            assert_eq!(replay.deleted_count, 0);
        }

        #[test]
        fn review_draft_commands_reserve_save_read_and_delete_through_state() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            let base_revision = seed_state_clip(&app, tmp.path(), "draft-clip");

            assert_eq!(
                reserve_review_draft_write_v1(app.state(), "bad id!".into(), "op-1".into()).unwrap_err().code,
                "INVALID_SEGMENT_ID"
            );
            assert_eq!(
                reserve_review_draft_write_v1(app.state(), "draft-clip".into(), "op !".into()).unwrap_err().code,
                "INVALID_OPERATION_ID"
            );

            assert_eq!(get_review_draft_v1(app.state(), "bad id!".into()).unwrap_err().code, "INVALID_SEGMENT_ID");
            assert!(get_review_draft_v1(app.state(), "draft-clip".into()).expect("no draft yet").is_none());

            assert_eq!(
                save_review_draft_v1(app.state(), "draft-clip".into(), -1, "x".into(), "op-save".into())
                    .unwrap_err()
                    .code,
                "INVALID_REVIEW_REVISION"
            );
            assert_eq!(
                save_review_draft_v1(
                    app.state(),
                    "draft-clip".into(),
                    base_revision,
                    "x".repeat(100_001),
                    "op-save".into(),
                )
                .unwrap_err()
                .code,
                "INVALID_REVIEW_DRAFT"
            );

            reserve_review_draft_write_v1(app.state(), "draft-clip".into(), "op-save".into()).expect("reserve save");
            let saved = save_review_draft_v1(
                app.state(),
                "draft-clip".into(),
                base_revision,
                "نیوە کار".into(),
                "op-save".into(),
            )
            .expect("durable draft save");
            assert_eq!(saved.segment_id, "draft-clip");
            assert_eq!(saved.base_revision, base_revision);
            assert_eq!(saved.text, "نیوە کار");

            let loaded = get_review_draft_v1(app.state(), "draft-clip".into()).expect("read").expect("draft exists");
            assert_eq!(loaded.text, "نیوە کار");

            assert_eq!(
                delete_review_draft_v1(app.state(), "draft-clip".into(), -1, "op-del".into()).unwrap_err().code,
                "INVALID_REVIEW_REVISION"
            );
            reserve_review_draft_write_v1(app.state(), "draft-clip".into(), "op-del".into()).expect("reserve delete");
            assert!(delete_review_draft_v1(app.state(), "draft-clip".into(), base_revision, "op-del".into())
                .expect("revision-guarded delete"));
            assert!(get_review_draft_v1(app.state(), "draft-clip".into()).expect("read after delete").is_none());
        }

        #[test]
        fn commit_review_v1_commits_policy4_truth_and_refuses_bad_requests_through_state() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            seed_state_clip(&app, tmp.path(), "state-commit");
            // Minting the policy-4 receipt rewrites the row's canonical audio hash, which advances
            // review_revision. Read the revision AFTER arming, exactly as production sends the
            // served revision from the fresh review-queue payload (same order as the parent
            // module's typed_review_commit test).
            let (playback_receipt_id, base_revision) = {
                let state = app.state::<AppState>();
                let db = state.lock_db();
                let playback_receipt_id = exact_policy4_receipt(&db, "state-commit", 9_000);
                let base_revision = db.segment_review_revision("state-commit").unwrap().unwrap();
                (playback_receipt_id, base_revision)
            };

            let invalid = commit_review_v1(
                app.state(),
                CommitReviewRequestV1 {
                    operation_id: "aaaa1111-1111-4111-8111-111111111111".into(),
                    segment_id: "bad id!".into(),
                    base_revision,
                    decision: ReviewDecisionV1::Accept,
                    transcript: Some("دەق".into()),
                    reason_code: None,
                    playback_receipt_id: playback_receipt_id.clone(),
                },
            )
            .expect_err("identity gate");
            assert_eq!(invalid.code, "INVALID_REVIEW_REQUEST");

            let reason = commit_review_v1(
                app.state(),
                CommitReviewRequestV1 {
                    operation_id: "aaaa2222-2222-4222-8222-222222222222".into(),
                    segment_id: "state-commit".into(),
                    base_revision,
                    decision: ReviewDecisionV1::Reject,
                    transcript: None,
                    reason_code: Some("noise".into()),
                    playback_receipt_id: playback_receipt_id.clone(),
                },
            )
            .expect_err("structured unusable reasons are not persistable in this release");
            assert_eq!(reason.code, "REASON_CODE_NOT_SUPPORTED");

            let committed = commit_review_v1(
                app.state(),
                CommitReviewRequestV1 {
                    operation_id: "aaaa3333-3333-4333-8333-333333333333".into(),
                    segment_id: "state-commit".into(),
                    base_revision,
                    decision: ReviewDecisionV1::Accept,
                    transcript: Some("دەق".into()),
                    reason_code: None,
                    playback_receipt_id,
                },
            )
            .expect("typed accept through the full state boundary");
            assert_eq!(committed.segment_id, "state-commit");
            assert!(committed.committed_revision > base_revision, "the commit must advance review truth");
            assert_eq!(committed.authoritative_transcript, "دەق");
            assert!(committed.decision_id.starts_with("effect:"));
            let row = app.state::<AppState>().lock_db().get_segment_by_id("state-commit").unwrap().unwrap();
            assert_eq!(row.human_decision.as_deref(), Some("accept"));
            assert!(row.verified);
        }

        #[test]
        fn mark_segment_unusable_v1_seals_reproduced_corruption_through_state() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            let base_revision = seed_state_clip(&app, tmp.path(), "state-unusable");
            std::fs::write(tmp.path().join("state-unusable.wav"), b"not an audio container").unwrap();

            let invalid = mark_segment_unusable_v1(
                app.state(),
                MarkSegmentUnusableRequestV1 {
                    operation_id: "bbbb1111-1111-4111-8111-111111111111".into(),
                    segment_id: "bad id!".into(),
                    base_revision,
                    reason: TechnicalUnusableReasonV1::CorruptContainer,
                },
            )
            .expect_err("identity gate");
            assert_eq!(invalid.code, "INVALID_MARK_UNUSABLE_REQUEST");

            let marked = mark_segment_unusable_v1(
                app.state(),
                MarkSegmentUnusableRequestV1 {
                    operation_id: "bbbb2222-2222-4222-8222-222222222222".into(),
                    segment_id: "state-unusable".into(),
                    base_revision,
                    reason: TechnicalUnusableReasonV1::CorruptContainer,
                },
            )
            .expect("a reproduced corrupt container seals the technical flag");
            assert_eq!(marked.segment_id, "state-unusable");
            assert_eq!(marked.committed_revision, base_revision + 1);
            assert_eq!(marked.reason, TechnicalUnusableReasonV1::CorruptContainer);
            assert!(marked.effect_id.starts_with("flag-effect:"));
            let row = app.state::<AppState>().lock_db().get_segment_by_id("state-unusable").unwrap().unwrap();
            assert!(crate::quality::is_technically_unusable(&row));
            assert!(row.human_decision.is_none(), "a technical failure is never human truth");
        }

        #[test]
        fn record_review_flag_commits_and_refuses_non_uuid_operations_through_state() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            let base_revision = seed_state_clip(&app, tmp.path(), "state-flag");

            let invalid = record_review_flag(
                app.state(),
                RecordReviewFlagRequestV1 {
                    operation_id: "not-a-uuid".into(),
                    segment_id: "state-flag".into(),
                    base_revision,
                    rationale: "Needs a second listen".into(),
                },
            )
            .expect_err("flag idempotency requires a canonical UUID");
            assert_eq!(invalid.code, "INVALID_REVIEW_FLAG_REQUEST");

            let committed = record_review_flag(
                app.state(),
                RecordReviewFlagRequestV1 {
                    operation_id: "cccc1111-1111-4111-8111-111111111111".into(),
                    segment_id: "state-flag".into(),
                    base_revision,
                    rationale: "Needs a second listen".into(),
                },
            )
            .expect("generic owner flag");
            assert_eq!(committed.segment_id, "state-flag");
            assert_eq!(committed.prior_revision, base_revision);
            assert_eq!(committed.flag_revision, base_revision + 1);
            assert!(committed.segment.escalated);
        }

        #[test]
        fn desktop_undo_commands_discover_and_apply_the_flag_inverse_through_state() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());
            let base_revision = seed_state_clip(&app, tmp.path(), "state-undo-flag");

            let fresh = tauri::async_runtime::block_on(get_desktop_review_undo_target_v1(app.state()))
                .expect("empty history read");
            assert!(matches!(fresh, DesktopReviewUndoAvailabilityV1::None));

            app.state::<AppState>()
                .review_writes()
                .record_flag(
                    "state-undo-flag",
                    base_revision,
                    "Undo me exactly once",
                    "dddd1111-1111-4111-8111-111111111111",
                )
                .expect("seed a durable flag effect");

            let target = available_undo_target(
                tauri::async_runtime::block_on(get_desktop_review_undo_target_v1(app.state()))
                    .expect("restart-safe target read"),
            );

            let forged = tauri::async_runtime::block_on(undo_desktop_review_action_v1(
                app.state(),
                exact_undo_request(&target, "not-a-uuid"),
            ))
            .expect_err("a non-UUID inverse identity must be refused");
            assert_eq!(forged.code, "INVALID_UNDO_REQUEST");
            assert!(app.state::<AppState>().lock_db().get_segment_by_id("state-undo-flag").unwrap().unwrap().escalated);

            let applied = tauri::async_runtime::block_on(undo_desktop_review_action_v1(
                app.state(),
                exact_undo_request(&target, "dddd2222-2222-4222-8222-222222222222"),
            ))
            .expect("exact typed inverse applies");
            assert!(matches!(applied, DesktopReviewUndoOutcomeV1::Applied { .. }));
            assert!(
                !app.state::<AppState>().lock_db().get_segment_by_id("state-undo-flag").unwrap().unwrap().escalated,
                "the applied inverse must clear the escalation"
            );
        }

        #[test]
        fn playback_session_commands_validate_and_refuse_unknown_authority_through_state() {
            let tmp = tempfile::tempdir().unwrap();
            let app = managed_app_state(tmp.path());

            assert_eq!(
                begin_desktop_playback_session_v1(app.state(), "bad id!".into(), "grant-1".into(), 0, "att-1".into())
                    .unwrap_err()
                    .code,
                "INVALID_SEGMENT_ID"
            );
            assert_eq!(
                begin_desktop_playback_session_v1(app.state(), "seg-1".into(), "bad grant!".into(), 0, "att-1".into())
                    .unwrap_err()
                    .code,
                "INVALID_MEDIA_GRANT"
            );
            assert_eq!(
                begin_desktop_playback_session_v1(app.state(), "seg-1".into(), "grant-1".into(), 0, "bad att!".into())
                    .unwrap_err()
                    .code,
                "INVALID_PLAYBACK_ATTEMPT"
            );
            assert_eq!(
                begin_desktop_playback_session_v1(app.state(), "seg-1".into(), "grant-1".into(), -1, "att-1".into())
                    .unwrap_err()
                    .code,
                "INVALID_REVIEW_REVISION"
            );
            let expired = begin_desktop_playback_session_v1(
                app.state(),
                "seg-1".into(),
                uuid::Uuid::new_v4().to_string(),
                0,
                "att-1".into(),
            )
            .expect_err("a grant the registry never issued cannot begin playback");
            assert_eq!(expired.code, "PLAYBACK_SESSION_EXPIRED");

            let proof_failed = cancel_desktop_playback_session_v1(app.state(), "seg-receipt".into(), "att-1".into())
                .expect_err("a non-UUID receipt identity fails the database boundary");
            assert_eq!(proof_failed.code, "PLAYBACK_PROOF_FAILED");
            assert!(
                !cancel_desktop_playback_session_v1(
                    app.state(),
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                )
                .expect("cancelling an authority that no longer exists is an idempotent no-op"),
                "nothing durable was retired"
            );

            assert_eq!(
                finalize_desktop_playback_session_v1(app.state(), "bad receipt!".into(), "grant-1".into(), Vec::new())
                    .unwrap_err()
                    .code,
                "INVALID_PLAYBACK_RECEIPT"
            );
            assert_eq!(
                finalize_desktop_playback_session_v1(
                    app.state(),
                    uuid::Uuid::new_v4().to_string(),
                    "bad grant!".into(),
                    Vec::new(),
                )
                .unwrap_err()
                .code,
                "INVALID_MEDIA_GRANT"
            );
            let unavailable = finalize_desktop_playback_session_v1(
                app.state(),
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                vec![PlaybackIntervalV1 { start_ms: 0, end_ms: 1_000 }],
            )
            .expect_err("no committed receipt and no live grant is a proven non-commit");
            assert_eq!(unavailable.code, "PLAYBACK_MEDIA_GRANT_UNAVAILABLE");
            assert!(unavailable.retryable);
        }
    }
}
