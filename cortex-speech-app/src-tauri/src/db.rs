use crate::error::{AppError, AppResult};
use crate::fingerprint::{AudioIdentity, StoredAudioIdentity};
use crate::normalizer::learning_text_key;

/// Which background pass's backlog [`Database::get_pending_segments`] should return.
///
/// An enum rather than a caller-supplied SQL fragment: the three predicates are fixed, and a
/// `&str` parameter here would be an injection-shaped API for no gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingWork {
    /// No real transcript yet — empty or an ASR placeholder. The 7B refinement driver's targets.
    Transcript,
    /// No forced-alignment CTC score yet.
    CtcScore,
    /// No signal-anomaly score yet.
    SignalAnomaly,
}

use base64::Engine as _;
use rusqlite::{backup, params, types::Value, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use unicode_normalization::UnicodeNormalization;

/// One row of the `jobs` table as read: (id, kind, state, progress, completed, total, error_code).
type JobRow = (String, String, String, f64, i64, Option<i64>, Option<String>);

const REVIEW_AUTHORITY_DELETE_ABORT: &str = "segment with durable review authority cannot be deleted";

fn map_segment_delete_error(error: rusqlite::Error) -> AppError {
    if error.to_string().contains(REVIEW_AUTHORITY_DELETE_ABORT) {
        AppError::Validation(format!(
            "segment deletion refused: {REVIEW_AUTHORITY_DELETE_ABORT}; reviewed clips and their evidence are append-only"
        ))
    } else {
        AppError::Database(error)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SpeechSegment {
    pub id: String,
    pub created_at: Option<String>,
    pub audio_path: String,
    pub raw_transcript: String,
    pub normalized_transcript: Option<String>,
    pub annotated_transcript: Option<String>,
    pub alignment_json: Option<String>,
    pub duration_ms: i64,
    pub speaker_id: Option<String>,
    pub verified: bool,
    pub confidence: Option<f64>,
    pub ctc_score: Option<f64>,
    pub clipping_ratio: Option<f64>,
    pub rms_db: Option<f64>,
    pub snr_db: Option<f64>,
    pub split: Option<String>,
    pub signal_anomaly_score: Option<f64>,
    // ── Jury fields (Migration v11) ────────────────────────────────
    /// NULL = unprocessed; "auto_accept" | "jury_accept" | "jury_edit"
    /// | "escalated" | "human_accept" | "human_edit" | "human_reject"
    pub verdict: Option<String>,
    pub verdict_transcript: Option<String>,
    pub rationale: Option<String>,
    pub evidence_json: Option<String>,
    pub agreement_score: Option<f64>,
    pub escalated: bool,
    pub human_decision: Option<String>,
    pub corrected_at: Option<String>,
    pub is_gold: bool,
    // ── Alignment quality (Migration v12) ─────────────────────────
    /// "ctc_forced" | "energy_heuristic" | None (never aligned)
    pub alignment_quality: Option<String>,
    /// Existing model registry id (Migration v22), e.g. "omniasr-ctc-300m".
    pub model_version_id: Option<String>,
    /// "real_posterior" | "heuristic" | provider-specific value. Heuristic confidence is not calibrated.
    pub confidence_source: Option<String>,
    /// Whether producing this segment transcript involved sending audio/transcript to a cloud provider.
    pub cloud_call: bool,
    /// Hash of decoder/runtime settings that materially affect the transcript.
    pub decoder_config_hash: Option<String>,
    /// Producer version for `normalized_transcript`: the Sorani normalizer, or the explicitly
    /// versioned refinement/LOOP0 review projection when normalization is disabled. The column name
    /// is historical; a non-null derived transcript and producer marker always travel together.
    pub normalizer_version: Option<String>,
    // ── Per-segment processing provenance (Migration v41, P0.4) ────
    /// Whether the denoiser ACTUALLY ran for this segment at import (`settings.enable_denoising` AND the
    /// denoiser model was loadable). `None` = not recorded (legacy row imported before v41). Lets an
    /// export report stored per-segment truth instead of recomputing from export-day model state (H3).
    pub denoised: Option<bool>,
    /// Whether diarization ACTUALLY ran for this segment at import (`settings.enable_diarization` AND the
    /// CAM++ speaker-embedding model was loadable). `None` = not recorded (legacy row). Distinct from
    /// `speaker_id`, which can be a filename hint even when diarization did not run.
    pub diarized: Option<bool>,
    /// Which VAD backend ACTUALLY produced this segment's speech region (Migration v42): "silero",
    /// "energy" (fallback), or "none" (short file taken whole, no VAD). `None` = not recorded (legacy row
    /// / cloud Scribe path). Surfaced from the detector at import, never a path-exists probe.
    pub vad_backend: Option<String>,
    // ── Reviewer attribution (Migration v43) ───────────────────────
    /// WHICH human made this row's current decision — a named Couch Review reviewer. `None` = not
    /// attributed: a legacy pre-v43 row, an undecided row, or a decision made at the owner's own
    /// desktop (one human, no token to name them). Written in the same transaction as the verdict by
    /// `record_human_decision_by`, and cleared by `clear_human_decision` along with the decision itself.
    pub reviewed_by: Option<String>,
    // ── Speaker-change measurement (Migration v47) ─────────────────
    /// Cosine similarity between CAM++ embeddings of this clip's FIRST and SECOND half. Low means the
    /// two halves are different people, i.e. the clip spans a turn — which `speaker_id` cannot express,
    /// since one label is attached to the whole chunk however many people are in it.
    ///
    /// `None` = NOT MEASURED, never "measured, one speaker": every pre-v47 row and every import (the
    /// import path does not run it). Filled by `src/bin/speaker_change_probe.rs --persist`. Compare
    /// against [`crate::diarization::SPEAKER_CHANGE_THRESHOLD`], where the calibration is documented.
    pub speaker_change_score: Option<f64>,
    /// Read-only export projection of the final pool decision, not a stored first opinion.
    /// IPC/import JSON cannot carry this field; database reads always initialize it to None.
    /// Dataset serializers explicitly include it through their export-only record.
    #[serde(skip)]
    #[specta(skip)]
    pub export_review: Option<crate::export_review::ExportReviewAuthority>,
}

/// Minimal server-owned inverse for a speaker metadata change. Keeping only the changed column and
/// segment identity makes even a large batch undo bounded without retaining duplicate transcripts,
/// paths, or review evidence in memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerAssignmentChange {
    pub segment_id: String,
    pub previous_speaker_id: Option<String>,
    pub current_speaker_id: Option<String>,
}

/// One atomic, server-authored human adjudication and its exact inverse identity.
///
/// The renderer receives the authoritative post-commit row; it never supplies the snapshot used by
/// Undo.  `effect_event_id` binds every learning/pay side effect produced by this decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanDecisionCommit {
    pub effect_event_id: i64,
    pub segment_id: String,
    pub effective_action: String,
    pub prior_revision: i64,
    pub decided_revision: i64,
    pub segment: SpeechSegment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum HumanDecisionUndoOutcome {
    #[serde(rename = "applied")]
    Applied {
        #[serde(rename = "restoredRevision")]
        restored_revision: i64,
        segment: SpeechSegment,
    },
    #[serde(rename = "alreadyApplied")]
    AlreadyApplied {
        #[serde(rename = "restoredRevision")]
        restored_revision: i64,
        segment: SpeechSegment,
    },
    #[serde(rename = "conflict")]
    Conflict { segment: SpeechSegment },
}

/// Immutable identity of the one desktop decision currently eligible for exact Undo. Every field
/// is rechecked inside the inverse transaction so a restore cannot make a reused integer row id
/// authorize a different segment or decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopHumanDecisionUndoAuthority {
    pub effect_event_id: i64,
    pub segment_id: String,
    pub action: String,
    pub decision_operation_id: String,
    pub decision_payload_hash: String,
}

/// Closed renderer-visible classification for a review flag. The structured technical rationale
/// contains source fingerprints and remains database-private; only its audited reason code crosses
/// the IPC boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopReviewFlagKind {
    Generic,
    TechnicalUnusable(String),
}

