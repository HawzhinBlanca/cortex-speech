use crate::error::{AppError, AppResult};
use crate::fingerprint::{AudioIdentity, StoredAudioIdentity};

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
use rusqlite::{backup, params, types::Value, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// One row of the `jobs` table as read: (id, kind, state, progress, completed, total, error_code).
type JobRow = (String, String, String, f64, i64, Option<i64>, Option<String>);

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
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
    /// Version of the Sorani normalizer used for normalized_transcript / metrics.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentsPage {
    pub items: Vec<SpeechSegment>,
    pub total: usize,
    pub next_cursor: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioHealth {
    pub total_files: usize,
    pub missing_files: usize,
    pub missing_paths: Vec<String>,
}

/// P3.3: outcome of a basename-based relink.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SegmentHypothesis {
    pub segment_id: String,
    pub model_id: String,
    pub transcript: String,
    pub confidence: Option<f64>,
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

pub struct Database {
    conn: Connection,
    path: String,
}

fn human_verdict_for_decision(decision: &str) -> AppResult<&'static str> {
    match decision {
        "accept" => Ok("human_accept"),
        "edit" => Ok("human_edit"),
        "reject" => Ok("human_reject"),
        other => Err(AppError::Validation(format!("Unknown human decision: {other}"))),
    }
}

/// The speech_segments columns loaded when recording a human decision, in SELECT order:
/// `(is_gold, raw_transcript, normalized_transcript, annotated_transcript, verdict_transcript,
/// prior_verdict, audio_path, model_version_id)`. Aliased to keep the `query_row` annotation
/// readable (clippy::type_complexity).
type HumanDecisionContext =
    (i32, String, Option<String>, Option<String>, Option<String>, Option<String>, String, String);

/// Canonicalize stored text to Unicode NFC. Sorani/Arabic combining marks (diacritics,
/// madda, hamza) can arrive decomposed from ASR or import; storing inconsistent forms
/// silently fragments FTS search, content-dedup, and WER references that all assume one
/// canonical spelling. Idempotent — NFC of already-NFC text is unchanged.
/// `pub(crate)` so sibling modules can canonicalize the SAME way before comparing against a stored
/// transcript. `couch.rs` needs it to tell a network retry apart from a genuine re-review: the write
/// path NFC-normalizes, so a decomposed (NFD) paste from a phone IME would otherwise never compare
/// equal to the value it just stored, and every retry would look like a brand-new decision.
/// Bump when the sufficiency RULE changes. Stored on every receipt so a row always says which rule
/// it satisfied, instead of being re-judged later under one it never met.
pub const PLAYBACK_POLICY_VERSION: i64 = 1;

/// How much of the clip's media time must actually have been played.
///
/// Not 100%: a reviewer who has heard the sentence often stops before the trailing silence, and
/// demanding the last frame would train them to leave it running rather than to listen. Not 50%
/// either — half a Sorani sentence is exactly where a plausible-but-wrong verdict comes from.
///
/// 0.85, set by the owner 2026-08-19 against the first real device evidence. The bar opened at 0.90
/// and the observe-mode pilot measured 12 receipts from a live phone:
///
///   1.000 ×6 (replays, capped)   0.974   0.968   0.915   0.907   0.856   0.352
///
/// Two would have been refused at 0.90 and two more sat within 0.015 of it — roughly one honest
/// decision in six rejected, on a flat ratio that punishes short clips hardest (the 0.856 case was a
/// 3.4 s clip missing its last 489 ms of trailing silence). 0.85 keeps the 0.352 refusal, which is
/// the case the guard exists for, and stops rejecting reviewers who heard the whole sentence.
///
/// Twelve samples from ONE device is a thin basis for a calibrated number. Revisit once the pilot
/// has 20+ decisions across both reviewers; a length-aware rule (ratio OR a fixed unheard-tail
/// allowance) is the likelier long-run answer than any single ratio.
pub const MIN_PLAYBACK_COVERAGE: f64 = 0.85;

/// A reviewer's record of having heard one clip at one revision.
#[derive(Debug, Clone)]
pub struct PlaybackReceipt {
    pub segment_id: String,
    pub segment_revision: i64,
    pub audio_fingerprint: String,
    pub reviewer: Option<String>,
    pub session_id: Option<String>,
    pub started_at_ms: i64,
    pub played_ms: i64,
    pub clip_duration_ms: i64,
}

pub(crate) fn to_nfc(s: &str) -> String {
    s.nfc().collect()
}

/// NFC-canonicalize a segment's three transcript fields for storage.
fn nfc_transcripts(seg: &SpeechSegment) -> (String, Option<String>, Option<String>) {
    (
        to_nfc(&seg.raw_transcript),
        seg.normalized_transcript.as_deref().map(to_nfc),
        seg.annotated_transcript.as_deref().map(to_nfc),
    )
}

/// Fold Sorani codepoint variants (Kaf ك/ک, Yeh ي/ی, Heh, Hamza, ZWNJ, tatweel) in a
/// full-text search query so it matches the canonical `normalized_transcript` column
/// regardless of which keyboard variant the user typed — the FTS index stores the
/// normalizer's unified form, but a raw query in a different codepoint would never
/// match it. Digit conversion/verbalization is intentionally skipped so a digit query
/// still matches the raw transcript; the letter rules mirror those applied to the
/// stored normalized text.
fn normalize_search_query(text: &str) -> String {
    crate::normalizer::SoraniNormalizer::with_config(crate::normalizer::NormalizationConfig {
        normalize_numbers: false,
        verbalize_numbers: false,
        normalize_hamza: true,
        remove_diacritics: false,
    })
    .normalize(text)
}

/// Convert free-text search input into a SAFE FTS5 `MATCH` string. FTS5 parses the bound value as a
/// full-text *query*, so bare metacharacters (`"` `:` `*` `(` `)` `^` `-` and the bareword keywords
/// `AND`/`OR`/`NEAR`) make it raise a hard error on ordinary punctuation — e.g. a user typing a
/// single `"` or `:` in the transcript search box would get a low-level "fts5: syntax error" instead
/// of results. We treat the box as literal text: split on whitespace and wrap each token as a quoted
/// FTS5 string (internal `"` doubled), which FTS5 reads as a literal term. Tokens are implicitly
/// AND-ed (matching the previous behaviour for multi-word queries). Returns `""` for whitespace-only
/// input so the caller can short-circuit to an empty result.
fn to_fts5_match(query: &str) -> String {
    // Control characters (NUL and other C0/C1) are never meaningful search terms and an embedded
    // NUL makes SQLite/FTS5 raise a hard error (interior NUL in a bound string), so map every
    // control char to a separator before tokenizing — they can't survive into the MATCH string.
    let cleaned: String = query.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    cleaned
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The only split labels the export/stats math understands.
const VALID_SPLITS: &[&str] = &["train", "validation", "test"];

/// Longest gap still counted as "one reviewing session" for per-reviewer throughput (Migration v45).
/// Matches `stats.rs::SESSION_GAP_MS` deliberately, so the phone figure and the desktop figure mean
/// the same thing; a reviewer who closes the page and returns tomorrow did not spend fourteen hours
/// on one clip.
const REVIEW_SESSION_GAP_MS: i64 = 300_000; // 5 minutes

const SEGMENT_SELECT_COLUMNS: &str = "id, created_at, audio_path, raw_transcript, normalized_transcript,
                    annotated_transcript, alignment_json, duration_ms, speaker_id, verified,
                    confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                    verdict, verdict_transcript, rationale, evidence_json,
                    agreement_score, escalated, human_decision, corrected_at, is_gold,
                    alignment_quality, model_version_id, confidence_source, cloud_call,
                    decoder_config_hash, normalizer_version, denoised, diarized, vad_backend,
                    reviewed_by, speaker_change_score";

// List rows preserve the SpeechSegment wire shape but omit large payloads that are hydrated only
// when a row is selected.
const SEGMENT_LIST_SELECT_COLUMNS: &str = "id, created_at, audio_path, raw_transcript, normalized_transcript,
                    annotated_transcript, NULL AS alignment_json, duration_ms, speaker_id, verified,
                    confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                    verdict, verdict_transcript, rationale, NULL AS evidence_json,
                    agreement_score, escalated, human_decision, corrected_at, is_gold,
                    alignment_quality, model_version_id, confidence_source, cloud_call,
                    decoder_config_hash, normalizer_version, denoised, diarized, vad_backend,
                    reviewed_by, speaker_change_score";

fn canonical_segment_sort(sort: &str) -> &str {
    match sort {
        "active_learning" => "activeLearning",
        "suspect_first" => "suspectFirst",
        "oldest" | "duration" | "verified" | "confidence" | "activeLearning" | "suspectFirst" => sort,
        _ => "newest",
    }
}

fn segment_page_scope(verified: Option<bool>, text_query: Option<&str>) -> String {
    let mut hash = Sha256::new();
    hash.update(match verified {
        Some(true) => b"verified:1".as_slice(),
        Some(false) => b"verified:0".as_slice(),
        None => b"verified:*".as_slice(),
    });
    hash.update(b"\0query:");
    hash.update(text_query.unwrap_or_default().trim().as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.finalize())
}

fn decode_segment_cursor(raw: &str) -> AppResult<SegmentPageCursor> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| AppError::Validation("Invalid segment page cursor".into()))?;
    let cursor: SegmentPageCursor =
        serde_json::from_slice(&bytes).map_err(|_| AppError::Validation("Invalid segment page cursor".into()))?;
    if cursor.version != 1 || cursor.anchor_rowid < 0 || cursor.last.id.is_empty() {
        return Err(AppError::Validation("Unsupported segment page cursor".into()));
    }
    Ok(cursor)
}