/// Immutable identity of the one desktop flag currently eligible for exact Undo. The payload hash
/// binds the complete database rationale without exposing technical source fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopReviewFlagUndoAuthority {
    pub effect_event_id: i64,
    pub segment_id: String,
    pub flag_operation_id: String,
    pub prior_revision: i64,
    pub flag_revision: i64,
    pub flag_payload_hash: String,
    pub flag_kind: DesktopReviewFlagKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopReviewUndoAuthority {
    Decision(DesktopHumanDecisionUndoAuthority),
    Flag(DesktopReviewFlagUndoAuthority),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopReviewUndoBlockReason {
    LegacyHistory,
    LatestDecisionUndone,
    LatestFlagUndone,
    DecisionShadowed,
    FlagShadowed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopReviewUndoAvailability {
    NoHistory,
    Blocked(DesktopReviewUndoBlockReason),
    Available(DesktopReviewUndoAuthority),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanFlagCommit {
    pub effect_event_id: i64,
    pub segment_id: String,
    pub prior_revision: i64,
    pub flag_revision: i64,
    pub segment: SpeechSegment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum HumanFlagUndoOutcome {
    #[serde(rename = "applied")]
    Applied {
        #[serde(rename = "restoredRevision")]
        restored_revision: i64,
        segment: Box<SpeechSegment>,
    },
    #[serde(rename = "alreadyApplied")]
    AlreadyApplied,
    #[serde(rename = "conflict")]
    Conflict,
}

#[derive(Debug)]
struct DecisionEffectSnapshot {
    review_event_id: Option<i64>,
    segment_id: String,
    reviewer: Option<String>,
    source: String,
    operation_id: Option<String>,
    operation_payload_hash: Option<String>,
    action: String,
    decision_transcript: Option<String>,
    decision_annotated_transcript: Option<String>,
    decision_verified: bool,
    decision_corrected_at: String,
    decision_rationale: Option<String>,
    prior_revision: i64,
    decision_revision: i64,
    prior_verified: bool,
    prior_annotated_transcript: Option<String>,
    prior_verdict: Option<String>,
    prior_verdict_transcript: Option<String>,
    prior_rationale: Option<String>,
    prior_escalated: bool,
    prior_human_decision: Option<String>,
    prior_corrected_at: Option<String>,
    prior_reviewed_by: Option<String>,
}

#[derive(Debug)]
struct DesktopReplayEffect {
    id: i64,
    segment_id: String,
    source: String,
    reviewer: Option<String>,
    operation_payload_hash: String,
    action: String,
    decision_transcript: Option<String>,
    decision_annotated_transcript: Option<String>,
    decision_verified: bool,
    decision_corrected_at: String,
    decision_rationale: Option<String>,
    requested_action: String,
    requested_transcript: Option<String>,
    requested_timestamp_ms: i64,
    prior_revision: i64,
    decision_revision: i64,
    prior_verdict_transcript: Option<String>,
    desktop_review_contract_version: Option<i64>,
    playback_authority_session_id: Option<String>,
}

#[derive(Debug)]
struct FlagEffectSnapshot {
    segment_id: String,
    operation_id: String,
    prior_revision: i64,
    flag_revision: i64,
    prior_verdict: Option<String>,
    prior_rationale: Option<String>,
    flag_rationale: String,
    prior_escalated: bool,
}

/// One reviewer's measured throughput, from the append-only `review_events` trail (Migration v45).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerThroughput {
    pub reviewer: String,
    /// DISTINCT clips they decided. Counting rows instead would let a network retry inflate it.
    pub clips: usize,
    /// Median seconds between their consecutive decisions, computed WITHIN this reviewer's own
    /// stream. `None` until they have two decisions close enough together to time.
    pub median_seconds: Option<f64>,
    /// How many gaps that median is drawn from — a median over two samples is not a rate.
    pub samples: usize,
}

/// Owner-authorized reviewer compensation policy (2026-08-21).
///
/// Money is deliberately NOT derived from `reviewed_audio_ms`: that is full activity progress,
/// while compensation is action-weighted.  Integer micro-IQD keeps every millisecond exact at this
/// rate (edit = 5,000 micro-IQD/ms; accept/reject = 500) and postpones whole-IQD rounding until an
/// actual settlement is produced.
pub const REVIEW_PAY_POLICY_VERSION: &str = "review-iqd-v1-2026-08-21";
pub const REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR: i64 = 18_000_000_000;
pub const REVIEW_PAY_EDIT_BPS: i64 = 10_000;
pub const REVIEW_PAY_ACCEPT_BPS: i64 = 1_000;
pub const REVIEW_PAY_REJECT_BPS: i64 = 1_000;
pub const REVIEW_PAY_SKIP_BPS: i64 = 0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCompensationSummary {
    pub policy_version: String,
    /// Exact signed total under the active policy. Divide only for display; never round per clip.
    pub earned_micro_iqd: i64,
    /// Full-equivalent duration whose currently active semantic action is `edit` (100%).
    /// This is correction work, distinct from all judged activity and from money.
    pub corrected_audio_ms: i64,
    /// Exact ledger credit already allocated to immutable external payout references.
    pub settled_micro_iqd: i64,
    /// Exact earned credit not yet allocated. May be negative after a post-settlement reversal and
    /// therefore must be carried into the next settlement rather than hidden.
    pub outstanding_micro_iqd: i64,
    /// Pre-policy events remain outside this total until separately reconciled and authorized.
    pub legacy_events_pending_reconciliation: usize,
    /// Entries that had to fall back to row identity because canonical audio identity was absent.
    /// The production readiness gate requires zero inside an active paid campaign.
    pub fallback_identity_entries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCompensationSettlement {
    pub settlement_id: String,
    pub policy_version: String,
    pub reviewer: String,
    pub from_ledger_id_exclusive: i64,
    pub through_ledger_id_inclusive: i64,
    pub allocated_micro_iqd: i64,
    pub payout_reference: String,
}

/// Durable receipt for one client-authored review operation. The payload hash lets the HTTP layer
/// distinguish a safe retry from accidental/malicious reuse of the same UUID for different work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewOperationReceipt {
    pub operation_id: String,
    pub operation_payload_hash: String,
    pub review_event_id: i64,
    pub segment_id: String,
    pub reviewer: String,
    pub action: String,
    pub compensation_action: String,
}

/// Database-enforced boundary for one controlled Couch pilot.
///
/// The HTTP layer also narrows queues for usability, but this object enters the SAME immediate
/// transaction as the verdict and pay-ledger append. That is the authority: two reviewer threads or
/// two processes racing the final slot cannot both commit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDecisionLimit {
    after_review_event_id: i64,
    max_total_review_actions: i64,
    reviewer_caps: Vec<(String, i64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDecisionProgress {
    pub total_review_actions: i64,
    pub by_reviewer: HashMap<String, i64>,
}

pub const REVIEW_PILOT_LIMIT_REACHED: &str = "E_REVIEW_PILOT_LIMIT_REACHED";

impl ReviewDecisionLimit {
    pub fn new(
        after_review_event_id: i64,
        max_total_review_actions: i64,
        reviewer_caps: Vec<(String, i64)>,
    ) -> AppResult<Self> {
        if after_review_event_id < 0 || max_total_review_actions <= 0 || reviewer_caps.is_empty() {
            return Err(AppError::Validation("invalid controlled-review decision limit".into()));
        }
        let mut canonical: Vec<(String, i64)> = Vec::with_capacity(reviewer_caps.len());
        let mut sum = 0_i64;
        for (raw_name, cap) in reviewer_caps {
            let name = raw_name.trim();
            if name.is_empty() || cap <= 0 {
                return Err(AppError::Validation("invalid controlled-review reviewer limit".into()));
            }
            if canonical.iter().any(|(existing, _)| existing.eq_ignore_ascii_case(name)) {
                return Err(AppError::Validation("duplicate controlled-review reviewer".into()));
            }
            sum =
                sum.checked_add(cap).ok_or_else(|| AppError::Validation("controlled-review limits overflow".into()))?;
            canonical.push((name.to_string(), cap));
        }
        if sum != max_total_review_actions {
            return Err(AppError::Validation("controlled-review reviewer limits must sum to the total limit".into()));
        }
        canonical.sort_by_key(|(name, _)| name.to_ascii_lowercase());
        Ok(Self { after_review_event_id, max_total_review_actions, reviewer_caps: canonical })
    }

    pub fn after_review_event_id(&self) -> i64 {
        self.after_review_event_id
    }

    pub fn max_total_review_actions(&self) -> i64 {
        self.max_total_review_actions
    }

    pub fn reviewer_names(&self) -> Vec<String> {
        self.reviewer_caps.iter().map(|(name, _)| name.clone()).collect()
    }

    pub fn cap_for(&self, reviewer: &str) -> Option<i64> {
        self.reviewer_caps.iter().find(|(name, _)| name.eq_ignore_ascii_case(reviewer.trim())).map(|(_, cap)| *cap)
    }
}

fn review_decision_progress_on(conn: &Connection, limit: &ReviewDecisionLimit) -> AppResult<ReviewDecisionProgress> {
    let mut by_reviewer: HashMap<String, i64> = limit.reviewer_caps.iter().map(|(name, _)| (name.clone(), 0)).collect();
    let mut statement = conn.prepare(
        "SELECT reviewer, COUNT(*)
           FROM (
                SELECT reviewer
                  FROM effective_review_events_v60
                 WHERE review_event_id > ?1 AND source = 'couch'
                   AND action IN ('accept','edit','reject')
                UNION ALL
                SELECT reviewer
                  FROM review_events
                 WHERE id > ?1 AND source = 'couch' AND action = 'skip'
           ) counted_actions
          GROUP BY LOWER(TRIM(reviewer))",
    )?;
    let rows = statement
        .query_map(params![limit.after_review_event_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    let mut total = 0_i64;
    for row in rows {
        let (actual_name, count) = row?;
        let Some((canonical_name, cap)) =
            limit.reviewer_caps.iter().find(|(allowed, _)| allowed.eq_ignore_ascii_case(actual_name.trim()))
        else {
            return Err(AppError::Validation(format!(
                "controlled-review history contains unauthorized reviewer {actual_name:?}"
            )));
        };
        if count < 0 || count > *cap {
            return Err(AppError::Validation(format!(
                "controlled-review history exceeds the limit for {canonical_name}"
            )));
        }
        by_reviewer.insert(canonical_name.clone(), count);
        total = total
            .checked_add(count)
            .ok_or_else(|| AppError::Validation("controlled-review history count overflow".into()))?;
    }
    if total > limit.max_total_review_actions {
        return Err(AppError::Validation("controlled-review history exceeds the total limit".into()));
    }
    Ok(ReviewDecisionProgress { total_review_actions: total, by_reviewer })
}

fn enforce_review_action_limit_on(conn: &Connection, reviewer: &str, limit: &ReviewDecisionLimit) -> AppResult<()> {
    let current_max: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))?;
    if limit.after_review_event_id > current_max {
        return Err(AppError::Validation("controlled-review baseline is ahead of durable review history".into()));
    }
    let reviewer_cap = limit.cap_for(reviewer).ok_or_else(|| {
        AppError::Validation(format!("controlled-review policy does not authorize reviewer {reviewer:?}"))
    })?;
    let progress = review_decision_progress_on(conn, limit)?;
    let reviewer_count = progress
        .by_reviewer
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(reviewer.trim()))
        .map(|(_, count)| *count)
        .unwrap_or(0);
    if progress.total_review_actions >= limit.max_total_review_actions || reviewer_count >= reviewer_cap {
        return Err(AppError::Validation(format!(
            "{REVIEW_PILOT_LIMIT_REACHED}: controlled review pilot is complete for {reviewer}"
        )));
    }
    Ok(())
}

fn validate_review_operation_identity(operation_id: &str, payload_hash: &str) -> AppResult<()> {
    validate_operation_uuid(operation_id)?;
    if payload_hash.len() != 64
        || !payload_hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AppError::Validation("review operation payload hash must be canonical lowercase SHA-256".into()));
    }
    Ok(())
}

fn validate_operation_uuid(operation_id: &str) -> AppResult<()> {
    let parsed = uuid::Uuid::parse_str(operation_id)
        .map_err(|_| AppError::Validation("operation id must be a canonical UUID".into()))?;
    if parsed.hyphenated().to_string() != operation_id {
        return Err(AppError::Validation("operation id must be a lowercase hyphenated UUID".into()));
    }
    Ok(())
}

/// Server-derived idempotency digest for one desktop adjudication request.
///
/// Length framing makes the encoding unambiguous; NFC + trimming mirrors the persisted transcript
/// boundary. The renderer supplies only the UUID, so it cannot make two different requests claim
/// the same payload digest. This hash is request identity (SHA-256), not the decoded-PCM BLAKE3
/// audio identity stored in `speech_segments.audio_content_hash`.
pub(crate) fn desktop_decision_payload_hash(
    segment_id: &str,
    decision: &str,
    corrected_transcript: Option<&str>,
    timestamp_ms: Option<i64>,
) -> String {
    fn framed(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
    }

    let corrected = corrected_transcript.map(|text| to_nfc(text.trim())).filter(|text| !text.is_empty());
    let mut hash = Sha256::new();
    hash.update(b"cortex-desktop-human-decision-v1\0");
    framed(&mut hash, segment_id.as_bytes());
    framed(&mut hash, decision.as_bytes());
    match corrected.as_deref() {
        Some(text) => {
            hash.update([1]);
            framed(&mut hash, text.as_bytes());
        }
        None => hash.update([0]),
    }
    match timestamp_ms {
        Some(value) => {
            hash.update([1]);
            hash.update(value.to_be_bytes());
        }
        None => hash.update([0]),
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Idempotency digest for the versioned desktop review IPC.
///
/// Unlike the legacy desktop command, `CommitReviewRequestV1` intentionally has no client clock.
/// Its immutable compare-and-swap revision is a better request identity: it binds the operation to
/// the exact row the reviewer saw without trusting a renderer timestamp. The database still records
/// its own positive audit time, but that time is not part of the caller's replay payload.
pub(crate) fn desktop_review_v1_payload_hash(
    segment_id: &str,
    base_revision: i64,
    decision: &str,
    corrected_transcript: Option<&str>,
    playback_receipt_id: &str,
) -> String {
    fn framed(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
    }

    let corrected = corrected_transcript.map(|text| to_nfc(text.trim())).filter(|text| !text.is_empty());
    let mut hash = Sha256::new();
    hash.update(b"cortex-desktop-review-ipc-v1\0");
    framed(&mut hash, segment_id.as_bytes());
    hash.update(base_revision.to_be_bytes());
    framed(&mut hash, decision.as_bytes());
    framed(&mut hash, playback_receipt_id.as_bytes());
    match corrected.as_deref() {
        Some(text) => {
            hash.update([1]);
            framed(&mut hash, text.as_bytes());
        }
        None => hash.update([0]),
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Server-derived immutable identity for one desktop review flag. The exact rationale is included
/// because it is part of the effect's owned post-state, but only this digest is exposed to the
/// renderer (technical rationales embed private source fingerprints).
pub(crate) fn desktop_review_flag_payload_hash(segment_id: &str, prior_revision: i64, flag_rationale: &str) -> String {
    fn framed(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
    }

    let mut hash = Sha256::new();
    hash.update(b"cortex-desktop-review-flag-v1\0");
    framed(&mut hash, segment_id.as_bytes());
    hash.update(prior_revision.to_be_bytes());
    framed(&mut hash, flag_rationale.as_bytes());
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Historical typed-desktop digest used only to validate/read effects written before policy 4
/// persisted the exact authorizing receipt. New writes and new replays must use
/// `desktop_review_v1_payload_hash`, which includes that immutable authority ID.
fn legacy_desktop_review_v1_payload_hash(
    segment_id: &str,
    base_revision: i64,
    decision: &str,
    corrected_transcript: Option<&str>,
) -> String {
    fn framed(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
    }

    let corrected = corrected_transcript.map(|text| to_nfc(text.trim())).filter(|text| !text.is_empty());
    let mut hash = Sha256::new();
    hash.update(b"cortex-desktop-review-ipc-v1\0");
    framed(&mut hash, segment_id.as_bytes());
    hash.update(base_revision.to_be_bytes());
    framed(&mut hash, decision.as_bytes());
    match corrected.as_deref() {
        Some(text) => {
            hash.update([1]);
            framed(&mut hash, text.as_bytes());
        }
        None => hash.update([0]),
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Canonical phone/Couch operation payload. This is shared with the HTTP boundary and persisted
/// request snapshots so restore validation can rederive every post-v60 operation digest exactly.
pub(crate) fn review_operation_payload_hash(
    segment_id: &str,
    action: &str,
    transcript: &str,
    reviewer: &str,
) -> String {
    let transcript = to_nfc(transcript.trim());
    let mut hash = Sha256::new();
    hash.update(b"cortex-review-operation-v1");
    for value in [segment_id, action, transcript.as_str(), reviewer] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_review_pilot_policy_sha256(policy_sha256: &str) -> AppResult<()> {
    if policy_sha256.len() != 64
        || !policy_sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AppError::Validation("controlled-review policy digest must be canonical lowercase SHA-256".into()));
    }
    Ok(())
}

fn canonical_review_pilot_reviewer(reviewer: &str) -> AppResult<String> {
    let reviewer = reviewer.trim();
    if reviewer.is_empty() || reviewer.chars().count() > 40 || reviewer.chars().any(char::is_control) {
        return Err(AppError::Validation("invalid controlled-review pilot reviewer".into()));
    }
    Ok(reviewer.to_string())
}

fn validate_review_pilot_hidden_namespace(
    policy_sha256: &str,
    after_review_event_id: i64,
    reviewer: &str,
) -> AppResult<String> {
    validate_review_pilot_policy_sha256(policy_sha256)?;
    if after_review_event_id < 0 {
        return Err(AppError::Validation("controlled-review pilot baseline must be non-negative".into()));
    }
    canonical_review_pilot_reviewer(reviewer)
}

fn validate_review_pilot_hidden_segment_id(segment_id: &str) -> AppResult<()> {
    crate::validation::input::validate_identifier(segment_id).map_err(AppError::Validation)
}

fn review_pay_basis_points(action: &str) -> AppResult<i64> {
    match action {
        "edit" => Ok(REVIEW_PAY_EDIT_BPS),
        "accept" => Ok(REVIEW_PAY_ACCEPT_BPS),
        "reject" => Ok(REVIEW_PAY_REJECT_BPS),
        "skip" | "undo" => Ok(REVIEW_PAY_SKIP_BPS),
        other => Err(AppError::Validation(format!("unsupported compensation action {other:?}"))),
    }
}

fn review_pay_entitlement_micro_iqd(duration_ms: i64, basis_points: i64) -> AppResult<i64> {
    if duration_ms < 0 || !(0..=10_000).contains(&basis_points) {
        return Err(AppError::Validation("invalid compensation duration or basis points".into()));
    }
    let numerator = i128::from(duration_ms)
        .checked_mul(i128::from(REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR))
        .and_then(|value| value.checked_mul(i128::from(basis_points)))
        .ok_or_else(|| AppError::Other("review compensation arithmetic overflow".into()))?;
    let denominator = 3_600_000_i128 * 10_000_i128;
    if numerator % denominator != 0 {
        return Err(AppError::Other("review compensation policy does not produce exact integer micro-IQD".into()));
    }
    i64::try_from(numerator / denominator)
        .map_err(|_| AppError::Other("review compensation exceeds the supported integer range".into()))
}

/// A two-rater agreement sample, ready for `scripts/agreement_kappa.py`.
///
/// Cohen's kappa is a TWO-rater statistic, so when more than two people have reviewed overlapping
/// clips this reports the pair with the most items in common and NAMES the reviewers it left out —
/// silently averaging three raters into one number would be exactly the kind of quiet fabrication the
/// honesty law exists to prevent. (For >2 raters the right statistic is Krippendorff's alpha, which
/// the script deliberately does not implement.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgreementExport {
    pub rater_a: String,
    pub rater_b: String,
    /// Clips BOTH raters answered. Kappa on a handful of items means nothing; this is the number that
    /// says whether the figure is worth quoting.
    pub items: usize,
    /// Header row + one `label_a<TAB>label_b` line per shared clip, exactly what the script consumes.
    pub tsv: String,
    /// Where the file was written, so the owner can run the harness on it directly.
    pub path: String,
    /// Reviewers excluded because kappa takes exactly two. Never silently dropped.
    pub other_reviewers: Vec<String>,
}

/// One remote reviewer's score on clips whose answer was already known (Migration v44).
///
/// `noticed` is the blind-accept signal and the number to read first: a reviewer who listens corrects
/// a deliberately-wrong draft, one who taps "accept" hands it straight back. `mean_cer` then says how
/// close their corrections landed. A low `noticed` with any `checks` at all is the finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotCheckScore {
    pub reviewer: String,
    /// How many known-answer clips this reviewer has been given. Interpret nothing from a handful.
    pub checks: usize,
    /// On how many of them they changed the wrong draft (or rejected the clip) rather than accepting it.
    pub noticed: usize,
    /// Mean character error rate of their submitted text against the known answer.
    pub mean_cer: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SegmentsPage {
    pub items: Vec<SpeechSegment>,
    pub total: usize,
    pub next_cursor: Option<String>,
    /// Database-owned revision paired with each list row by the same SQLite result row. Typed review
    /// IPC consumes this map; legacy list callers safely ignore the additive field.
    #[serde(default)]
    pub revisions: std::collections::BTreeMap<String, i64>,
    /// True when a voice focus narrowed this page — i.e. `total` counts a SUBSET of the library.
    ///
    /// The page needs this to tell the truth. Review mode measures progress against the corpus and
    /// fires an "all clips reviewed" banner when its queue empties, a rule it already suppresses for
    /// a SEARCH subset. A focus is a subset too, so without this flag draining 1,318 focused clips
    /// announced a 15,262-clip library as finished (review 2026-08-20).
    #[serde(default)]
    pub focus_narrowed: bool,
}

/// Opaque continuation token for a stable keyset walk through the library list.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SegmentPageCursor {
    version: u8,
    sort: String,
    scope: String,
    anchor_rowid: i64,
    total: usize,
    emitted: usize,
    last: SegmentPageKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SegmentPageKey {
    id: String,
    created_at: String,
    duration_ms: i64,
    verified: bool,
    confidence: f64,
    active_learning: f64,
    escalated: bool,
    poor_audio: bool,
    agreement: f64,
}

/// One immutable description of a segment-page read. Keeping the scope together prevents callers
/// from accidentally swapping adjacent filter/cursor arguments as this query gains new modes.
struct SegmentPageQuery<'a> {
    verified: Option<bool>,
    text_query: Option<&'a str>,
    sort: &'a str,
    limit: usize,
    cursor: Option<&'a str>,
    focus: Option<&'a std::collections::HashSet<String>>,
    escalation_only: bool,
}

/// Rights attached to one source RECORDING (migration v49, deep-audit #6).
///
/// A voice recording is Article 9 biometric data: the lawful basis, the permitted use and the ability
/// to honour a withdrawal attach to the individual recording, and none of that is expressible in a
/// repo-level ATTRIBUTION.md. Every field is Optional and every default is None, because "unknown" is
/// the truthful state for a library whose provenance was never recorded per clip — and because the
/// default must FAIL CLOSED at the redistribution gate rather than assume permission.
///
/// Speaker identity deliberately does NOT live here: `speech_segments.speaker_id` already carries a
/// pseudonymous label (`SPEAKER_00`), and adding a real-identity column would create the re-
/// identification risk this schema exists to bound.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecordingRights {
    /// The licence the RECORDING is held under, e.g. "CC-BY-4.0" or "owner-private".
    pub license: Option<String>,
    /// The Article 9 lawful basis, e.g. "explicit_consent" / "public_dataset_licence".
    pub consent_basis: Option<String>,
    /// What the basis actually permits, e.g. "train" / "train,redistribute" / "private_only".
    pub permitted_use: Option<String>,
    /// The credit line the licence requires, carried into every export manifest.
    pub attribution: Option<String>,
    /// Where the recording came from — provenance, not a file path.
    pub source: Option<String>,
    /// Revocation lineage. Non-NULL means consent was withdrawn; it outranks every field above.
    pub revoked_at: Option<String>,
}

/// What the recorded rights permit. Ordered by severity — `Revoked` outranks everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightsDisposition {
    /// Consent withdrawn. Excluded from EVERY export path, including local personal ones.
    Revoked,
    /// Nothing recorded. Local personal export is allowed; redistribution is not.
    Unknown,
    /// A licence and basis are recorded, but they do not permit redistribution.
    PrivateOnly,
    /// Recorded, and the permitted use explicitly includes redistribution.
    Redistributable,
}

impl RecordingRights {
    /// The single place that decides what these fields permit.
    ///
    /// Fails closed at every step: no revocation check can be skipped, and absent fields never grant
    /// permission. `permitted_use` must NAME redistribution — a licence string alone is not consent to
    /// republish someone's voice.
    pub fn disposition(&self) -> RightsDisposition {
        if self.revoked_at.as_deref().is_some_and(|s| !s.trim().is_empty()) {
            return RightsDisposition::Revoked;
        }
        let declared = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
        if !declared(&self.license) || !declared(&self.consent_basis) {
            return RightsDisposition::Unknown;
        }
        let permits = self
            .permitted_use
            .as_deref()
            .unwrap_or("")
            .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
            .any(|t| matches!(t.trim().to_ascii_lowercase().as_str(), "redistribute" | "publish"));
        if permits {
            RightsDisposition::Redistributable
        } else {
            RightsDisposition::PrivateOnly
        }
    }

    /// True only for a recording that may leave this machine.
    pub fn permits_redistribution(&self) -> bool {
        self.disposition() == RightsDisposition::Redistributable
    }

    /// True when the recording must not appear in ANY export, local ones included.
    pub fn is_revoked(&self) -> bool {
        self.disposition() == RightsDisposition::Revoked
    }
}

/// P3.3: which distinct source audio files are missing on disk.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AudioHealth {
    pub total_files: usize,
    pub missing_files: usize,
    pub missing_paths: Vec<String>,
}

/// P3.3: outcome of a basename-based relink.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RelinkResult {
    pub relinked: usize,
    pub still_missing: usize,
}

/// P3.2: a directory-import job in the resume journal (a crash leaves one 'running').
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportJob {
    pub id: String,
    pub dir: String,
    pub total_files: usize,
    pub completed_paths: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscardImportJobOutcome {
    Discarded,
    NotFound,
    Changed,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SegmentHypothesis {
    pub segment_id: String,
    pub model_id: String,
    pub transcript: String,
    pub confidence: Option<f64>,
}

/// Exact database authority captured before one already-imported segment is transcribed.
///
/// Champion inference runs outside SQLite and can take minutes. The decoded-PCM lease held by the
/// caller freezes the source file itself; this snapshot supplies the other half of that boundary so
/// the final transaction can prove the segment id still names the same path, source span, duration,
/// PCM identity and review revision that were selected before inference began.
#[derive(Debug, Clone)]
pub(crate) struct ChampionTranscriptionSourceSnapshot {
    pub(crate) segment: SpeechSegment,
    pub(crate) review_revision: i64,
    pub(crate) audio_content_hash: Option<String>,
}

/// A declaration that a source recording was PROCESSED before it was ever imported.
///
/// The absence of a record means one thing only: nothing has claimed this recording was processed.
/// It never means "verified original" — see `Database::source_audio_provenance`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceAudioProvenance {
    pub audio_path: String,
    /// Human-readable statement of what was done to the audio, carried into every export.
    pub processing: String,
    /// The separation/enhancement model, when one was used.
    pub separator_model: Option<String>,
    /// False when the processing CUT audio out, so timestamps no longer map to the original. That is
    /// the property a downstream consumer must not get wrong: it decides whether an offset means
    /// anything at all.
    pub timeline_preserved: bool,
    /// Where the full parameter set lives (the cleaner's own manifest.json).
    pub manifest_path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SourceTranscriptRecord {
    pub audio_path: String,
    pub model_id: String,
    pub audio_content_hash: Option<String>,
    pub audio_size_bytes: Option<i64>,
    pub transcript_path: String,
    pub transcript_text: String,
    pub created_at: Option<String>,
}

mod core;
pub use core::*;
pub mod batch_jobs;
pub use batch_jobs::*;
mod decisions;
mod finalization;
mod history;
mod jobs_rights;
mod playback;
mod queries_recovery;
mod review;
mod review_pilot_keys;
mod segments;

impl Database {
    /// Review operation UUIDs are a single truth namespace even though the legacy schema stores
    /// canonical and blinded-second-pass decisions in separate tables.  Every writer that can
    /// publish canonical truth holds BEGIN IMMEDIATE before calling this helper; the independent
    /// writer takes the same reservation and performs the symmetric check.  That closes both race
    /// directions without rewriting immutable migrations 1-65.
    fn require_canonical_operation_namespace_on(conn: &Connection, operation_id: &str) -> AppResult<()> {
        let independent_collision: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM independent_review_decisions WHERE operation_id=?1
                 UNION ALL
                 SELECT 1 FROM independent_review_reversals WHERE operation_id=?1
                 UNION ALL
                 SELECT 1 FROM review_pool_reversals WHERE operation_id=?1
             )",
            [operation_id],
            |row| row.get(0),
        )?;
        if independent_collision {
            return Err(AppError::Validation(
                "E_REVIEW_OPERATION_NAMESPACE_COLLISION: operation UUID is already bound to independent review truth"
                    .into(),
            ));
        }
        Ok(())
    }

    fn lock_playback_live_sessions(&self) -> MutexGuard<'_, HashMap<String, u64>> {
        self.playback_live_sessions.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned policy-4 active-time ledger");
            poisoned.into_inner()
        })
    }

    fn playback_clock_now(&self) -> AppResult<(i64, u64)> {
        #[cfg(test)]
        if let Some(clock) = *self.playback_test_clock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            return Ok(clock);
        }
        Ok((playback_server_now_ms()?.max(1), playback_active_now_100ns()))
    }

    fn live_playback_elapsed_ms(&self, playback_receipt_id: &str, active_now_100ns: u64) -> AppResult<Option<i64>> {
        let issued = self.lock_playback_live_sessions().get(playback_receipt_id).copied();
        let Some(issued) = issued else { return Ok(None) };
        let elapsed_100ns = active_now_100ns.checked_sub(issued).ok_or_else(|| {
            AppError::Validation(
                "E_PLAYBACK_ACTIVE_CLOCK_REGRESSED: the workstation active-time clock regressed; reload the clip"
                    .into(),
            )
        })?;
        let elapsed_ms = elapsed_100ns / 10_000;
        Ok(Some(i64::try_from(elapsed_ms).unwrap_or(i64::MAX)))
    }

    fn active_playback_session_ids(&self, active_now_100ns: u64) -> HashSet<String> {
        let ttl_100ns = u64::try_from(DESKTOP_PLAYBACK_SESSION_TTL_MS).unwrap_or(u64::MAX).saturating_mul(10_000);
        let mut sessions = self.lock_playback_live_sessions();
        sessions.retain(|_, issued| {
            active_now_100ns
                .checked_sub(*issued)
                .map(|elapsed| elapsed <= ttl_100ns)
                // A regressing platform clock is an error at finalization, not permission to delete
                // a potentially-live attempt underneath it.
                .unwrap_or(true)
        });
        sessions.keys().cloned().collect()
    }

    fn prune_abandoned_playback_sessions_on(
        tx: &rusqlite::Transaction<'_>,
        active_session_ids: &HashSet<String>,
    ) -> AppResult<()> {
        let abandoned = {
            let mut statement = tx.prepare(
                "SELECT session.playback_receipt_id
                   FROM desktop_playback_sessions_v4 session
                  WHERE NOT EXISTS (
                        SELECT 1 FROM playback_receipts receipt
                         WHERE receipt.authority_session_id=session.playback_receipt_id
                  )",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for playback_receipt_id in abandoned {
            if active_session_ids.contains(&playback_receipt_id) {
                continue;
            }
            tx.execute(
                "DELETE FROM desktop_playback_intervals_v4 WHERE playback_receipt_id=?1",
                [&playback_receipt_id],
            )?;
            tx.execute(
                "DELETE FROM desktop_playback_sessions_v4 WHERE playback_receipt_id=?1",
                [&playback_receipt_id],
            )?;
        }
        Ok(())
    }

    /// Remove one never-finalized playback authority while its exact session row is still the only
    /// durable state.  A receipt makes the authority immutable and therefore non-cancellable.
    /// `expected_client_attempt_id` is required at the renderer-facing boundary so a stale component
    /// cannot retire a newer component's session even if it somehow retained the receipt UUID.
    fn retire_unfinalized_playback_session_on(
        tx: &rusqlite::Transaction<'_>,
        playback_receipt_id: &str,
        expected_client_attempt_id: Option<&str>,
    ) -> AppResult<bool> {
        let stored_attempt: Option<String> = tx
            .query_row(
                "SELECT client_attempt_id
                   FROM desktop_playback_sessions_v4
                  WHERE playback_receipt_id=?1",
                [playback_receipt_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored_attempt) = stored_attempt else {
            // Exact cancellation is idempotent. After the first successful retirement there is no
            // durable authority left to mutate, so the replay is a safe no-op.
            return Ok(false);
        };
        if expected_client_attempt_id.is_some_and(|expected| expected != stored_attempt) {
            return Err(AppError::Validation(
                "E_PLAYBACK_CANCEL_IDENTITY_MISMATCH: playback authority belongs to a different client attempt".into(),
            ));
        }
        let finalized: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM playback_receipts
                  WHERE authority_session_id=?1 AND policy_version=?2
             )",
            params![playback_receipt_id, DESKTOP_PLAYBACK_POLICY_VERSION],
            |row| row.get(0),
        )?;
        if finalized {
            return Err(AppError::Validation(
                "E_PLAYBACK_SESSION_FINALIZED: finalized playback authority is immutable and cannot be cancelled"
                    .into(),
            ));
        }

        // Intervals are normally inserted atomically with the receipt, but deleting them first keeps
        // cleanup correct for an interrupted developer/staged database without weakening finalized
        // evidence (the immutable trigger and receipt check above both fail closed).
        tx.execute("DELETE FROM desktop_playback_intervals_v4 WHERE playback_receipt_id=?1", [playback_receipt_id])?;
        let deleted = tx.execute(
            "DELETE FROM desktop_playback_sessions_v4
              WHERE playback_receipt_id=?1
                AND NOT EXISTS (
                    SELECT 1 FROM playback_receipts receipt
                     WHERE receipt.authority_session_id=desktop_playback_sessions_v4.playback_receipt_id
                       AND receipt.policy_version=?2
                )",
            params![playback_receipt_id, DESKTOP_PLAYBACK_POLICY_VERSION],
        )?;
        if deleted != 1 {
            return Err(AppError::Other(
                "playback cancellation lost its exact unfinalized session inside the write transaction".into(),
            ));
        }
        Ok(true)
    }

    /// Select the oldest unfinalized authorities that must be retired to make room for one new
    /// attempt. This is the server-side safety net for a lost renderer cancellation: ordinary N/P
    /// browsing can never consume all 64 slots for 30 minutes. Finalized receipts are excluded and
    /// therefore remain immutable regardless of capacity pressure.
    fn playback_sessions_to_reclaim_on(tx: &rusqlite::Transaction<'_>, segment_id: &str) -> AppResult<Vec<String>> {
        fn unfinalized_ids(tx: &rusqlite::Transaction<'_>, segment_id: Option<&str>) -> AppResult<Vec<String>> {
            let sql = if segment_id.is_some() {
                "SELECT session.playback_receipt_id
                   FROM desktop_playback_sessions_v4 session
                  WHERE session.segment_id=?1
                    AND NOT EXISTS (
                        SELECT 1 FROM playback_receipts receipt
                         WHERE receipt.authority_session_id=session.playback_receipt_id
                    )
                  ORDER BY session.issued_at_ms, session.playback_receipt_id"
            } else {
                "SELECT session.playback_receipt_id
                   FROM desktop_playback_sessions_v4 session
                  WHERE NOT EXISTS (
                        SELECT 1 FROM playback_receipts receipt
                         WHERE receipt.authority_session_id=session.playback_receipt_id
                    )
                  ORDER BY session.issued_at_ms, session.playback_receipt_id"
            };
            let mut statement = tx.prepare(sql)?;
            if let Some(value) = segment_id {
                Ok(statement.query_map([value], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            } else {
                Ok(statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            }
        }

        let mut reclaimed = Vec::new();
        let per_segment = unfinalized_ids(tx, Some(segment_id))?;
        let keep_before_insert =
            usize::try_from(MAX_LIVE_DESKTOP_PLAYBACK_SESSIONS_PER_SEGMENT - 1).unwrap_or_default();
        let per_segment_excess = per_segment.len().saturating_sub(keep_before_insert);
        for playback_receipt_id in per_segment.into_iter().take(per_segment_excess) {
            if Self::retire_unfinalized_playback_session_on(tx, &playback_receipt_id, None)? {
                reclaimed.push(playback_receipt_id);
            }
        }

        let global = unfinalized_ids(tx, None)?;
        let keep_before_insert = usize::try_from(MAX_LIVE_DESKTOP_PLAYBACK_SESSIONS - 1).unwrap_or_default();
        let global_excess = global.len().saturating_sub(keep_before_insert);
        for playback_receipt_id in global.into_iter().take(global_excess) {
            if Self::retire_unfinalized_playback_session_on(tx, &playback_receipt_id, None)? {
                reclaimed.push(playback_receipt_id);
            }
        }
        Ok(reclaimed)
    }

    /// Idempotently retire the exact renderer attempt only while it has no immutable receipt. A
    /// component teardown may race a successful finalization; in that ordering cancellation refuses
    /// to touch the receipt and the caller can still replay/commit it normally.
    pub fn cancel_desktop_playback_session_v1(
        &self,
        playback_receipt_id: &str,
        client_attempt_id: &str,
    ) -> AppResult<bool> {
        validate_operation_uuid(playback_receipt_id)?;
        validate_operation_uuid(client_attempt_id)?;
        let retired = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            let retired =
                Self::retire_unfinalized_playback_session_on(&tx, playback_receipt_id, Some(client_attempt_id))?;
            if retired {
                tx.commit()?;
            } else {
                tx.rollback()?;
            }
            Ok(retired)
        })?;
        if retired {
            self.lock_playback_live_sessions().remove(playback_receipt_id);
            self.track_write()?;
        }
        Ok(retired)
    }

    #[cfg(test)]
    pub(crate) fn set_playback_test_clock(&self, wall_ms: i64, active_ms: u64) {
        *self.playback_test_clock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((wall_ms.max(1), active_ms.saturating_mul(10_000)));
    }

    #[cfg(test)]
    pub(crate) fn clear_playback_test_clock(&self) {
        *self.playback_test_clock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    pub(crate) fn with_full_sync<T>(&self, operation: impl FnOnce() -> AppResult<T>) -> AppResult<T> {
        self.conn.execute_batch("PRAGMA synchronous=FULL;")?;
        let result = operation();
        let reset = self.conn.execute_batch("PRAGMA synchronous=NORMAL;");
        match result {
            Ok(value) => {
                // The closure may already have committed durable truth. A failure to relax the
                // connection back to NORMAL cannot turn that commit into an error response: doing
                // so creates an ambiguous lost-response state and falsely tells the renderer the
                // write failed. Remaining at FULL is conservative (slower, never less durable).
                if let Err(reset_error) = reset {
                    tracing::error!(
                        "durable FULL-sync operation committed, but SQLite synchronous=NORMAL could not be restored; keeping the conservative connection mode: {reset_error}"
                    );
                }
                Ok(value)
            }
            Err(error) => {
                if let Err(reset_error) = reset {
                    tracing::warn!("failed to restore SQLite synchronous=NORMAL after error: {reset_error}");
                }
                Err(error)
            }
        }
    }

    /// Open a frozen SQLite artifact without creating `-wal`/`-shm` sidecars beside it. Plain
    /// `SQLITE_OPEN_READ_ONLY` can still open WAL shared memory; that mutates a manifest-bound
    /// snapshot and makes its final exact-inventory verification fail. SQLite's `immutable=1` URI
    /// promises the bytes cannot change and suppresses all journal/shared-memory creation.
    pub(crate) fn open_immutable_connection(path: &Path) -> AppResult<Connection> {
        let absolute = std::fs::canonicalize(path)?;
        let mut normalized = absolute.to_string_lossy().replace('\\', "/");
        if let Some(stripped) = normalized.strip_prefix("//?/UNC/") {
            normalized = format!("//{stripped}");
        } else if let Some(stripped) = normalized.strip_prefix("//?/") {
            normalized = stripped.to_string();
        }
        let mut encoded = String::with_capacity(normalized.len());
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for byte in normalized.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                encoded.push('%');
                encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
        let uri = format!("file:{encoded}?immutable=1");
        Connection::open_with_flags(
            uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(Into::into)
    }

    pub fn open(path: &str) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA cache_size=-64000;
             PRAGMA busy_timeout=10000;",
        )?;
        Ok(Self::from_connection(conn, path.to_string()))
    }

    /// Open the live SQLite/WAL path with query-only authority and one stable read transaction.
    /// This preserves real disk/cache behavior without a full in-memory copy, but cannot change rows,
    /// journal mode, or invoke startup corruption recovery. The first query establishes the WAL
    /// snapshot and every later certification query sees that same point in time.
    pub fn open_read_only(path: &str) -> AppResult<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA cache_size=-64000;
             PRAGMA busy_timeout=10000;
             BEGIN DEFERRED;",
        )?;
        Ok(Self::from_connection(conn, path.to_string()))
    }

    /// Take a WAL-consistent, detached in-memory snapshot of an existing database without acquiring
    /// source write authority or changing its journal mode. Offline certification and production
    /// export use the private copy so they can never bootstrap, migrate, or mutate the live library.
    /// The copy stays writable because SQLite's FTS5 integrity check uses temporary internal writes;
    /// even an accidental caller write can affect only this disposable connection.
    pub fn open_detached_read_snapshot(path: &str) -> AppResult<Self> {
        let source = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        source.execute_batch("PRAGMA busy_timeout=10000;")?;
        let mut conn = Connection::open_in_memory()?;
        {
            let backup = backup::Backup::new(&source, &mut conn)?;
            backup.run_to_completion(Self::BACKUP_PAGES_PER_STEP, Self::BACKUP_STEP_PAUSE, None)?;
        }
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA cache_size=-64000;
             PRAGMA busy_timeout=10000;",
        )?;
        Ok(Self::from_connection(conn, path.to_string()))
    }

    /// Copy a manifest-bound, single-file SQLite authority into a writable in-memory database.
    ///
    /// Unlike [`Self::open_detached_read_snapshot`], this deliberately uses SQLite's `immutable=1`
    /// source contract: sibling WAL/SHM files are ignored and cannot be created or consulted. Callers
    /// must independently prove that `path` is a frozen, journal-free authority before using it.
    pub fn open_detached_immutable_snapshot(path: &Path) -> AppResult<Self> {
        let source = Self::open_immutable_connection(path)?;
        source.execute_batch("PRAGMA busy_timeout=10000;")?;
        let mut conn = Connection::open_in_memory()?;
        {
            let backup = backup::Backup::new(&source, &mut conn)?;
            backup.run_to_completion(Self::BACKUP_PAGES_PER_STEP, Self::BACKUP_STEP_PAUSE, None)?;
        }
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA cache_size=-64000;
             PRAGMA busy_timeout=10000;",
        )?;
        Ok(Self::from_connection(conn, path.to_string_lossy().into_owned()))
    }

    /// Open the database with a retry policy for corruption.
    ///
    /// Recovery is fail-CLOSED. Genuine corruption produces a stable, byte-verified COPY of the
    /// complete SQLite main/WAL/SHM bundle, leaves the source bundle untouched, and aborts startup.
    /// Cortex never opens a fresh empty library behind the owner's back. A transient error — an
    /// external process holding the file locked past busy_timeout, a disk I/O hiccup, OOM during the
    /// integrity check — does not even create a quarantine copy; it simply aborts so the owner can
    /// clear the locker / fix the disk and retry the original authority intact.
    pub fn open_with_retry(path: &str) -> AppResult<Self> {
        match Self::open(path) {
            Ok(db) => {
                match db.integrity_check() {
                    Ok(result) if result.trim() == "ok" => {
                        return Ok(db);
                    }
                    Ok(result) if integrity_result_looks_transient(&result) => {
                        // PRAGMA integrity_check reports a transient page-read failure (a momentary disk
                        // error, or AV/backup/indexer holding a page locked mid-scan) as a text result
                        // ROW, e.g. "unable to get the page 42. error code=8" — which arrives here as
                        // Ok(non-"ok"). Quarantining (renaming the live db away and opening an empty one)
                        // a HEALTHY db on that transient signal is silent total data loss. Mirror the
                        // Err branch's discipline: abort startup WITHOUT quarantine so the user can retry
                        // with their data intact.
                        tracing::error!(
                            "Database integrity check returned a transient I/O message (not corruption); aborting startup without quarantine: {result}"
                        );
                        return Err(AppError::Other(format!(
                            "Database integrity check could not complete (transient, not corruption): {result}"
                        )));
                    }
                    Ok(result) => {
                        // A non-"ok", non-transient string is SQLite reporting genuine structural page
                        // corruption: quarantine.
                        tracing::error!("Database integrity check failed on open; quarantining database: {result}");
                    }
                    Err(e) if is_corruption_error(&e) => {
                        tracing::error!(
                            "Database integrity check returned a corruption code on open; quarantining database: {e}"
                        );
                    }
                    Err(e) => {
                        // Transient/non-corruption error — do NOT destroy a possibly-healthy database.
                        tracing::error!(
                            "Database integrity check could not complete (transient, not corruption); aborting startup without quarantine: {e}"
                        );
                        return Err(e);
                    }
                }
                drop(db);
                let quarantine = recover_database_at(path)?;
                Err(corrupt_database_hard_stop(&quarantine))
            }
            Err(e) if is_corruption_error(&e) => {
                tracing::error!("Failed to open database with a corruption code: {e}. Preserving a quarantine copy...");
                let quarantine = recover_database_at(path)?;
                Err(corrupt_database_hard_stop(&quarantine))
            }
            Err(e) => {
                // A non-corruption open failure (lock contention, permissions, transient I/O) must not
                // quarantine the database — surface it so the user can resolve and retry.
                tracing::error!("Failed to open database (transient/non-corruption); aborting without quarantine: {e}");
                Err(e)
            }
        }
    }

    pub fn initialize(&self) -> AppResult<()> {
        self.initialize_inner(true)
    }

    fn initialize_inner(&self, enforce_schema_contract: bool) -> AppResult<()> {
        // Capture freshness before creating any base-schema object. Migration history may be empty
        // only for this proven-pristine case; an existing database with deleted/tampered history must
        // fail closed instead of replaying migrations against live data.
        let was_pristine = crate::migrations::database_is_pristine(&self.conn)?;
        if !was_pristine {
            crate::migrations::validate_applied_history(&self.conn)?;
        }
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS speech_segments (
                id TEXT PRIMARY KEY,
                audio_path TEXT NOT NULL,
                raw_transcript TEXT NOT NULL DEFAULT '',
                normalized_transcript TEXT,
                annotated_transcript TEXT,
                alignment_json TEXT,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                speaker_id TEXT,
                verified INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_segments_verified ON speech_segments(verified);
            CREATE INDEX IF NOT EXISTS idx_segments_speaker ON speech_segments(speaker_id);
            CREATE INDEX IF NOT EXISTS idx_segments_created ON speech_segments(created_at);
            -- AUTHORITATIVE segments_fts schema. Round-23 #8: migrations/001_initial.sql contains a
            -- second, DIFFERENT (4-column) CREATE for segments_fts, but this block runs first on a fresh
            -- boot, so the migration's `IF NOT EXISTS` makes it a no-op — THIS definition is the one in
            -- effect. Edit the FTS schema HERE (and the three triggers below), not in the migration copy.
            -- `audio_path` stays indexed for trigger symmetry but is excluded from search by a column
            -- filter in search_segments (#7), so it never produces false-positive transcript hits.
            CREATE VIRTUAL TABLE IF NOT EXISTS segments_fts USING fts5(
                id UNINDEXED,
                audio_path,
                raw_transcript,
                normalized_transcript,
                annotated_transcript,
                content=speech_segments,
                content_rowid=rowid,
                tokenize='unicode61'
            );
            CREATE TRIGGER IF NOT EXISTS segments_ai AFTER INSERT ON speech_segments BEGIN
                INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
            END;
            CREATE TRIGGER IF NOT EXISTS segments_ad AFTER DELETE ON speech_segments BEGIN
                INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
            END;
            CREATE TRIGGER IF NOT EXISTS segments_au AFTER UPDATE ON speech_segments BEGIN
                INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
                INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
            END;"
        )?;
        self.conn.execute_batch("INSERT INTO segments_fts(segments_fts) VALUES('rebuild');")?;
        crate::migrations::run_migrations_after_pristine_initialize(self, was_pristine)?;
        if enforce_schema_contract {
            validate_current_schema_contract(&self.conn)?;
        }
        validate_policy4_effect_authority(&self.conn)?;
        // Batch headers and items are a second immutable effect ledger.  Validate their canonical
        // request/projection hashes at the same boundary as policy-4 authority so both normal
        // startup and an isolated staged restore fail closed before exposing forged or torn work.
        self.validate_batch_job_authority_v1()?;
        // v69 is an immutable total-order ledger. Prove its complete set equality and inverse
        // ordering once at startup; interactive Undo availability can then read only the journal
        // tail under the append-only triggers instead of rescanning the complete history.
        self.validate_desktop_review_action_journal()?;
        Ok(())
    }

    /// Re-run the complete policy-4 receipt/session/interval/consumption/effect proof at a restore
    /// boundary. Staged restore initialization already calls this validator; exposing the same
    /// read-only authority here lets higher-level cross-ledger checks fail closed after any
    /// characterization mutation without copying or weakening the canonical proof.
    pub(crate) fn validate_policy4_restore_authority(&self) -> AppResult<()> {
        validate_policy4_effect_authority(&self.conn)?;
        batch_jobs::validate_batch_job_authority_on(&self.conn)
    }

    pub(crate) fn cleanup_savepoint_after_error(&self, savepoint: &str) {
        if let Err(error) = self.conn.execute(&format!("ROLLBACK TO {savepoint}"), []) {
            tracing::warn!("Failed to roll back savepoint {savepoint}: {error}");
        }
        if let Err(error) = self.conn.execute(&format!("RELEASE {savepoint}"), []) {
            tracing::warn!("Failed to release savepoint {savepoint}: {error}");
        }
    }

    /// Release (commit) the named OUTERMOST savepoint. For the outermost savepoint, RELEASE *is* the
    /// WAL commit and can fail (SQLITE_BUSY/IOERR at commit time); SQLite then leaves the savepoint
    /// OPEN. If we returned that error without unwinding (the old `RELEASE ...?`), the dangling
    /// savepoint would persist on the shared, poison-recovering (never-reopened) connection, so the
    /// NEXT command would run inside the stale transaction and a later ROLLBACK TO could silently
    /// discard writes already reported as committed. Roll it back + release on failure so a failed
    /// commit cannot poison the connection.
    pub(crate) fn release_savepoint(&self, savepoint: &str) -> AppResult<()> {
        if let Err(error) = self.conn.execute(&format!("RELEASE {savepoint}"), []) {
            self.cleanup_savepoint_after_error(savepoint);
            return Err(error.into());
        }
        Ok(())
    }
}

/// True only when an error indicates the database FILE itself is corrupt / not a database — the only
/// conditions under which a forensic quarantine COPY is warranted. Transient errors
/// (SQLITE_BUSY/LOCKED, disk I/O, OOM) return false so a healthy db is never copied or displaced.
fn is_corruption_error(err: &AppError) -> bool {
    use rusqlite::ErrorCode;
    matches!(
        err,
        AppError::Database(rusqlite::Error::SqliteFailure(f, _))
            if matches!(f.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

/// Whether a non-"ok" `PRAGMA integrity_check` result row is a TRANSIENT page-access / I/O message
/// rather than genuine structural corruption. integrity_check is designed to keep walking the b-tree
/// and report problems as up to 100 text result rows instead of failing the statement, so a momentary
/// page-read failure (disk hiccup, or an AV/backup/indexer holding a page locked mid-scan) surfaces as
/// `Ok("unable to get the page N. error code=...")`. Treating that as corruption and quarantining a
/// HEALTHY database is silent total data loss, so these abort startup without quarantine instead.
fn integrity_result_looks_transient(result: &str) -> bool {
    let r = result.to_ascii_lowercase();
    r.contains("unable to get the page")
        || r.contains("error code=")
        || r.contains("i/o error")
        || r.contains("disk i/o")
        || r.contains("is locked")
        || r.contains("out of memory")
}

fn corrupt_database_hard_stop(quarantine: &Path) -> AppError {
    AppError::Other(format!(
        "Database corruption was confirmed. Cortex preserved a byte-verified quarantine bundle at {} and left the original database untouched. Automatic empty-library recovery is refused; restore a validated snapshot or repair the quarantined copy before continuing.",
        quarantine.display()
    ))
}

/// Preserve a stable forensic copy of the complete SQLite authority without mutating the source.
///
/// The old implementation renamed the main file first and moved WAL/SHM best-effort. A crash or a
/// Windows sharing violation in that gap stranded committed WAL pages beside a missing main file;
/// SQLite then created a fresh database and discarded the orphan WAL on restart. Here every present
/// part is copied, flushed, and hash-verified twice while the original remains in place. Staged
/// sidecars are promoted first and the quarantine main file last, so a `*.corrupt.*` main name is a
/// completion marker rather than evidence of a partial bundle. Startup still hard-stops afterward.
fn recover_database_at(path: &str) -> AppResult<PathBuf> {
    let path_buf = Path::new(path);
    if !path_buf.exists() {
        return Err(AppError::Other(
            "database corruption recovery lost its source before a forensic copy could be made; refusing to create an empty library"
                .into(),
        ));
    }

    let backup_path = unique_corrupt_backup_path(path_buf, chrono::Utc::now().timestamp());
    copy_database_bundle_with(path_buf, &backup_path, |source, destination| {
        std::fs::copy(source, destination).map(|_| ())
    })?;
    tracing::error!(
        "Corrupt database preserved without source mutation at {}; startup remains blocked",
        backup_path.display()
    );
    Ok(backup_path)
}

fn unique_corrupt_backup_path(db_path: &Path, timestamp: i64) -> PathBuf {
    let base = db_path.with_extension(format!("corrupt.{timestamp}"));
    if quarantine_path_is_available(&base) {
        return base;
    }

    for suffix in 1..1000 {
        let candidate = db_path.with_extension(format!("corrupt.{timestamp}.{suffix}"));
        if quarantine_path_is_available(&candidate) {
            return candidate;
        }
    }

    loop {
        let candidate = db_path.with_extension(format!("corrupt.{timestamp}.{}", uuid::Uuid::new_v4()));
        if quarantine_path_is_available(&candidate) {
            return candidate;
        }
    }
}

fn quarantine_path_is_available(path: &Path) -> bool {
    !path.exists() && !sqlite_sidecar_path(path, "-wal").exists() && !sqlite_sidecar_path(path, "-shm").exists()
}

fn copy_database_bundle_with(
    source_db: &Path,
    backup_db: &Path,
    mut copy_file: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> AppResult<()> {
    // Staging names intentionally do not contain `.corrupt.`. Recovery notices and snapshot pins
    // treat that token on a main file as the completed-quarantine marker, so a crash during copy
    // must never manufacture a false completed bundle.
    let source_name = source_db.file_name().and_then(|name| name.to_str()).unwrap_or("cortex-speech.db");
    let staging_db = source_db.with_file_name(format!(".{source_name}.quarantine-staging-{}", uuid::Uuid::new_v4()));
    let _staging_guard = QuarantineStagingGuard::new(staging_db.clone());
    let mut parts: Vec<(PathBuf, PathBuf, PathBuf, u64, [u8; 32])> = Vec::new();
    let expected_sidecars = [
        ("-wal", sqlite_sidecar_path(source_db, "-wal").exists()),
        ("-shm", sqlite_sidecar_path(source_db, "-shm").exists()),
    ];

    // Copy the main file first so an injected/faulting WAL copy proves the source main was never
    // renamed. Nothing is promoted until every present part is stable and verified.
    for suffix in ["", "-wal", "-shm"] {
        let source = if suffix.is_empty() { source_db.to_path_buf() } else { sqlite_sidecar_path(source_db, suffix) };
        if !source.exists() {
            continue;
        }
        let staging = if suffix.is_empty() { staging_db.clone() } else { sqlite_sidecar_path(&staging_db, suffix) };
        let final_path =
            if suffix.is_empty() { backup_db.to_path_buf() } else { sqlite_sidecar_path(backup_db, suffix) };
        copy_file(&source, &staging)?;
        std::fs::OpenOptions::new().write(true).open(&staging)?.sync_all()?;
        let source_len = std::fs::metadata(&source)?.len();
        let source_hash = sha256_file_bytes(&source)?;
        if std::fs::metadata(&staging)?.len() != source_len || sha256_file_bytes(&staging)? != source_hash {
            return Err(AppError::Other(format!(
                "forensic quarantine copy verification failed for {}; original database bundle is unchanged",
                source.display()
            )));
        }
        parts.push((source, staging, final_path, source_len, source_hash));
    }
    let Some((captured_main, _, _, _, _)) = parts.first() else {
        return Err(AppError::Other(
            "forensic quarantine did not capture the SQLite main file; original database bundle is unchanged".into(),
        ));
    };
    if captured_main != source_db {
        return Err(AppError::Other(
            "forensic quarantine did not capture the SQLite main file first; original database bundle is unchanged"
                .into(),
        ));
    }

    // Detect a concurrent/external writer after the copies. Hashing every source again makes a mixed
    // generation fail closed; no original path has been renamed or deleted.
    for (source, _, _, expected_len, expected_hash) in &parts {
        if std::fs::metadata(source)?.len() != *expected_len || sha256_file_bytes(source)? != *expected_hash {
            return Err(AppError::Other(format!(
                "database bundle changed while forensic quarantine was being copied ({}); original remains authoritative and startup is blocked",
                source.display()
            )));
        }
    }
    for (suffix, was_present) in expected_sidecars {
        if sqlite_sidecar_path(source_db, suffix).exists() != was_present {
            return Err(AppError::Other(format!(
                "database bundle membership changed while forensic quarantine was being copied ({suffix}); original remains authoritative and startup is blocked"
            )));
        }
    }

    // The main quarantine name is the completion marker. Promote sidecars first, main last.
    for (_, staging, final_path, _, _) in parts.iter().filter(|(_, _, final_path, _, _)| final_path != backup_db) {
        std::fs::rename(staging, final_path)?;
        crate::atomic_file::fsync_parent_dir(final_path);
    }
    // Close the last meaningful race before publishing the main completion marker. A supported
    // single-owner process has no writer here, but an unexpected external SQLite writer must not
    // silently add/remove a WAL or SHM generation while quarantine is being assembled.
    for (suffix, was_present) in expected_sidecars {
        if sqlite_sidecar_path(source_db, suffix).exists() != was_present {
            return Err(AppError::Other(format!(
                "database bundle membership changed before forensic quarantine publication ({suffix}); original remains authoritative and startup is blocked"
            )));
        }
    }
    let (_, staging_main, final_main, _, _) = &parts[0];
    std::fs::rename(staging_main, final_main)?;
    crate::atomic_file::fsync_parent_dir(final_main);
    Ok(())
}

struct QuarantineStagingGuard {
    main: PathBuf,
}

impl QuarantineStagingGuard {
    fn new(main: PathBuf) -> Self {
        Self { main }
    }
}

impl Drop for QuarantineStagingGuard {
    fn drop(&mut self) {
        // Best-effort cleanup is safe because these UUID-named files are private staging artifacts;
        // the source authority and any fully promoted `.corrupt.*` bundle are different paths.
        for path in
            [sqlite_sidecar_path(&self.main, "-shm"), sqlite_sidecar_path(&self.main, "-wal"), self.main.clone()]
        {
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        "Could not remove incomplete quarantine staging artifact {}: {}",
                        path.display(),
                        error
                    );
                }
            }
        }
    }
}

fn sha256_file_bytes(path: &Path) -> AppResult<[u8; 32]> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().into())
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", db_path.display(), suffix))
}