fn encode_segment_cursor(cursor: &SegmentPageCursor) -> AppResult<String> {
    let bytes =
        serde_json::to_vec(cursor).map_err(|e| AppError::Validation(format!("Could not encode page cursor: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Reject structurally-invalid segments at the DB write boundary, before they can
/// corrupt the downstream split/stats/training-grade math that every later stage
/// branches on. Guards the fields these insert paths actually persist; verdict and
/// human_decision are validated at their own dedicated write paths.
fn validate_segment(seg: &SpeechSegment) -> AppResult<()> {
    if seg.id.trim().is_empty() {
        return Err(AppError::Validation("Segment id must not be empty".into()));
    }
    // P1.1: reject a UNC/network audio_path at the shared DB write boundary (covers merge_dataset_json
    // and every other insert path). A renderer-planted `\\attacker\share\clip.wav` would otherwise flow
    // into the row and drive the SMB redirector (NTLM forced-auth leak) the moment any downstream
    // consumer (validate_dataset, compute_acoustic_scores, decode) touches it. Syntactic, zero I/O; an
    // empty audio_path is allowed (not a UNC path). Mirrors the export-side guard shipped in #131.
    crate::validation::input::reject_unc_path(&seg.audio_path).map_err(AppError::Validation)?;
    if seg.duration_ms < 0 {
        return Err(AppError::Validation(format!(
            "Segment '{}' has a negative duration_ms ({})",
            seg.id, seg.duration_ms
        )));
    }
    if let Some(alignment) = seg.alignment_json.as_deref() {
        crate::validation::input::validate_alignment_json(alignment).map_err(AppError::Validation)?;
    }
    if let Some(split) = seg.split.as_deref() {
        if !VALID_SPLITS.contains(&split) {
            return Err(AppError::Validation(format!(
                "Segment '{}' has invalid split '{split}' (expected one of {VALID_SPLITS:?})",
                seg.id
            )));
        }
    }
    Ok(())
}

use crate::normalizer::learning_text_key;

/// Suspect-first review ordering (external review 2026-08-06 #2).
///
/// `agreement_score` on an escalated row is the IRT model-AGREEMENT confidence, and agreement is not
/// trustworthiness. A clip escalated because its AUDIO is poor — `has_hard_distrust_veto`'s snr < 5 dB
/// or clipping > 0.1 — can carry 0.97 there because every recognizer confidently agreed on the same
/// garbage. Ordering by that value alone sent exactly the clips the gate refused to trust to the BACK
/// of a queue whose whole purpose is riskiest-first.
///
/// So acoustic quality is ranked BEFORE agreement rather than being collapsed into it. The signals stay
/// separate — this reads them separately instead of averaging them into a single number that means
/// neither thing. No new column: snr_db and clipping_ratio are already on the row, which is why the
/// data to do this properly was there all along.
///
/// Order: escalated first; within that, poor audio first; then genuine disagreement (low agreement);
/// then newest.
/// P1.2: BUILT from `quality::POOR_AUDIO_*` rather than repeating the numbers. This string used to
/// carry its own hand-typed `< 5.0` / `> 0.1`, so changing the jury's veto thresholds silently left the
/// review queue ordering on the old rule — the queue would stop leading with the clips the gate had
/// just started distrusting, and nothing would report it.
///
/// The COALESCE defaults are deliberately "fine" values (99.0 dB SNR, 0.0 clipping) so an UNMEASURED
/// row sorts as unremarkable rather than as poor audio, matching `quality::has_poor_audio` treating
/// `None` as not-poor. Absence of a measurement is not evidence of bad audio.
static SUSPECT_FIRST_ORDER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "escalated DESC, \
         (CASE WHEN COALESCE(snr_db, 99.0) < {snr} OR COALESCE(clipping_ratio, 0.0) > {clip} THEN 0 ELSE 1 END) ASC, \
         COALESCE(agreement_score, 0.5) ASC, \
         datetime(created_at) DESC, id ASC",
        snr = crate::quality::POOR_AUDIO_SNR_DB,
        clip = crate::quality::POOR_AUDIO_CLIPPING_RATIO,
    )
});

fn rejected_transcript_for_learning(corrected: &str, candidates: &[Option<String>]) -> Option<String> {
    let corrected_key = learning_text_key(corrected);
    candidates.iter().filter_map(|candidate| candidate.as_deref()).find_map(|candidate| {
        let trimmed = candidate.trim();
        if trimmed.is_empty() || learning_text_key(trimmed) == corrected_key {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

impl Database {
    pub fn open(path: &str) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA cache_size=-64000;
             PRAGMA busy_timeout=10000;",
        )?;
        Ok(Self { conn, path: path.to_string() })
    }

    /// Open the database with a retry policy for corruption.
    ///
    /// Recovery is fail-CLOSED: `recover_database_at` is DESTRUCTIVE (it renames the live db away and
    /// opens a fresh empty one), so it must fire ONLY on genuine corruption. A transient error — an
    /// external process holding the file locked past busy_timeout, a disk I/O hiccup, OOM during the
    /// integrity check — must NOT quarantine a healthy database (that would be silent data loss);
    /// instead it aborts startup so the user can clear the locker / fix the disk and retry intact.
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
                        tracing::error!("Database integrity check returned a transient I/O message (not corruption); aborting startup without quarantine: {result}");
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
                        tracing::error!("Database integrity check could not complete (transient, not corruption); aborting startup without quarantine: {e}");
                        return Err(e);
                    }
                }
                drop(db);
                recover_database_at(path)?;
                Self::open(path)
            }
            Err(e) if is_corruption_error(&e) => {
                tracing::error!("Failed to open database with a corruption code: {e}. Attempting recovery...");
                recover_database_at(path)?;
                Self::open(path)
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
        crate::migrations::run_migrations(self)?;
        Ok(())
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

    /// ASR-side insert/upsert. On id conflict it rewrites the ASR-owned columns (transcripts, audio,
    /// acoustic metrics, provenance) and DELIBERATELY omits every jury / human-decision / gold column
    /// (verdict*, human_decision, corrected_at, is_gold, ...) and `created_at` — those survive an upsert
    /// (pinned by the history-restore tests; full-row restore is [`Self::resurrect_segment_snapshot`]).
    ///
    /// CALLER CONTRACT (anti-clobber): `annotated_transcript`, `verified` and `speaker_id` ARE in the
    /// update list, so never call this with a row snapshot held across long/await-able work — a
    /// concurrent human edit to those columns would be silently reverted (the rediarize bug). Re-read
    /// the row at persist time, or use a targeted update (`update_speaker_id`,
    /// `update_asr_transcript_if_unreviewed`, ...). Audited call sites: batch normalize + couch submit
    /// re-read fresh; history/couch undo restore a snapshot BY DESIGN; imports build fresh rows.
    pub fn insert_segment(&self, seg: &SpeechSegment) -> AppResult<()> {
        validate_segment(seg)?;
        let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, audio_path, raw_transcript, normalized_transcript,
                 annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score, alignment_quality,
                 model_version_id, confidence_source, cloud_call, decoder_config_hash, normalizer_version, denoised, diarized, vad_backend)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, COALESCE(?18, 'unknown@pre-registry'), COALESCE(?19, 'unknown'), ?20, ?21, ?22, ?23, ?24, ?25)
             ON CONFLICT(id) DO UPDATE SET
                audio_path=excluded.audio_path,
                raw_transcript=excluded.raw_transcript,
                normalized_transcript=excluded.normalized_transcript,
                annotated_transcript=excluded.annotated_transcript,
                alignment_json=excluded.alignment_json,
                duration_ms=excluded.duration_ms,
                speaker_id=excluded.speaker_id,
                verified=excluded.verified,
                confidence=excluded.confidence,
                ctc_score=excluded.ctc_score,
                clipping_ratio=excluded.clipping_ratio,
                rms_db=excluded.rms_db,
                snr_db=excluded.snr_db,
                split=excluded.split,
                signal_anomaly_score=excluded.signal_anomaly_score,
                alignment_quality=excluded.alignment_quality,
                model_version_id=excluded.model_version_id,
                confidence_source=excluded.confidence_source,
                cloud_call=excluded.cloud_call,
                decoder_config_hash=excluded.decoder_config_hash,
                normalizer_version=excluded.normalizer_version,
                denoised=excluded.denoised,
                diarized=excluded.diarized,
                vad_backend=excluded.vad_backend,
                updated_at=datetime('now')",
            params![
                seg.id, seg.audio_path, raw_nfc,
                normalized_nfc, annotated_nfc,
                seg.alignment_json, seg.duration_ms, seg.speaker_id,
                seg.verified as i32, seg.confidence, seg.ctc_score,
                seg.clipping_ratio, seg.rms_db, seg.snr_db, seg.split,
                seg.signal_anomaly_score, seg.alignment_quality,
                seg.model_version_id,
                seg.confidence_source,
                seg.cloud_call as i32,
                seg.decoder_config_hash,
                seg.normalizer_version,
                seg.denoised.map(|b| b as i32),
                seg.diarized.map(|b| b as i32),
                seg.vad_backend,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Resurrect a HARD-DELETED segment from a full in-memory snapshot, persisting EVERY column the
    /// snapshot carries — including the jury / human-review / gold-provenance fields (verdict,
    /// verdict_transcript, rationale, evidence_json, agreement_score, escalated, human_decision,
    /// corrected_at, is_gold) and `created_at` that [`insert_segment`] deliberately omits.
    ///
    /// [`insert_segment`]'s 17-column subset is correct for the normal edit path, where the row still
    /// exists and its `ON CONFLICT DO UPDATE` branch leaves the untouched columns intact. But undoing a
    /// deletion runs as a *fresh* INSERT (the row was physically removed by `delete_segment` /
    /// `delete_segments_batch`), so anything `insert_segment` skips would silently revert to its schema
    /// default: verdict/human_decision/is_gold/corrected_at → NULL/0 and `created_at` → datetime('now'),
    /// reordering the row in every `ORDER BY created_at` query and export. This method writes the whole
    /// row so a restore is lossless. `created_at` falls back to `datetime('now')` only when the snapshot
    /// genuinely lacks one (the column is NOT NULL).
    pub fn insert_segment_full(&self, seg: &SpeechSegment) -> AppResult<()> {
        validate_segment(seg)?;
        let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
        // NFC-normalize the jury verdict transcript too, so a restored row stays byte-consistent with
        // the rest of the (already NFC) transcript columns.
        let verdict_transcript_nfc = seg.verdict_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, created_at, audio_path, raw_transcript, normalized_transcript,
                 annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence,
                 ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                 verdict, verdict_transcript, rationale, evidence_json, agreement_score, escalated,
                 human_decision, corrected_at, is_gold, alignment_quality, model_version_id,
                 confidence_source, cloud_call, decoder_config_hash, normalizer_version, denoised, diarized, vad_backend,
                 reviewed_by, speaker_change_score, updated_at)
             VALUES (?1, COALESCE(?2, datetime('now')), ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                 COALESCE(?28, 'unknown@pre-registry'), COALESCE(?29, 'unknown'), ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                created_at=excluded.created_at,
                audio_path=excluded.audio_path,
                raw_transcript=excluded.raw_transcript,
                normalized_transcript=excluded.normalized_transcript,
                annotated_transcript=excluded.annotated_transcript,
                alignment_json=excluded.alignment_json,
                duration_ms=excluded.duration_ms,
                speaker_id=excluded.speaker_id,
                verified=excluded.verified,
                confidence=excluded.confidence,
                ctc_score=excluded.ctc_score,
                clipping_ratio=excluded.clipping_ratio,
                rms_db=excluded.rms_db,
                snr_db=excluded.snr_db,
                split=excluded.split,
                signal_anomaly_score=excluded.signal_anomaly_score,
                verdict=excluded.verdict,
                verdict_transcript=excluded.verdict_transcript,
                rationale=excluded.rationale,
                evidence_json=excluded.evidence_json,
                agreement_score=excluded.agreement_score,
                escalated=excluded.escalated,
                human_decision=excluded.human_decision,
                corrected_at=excluded.corrected_at,
                is_gold=excluded.is_gold,
                alignment_quality=excluded.alignment_quality,
                model_version_id=excluded.model_version_id,
                confidence_source=excluded.confidence_source,
                cloud_call=excluded.cloud_call,
                decoder_config_hash=excluded.decoder_config_hash,
                normalizer_version=excluded.normalizer_version,
                denoised=excluded.denoised,
                diarized=excluded.diarized,
                vad_backend=excluded.vad_backend,
                reviewed_by=excluded.reviewed_by,
                speaker_change_score=excluded.speaker_change_score,
                updated_at=datetime('now')",
            params![
                seg.id,
                seg.created_at,
                seg.audio_path,
                raw_nfc,
                normalized_nfc,
                annotated_nfc,
                seg.alignment_json,
                seg.duration_ms,
                seg.speaker_id,
                seg.verified as i32,
                seg.confidence,
                seg.ctc_score,
                seg.clipping_ratio,
                seg.rms_db,
                seg.snr_db,
                seg.split,
                seg.signal_anomaly_score,
                seg.verdict,
                verdict_transcript_nfc,
                seg.rationale,
                seg.evidence_json,
                seg.agreement_score,
                seg.escalated as i32,
                seg.human_decision,
                seg.corrected_at,
                seg.is_gold as i32,
                seg.alignment_quality,
                seg.model_version_id,
                seg.confidence_source,
                seg.cloud_call as i32,
                seg.decoder_config_hash,
                seg.normalizer_version,
                seg.denoised.map(|b| b as i32),
                seg.diarized.map(|b| b as i32),
                seg.vad_backend,
                seg.reviewed_by,
                // Restoring a deleted clip must bring its measurement back with it. A fresh INSERT
                // takes the schema default for anything omitted, so leaving this out would silently
                // un-flag a two-speaker clip the moment it was deleted and undone.
                seg.speaker_change_score,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Targeted single-column update: sets `verified` without touching any other field.
    /// Returns true if the row was found and updated.
    pub fn update_verified(&self, id: &str, verified: bool) -> AppResult<bool> {
        let rows = self.conn.execute(
            "UPDATE speech_segments SET verified = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, verified as i32],
        )?;
        Ok(rows > 0)
    }

    /// Targeted single-column update: sets `speaker_id` without touching any other field.
    /// Pass `None` to clear the speaker assignment.
    /// Returns true if the row was found and updated.
    pub fn update_speaker_id(&self, id: &str, speaker_id: Option<&str>) -> AppResult<bool> {
        let rows = self.conn.execute(
            "UPDATE speech_segments SET speaker_id = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, speaker_id],
        )?;
        Ok(rows > 0)
    }

    /// Targeted single-column update: sets `normalized_transcript` (the normalized ASR draft) without
    /// touching the human's answer (annotated_transcript / verdict) or any other field. Returns true
    /// if the row was found and updated. Used by batch_normalize instead of a read-modify-write +
    /// whole-row insert_segment upsert, which could clobber a concurrent write between the re-read and
    /// the write (the anti-clobber discipline the sibling batch updates already follow).
    pub fn update_normalized_transcript(&self, id: &str, normalized: &str) -> AppResult<bool> {
        let rows = self.conn.execute(
            "UPDATE speech_segments SET normalized_transcript = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, normalized],
        )?;
        Ok(rows > 0)
    }

    pub fn insert_segments_batch(&self, segments: &[SpeechSegment]) -> AppResult<()> {
        // Use a SAVEPOINT on the shared connection — avoids opening a second
        // file handle that could race with other writers under WAL mode.
        self.conn.execute("SAVEPOINT batch_insert", [])?;
        let result: AppResult<()> = (|| {
            let mut stmt = self.conn.prepare(
                "INSERT INTO speech_segments
                    (id, audio_path, raw_transcript, normalized_transcript,
                     annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                     model_version_id, confidence_source, cloud_call, decoder_config_hash, normalizer_version, denoised, diarized, vad_backend)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, COALESCE(?17, 'unknown@pre-registry'), COALESCE(?18, 'unknown'), ?19, ?20, ?21, ?22, ?23, ?24)
                 ON CONFLICT(id) DO UPDATE SET
                    audio_path=excluded.audio_path,
                    raw_transcript=excluded.raw_transcript,
                    normalized_transcript=excluded.normalized_transcript,
                    annotated_transcript=excluded.annotated_transcript,
                    alignment_json=excluded.alignment_json,
                    duration_ms=excluded.duration_ms,
                    speaker_id=excluded.speaker_id,
                    verified=excluded.verified,
                    confidence=excluded.confidence,
                    ctc_score=excluded.ctc_score,
                    clipping_ratio=excluded.clipping_ratio,
                    rms_db=excluded.rms_db,
                    snr_db=excluded.snr_db,
                    split=excluded.split,
                    signal_anomaly_score=excluded.signal_anomaly_score,
                    model_version_id=excluded.model_version_id,
                    confidence_source=excluded.confidence_source,
                    cloud_call=excluded.cloud_call,
                    decoder_config_hash=excluded.decoder_config_hash,
                    normalizer_version=excluded.normalizer_version,
                    denoised=excluded.denoised,
                    diarized=excluded.diarized,
                    vad_backend=excluded.vad_backend,
                    updated_at=datetime('now')"
            )?;
            for seg in segments {
                validate_segment(seg)?;
                let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
                stmt.execute(params![
                    seg.id,
                    seg.audio_path,
                    raw_nfc,
                    normalized_nfc,
                    annotated_nfc,
                    seg.alignment_json,
                    seg.duration_ms,
                    seg.speaker_id,
                    seg.verified as i32,
                    seg.confidence,
                    seg.ctc_score,
                    seg.clipping_ratio,
                    seg.rms_db,
                    seg.snr_db,
                    seg.split,
                    seg.signal_anomaly_score,
                    seg.model_version_id,
                    seg.confidence_source,
                    seg.cloud_call as i32,
                    seg.decoder_config_hash,
                    seg.normalizer_version,
                    seg.denoised.map(|b| b as i32),
                    seg.diarized.map(|b| b as i32),
                    seg.vad_backend,
                ])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("batch_insert")?;
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("batch_insert");
                Err(e)
            }
        }
    }

    pub fn merge_dataset_json(&self, json_content: &str) -> AppResult<(usize, usize)> {
        let external_segments: Vec<SpeechSegment> = serde_json::from_str(json_content)?;
        let mut updated = 0;
        let mut created = 0;

        self.conn.execute("SAVEPOINT merge_json", [])?;
        let result: AppResult<()> = (|| {
            let mut check_stmt = self.conn.prepare("SELECT id FROM speech_segments WHERE id = ?1")?;
            // Guard: never overwrite a human's reviewed row; only update unreviewed ones. verified = 0 is
            // load-bearing alongside the human_decision/verdict checks: "Verify"/"Verify selected"
            // (batch_verify -> update_verified) sets ONLY verified=1 and leaves human_decision/verdict NULL,
            // so without this clause a pasted-dataset merge would overwrite a human-VERIFIED row's transcript
            // (and its verified flag) with imported machine text — silently destroying reviewed work and, if
            // the imported row carries verified=true, shipping unapproved text as human-verified GOLD. Mirrors
            // the sibling update_asr_transcript_if_unreviewed / update_batch_transcription_if_unreviewed
            // guards (the merge path was simply never given the clause). Importing a NEW verified row (an id
            // not present locally) still works — this only refuses to OVERWRITE an existing reviewed row.
            let mut update_stmt = self.conn.prepare(
                "UPDATE speech_segments SET
                    audio_path=?2, raw_transcript=?3, normalized_transcript=?4,
                    annotated_transcript=?5, alignment_json=?6, duration_ms=?7,
                    speaker_id=?8, verified=?9, confidence=?10, ctc_score=?11,
                    clipping_ratio=?12, rms_db=?13, snr_db=?14, split=?15, signal_anomaly_score=?16,
                    model_version_id=COALESCE(?17, 'unknown@pre-registry'),
                    confidence_source=COALESCE(?18, 'unknown'),
                    cloud_call=?19,
                    decoder_config_hash=?20,
                    normalizer_version=?21,
                    updated_at=datetime('now')
                 WHERE id=?1
                   AND verified = 0
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            )?;

            for seg in &external_segments {
                validate_segment(seg)?;
                let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
                let exists = check_stmt.exists(params![seg.id])?;
                if exists {
                    // Count only rows the guard actually changed — a human-reviewed row matches 0
                    // rows here (the UPDATE skips it), so it must not be reported as "updated".
                    let changed = update_stmt.execute(params![
                        seg.id,
                        seg.audio_path,
                        raw_nfc,
                        normalized_nfc,
                        annotated_nfc,
                        seg.alignment_json,
                        seg.duration_ms,
                        seg.speaker_id,
                        seg.verified as i32,
                        seg.confidence,
                        seg.ctc_score,
                        seg.clipping_ratio,
                        seg.rms_db,
                        seg.snr_db,
                        seg.split,
                        seg.signal_anomaly_score,
                        seg.model_version_id,
                        seg.confidence_source,
                        seg.cloud_call as i32,
                        seg.decoder_config_hash,
                        seg.normalizer_version,
                    ])?;
                    if changed > 0 {
                        updated += 1;
                    }
                } else {
                    // Lossless full-column insert for NEW ids. SpeechSegment deserializes every jury /
                    // human-review / gold column, and a merged dataset can carry reviewed rows — the old
                    // ASR-column-only INSERT silently dropped verdict/human_decision/is_gold/
                    // corrected_at/alignment_quality/created_at, stripping the human work product so the
                    // merged rows graded as unreviewed machine drafts. A new id has no local state to
                    // protect, so persisting the whole snapshot is unconditionally correct (the guarded
                    // UPDATE above keeps its unreviewed-only, ASR-columns-only semantics for EXISTING
                    // rows — external jury state must not overwrite local jury state).
                    self.insert_segment_full(seg)?;
                    created += 1;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("merge_json")?;
                self.track_write()?;
                Ok((created, updated))
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("merge_json");
                Err(e)
            }
        }
    }

    /// Safely update the ASR transcript for a segment only if a human has NOT
    /// already reviewed it. This is the correct API for the WSL 7B branch to
    /// persist refined transcripts without overwriting user edits.
    #[allow(clippy::too_many_arguments)]
    pub fn update_asr_transcript_if_unreviewed(
        &self,
        segment_id: &str,
        raw_transcript: &str,
        normalized_transcript: Option<&str>,
        confidence: Option<f64>,
        confidence_source: Option<&str>,
        model_version_id: Option<&str>,
        cloud_call: bool,
    ) -> AppResult<bool> {
        // NFC-canonicalize before writing the FTS-indexed columns, exactly like insert_segment /
        // update_segment. The WSL 7B branch feeds raw ASR output here, which can arrive decomposed;
        // storing a non-NFC form fragments the search index so the text can't be found.
        let raw_nfc = to_nfc(raw_transcript);
        let normalized_nfc = normalized_transcript.map(to_nfc);
        let rows_changed = self.conn.execute(
            "UPDATE speech_segments
             SET raw_transcript        = ?2,
                 normalized_transcript = ?3,
                 confidence            = ?4,
                 confidence_source     = COALESCE(?5, 'unknown'),
                 model_version_id      = COALESCE(?6, 'unknown@pre-registry'),
                 cloud_call            = ?7,
                 updated_at            = datetime('now')
             WHERE id = ?1
               -- verified = 0: a human who clicked \"Verify\"/\"Verify selected\" (batch_verify ->
               -- update_verified) sets ONLY `verified`, leaving human_decision/verdict NULL. Without this
               -- clause the background WSL-7B refinement loop (which snapshots empty-transcript targets at
               -- start) would reach a segment the human re-transcribed + verified mid-run and silently
               -- overwrite its raw/normalized transcript with unapproved 7B text, while the row stays
               -- verified=1 and still exports as human-verified GOLD. Mirrors the sibling
               -- update_batch_transcription_if_unreviewed's guard for the identical race.
               AND verified = 0
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            params![
                segment_id,
                raw_nfc,
                normalized_nfc,
                confidence,
                confidence_source,
                model_version_id,
                cloud_call as i32,
            ],
        )?;
        self.track_write()?;
        Ok(rows_changed > 0)
    }

    /// Atomically commit a champion transcript and make its hypothesis the segment's sole vote.
    ///
    /// The champion runs outside SQLite, so a reviewer may verify or decide the segment while inference
    /// is in flight. The guarded update is the compare-and-swap boundary: `Ok(false)` means the row still
    /// exists but became human-owned before this commit. In that case its transcript and all existing
    /// hypotheses remain untouched. A missing segment is an error rather than being misreported as a
    /// review race.
    ///
    /// Transcript/provenance replacement and stale-hypothesis cleanup share one savepoint. If deleting
    /// or inserting the sole champion hypothesis fails, the transcript update is rolled back too.
    pub fn commit_champion_transcript_if_unreviewed(
        &self,
        champion: &SegmentHypothesis,
        expected_deployment_sha256: Option<&str>,
        normalized_transcript: Option<&str>,
        confidence_source: Option<&str>,
        cloud_call: bool,
    ) -> AppResult<bool> {
        crate::validation::input::validate_identifier(&champion.segment_id).map_err(AppError::Validation)?;
        crate::validation::input::validate_identifier(&champion.model_id).map_err(AppError::Validation)?;
        crate::validation::input::validate_text(&champion.transcript, 100_000, "Champion transcript")
            .map_err(AppError::Validation)?;
        if let Some(sha) = expected_deployment_sha256 {
            if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return Err(AppError::Validation(
                    "Champion deployment identity must be a canonical lowercase SHA-256".into(),
                ));
            }
        }
        if let Some(normalized) = normalized_transcript {
            crate::validation::input::validate_text(normalized, 100_000, "Normalized transcript")
                .map_err(AppError::Validation)?;
        }

        let transcript_nfc = to_nfc(&champion.transcript);
        let normalized_nfc = normalized_transcript.map(to_nfc);
        self.conn.execute("SAVEPOINT champion_commit", [])?;
        let result = (|| -> AppResult<bool> {
            if let Some(expected_sha) = expected_deployment_sha256 {
                let identity_is_current: bool = self.conn.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM model_versions
                        WHERE id = ?1 AND family = 'omniasr-7b' AND status = 'champion'
                          AND checkpoint_sha256 = ?2
                    )",
                    params![champion.model_id, expected_sha],
                    |row| row.get(0),
                )?;
                if !identity_is_current {
                    return Err(AppError::Validation(format!(
                        "MODEL_IDENTITY_CHANGED: refusing transcript from model '{}' deployment '{}' because it is not the current registry champion",
                        champion.model_id, expected_sha
                    )));
                }
            }
            let rows_changed = self.conn.execute(
                "UPDATE speech_segments
                 SET raw_transcript        = ?2,
                     normalized_transcript = ?3,
                     confidence            = ?4,
                     confidence_source     = COALESCE(?5, 'unknown'),
                     model_version_id      = ?6,
                     cloud_call            = ?7,
                     updated_at            = datetime('now')
                 WHERE id = ?1
                   AND verified = 0
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
                params![
                    champion.segment_id,
                    transcript_nfc,
                    normalized_nfc,
                    champion.confidence,
                    confidence_source,
                    champion.model_id,
                    cloud_call as i32,
                ],
            )?;

            if rows_changed == 0 {
                let exists: bool = self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM speech_segments WHERE id = ?1)",
                    params![champion.segment_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(AppError::Validation(format!(
                        "Cannot commit champion transcript: segment '{}' does not exist",
                        champion.segment_id
                    )));
                }
                return Ok(false);
            }

            self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id = ?1", params![champion.segment_id])?;
            self.conn.execute(
                "INSERT INTO segment_hypotheses
                    (segment_id, model_id, transcript, confidence, model_version_id)
                 VALUES (?1, ?2, ?3, ?4, ?2)",
                params![champion.segment_id, champion.model_id, transcript_nfc, champion.confidence],
            )?;
            Ok(true)
        })();

        match result {
            Ok(committed) => {
                self.release_savepoint("champion_commit")?;
                if committed {
                    self.track_write()?;
                }
                Ok(committed)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("champion_commit");
                Err(error)
            }
        }
    }

    /// Persist a batch (re)transcription result WITHOUT clobbering concurrent human work.
    ///
    /// Batch transcription runs in a background thread off a snapshot taken at batch start; a human can
    /// verify or edit a target segment while the batch is in flight. Writing the whole stale snapshot
    /// back (the old `insert_segment` path) reverted the human's `verified` flag and overwrote their
    /// edited annotation — a silent lost update. This targeted write instead:
    ///   • updates ONLY the ASR-derived columns (raw / normalized / confidence),
    ///   • NEVER touches `annotated_transcript` — that field is human-only, by law. This function
    ///     used to seed it with the machine draft via `COALESCE(annotated_transcript, seed)`, and
    ///     because every serving path ranks annotated first on presence alone, that first machine
    ///     seed outranked every later champion re-draft FOREVER: the 2026-08-12 incident, where 348
    ///     review clips served a stale machine paraphrase while fresh champion text sat invisible.
    ///     Pinned by scripts/test_machine_never_writes_annotated_policy.py.
    ///   • never touches `verified`, and
    ///   • skips any row a human has verified or reviewed since the batch began.
    /// Returns Ok(true) if the row was updated, Ok(false) if it was skipped as human-owned.
    #[allow(clippy::too_many_arguments)]
    pub fn update_batch_transcription_if_unreviewed(
        &self,
        segment_id: &str,
        raw_transcript: &str,
        normalized_transcript: Option<&str>,
        confidence: Option<f64>,
        confidence_source: Option<&str>,
        model_version_id: Option<&str>,
        cloud_call: bool,
    ) -> AppResult<bool> {
        let raw_nfc = to_nfc(raw_transcript);
        let normalized_nfc = normalized_transcript.map(to_nfc);
        let rows_changed = self.conn.execute(
            "UPDATE speech_segments
             SET raw_transcript        = ?2,
                 normalized_transcript = ?3,
                 confidence            = ?4,
                 confidence_source     = COALESCE(?5, 'unknown'),
                 model_version_id      = COALESCE(?6, 'unknown@pre-registry'),
                 cloud_call            = ?7,
                 updated_at            = datetime('now')
             WHERE id = ?1
               AND verified = 0
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            params![
                segment_id,
                raw_nfc,
                normalized_nfc,
                confidence,
                confidence_source,
                model_version_id,
                cloud_call as i32,
            ],
        )?;
        self.track_write()?;
        Ok(rows_changed > 0)
    }

    /// Fold ONE segment's LOOP-0 shadow evidence into the durable archive BEFORE it is deleted, so the
    /// C5 over-trigger gate isn't survivor-biased by the owner's normal cleanup (review a bad clip, then
    /// delete it — exactly the rows most likely to be over-triggers). Uses the same correlation as
    /// `intelligence_report`. Must run while the segment + its shadow rows still exist (before DELETE).
    fn archive_loop0_evidence_for(&self, id: &str) -> AppResult<()> {
        // Per-SEGMENT semantics (true-10 audit 2026-07-09): a segment re-processed N times holds N
        // shadow rows, but the C5 gate reasons about distinct events — one clip, one human decision,
        // at most one over-trigger. Fold MAX(memory_fired) per segment (this fn archives exactly one
        // segment), matching intelligence_report's DISTINCT-segment live counts.
        self.conn.execute(
            "UPDATE loop0_evidence_archive SET
                 total_observations = total_observations
                     + COALESCE((SELECT COUNT(DISTINCT segment_id) FROM loop0_shadow_log WHERE segment_id = ?1), 0),
                 would_fire = would_fire
                     + COALESCE((SELECT MAX(memory_fired) FROM loop0_shadow_log WHERE segment_id = ?1), 0),
                 fired_human_accepted = fired_human_accepted + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('accept','human_accept') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0),
                 fired_human_edited = fired_human_edited + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('edit','human_edit') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0),
                 fired_human_rejected = fired_human_rejected + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('reject','human_reject') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0)
             WHERE id = 1",
            params![id],
        )?;
        Ok(())
    }

    /// v34 twin of [`Self::archive_loop0_evidence_for`], for the C4 auto-accept-precision denominator:
    /// decision_verdicts CASCADE-deletes with its segment, so the owner's normal cleanup (review a bad
    /// clip, then delete it) removed exactly the T0_ACCEPT rows whose humans CONTRADICTED the machine —
    /// the precision gating any autonomy increase could only drift optimistic (true-10 audit
    /// 2026-07-09). Must run while the segment + its verdict row still exist (before DELETE).
    fn archive_c4_evidence_for(&self, id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE c4_evidence_archive SET
                 t0_accepts = t0_accepts + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts WHERE segment_id = ?1 AND auto_accept_verdict = 'T0_ACCEPT'), 0),
                 t1_escalations = t1_escalations + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts WHERE segment_id = ?1 AND auto_accept_verdict = 'T1_ESCALATE'), 0),
                 t0_human_confirmed = t0_human_confirmed + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id
                     WHERE dv.segment_id = ?1 AND dv.auto_accept_verdict = 'T0_ACCEPT'
                       AND s.human_decision IN ('accept','human_accept')), 0),
                 t0_human_contradicted = t0_human_contradicted + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id
                     WHERE dv.segment_id = ?1 AND dv.auto_accept_verdict = 'T0_ACCEPT'
                       AND s.human_decision IN ('edit','human_edit','reject','human_reject')), 0)
             WHERE id = 1",
            params![id],
        )?;
        Ok(())
    }

    pub fn delete_segment(&self, id: &str) -> AppResult<()> {
        self.conn.execute("SAVEPOINT del_seg", [])?;
        let result: AppResult<()> = (|| {
            self.archive_loop0_evidence_for(id)?;
            self.archive_c4_evidence_for(id)?;
            self.conn.execute("DELETE FROM speech_segments WHERE id = ?1", params![id])?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("del_seg")?;
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("del_seg");
                Err(e)
            }
        }
    }

    pub fn delete_segments_batch(&self, ids: &[String]) -> AppResult<()> {
        self.conn.execute("SAVEPOINT batch_delete", [])?;
        let result: AppResult<()> = (|| {
            // Archive each segment's shadow + C4 evidence FIRST (while its rows still exist), then delete.
            for id in ids {
                self.archive_loop0_evidence_for(id)?;
                self.archive_c4_evidence_for(id)?;
            }
            let mut stmt = self.conn.prepare("DELETE FROM speech_segments WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("batch_delete")?;
                // Keep the FTS5 index clean after bulk deletions.
                if let Err(error) = self.conn.execute("INSERT INTO segments_fts(segments_fts) VALUES('optimize')", []) {
                    tracing::warn!("Failed to optimize segments FTS index after batch delete: {error}");
                }
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("batch_delete");
                Err(e)
            }
        }
    }

    pub fn get_segment_by_id(&self, id: &str) -> AppResult<Option<SpeechSegment>> {
        let query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE id = ?1");
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// Read the complete row and its database-owned review revision in ONE SQLite statement.
    /// Couch Review must never pair a row snapshot from one instant with a revision fetched by a
    /// second statement after a concurrent writer has already changed it.
    pub fn get_segment_by_id_with_revision(&self, id: &str) -> AppResult<Option<(SpeechSegment, i64)>> {
        let query = format!("SELECT {SEGMENT_SELECT_COLUMNS}, review_revision FROM speech_segments WHERE id = ?1");
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some((Self::map_row(row)?, row.get(37)?)))
        } else {
            Ok(None)
        }
    }

    /// Look up a segment by its `audio_path` column using the `idx_segments_audio_path` index.
    /// Used by the media registry to verify playback access without a full table scan.
    /// Returns `Ok(Some(...))` when found, `Ok(None)` when no segment matches the path.
    pub fn get_segment_by_audio_path(&self, audio_path: &str) -> AppResult<Option<SpeechSegment>> {
        let query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE audio_path = ?1 LIMIT 1");
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![audio_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// Segments usable as SPOT CHECKS: a trusted human answer already exists, and the raw ASR draft
    /// DIFFERS from it (Migration v44, docs/REMOTE_REVIEW_PLAN.md §2.1).
    ///
    /// "Trusted" must exclude a PEER'S FRESH GUESS. Without that, every clip a reviewer had just
    /// corrected became an answer key the moment they saved it, and the next reviewer was graded
    /// against it and marked wrong for merely disagreeing. (Found by the soak test: clients reported
    /// 61 successes against 60 clips, the extra one being a just-decided clip re-served as a check.)
    ///
    /// That was first expressed as `is_gold = 1`, which was correct in intent and inert in fact:
    /// **nothing in this application ever sets `is_gold`.** Every write of it lives inside
    /// `#[cfg(test)]`, and the migrations only ever declare it `DEFAULT 0` — the `gold_segments`
    /// table is a different thing entirely (the frozen eval set). So the candidate set was
    /// unconditionally empty in every real installation and the whole spot-check mechanism could
    /// never fire, silently, while the UI simply showed nothing.
    ///
    /// `reviewed_by IS NULL` is the trust signal that is actually populated. It is set
    /// UNCONDITIONALLY by `record_human_decision_by` to the name of whoever authored the row's
    /// current decision, and the desktop path passes `None` while every phone decision passes a
    /// reviewer name — so NULL means "verified here, by the owner, not by a remote reviewer", which
    /// is exactly the distinction the gold flag was reaching for. `is_gold = 1` is still honoured so
    /// that anything which does mark gold in future keeps working.
    ///
    /// The cost is honest and bounded: spot-check volume is capped by how much owner-verified work
    /// exists, so a small library yields few checks. `SpotCheckScore::checks` reports the real number
    /// rather than hiding it, and no conclusion should be drawn from a handful.
    ///
    /// The difference is the whole mechanism. Served with its raw draft, such a clip is a trap that a
    /// reviewer who actually listens will correct and a reviewer who taps "accept" will not — with no
    /// synthetic or planted data anywhere: these are real clips a human already answered.
    ///
    /// Ordered by id so the selection is deterministic; a queue that reshuffled its traps every poll
    /// would grade two reviewers on different material and make the scores incomparable.
    ///
    /// Excludes what THIS reviewer has already been scored on, and that exclusion is what makes the
    /// number mean anything. Without it every batch drew the same first-N-by-id, so a reviewer met the
    /// identical traps over and over: after the first batch they are answering from memory, not from
    /// listening. Worse, `record_spot_check` upserts on (segment_id, reviewer) — so the later,
    /// memorised attempt OVERWRITES the one honest measurement, and the score drifts upward the longer
    /// someone works. It also meant only the first few candidates were ever used no matter how many
    /// existed. Per-reviewer, not global: two reviewers meeting the same clip independently is the
    /// point (it is what `agreement_sample` reads).
    ///
    /// When a reviewer exhausts the pool they simply stop being measured, which is the honest outcome:
    /// `SpotCheckScore::checks` reports how many they actually answered, and re-testing on answers they
    /// already know would be a bigger number meaning less.
    ///
    /// `exclude` is the caller's set of "do not serve this reviewer this clip again" ids — in practice
    /// the clips they SKIPPED (R4.4). The SQL exclusion above cannot cover those: it lists clips with a
    /// row in `spot_checks`, i.e. ones the reviewer was SCORED on, and a skip is the absence of an
    /// answer and writes no score. Without this the one clip somebody said they could not judge was
    /// re-inserted into every subsequent batch forever, and the skip button did nothing for it.
    ///
    /// Filtered here rather than by the caller dropping candidates afterwards, and the difference
    /// matters: this still returns `limit` candidates, just DIFFERENT ones. Skipping a check must cost
    /// you that clip, never your place in the measurement — otherwise the honest exit doubles as a way
    /// to never be tested again.
    pub fn list_spot_check_candidates(
        &self,
        limit: usize,
        reviewer: &str,
        exclude: &std::collections::HashSet<String>,
        allowed_dialects: Option<&[String]>,
    ) -> AppResult<Vec<(SpeechSegment, String)>> {
        // `reviewed_by IS NULL` keeps OWNER-desktop decisions (which pass no annotator name) as
        // keys while excluding a phone peer's fresh correction — grading one reviewer against
        // another's guess would mark them wrong for disagreeing. Machine-text keys are impossible
        // a layer down: `human_verified_text` (gold-provenance law, 2026-08-12) returns None
        // unless a REAL human decision produced the text, so flag-only verifies and reject rows
        // can never become answer keys regardless of what this WHERE admits.
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments
             WHERE verified = 1 AND raw_transcript <> '' AND (is_gold = 1 OR reviewed_by IS NULL)
               AND id NOT IN (SELECT segment_id FROM spot_checks WHERE reviewer = ?1)
             ORDER BY id ASC"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([reviewer], Self::map_row)?;
        let mut out = Vec::new();
        for row in rows {
            // Checked BEFORE the push, not after. Testing it afterwards makes `limit == 0` return ONE
            // candidate — an off-by-one that silently hands a spot check to a caller that asked for
            // none. Found by a fail-before revert that failed to fail.
            if out.len() >= limit {
                break;
            }
            let seg = row?;
            if exclude.contains(&seg.id) {
                continue; // they already declined this one; find them a different key
            }
            let Some(expected) = crate::quality::human_verified_text(&seg) else {
                continue; // a machine verdict is not an answer key
            };
            // The reviewer has to LISTEN to a check — that is the entire point of it — so a key whose
            // audio file has gone is not a key, it is a broken clip in the middle of their batch.
            //
            // MEASURED 2026-08-15: every answer key came from the original corpus, whose audio had been
            // deleted. `pending_segment_ids` had already been taught to skip unplayable WORK, but spot
            // checks are injected here, on a separate path, so they bypassed that filter entirely and
            // `/api/audio` answered 500. With SPOT_CHECK_EVERY = 8 that put a dead clip roughly every
            // eighth item — which is exactly what the reviewers reported: the first few clips play,
            // then one errors.
            if !std::path::Path::new(&seg.audio_path).is_file() {
                continue;
            }
            // Spot checks are injected AFTER the queue's own dialect filter, on this separate path —
            // so without this they were the one way a reviewer could still be handed a dialect they
            // do not speak, and the worst possible one: a check is SCORED, so an honest reviewer
            // fails a test they had no way to pass and reads as a blind-accepter. The queue and the
            // measurement have to agree about who may judge what.
            if !crate::dialect::reviewer_may_judge(allowed_dialects, &seg.audio_path) {
                continue;
            }
            // Only a clip whose raw draft is WRONG can distinguish listening from tapping.
            if learning_text_key(expected) == learning_text_key(&seg.raw_transcript) {
                continue;
            }
            let expected = expected.to_string();
            out.push((seg, expected));
        }
        Ok(out)
    }

    /// Record how a reviewer answered one spot check. Upserts on (segment_id, reviewer) so a network
    /// retry cannot inflate a score with duplicate rows — and so a reviewer is graded on their latest
    /// answer for a clip rather than on whichever attempt happened to arrive first.
    ///
    /// Writes ONLY to `spot_checks`. Grading a reviewer must never be able to alter the corpus it
    /// grades against, so the segment itself is left completely untouched.
    pub fn record_spot_check(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        submitted: &str,
        expected: &str,
    ) -> AppResult<()> {
        let submitted_nfc = to_nfc(submitted.trim());
        let expected_nfc = to_nfc(expected.trim());
        // "Noticed" = they did not simply hand back the draft they were given. A reject counts: judging
        // a clip unusable is a real act of attention, not a blind accept.
        let raw: String = self.conn.query_row(
            "SELECT raw_transcript FROM speech_segments WHERE id = ?1",
            params![segment_id],
            |row| row.get(0),
        )?;
        let noticed = action == "reject" || learning_text_key(&submitted_nfc) != learning_text_key(&raw);
        let cer = crate::wer::compute_cer(&expected_nfc, &submitted_nfc);
        self.conn.execute(
            "INSERT INTO spot_checks
                 (segment_id, reviewer, action, submitted_transcript, expected_transcript, noticed, cer)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(segment_id, reviewer) DO UPDATE SET
                action=excluded.action,
                submitted_transcript=excluded.submitted_transcript,
                expected_transcript=excluded.expected_transcript,
                noticed=excluded.noticed,
                cer=excluded.cer,
                created_at=datetime('now')",
            params![segment_id, reviewer, action, submitted_nfc, expected_nfc, noticed as i32, cer],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Append one row to the audit trail (Migration v45). Never updates, never deletes.
    ///
    /// Best-effort by contract: the caller logs a failure and carries on. Losing an audit row must
    /// never cost a reviewer their decision — the decision is the work, this is the record of it.
    pub fn record_review_event(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        source: &str,
        timestamp_ms: i64,
    ) -> AppResult<()> {
        // v56: the event snapshots the clip's length at the moment of the act, because
        // `reviewed_audio_ms` is the basis the owner PAYS on and must survive the clip later being
        // deleted — the audit trail always did; the pay metric silently lost the row's duration.
        self.conn.execute(
            "INSERT INTO review_events (segment_id, reviewer, action, source, timestamp_ms, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, (SELECT duration_ms FROM speech_segments WHERE id = ?1))",
            params![segment_id, reviewer, action, source, timestamp_ms],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Per-reviewer throughput from the audit trail, busiest first.
    ///
    /// The median is computed **within each reviewer's own stream**, which is the entire reason this
    /// does not reuse `stats.rs::compute_review_timing`: that one orders `decision_log` GLOBALLY, so
    /// with several people reviewing at once it would measure the gap between two DIFFERENT humans'
    /// decisions and report it as one person's pace. Correct for a single reviewer, meaningless for a
    /// team — so the existing metric is left exactly as it is and this one is partitioned by design.
    ///
    /// Gaps longer than [`REVIEW_SESSION_GAP_MS`] are dropped: a reviewer who closes the page and
    /// returns tomorrow did not spend fourteen hours on one clip.
    ///
    /// Counts DECISIONS only. `review_events` is a general audit trail and now also carries `skip` —
    /// a reviewer saying "I cannot judge this one", which writes nothing to the corpus. Counting those
    /// would credit somebody for work they explicitly did not do and inflate the one number that says
    /// how fast the corpus is really being reviewed. A WHITELIST, not `action <> 'skip'`: the next
    /// non-decision event added to this trail must not silently re-open the hole.
    /// Total AUDIO this reviewer has completed, in ms — what the phone shows them as progress, and
    /// the basis the owner pays on (reviewers are paid per hour of audio reviewed, not per hour at
    /// the desk).
    ///
    /// DISTINCT segment_id, so a network retry or a re-decision of the same clip cannot inflate it —
    /// the same reason `ReviewerThroughput::clips` counts distinct. Scoped to ONE reviewer rather
    /// than reusing `reviewer_throughput`, which walks every event of every reviewer: this runs on
    /// each queue fetch from a phone, so it stays a single aggregate.
    pub fn reviewed_audio_ms(&self, reviewer: &str) -> AppResult<i64> {
        // LEFT JOIN + the event's own duration snapshot (v56): an INNER JOIN silently shrank this
        // total whenever the owner deleted a reviewed clip — real paid work vanishing from the one
        // number reviewers are paid on. The event's snapshot wins; the live row backfills events
        // that predate v56; an event whose clip is gone AND predates the snapshot stays 0 rather
        // than invented.
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(d), 0)
               FROM (SELECT MAX(COALESCE(e.duration_ms, s.duration_ms, 0)) AS d
                       FROM review_events e
                       LEFT JOIN speech_segments s ON s.id = e.segment_id
                      WHERE e.reviewer = ?1 AND e.action IN ('accept', 'edit', 'reject')
                      GROUP BY e.segment_id)",
            params![reviewer],
            |row| row.get(0),
        )?)
    }

    pub fn reviewer_throughput(&self) -> AppResult<Vec<ReviewerThroughput>> {
        let mut stmt = self.conn.prepare(
            "SELECT reviewer, segment_id, timestamp_ms FROM review_events
             WHERE action IN ('accept', 'edit', 'reject')
             ORDER BY reviewer ASC, timestamp_ms ASC",
        )?;
        let rows =
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?;

        let mut by_reviewer: std::collections::BTreeMap<String, (std::collections::BTreeSet<String>, Vec<i64>)> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (reviewer, segment, ts) = row?;
            let entry = by_reviewer.entry(reviewer).or_default();
            entry.0.insert(segment);
            entry.1.push(ts);
        }

        let mut out: Vec<ReviewerThroughput> = by_reviewer
            .into_iter()
            .map(|(reviewer, (segments, stamps))| {
                let mut deltas: Vec<i64> =
                    stamps.windows(2).map(|w| w[1] - w[0]).filter(|&d| d > 0 && d <= REVIEW_SESSION_GAP_MS).collect();
                deltas.sort_unstable();
                let median_seconds = if deltas.is_empty() {
                    None
                } else {
                    let mid = deltas.len() / 2;
                    let ms = if deltas.len() % 2 == 1 {
                        deltas[mid] as f64
                    } else {
                        (deltas[mid - 1] + deltas[mid]) as f64 / 2.0
                    };
                    Some(ms / 1000.0)
                };
                ReviewerThroughput { reviewer, clips: segments.len(), median_seconds, samples: deltas.len() }
            })
            .collect();
        out.sort_by(|a, b| b.clips.cmp(&a.clips).then_with(|| a.reviewer.cmp(&b.reviewer)));
        Ok(out)
    }

    /// Build the two-rater agreement sample from clips more than one reviewer has answered.
    ///
    /// INTER-ANNOTATOR AGREEMENT NEEDS DOUBLE-ASSIGNMENT, AND SPOT CHECKS ALREADY PROVIDE IT. Leasing
    /// exists to stop two reviewers colliding on the same pending clip — but spot checks are
    /// deliberately NOT leased, because measuring two people independently is the point. So the
    /// overlap an agreement study requires is already there as a side effect, and `spot_checks` is
    /// already one row per (clip, reviewer): a per-decision table in all but name.
    ///
    /// The labels compared are the ACTIONS (accept / edit / reject) — the categorical judgement kappa
    /// is defined over. Comparing free transcripts instead would measure typing, not agreement.
    ///
    /// Returns `None` when no clip has yet been answered by two different people; a kappa computed
    /// from nothing would be a number with no evidence under it.
    pub fn agreement_sample(&self) -> AppResult<Option<AgreementExport>> {
        let mut stmt =
            self.conn.prepare("SELECT segment_id, reviewer, action FROM spot_checks ORDER BY segment_id ASC")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?;
        // segment -> (reviewer -> action), BTreeMap so the emitted TSV is byte-identical run to run.
        let mut by_segment: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> =
            std::collections::BTreeMap::new();
        let mut reviewers: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for row in rows {
            let (segment, reviewer, action) = row?;
            reviewers.insert(reviewer.clone());
            by_segment.entry(segment).or_default().insert(reviewer, action);
        }

        // The pair sharing the most clips. Ties break on the (sorted) names so the choice is
        // deterministic — a report that silently picked a different pair on each run would make two
        // kappa numbers incomparable for no visible reason.
        let names: Vec<&String> = reviewers.iter().collect();
        let mut best: Option<(usize, &String, &String)> = None;
        for (ai, a) in names.iter().enumerate() {
            for b in names.iter().skip(ai + 1) {
                let shared =
                    by_segment.values().filter(|m| m.contains_key(a.as_str()) && m.contains_key(b.as_str())).count();
                // Written out rather than via `is_none_or`, which is stable only since Rust 1.82 while
                // this crate's MSRV is 1.81 (clippy::incompatible_msrv catches it).
                let better = match best {
                    None => true,
                    Some((most, _, _)) => shared > most,
                };
                if shared > 0 && better {
                    best = Some((shared, a, b));
                }
            }
        }
        let Some((items, a, b)) = best else {
            return Ok(None);
        };

        let mut tsv = format!("{a}\t{b}\n");
        for actions in by_segment.values() {
            if let (Some(la), Some(lb)) = (actions.get(a.as_str()), actions.get(b.as_str())) {
                tsv.push_str(&format!("{la}\t{lb}\n"));
            }
        }
        let other_reviewers: Vec<String> =
            reviewers.iter().filter(|r| *r != a && *r != b).map(|r| r.to_string()).collect();
        Ok(Some(AgreementExport {
            rater_a: a.to_string(),
            rater_b: b.to_string(),
            items,
            tsv,
            path: String::new(), // filled in by the command that writes it
            other_reviewers,
        }))
    }

    /// Per-reviewer spot-check scores, worst `noticed` rate first — the order that puts a reviewer who
    /// may not be listening at the top of the list rather than buried under the diligent ones.
    pub fn spot_check_report(&self) -> AppResult<Vec<SpotCheckScore>> {
        let mut stmt = self.conn.prepare(
            "SELECT reviewer, COUNT(*), SUM(noticed), AVG(cer)
             FROM spot_checks GROUP BY reviewer ORDER BY (CAST(SUM(noticed) AS REAL) / COUNT(*)) ASC, reviewer ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let checks: i64 = row.get(1)?;
            let noticed: i64 = row.get(2)?;
            Ok(SpotCheckScore {
                reviewer: row.get(0)?,
                checks: checks as usize,
                noticed: noticed as usize,
                mean_cer: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn get_segments(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        let mut query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments");
        if let Some(v) = verified {
            query.push_str(&format!(" WHERE verified = {}", if v { 1 } else { 0 }));
        }
        // `, id ASC` is a deterministic tiebreaker: created_at has 1s resolution, so a chunked file's
        // batch-inserted segments tie, and without a unique secondary key SQLite's tie order is
        // undefined — making JSON/JSONL/CSV/Parquet exports non-byte-reproducible across plan/VACUUM.
        query.push_str(" ORDER BY created_at DESC, id ASC");

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    /// Stream every segment through a callback, one row at a time, without materialising the corpus.
    ///
    /// External review 2026-08-06 P1.3, for the corpus-wide STATISTICS in `commands/dataset_analytics.rs`.
    /// Those are whole-corpus by definition — a training-grade breakdown that skipped rows would be
    /// wrong, not slow — so the fix is not a WHERE clause and it is emphatically NOT reimplementing the
    /// grading rule in SQL: `training_grade_for_segment` is the same function the EXPORT gates on, and a
    /// second SQL copy of it would let the dashboard and the export disagree about what is training-ready.
    /// That is the P1.2 drift bug wearing a different hat.
    ///
    /// So the row is what gets bounded, not the rule. Each caller folds into a small accumulator
    /// (counters, or a `(f64, f64)` per eligible clip) while the full record — transcript, alignment
    /// JSON, evidence JSON — lives only for the duration of one callback.
    ///
    /// Same ORDER BY as `get_segments`, so a fold that is order-sensitive sees exactly what the
    /// collect-then-iterate version saw.
    /// `verified` filters exactly like [`get_segments`], so a streaming caller sees the same rows in the
    /// same order as the collecting one it replaced.
    pub fn for_each_segment(&self, verified: Option<bool>, mut f: impl FnMut(SpeechSegment)) -> AppResult<()> {
        let where_sql = match verified {
            Some(true) => " WHERE verified = 1",
            Some(false) => " WHERE verified = 0",
            None => "",
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments{where_sql} ORDER BY created_at DESC, id ASC"
        ))?;
        let rows = stmt.query_map([], Self::map_row)?;
        for row in rows {
            f(row?);
        }
        Ok(())
    }

    /// The IDs of every pending (unverified) clip, in the same order `get_segments(Some(false))` returns.
    ///
    /// External review 2026-08-06 P1.3, for the couch-review queue. That queue must walk EVERY pending
    /// row — its `heldByOthers` / `skippedByYou` / `pendingTotal` counts depend on in-memory lease state,
    /// so no SQL aggregate can produce them — but it only needs the ID of a row it is merely COUNTING.
    /// The full record (transcript, alignment JSON, evidence JSON) is needed for the <= QUEUE_BATCH
    /// clips it actually hands out, and those are hydrated with `get_segments_by_ids`.
    ///
    /// Ordering is byte-identical to `get_segments` on purpose: this replaced a whole-row read, and a
    /// different ORDER BY would silently change WHICH clips a reviewer is handed.
    /// Ids the review queue may hand out.
    ///
    /// A clip whose draft is still a PLACEHOLDER is excluded. `api_decision` already refuses to
    /// verify `[...]` text, so serving one could only ever waste the reviewer's time — but the real
    /// cost is worse than that: if they type the transcript themselves instead, the clip is finished
    /// without the champion ever drafting it, so it permanently has no baseline and no CER can be
    /// measured against it. What the queue SERVES now agrees with what the decide path ACCEPTS.
    ///
    /// This matters because placeholders exist for a whole import: segments are created carrying
    /// `[Pending WSL 7B ASR]` and filled in afterwards at ~8.5 clips/minute. Normally the app is
    /// closed for imports and nobody can reach them; an INTERRUPTED import leaves them behind, which
    /// is exactly what happened on 2026-08-14 (36 orphaned placeholder rows).
    ///
    /// Matched in SQL as `[...]` — the same shape the decide guard rejects — so the two cannot
    /// disagree. `quality::is_placeholder_transcript` remains the authority on what a placeholder
    /// IS; this is the cheap narrowing that keeps the id-only query fast (P1.3).
    ///
    /// OLDEST FIRST, deliberately (2026-08-14). This was newest-first, which quietly buries the work
    /// the owner actually wants finished: after importing 27 hours of new podcast audio the queue
    /// would have served the NEW material first and left the 537 clips of the original corpus behind
    /// 6,823 newer ones. A review queue is FIFO — an import must go to the BACK of the line, so
    /// adding more audio can never delay finishing what is already in progress. (The desktop library
    /// view keeps newest-first; that is a browsing order, not a work order.)
    /// A clip whose AUDIO FILE IS GONE is never served either (2026-08-15). The reviewer cannot listen,
    /// so the only verdicts they can give are guesses about text they never heard — and this is a
    /// VERBATIM corpus, where an unheard "looks good" is worse than no decision at all.
    ///
    /// MEASURED that day: three staging folders under `SoraniVoice_PC_` had ceased to exist, taking the
    /// audio for 1,031 clips (7% of the library) with them. 536 of those were still pending, and because
    /// this queue is oldest-first they sat at the very HEAD of it — so every reviewer who opened a link
    /// was handed unplayable clips first and reported "the audio does not play". Nothing detected it:
    /// the rows are perfectly well-formed, and every gate in the sweep reads the database, not the disk.
    ///
    /// Existence is memoised PER DISTINCT PATH, not per row: segments are chunks of a recording, so a
    /// few hundred files back the whole library and the check costs a few hundred `stat` calls rather
    /// than one per pending row.
    pub fn pending_segment_ids(&self) -> AppResult<Vec<String>> {
        self.pending_segment_ids_for(None)
    }

    /// As above, but only clips in dialects this reviewer can actually judge. `None` = unrestricted.
    ///
    /// Owner instruction 2026-08-16: a reviewer who does not speak Hawleri must not be handed Hawleri.
    /// Judging a dialect you do not speak produces confident WRONG verdicts, and downstream those are
    /// indistinguishable from good ones — so this is a corpus-integrity filter, not a convenience.
    pub fn pending_segment_ids_for(&self, allowed_dialects: Option<&[String]>) -> AppResult<Vec<String>> {
        self.pending_segment_ids_focused(allowed_dialects, None)
    }

    /// `pending_segment_ids_for`, additionally narrowed to a voice-focus set when one is active.
    ///
    /// The focus is an ALLOW-LIST of segment ids (see `voice_focus.rs`): `None` is the full queue,
    /// `Some(set)` serves only clips in it. Applied after the dialect fence, never instead of it — a
    /// reviewer restricted to Sorani stays restricted even when the focus set spans both dialects.
    pub fn pending_segment_ids_focused(
        &self,
        allowed_dialects: Option<&[String]>,
        focus: Option<&std::collections::HashSet<String>>,
    ) -> AppResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, audio_path FROM speech_segments
             WHERE verified = 0
               AND TRIM(COALESCE(raw_transcript, '')) <> ''
               AND NOT (TRIM(raw_transcript) LIKE '[%]')
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut ids = Vec::new();
        // Both checks are memoised per DISTINCT PATH: 13,797 clips come from 32 recordings, so this
        // costs a few dozen stat calls and dialect lookups rather than one per pending row.
        let mut servable: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for row in rows {
            let (id, audio_path) = row?;
            let ok = *servable.entry(audio_path.clone()).or_insert_with(|| {
                std::path::Path::new(&audio_path).is_file()
                    && crate::dialect::reviewer_may_judge(allowed_dialects, &audio_path)
            });
            if ok && focus.map_or(true, |set| set.contains(&id)) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// The backlog one background pass still has to process.
    ///
    /// External review 2026-08-06 P1.3. Each of these three passes used to call `get_segments(None)` —
    /// materialising the WHOLE library — and then `continue` past every row that was already done. The
    /// work list is a WHERE clause; reading the corpus to find it is the O(corpus) read the policy gate
    /// exists to retire.
    ///
    /// What bounds this is the BACKLOG, not a page size: these passes must process every unfinished row,
    /// so a `LIMIT` here would silently drop work rather than bound it. The bound is that a finished row
    /// is never read at all — after the first run the result is empty, where the old code still paid for
    /// the whole library every time.
    ///
    /// Ordering matches `get_segments` (created_at, then a unique tiebreak) so a capped or interrupted
    /// run resumes deterministically instead of picking an arbitrary SQLite row order.
    pub fn get_pending_segments(&self, work: PendingWork) -> AppResult<Vec<SpeechSegment>> {
        let where_sql = match work {
            // A SUPERSET of what `quality::is_placeholder_transcript` accepts, deliberately: SQL narrows
            // cheaply and Rust stays the single authority on what a placeholder IS. Widening in the
            // permissive direction can only cost an extra row that Rust then rejects; it can never drop a
            // real target. `db_tests::sql_placeholder_prefilter_is_a_superset_of_the_rust_predicate`
            // pins that relationship so the two cannot drift apart.
            PendingWork::Transcript => {
                "TRIM(COALESCE(raw_transcript, '')) = ''
                 OR TRIM(raw_transcript) LIKE '[%'
                 OR LENGTH(TRIM(raw_transcript)) <= 4"
            }
            PendingWork::CtcScore => "ctc_score IS NULL",
            PendingWork::SignalAnomaly => "signal_anomaly_score IS NULL",
        };
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments
              WHERE {where_sql}
              ORDER BY created_at ASC, audio_path ASC, id ASC"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    pub fn get_segments_page(
        &self,
        verified: Option<bool>,
        text_query: Option<&str>,
        sort: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> AppResult<SegmentsPage> {
        let limit = limit.clamp(1, 500);
        let sort = canonical_segment_sort(sort);
        let scope = segment_page_scope(verified, text_query);
        let decoded_cursor = cursor.map(decode_segment_cursor).transpose()?;
        if let Some(ref cursor) = decoded_cursor {
            if cursor.sort != sort || cursor.scope != scope {
                return Err(AppError::Validation(
                    "Segment page cursor does not match the active filter or sort".into(),
                ));
            }
        }
        let anchor_rowid = if let Some(ref cursor) = decoded_cursor {
            cursor.anchor_rowid
        } else {
            self.conn.query_row("SELECT COALESCE(MAX(rowid), 0) FROM speech_segments", [], |row| row.get(0))?
        };

        let mut where_parts: Vec<String> = vec!["rowid <= ?1".to_string()];
        let mut bind_values: Vec<Value> = vec![Value::Integer(anchor_rowid)];
        if let Some(v) = verified {
            bind_values.push(Value::Integer(if v { 1 } else { 0 }));
            where_parts.push(format!("verified = ?{}", bind_values.len()));
        }
        if let Some(raw_query) = text_query.map(str::trim).filter(|value| !value.is_empty()) {
            let match_query = to_fts5_match(&normalize_search_query(raw_query));
            if match_query.is_empty() {
                return Ok(SegmentsPage { items: Vec::new(), total: 0, next_cursor: None });
            }
            let scoped_query =
                format!("{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})");
            bind_values.push(Value::Text(scoped_query));
            where_parts
                .push(format!("id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?{})", bind_values.len()));
        }
        let total = if let Some(ref cursor) = decoded_cursor {
            cursor.total
        } else {
            let count_where_sql = format!(" WHERE {}", where_parts.join(" AND "));
            let count_sql = format!("SELECT COUNT(*) FROM speech_segments{count_where_sql}");
            let total: i64 =
                self.conn.query_row(&count_sql, rusqlite::params_from_iter(bind_values.iter()), |row| row.get(0))?;
            usize::try_from(total).map_err(|_| AppError::Validation("Segment count is out of range".into()))?
        };

        let created_expr = "COALESCE(datetime(created_at), '')";
        let confidence_expr = "COALESCE(confidence, 1.0)";
        let active_expr = "ABS(((1.0 - COALESCE(confidence, 0.5)) + (0.1 * -COALESCE(ctc_score, -5.0))) - 0.35)";
        let poor_expr = format!(
            "CASE WHEN COALESCE(snr_db, 99.0) < {} OR COALESCE(clipping_ratio, 0.0) > {} THEN 0 ELSE 1 END",
            crate::quality::POOR_AUDIO_SNR_DB,
            crate::quality::POOR_AUDIO_CLIPPING_RATIO,
        );
        let order_sql = match sort {
            "oldest" => format!("{created_expr} ASC, id ASC"),
            "duration" => "duration_ms DESC, id ASC".to_string(),
            "verified" => format!("verified DESC, {created_expr} DESC, id ASC"),
            "confidence" => format!("{confidence_expr} ASC, id ASC"),
            "activeLearning" => format!("{active_expr} ASC, id ASC"),
            "suspectFirst" => format!(
                "escalated DESC, {poor_expr} ASC, COALESCE(agreement_score, 0.5) ASC, {created_expr} DESC, id ASC"
            ),
            _ => format!("{created_expr} DESC, id ASC"),
        };

        if let Some(cursor) = decoded_cursor.as_ref() {
            let key = &cursor.last;
            let mut bind = |value: Value| {
                bind_values.push(value);
                bind_values.len()
            };
            let keyset = match sort {
                "oldest" => {
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("({created_expr} > COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))")
                }
                "duration" => {
                    let d1 = bind(Value::Integer(key.duration_ms));
                    let d2 = bind(Value::Integer(key.duration_ms));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("(duration_ms < ?{d1} OR (duration_ms = ?{d2} AND id > ?{id}))")
                }
                "verified" => {
                    let v1 = bind(Value::Integer(i64::from(key.verified)));
                    let v2 = bind(Value::Integer(i64::from(key.verified)));
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("(verified < ?{v1} OR (verified = ?{v2} AND ({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))))")
                }
                "confidence" => {
                    let c1 = bind(Value::Real(key.confidence));
                    let c2 = bind(Value::Real(key.confidence));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("({confidence_expr} > ?{c1} OR ({confidence_expr} = ?{c2} AND id > ?{id}))")
                }
                "activeLearning" => {
                    let a1 = bind(Value::Real(key.active_learning));
                    let a2 = bind(Value::Real(key.active_learning));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("({active_expr} > ?{a1} OR ({active_expr} = ?{a2} AND id > ?{id}))")
                }
                "suspectFirst" => {
                    let e1 = bind(Value::Integer(i64::from(key.escalated)));
                    let e2 = bind(Value::Integer(i64::from(key.escalated)));
                    let p1 = bind(Value::Integer(i64::from(!key.poor_audio)));
                    let p2 = bind(Value::Integer(i64::from(!key.poor_audio)));
                    let a1 = bind(Value::Real(key.agreement));
                    let a2 = bind(Value::Real(key.agreement));
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("(escalated < ?{e1} OR (escalated = ?{e2} AND ({poor_expr} > ?{p1} OR ({poor_expr} = ?{p2} AND (COALESCE(agreement_score, 0.5) > ?{a1} OR (COALESCE(agreement_score, 0.5) = ?{a2} AND ({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))))))))")
                }
                _ => {
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))")
                }
            };
            where_parts.push(keyset);
        }

        let where_sql = format!(" WHERE {}", where_parts.join(" AND "));
        bind_values.push(Value::Integer(limit as i64));
        let limit_idx = bind_values.len();
        let page_sql = format!(
            "SELECT {SEGMENT_LIST_SELECT_COLUMNS} FROM speech_segments{where_sql} ORDER BY {order_sql} LIMIT ?{limit_idx}"
        );
        let mut stmt = self.conn.prepare(&page_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind_values.iter()), Self::map_row)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        let emitted = decoded_cursor.as_ref().map_or(0, |cursor| cursor.emitted) + items.len();
        let next_cursor = if emitted < total && !items.is_empty() {
            let Some(last) = items.last() else { unreachable!("positive limit with full page") };
            let active_learning =
                (((1.0 - last.confidence.unwrap_or(0.5)) + (0.1 * -last.ctc_score.unwrap_or(-5.0))) - 0.35).abs();
            let poor_audio = last.snr_db.is_some_and(|v| v < crate::quality::POOR_AUDIO_SNR_DB)
                || last.clipping_ratio.is_some_and(|v| v > crate::quality::POOR_AUDIO_CLIPPING_RATIO);
            Some(encode_segment_cursor(&SegmentPageCursor {
                version: 1,
                sort: sort.to_string(),
                scope,
                anchor_rowid,
                total,
                emitted,
                last: SegmentPageKey {
                    id: last.id.clone(),
                    created_at: last.created_at.clone().unwrap_or_default(),
                    duration_ms: last.duration_ms,
                    verified: last.verified,
                    confidence: last.confidence.unwrap_or(1.0),
                    active_learning,
                    escalated: last.escalated,
                    poor_audio,
                    agreement: last.agreement_score.unwrap_or(0.5),
                },
            })?)
        } else {
            None
        };
        Ok(SegmentsPage { items, total, next_cursor })
    }

    /// Lightweight batch scope: return only ids plus the transcript needed for the optional content
    /// gate. This lets whole-filter actions remain whole-filter actions without hydrating every row.
    pub fn get_segment_ids_for_view(
        &self,
        verified: Option<bool>,
        text_query: Option<&str>,
        transcript_state: &str,
    ) -> AppResult<Vec<String>> {
        let mut where_parts: Vec<String> = Vec::new();
        let mut bind_values: Vec<Value> = Vec::new();
        if let Some(v) = verified {
            bind_values.push(Value::Integer(i64::from(v)));
            where_parts.push(format!("verified = ?{}", bind_values.len()));
        }
        if let Some(raw_query) = text_query.map(str::trim).filter(|value| !value.is_empty()) {
            let match_query = to_fts5_match(&normalize_search_query(raw_query));
            if match_query.is_empty() {
                return Ok(Vec::new());
            }
            bind_values.push(Value::Text(format!(
                "{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})"
            )));
            where_parts
                .push(format!("id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?{})", bind_values.len()));
        }
        let where_sql =
            if where_parts.is_empty() { String::new() } else { format!(" WHERE {}", where_parts.join(" AND ")) };
        let sql = format!("SELECT id, raw_transcript FROM speech_segments{where_sql} ORDER BY id ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind_values.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut ids = Vec::new();
        for row in rows {
            let (id, transcript) = row?;
            let placeholder = crate::quality::is_placeholder_transcript(&transcript);
            if transcript_state == "any"
                || (transcript_state == "real" && !placeholder)
                || (transcript_state == "missing" && placeholder)
            {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    pub fn get_signal_anomaly_segments(&self, limit: usize) -> AppResult<Vec<SpeechSegment>> {
        let sql = format!(
            "SELECT {SEGMENT_LIST_SELECT_COLUMNS} FROM speech_segments
             WHERE signal_anomaly_score IS NOT NULL
             ORDER BY signal_anomaly_score DESC, id ASC LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([limit.clamp(1, 500) as i64], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    /// M2.5: Return segments ordered by suspect-first priority for ReviewInbox.
    /// Jury escalated segments first, then low-confidence (suspicious) segments, then chronological.
    pub fn get_segments_suspect_first(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        let mut query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments");
        if let Some(v) = verified {
            query.push_str(&format!(" WHERE verified = {}", if v { 1 } else { 0 }));
        }
        // Priority: escalated (jury doubts) first, then low agent confidence (suspicious), then chronological.
        query.push_str(&format!(" ORDER BY {}", *SUSPECT_FIRST_ORDER));

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    pub fn search_segments(&self, text: &str) -> AppResult<Vec<SpeechSegment>> {
        let match_query = to_fts5_match(&normalize_search_query(text));
        // Whitespace-only / empty input is an empty result, not an FTS5 `MATCH ""` error.
        if match_query.is_empty() {
            return Ok(Vec::new());
        }
        // Round-23 #7: the segments_fts table also indexes `audio_path`, so a bare `MATCH ?` matches the
        // query against the FILE PATH too — a token that appears only in a folder/file name returned
        // false-positive segments whose transcript did not contain it. Restrict the match to the
        // transcript columns with an FTS5 column filter so only transcript content is searched.
        let scoped_query = format!("{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})");
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS}
             FROM speech_segments
             WHERE id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?1)
             ORDER BY created_at DESC, id ASC"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![scoped_query], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    /// Batch-fetch segments by a list of IDs using a single `WHERE id IN (...)` query.
    /// Dramatically faster than N individual `get_segment_by_id` calls for delete/undo
    /// history snapshots on large selections.
    pub fn get_segments_by_ids(&self, ids: &[String]) -> AppResult<Vec<SpeechSegment>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQLite caps bound parameters per statement (SQLITE_MAX_VARIABLE_NUMBER — only 999 on older
        // builds). A large selection (delete/undo of thousands of segments) would overflow a single
        // IN(?,?,…) and fail with "too many SQL variables", so fetch in bounded chunks and re-impose
        // the global ordering afterwards (per-chunk ORDER BY doesn't compose across chunks).
        const CHUNK: usize = 500;
        let mut segments: Vec<SpeechSegment> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            // Build a parameterised placeholder list: (?1,?2,...?N)
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let query = format!(
                "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&query)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), Self::map_row)?;
            for row in rows {
                segments.push(row?);
            }
        }
        // Match the single-query contract: created_at DESC (newest first), then id ASC. None sorts
        // last under DESC, mirroring SQLite ordering NULLs after non-NULLs in a descending sort.
        segments.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(segments)
    }

    /// Couch Review variant of [`Self::get_segments_by_ids`]: each row and its revision come from the
    /// same SQLite result row. Fetching the revision afterwards could stamp an older transcript with a
    /// newer revision and let a decision against text the reviewer never saw pass its CAS fence.
    pub fn get_segments_by_ids_with_revisions(&self, ids: &[String]) -> AppResult<Vec<(SpeechSegment, i64)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        const CHUNK: usize = 500;
        let mut segments: Vec<(SpeechSegment, i64)> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let query = format!(
                "SELECT {SEGMENT_SELECT_COLUMNS}, review_revision FROM speech_segments WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&query)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((Self::map_row(row)?, row.get(37)?)))?;
            for row in rows {
                segments.push(row?);
            }
        }
        segments.sort_by(|a, b| b.0.created_at.cmp(&a.0.created_at).then_with(|| a.0.id.cmp(&b.0.id)));
        Ok(segments)
    }

    pub fn rename_speaker(&self, old_id: &str, new_id: &str) -> AppResult<usize> {
        let count = self.conn.execute(
            "UPDATE speech_segments SET speaker_id = ?2, updated_at = datetime('now') WHERE speaker_id = ?1",
            params![old_id, new_id],
        )?;
        self.track_write()?;
        Ok(count)
    }

    pub fn integrity_check(&self) -> AppResult<String> {
        let result: String = self.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(result)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// The on-disk path this connection was opened from (or `":memory:"`). Used by commands that need
    /// to open a SECOND, dedicated connection so they can release the global AppState db Mutex before a
    /// long network call (e.g. cloud jury T2) — holding it across the round-trip would freeze every
    /// other DB-touching command app-wide.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn info(&self) -> AppResult<serde_json::Value> {
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let journal_mode: String = self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let segment_count: i64 = self.conn.query_row("SELECT count(*) FROM speech_segments", [], |r| r.get(0))?;
        Ok(serde_json::json!({
            "path": self.path,
            "sizeBytes": size,
            "journalMode": journal_mode,
            "segmentCount": segment_count,
        }))
    }

    pub fn segment_count(&self) -> AppResult<i64> {
        let count: i64 = self.conn.query_row("SELECT count(*) FROM speech_segments", [], |r| r.get(0))?;
        Ok(count)
    }

    /// Pages copied per backup step, and the pause between steps. See [`Self::backup`].
    const BACKUP_PAGES_PER_STEP: std::os::raw::c_int = 4096;
    const BACKUP_STEP_PAUSE: std::time::Duration = std::time::Duration::from_millis(1);

    /// SQLite online backup — safe against a live database, unlike a file copy.
    ///
    /// The pacing is load-bearing and was pathological. It used to be `(5, 250ms)` — the literal
    /// rusqlite doc example — which copies 5 pages, sleeps a quarter second, and repeats. At 4 KB
    /// pages that is 80 KB/s, so the 84 MB library took ~21,600 pages / 5 × 250 ms = **18 minutes**.
    ///
    /// MEASURED 2026-08-17. Three consequences, none of them obvious from this function:
    ///   * `take_snapshot` runs SYNCHRONOUSLY on the startup path, so every launch held the review
    ///     port shut for ~16 minutes. That is the entire cold start — the watchdog's startup grace
    ///     was raised to 45 minutes twice to accommodate it, treating the symptom both times.
    ///   * The periodic snapshot timer is 10 minutes, i.e. SHORTER than one snapshot took, so a copy
    ///     was essentially always in flight against the database reviewers were writing to.
    ///   * A backup restarts from scratch when the source is written mid-copy, so the slower it is,
    ///     the likelier it is to never finish on a busy library.
    ///
    /// 4096 pages (~16 MB) per step holds the source lock for a few milliseconds at a time, which is
    /// what stepping is actually for, and finishes an 84 MB database in well under a second.
    pub fn backup<P: AsRef<Path>>(&self, dest: P) -> AppResult<()> {
        let mut dest_conn = Connection::open(dest.as_ref())?;
        let backup = backup::Backup::new(&self.conn, &mut dest_conn)?;
        backup.run_to_completion(Self::BACKUP_PAGES_PER_STEP, Self::BACKUP_STEP_PAUSE, None)?;
        Ok(())
    }

    pub fn restore<P: AsRef<Path>>(&mut self, src: P) -> AppResult<()> {
        let src_conn = Connection::open(src.as_ref())?;
        // Verify the SOURCE snapshot is a healthy database BEFORE overwriting the live one, so a corrupt
        // snapshot fails fast with a clear error instead of part-way through the backup copy.
        let integrity: String = src_conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity.trim() != "ok" {
            return Err(AppError::Other(format!("snapshot database failed its integrity check: {integrity}")));
        }
        // Forward-compatibility fence: refuse a snapshot whose schema is NEWER than this build supports.
        // Restore copies the source's pages directly into the live DB, bypassing run_migrations' startup
        // guard — so without this check, restoring a snapshot made by a newer Cortex Speech would leave
        // this build silently operating a future schema with stale semantics (the same data-integrity
        // hazard run_migrations refuses at open). A missing schema_migrations table reads as version 0
        // (an old/fresh snapshot is safe to restore); a genuine read error propagates.
        let snap_version: i64 =
            match src_conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| r.get(0)) {
                Ok(v) => v,
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg))) if msg.contains("no such table") => 0,
                Err(e) => return Err(e.into()),
            };
        let max_known = crate::migrations::max_supported_version();
        if snap_version > max_known {
            return Err(AppError::Other(format!(
                "this snapshot is at schema v{snap_version}, newer than this build supports (v{max_known}) \
                 — it was created by a newer version of Cortex Speech. Update the app before restoring it. \
                 Refusing so the current library is not overwritten with a database this build cannot safely read."
            )));
        }
        let backup = backup::Backup::new(&src_conn, &mut self.conn)?;
        // Same pacing as `backup`, and for the same reason: a restore is the recovery path, where
        // 18 minutes of apparent hang is when someone concludes it is broken and interrupts it.
        backup.run_to_completion(Self::BACKUP_PAGES_PER_STEP, Self::BACKUP_STEP_PAUSE, None)?;
        drop(backup); // release the &mut self.conn borrow before re-migrating self
                      // Bring the restored DB up to the current schema IN PLACE. The newer-schema case was refused
                      // above, so the source is at an OLDER (or equal) version; without this, a restored old snapshot
                      // would sit at a stale schema — missing columns/tables a later migration added — and the running
                      // app (new code) would hit "no such column" errors until the next startup re-migrated it. Equal
                      // is a no-op; each migration is CREATE/ALTER ... guarded and applied in its own transaction.
        crate::migrations::run_migrations(self)?;
        Ok(())
    }

    pub fn vacuum(&self) -> AppResult<()> {
        // SQLite VACUUM cannot run inside a transaction — it commits any pending work and runs
        // standalone — so the VACUUM and its compensating FTS rebuild below CANNOT be wrapped in one
        // atomic statement. VACUUM renumbers speech_segments' implicit rowids, desyncing the
        // external-content FTS index (search would return unrelated rows). Rebuild it immediately.
        self.conn.execute("VACUUM", [])?;
        // If the rebuild fails the index is left stale, but only until the next launch: initialize()
        // unconditionally rebuilds segments_fts on every startup. Surface that so a rebuild failure is
        // an actionable "restart repairs search", not a cryptic error over a silently-wrong index.
        self.conn.execute("INSERT INTO segments_fts(segments_fts) VALUES('rebuild')", []).map_err(|e| {
            AppError::Other(format!(
                "VACUUM completed but rebuilding the search index failed: {e}. Search may return stale \
                 results until you restart the app, which rebuilds the index automatically."
            ))
        })?;
        Ok(())
    }

    pub fn wal_checkpoint(&self) -> AppResult<()> {
        self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    /// P3.3: audio durability — segments reference their source audio by absolute path in place, so a
    /// file the owner moves/renames over months of use silently breaks playback, re-transcription, and
    /// the jury's source-reference guard. Report which distinct audio files are now missing.
    pub fn audio_health(&self) -> AppResult<AudioHealth> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT audio_path FROM speech_segments")?;
        let paths: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
        let total_files = paths.len();
        let mut missing_paths: Vec<String> = paths.into_iter().filter(|p| !Path::new(p).exists()).collect();
        missing_paths.sort();
        Ok(AudioHealth { total_files, missing_files: missing_paths.len(), missing_paths })
    }

    /// P3.3: relink missing source audio by basename — for each missing `audio_path`, if a file with the
    /// same file name exists under `search_dir`, repoint every segment on that old path to the found one.
    /// Basename match (speech_segments store no content hash to verify against); the owner points at the
    /// folder they moved the audio to. Returns how many distinct paths were relinked + how many remain.
    ///
    /// AMBIGUITY GUARD: if two DISTINCT missing source paths share a basename (e.g. `interview.wav`
    /// imported from two different folders), a single found `interview.wav` cannot be known to be the
    /// right one for both — blindly repointing both would serve the WRONG audio for one recording on
    /// playback/re-transcription. Such colliding paths are left missing (and warned), never guessed.
    pub fn relink_audio(&self, search_dir: &Path) -> AppResult<RelinkResult> {
        let missing = self.audio_health()?.missing_paths;
        // Count distinct missing paths per basename so we can refuse ambiguous relinks.
        let mut basename_counts: std::collections::HashMap<std::ffi::OsString, usize> =
            std::collections::HashMap::new();
        for old in &missing {
            if let Some(name) = Path::new(old).file_name() {
                *basename_counts.entry(name.to_os_string()).or_insert(0) += 1;
            }
        }
        let mut relinked = 0usize;
        for old in &missing {
            let Some(name) = Path::new(old).file_name() else { continue };
            if basename_counts.get(name).copied().unwrap_or(0) > 1 {
                tracing::warn!(
                    "relink: '{}' shares its filename with another missing source — skipped (ambiguous, would risk the wrong audio)",
                    old
                );
                continue;
            }
            let candidate = search_dir.join(name);
            if candidate.is_file() {
                let new_path = candidate.to_string_lossy().to_string();
                // Second ambiguity guard: the collision check above only covers basenames shared among
                // MISSING paths. If the candidate file is already OWNED by another library entry (a
                // still-present segment whose recording happens to share the name), repointing would
                // alias this missing recording onto THAT recording's audio — transcript/audio
                // mispairing, the exact wrong-audio hazard this function refuses to guess about.
                let owned: i64 = self.conn.query_row(
                    "SELECT COUNT(*) FROM speech_segments WHERE audio_path = ?1",
                    params![new_path],
                    |r| r.get(0),
                )?;
                if owned > 0 {
                    tracing::warn!(
                        "relink: '{}' matches '{}', which another library recording already owns — skipped \
                         (ambiguous, would serve the wrong audio)",
                        old,
                        new_path
                    );
                    continue;
                }
                let n = self.conn.execute(
                    "UPDATE speech_segments SET audio_path = ?2, updated_at = datetime('now') WHERE audio_path = ?1",
                    params![old, new_path],
                )?;
                // Carry the SOURCE-KEYED tables with the move. Both are joined to segments by
                // audio_path, so relinking only `speech_segments` orphans them at the old key:
                //
                //  * `source_audio_provenance` (v54) then reports the recording as UNCLAIMED, and a
                //    training pack and dataset card would describe neural-separated, re-concatenated
                //    audio as an original field recording — precisely the lie that table exists to
                //    prevent, reintroduced by a file move. Found by adversarial verification
                //    2026-08-17.
                //  * `source_transcripts` would silently lose its whole-file reference transcripts,
                //    forcing a re-fetch that the cache was built to avoid.
                //
                // Not fatal: a relink whose provenance carry fails must still relink the audio (the
                // clips are otherwise unplayable), so the failure warns rather than aborting.
                for (table, what) in [
                    ("source_audio_provenance", "processing provenance"),
                    ("source_transcripts", "reference transcripts"),
                ] {
                    if let Err(e) = self.conn.execute(
                        &format!("UPDATE OR IGNORE {table} SET audio_path = ?2 WHERE audio_path = ?1"),
                        params![old, new_path],
                    ) {
                        tracing::warn!("relink: {what} for '{old}' could not follow the move to '{new_path}': {e}");
                    }
                }
                if n > 0 {
                    relinked += 1;
                }
            }
        }
        self.track_write()?;
        Ok(RelinkResult { relinked, still_missing: self.audio_health()?.missing_files })
    }

    // ── P3.2: import journal (resume a directory import interrupted by a crash) ──────────────────

    /// Open a new import job (status 'running'). Also prunes old finished jobs so the journal stays
    /// small. Journal writes are best-effort at the call sites — a failure here never fails an import.
    pub fn begin_import_job(&self, dir: &str, total_files: usize) -> AppResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        // SAVEPOINT (write-path audit, Week 2): reap + INSERT + retention are one invariant — a failure
        // after the reap used to leave prior crashes marked 'abandoned' WITHOUT the new running job that
        // justified abandoning them (the resume prompt would then find nothing to offer).
        self.conn.execute("SAVEPOINT import_job_begin", [])?;
        let result: AppResult<()> = (|| {
            // Reap stale crashes first. Imports are single-flight (try_start_import guards the only call
            // site), so when a NEW import begins any lingering 'running' job is a PRIOR crash the user did
            // not resume — the startup resume prompt already had its chance before this new import started.
            // Marking them 'abandoned' keeps exactly one 'running' job (the active one), so:
            //   * find_interrupted_import_job stays unambiguous — no spurious "resume?" for an old crash
            //     after the user already resumed a newer one, and
            //   * 'running' rows can't accumulate unboundedly across repeated crashes (abandoned rows are
            //     status != 'running', so the retention prune below reaps them + CASCADE clears their files).
            self.conn.execute(
                "UPDATE import_jobs SET status = 'abandoned', updated_at = datetime('now') WHERE status = 'running'",
                [],
            )?;
            self.conn.execute(
                "INSERT INTO import_jobs (id, dir, total_files, status) VALUES (?1, ?2, ?3, 'running')",
                params![id, dir, total_files as i64],
            )?;
            // Retention: keep the newest 50 FINISHED jobs (running jobs are always kept — they may be crashes).
            self.conn.execute(
                "DELETE FROM import_jobs WHERE status != 'running' AND id NOT IN (
                     SELECT id FROM import_jobs WHERE status != 'running'
                     ORDER BY datetime(created_at) DESC, id DESC LIMIT 50
                 )",
                [],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("import_job_begin")?;
                Ok(id)
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("import_job_begin");
                Err(e)
            }
        }
    }

    /// Record that `path` finished processing in job `job_id` (idempotent).
    pub fn mark_import_file_done(&self, job_id: &str, path: &str) -> AppResult<()> {
        self.conn
            .execute("INSERT OR IGNORE INTO import_job_files (job_id, path) VALUES (?1, ?2)", params![job_id, path])?;
        self.conn.execute("UPDATE import_jobs SET updated_at = datetime('now') WHERE id = ?1", params![job_id])?;
        Ok(())
    }

    /// Mark a job finished (a clean end): it is no longer an interruption to resume.
    pub fn complete_import_job(&self, job_id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE import_jobs SET status = 'completed', updated_at = datetime('now') WHERE id = ?1",
            params![job_id],
        )?;
        Ok(())
    }

    /// Discard an interrupted job (the user chose not to resume). Deletes both tables explicitly so it
    /// works whether or not the foreign-keys pragma is enabling CASCADE.
    pub fn discard_import_job(&self, job_id: &str) -> AppResult<()> {
        // SAVEPOINT: the two deletes are one invariant (same pattern as begin_import_job). As two
        // auto-commit statements, a failure between them deleted the job's per-file progress journal
        // while leaving the job row alive and 'running' — the startup resume prompt would then offer a
        // job with an EMPTY completed-files list, and resuming would re-import files whose segments
        // already exist, duplicating them.
        self.conn.execute("SAVEPOINT discard_import_job", [])?;
        let result: AppResult<()> = (|| {
            self.conn.execute("DELETE FROM import_job_files WHERE job_id = ?1", params![job_id])?;
            self.conn.execute("DELETE FROM import_jobs WHERE id = ?1", params![job_id])?;
            Ok(())
        })();
        match result {
            Ok(()) => self.release_savepoint("discard_import_job"),
            Err(e) => {
                self.cleanup_savepoint_after_error("discard_import_job");
                Err(e)
            }
        }
    }

    /// The most recent still-'running' job — a crash never calls `complete_import_job`, so it stays
    /// running. Intended to be queried at STARTUP (when no import is active, a running job IS a crash).
    pub fn find_interrupted_import_job(&self) -> AppResult<Option<ImportJob>> {
        let head = self.conn.query_row(
            "SELECT id, dir, total_files, created_at FROM import_jobs
             WHERE status = 'running' ORDER BY datetime(created_at) DESC, id DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)),
        );
        let (id, dir, total_files, created_at) = match head {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut stmt = self.conn.prepare("SELECT path FROM import_job_files WHERE job_id = ?1")?;
        let completed_paths: Vec<String> = stmt.query_map(params![id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(Some(ImportJob { id, dir, total_files: total_files as usize, completed_paths, created_at }))
    }

    // ── Durable jobs (migration v37 + crate::jobs::JobState) — the persistent Job Supervisor. ──

    /// Build a `Job` from a `(id, kind, state_str, progress, completed, total, error_code)` row tuple,
    /// erroring if the persisted state is outside the lifecycle vocabulary (the CHECK constraint makes
    /// that impossible, but a corrupt DB shouldn't silently coerce to a wrong state).
    fn job_from_row(row: JobRow) -> AppResult<crate::jobs::Job> {
        let (id, kind, state_str, progress, completed, total, error_code) = row;
        let state = crate::jobs::JobState::parse(&state_str)
            .ok_or_else(|| AppError::Other(format!("job {id} has an unknown state {state_str:?} in the database")))?;
        Ok(crate::jobs::Job { id, kind, state, progress, completed, total, error_code })
    }

    const JOB_COLS: &str = "id, kind, state, progress, completed, total, error_code";

    fn read_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
    }

    fn read_payload_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::jobs::PayloadJob> {
        let state_token: String = r.get(2)?;
        let id: String = r.get(0)?;
        let state = crate::jobs::JobState::parse(&state_token).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                format!("job {id} has unknown state {state_token:?}").into(),
            )
        })?;
        Ok(crate::jobs::PayloadJob {
            id,
            kind: r.get(1)?,
            state,
            idempotency_key: r.get(3)?,
            error_code: r.get(4)?,
            payload_json: r.get(5)?,
        })
    }

    const PAYLOAD_JOB_COLS: &str = "id, kind, state, idempotency_key, error_code, payload_json";

    /// Fetch a job by id, or `None` if it doesn't exist.
    pub fn get_job(&self, id: &str) -> AppResult<Option<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs WHERE id = ?1", Self::JOB_COLS);
        match self.conn.query_row(&sql, params![id], Self::read_job_row) {
            Ok(row) => Ok(Some(Self::job_from_row(row)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn get_job_by_idempotency_key(&self, key: &str) -> AppResult<Option<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs WHERE idempotency_key = ?1", Self::JOB_COLS);
        match self.conn.query_row(&sql, params![key], Self::read_job_row) {
            Ok(row) => Ok(Some(Self::job_from_row(row)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Create a queued job, or return the existing one when `idempotency_key` is already present — so a
    /// re-issued identical request (retry, double-click) resumes the same job instead of duplicating work.
    pub fn create_or_get_job(
        &self,
        id: &str,
        kind: &str,
        idempotency_key: Option<&str>,
        total: Option<i64>,
    ) -> AppResult<crate::jobs::Job> {
        if let Some(key) = idempotency_key {
            if let Some(existing) = self.get_job_by_idempotency_key(key)? {
                return Ok(existing);
            }
        }
        self.conn.execute(
            "INSERT INTO jobs (id, kind, idempotency_key, total, state) VALUES (?1, ?2, ?3, ?4, 'queued')",
            params![id, kind, idempotency_key, total],
        )?;
        self.get_job(id)?.ok_or_else(|| AppError::Other(format!("job {id} vanished immediately after insert")))
    }

    /// Fetch the durable payload for a payload-owning job kind. A NULL payload is an integrity error:
    /// callers of this API have declared that their recovery contract depends on the journal bytes.
    pub fn get_payload_job(&self, id: &str) -> AppResult<Option<crate::jobs::PayloadJob>> {
        let sql = format!("SELECT {} FROM jobs WHERE id = ?1", Self::PAYLOAD_JOB_COLS);
        match self.conn.query_row(&sql, params![id], Self::read_payload_job_row) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically publish a job as `running` with its recovery payload before any external mutation.
    /// An idempotent retry returns the existing row; the owning coordinator must compare its exact
    /// contract bytes before resuming. No queued-without-payload crash window exists.
    pub fn begin_running_payload_job(
        &self,
        id: &str,
        kind: &str,
        idempotency_key: &str,
        payload_json: &str,
    ) -> AppResult<crate::jobs::BegunPayloadJob> {
        self.conn.execute("SAVEPOINT begin_running_payload_job", [])?;
        let result: AppResult<crate::jobs::BegunPayloadJob> = (|| {
            let by_key_sql = format!("SELECT {} FROM jobs WHERE idempotency_key = ?1", Self::PAYLOAD_JOB_COLS);
            let existing = match self.conn.query_row(&by_key_sql, params![idempotency_key], Self::read_payload_job_row)
            {
                Ok(row) => Some(row),
                Err(rusqlite::Error::QueryReturnedNoRows) => self.get_payload_job(id)?,
                Err(e) => return Err(e.into()),
            };
            if let Some(job) = existing {
                return Ok(crate::jobs::BegunPayloadJob { job, created: false });
            }

            self.conn.execute(
                "INSERT INTO jobs
                    (id, kind, state, idempotency_key, payload_json, started_at)
                 VALUES (?1, ?2, 'running', ?3, ?4, datetime('now'))",
                params![id, kind, idempotency_key, payload_json],
            )?;
            let job = self
                .get_payload_job(id)?
                .ok_or_else(|| AppError::Other(format!("payload job {id} vanished immediately after insert")))?;
            Ok(crate::jobs::BegunPayloadJob { job, created: true })
        })();
        match result {
            Ok(value) => {
                self.release_savepoint("begin_running_payload_job")?;
                Ok(value)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("begin_running_payload_job");
                Err(error)
            }
        }
    }

    /// Compare-and-swap a running job's recovery payload. Re-running an external side effect after a
    /// crash is safe only when phase advancement cannot silently overwrite another recovery worker.
    pub fn compare_and_swap_running_job_payload(
        &self,
        id: &str,
        kind: &str,
        expected_payload_json: &str,
        next_payload_json: &str,
    ) -> AppResult<()> {
        let changed = self.conn.execute(
            "UPDATE jobs SET payload_json = ?4, updated_at = datetime('now')
             WHERE id = ?1 AND kind = ?2 AND state = 'running' AND payload_json = ?3",
            params![id, kind, expected_payload_json, next_payload_json],
        )?;
        if changed != 1 {
            return Err(AppError::Validation(format!(
                "running payload job {id} changed concurrently or is no longer resumable"
            )));
        }
        Ok(())
    }

    /// Atomically persist a final payload and terminal state. The expected payload comparison makes a
    /// stale recovery worker unable to claim success (or failure) over a newer phase.
    pub fn finish_running_payload_job(
        &self,
        id: &str,
        kind: &str,
        expected_payload_json: &str,
        final_payload_json: &str,
        state: crate::jobs::JobState,
        error_code: Option<&str>,
    ) -> AppResult<()> {
        if !matches!(state, crate::jobs::JobState::Succeeded | crate::jobs::JobState::Failed) {
            return Err(AppError::Validation(format!(
                "payload job {id} can only finish succeeded or failed, not {state}"
            )));
        }
        let changed = self.conn.execute(
            "UPDATE jobs SET state = ?5, error_code = ?6, payload_json = ?4,
                 progress = CASE WHEN ?5 = 'succeeded' THEN 1.0 ELSE progress END,
                 finished_at = datetime('now'), updated_at = datetime('now')
             WHERE id = ?1 AND kind = ?2 AND state = 'running' AND payload_json = ?3",
            params![id, kind, expected_payload_json, final_payload_json, state.as_str(), error_code],
        )?;
        if changed != 1 {
            return Err(AppError::Validation(format!(
                "running payload job {id} changed concurrently or is no longer finishable"
            )));
        }
        Ok(())
    }

    /// Startup recovery inventory for one durable kind. Unlike the generic orphan reaper, this
    /// exposes the exact payloads so the owner can complete or roll back each state machine.
    pub fn list_running_payload_jobs(&self, kind: &str) -> AppResult<Vec<crate::jobs::PayloadJob>> {
        let sql = format!(
            "SELECT {} FROM jobs WHERE kind = ?1 AND state = 'running'
             ORDER BY datetime(created_at), id",
            Self::PAYLOAD_JOB_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![kind], Self::read_payload_job_row)?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Move a job to `to`, enforcing the `JobState` lifecycle (an illegal edge — e.g. completing twice,
    /// or resurrecting a cancelled job — is rejected, not silently written). Stamps `started_at` on the
    /// first entry to `running` and `finished_at` on any terminal state.
    pub fn transition_job(&self, id: &str, to: crate::jobs::JobState, error_code: Option<&str>) -> AppResult<()> {
        let current = self.get_job(id)?.ok_or_else(|| AppError::Other(format!("job {id} not found")))?;
        if !current.state.can_transition_to(to) {
            return Err(AppError::Validation(format!(
                "illegal job transition {} -> {} for job {id}",
                current.state, to
            )));
        }
        let finished = to.is_terminal() as i64;
        // Compare-and-swap (write-path audit, Week 2): the lifecycle check above is read-then-write, and
        // a concurrent transition on ANOTHER connection could land between the read and this UPDATE —
        // the old unconditional WHERE would then apply an edge validated against a stale state (e.g.
        // resurrecting a just-cancelled job). Conditioning on the state we validated makes the racing
        // writer's edge a 0-row miss, surfaced as an honest error instead of a silent double-write.
        let affected = self.conn.execute(
            "UPDATE jobs SET
                 state = ?2,
                 error_code = ?3,
                 started_at = CASE WHEN ?2 = 'running' AND started_at IS NULL THEN datetime('now') ELSE started_at END,
                 finished_at = CASE WHEN ?4 = 1 THEN datetime('now') ELSE finished_at END,
                 updated_at = datetime('now')
             WHERE id = ?1 AND state = ?5",
            params![id, to.as_str(), error_code, finished, current.state.as_str()],
        )?;
        if affected == 0 {
            let now_state = self.get_job(id)?.map(|j| j.state.to_string()).unwrap_or_else(|| "<gone>".to_string());
            return Err(AppError::Validation(format!(
                "job {id} was transitioned concurrently ({} -> {now_state}); {} -> {to} rejected",
                current.state, current.state
            )));
        }
        Ok(())
    }

    /// Update a running job's progress. `progress` is clamped to 0.0..=1.0 to respect the CHECK constraint.
    pub fn update_job_progress(&self, id: &str, completed: i64, progress: f64) -> AppResult<()> {
        let progress = progress.clamp(0.0, 1.0);
        self.conn.execute(
            "UPDATE jobs SET completed = ?2, progress = ?3, updated_at = datetime('now') WHERE id = ?1",
            params![id, completed, progress],
        )?;
        Ok(())
    }

    /// At STARTUP, any job still `running` is a crash residue (a clean run always reaches a terminal
    /// state). Mark them failed with a stable `INTERRUPTED` code so the UI can honestly show "interrupted"
    /// instead of a ghost that never finishes. Returns how many were reaped.
    // ponytail: generic recovery = fail+INTERRUPTED; a resumable job kind can re-create from its own
    // durable state on the next run. Add per-kind auto-resume only when a kind actually needs it.
    pub fn mark_orphaned_running_jobs_failed(&self) -> AppResult<usize> {
        let n = self.conn.execute(
            "UPDATE jobs SET state = 'failed', error_code = COALESCE(error_code, 'INTERRUPTED'),
                 finished_at = datetime('now'), updated_at = datetime('now')
             WHERE state = 'running' AND kind <> 'model_promotion'",
            [],
        )?;
        Ok(n)
    }

    /// The most recent jobs (newest first), for a UI activity surface.
    pub fn list_recent_jobs(&self, limit: i64) -> AppResult<Vec<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs ORDER BY datetime(created_at) DESC, id DESC LIMIT ?1", Self::JOB_COLS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<JobRow> = stmt.query_map(params![limit], Self::read_job_row)?.collect::<Result<_, _>>()?;
        rows.into_iter().map(Self::job_from_row).collect()
    }

    /// Bracket `work` as a durable job: record a queued→running lifecycle, run it, then mark
    /// succeeded, or failed with the stable `error_code` on error (the original error still propagates).
    /// A crash mid-`work` leaves a `running` row that `mark_orphaned_running_jobs_failed` reaps at the
    /// next startup — that is the whole point of routing a long op through here. `job_id` is caller-
    /// supplied (a fresh uuid) so the id is known before `work` starts and survives a crash.
    pub fn run_tracked<T>(
        &self,
        job_id: &str,
        kind: &str,
        error_code: &str,
        work: impl FnOnce(&Database) -> AppResult<T>,
    ) -> AppResult<T> {
        self.create_or_get_job(job_id, kind, None, None)?;
        self.transition_job(job_id, crate::jobs::JobState::Running, None)?;
        // Once `work` returns, the op's real outcome is DECIDED (the export file is on disk, or not).
        // The terminal stamp is a best-effort RECORD of that — it must never change what the caller sees.
        // If a stamp write fails, the row lingers `running` and is reaped as INTERRUPTED at the next
        // startup: a cosmetic history wart, never a false result to the user or data loss.
        match work(self) {
            Ok(v) => {
                if let Err(e) = self.transition_job(job_id, crate::jobs::JobState::Succeeded, None) {
                    tracing::warn!("job {job_id} ({kind}) succeeded but recording success failed: {e}");
                }
                Ok(v)
            }
            Err(e) => {
                let _ = self.transition_job(job_id, crate::jobs::JobState::Failed, Some(error_code));
                Err(e)
            }
        }
    }

    pub fn insert_hypothesis(&self, hyp: &SegmentHypothesis) -> AppResult<()> {
        // NFC-canonicalize the vote at this single chokepoint so EVERY engine's hypothesis (local
        // 300M/1B/WSL-7B and cloud Scribe) is stored in the same normalization form. The jury scores
        // agreement by exact surface word-equality (diff/phonetic.rs); without this, two engines that
        // emit the same Sorani text in different forms (NFD vs NFC) would be scored as disagreeing and
        // a real consensus would be spuriously escalated. Matches the NFC enforced on speech_segments.
        let transcript = to_nfc(&hyp.transcript);
        self.conn.execute(
            "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(segment_id, model_id) DO UPDATE SET
                transcript=excluded.transcript,
                confidence=excluded.confidence,
                created_at=datetime('now')",
            params![hyp.segment_id, hyp.model_id, transcript, hyp.confidence],
        )?;
        Ok(())
    }

    /// Atomically make one model the segment's sole machine hypothesis. Champion transcription uses
    /// this after a successful 7B write so votes left by an older 300M/1B/MMS/Scribe run cannot remain
    /// active evidence. DELETE + INSERT must be one savepoint: a failed insert may never leave a good
    /// segment with no provenance merely because cleanup ran first.
    pub fn replace_hypotheses_with(&self, hyp: &SegmentHypothesis) -> AppResult<()> {
        let transcript = to_nfc(&hyp.transcript);
        self.conn.execute("SAVEPOINT replace_hypotheses", [])?;
        let result = (|| -> AppResult<()> {
            self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id = ?1", params![hyp.segment_id])?;
            self.conn.execute(
                "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hyp.segment_id, hyp.model_id, transcript, hyp.confidence],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => self.release_savepoint("replace_hypotheses"),
            Err(error) => {
                self.cleanup_savepoint_after_error("replace_hypotheses");
                Err(error)
            }
        }
    }

    /// The rights attached to the recording this segment came from (migration v49, audit #6).
    ///
    /// Read as its own row lookup rather than as fields on [`SpeechSegment`]: that struct is already
    /// wide, and every past widening broke every destructuring insert site. The export gate that needs
    /// this already runs a per-segment query for hypotheses, so this costs the same shape it already
    /// pays.
    pub fn rights_for_segment(&self, segment_id: &str) -> AppResult<RecordingRights> {
        let rights = self.conn.query_row(
            "SELECT rights_license, rights_consent_basis, rights_permitted_use,
                    rights_attribution, rights_source, rights_revoked_at
             FROM speech_segments WHERE id = ?1",
            params![segment_id],
            |r| {
                Ok(RecordingRights {
                    license: r.get(0)?,
                    consent_basis: r.get(1)?,
                    permitted_use: r.get(2)?,
                    attribution: r.get(3)?,
                    source: r.get(4)?,
                    revoked_at: r.get(5)?,
                })
            },
        );
        match rights {
            Ok(v) => Ok(v),
            // A missing row is UNKNOWN rights, never "permitted": the default must fail closed.
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(RecordingRights::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Declare rights for a RECORDING — every segment cut from that source file, in one statement.
    ///
    /// Storage is per row (see migration v49) but the unit of consent is the recording, so the API
    /// takes an audio path. Returns the number of segments updated so a caller can report what it
    /// actually covered rather than what it attempted.
    ///
    /// Deliberately does NOT clear `rights_revoked_at`: re-declaring a licence must not silently
    /// resurrect a withdrawn recording. Un-revoking is a separate, explicit act.
    pub fn set_recording_rights(&self, audio_path: &str, rights: &RecordingRights) -> AppResult<usize> {
        Ok(self.conn.execute(
            "UPDATE speech_segments
                SET rights_license = ?2, rights_consent_basis = ?3, rights_permitted_use = ?4,
                    rights_attribution = ?5, rights_source = ?6, updated_at = datetime('now')
              WHERE audio_path = ?1",
            params![
                audio_path,
                rights.license,
                rights.consent_basis,
                rights.permitted_use,
                rights.attribution,
                rights.source,
            ],
        )?)
    }

    /// Record a withdrawal of consent for a recording. Stamps every segment cut from it.
    ///
    /// This is the revocation lineage: once stamped, `rights_disposition` returns `Revoked` and every
    /// export path — including plain local export — must drop the row. A withdrawal that only blocks
    /// future publishing is not a withdrawal.
    /// Persist BOTH identity tiers for every segment of one source recording (v50 spectral + v51 hash).
    ///
    /// Keyed on `audio_path` like `set_recording_rights`: all VAD chunks of one recording share it, and
    /// the identity being recorded is the RECORDING's, not the chunk's.
    ///
    /// Both columns in ONE statement deliberately: a caller that could write the spectral value without
    /// the content hash would re-create the pre-v51 state on the next restart, where a rehydrated entry
    /// has a bucket key but nothing able to prove identity.
    ///
    /// `spectral as i64` is a bit-cast, not a numeric conversion — SQLite integers are i64 and the
    /// value is a u64, so the top bit round-trips only because both directions cast rather than
    /// convert. `load_audio_identities` casts back the same way.
    pub fn set_audio_identity(&self, audio_path: &str, identity: &AudioIdentity) -> AppResult<usize> {
        Ok(self.conn.execute(
            "UPDATE speech_segments SET audio_fingerprint = ?2, audio_content_hash = ?3
              WHERE audio_path = ?1",
            params![audio_path, identity.spectral as i64, identity.content],
        )?)
    }

    /// Every stored recording identity, for rehydrating the in-memory dedup map at startup.
    ///
    /// DISTINCT because a recording is many chunks sharing one path and one identity; without it a
    /// 144-chunk file would return 144 identical rows. Rows whose spectral value was never computed
    /// (every row predating v50, until the backfill runs) are skipped by the WHERE, not defaulted to 0 —
    /// a zero value is the degenerate silent-window bucket `register` deliberately refuses to store.
    ///
    /// `content` is `None` for v50-era rows: they have a bucket key but were never hashed. The dedup map
    /// keeps them and never lets them reject, because a value that cannot distinguish content must not
    /// be allowed to discard a legitimate recording. `backfill_fingerprints` closes that gap.
    pub fn load_audio_identities(&self) -> AppResult<Vec<StoredAudioIdentity>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT audio_fingerprint, audio_content_hash, audio_path FROM speech_segments
             WHERE audio_fingerprint IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(StoredAudioIdentity {
                    spectral: r.get::<_, i64>(0)? as u64,
                    content: r.get::<_, Option<String>>(1)?,
                    audio_path: r.get::<_, String>(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn revoke_recording(&self, audio_path: &str) -> AppResult<usize> {
        Ok(self.conn.execute(
            "UPDATE speech_segments
                SET rights_revoked_at = COALESCE(rights_revoked_at, datetime('now')),
                    updated_at = datetime('now')
              WHERE audio_path = ?1",
            params![audio_path],
        )?)
    }

    /// Every distinct source recording plus its rights, for the operator view.
    pub fn list_recording_rights(&self) -> AppResult<Vec<(String, usize, RecordingRights)>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, COUNT(*), rights_license, rights_consent_basis, rights_permitted_use,
                    rights_attribution, rights_source, rights_revoked_at
             FROM speech_segments GROUP BY audio_path ORDER BY audio_path",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as usize,
                    RecordingRights {
                        license: r.get(2)?,
                        consent_basis: r.get(3)?,
                        permitted_use: r.get(4)?,
                        attribution: r.get(5)?,
                        source: r.get(6)?,
                        revoked_at: r.get(7)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_hypotheses_for_segment(&self, segment_id: &str) -> AppResult<Vec<SegmentHypothesis>> {
        let mut stmt = self.conn.prepare(
            "SELECT segment_id, model_id, transcript, confidence
             FROM segment_hypotheses WHERE segment_id = ?1
             ORDER BY created_at DESC, model_id ASC",
        )?;
        let rows = stmt.query_map(params![segment_id], |row| {
            Ok(SegmentHypothesis {
                segment_id: row.get(0)?,
                model_id: row.get(1)?,
                transcript: row.get(2)?,
                confidence: row.get(3)?,
            })
        })?;
        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    pub fn delete_hypotheses_for_segment(&self, segment_id: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id = ?1", params![segment_id])?;
        Ok(())
    }

    /// Seal a dataset snapshot: an IMMUTABLE record of exactly which rows a training pack contained.
    ///
    /// `id` is the manifest's content hash, so the same rows always produce the same snapshot id and
    /// a different selection can never masquerade as the same one. INSERT OR IGNORE is the
    /// immutability: re-exporting identical data is a no-op, and an existing snapshot is never
    /// rewritten — a training run that cites a snapshot id must be able to trust that what it cites
    /// has not changed underneath it.
    ///
    /// Returns true when this call sealed a NEW snapshot, false when the id already existed.
    pub fn seal_dataset_snapshot(&self, id: &str, name: &str, config_json: &str) -> AppResult<bool> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO dataset_runs (id, name, status, config_json, completed_at)
             VALUES (?1, ?2, 'sealed', ?3, datetime('now'))",
            params![id, name, config_json],
        )?;
        self.track_write()?;
        Ok(changed > 0)
    }

    /// A sealed snapshot's stored record, or `None` if that id was never sealed.
    pub fn dataset_snapshot(&self, id: &str) -> AppResult<Option<(String, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT name, status, config_json FROM dataset_runs WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?))),
            None => Ok(None),
        }
    }

    /// Record that `record.audio_path` was processed before import (see [`SourceAudioProvenance`]).
    pub fn upsert_source_audio_provenance(&self, record: &SourceAudioProvenance) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO source_audio_provenance
                (audio_path, processing, separator_model, timeline_preserved, manifest_path)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(audio_path) DO UPDATE SET
                processing=excluded.processing,
                separator_model=excluded.separator_model,
                timeline_preserved=excluded.timeline_preserved,
                manifest_path=excluded.manifest_path,
                recorded_at=datetime('now')",
            params![
                record.audio_path,
                record.processing,
                record.separator_model,
                record.timeline_preserved as i32,
                record.manifest_path
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// What was done to this recording before import, or `None` if nothing ever said.
    ///
    /// `None` is NOT a certificate of originality. It means unclaimed — a recording imported before
    /// this table existed reads the same as one that was never processed, which is why the export
    /// wording below states what is known rather than asserting the audio is raw.
    pub fn source_audio_provenance(&self, audio_path: &str) -> AppResult<Option<SourceAudioProvenance>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, processing, separator_model, timeline_preserved, manifest_path
             FROM source_audio_provenance WHERE audio_path = ?1",
        )?;
        let mut rows = stmt.query(params![audio_path])?;
        match rows.next()? {
            Some(row) => Ok(Some(SourceAudioProvenance {
                audio_path: row.get(0)?,
                processing: row.get(1)?,
                separator_model: row.get(2)?,
                timeline_preserved: row.get::<_, i32>(3)? != 0,
                manifest_path: row.get(4)?,
            })),
            None => Ok(None),
        }
    }

    /// Every declaration at once, keyed by source path — one query for a whole export instead of one
    /// per clip (a 550 h corpus is ~250k clips over a few thousand recordings).
    pub fn source_audio_provenance_map(&self) -> AppResult<HashMap<String, SourceAudioProvenance>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, processing, separator_model, timeline_preserved, manifest_path
             FROM source_audio_provenance",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SourceAudioProvenance {
                audio_path: row.get(0)?,
                processing: row.get(1)?,
                separator_model: row.get(2)?,
                timeline_preserved: row.get::<_, i32>(3)? != 0,
                manifest_path: row.get(4)?,
            })
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let record = row?;
            map.insert(record.audio_path.clone(), record);
        }
        Ok(map)
    }

    pub fn upsert_source_transcript(&self, record: &SourceTranscriptRecord) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO source_transcripts
                (audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(audio_path, model_id) DO UPDATE SET
                audio_content_hash=excluded.audio_content_hash,
                audio_size_bytes=excluded.audio_size_bytes,
                transcript_path=excluded.transcript_path,
                transcript_text=excluded.transcript_text,
                updated_at=datetime('now')",
            params![
                record.audio_path,
                record.model_id,
                record.audio_content_hash,
                record.audio_size_bytes,
                record.transcript_path,
                record.transcript_text
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn get_source_transcript(&self, audio_path: &str, model_id: &str) -> AppResult<Option<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1 AND model_id = ?2",
        )?;
        let mut rows = stmt.query(params![audio_path, model_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_latest_source_transcript_for_audio(
        &self,
        audio_path: &str,
    ) -> AppResult<Option<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1
             ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![audio_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_source_transcripts_for_audio(&self, audio_path: &str) -> AppResult<Vec<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1
             -- model_id breaks the tie, and the tie is the COMMON case: both timestamps have
             -- one-second granularity, and two reference transcripts for the same clip are written
             -- back to back. Without it SQLite is free to return them in any order, which made
             -- everything downstream of this list per-run nondeterministic — including the
             -- `multi-reference-consensus:a+b` provenance string that gets persisted and exported.
             ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC, model_id ASC",
        )?;
        let rows = stmt.query_map(params![audio_path], |row| {
            Ok(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// P3.2 (resume fix): segment IDs previously imported from a given source audio file. Used on
    /// import-resume to fold already-imported files back into the post-import jury batch — the jury
    /// runs once at the end keyed on the freshly-imported ids, so without this the files persisted
    /// before a crash would never be adjudicated (they are skipped from re-processing on resume).
    pub fn segment_ids_for_audio_path(&self, audio_path: &str) -> AppResult<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT id FROM speech_segments WHERE audio_path = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![audio_path], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_all_hypotheses(&self) -> AppResult<Vec<SegmentHypothesis>> {
        let mut stmt =
            self.conn.prepare("SELECT segment_id, model_id, transcript, confidence FROM segment_hypotheses")?;
        let rows = stmt.query_map([], |row| {
            Ok(SegmentHypothesis {
                segment_id: row.get(0)?,
                model_id: row.get(1)?,
                transcript: row.get(2)?,
                confidence: row.get(3)?,
            })
        })?;
        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Load the persisted per-model IRT abilities (F7). Empty when learning has never run, in which
    /// case the consensus falls back to the hardcoded heuristic priors (identical to the old behavior).
    pub fn load_model_abilities(&self) -> AppResult<std::collections::HashMap<String, f64>> {
        let mut stmt = self.conn.prepare("SELECT model_id, ability FROM model_abilities")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (id, ability) = r?;
            map.insert(id, ability);
        }
        Ok(map)
    }

    /// Upsert the EM-fitted per-model IRT abilities (F7). Only finite abilities are stored.
    pub fn save_model_abilities(&self, abilities: &std::collections::HashMap<String, f64>) -> AppResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO model_abilities (model_id, ability, updated_at) VALUES (?1, ?2, datetime('now'))
                 ON CONFLICT(model_id) DO UPDATE SET ability = excluded.ability, updated_at = excluded.updated_at",
            )?;
            for (model_id, ability) in abilities {
                if ability.is_finite() {
                    stmt.execute(rusqlite::params![model_id, ability])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The row's database-owned monotonic review revision, encoded as text to preserve the existing
    /// Couch Review JSON wire shape (`rowVersion` was historically a timestamp string). Unlike
    /// `updated_at`, this changes for every update even when several writes land in the same second.
    pub fn segment_row_stamp(&self, segment_id: &str) -> AppResult<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT CAST(review_revision AS TEXT) FROM speech_segments WHERE id = ?1",
                params![segment_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn segment_review_revision(&self, segment_id: &str) -> AppResult<Option<i64>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row("SELECT review_revision FROM speech_segments WHERE id = ?1", params![segment_id], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Returns the number of rows actually changed — human-reviewed rows are skipped by the guard,
    /// so this can be less than `updates.len()`; callers must report THIS, not the attempted count.
    pub fn update_segment_consensus_batch(&self, updates: &[(String, String, String, f64)]) -> AppResult<usize> {
        self.conn.execute("SAVEPOINT consensus_batch", [])?;
        let result: AppResult<usize> = (|| {
            let mut stmt = self.conn.prepare(
                // Guard: never overwrite a human-reviewed/edited segment with machine consensus —
                // mirrors update_asr_transcript_if_unreviewed and merge_dataset_json. Without this,
                // running the consensus refinery silently discards human corrections.
                // `confidence_source` is restamped WITH the confidence it now describes: the stored
                // number becomes an IRT-consensus score, and leaving the decoder's tag (e.g.
                // "real_posterior") on it is a provenance lie — conformal.rs branches on that exact
                // token when counting real-posterior calibration coverage.
                "UPDATE speech_segments
                 SET raw_transcript = ?2,
                     normalized_transcript = ?3,
                     confidence = ?4,
                     confidence_source = 'irt_consensus',
                     updated_at = datetime('now')
                 WHERE id = ?1
                   AND verified = 0
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            )?;
            let mut changed = 0usize;
            for (seg_id, cons, norm, conf) in updates {
                // NFC-canonicalize the consensus transcript + its normalization before they hit the
                // FTS-indexed columns — same guard as the other write paths, so machine consensus
                // doesn't store a decomposed form that search can't match.
                changed += stmt.execute(params![seg_id, to_nfc(cons), to_nfc(norm), conf])?;
            }
            Ok(changed)
        })();
        match result {
            Ok(changed) => {
                self.release_savepoint("consensus_batch")?;
                self.track_write()?;
                Ok(changed)
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("consensus_batch");
                Err(e)
            }
        }
    }

    pub fn update_ctc_score(&self, id: &str, score: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET ctc_score = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, score],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_signal_anomaly_score(&self, id: &str, score: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET signal_anomaly_score = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, score],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_segment_split(&self, id: &str, split: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET split = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, split],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_quality_metrics(&self, id: &str, clipping: f64, rms: f64, snr: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET clipping_ratio = ?2, rms_db = ?3, snr_db = ?4, updated_at = datetime('now') WHERE id = ?1",
            params![id, clipping, rms, snr],
        )?;
        self.track_write()?;
        Ok(())
    }

    fn track_write(&self) -> AppResult<()> {
        // Placeholder for write-tracking if needed by external observers
        Ok(())
    }

    /// Persist word timings AND their honest quality marker in ONE atomic statement.
    /// `alignment_json` is metadata (chunk window + per-word timings), NOT FTS-indexed transcript
    /// text, so no NFC canonicalization is needed. `quality`: "ctc_forced" | "energy_heuristic".
    ///
    /// These two columns must never be written as separate statements: quality.rs raises the
    /// `energy_heuristic_alignment` review-risk reason only when the marker is PRESENT, so timings
    /// that land without their marker read as trustworthy alignment. The old two-statement pair had
    /// exactly that window — and the background aligner swallowed the second write's error outright
    /// (`let _ =`), silently laundering heuristic timestamps whenever the quality stamp failed.
    pub fn update_segment_alignment(&self, segment_id: &str, alignment_json: &str, quality: &str) -> AppResult<()> {
        crate::validation::input::validate_alignment_json(alignment_json).map_err(AppError::Validation)?;
        self.conn.execute(
            "UPDATE speech_segments
             SET alignment_json = ?2, alignment_quality = ?3, updated_at = datetime('now')
             WHERE id = ?1",
            params![segment_id, alignment_json, quality],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Persist alignment output only if the canonical alignment read before slow inference is still
    /// current. SQLite `IS` is deliberately used instead of `=` so `None` compares NULL-safely. A
    /// concurrent boundary/timing edit returns `Ok(false)` and is never overwritten.
    pub fn update_segment_alignment_if_unchanged(
        &self,
        segment_id: &str,
        expected_alignment: Option<&str>,
        alignment_json: &str,
        quality: &str,
    ) -> AppResult<bool> {
        crate::validation::input::validate_alignment_json(alignment_json).map_err(AppError::Validation)?;
        let changed = self.conn.execute(
            "UPDATE speech_segments
             SET alignment_json = ?3, alignment_quality = ?4, updated_at = datetime('now')
             WHERE id = ?1 AND alignment_json IS ?2",
            params![segment_id, expected_alignment, alignment_json, quality],
        )?;
        if changed > 0 {
            self.track_write()?;
        }
        Ok(changed > 0)
    }

    /// Record the within-clip speaker-change measurement (Migration v47).
    ///
    /// Writes ONE column. Nothing here may touch the transcript, the verdict or the human decision:
    /// this is a measurement about the audio, and the tool that produces it runs over the whole
    /// library at once — a wider write would put every reviewed row in its blast radius.
    ///
    /// `updated_at` is deliberately NOT bumped. It means "when did this row's CONTENT last change",
    /// and re-running a measurement changes nothing a reviewer or an export would read differently.
    pub fn set_speaker_change_score(&self, segment_id: &str, score: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET speaker_change_score = ?2 WHERE id = ?1",
            params![segment_id, score],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Read a column that an older schema may lack (the jury cols were added by Migration v11 and
    /// alignment_quality by v12). A genuinely ABSENT column (`InvalidColumnIndex`, i.e. a row read
    /// through a pre-migration schema) yields `None`, and a SQL `NULL` yields `None` — both the
    /// intended defaults. But a type-mismatch / decode fault PROPAGATES instead of being masked:
    /// silently defaulting one of these on a decode error would misreport a genuinely gold /
    /// human-reviewed segment as `is_gold = false` / `human_decision = None`, which the
    /// human-protection guards key on — the exact silent-corruption the honesty rule forbids. This
    /// mirrors the strict `?` handling of columns 0-16 and the fail-closed `is_gold` read in
    /// `record_model_correction`.
    fn optional_col<T: rusqlite::types::FromSql>(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<T>> {
        match row.get::<_, Option<T>>(idx) {
            Ok(value) => Ok(value),
            Err(rusqlite::Error::InvalidColumnIndex(_)) => Ok(None),
            Err(other) => Err(other),
        }
    }

    fn map_row(row: &rusqlite::Row) -> rusqlite::Result<SpeechSegment> {
        Ok(SpeechSegment {
            id: row.get(0)?,
            created_at: row.get(1)?,
            audio_path: row.get(2)?,
            raw_transcript: row.get(3)?,
            normalized_transcript: row.get(4)?,
            annotated_transcript: row.get(5)?,
            alignment_json: row.get(6)?,
            duration_ms: row.get(7)?,
            speaker_id: row.get(8)?,
            verified: row.get::<_, i32>(9)? != 0,
            confidence: row.get(10)?,
            ctc_score: row.get(11)?,
            clipping_ratio: row.get(12)?,
            rms_db: row.get(13)?,
            snr_db: row.get(14)?,
            split: row.get(15)?,
            signal_anomaly_score: row.get(16)?,
            // Jury fields — added by Migration v11. Default ONLY when the column is genuinely absent
            // (old schema) or NULL; a decode error propagates so it can't silently strip provenance.
            verdict: Self::optional_col(row, 17)?,
            verdict_transcript: Self::optional_col(row, 18)?,
            rationale: Self::optional_col(row, 19)?,
            evidence_json: Self::optional_col(row, 20)?,
            agreement_score: Self::optional_col(row, 21)?,
            escalated: Self::optional_col::<i32>(row, 22)?.unwrap_or(0) != 0,
            human_decision: Self::optional_col(row, 23)?,
            corrected_at: Self::optional_col(row, 24)?,
            is_gold: Self::optional_col::<i32>(row, 25)?.unwrap_or(0) != 0,
            // Alignment quality — added by Migration v12; same fail-closed treatment.
            alignment_quality: Self::optional_col(row, 26)?,
            model_version_id: Self::optional_col(row, 27)?,
            confidence_source: Self::optional_col(row, 28)?,
            cloud_call: Self::optional_col::<i32>(row, 29)?.unwrap_or(0) != 0,
            decoder_config_hash: Self::optional_col(row, 30)?,
            normalizer_version: Self::optional_col(row, 31)?,
            // Per-segment processing provenance — Migration v41; nullable 0/1 -> Option<bool>, where
            // None (absent/NULL, i.e. a legacy pre-v41 row) stays "not recorded" rather than a fake false.
            denoised: Self::optional_col::<i32>(row, 32)?.map(|v| v != 0),
            diarized: Self::optional_col::<i32>(row, 33)?.map(|v| v != 0),
            // VAD backend — Migration v42; nullable TEXT. None (absent/NULL) stays "not recorded".
            vad_backend: Self::optional_col(row, 34)?,
            // Reviewer attribution — Migration v43; nullable TEXT. None = not attributed (legacy row,
            // undecided row, or a desktop decision), never a fabricated "owner".
            reviewed_by: Self::optional_col(row, 35)?,
            // Speaker-change score — Migration v47; nullable REAL. None (absent/NULL) means NOT
            // MEASURED and must never be read as "measured, one speaker".
            speaker_change_score: Self::optional_col(row, 36)?,
        })
    }

    // ── Jury DB helpers ───────────────────────────────────────────────────────

    /// M2.3 / P1.3: record what LOOP-0 WOULD have done for a segment WITHOUT mutating it. `memory_fired`
    /// is true when a correction memory would have changed the finalized transcript. One row per shadow
    /// observation; the C5 over-trigger decision joins these to the human's later decision at analysis
    /// time (an over-trigger is a would-fire the human subsequently contradicts).
    pub fn record_loop0_shadow(&self, segment_id: &str, memory_fired: bool) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO loop0_shadow_log (segment_id, memory_fired) VALUES (?1, ?2)",
            params![segment_id, memory_fired],
        )?;
        Ok(())
    }

    /// True-10 audit: the READ side of the intelligence instrumentation. loop0_shadow_log and
    /// decision_verdicts were write-only — the C5 (LOOP-0 go-live) and C4 (auto-accept precision)
    /// decisions were impossible to make in-app. This joins both against the humans' subsequent
    /// decisions:
    ///
    /// * LOOP-0 shadow: `fired_but_human_accepted_original` is the OVER-TRIGGER count (the memory
    ///   would have changed text a human then confirmed was already right) — C5 requires this to be
    ///   0 before `loop0_firing_enabled` may ever be turned on. `fired_and_human_edited` is
    ///   inconclusive-positive (the text did need changing; whether the memory's change matched the
    ///   human's is not knowable from the flag alone).
    /// * C4: of the machine's T0 auto-accepts that a human later reviewed, how many did the human
    ///   confirm vs contradict (edit/reject) — the honest precision behind any autonomy increase.
    pub fn intelligence_report(&self) -> AppResult<serde_json::Value> {
        // Live counts over surviving segments PLUS the durable archive of segments already deleted
        // (migration v33), so the C5 over-trigger gate is not survivor-biased by ordinary cleanup.
        // Per-SEGMENT counts (true-10 audit 2026-07-09): shadow_log holds one row per OBSERVATION and
        // re-processed segments accumulate several, but C5 reasons about distinct events — one clip,
        // one human decision, at most one over-trigger. Aggregate per segment first (MAX(memory_fired)
        // = "ever would have fired for this clip"), then count segments; the v33/v34 archives fold the
        // same per-segment semantics at delete time. (Archive rows accumulated before this change may
        // carry per-observation counts — a conservative overstatement for the C5 "must be 0" gate.)
        let loop0 = self.conn.query_row(
            "WITH per_seg AS (
                 SELECT l.segment_id, MAX(l.memory_fired) AS fired, s.human_decision AS hd
                 FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id
                 GROUP BY l.segment_id
             )
             SELECT COUNT(*) + COALESCE((SELECT total_observations FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(fired), 0)
                        + COALESCE((SELECT would_fire FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('accept','human_accept') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_accepted FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('edit','human_edit') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_edited FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('reject','human_reject') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_rejected FROM loop0_evidence_archive WHERE id = 1), 0)
             FROM per_seg",
            [],
            |row| {
                Ok(serde_json::json!({
                    "totalObservations": row.get::<_, i64>(0)?,
                    "wouldFire": row.get::<_, i64>(1)?,
                    "firedButHumanAcceptedOriginal": row.get::<_, i64>(2)?,
                    "firedAndHumanEdited": row.get::<_, i64>(3)?,
                    "firedAndHumanRejected": row.get::<_, i64>(4)?,
                }))
            },
        )?;
        // Live counts PLUS the v34 durable archive — deleting a reviewed clip must not shrink
        // t0HumanContradicted (the C4 precision could only drift optimistic; same class as v33/C5).
        let c4 = self.conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_accepts FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T1_ESCALATE' THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t1_escalations FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' AND s.human_decision IN ('accept','human_accept') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_human_confirmed FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' AND s.human_decision IN ('edit','human_edit','reject','human_reject') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_human_contradicted FROM c4_evidence_archive WHERE id = 1), 0)
             FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id",
            [],
            |row| {
                Ok(serde_json::json!({
                    "t0Accepts": row.get::<_, i64>(0)?,
                    "t1Escalations": row.get::<_, i64>(1)?,
                    "t0HumanConfirmed": row.get::<_, i64>(2)?,
                    "t0HumanContradicted": row.get::<_, i64>(3)?,
                }))
            },
        )?;
        // C3 honesty (true-10 audit 2026-07-09): the T0 auto-accept gate needs a Hoeffding-certified
        // per-SNR-bucket calibration set, and at the shipped constants that means ~thousands of
        // perfectly-transcribed verified clips PER BUCKET — previously invisible, so the user just
        // experienced "the jury escalates everything" with no stated reason or distance. Surface the
        // per-bucket progress: verified-with-reference counts vs the minimum needed at ZERO CER
        // (a hard lower bound — real data needs more). The gate itself is deliberately unchanged.
        let mut bucket_counts = [0i64; crate::quality::conformal::N_SNR_BUCKETS];
        {
            let mut stmt = self.conn.prepare(
                // Exclude human-REJECTED clips: "mark bad" sets verified=1 (to leave the review queue) with
                // human_decision='reject'/verdict='human_reject' while keeping annotated_transcript, so
                // without this guard a discarded clip counts as a "verified-with-reference" calibration
                // sample — overstating C3 progress toward T0 auto-accept. Matches quality::is_human_rejected,
                // which every export/gate path uses to drop these rows.
                "SELECT snr_db FROM speech_segments
                 WHERE verified = 1 AND annotated_transcript IS NOT NULL AND TRIM(annotated_transcript) != ''
                   AND NOT (COALESCE(human_decision,'') IN ('reject','human_reject') OR COALESCE(verdict,'') = 'human_reject')",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, Option<f64>>(0))?;
            for snr in rows {
                bucket_counts[crate::quality::conformal::snr_bucket(snr?)] += 1;
            }
        }
        // The T0 gate's shipped constants (jury/mod.rs): target 5% CER at 90% joint confidence,
        // Bonferroni-split across the buckets.
        let target_error = 0.05;
        let per_bucket_delta = (1.0 - 0.90) / crate::quality::conformal::N_SNR_BUCKETS as f64;
        let min_needed = crate::quality::conformal::min_calibration_n(target_error, per_bucket_delta);
        let bucket_labels = ["<5 dB (very noisy)", "5-15 dB", "15-25 dB", ">25 dB (clean)", "unknown SNR"];
        let calibration: Vec<serde_json::Value> = (0..crate::quality::conformal::N_SNR_BUCKETS)
            .map(|b| {
                serde_json::json!({
                    "bucket": bucket_labels[b],
                    "verifiedWithReference": bucket_counts[b],
                    "minNeededAtZeroCer": min_needed,
                })
            })
            .collect();
        let conformal_progress = serde_json::json!({
            "targetErrorCer": target_error,
            "perBucketDelta": per_bucket_delta,
            "minNeededAtZeroCer": min_needed,
            "buckets": calibration,
        });
        Ok(serde_json::json!({
            "loop0Shadow": loop0,
            "autoAcceptPrecision": c4,
            "conformalCalibration": conformal_progress,
        }))
    }

    /// M2.2 / P1.2: classify a MACHINE verdict as T0 (auto-resolved, no human needed) or T1
    /// (escalated to a human) and record it in decision_verdicts — the denominator/index for the C4
    /// auto-accept-precision measurement. Human verdicts (`human_*`) and any unknown string record
    /// nothing: they are not machine auto-accept decisions. The raw verdict stays on
    /// speech_segments.verdict, so a C4 query can still recover auto_accept-vs-jury_accept-vs-jury_edit.
    /// Call ONLY after the verdict UPDATE affected the row (segment was not already human-decided), so a
    /// stale/late machine verdict never plants a phantom T0/T1 over a human's decision.
    pub fn record_decision_verdict(&self, segment_id: &str, verdict: &str, escalated: bool) -> AppResult<()> {
        let auto_accept_verdict = if escalated || verdict == "escalated" {
            "T1_ESCALATE"
        } else if matches!(verdict, "auto_accept" | "jury_accept" | "jury_edit") {
            "T0_ACCEPT"
        } else {
            return Ok(());
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO decision_verdicts (segment_id, auto_accept_verdict, verdict_computed_at)
             VALUES (?1, ?2, datetime('now'))",
            params![segment_id, auto_accept_verdict],
        )?;
        Ok(())
    }

    /// Write a MACHINE jury verdict to a segment (T0/T1/T2 and the agentic/escalation paths).
    ///
    /// The human-review path is `record_human_decision`, NOT this function. A machine verdict must never
    /// overwrite a human decision: the jury runs on a SEPARATE WAL connection from the human path, reads
    /// its segment snapshot once at the start of a run, then may block for seconds on a T2 cloud call —
    /// so a curator can accept/edit the same segment mid-run. Without this guard the late machine write
    /// would silently revert the human's `verdict` (the COALESCE-preferred gold transcript source) and
    /// flip `escalated` back, mis-routing the segment. The predicate mirrors the consensus/ASR write
    /// paths (the `human_decision`/`verdict NOT IN (human_*)` guards elsewhere in this file): a verdict
    /// for an already-human-decided segment matches 0 rows and is a no-op, leaving the human authoritative.
    #[allow(clippy::too_many_arguments)]
    pub fn write_segment_verdict(
        &self,
        segment_id: &str,
        verdict: &str,
        transcript: Option<&str>,
        rationale: Option<&str>,
        evidence_json: Option<&str>,
        agreement_score: Option<f64>,
        escalated: bool,
    ) -> AppResult<()> {
        // agreement_score uses COALESCE(?6, existing): a machine verdict that carries NO confidence
        // signal must never destroy a previously persisted one. The T1/T2 escalation paths (cloud-off,
        // audio-prep failure, no-majority) all write None moments after run_t0_gate persisted the real
        // IRT confidence for the same segment — the unconditional overwrite NULLed it, and both
        // suspect-first orderings (COALESCE(agreement_score, 0.5)) collapsed back to recency: the one
        // live review-speed feature was silently nominal (true-10 audit 2026-07-09). No caller has a
        // legitimate "clear the confidence" case; callers that HAVE a signal pass Some and still win.
        // SAVEPOINT (write-path audit, Week 2): the verdict UPDATE and its decision-log INSERT are one
        // invariant — a crash or SQLITE_BUSY between them used to leave a written verdict with no C4
        // denominator record. Same idiom as delete_segment/del_seg.
        self.conn.execute("SAVEPOINT verdict_write", [])?;
        let result: AppResult<()> = (|| {
            let affected = self.conn.execute(
                "UPDATE speech_segments
                 SET verdict              = ?2,
                     verdict_transcript   = ?3,
                     -- v48: the SAME text, kept where no human path can overwrite it. `verdict_transcript`
                     -- is whichever verdict is current and `record_human_decision_by` replaces it with the
                     -- reviewer's correction, which is why the label-quality lift compared the human's
                     -- answer with itself on every decided row. Written ONLY by the machine-verdict
                     -- writers (here and jury::write_verdict), never by the human-decision path, so
                     -- the machine's own output survives the human's.
                     jury_transcript      = ?3,
                     rationale            = ?4,
                     evidence_json        = ?5,
                     agreement_score     = COALESCE(?6, agreement_score),
                     escalated            = ?7,
                     updated_at           = datetime('now')
                 WHERE id = ?1
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept', 'human_edit', 'human_reject'))",
                params![segment_id, verdict, transcript, rationale, evidence_json, agreement_score, escalated as i32],
            )?;
            if affected == 0 {
                // Either the row is gone or a human already decided it — in both cases the machine verdict
                // correctly does not apply. Logged (not an error) so the no-op is visible without masking it.
                tracing::debug!(
                    "write_segment_verdict({segment_id}, {verdict}): no-op — segment is human-decided or missing"
                );
            } else {
                // M2.2/P1.2: record the T0/T1 classification for the C4 denominator (no-op for human/unknown).
                self.record_decision_verdict(segment_id, verdict, escalated)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("verdict_write")?;
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("verdict_write");
                Err(e)
            }
        }
    }

    /// Fully RE-OPEN a segment whose human decision is being undone. record_human_decision OVERWRITES
    /// the prior machine verdict with the human one, so the pre-decision verdict is gone — the honest
    /// reset is "un-adjudicated": clear the human decision AND the verdict it set, and return the segment
    /// to the review queue (escalated = 1). Clearing only human_decision (the old behavior) left a stale
    /// verdict = 'human_*' so the "undone" segment still looked decided on reload AND the machine
    /// verdict-write guard (write_segment_verdict / jury::write_verdict) would refuse to re-adjudicate it.
    pub fn clear_human_decision(&self, segment_id: &str) -> AppResult<()> {
        // Undo also retracts the DPO/few-shot learning pair the edit produced (round-24 hunt #9): the
        // agent_examples row is the ONLY provenance of a human edit, and build_dpo_dataset /
        // get_few_shot_examples filter solely on verified_by_human=1 (never on the segment's current
        // decision). Left behind, a retracted edit would permanently train the model to prefer a fix
        // the human took back. Delete it in the SAME transaction as the re-open so the two can never
        // diverge (a decision cleared with its learning pair still live, or vice versa).
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE speech_segments
             SET human_decision     = NULL,
                 corrected_at       = NULL,
                 -- The attribution belongs to the decision being undone; leaving it would credit a
                 -- reviewer for a verdict that no longer exists (v43).
                 reviewed_by        = NULL,
                 verdict            = NULL,
                 verdict_transcript = NULL,
                 rationale          = NULL,
                 evidence_json      = NULL,
                 agreement_score   = NULL,
                 escalated          = 1,
                 updated_at         = datetime('now')
             WHERE id = ?1",
            params![segment_id],
        )?;
        tx.execute("DELETE FROM agent_examples WHERE segment_id = ?1", params![segment_id])?;
        tx.commit()?;
        self.track_write()?;
        Ok(())
    }

    /// Losslessly undo one phone decision as a single compare-and-swap transaction.
    ///
    /// The previous implementation committed `clear_human_decision` and the snapshot restore as two
    /// independent writes. If the second failed, the row was permanently half-undone and the retry
    /// fence rejected it because the first write had already cleared `reviewed_by`. Here the full-row
    /// restore and retraction of its trainable example either both commit or both roll back. A revision
    /// mismatch returns `Ok(None)` without changing anything.
    pub fn undo_phone_human_decision(
        &self,
        previous: &SpeechSegment,
        reviewer: &str,
        expected_revision: i64,
    ) -> AppResult<Option<i64>> {
        validate_segment(previous)?;
        let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(previous);
        let verdict_transcript_nfc = previous.verdict_transcript.as_deref().map(to_nfc);
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE speech_segments SET
                 created_at = COALESCE(:created_at, created_at),
                 audio_path = :audio_path,
                 raw_transcript = :raw_transcript,
                 normalized_transcript = :normalized_transcript,
                 annotated_transcript = :annotated_transcript,
                 alignment_json = :alignment_json,
                 duration_ms = :duration_ms,
                 speaker_id = :speaker_id,
                 verified = :verified,
                 confidence = :confidence,
                 ctc_score = :ctc_score,
                 clipping_ratio = :clipping_ratio,
                 rms_db = :rms_db,
                 snr_db = :snr_db,
                 split = :split,
                 signal_anomaly_score = :signal_anomaly_score,
                 verdict = :verdict,
                 verdict_transcript = :verdict_transcript,
                 rationale = :rationale,
                 evidence_json = :evidence_json,
                 agreement_score = :agreement_score,
                 escalated = :escalated,
                 human_decision = :human_decision,
                 corrected_at = :corrected_at,
                 is_gold = :is_gold,
                 alignment_quality = :alignment_quality,
                 model_version_id = COALESCE(:model_version_id, 'unknown@pre-registry'),
                 confidence_source = COALESCE(:confidence_source, 'unknown'),
                 cloud_call = :cloud_call,
                 decoder_config_hash = :decoder_config_hash,
                 normalizer_version = :normalizer_version,
                 denoised = :denoised,
                 diarized = :diarized,
                 vad_backend = :vad_backend,
                 reviewed_by = :reviewed_by,
                 speaker_change_score = :speaker_change_score,
                 updated_at = datetime('now')
             WHERE id = :id
               AND review_revision = :expected_revision
               AND reviewed_by = :reviewer",
            rusqlite::named_params! {
                ":id": previous.id,
                ":expected_revision": expected_revision,
                ":reviewer": reviewer,
                ":created_at": previous.created_at,
                ":audio_path": previous.audio_path,
                ":raw_transcript": raw_nfc,
                ":normalized_transcript": normalized_nfc,
                ":annotated_transcript": annotated_nfc,
                ":alignment_json": previous.alignment_json,
                ":duration_ms": previous.duration_ms,
                ":speaker_id": previous.speaker_id,
                ":verified": previous.verified as i32,
                ":confidence": previous.confidence,
                ":ctc_score": previous.ctc_score,
                ":clipping_ratio": previous.clipping_ratio,
                ":rms_db": previous.rms_db,
                ":snr_db": previous.snr_db,
                ":split": previous.split,
                ":signal_anomaly_score": previous.signal_anomaly_score,
                ":verdict": previous.verdict,
                ":verdict_transcript": verdict_transcript_nfc,
                ":rationale": previous.rationale,
                ":evidence_json": previous.evidence_json,
                ":agreement_score": previous.agreement_score,
                ":escalated": previous.escalated as i32,
                ":human_decision": previous.human_decision,
                ":corrected_at": previous.corrected_at,
                ":is_gold": previous.is_gold as i32,
                ":alignment_quality": previous.alignment_quality,
                ":model_version_id": previous.model_version_id,
                ":confidence_source": previous.confidence_source,
                ":cloud_call": previous.cloud_call as i32,
                ":decoder_config_hash": previous.decoder_config_hash,
                ":normalizer_version": previous.normalizer_version,
                ":denoised": previous.denoised.map(i32::from),
                ":diarized": previous.diarized.map(i32::from),
                ":vad_backend": previous.vad_backend,
                ":reviewed_by": previous.reviewed_by,
                ":speaker_change_score": previous.speaker_change_score,
            },
        )?;
        if changed == 0 {
            tx.rollback()?;
            return Ok(None);
        }
        tx.execute("DELETE FROM agent_examples WHERE segment_id = ?1", params![previous.id])?;
        let restored_revision: i64 =
            tx.query_row("SELECT review_revision FROM speech_segments WHERE id = ?1", params![previous.id], |row| {
                row.get(0)
            })?;
        // Carry THIS reviewer's best listening receipt forward to the restored revision. The decision
        // bumped the revision past the receipt, so without this the immediate re-decision on a clip
        // the reviewer just heard, decided and undid was refused 428 — Undo punished listening.
        // Fingerprint equality is the invariant that makes the carry honest: the revision is a write
        // counter, but the receipt names the exact audio bytes, and those are unchanged. One row,
        // best coverage, only if no receipt already sits at the restored revision, only for the
        // reviewer doing the undo — everyone else still owes their own listen.
        tx.execute(
            "INSERT INTO playback_receipts (segment_id, segment_revision, audio_fingerprint, reviewer,                                             session_id, started_at_ms, played_ms, clip_duration_ms,                                             coverage_ratio, policy_version)              SELECT segment_id, ?3, audio_fingerprint, reviewer, session_id, started_at_ms,                     played_ms, clip_duration_ms, coverage_ratio, policy_version              FROM playback_receipts              WHERE segment_id = ?1 AND reviewer IS ?2                AND audio_fingerprint = (SELECT COALESCE(NULLIF(TRIM(COALESCE(audio_fingerprint, '')), ''),                                                          'id:' || id)                                          FROM speech_segments WHERE id = ?1)                AND NOT EXISTS (SELECT 1 FROM playback_receipts p2                                 WHERE p2.segment_id = ?1 AND p2.reviewer IS ?2                                   AND p2.segment_revision = ?3)              ORDER BY coverage_ratio DESC LIMIT 1",
            params![previous.id, reviewer, restored_revision],
        )?;
        tx.commit()?;
        self.track_write()?;
        Ok(Some(restored_revision))
    }

    /// Reverse a UI `flag()` escalation (the review-inbox Undo path): clear the `escalated` flag and the
    /// machine `'escalated'` verdict + rationale that flag wrote, WITHOUT touching a human_decision (flag
    /// never sets one). This is the exact inverse of flag — unlike `clear_human_decision`, which
    /// deliberately SETS escalated=1 to reopen a human-decided row for re-adjudication. Guarded to a
    /// still-undecided row so it can never stomp a human decision made after the flag; idempotent. Every
    /// SET expression references the row's PRE-UPDATE values (SQLite semantics), so both CASEs see the
    /// original verdict.
    pub fn clear_escalation(&self, segment_id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments
             SET escalated  = 0,
                 verdict    = CASE WHEN verdict = 'escalated' THEN NULL ELSE verdict END,
                 rationale  = CASE WHEN verdict = 'escalated' THEN NULL ELSE rationale END,
                 updated_at = datetime('now')
             WHERE id = ?1 AND (human_decision IS NULL OR human_decision = '')",
            params![segment_id],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Capture a MODEL correction (the jury auto-correcting OmniASR) as a provenance-tagged PSEUDO
    /// example: `source='model'`, `verified_by_human=0`. Unlike a human edit, this is NOT trusted
    /// training data — it is a candidate for human review / a future gated pseudo-label pass, and is
    /// excluded from the DPO export and few-shot context until a human signs off. Training directly
    /// on model-generated corrections causes model collapse (Shumailov et al., Nature 2024), so this
    /// path only RECORDS; it never promotes a label into the trainable pool.
    ///
    /// No-ops when the corrected text equals the wrong text (not a real correction) or the segment is
    /// gold/holdout (quarantined at capture). Best-effort: returns Ok even when it records nothing.
    pub fn record_model_correction(
        &self,
        segment_id: &str,
        wrong_transcript: &str,
        corrected_transcript: &str,
        corrector_model_id: &str,
    ) -> AppResult<()> {
        let wrong = wrong_transcript.trim();
        let fix = corrected_transcript.trim();
        if fix.is_empty() || wrong == fix {
            return Ok(()); // not a correction
        }
        // Quarantine gold at capture time (holdout exclusion is also applied at every export). Distinguish
        // "no such segment" (genuinely not gold -> 0) from a TRANSIENT read error (e.g. SQLITE_BUSY after
        // the busy_timeout, under a long adjudication on the other connection): the latter must NOT
        // fail-OPEN the quarantine by defaulting to 0 and writing a model pseudo-label onto a gold row —
        // propagate it so the best-effort caller simply skips this capture.
        let is_gold: i64 =
            match self
                .conn
                .query_row("SELECT is_gold FROM speech_segments WHERE id = ?1", params![segment_id], |r| r.get(0))
            {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => 0,
                Err(e) => return Err(e.into()),
            };
        if is_gold != 0 {
            return Ok(());
        }
        let example_id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO agent_examples
                 (id, segment_id, wrong_transcript, human_fix, source, verified_by_human, corrector_model_id)
             VALUES (?1, ?2, ?3, ?4, 'model', 0, ?5)",
            params![example_id, segment_id, wrong, fix, corrector_model_id],
        )?;
        Ok(())
    }

    /// Record a human decision (accept/edit/reject) and optionally store a
    /// corrected transcript.  Gold segments are updated but never written to
    /// agent_examples. M2.1: Also logs decision timing to decision_log table.
    pub fn record_human_decision(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
    ) -> AppResult<()> {
        self.record_human_decision_by(segment_id, decision, corrected_transcript, timestamp_ms, None)
    }

    /// [`record_human_decision`] with reviewer attribution (Migration v43).
    ///
    /// `annotator` names the human who made THIS decision — a named Couch Review reviewer. `None` means
    /// "not attributed" and is the correct value for the owner's own desktop, where there is exactly one
    /// human and no token naming them; it is stored as SQL NULL rather than a fabricated "owner", because
    /// a provenance column that invents its own values is worse than an empty one.
    ///
    /// The attribution is written INSIDE the same transaction as the verdict, so a crash can never leave a
    /// decision whose author is unknown (or an author for a decision that never committed).
    pub fn record_human_decision_by(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        annotator: Option<&str>,
    ) -> AppResult<()> {
        self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            timestamp_ms,
            annotator,
            None,
            false,
            None,
        )?;
        Ok(())
    }

    /// Phone review is a complete adjudication, so the transcript/verdict, attribution, learning
    /// side-effects, annotation, and `verified` flag must share one commit. Keeping finalization in
    /// this transaction prevents an interrupted second write from leaving a decided-but-pending row.
    pub fn record_phone_human_decision_by(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        annotator: &str,
    ) -> AppResult<()> {
        let expected_revision = self
            .segment_review_revision(segment_id)?
            .ok_or_else(|| AppError::Other(format!("segment {segment_id} no longer exists")))?;
        match self.record_phone_human_decision_by_at_revision(
            segment_id,
            decision,
            corrected_transcript,
            annotator,
            expected_revision,
        )? {
            Some(_) => Ok(()),
            None => Err(AppError::Other(
                "segment changed while the phone decision was being recorded; reload and retry".into(),
            )),
        }
    }

    /// Atomically record a phone decision only if the row is still the exact revision that was
    /// served/read. `Ok(None)` is a normal compare-and-swap miss: no row or learning side effect was
    /// written. `Ok(Some(revision))` returns the post-decision revision for a safe undo token.
    pub fn record_phone_human_decision_by_at_revision(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        annotator: &str,
        expected_revision: i64,
    ) -> AppResult<Option<i64>> {
        self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            None,
            Some(annotator),
            Some(expected_revision),
            true,
            // The phone's pay-bearing audit row commits WITH the decision (2026-08-20 hunt).
            Some("couch"),
        )
    }

    /// Record a DESKTOP adjudication complete: decision, transcript, attribution and `verified` in
    /// one commit, so an interrupted second write can never leave a decided-but-pending row.
    ///
    /// The phone has had this since the finalization transaction was written; the desktop reached
    /// `verified` through a separate `update_segment_fields` call that ReviewInbox never made.
    pub fn finalize_human_review(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        annotator: Option<&str>,
    ) -> AppResult<()> {
        self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            timestamp_ms,
            annotator,
            None,
            true,
            None,
        )
        .map(|_| ())
    }

    /// Finish a row written by an older release that committed the decision but not phone
    /// finalization. This intentionally does not replay learning/audit side effects.
    pub fn finalize_phone_human_decision(&self, segment_id: &str, corrected_transcript: Option<&str>) -> AppResult<()> {
        let expected_revision = self
            .segment_review_revision(segment_id)?
            .ok_or_else(|| AppError::Other(format!("segment {segment_id} no longer exists")))?;
        match self.finalize_phone_human_decision_at_revision(segment_id, corrected_transcript, expected_revision)? {
            Some(_) => Ok(()),
            None => Err(AppError::Other(
                "segment changed while the interrupted phone decision was being finalized; reload and retry".into(),
            )),
        }
    }

    pub fn finalize_phone_human_decision_at_revision(
        &self,
        segment_id: &str,
        corrected_transcript: Option<&str>,
        expected_revision: i64,
    ) -> AppResult<Option<i64>> {
        let corrected = corrected_transcript.map(|text| to_nfc(text.trim())).filter(|text| !text.is_empty());
        let changed = self.conn.execute(
            "UPDATE speech_segments
             SET annotated_transcript = COALESCE(?2, annotated_transcript),
                 verified = 1,
                 updated_at = datetime('now')
             WHERE id = ?1 AND human_decision IS NOT NULL AND review_revision = ?3",
            params![segment_id, corrected, expected_revision],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let revision = self
            .segment_review_revision(segment_id)?
            .ok_or_else(|| AppError::Other(format!("segment {segment_id} disappeared after finalization")))?;
        self.track_write()?;
        Ok(Some(revision))
    }

    /// Collapse runs of whitespace, exactly as `check_review_serving_provenance._norm` does.
    /// A differing space is not a differing transcript, and the two sides of this invariant must
    /// agree byte-for-byte or the gate fails on rows the backend believed it had classified.
    fn provenance_norm(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// What a human decision REALLY is, decided from the stored bytes rather than the client's word.
    ///
    /// `accept` asserts a specific, checkable thing: an ASR engine produced this exact text and a
    /// human approved it unchanged. That is true on a first review and routinely FALSE on a
    /// re-review, where the displayed text is a previous human's correction. Recording the latter as
    /// `accept` launders human authorship into machine provenance — the row then claims an engine
    /// wrote words no engine ever emitted, and `check_review_serving_provenance` rejects it.
    ///
    /// So: an accept whose approved text matches one of the segment's own hypotheses stays an
    /// accept; one that matches none is reclassified `edit`, which is what it actually is — a human
    /// affirming human-authored text. The text is never invented, only carried forward.
    ///
    /// Returns the effective decision and, when it had to resolve the approved text itself, that text.
    fn authoritative_decision(
        &self,
        segment_id: &str,
        requested: &str,
        approved: Option<&str>,
    ) -> AppResult<(String, Option<String>)> {
        use rusqlite::OptionalExtension;
        if requested != "accept" {
            return Ok((requested.to_string(), None));
        }
        // Accept-what-you-SEE passes the displayed text; when it does not, the approved text is
        // whatever the export would ship for this row today.
        let (approved_text, resolved): (String, Option<String>) = match approved {
            Some(text) if !text.trim().is_empty() => (text.to_string(), None),
            _ => {
                let shipped: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT COALESCE(NULLIF(TRIM(verdict_transcript), ''),                                  NULLIF(TRIM(annotated_transcript), ''),                                  raw_transcript)                          FROM speech_segments WHERE id = ?1",
                        [segment_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                match shipped.map(|text| to_nfc(text.trim())).filter(|text| !text.is_empty()) {
                    Some(text) => (text.clone(), Some(text)),
                    // Nothing to verify against; leave the caller's decision untouched rather than
                    // invent a classification.
                    None => return Ok((requested.to_string(), None)),
                }
            }
        };

        let wanted = Self::provenance_norm(&approved_text);
        let mut stmt = self.conn.prepare("SELECT transcript FROM segment_hypotheses WHERE segment_id = ?1")?;
        let mut rows = stmt.query([segment_id])?;
        let mut saw_any_hypothesis = false;
        while let Some(row) = rows.next()? {
            let hypothesis: String = row.get(0)?;
            saw_any_hypothesis = true;
            if Self::provenance_norm(&hypothesis) == wanted {
                return Ok(("accept".to_string(), resolved));
            }
        }
        // Only a CONTRADICTION justifies reclassifying. With no hypotheses on file there is nothing
        // to contradict, and calling the decision an "edit" would launder in the other direction —
        // dressing a genuine ASR accept as human authorship because provenance was never recorded.
        // That is a data-completeness problem, and `check_review_serving_provenance` reports it as
        // one ("no ASR hypothesis on file to justify the accept"); it is not this function's to hide.
        if !saw_any_hypothesis {
            return Ok((requested.to_string(), resolved));
        }
        tracing::info!(
            "segment {segment_id}: accept reclassified as edit — the approved text matches none of              this segment's ASR hypotheses, so it is human-authored, not an engine transcript"
        );
        Ok(("edit".to_string(), Some(approved_text)))
    }

    /// The stored content fingerprint of a segment's audio, if one has been computed.
    ///
    /// Read as a single column rather than widening `SpeechSegment`: that struct's field order is
    /// load-bearing for SEGMENT_SELECT_COLUMNS' index-based map_row, and adding to it has broken
    /// every destructuring site twice before.
    /// The clip's length as the SERVER knows it — the denominator of every coverage ratio.
    ///
    /// A page reporting "I played 100ms of a 100ms clip" scores 1.0 against its own claim. The
    /// length has to come from here or the guard compares the client's claim with the client's claim.
    /// Does this recording still carry any machine PLACEHOLDER (`[...]`-shaped) or EMPTY drafts?
    ///
    /// Resume uses it to tell an ADOPTABLE finished file from a crash-interrupted stage: rows are
    /// committed before the champion pass fills them, so "rows exist" alone proved nothing — the
    /// 2026-08-14 incident left 36 placeholder rows that every later resume would have adopted as a
    /// completed file (found again by the 2026-08-20 external review). Same `[%]` narrowing the
    /// review-queue exclusion uses, so the two cannot disagree about what a placeholder is.
    pub fn audio_path_has_placeholder_rows(&self, audio_path: &str) -> AppResult<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM speech_segments
             WHERE audio_path = ?1
               AND (TRIM(COALESCE(raw_transcript, '')) LIKE '[%]' OR TRIM(COALESCE(raw_transcript, '')) = '')",
            params![audio_path],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn segment_clip_duration_ms(&self, segment_id: &str) -> AppResult<Option<i64>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT NULLIF(COALESCE(duration_ms, 0), 0) FROM speech_segments WHERE id = ?1",
                [segment_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn segment_audio_fingerprint(&self, segment_id: &str) -> AppResult<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT NULLIF(TRIM(COALESCE(audio_fingerprint, '')), '') FROM speech_segments WHERE id = ?1",
                [segment_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Refuse a gold-minting verdict that has no proof the reviewer heard THIS clip.
    ///
    /// The enforcement point is here, not in a renderer: a decision surface can be reloaded, scripted
    /// or replayed offline, so "the button was disabled" is a usability property, never a guarantee.
    /// This is the guarantee.
    ///
    /// `reject` is included deliberately. Marking a clip bad is a judgement about the AUDIO, and a
    /// reviewer who never heard it cannot make it — a wrongly rejected clip is silently dropped from
    /// the corpus, which is the most expensive mistake available and the hardest to notice later.
    /// `skip` never reaches here: it writes no verdict at all.
    pub fn require_playback_evidence(
        &self,
        segment_id: &str,
        revision: i64,
        fingerprint: &str,
        reviewer: Option<&str>,
    ) -> AppResult<()> {
        if self.has_sufficient_playback_evidence(segment_id, revision, fingerprint, reviewer)? {
            return Ok(());
        }
        Err(AppError::Validation(format!(
            "E_NO_PLAYBACK_EVIDENCE: no receipt shows this reviewer heard at least {:.0}% of segment              {segment_id} at revision {revision}. A verdict on an unheard clip is a guess, not a label.",
            MIN_PLAYBACK_COVERAGE * 100.0
        )))
    }

    /// A reviewer's own record that they HEARD this exact clip at this exact revision.
    ///
    /// `played_ms` is cumulative MEDIA time advanced, never wall-clock and never a `play()` call —
    /// download, metadata load and an autoplay attempt all prove nothing about listening.
    ///
    /// EVERY identity field is resolved HERE, from the row — revision, fingerprint, and the
    /// coverage denominator alike. The struct's values for those three are treated as claims and
    /// overwritten. Found by the 2026-08-19 hunt: the phone path resolved all three at its call
    /// site, the desktop resolved two and passed the renderer's clipDurationMs through — and both
    /// desktop surfaces report the WHOLE source file's length (403 of 414 clips share one
    /// recording), so an honest 10s listen scored ~0.004 and was refused, while a shrunk claim
    /// minted 1.0. Per-caller resolution is exactly how one caller gets it wrong; this is the one
    /// door, so no caller can.
    pub fn record_playback_receipt(&self, receipt: &PlaybackReceipt) -> AppResult<()> {
        use rusqlite::OptionalExtension;
        let row: Option<(i64, Option<String>, i64)> = self
            .conn
            .query_row(
                "SELECT COALESCE(review_revision, 0),
                        NULLIF(TRIM(COALESCE(audio_fingerprint, '')), ''),
                        COALESCE(duration_ms, 0)
                 FROM speech_segments WHERE id = ?1",
                [&receipt.segment_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((revision, fingerprint, duration_ms)) = row else {
            return Err(AppError::Validation(format!(
                "cannot mint a listening receipt for unknown segment {}",
                receipt.segment_id
            )));
        };
        let resolved = PlaybackReceipt {
            segment_revision: revision,
            audio_fingerprint: fingerprint.unwrap_or_else(|| format!("id:{}", receipt.segment_id)),
            // The row's own clip length wins; the claim survives only for a row with no duration,
            // which on the live library is zero rows of 15,905.
            clip_duration_ms: if duration_ms > 0 { duration_ms } else { receipt.clip_duration_ms },
            ..receipt.clone()
        };
        self.record_playback_receipt_raw(&resolved)
    }

    /// Mint a receipt ONLY IF the row is still at `expected_revision` — one atomic statement.
    ///
    /// The 2026-08-20 hunt found the front door's own resolution re-opening the hole the serve/decide
    /// fence had just closed: the fence verifies the serve against the current revision, then
    /// [`Self::record_playback_receipt`] re-queries the row, so a write landing in between rebinds
    /// the receipt to a revision (and fingerprint) the reviewer never heard. Check-and-insert as one
    /// `INSERT … SELECT … WHERE review_revision = ?` closes that: SQLite executes it atomically, so
    /// either the receipt is minted against exactly the verified revision or nothing is written.
    ///
    /// Returns `false` (and writes NOTHING) when the row moved or vanished — the caller treats that
    /// as the fence firing late, not as an error.
    pub fn record_playback_receipt_if_at_revision(
        &self,
        receipt: &PlaybackReceipt,
        expected_revision: i64,
    ) -> AppResult<bool> {
        let changed = self.conn.execute(
            "INSERT INTO playback_receipts (segment_id, segment_revision, audio_fingerprint, reviewer,
                                            session_id, started_at_ms, played_ms, clip_duration_ms,
                                            coverage_ratio, policy_version)
             SELECT s.id, ?2,
                    COALESCE(NULLIF(TRIM(COALESCE(s.audio_fingerprint, '')), ''), 'id:' || s.id),
                    ?3, ?4, ?5, ?6,
                    CASE WHEN COALESCE(s.duration_ms, 0) > 0 THEN s.duration_ms ELSE ?7 END,
                    CASE WHEN CASE WHEN COALESCE(s.duration_ms, 0) > 0 THEN s.duration_ms ELSE ?7 END > 0
                         THEN MIN(1.0, CAST(?6 AS REAL)
                                       / (CASE WHEN COALESCE(s.duration_ms, 0) > 0 THEN s.duration_ms ELSE ?7 END))
                         ELSE 0.0 END,
                    ?8
             FROM speech_segments s
             WHERE s.id = ?1 AND COALESCE(s.review_revision, 0) = ?2",
            params![
                receipt.segment_id,
                expected_revision,
                receipt.reviewer,
                receipt.session_id,
                receipt.started_at_ms,
                receipt.played_ms.max(0),
                receipt.clip_duration_ms.max(0),
                PLAYBACK_POLICY_VERSION,
            ],
        )?;
        Ok(changed == 1)
    }

    /// The raw writer: stores exactly what it is given, resolving nothing.
    ///
    /// For tests that must fabricate divergent worlds (a receipt at a dead revision, audio bytes
    /// that changed after the listen) and for the undo path carrying a reviewer's own evidence
    /// forward. Production surfaces go through [`Self::record_playback_receipt`]; minting a receipt
    /// from unresolved client claims through this is the exact bug the front door closed.
    pub(crate) fn record_playback_receipt_raw(&self, receipt: &PlaybackReceipt) -> AppResult<()> {
        let coverage = if receipt.clip_duration_ms > 0 {
            (receipt.played_ms as f64 / receipt.clip_duration_ms as f64).min(1.0)
        } else {
            0.0
        };
        self.conn.execute(
            "INSERT INTO playback_receipts (segment_id, segment_revision, audio_fingerprint, reviewer,                                             session_id, started_at_ms, played_ms, clip_duration_ms,                                             coverage_ratio, policy_version)              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                receipt.segment_id,
                receipt.segment_revision,
                receipt.audio_fingerprint,
                receipt.reviewer,
                receipt.session_id,
                receipt.started_at_ms,
                receipt.played_ms,
                receipt.clip_duration_ms,
                coverage,
                PLAYBACK_POLICY_VERSION,
            ],
        )?;
        Ok(())
    }

    /// Is there evidence this reviewer heard ENOUGH of the CURRENT clip to judge it?
    ///
    /// Deliberately strict about identity, not just quantity:
    ///   * the receipt must name this segment AND the revision being decided — a correction changes
    ///     the text under judgement, so the previous listen does not carry over;
    ///   * it must name the audio FINGERPRINT now on file, so a receipt cannot be replayed against a
    ///     different clip or survive the audio being swapped underneath it;
    ///   * coverage is cumulative media time, so a paused, seeked or replayed listen still counts
    ///     honestly and a download does not count at all.
    ///
    /// `reviewer` is part of the evidence's identity: listening is PERSONAL.
    ///
    /// Found by the hunt: matching on segment+revision+fingerprint alone let reviewer A's full
    /// listen evidence reviewer B's blind verdict (a clip A heard and skipped goes to B's queue
    /// with A's receipt still valid for it). `None` matches only anonymous (desktop-minted)
    /// receipts, never a named phone receipt, and vice versa: SQL `IS` treats NULL as its own
    /// identity.
    pub fn has_sufficient_playback_evidence(
        &self,
        segment_id: &str,
        revision: i64,
        fingerprint: &str,
        reviewer: Option<&str>,
    ) -> AppResult<bool> {
        use rusqlite::OptionalExtension;
        let best: Option<f64> = self
            .conn
            .query_row(
                "SELECT MAX(coverage_ratio) FROM playback_receipts                  WHERE segment_id = ?1 AND segment_revision = ?2 AND audio_fingerprint = ?3                    AND reviewer IS ?4",
                params![segment_id, revision, fingerprint, reviewer],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(best.unwrap_or(0.0) >= MIN_PLAYBACK_COVERAGE)
    }

    // Nine parameters, deliberately: each is a distinct fact about ONE adjudication (which clip,
    // what verdict, whose text, when, who, at which revision, whether this call finalizes, and which
    // surface audits it). Bundling them into a struct would move the width rather than remove it,
    // and this is a private helper with three call sites — all in this file.
    //
    // `audit_source`: when `Some`, the review_events audit row — the basis reviewers are PAID on —
    // is written INSIDE this same transaction. The 2026-08-20 hunt measured the alternative: the
    // phone wrote it as a separate best-effort INSERT after the commit, so a kill (or SQLITE_BUSY)
    // in between left a verified row whose completed work no pay metric could ever see, and the
    // outbox replay hit the duplicate fast-path without backfilling it. Requires `annotator`.
    #[allow(clippy::too_many_arguments)]
    fn record_human_decision_by_with_finalize(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        annotator: Option<&str>,
        expected_revision: Option<i64>,
        finalize: bool,
        audit_source: Option<&str>,
    ) -> AppResult<Option<i64>> {
        // `finalize` and `expected_revision` are INDEPENDENT, and conflating them cost real work.
        // Until 2026-08-20 finalize was derived as `expected_revision.is_some()` — a CAS token only
        // the phone supplies — so no desktop decision ever set `verified`. ReviewMode hid it behind a
        // second write; ReviewInbox had none, so its decisions never reached the corpus at all (the
        // export counts `verified = 1`). Nine such rows were found on the live library.
        //
        // The SQL already treats them separately (`verified = CASE WHEN ?6`, and the CAS clause is
        // `?7 IS NULL OR ...`), so a caller may finalize without a revision token.
        // NFC-canonicalize the human correction like EVERY other transcript write path (insert/restore/
        // update_*). Without it a decomposed (NFD) paste / IME input becomes the lone non-NFC label in an
        // otherwise-NFC corpus (verdict_transcript is the COALESCE-preferred gold source) and defeats the
        // no-op-edit dedup guard — which compares via learning_text_key WITHOUT NFC — so an edit that is
        // byte-different-but-NFC-identical to the wrong text emits a degenerate DPO pair.
        let corrected_owned: Option<String> =
            corrected_transcript.map(|t| to_nfc(t.trim())).filter(|value| !value.is_empty());
        let corrected_transcript = corrected_owned.as_deref();
        // AUTHORITATIVE CLASSIFICATION — the backend decides what this decision IS, never the
        // renderer's word for it. A reviewer pressing "Accept" is approving the text in front of
        // them; on a RE-review that text is a previous human's correction, and recording it as
        // `accept` would state that an ASR engine produced it. Measured 2026-08-18 on segment
        // 82681df2: five humans edited it (Rubar, Lamo, Sewa, Rubar, Roza), the sixth pressed
        // Accept, and the row then claimed the champion had written a name and punctuation that
        // appear in none of its four hypotheses.
        let (decision_owned, resolved_text) =
            self.authoritative_decision(segment_id, decision, corrected_transcript)?;
        let decision = decision_owned.as_str();
        let corrected_owned = resolved_text.or(corrected_owned);
        let corrected_transcript = corrected_owned.as_deref();
        let human_verdict = human_verdict_for_decision(decision)?;
        if decision == "edit" && corrected_transcript.is_none() {
            return Err(AppError::Validation("Human edit decisions require a corrected transcript".into()));
        }

        let (
            is_gold,
            raw_transcript,
            normalized_transcript,
            annotated_transcript,
            verdict_transcript,
            prior_verdict,
            audio_path,
            model_version_id,
        ): HumanDecisionContext = self.conn.query_row(
            "SELECT COALESCE(is_gold, 0), raw_transcript, normalized_transcript, annotated_transcript,
                        verdict_transcript, verdict, audio_path, model_version_id
                 FROM speech_segments
                 WHERE id = ?1",
            params![segment_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )?;

        // Accept-what-you-SEE, enforced at the shared root (text-provenance audit defect #22): an
        // accept arriving with no text used to keep the PRIOR verdict_transcript via COALESCE — a
        // machine jury proposal the reviewer may never have seen then graded as "human_verified"
        // gold. Snapshot what the serving law actually showed them (annotated ▸ verbatim raw).
        // Empty snapshot stays None (blank-transcript law: never persist a blank over anything).
        let accept_snapshot: Option<String> = if decision == "accept" && corrected_transcript.is_none() {
            let seen = crate::corrections::loop0_draft_text(annotated_transcript.as_deref(), &raw_transcript);
            Some(to_nfc(seen.trim())).filter(|t| !t.is_empty())
        } else {
            None
        };
        let corrected_transcript = corrected_transcript.or(accept_snapshot.as_deref());

        let rejected_learning_transcript = if decision == "edit" {
            corrected_transcript.and_then(|fix| {
                rejected_transcript_for_learning(
                    fix,
                    &[
                        verdict_transcript.clone(),
                        annotated_transcript.clone(),
                        normalized_transcript.clone(),
                        Some(raw_transcript.clone()),
                    ],
                )
            })
        } else {
            None
        };

        // For an edit, capture the durable audio identity for the corrections ledger. Best-effort:
        // computed BEFORE the transaction (no file I/O while a write is open) and, if the audio is
        // unavailable, the verdict still records — we skip the audit row rather than fail the
        // human's correction over a missing file.
        let ledger_hash = if decision == "edit" {
            crate::pipeline::source_audio_identity(Path::new(&audio_path)).ok().map(|identity| identity.content_hash)
        } else {
            None
        };

        // The model's wrong transcript for this edit (the agent proposal when available, else the
        // raw ASR) — the shared "wrong" side of both the audit-ledger row and the LOOP-0 memory.
        let wrong_side: Option<String> = if decision == "edit" {
            Some(rejected_learning_transcript.clone().unwrap_or_else(|| raw_transcript.clone()))
        } else {
            None
        };

        // LOOP-0 evidence-based confidence (true-10 audit): for every PRE-EXISTING correction memory
        // that would have fired on this segment, record whether the human's decision confirmed or
        // contradicted it. The finalized transcript the human reviewed (annotated ▸ normalized ▸ raw)
        // mirrors `pipeline::shadow_log_loop0`, so the evidence matches the shadow signal.
        //
        //   * edit   -> reference is the human's fix; a memory whose firing moves the text TOWARD it is
        //               a confirm, AWAY is an override (over-trigger).
        //   * accept -> reference IS the finalized text; any memory that fires there over-triggered on a
        //               draft the human just confirmed was already correct -> override.
        //   * reject -> inconclusive (the human discarded the whole clip, not a verdict on any word) -> skip.
        //
        // Snapshot the memories BEFORE the capture/upsert below so the memory born from THIS edit cannot
        // confirm itself. Gold is excluded to match the capture path: gold human-decisions are the firing
        // eval set (see `firing_error_delta`), and tuning the store on them would leak.
        let finalized_text =
            crate::corrections::loop0_draft_text(annotated_transcript.as_deref(), &raw_transcript).to_string();
        let confidence_reference: Option<String> = match decision {
            "edit" => corrected_transcript.map(str::to_string),
            "accept" => Some(finalized_text.clone()),
            _ => None,
        };
        type MemoryOutcomeUpdate = (String, String, String, crate::corrections::MemoryOutcome);
        let confidence_updates: Vec<MemoryOutcomeUpdate> = match (is_gold, confidence_reference.as_deref()) {
            (0, Some(reference)) => {
                let cfg = crate::corrections::FiringConfig::default();
                let mems = self.load_correction_memories()?;
                // Winner-take-all per slot (matches runtime firing): only the memory that would actually
                // fire at each slot is credited, so a losing sibling in the same slot earns no spurious
                // confirm/override.
                crate::corrections::classify_memory_outcomes(&finalized_text, reference, &mems, &cfg)
                    .into_iter()
                    .map(|(idx, outcome)| {
                        let m = &mems[idx];
                        (m.slot_key.clone(), m.wrong_token.clone(), m.human_token.clone(), outcome)
                    })
                    .collect()
            }
            _ => Vec::new(),
        };

        // DURABILITY, exactly where the data is irreplaceable. The connection runs
        // `synchronous=NORMAL`, which under WAL means a commit survives a process crash but not
        // necessarily power loss — recent commits can still sit in the OS page cache. Every other
        // write here is reproducible: audio can be re-transcribed, alignments recomputed, packs
        // re-exported. A human decision cannot. It is a person listening to a clip once, and losing
        // it means asking a paid reviewer to redo work with no way to know which clips were lost.
        //
        // SQLite refuses `PRAGMA synchronous` inside a transaction ("Safety level may not be changed
        // inside a transaction"), so the escalation happens HERE, before the write opens, and is
        // dropped after the commit. The cost lands on the decision write alone.
        self.conn.execute_batch("PRAGMA synchronous=FULL;")?;

        // The human's verdict, the learning pair, and the audit-ledger row commit together as one
        // atomic correction — a crash can never leave a verdict without its provenance, or vice versa.
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE speech_segments
             SET human_decision     = ?2,
                 verdict            = ?3,
                 verdict_transcript = COALESCE(?4, verdict_transcript),
                 escalated          = 0,
                 reviewed_by        = ?5,
                 annotated_transcript = CASE WHEN ?6 THEN COALESCE(?4, annotated_transcript)
                                             ELSE annotated_transcript END,
                 verified           = CASE WHEN ?6 THEN 1 ELSE verified END,
                 corrected_at       = datetime('now'),
                 updated_at         = datetime('now')
             WHERE id = ?1
               AND (?7 IS NULL OR review_revision = ?7)",
            // reviewed_by is set UNCONDITIONALLY (not COALESCEd): it names the author of the row's
            // CURRENT decision, so a desktop re-review of a clip a phone reviewer had decided must clear
            // the stale name rather than leave the previous reviewer credited for someone else's verdict.
            params![segment_id, decision, human_verdict, corrected_transcript, annotator, finalize, expected_revision],
        )?;
        if changed == 0 {
            tx.rollback()?;
            return Ok(None);
        }
        // Migration v53's trigger increments this in the SAME statement. Capture it before commit so
        // the caller can bind an undo entry to the exact post-decision row without a second-read race.
        let decided_revision: i64 =
            tx.query_row("SELECT review_revision FROM speech_segments WHERE id = ?1", params![segment_id], |row| {
                row.get(0)
            })?;

        // Rejecting a clip retracts any prior EDIT's learning pair (round-24 hunt #9): a human who
        // edited a segment and then rejects it as garbage audio must not keep training the model to
        // prefer that edit. build_dpo_dataset / get_few_shot_examples key only on verified_by_human=1,
        // never on the current decision, so the stale pair would survive; delete it here, in the same
        // transaction as the reject verdict. (Undo is handled the same way in clear_human_decision.)
        if decision == "reject" {
            tx.execute("DELETE FROM agent_examples WHERE segment_id = ?1", params![segment_id])?;
        }

        // Insert into agent_examples when it is an edit on a non-gold segment.
        // The rejected side must be the actual agent proposal when available,
        // not blindly the original raw ASR transcript.
        if is_gold == 0 {
            if let (Some(wrong), Some(fix)) = (rejected_learning_transcript.clone(), corrected_transcript) {
                let example_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![example_id, segment_id, wrong, fix],
                )?;
            }
        }

        // Append to the corrections provenance ledger for any edit with a concrete fix and a
        // resolvable audio identity. Holdout exclusion is applied downstream by content hash, so
        // the ledger records gold and non-gold alike — it is the full audit trail, keyed on the
        // durable audio_content_hash, that makes the training set reconstructable and every label
        // attributable to the model_version that produced it.
        if let (Some(content_hash), Some(fix), Some(wrong)) = (ledger_hash, corrected_transcript, wrong_side.as_deref())
        {
            // Only record a GENUINE correction. `wrong_side` falls back to raw_transcript when no
            // candidate differed from the fix (the model was already right), which would otherwise
            // append a row whose raw_hypothesis == human_fix (up to whitespace/case) and pollute the
            // reconstructable training ledger with non-corrections. Gate on the same learning-key
            // difference the agent_examples / LOOP-0 paths already use.
            if learning_text_key(wrong) != learning_text_key(fix) {
                let correction_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO corrections
                        (id, segment_id, audio_content_hash, raw_hypothesis, human_fix, jury_verdict, model_version_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![correction_id, segment_id, content_hash, wrong, fix, prior_verdict, model_version_id],
                )?;
            }
        }

        // LOOP 0: distil the edit into per-slot error memories so the SAME confusion is corrected on
        // the next decode with no retraining. Gold is excluded — a memory firing on a held-out clip
        // would leak into eval. Upsert on the natural key (slot + wrong + human): a repeated,
        // independently confirmed correction bumps hit_count instead of inserting a duplicate.
        if is_gold == 0 {
            if let (Some(wrong), Some(fix)) = (wrong_side.as_deref(), corrected_transcript) {
                // Dedup within THIS one correction by natural key. hit_count tracks INDEPENDENT
                // (cross-segment) confirmations — the anti-one-off guard (min_hits). A single edit that
                // repeats the SAME confusion in one sentence (e.g. a doubled phrase) yields duplicate
                // memories; without deduping, the first occurrence INSERTs the row and the second UPDATEs
                // the row just inserted, so ONE edit on ONE segment fakes a second confirmation
                // (hit_count 1). Count each distinct confusion in a correction exactly once.
                let mut seen_keys: std::collections::HashSet<(String, String, String)> =
                    std::collections::HashSet::new();
                for mem in crate::corrections::extract_substitution_memories(wrong, fix) {
                    if !seen_keys.insert((mem.slot_key.clone(), mem.wrong_token.clone(), mem.human_token.clone())) {
                        continue;
                    }
                    let bumped = tx.execute(
                        "UPDATE correction_memory SET hit_count = hit_count + 1
                         WHERE slot_key = ?1 AND wrong_token = ?2 AND human_token = ?3",
                        params![mem.slot_key, mem.wrong_token, mem.human_token],
                    )?;
                    if bumped == 0 {
                        let mem_id = uuid::Uuid::new_v4().to_string();
                        tx.execute(
                            "INSERT INTO correction_memory
                                (id, wrong_token, human_token, slot_key, phonetic_key, source_segment,
                                 model_version_id, confidence)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                mem_id,
                                mem.wrong_token,
                                mem.human_token,
                                mem.slot_key,
                                mem.phonetic_key,
                                segment_id,
                                model_version_id,
                                // Start at the Beta(1,1) prior (0.5), not the frozen 1.0: a fresh memory
                                // has zero firing-outcome evidence and must earn its way past tau_conf.
                                crate::corrections::beta_confidence(0, 0)
                            ],
                        )?;
                    }
                }
            }
        }

        // Apply the pre-computed LOOP-0 confidence evidence. Each pre-existing memory that would have
        // fired on this segment gets a confirm or an override; `confidence` becomes the Beta(1,1)
        // posterior of the updated counts (the SET expressions evaluate against the OLD row values, so
        // `+1`/`+2`/`+3` reconstruct `beta_confidence(new_confirm, new_override)` exactly). `last_fired_at`
        // records this shadow-fire against a human-reviewed segment — the column was never written before.
        for (slot_key, wrong_token, human_token, outcome) in &confidence_updates {
            let sql = match outcome {
                crate::corrections::MemoryOutcome::Confirm => {
                    "UPDATE correction_memory
                        SET confirm_count = confirm_count + 1,
                            confidence    = (confirm_count + 2.0) / (confirm_count + override_count + 3.0),
                            last_fired_at = datetime('now')
                      WHERE slot_key = ?1 AND wrong_token = ?2 AND human_token = ?3"
                }
                crate::corrections::MemoryOutcome::Override => {
                    "UPDATE correction_memory
                        SET override_count = override_count + 1,
                            confidence     = (confirm_count + 1.0) / (confirm_count + override_count + 3.0),
                            last_fired_at  = datetime('now')
                      WHERE slot_key = ?1 AND wrong_token = ?2 AND human_token = ?3"
                }
                crate::corrections::MemoryOutcome::Neutral => continue,
            };
            tx.execute(sql, params![slot_key, wrong_token, human_token])?;
        }

        // M2.1: Log decision timing to decision_log for instrumentation before M3 marathon.
        if let Some(ts_ms) = timestamp_ms {
            tx.execute(
                "INSERT INTO decision_log (segment_id, decision_type, timestamp_ms, human_decision, created_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                params![segment_id, decision, ts_ms, decision],
            )?;
        }

        // The audit row rides the SAME commit as the decision it records (see the fn doc): a
        // verified row without its pay-bearing event, or an event for a verdict that never
        // committed, are both now unrepresentable states rather than kill-window outcomes.
        if let (Some(source), Some(who)) = (audit_source, annotator) {
            let event_ts = timestamp_ms.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0)
            });
            tx.execute(
                "INSERT INTO review_events (segment_id, reviewer, action, source, timestamp_ms, duration_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, (SELECT duration_ms FROM speech_segments WHERE id = ?1))",
                params![segment_id, who, decision, source, event_ts],
            )?;
        }

        // DURABILITY, exactly where the data is irreplaceable. The connection runs
        // `synchronous=NORMAL`, which under WAL means a commit is durable against a process crash
        // but NOT necessarily against power loss: recent commits can still be sitting in the OS page
        // cache when the machine drops. Every other write here is reproducible — audio can be
        // re-transcribed, alignments recomputed, packs re-exported. A human decision cannot: it is a
        // person listening to a clip once, and losing it means asking a paid reviewer to do the work
        // again with no way to know which clips were lost.
        //
        // So this ONE commit escalates to FULL (an fsync at commit) and drops back afterwards. The
        // cost lands on the decision write alone, not on transcription or import.
        //
        // On the error path the connection is deliberately LEFT at FULL: too-durable is the safe
        // direction, and a slower connection is a better outcome than a silently less durable one.
        tx.commit()?;
        // Back to NORMAL now the durable commit is done. On the error paths above the connection is
        // deliberately left at FULL: too-durable is the safe direction to fail in.
        self.conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        self.track_write()?;
        Ok(Some(decided_revision))
    }

    /// Load all LOOP-0 correction memories for the firing rule. `apply_memories` applies the
    /// confidence / hit-count / phonetic gates itself, so every stored row is returned here.
    pub fn load_correction_memories(&self) -> AppResult<Vec<crate::corrections::MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT wrong_token, human_token, slot_key, phonetic_key, confidence, hit_count
             FROM correction_memory",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::corrections::MemoryEntry {
                wrong_token: row.get(0)?,
                human_token: row.get(1)?,
                slot_key: row.get(2)?,
                phonetic_key: row.get(3)?,
                confidence: row.get(4)?,
                hit_count: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Return escalated segments ordered riskiest-first (lowest agreement_score).
    pub fn get_escalation_queue(&self, limit: usize) -> AppResult<Vec<SpeechSegment>> {
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS}
             FROM speech_segments
             WHERE escalated = 1
               AND (human_decision IS NULL OR human_decision = '')
             ORDER BY COALESCE(agreement_score, 0.5) ASC, id ASC
             LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![limit as i64], Self::map_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// True only when an error indicates the database FILE itself is corrupt / not a database — the only
/// conditions under which the destructive `recover_database_at` quarantine is warranted. Transient
/// errors (SQLITE_BUSY/LOCKED, disk I/O, OOM) return false so a healthy db is never quarantined.
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

fn recover_database_at(path: &str) -> AppResult<()> {
    let path_buf = Path::new(path);
    if !path_buf.exists() {
        return Ok(());
    }

    let backup_path = unique_corrupt_backup_path(path_buf, chrono::Utc::now().timestamp());
    std::fs::rename(path_buf, &backup_path)?;
    tracing::info!("Corrupt database moved to {:?}", backup_path);

    move_sqlite_sidecar(path_buf, &backup_path, "-wal");
    move_sqlite_sidecar(path_buf, &backup_path, "-shm");
    Ok(())
}

fn unique_corrupt_backup_path(db_path: &Path, timestamp: i64) -> PathBuf {
    let base = db_path.with_extension(format!("corrupt.{timestamp}"));
    if !base.exists() {
        return base;
    }

    for suffix in 1..1000 {
        let candidate = db_path.with_extension(format!("corrupt.{timestamp}.{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }

    db_path.with_extension(format!("corrupt.{timestamp}.{}", std::process::id()))
}

fn move_sqlite_sidecar(original_db: &Path, backup_db: &Path, suffix: &str) {
    let original = sqlite_sidecar_path(original_db, suffix);
    if !original.exists() {
        return;
    }

    let backup = sqlite_sidecar_path(backup_db, suffix);
    if let Err(e) = std::fs::rename(&original, &backup) {
        tracing::warn!("Failed to quarantine SQLite sidecar {} to {}: {e}", original.display(), backup.display());
    }
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", db_path.display(), suffix))
}

#[cfg(test)]
#[path = "db_tests.rs"]
mod tests;