/// Regression gates for the 2026-08-25 deep-audit findings in this file. Kept here rather than in
/// `db_tests.rs` so the fix and the proof it stays fixed live in one file.
#[cfg(test)]
mod audit_20260825_tests {
    use super::*;

    fn db() -> Database {
        let database = Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        database
    }

    fn machine_segment(id: &str, audio_path: &str, raw: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            audio_path: audio_path.to_string(),
            raw_transcript: raw.to_string(),
            duration_ms: 4_000,
            ..SpeechSegment::default()
        }
    }

    /// M11. The queue's SQL narrowing and `quality::is_placeholder_transcript` must agree EXACTLY.
    /// They did not: SQL tested `[%]` only, so an `n/a` draft passed the filter and was served to a
    /// paid reviewer who could blind-accept it into gold without the champion ever drafting the clip.
    #[test]
    fn placeholder_sql_agrees_with_the_rust_placeholder_authority() {
        let database = db();
        let cases = [
            "",
            "   ",
            "n/a",
            "N/A",
            "  NuLl  ",
            "null",
            "[Pending WSL 7B ASR]",
            "[ASR unavailable: model load failed]",
            "سڵاو، چۆنی باشی؟",
            "a real transcript a human would keep",
        ];
        for (i, text) in cases.iter().enumerate() {
            database.insert_segment(&machine_segment(&format!("p{i:02}"), &format!("/audio/p{i}.wav"), text)).unwrap();
        }

        let sql =
            format!("SELECT id FROM speech_segments WHERE {}", placeholder_or_empty_transcript_sql("raw_transcript"));
        let mut stmt = database.connection().prepare(&sql).unwrap();
        let flagged: std::collections::HashSet<String> =
            stmt.query_map([], |row| row.get::<_, String>(0)).unwrap().map(Result::unwrap).collect();

        for (i, text) in cases.iter().enumerate() {
            let authority = crate::quality::is_placeholder_transcript(text) || text.trim().is_empty();
            assert_eq!(
                flagged.contains(&format!("p{i:02}")),
                authority,
                "the SQL fragment and quality::is_placeholder_transcript disagree about {text:?} — a \
                 placeholder either reaches a paid reviewer or a real transcript is dropped from the queue"
            );
        }
    }

    /// M11, at the SERVING path: the couch queue itself must not hand an `n/a` draft to a reviewer.
    #[test]
    fn the_couch_queue_never_serves_a_placeholder_draft() {
        let temp = tempfile::tempdir().unwrap();
        let database = db();
        for (id, text) in
            [("real", "دەقێکی ڕاستەقینە"), ("na", "n/a"), ("nul", "NULL"), ("bracket", "[Pending WSL 7B ASR]")]
        {
            // The queue also refuses clips whose audio is gone, so every fixture gets a real file.
            let audio = temp.path().join(format!("{id}.wav"));
            std::fs::write(&audio, b"RIFF").unwrap();
            database.insert_segment(&machine_segment(id, audio.to_str().unwrap(), text)).unwrap();
        }
        assert_eq!(database.pending_segment_ids().unwrap(), vec!["real".to_string()]);
    }

    /// The twice-fixed blank-transcript-overwrite class, now fenced at the shared persist boundary
    /// instead of only at each call site.
    #[test]
    fn a_blank_draft_is_refused_at_both_shared_asr_persist_boundaries() {
        let database = db();
        database.insert_segment(&machine_segment("blank", "/blank.wav", "a good champion draft")).unwrap();

        for blank in ["", "   ", "\n\t "] {
            let refine = database
                .update_asr_transcript_if_unreviewed("blank", blank, None, Some(0.9), None, None, false)
                .unwrap_err();
            assert!(refine.to_string().contains("blank ASR transcript"), "{refine}");
            let batch = database
                .update_batch_transcription_if_unreviewed("blank", blank, None, Some(0.9), None, None, false)
                .unwrap_err();
            assert!(batch.to_string().contains("blank ASR transcript"), "{batch}");
        }

        let row = database.get_segment_by_id("blank").unwrap().unwrap();
        assert_eq!(row.raw_transcript, "a good champion draft", "the good draft must survive every blank write");
        let fresh = database
            .update_asr_transcript_if_unreviewed("blank", "a fresh draft", None, Some(0.9), None, None, false)
            .unwrap();
        assert!(fresh, "a real draft must still persist — the guard is about blankness, not about writing");
    }

    /// Undo of a batch transcription names only ASR columns, so applying it after a human decided
    /// would swap the transcript out from under a live verdict. Refuse instead.
    #[test]
    fn batch_transcription_undo_refuses_when_a_human_decided_after_the_batch() {
        let database = db();
        database.insert_segment(&machine_segment("undo", "/undo.wav", "pre-batch draft")).unwrap();
        let pre_batch = database.get_segment_by_id("undo").unwrap().unwrap();

        assert!(database
            .update_batch_transcription_if_unreviewed("undo", "batch draft", None, Some(0.7), None, None, false)
            .unwrap());
        assert!(database.update_verified_for_test("undo", true).unwrap());

        let error = database.restore_batch_transcription_snapshot(&pre_batch).unwrap_err();
        assert!(error.to_string().contains("human decision landed after the batch"), "{error}");
        assert_eq!(
            database.get_segment_by_id("undo").unwrap().unwrap().raw_transcript,
            "batch draft",
            "the row the human judged must be exactly what stays on disk"
        );

        // Without a later human decision the undo still works — this is a fence, not a removal.
        database.insert_segment(&machine_segment("undo2", "/undo2.wav", "pre-batch draft")).unwrap();
        let pre_batch2 = database.get_segment_by_id("undo2").unwrap().unwrap();
        assert!(database
            .update_batch_transcription_if_unreviewed("undo2", "batch draft", None, Some(0.7), None, None, false)
            .unwrap());
        database.restore_batch_transcription_snapshot(&pre_batch2).unwrap();
        assert_eq!(database.get_segment_by_id("undo2").unwrap().unwrap().raw_transcript, "pre-batch draft");
    }

    /// Activity/throughput/agreement read reviewer identity the same way money does — COLLATE NOCASE.
    /// Case-sensitive, one person spelled two ways showed half their work beside the full balance.
    #[test]
    fn reviewer_activity_reads_fold_case_like_the_money_paths() {
        let database = db();
        for (i, spelling) in ["Sara", "sara"].into_iter().enumerate() {
            let id = format!("act{i}");
            database.insert_segment(&machine_segment(&id, &format!("/act{i}.wav"), "text")).unwrap();
            // Written straight to the audit trail: `record_review_event` is the zero-credit skip path,
            // and the paid writers need playback proof this fixture is not about.
            database
                .connection()
                .execute(
                    "INSERT INTO review_events (segment_id, reviewer, action, source, timestamp_ms, duration_ms)
                     VALUES (?1, ?2, 'accept', 'desktop', ?3, 4000)",
                    params![id, spelling, 1_000 + i as i64 * 60_000],
                )
                .unwrap();
            // Both spellings answer the SAME clip: case-sensitively that looked like two raters
            // agreeing with each other, which is one person's kappa against themselves.
            database
                .connection()
                .execute(
                    "INSERT INTO spot_checks
                        (segment_id, reviewer, action, submitted_transcript, expected_transcript, noticed, cer)
                     VALUES ('act0', ?1, 'accept', 'x', 'x', 1, 0.0)",
                    params![spelling],
                )
                .unwrap();
        }

        assert_eq!(database.reviewed_audio_ms("Sara").unwrap(), 8_000, "both spellings are one reviewer's activity");
        assert_eq!(database.reviewed_audio_ms("SARA").unwrap(), 8_000);

        let throughput = database.reviewer_throughput().unwrap();
        assert_eq!(throughput.len(), 1, "one human must not appear twice: {throughput:?}");
        assert_eq!(throughput[0].clips, 2);

        // Two spellings of one person are not two raters, so there is no agreement pair to report.
        assert!(
            database.agreement_sample().unwrap().is_none(),
            "kappa must never measure a reviewer against their own other spelling"
        );
    }

    /// The settlement writer raises `synchronous=FULL` on the SHARED connection. Every exit path has
    /// to lower it again — the `?` returns included, which the three explicit early returns missed.
    #[test]
    fn a_failed_settlement_restores_synchronous_on_the_shared_connection() {
        // A real file, not `:memory:` — `synchronous` is exactly the durability knob a memory database
        // has no reason to honour, and this test is about the knob.
        let temp = tempfile::tempdir().unwrap();
        let database = Database::open(temp.path().join("pay.db").to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database.insert_segment(&machine_segment("pay", "/pay.wav", "text")).unwrap();
        database
            .connection()
            .execute(
                "INSERT INTO review_compensation_ledger
                (entry_id, entry_key, policy_version, canonical_work_id, canonical_identity_kind,
                 reviewer, segment_id, source, compensation_action, effective_decision, duration_ms,
                 rate_basis_points, entitlement_micro_iqd, delta_micro_iqd, corrected_entitlement_ms,
                 delta_corrected_ms)
             VALUES ('e1', 'k1', ?1, 'pay', 'segment_id', 'Sara', 'pay', 'couch', 'accept', 'accept',
                     4000, 1000, 2000, 2000, 0, 0)",
                params![REVIEW_PAY_POLICY_VERSION],
            )
            .unwrap();

        // Boundary 5 is past the last ledger id, so the settlement trigger ABORTs the INSERT — the
        // `?` path that used to leave the connection pinned at FULL for the rest of the process.
        let error = database.record_review_compensation_settlement("Sara", 5, "payout-1").unwrap_err();
        assert!(error.to_string().contains("range/amount is invalid"), "{error}");
        let synchronous: i64 = database.connection().query_row("PRAGMA synchronous", [], |row| row.get(0)).unwrap();
        assert_eq!(synchronous, 1, "a failed money write must leave the shared connection at NORMAL, not FULL");

        // The happy path still settles, and still restores the pragma.
        let settlement = database.record_review_compensation_settlement("Sara", 1, "payout-2").unwrap();
        assert_eq!(settlement.allocated_micro_iqd, 2_000, "the settled amount is unchanged by this fix");
        let synchronous: i64 = database.connection().query_row("PRAGMA synchronous", [], |row| row.get(0)).unwrap();
        assert_eq!(synchronous, 1);
    }

    /// `create_or_get_job` must absorb the retry it exists for even when it loses the check-then-insert
    /// race: the loser used to receive a raw UNIQUE-constraint error instead of the winner's job.
    #[test]
    fn create_or_get_job_returns_the_existing_job_when_its_insert_conflicts() {
        let database = db();
        let first = database.create_or_get_job("job-a", "import", Some("key-1"), Some(10)).unwrap();

        // The lost race: a second caller passed the existence check before the winner committed, so it
        // reaches the INSERT with a fresh id and the SAME idempotency key.
        let second = database.create_or_get_job("job-b", "import", Some("key-1"), Some(10)).unwrap();
        assert_eq!(second.id, first.id, "the retry must resume the existing job, not fail or duplicate it");

        let jobs: i64 = database.connection().query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0)).unwrap();
        assert_eq!(jobs, 1, "no duplicate job row may be created");

        // A genuinely new key still creates a job.
        let other = database.create_or_get_job("job-c", "import", Some("key-2"), None).unwrap();
        assert_eq!(other.id, "job-c");
    }

    /// `record_spot_check` writes the PAID ledger with `operation: None`, which clears
    /// `enforce_production_proof` — no operation identity, no playback evidence. Nothing in the app
    /// calls it, so it must not be compiled into the production binary at all.
    #[test]
    fn the_proof_free_spot_check_writer_is_not_in_the_production_binary() {
        let source = include_str!("db/review.rs");
        let at = source.find("fn record_spot_check(").expect("record_spot_check must still exist");
        assert!(
            source[..at].lines().rev().take(4).any(|line| line.trim() == "#[cfg(test)]"),
            "record_spot_check lost its #[cfg(test)] gate — a proof-free paid-ledger writer is back in \
             the shipped app. Either gate it again or route it through the playback-proof writers."
        );
    }
}

#[cfg(test)]
#[path = "db_tests.rs"]
mod tests;
