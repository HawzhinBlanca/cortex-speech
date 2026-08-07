pub mod calibration;
pub mod conformal;
pub mod irt;
pub mod signal_anomaly;

use crate::chunking;
use crate::db::{Database, SegmentHypothesis, SpeechSegment};
use crate::error::AppResult;
use crate::settings::AppSettings;
use crate::wer;
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Minimum average word alignment confidence below which a segment is flagged.
pub const LOW_CONFIDENCE_THRESHOLD: f64 = 0.6;
pub const TRAINING_GRADE_GOLD: &str = "gold";
pub const TRAINING_GRADE_SILVER: &str = "silver";
pub const TRAINING_GRADE_REVIEW: &str = "review";
pub const TRAINING_GRADE_REJECT: &str = "reject";
pub const MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE: usize = 2;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrainingGradeReport {
    pub transcript: String,
    pub transcript_source: String,
    pub grade: String,
    pub training_ready: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrainingGradeSummary {
    pub total_segments: usize,
    pub training_ready_segments: usize,
    pub gold_segments: usize,
    pub silver_segments: usize,
    pub review_segments: usize,
    pub rejected_segments: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrainingGradeBreakdown {
    pub summary: TrainingGradeSummary,
    /// How many segments carry each grade reason, e.g. `energy_heuristic_alignment -> 144`.
    pub reason_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DatasetQuality {
    pub total_segments: usize,
    pub empty_transcript_count: usize,
    pub low_confidence_count: usize,
    pub duplicate_transcript_groups: usize,
    pub duplicate_transcript_segments: usize,
    pub duration_outlier_count: usize,
    pub median_duration_ms: i64,
    pub q1_duration_ms: i64,
    pub q3_duration_ms: i64,
    pub duplicate_groups: Vec<DuplicateGroup>,
    pub duration_outliers: Vec<DurationOutlier>,
    pub annotated_segment_count: usize,
    pub mean_wer: Option<f64>,
    pub mean_cer: Option<f64>,
    pub segments_above_wer_threshold: usize,
    pub segments_above_cer_threshold: usize,
    pub quality_gate_passed: bool,
    pub wer_outliers: Vec<WerOutlier>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WerOutlier {
    pub segment_id: String,
    pub wer: f64,
    pub cer: f64,
    pub reference_preview: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    pub transcript_hash: String,
    pub segment_ids: Vec<String>,
    pub normalized_preview: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DurationOutlier {
    pub segment_id: String,
    pub duration_ms: i64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HypothesisCoverageReport {
    pub minimum_non_empty_model_count: usize,
    pub non_empty_model_count: usize,
    pub passes_minimum: bool,
    pub non_empty_models: Vec<String>,
    pub ignored_models: Vec<String>,
}

pub fn hypothesis_coverage_for_model_outputs(hypotheses: &[SegmentHypothesis]) -> HypothesisCoverageReport {
    let mut non_empty_models = std::collections::BTreeSet::new();
    let mut ignored_models = std::collections::BTreeSet::new();

    for hypothesis in hypotheses {
        let model_id = hypothesis.model_id.trim();
        if model_id.is_empty() {
            continue;
        }
        // An EMPTY/whitespace transcript is not real model output — a CTC voter that decoded blank on a
        // near-silent clip stores "" (parse_wsl_segment_result treats an empty result as a legitimate
        // silent-clip outcome). It must NOT count toward `non_empty_models`: is_placeholder_transcript("")
        // is false (it only matches the [ASR…/[Pending/n/a/null sentinels), so without this guard a blank
        // hypothesis inflated the displayed nonEmptyModelCount AND defeated the >=2-real-model corroboration
        // gate that lets a SILVER machine row into the training export — passing "3 models corroborate" when
        // only one produced text. Ignore it, like a placeholder.
        if model_id.eq_ignore_ascii_case("asr")
            || hypothesis.transcript.trim().is_empty()
            || is_placeholder_transcript(&hypothesis.transcript)
        {
            ignored_models.insert(model_id.to_string());
        } else {
            non_empty_models.insert(model_id.to_string());
        }
    }

    let non_empty_models = non_empty_models.into_iter().collect::<Vec<_>>();
    let non_empty_model_count = non_empty_models.len();
    HypothesisCoverageReport {
        minimum_non_empty_model_count: MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE,
        non_empty_model_count,
        passes_minimum: non_empty_model_count >= MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE,
        non_empty_models,
        ignored_models: ignored_models.into_iter().collect(),
    }
}

pub fn compute_quality(db: &Database) -> AppResult<DatasetQuality> {
    let segments = db.get_segments(None)?;
    Ok(compute_quality_from_segments(&segments))
}

pub fn compute_quality_with_settings(db: &Database, settings: &AppSettings) -> AppResult<DatasetQuality> {
    let segments = db.get_segments(None)?;
    Ok(compute_quality_from_segments_with_settings(&segments, settings))
}

pub fn compute_quality_from_segments(segments: &[SpeechSegment]) -> DatasetQuality {
    let default_settings = AppSettings::default();
    compute_quality_from_segments_with_settings(segments, &default_settings)
}

pub fn compute_quality_from_segments_with_settings(
    segments: &[SpeechSegment],
    settings: &AppSettings,
) -> DatasetQuality {
    let total = segments.len();

    if total == 0 {
        return DatasetQuality::default();
    }

    let empty_transcript_count = segments.iter().filter(|s| effective_transcript(s).trim().is_empty()).count();

    let low_confidence_count = segments.iter().filter(|s| is_low_confidence(s)).count();

    let (duplicate_transcript_groups, duplicate_transcript_segments, duplicate_groups) =
        find_duplicate_transcripts(segments);

    let durations: Vec<i64> = segments.iter().map(|s| s.duration_ms).collect();
    let (median_duration_ms, q1_duration_ms, q3_duration_ms) = quartiles(&durations);
    let duration_outliers = find_duration_outliers(segments, q1_duration_ms, q3_duration_ms);
    let duration_outlier_count = duration_outliers.len();

    let (
        annotated_segment_count,
        mean_wer,
        mean_cer,
        segments_above_wer_threshold,
        segments_above_cer_threshold,
        wer_outliers,
    ) = compute_wer_cer_metrics(segments, settings);

    let quality_gate_passed = check_quality_gates(
        segments_above_wer_threshold,
        segments_above_cer_threshold,
        empty_transcript_count,
        low_confidence_count,
        settings,
    );

    DatasetQuality {
        total_segments: total,
        empty_transcript_count,
        low_confidence_count,
        duplicate_transcript_groups,
        duplicate_transcript_segments,
        duration_outlier_count,
        median_duration_ms,
        q1_duration_ms,
        q3_duration_ms,
        duplicate_groups,
        duration_outliers,
        annotated_segment_count,
        mean_wer,
        mean_cer,
        segments_above_wer_threshold,
        segments_above_cer_threshold,
        quality_gate_passed,
        wer_outliers,
    }
}

pub fn training_grade_summary(segments: &[SpeechSegment]) -> TrainingGradeSummary {
    training_grade_breakdown(segments).summary
}

/// The grade counts PLUS why each clip landed where it did.
///
/// A bare "0 training-ready" is a dead end: it tells the owner nothing is exportable without saying
/// what to fix. The reason tally turns that into an actionable blocker — on a library with no word
/// aligner, `energy_heuristic_alignment` dominates and names the real cause, which is a missing model
/// rather than anything the reviewer could fix by reviewing harder.
///
/// Computed with the SAME [`training_grade_for_segment`] the export gates on, in ONE pass that also
/// produces the summary, so a readiness shown in the UI can never disagree with what an export would
/// actually write. [`training_grade_summary`] delegates here rather than repeating the match — two
/// copies of that bucketing is exactly how a count starts lying.
/// Running tally behind [`training_grade_breakdown`], so the corpus-wide breakdown can be folded from a
/// STREAM instead of a materialised `Vec<SpeechSegment>` (P1.3).
///
/// The grading itself is untouched and still goes through `training_grade_for_segment` — the same
/// function the export gates on. Only the row's lifetime shrinks: it now lives for one `push` instead of
/// for the whole corpus. State is O(distinct reasons), not O(segments).
#[derive(Debug, Default)]
pub struct TrainingGradeTally {
    summary: TrainingGradeSummary,
    reason_counts: BTreeMap<String, usize>,
}

impl TrainingGradeTally {
    pub fn push(&mut self, seg: &SpeechSegment) {
        self.summary.total_segments += 1;
        let report = training_grade_for_segment(seg);
        if report.training_ready {
            self.summary.training_ready_segments += 1;
        }
        match report.grade.as_str() {
            TRAINING_GRADE_GOLD => self.summary.gold_segments += 1,
            TRAINING_GRADE_SILVER => self.summary.silver_segments += 1,
            TRAINING_GRADE_REVIEW => self.summary.review_segments += 1,
            TRAINING_GRADE_REJECT => self.summary.rejected_segments += 1,
            _ => self.summary.review_segments += 1,
        }
        for reason in report.reasons {
            *self.reason_counts.entry(reason).or_insert(0) += 1;
        }
    }

    pub fn finish(self) -> TrainingGradeBreakdown {
        TrainingGradeBreakdown { summary: self.summary, reason_counts: self.reason_counts }
    }
}

pub fn training_grade_breakdown(segments: &[SpeechSegment]) -> TrainingGradeBreakdown {
    let mut tally = TrainingGradeTally::default();
    for seg in segments {
        tally.push(seg);
    }
    tally.finish()
}

/// True when a human explicitly REJECTED this segment's draft — the review "mark bad" action, or a
/// jury/import `human_reject` verdict. Such a clip is kept in the library (reversible: re-review to
/// accept, or re-transcribe) but is bad data the reviewer discarded: it must never appear in an
/// exported dataset and must never be counted as a "verified" (confirmed-good) segment, even though
/// its `verified` flag is set true to finalize it out of the review queue. The training/HuggingFace
/// path already drops it via [`training_grade_for_segment`]; this shared predicate lets the plain
/// JSON/JSONL/CSV/Parquet exports and quality counts honor a reject exactly the same way.
pub fn is_human_rejected(seg: &SpeechSegment) -> bool {
    decision_is(seg.human_decision.as_deref(), &["reject", "human_reject"])
        || decision_is(seg.verdict.as_deref(), &["human_reject"])
}

pub fn training_grade_for_segment(seg: &SpeechSegment) -> TrainingGradeReport {
    let (transcript, source) = training_transcript_with_source(seg);
    let text = transcript.trim();
    let mut reasons = Vec::new();

    let human_rejected = is_human_rejected(seg);

    if human_rejected {
        reasons.push("human_rejected".to_string());
    }
    if text.is_empty() {
        reasons.push("blank_transcript".to_string());
    }
    if is_placeholder_transcript(text) {
        reasons.push("placeholder_transcript".to_string());
    }

    let severe_audio_issue = add_audio_quality_reasons(seg, &mut reasons);
    let low_confidence_alignment = is_low_confidence(seg);
    if low_confidence_alignment {
        reasons.push("low_confidence_alignment".to_string());
    }
    if seg.alignment_json.is_some() && seg.alignment_quality.as_deref() == Some("energy_heuristic") {
        reasons.push("energy_heuristic_alignment".to_string());
    }

    if human_rejected || text.is_empty() || is_placeholder_transcript(text) || severe_audio_issue {
        return TrainingGradeReport {
            transcript: text.to_string(),
            transcript_source: source.to_string(),
            grade: TRAINING_GRADE_REJECT.to_string(),
            training_ready: false,
            reasons,
        };
    }

    let has_review_risk = reasons.iter().any(|reason| {
        matches!(
            reason.as_str(),
            "clipping_warning"
                | "low_rms_volume"
                | "low_snr"
                | "low_confidence_alignment"
                | "energy_heuristic_alignment"
        )
    });

    let human_verified = seg.verified
        || seg.is_gold
        || decision_is(seg.human_decision.as_deref(), &["accept", "edit", "human_accept", "human_edit"])
        || decision_is(seg.verdict.as_deref(), &["human_accept", "human_edit"]);

    if human_verified {
        reasons.push("human_verified".to_string());
        let grade = if has_review_risk { TRAINING_GRADE_REVIEW } else { TRAINING_GRADE_GOLD };
        return TrainingGradeReport {
            transcript: text.to_string(),
            transcript_source: source.to_string(),
            grade: grade.to_string(),
            training_ready: grade == TRAINING_GRADE_GOLD,
            reasons,
        };
    }

    let jury_accepted = decision_is(seg.verdict.as_deref(), &["auto_accept", "jury_accept", "jury_edit"]);
    // AUDIT 2026-08-05. `seg.confidence` is evidence ONLY when it came from real model posteriors.
    // The shipped offline engine (OmniASR CTC via sherpa-onnx) exposes no token log-probs — its result
    // JSON always carries `ys_log_probs: []` — so asr::confidence_from_asr_result stamps a CONSTANT
    // 0.90 on every non-empty transcript and labels it `heuristic`. Feeding that constant into the
    // `>= 0.85` promotion test below made the clause VACUOUSLY TRUE for every clip on the default
    // path: a gate that reads as if it discriminates and filters nothing.
    //
    // Fails closed on `None`/legacy `unknown` too. A confidence whose provenance was never recorded
    // is not evidence that it was calibrated, and this decision promotes a clip into a training set
    // with no human ever having read it.
    //
    // `agreement_score` is untouched — that is a jury/agent judgement, already range-guarded in
    // jury::t2_listener, and it is the signal this branch was actually written for.
    let engine_confidence = match seg.confidence_source.as_deref() {
        Some(src) if src == crate::asr::ConfidenceSource::RealPosterior.as_db_value() => seg.confidence,
        _ => None,
    };
    let confidence = seg.agreement_score.or(engine_confidence).unwrap_or(0.0);
    let has_multi_agent_evidence = has_training_ready_machine_evidence(seg, text);
    if jury_accepted && confidence >= 0.85 && !has_review_risk && has_multi_agent_evidence {
        reasons.push("high_confidence_jury_accept".to_string());
        reasons.push("multi_agent_evidence_verified".to_string());
        return TrainingGradeReport {
            transcript: text.to_string(),
            transcript_source: source.to_string(),
            grade: TRAINING_GRADE_SILVER.to_string(),
            training_ready: true,
            reasons,
        };
    }

    if jury_accepted {
        reasons.push("jury_accept_needs_review".to_string());
        if confidence >= 0.85 && !has_review_risk && !has_multi_agent_evidence {
            reasons.push("missing_multi_agent_evidence".to_string());
        }
    } else {
        reasons.push("not_human_or_high_confidence_agent_verified".to_string());
    }

    TrainingGradeReport {
        transcript: text.to_string(),
        transcript_source: source.to_string(),
        grade: TRAINING_GRADE_REVIEW.to_string(),
        training_ready: false,
        reasons,
    }
}

/// The transcript for this segment ONLY when a human actually verified it — otherwise `None`.
///
/// Thin, deliberate wrapper over [`training_transcript_with_source`] so callers that need
/// known-correct text cannot accidentally settle for a machine verdict. Used by the Couch Review
/// spot-check (docs/REMOTE_REVIEW_PLAN.md §2.1), where grading a reviewer against a jury guess
/// instead of a human answer would measure agreement with the model, not with the truth.
pub fn human_verified_text(seg: &SpeechSegment) -> Option<&str> {
    match training_transcript_with_source(seg) {
        (text, "human_verified") => Some(text),
        _ => None,
    }
}

fn training_transcript_with_source(seg: &SpeechSegment) -> (&str, &'static str) {
    if let Some(text) = non_empty(seg.verdict_transcript.as_deref()) {
        if decision_is(seg.human_decision.as_deref(), &["accept", "edit", "human_accept", "human_edit"])
            || decision_is(seg.verdict.as_deref(), &["human_accept", "human_edit"])
        {
            return (text, "human_verified");
        }
    }

    if let Some(text) = non_empty(seg.annotated_transcript.as_deref()) {
        let source = if seg.verified || seg.is_gold { "human_verified" } else { "annotated" };
        return (text, source);
    }

    if let Some(text) = non_empty(seg.verdict_transcript.as_deref()) {
        let source = if decision_is(seg.verdict.as_deref(), &["auto_accept", "jury_accept", "jury_edit"]) {
            "jury_verdict"
        } else {
            "verdict"
        };
        return (text, source);
    }

    if let Some(text) = non_empty(seg.normalized_transcript.as_deref()) {
        return (text, "normalized_asr");
    }

    (&seg.raw_transcript, "raw_asr")
}

fn non_empty(text: Option<&str>) -> Option<&str> {
    text.filter(|value| !value.trim().is_empty())
}

/// The best available human-facing transcript for a segment (verdict → annotated → jury verdict →
/// normalized → raw ASR). Shared by training-grade logic and the plain transcript/subtitle export.
pub fn effective_transcript(seg: &SpeechSegment) -> &str {
    training_transcript_with_source(seg).0
}

fn decision_is(value: Option<&str>, accepted: &[&str]) -> bool {
    let Some(value) = value else {
        return false;
    };
    accepted.iter().any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn has_training_ready_machine_evidence(seg: &SpeechSegment, transcript: &str) -> bool {
    let Some(evidence) = seg.evidence_json.as_deref().filter(|value| !value.trim().is_empty()) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence) else {
        return false;
    };

    has_source_reference_commit_evidence_for_transcript(&value, transcript)
        || has_t2_listener_evidence_for_transcript(&value, transcript)
}

pub(crate) fn has_source_reference_commit_evidence(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let direct_commit = map.get("shouldCommit").and_then(serde_json::Value::as_bool).unwrap_or(false)
                && map
                    .get("referenceModelId")
                    .and_then(serde_json::Value::as_str)
                    .map(|model| !model.trim().is_empty())
                    .unwrap_or(false);
            direct_commit || map.get("referenceSelection").map(has_source_reference_commit_evidence).unwrap_or(false)
        }
        serde_json::Value::Array(values) => values.iter().any(has_source_reference_commit_evidence),
        _ => false,
    }
}

fn has_source_reference_commit_evidence_for_transcript(value: &serde_json::Value, transcript: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let direct_commit = map.get("shouldCommit").and_then(serde_json::Value::as_bool).unwrap_or(false)
                && map
                    .get("referenceModelId")
                    .and_then(serde_json::Value::as_str)
                    .map(|model| !model.trim().is_empty())
                    .unwrap_or(false)
                && evidence_transcript_matches(map.get("selectedTranscript"), transcript);
            direct_commit
                || map
                    .get("referenceSelection")
                    .map(|selection| has_source_reference_commit_evidence_for_transcript(selection, transcript))
                    .unwrap_or(false)
        }
        serde_json::Value::Array(values) => {
            values.iter().any(|value| has_source_reference_commit_evidence_for_transcript(value, transcript))
        }
        _ => false,
    }
}

fn has_t2_listener_evidence_for_transcript(value: &serde_json::Value, transcript: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let direct_t2 =
                map.contains_key("t2Evidence") && evidence_transcript_matches(map.get("t2Transcript"), transcript);
            direct_t2 || map.values().any(|value| has_t2_listener_evidence_for_transcript(value, transcript))
        }
        serde_json::Value::Array(values) => {
            values.iter().any(|value| has_t2_listener_evidence_for_transcript(value, transcript))
        }
        _ => false,
    }
}

fn evidence_transcript_matches(candidate: Option<&serde_json::Value>, transcript: &str) -> bool {
    let Some(candidate) = candidate.and_then(serde_json::Value::as_str) else {
        return false;
    };
    let expected = wer::normalize_for_metrics(transcript);
    let actual = wer::normalize_for_metrics(candidate);
    !expected.is_empty() && expected == actual
}

/// THE definition of "this clip's audio is too poor to trust a recognizer on".
///
/// External review 2026-08-06, P1.2. These two numbers were written out by hand in three independent
/// places — `jury::has_hard_distrust_veto` (the authority that vetoes auto-accept), the suspect-first
/// SQL ordering in `db.rs`, and `hasPoorAudio()` in `ReviewInbox.svelte` — each carrying a comment
/// saying it was "kept identical on purpose". Three hand-synced copies of a rule is three chances to
/// drift, and the drift is not cosmetic: if the UI's copy and the jury's copy disagree, the review
/// screen shows a reassuring green agreement badge on precisely the clip the gate refused to trust.
///
/// The two Rust sites now share these constants. The TypeScript copy cannot import them, so
/// `test_frontend_review_guards.py::test_poor_audio_thresholds_match_the_rust_authority` parses BOTH
/// files and fails if the numbers diverge — the copy stays, but it can no longer drift silently.
///
/// Changing a value here changes the jury veto, the review queue order and the UI band together, which
/// is the entire point.
pub const POOR_AUDIO_SNR_DB: f64 = 5.0;
pub const POOR_AUDIO_CLIPPING_RATIO: f64 = 0.1;

/// True when a clip's measured audio quality is poor by [`POOR_AUDIO_SNR_DB`] / [`POOR_AUDIO_CLIPPING_RATIO`].
///
/// `None` is NOT poor: an unmeasured clip is unknown, and absence of a measurement must never be
/// reported as evidence of bad audio. That matches what the SQL does with its COALESCE defaults
/// (99.0 dB SNR, 0.0 clipping — both deliberately "fine" so an unmeasured row sorts as unremarkable).
pub fn has_poor_audio(snr_db: Option<f64>, clipping_ratio: Option<f64>) -> bool {
    snr_db.is_some_and(|snr| snr < POOR_AUDIO_SNR_DB)
        || clipping_ratio.is_some_and(|clip| clip > POOR_AUDIO_CLIPPING_RATIO)
}

/// True when a stored "transcript" is actually a failure/placeholder marker (ASR error,
/// pending status, or an explicit n/a), not real speech content. Used to keep such markers
/// out of training/export and out of the transcript cache.
pub fn is_placeholder_transcript(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("[ASR unavailable")
        || trimmed.starts_with("[Pending")
        || trimmed.eq_ignore_ascii_case("n/a")
        || trimmed.eq_ignore_ascii_case("null")
}

/// True when a segment's EFFECTIVE (human-preferred) transcript is still an ASR placeholder — it carries
/// no real text and was not corrected by a human. Such rows must never ship in a dataset export (an
/// export taken mid-import, or after a stuck-placeholder incident, would otherwise emit the literal
/// "[Pending WSL 7B ASR]" string as a training row).
pub fn is_effective_placeholder(seg: &SpeechSegment) -> bool {
    is_placeholder_transcript(effective_transcript(seg))
}

/// Alignment-quality tier for a clip, decided by the character error rate (CER) between its
/// reference text and an ASR hypothesis of that same clip. This is the gate that both cleans
/// a dataset and yields an honest CER — once a clip's audio and text genuinely match, a low
/// CER means a good pair, a high CER means the cut drifted (or the speech is hard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipTier {
    /// CER ≤ 0.05 — clean, training-grade pair.
    Gold,
    /// 0.05 < CER ≤ 0.20 — usable but worth a human glance.
    Review,
    /// CER > 0.20 — likely misaligned or unusable; keep out of the clean set.
    Reject,
}

/// Thresholds follow low-resource TTS/ASR dataset practice (e.g. ManaTTS): ≤0.05 gold,
/// ≤0.20 review, else reject. Returns the tier plus the measured CER. CER uses the shared
/// `wer` module so reference and hypothesis are normalised + NFC-folded identically.
pub fn clip_cer_tier(reference: &str, hypothesis: &str) -> (ClipTier, f64) {
    let d = crate::wer::char_edit_distance(reference, hypothesis);
    let cer = if d.ref_len > 0 { (d.distance as f64 / d.ref_len as f64).min(1.0) } else { 0.0 };
    let tier = if cer <= 0.05 {
        ClipTier::Gold
    } else if cer <= 0.20 {
        ClipTier::Review
    } else {
        ClipTier::Reject
    };
    (tier, cer)
}

fn add_audio_quality_reasons(seg: &SpeechSegment, reasons: &mut Vec<String>) -> bool {
    let mut severe = false;
    if let Some(clipping) = seg.clipping_ratio {
        if clipping > 0.1 {
            reasons.push("severe_clipping".to_string());
            severe = true;
        } else if clipping > 0.01 {
            reasons.push("clipping_warning".to_string());
        }
    }
    if let Some(rms) = seg.rms_db {
        if rms < -45.0 {
            reasons.push("near_silence".to_string());
            severe = true;
        } else if rms < -40.0 {
            reasons.push("low_rms_volume".to_string());
        }
    }
    if let Some(snr) = seg.snr_db {
        if snr < 5.0 {
            reasons.push("severe_low_snr".to_string());
            severe = true;
        } else if snr < 10.0 {
            reasons.push("low_snr".to_string());
        }
    }
    severe
}

/// `pub(crate)`: the ValidationPanel's WER/CER gate (validation/mod.rs) must score the SAME
/// hypothesis as this quality gate — it previously scored normalized_transcript, so a perfect
/// transcript with one-way verbalized numbers passed here (0.0) yet raised a HighWer Error there,
/// blocking exports on a false positive.
pub(crate) fn hypothesis_transcript(seg: &SpeechSegment) -> &str {
    // Score the RAW ASR output, NOT normalized_transcript. wer::compute_wer/compute_cer apply
    // normalize_for_metrics (verbalize_numbers:false) to BOTH the reference and the hypothesis, so
    // both must enter in the same surface form. The stored normalized_transcript may have been
    // number-verbalized one-way (e.g. "١٤" -> "چواردە") per the user's verbalize_numbers setting —
    // a transform normalize_for_metrics cannot reverse — which would inflate WER/CER against the
    // digit-form human reference even on a perfect transcript. The raw transcript keeps digits,
    // matching the reference, so both sides canonicalize symmetrically.
    &seg.raw_transcript
}

fn compute_wer_cer_metrics(
    segments: &[SpeechSegment],
    settings: &AppSettings,
) -> (usize, Option<f64>, Option<f64>, usize, usize, Vec<WerOutlier>) {
    let mut wer_sum = 0.0f64;
    let mut cer_sum = 0.0f64;
    let mut annotated_count = 0usize;
    let mut above_wer = 0usize;
    let mut above_cer = 0usize;
    let mut outliers = Vec::new();

    for seg in segments {
        let Some(reference) = seg.annotated_transcript.as_deref().filter(|t| !t.trim().is_empty()) else {
            continue;
        };

        // A REJECTED clip is not evidence about ASR quality. `record_human_decision` leaves the
        // transcripts populated on a reject, so a clip the reviewer discarded still satisfies the
        // annotated-non-empty test above and folds its error into the headline mean WER/CER — over a row
        // `export_dataset` drops. `is_human_rejected`'s own docstring says this predicate exists so "the
        // plain JSON/JSONL/CSV/Parquet exports AND QUALITY COUNTS honor a reject exactly the same way";
        // this count was the one that did not. (Measured on the live library 2026-08-03: 27 rejects, 0
        // of them currently carrying an annotation — latent, not yet firing, and one reject-after-edit
        // away from firing.)
        if is_human_rejected(seg) {
            continue;
        }

        // A placeholder HYPOTHESIS means the ASR never produced output for this clip. Scoring it
        // against a real reference yields ~100% error, which reads as "the engine was completely
        // wrong" when the truth is "the engine did not run". That is a fabricated measurement, and
        // an inflated mean is exactly the direction nobody double-checks.
        if is_placeholder_transcript(hypothesis_transcript(seg)) {
            continue;
        }

        annotated_count += 1;
        let hypothesis = hypothesis_transcript(seg);
        let wer = wer::compute_wer(reference, hypothesis);
        let cer = wer::compute_cer(reference, hypothesis);
        wer_sum += wer;
        cer_sum += cer;

        if wer > settings.max_wer_threshold {
            above_wer += 1;
        }
        if cer > settings.max_cer_threshold {
            above_cer += 1;
        }

        if wer > settings.max_wer_threshold || cer > settings.max_cer_threshold {
            let preview = wer::normalize_for_metrics(reference);
            let preview = if preview.chars().count() > 60 {
                format!("{}…", preview.chars().take(60).collect::<String>())
            } else {
                preview
            };
            outliers.push(WerOutlier { segment_id: seg.id.clone(), wer, cer, reference_preview: preview });
        }
    }

    outliers.sort_by(|a, b| b.wer.partial_cmp(&a.wer).unwrap_or(std::cmp::Ordering::Equal));
    outliers.truncate(25);

    let mean_wer = if annotated_count > 0 { Some(wer_sum / annotated_count as f64) } else { None };
    let mean_cer = if annotated_count > 0 { Some(cer_sum / annotated_count as f64) } else { None };

    (annotated_count, mean_wer, mean_cer, above_wer, above_cer, outliers)
}

pub fn check_quality_gates(
    segments_above_wer: usize,
    segments_above_cer: usize,
    empty_transcript_count: usize,
    low_confidence_count: usize,
    settings: &AppSettings,
) -> bool {
    if !settings.enforce_quality_gates {
        return true;
    }
    segments_above_wer == 0 && segments_above_cer == 0 && empty_transcript_count == 0 && low_confidence_count == 0
}

/// Normalize transcript text for duplicate detection (case/whitespace insensitive).
pub fn normalize_transcript_for_hash(text: &str) -> String {
    // Unify Sorani codepoint variants (Kaf/Yeh/Heh, ZWNJ, tatweel, digit systems)
    // through the canonical normalizer first, so two transcripts that are visually
    // identical but differ only by codepoint hash to the same value and dedup
    // correctly instead of fragmenting. Then lowercase any Latin content and
    // collapse whitespace, preserving the previous behaviour for non-Sorani text.
    let canonical = crate::normalizer::SoraniNormalizer::new().normalize(text);
    canonical.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn transcript_hash(text: &str) -> String {
    let normalized = normalize_transcript_for_hash(text);
    let mut hasher = Hasher::new();
    hasher.update(normalized.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn is_low_confidence(seg: &SpeechSegment) -> bool {
    // The placeholder shortcut flags a failed/pending raw ASR ("[Pending WSL 7B ASR]",
    // "[ASR unavailable: …]") as low-confidence. But the grade is computed from the EFFECTIVE
    // transcript, which for a human edit is verdict_transcript — the authoritative hand-typed text. If
    // we keyed off raw_transcript here, a curator who manually re-transcribed a clip whose raw ASR
    // failed would push "low_confidence_alignment", turning has_review_risk true and demoting an
    // otherwise-GOLD human-verified clip to REVIEW — silently dropping it from HF export and the
    // promotion-gate training_ready count. Keying off the effective transcript means the shortcut only
    // fires when the text actually being graded is itself a placeholder (which the REJECT branch above
    // already catches), so a hand-corrected segment over a failed raw ASR can still grade GOLD.
    if is_placeholder_transcript(effective_transcript(seg)) {
        return true;
    }

    let Some(alignment) = seg.alignment_json.as_deref() else {
        return false;
    };

    let Some(words) = chunking::word_timestamps_from_alignment(alignment) else {
        return false;
    };

    if words.is_empty() {
        return false;
    }

    let sum: f64 = words.iter().map(|w| w.confidence).sum();
    let avg = sum / words.len() as f64;
    avg < LOW_CONFIDENCE_THRESHOLD || words.iter().any(|w| w.confidence < LOW_CONFIDENCE_THRESHOLD * 0.5)
}

fn find_duplicate_transcripts(segments: &[SpeechSegment]) -> (usize, usize, Vec<DuplicateGroup>) {
    let mut by_hash: HashMap<String, Vec<&SpeechSegment>> = HashMap::new();

    for seg in segments {
        // Same membership rule as every other quality tally: this is a signal about the dataset that
        // will SHIP, so rows `export_dataset` drops cannot raise an alarm about it.
        if is_human_rejected(seg) {
            continue;
        }
        let text = effective_transcript(seg);
        if text.trim().is_empty() {
            continue;
        }
        // Placeholders are not duplicate transcripts, they are ABSENT ones — and they are absent in
        // exactly the same words. Every clip stalled at "[Pending WSL 7B ASR]" carries that identical
        // string, so a stuck import (an incident `is_effective_placeholder`'s own docstring records as
        // having happened) would hash them into one enormous "duplicate transcripts" group and send
        // the owner hunting a data problem that does not exist. The real problem in that case is the
        // stall, which the placeholder counters already report.
        if is_placeholder_transcript(text) {
            continue;
        }
        let hash = transcript_hash(text);
        by_hash.entry(hash).or_default().push(seg);
    }

    let mut duplicate_groups = Vec::new();
    let mut duplicate_segment_count = 0usize;

    for (hash, group) in by_hash {
        if group.len() < 2 {
            continue;
        }
        duplicate_segment_count += group.len();
        let preview = normalize_transcript_for_hash(effective_transcript(group[0]));
        let preview = if preview.chars().count() > 80 {
            format!("{}…", preview.chars().take(80).collect::<String>())
        } else {
            preview
        };
        duplicate_groups.push(DuplicateGroup {
            transcript_hash: hash,
            segment_ids: group.iter().map(|s| s.id.clone()).collect(),
            normalized_preview: preview,
        });
    }

    duplicate_groups.sort_by_key(|g| std::cmp::Reverse(g.segment_ids.len()));

    let group_count = duplicate_groups.len();
    (group_count, duplicate_segment_count, duplicate_groups)
}

fn quartiles(values: &[i64]) -> (i64, i64, i64) {
    if values.is_empty() {
        return (0, 0, 0);
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();

    let median = percentile(&sorted, 0.5);
    let q1 = percentile(&sorted, 0.25);
    let q3 = percentile(&sorted, 0.75);
    (median, q1, q3)
}

fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn find_duration_outliers(segments: &[SpeechSegment], q1: i64, q3: i64) -> Vec<DurationOutlier> {
    if segments.len() < 4 {
        return Vec::new();
    }

    let iqr = q3 - q1;
    if iqr <= 0 {
        return Vec::new();
    }

    let lower = q1 - (iqr as f64 * 1.5) as i64;
    let upper = q3 + (iqr as f64 * 1.5) as i64;

    let mut outliers: Vec<DurationOutlier> = segments
        .iter()
        .filter_map(|seg| {
            if seg.duration_ms < lower {
                Some(DurationOutlier {
                    segment_id: seg.id.clone(),
                    duration_ms: seg.duration_ms,
                    reason: format!("below Q1-1.5×IQR ({lower}ms)"),
                })
            } else if seg.duration_ms > upper {
                Some(DurationOutlier {
                    segment_id: seg.id.clone(),
                    duration_ms: seg.duration_ms,
                    reason: format!("above Q3+1.5×IQR ({upper}ms)"),
                })
            } else {
                None
            }
        })
        .collect();

    outliers.sort_by_key(|o| o.duration_ms);
    outliers.truncate(50);
    outliers
}

impl Default for DatasetQuality {
    fn default() -> Self {
        Self {
            total_segments: 0,
            empty_transcript_count: 0,
            low_confidence_count: 0,
            duplicate_transcript_groups: 0,
            duplicate_transcript_segments: 0,
            duration_outlier_count: 0,
            median_duration_ms: 0,
            q1_duration_ms: 0,
            q3_duration_ms: 0,
            duplicate_groups: Vec::new(),
            duration_outliers: Vec::new(),
            annotated_segment_count: 0,
            mean_wer: None,
            mean_cer: None,
            segments_above_wer_threshold: 0,
            segments_above_cer_threshold: 0,
            quality_gate_passed: true,
            wer_outliers: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SpeechSegment;

    #[test]
    fn hypothesis_coverage_ignores_empty_transcripts_so_the_corroboration_gate_is_not_faked() {
        // A near-silent clip: two CTC voters decoded BLANK ("" / whitespace) and only one model produced
        // real text. An empty transcript is not real model output — it must be IGNORED, never counted as a
        // "non-empty model", or the displayed nonEmptyModelCount is fabricated AND the >=2-real-model
        // corroboration gate that lets a SILVER row into the training export passes on a single genuinely-
        // corroborating model (is_placeholder_transcript("") is false, so blanks slipped through).
        let hyp = |model: &str, text: &str| SegmentHypothesis {
            segment_id: "s1".to_string(),
            model_id: model.to_string(),
            transcript: text.to_string(),
            confidence: Some(0.9),
        };
        let report = hypothesis_coverage_for_model_outputs(&[
            hyp("omniasr-ctc-300m", ""),        // blank decode
            hyp("omniasr-ctc-1b", "   "),       // whitespace-only decode
            hyp("omniasr-wsl-7b", "دەقی ڕاست"), // the one real hypothesis
        ]);
        assert_eq!(
            report.non_empty_models,
            vec!["omniasr-wsl-7b".to_string()],
            "only the model with real text counts as non-empty"
        );
        assert_eq!(report.non_empty_model_count, 1);
        assert!(!report.passes_minimum, "one real model must NOT pass the >=2-model corroboration gate");
        assert!(
            report.ignored_models.contains(&"omniasr-ctc-300m".to_string())
                && report.ignored_models.contains(&"omniasr-ctc-1b".to_string()),
            "the blank-decode models are ignored, not counted: {:?}",
            report.ignored_models
        );
    }

    fn seg(id: &str, text: &str, duration_ms: i64) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: "/audio.wav".to_string(),
            raw_transcript: text.to_string(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms,
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
    fn metrics_score_raw_hypothesis_so_verbalized_numbers_dont_inflate_wer() {
        // Regression: a segment whose stored normalized_transcript was number-verbalized must still
        // score WER/CER 0 against a digit-form human reference — the dashboard/manifest metrics
        // score the RAW hypothesis and canonicalize both sides identically (verbalize_numbers:false),
        // so a perfect transcript reads as 0, not an inflated rate.
        let mut s = seg("num-1", "تەمەنی ١٤ ساڵ", 1500);
        s.annotated_transcript = Some("تەمەنی ١٤ ساڵ".to_string());
        // The one-way verbalized form must NOT be what gets scored.
        s.normalized_transcript = Some("تەمەنی یەک چوار ساڵ".to_string());
        let settings = AppSettings::default();
        let (n, wer, cer, above_wer, _above_cer, _outliers) =
            compute_wer_cer_metrics(std::slice::from_ref(&s), &settings);
        assert_eq!(n, 1, "the annotated segment is counted");
        assert_eq!(wer, Some(0.0), "verbalized normalized_transcript must not inflate WER");
        assert_eq!(cer, Some(0.0), "verbalized normalized_transcript must not inflate CER");
        assert_eq!(above_wer, 0, "a perfect transcript must not trip the WER threshold");
    }

    #[test]
    fn normalize_transcript_collapses_whitespace_and_case() {
        assert_eq!(normalize_transcript_for_hash("  Hello   WORLD  "), "hello world");
    }

    #[test]
    fn transcript_hash_unifies_sorani_codepoint_variants() {
        // The same Kurdish word written with Arabic Kaf (ك U+0643) + Arabic Yeh
        // (ي U+064A) versus Kurdish Keheh (ک U+06A9) + Kurdish Yeh (ی U+06CC). These
        // are visually identical and semantically one word; content-dedup must not
        // treat them as distinct, or duplicate Sorani transcripts fragment silently.
        let arabic_form = "كوردي"; // U+0643 ... U+064A
        let kurdish_form = "کوردی"; // U+06A9 ... U+06CC
        assert_ne!(arabic_form, kurdish_form, "sanity: the two forms are distinct codepoints");
        assert_eq!(
            transcript_hash(arabic_form),
            transcript_hash(kurdish_form),
            "dedup hash must unify Sorani Kaf/Yeh codepoint variants"
        );
    }

    #[test]
    fn placeholder_transcript_detection() {
        // Failure/pending markers must be recognised (kept out of cache + training/export).
        assert!(is_placeholder_transcript("[ASR unavailable: transcribe timed out]"));
        assert!(is_placeholder_transcript("[Pending WSL 7B ASR]"));
        assert!(is_placeholder_transcript("N/A"));
        assert!(is_placeholder_transcript("  null "));
        // Real Sorani content (and a plain empty string) must NOT be flagged as a placeholder.
        assert!(!is_placeholder_transcript("ئەمڕۆ هەوا زۆر خۆشە"));
        assert!(!is_placeholder_transcript(""));
        // A failed-transcript segment is graded reject / not training-ready (excluded from export).
        let report = training_grade_for_segment(&seg("p1", "[ASR unavailable: oom]", 4000));
        assert!(!report.training_ready);
        assert!(report.reasons.iter().any(|r| r == "placeholder_transcript"));
    }

    #[test]
    fn is_effective_placeholder_reflects_the_effective_transcript() {
        // Raw is a placeholder with no human correction -> effective transcript is a placeholder.
        assert!(is_effective_placeholder(&seg("p", "[Pending WSL 7B ASR]", 4000)));
        // A human edit OVER the placeholder makes the effective transcript real text.
        let mut edited = seg("e", "[Pending WSL 7B ASR]", 4000);
        edited.verdict = Some("human_edit".to_string());
        edited.verdict_transcript = Some("ئەمڕۆ هەوا زۆر خۆشە".to_string());
        edited.human_decision = Some("edit".to_string());
        assert!(!is_effective_placeholder(&edited), "a human-corrected placeholder is real text");
        // Real ASR text is never a placeholder.
        assert!(!is_effective_placeholder(&seg("r", "ئەمڕۆ باشە", 4000)));
    }

    #[test]
    fn mean_wer_cer_ignores_rows_the_export_drops() {
        // One good row: the human tidied a small ASR error. This is the only row that should count.
        let mut good = seg("good", "ئەمڕۆ هەوا خۆشە", 4000);
        good.annotated_transcript = Some("ئەمڕۆ هەوا زۆر خۆشە".to_string());

        // A REJECTED clip keeps its transcripts (record_human_decision leaves them populated), so it
        // still looks annotated. It is not evidence about ASR quality — the reviewer discarded it, and
        // export_dataset drops it.
        let mut rejected = seg("rej", "پۆلیس و هێزی ئەمنی هاتن", 4000);
        rejected.annotated_transcript = Some("ئەمڕۆ هەوا زۆر خۆشە".to_string());
        rejected.human_decision = Some("reject".to_string());

        // A PLACEHOLDER hypothesis means the engine never produced output. Scored, it reads as ~100%
        // error — the engine looking "completely wrong" when it simply did not run.
        let mut pending = seg("pend", "[Pending WSL 7B ASR]", 4000);
        pending.annotated_transcript = Some("ئەمڕۆ هەوا زۆر خۆشە".to_string());

        let only_good = compute_quality_from_segments(std::slice::from_ref(&good));
        let with_junk = compute_quality_from_segments(&[good, rejected, pending]);

        assert_eq!(only_good.annotated_segment_count, 1);
        assert_eq!(
            with_junk.annotated_segment_count, 1,
            "a rejected clip and a never-transcribed clip must not be counted as annotated evidence"
        );
        assert_eq!(
            with_junk.mean_cer, only_good.mean_cer,
            "the headline mean CER must not move when rows the export drops are added"
        );
        assert_eq!(with_junk.mean_wer, only_good.mean_wer, "same for mean WER");
    }

    #[test]
    fn duplicate_detection_ignores_rejects_and_never_groups_placeholders() {
        // Two genuinely identical shipping transcripts: a real duplicate, and the only one that counts.
        let a = seg("a", "ئەمڕۆ هەوا زۆر خۆشە", 4000);
        let b = seg("b", "ئەمڕۆ هەوا زۆر خۆشە", 4100);

        // Two clips stalled at the same placeholder. They are not duplicate transcripts, they are
        // absent ones — and a stuck import makes hundreds of them, all carrying this exact string.
        let p1 = seg("p1", "[Pending WSL 7B ASR]", 4000);
        let p2 = seg("p2", "[Pending WSL 7B ASR]", 4200);

        // Two rejected clips that happen to share text: dropped by export, so not a shipping problem.
        let mut r1 = seg("r1", "پۆلیس و هێزی ئەمنی هاتن", 4000);
        r1.human_decision = Some("reject".to_string());
        let mut r2 = seg("r2", "پۆلیس و هێزی ئەمنی هاتن", 4000);
        r2.human_decision = Some("reject".to_string());

        let (groups, members, _detail) = find_duplicate_transcripts(&[a, b, p1, p2, r1, r2]);
        assert_eq!(groups, 1, "only the real duplicate pair is a duplicate group");
        assert_eq!(members, 2, "placeholders and rejects must not be counted as duplicate segments");
    }

    #[test]
    fn clip_cer_tier_classifies_by_cer() {
        // Identical reference + hypothesis → gold (CER 0).
        assert_eq!(clip_cer_tier("ئەمڕۆ هەوا زۆر خۆشە", "ئەمڕۆ هەوا زۆر خۆشە").0, ClipTier::Gold);
        // Completely different content → reject (CER well above 0.20).
        let (tier, cer) = clip_cer_tier("ئەمڕۆ هەوا زۆر خۆشە", "پۆلیس و هێزی ئەمنی هاتن");
        assert_eq!(tier, ClipTier::Reject);
        assert!(cer > 0.20);
        // Empty hypothesis against a real reference → every char deleted → reject.
        assert_eq!(clip_cer_tier("ئەمڕۆ هەوا", "").0, ClipTier::Reject);
        // A single dropped char in a long line stays gold/review, never reject.
        let (tier, _) = clip_cer_tier(
            "ڕێکخراوی مافی مرۆڤی هەنگاو دەڵێت ئەمانەی کە لە سنە گیراون",
            "ڕێکخراوی مافی مرۆڤی هەنگاو دەڵێت ئەمانەی کە لە سنە گیران",
        );
        assert_ne!(tier, ClipTier::Reject);
    }

    #[test]
    fn duplicate_transcripts_detected_by_hash() {
        let segments =
            vec![seg("a", "Hello World", 5000), seg("b", "hello   world", 6000), seg("c", "Different text", 7000)];
        let q = compute_quality_from_segments(&segments);
        assert_eq!(q.duplicate_transcript_groups, 1);
        assert_eq!(q.duplicate_transcript_segments, 2);
    }

    #[test]
    fn empty_transcripts_counted() {
        let segments = vec![seg("a", "", 5000), seg("b", "  ", 6000), seg("c", "text", 7000)];
        let q = compute_quality_from_segments(&segments);
        assert_eq!(q.empty_transcript_count, 2);
    }

    #[test]
    fn low_confidence_from_alignment_words() {
        let words = serde_json::json!([
            {"word": "hello", "start": 0.0, "end": 0.5, "confidence": 0.3},
            {"word": "world", "start": 0.5, "end": 1.0, "confidence": 0.4},
        ]);
        let alignment = serde_json::json!({ "words": words }).to_string();
        let mut s = seg("a", "hello world", 5000);
        s.alignment_json = Some(alignment);
        let q = compute_quality_from_segments(&[s]);
        assert_eq!(q.low_confidence_count, 1);
    }

    #[test]
    fn placeholder_transcripts_count_as_low_confidence() {
        let segments = vec![
            seg("unavailable", "[ASR unavailable: model missing]", 5000),
            seg("pending", "[Pending WSL 7B ASR]", 5000),
        ];
        let q = compute_quality_from_segments(&segments);
        assert_eq!(q.low_confidence_count, 2);
    }

    #[test]
    fn pending_wsl_counts_as_low_confidence() {
        let q = compute_quality_from_segments(&[seg("pending-wsl", "[Pending WSL 7B ASR]", 5000)]);
        assert_eq!(q.low_confidence_count, 1);
    }

    #[test]
    fn training_grade_rejects_blank_and_placeholder_text() {
        let blank = training_grade_for_segment(&seg("blank", " ", 5000));
        assert_eq!(blank.grade, TRAINING_GRADE_REJECT);
        assert!(!blank.training_ready);
        assert!(blank.reasons.contains(&"blank_transcript".to_string()));

        let placeholder = training_grade_for_segment(&seg("missing", "[ASR unavailable: model missing]", 5000));
        assert_eq!(placeholder.grade, TRAINING_GRADE_REJECT);
        assert!(!placeholder.training_ready);
        assert!(placeholder.reasons.contains(&"placeholder_transcript".to_string()));

        let pending_wsl = training_grade_for_segment(&seg("pending-wsl", "[Pending WSL 7B ASR]", 5000));
        assert_eq!(pending_wsl.grade, TRAINING_GRADE_REJECT);
        assert!(!pending_wsl.training_ready);
        assert!(pending_wsl.reasons.contains(&"placeholder_transcript".to_string()));
    }

    /// The whole reason the Insights readiness verdict calls `training_grade_breakdown` instead of
    /// counting verified clips: on a library with no word aligner, EVERY clip can be human-verified
    /// and STILL export nothing. A dashboard that derived "ready" from the verified count would show
    /// a green, fully-reviewed library whose export writes zero rows — the "a tally counts rows the
    /// export drops" bug, inverted. This pins the disagreement, so a future refactor cannot quietly
    /// re-base readiness on `verified`.
    #[test]
    fn readiness_disagrees_with_the_verified_count_when_alignment_is_heuristic() {
        let verified_but_heuristic = |id: &str| {
            let mut s = seg(id, "دەقی ڕاست", 5000);
            s.verified = true;
            s.human_decision = Some("accept".to_string());
            s.clipping_ratio = Some(0.0);
            s.rms_db = Some(-20.0);
            s.snr_db = Some(25.0);
            // What a missing mms_aligner.onnx leaves behind: real timings, guessed from energy.
            s.alignment_json = Some("{\"words\":[]}".to_string());
            s.alignment_quality = Some("energy_heuristic".to_string());
            s
        };
        let segs = vec![verified_but_heuristic("a"), verified_but_heuristic("b")];

        let breakdown = training_grade_breakdown(&segs);

        assert_eq!(segs.iter().filter(|s| s.verified).count(), 2, "both clips ARE human-verified");
        assert_eq!(
            breakdown.summary.training_ready_segments, 0,
            "yet nothing is exportable — readiness must not be read off `verified`"
        );
        assert_eq!(breakdown.summary.review_segments, 2, "they grade REVIEW, not GOLD");
        assert_eq!(
            breakdown.reason_counts.get("energy_heuristic_alignment"),
            Some(&2),
            "and the reason names the real cause (a missing aligner), not the reviewer's effort: {:?}",
            breakdown.reason_counts
        );
    }

    /// `training_grade_summary` must stay a thin delegate. Two copies of the grade bucketing is how a
    /// count starts lying, so this pins that both surfaces report identically.
    #[test]
    fn training_grade_summary_agrees_with_the_breakdown_it_delegates_to() {
        let mut gold = seg("g", "دەقی ڕاست", 5000);
        gold.verified = true;
        gold.human_decision = Some("accept".to_string());
        gold.clipping_ratio = Some(0.0);
        gold.rms_db = Some(-20.0);
        gold.snr_db = Some(25.0);
        let segs = vec![gold, seg("blank", " ", 5000)];

        assert_eq!(training_grade_summary(&segs), training_grade_breakdown(&segs).summary);
        assert_eq!(training_grade_breakdown(&segs).summary.training_ready_segments, 1);
        assert_eq!(
            training_grade_breakdown(&segs).reason_counts.get("blank_transcript"),
            Some(&1),
            "the rejected clip's reason is tallied"
        );
    }

    #[test]
    fn training_grade_uses_human_edit_as_gold_transcript() {
        let mut s = seg("human", "raw transcript", 5000);
        s.annotated_transcript = Some("older annotation".to_string());
        s.verdict_transcript = Some("human corrected transcript".to_string());
        s.human_decision = Some("edit".to_string());
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-20.0);
        s.snr_db = Some(25.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.transcript, "human corrected transcript");
        assert_eq!(report.transcript_source, "human_verified");
        assert_eq!(report.grade, TRAINING_GRADE_GOLD);
        assert!(report.training_ready);
    }

    // A curator hand-transcribes a segment whose raw ASR was deferred/failed, so raw_transcript is a
    // permanent "[Pending WSL 7B ASR]" placeholder (record_human_decision never overwrites it). The
    // effective transcript is the correct human text and the segment is human-verified, so it must
    // grade GOLD / training_ready — not be demoted to REVIEW because is_low_confidence keyed off the
    // stale raw placeholder. Pins the fix that grades the EFFECTIVE transcript, not raw.
    #[test]
    fn training_grade_human_edit_over_failed_raw_asr_is_gold() {
        let mut s = seg("human_over_pending", "[Pending WSL 7B ASR]", 5000);
        s.verdict = Some("human_edit".to_string());
        s.verdict_transcript = Some("ئەمڕۆ هەوا زۆر خۆشە".to_string());
        s.human_decision = Some("edit".to_string());
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-20.0);
        s.snr_db = Some(25.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.transcript, "ئەمڕۆ هەوا زۆر خۆشە");
        assert_eq!(report.transcript_source, "human_verified");
        assert_eq!(
            report.grade, TRAINING_GRADE_GOLD,
            "human edit over a failed raw ASR must grade GOLD, not REVIEW. reasons: {:?}",
            report.reasons
        );
        assert!(report.training_ready, "hand-corrected clip must be training_ready");
        assert!(
            !report.reasons.iter().any(|r| r == "low_confidence_alignment"),
            "stale raw placeholder must not trigger low_confidence_alignment"
        );
    }

    // The same applies to an ASR error placeholder ("[ASR unavailable: …]") hand-corrected by a human.
    #[test]
    fn training_grade_human_edit_over_asr_error_is_gold() {
        let mut s = seg("human_over_error", "[ASR unavailable: transcribe timed out]", 5000);
        s.verdict = Some("human_edit".to_string());
        s.verdict_transcript = Some("دیتنا ڕاست".to_string());
        s.human_decision = Some("edit".to_string());
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-20.0);
        s.snr_db = Some(25.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.grade, TRAINING_GRADE_GOLD, "reasons: {:?}", report.reasons);
        assert!(report.training_ready);
    }

    /// Builds the exact shape that USED to sail through: a jury-accepted clip with full multi-agent
    /// evidence and clean audio, whose only confidence is the engine's. The caller picks the
    /// provenance, which is the single thing under test.
    fn jury_row_whose_only_confidence_is_the_engines(source: Option<&str>) -> SpeechSegment {
        let mut s = seg("jury", "raw transcript", 5000);
        s.verdict = Some("jury_accept".to_string());
        s.verdict_transcript = Some("jury selected transcript".to_string());
        s.agreement_score = None; // the ONLY confidence is the engine's
        s.confidence = Some(0.90); // exactly what asr.rs stamps on every non-empty transcript
        s.confidence_source = source.map(str::to_string);
        s.evidence_json = Some(
            serde_json::json!({
                "referenceModelId": "gemini-2.5-pro",
                "selectedModelId": "omniasr-wsl-7b",
                "selectedTranscript": "jury selected transcript",
                "shouldCommit": true
            })
            .to_string(),
        );
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-18.0);
        s.snr_db = Some(30.0);
        s
    }

    #[test]
    fn heuristic_engine_confidence_cannot_promote_a_clip_no_human_ever_read() {
        // asr.rs stamps a CONSTANT 0.90 with source "heuristic" on every non-empty transcript the
        // shipped OmniASR CTC path produces, because that engine exposes no token posteriors. If that
        // constant counted, `confidence >= 0.85` would be true for EVERY clip in the corpus and the
        // clause would filter nothing while appearing to.
        let s = jury_row_whose_only_confidence_is_the_engines(Some("heuristic"));
        let report = training_grade_for_segment(&s);
        assert_ne!(
            report.grade, TRAINING_GRADE_SILVER,
            "a constant heuristic confidence must not promote to SILVER: {:?}",
            report.reasons
        );
        assert!(!report.training_ready, "reasons: {:?}", report.reasons);
    }

    #[test]
    fn unrecorded_confidence_provenance_fails_closed() {
        // Legacy rows predate confidence_source, and 'unknown' is what the COALESCE in db.rs writes.
        // Neither is evidence the number was calibrated, and this branch promotes into a training set
        // with no human in the loop — so both fail closed rather than being given the benefit of doubt.
        for source in [None, Some("unknown")] {
            let s = jury_row_whose_only_confidence_is_the_engines(source);
            let report = training_grade_for_segment(&s);
            assert!(
                !report.training_ready,
                "confidence_source {source:?} must not be training_ready: {:?}",
                report.reasons
            );
        }
    }

    #[test]
    fn a_real_model_posterior_still_promotes() {
        // The guard must gate on PROVENANCE, not disable the branch. An engine that genuinely emits
        // per-token posteriors (ConfidenceSource::RealPosterior) is exactly what it was written for.
        let s = jury_row_whose_only_confidence_is_the_engines(Some(
            crate::asr::ConfidenceSource::RealPosterior.as_db_value(),
        ));
        let report = training_grade_for_segment(&s);
        assert_eq!(report.grade, TRAINING_GRADE_SILVER, "reasons: {:?}", report.reasons);
        assert!(report.training_ready);
    }

    #[test]
    fn training_grade_accepts_high_confidence_reference_jury_rows_as_silver() {
        let mut s = seg("jury", "raw transcript", 5000);
        s.verdict = Some("jury_accept".to_string());
        s.verdict_transcript = Some("jury selected transcript".to_string());
        s.agreement_score = Some(0.92);
        s.evidence_json = Some(
            serde_json::json!({
                "referenceModelId": "gemini-2.5-pro",
                "selectedModelId": "omniasr-wsl-7b",
                "selectedTranscript": "jury selected transcript",
                "shouldCommit": true
            })
            .to_string(),
        );
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-18.0);
        s.snr_db = Some(30.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.transcript, "jury selected transcript");
        assert_eq!(report.transcript_source, "jury_verdict");
        assert_eq!(report.grade, TRAINING_GRADE_SILVER);
        assert!(report.training_ready);
        assert!(report.reasons.contains(&"multi_agent_evidence_verified".to_string()));
    }

    #[test]
    fn training_grade_keeps_mismatched_reference_evidence_in_review() {
        let mut s = seg("stale-reference-evidence", "raw transcript", 5000);
        s.verdict = Some("jury_accept".to_string());
        s.verdict_transcript = Some("current promoted transcript".to_string());
        s.agreement_score = Some(0.96);
        s.evidence_json = Some(
            serde_json::json!({
                "referenceModelId": "gemini-2.5-pro",
                "selectedModelId": "omniasr-wsl-7b",
                "selectedTranscript": "older selected transcript",
                "shouldCommit": true
            })
            .to_string(),
        );
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-18.0);
        s.snr_db = Some(30.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.transcript, "current promoted transcript");
        assert_eq!(report.grade, TRAINING_GRADE_REVIEW);
        assert!(!report.training_ready);
        assert!(report.reasons.contains(&"missing_multi_agent_evidence".to_string()));
    }

    #[test]
    fn training_grade_keeps_high_confidence_jury_without_multi_agent_evidence_in_review() {
        let mut s = seg("weak-jury", "raw transcript", 5000);
        s.verdict = Some("jury_accept".to_string());
        s.verdict_transcript = Some("jury selected transcript".to_string());
        s.agreement_score = Some(0.97);
        s.evidence_json = Some(
            serde_json::json!([
                {"tool": "lexicon", "result": "looks plausible", "supportsHypothesis": true}
            ])
            .to_string(),
        );
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-18.0);
        s.snr_db = Some(30.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.transcript, "jury selected transcript");
        assert_eq!(report.grade, TRAINING_GRADE_REVIEW);
        assert!(!report.training_ready);
        assert!(report.reasons.contains(&"missing_multi_agent_evidence".to_string()));
    }

    #[test]
    fn training_grade_ignores_individual_reference_votes_without_consensus_commit() {
        let mut s = seg("reference-conflict", "raw transcript", 5000);
        s.verdict = Some("jury_accept".to_string());
        s.verdict_transcript = Some("jury selected transcript".to_string());
        s.agreement_score = Some(0.97);
        s.evidence_json = Some(
            serde_json::json!({
                "referenceModelId": "gemini-2.5-pro",
                "shouldCommit": false,
                "referenceAgreement": [
                    {
                        "referenceModelId": "gemini-2.5-pro",
                        "selectedTranscript": "first transcript",
                        "shouldCommit": true
                    },
                    {
                        "referenceModelId": "gemini-2.5-flash",
                        "selectedTranscript": "second transcript",
                        "shouldCommit": true
                    }
                ]
            })
            .to_string(),
        );
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-18.0);
        s.snr_db = Some(30.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.grade, TRAINING_GRADE_REVIEW);
        assert!(!report.training_ready);
        assert!(report.reasons.contains(&"missing_multi_agent_evidence".to_string()));
    }

    #[test]
    fn training_grade_accepts_t2_audio_listener_evidence_as_silver() {
        let mut s = seg("t2-jury", "raw transcript", 5000);
        s.verdict = Some("jury_accept".to_string());
        s.verdict_transcript = Some("t2 selected transcript".to_string());
        s.agreement_score = Some(0.9);
        s.evidence_json =
            Some(serde_json::json!({"t2Transcript": "t2 selected transcript", "t2Evidence": []}).to_string());
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-18.0);
        s.snr_db = Some(30.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.transcript, "t2 selected transcript");
        assert_eq!(report.grade, TRAINING_GRADE_SILVER);
        assert!(report.training_ready);
    }

    #[test]
    fn training_grade_keeps_mismatched_t2_evidence_in_review() {
        let mut s = seg("stale-t2-evidence", "raw transcript", 5000);
        s.verdict = Some("jury_accept".to_string());
        s.verdict_transcript = Some("current t2 transcript".to_string());
        s.agreement_score = Some(0.93);
        s.evidence_json =
            Some(serde_json::json!({"t2Transcript": "older t2 transcript", "t2Evidence": []}).to_string());
        s.clipping_ratio = Some(0.0);
        s.rms_db = Some(-18.0);
        s.snr_db = Some(30.0);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.transcript, "current t2 transcript");
        assert_eq!(report.grade, TRAINING_GRADE_REVIEW);
        assert!(!report.training_ready);
        assert!(report.reasons.contains(&"missing_multi_agent_evidence".to_string()));
    }

    #[test]
    fn training_grade_rejects_severe_audio_even_with_verified_text() {
        let mut s = seg("clipped", "trusted transcript", 5000);
        s.verified = true;
        s.clipping_ratio = Some(0.2);

        let report = training_grade_for_segment(&s);

        assert_eq!(report.grade, TRAINING_GRADE_REJECT);
        assert!(!report.training_ready);
        assert!(report.reasons.contains(&"severe_clipping".to_string()));
    }

    #[test]
    fn duration_outliers_use_iqr() {
        let segments = vec![
            seg("a", "one", 5000),
            seg("b", "two", 5200),
            seg("c", "three", 5100),
            seg("d", "four", 5300),
            seg("e", "five", 60000),
        ];
        let q = compute_quality_from_segments(&segments);
        assert_eq!(q.duration_outlier_count, 1);
        assert_eq!(q.duration_outliers[0].segment_id, "e");
    }

    #[test]
    fn empty_dataset_returns_defaults() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let q = compute_quality(&db).unwrap();
        assert_eq!(q.total_segments, 0);
        assert_eq!(q.duplicate_transcript_groups, 0);
    }

    #[test]
    fn wer_metrics_from_annotations() {
        let mut s = seg("a", "hello earth", 5000);
        s.annotated_transcript = Some("hello world".to_string());
        let settings = AppSettings::default();
        let q = compute_quality_from_segments_with_settings(&[s], &settings);
        assert_eq!(q.annotated_segment_count, 1);
        assert!(q.mean_wer.unwrap() > 0.0);
        assert!(q.segments_above_wer_threshold >= 1 || q.mean_wer.unwrap() > 0.0);
    }

    #[test]
    fn training_grade_summary_counts_ready_tiers() {
        let mut gold = seg("gold", "trusted", 5000);
        gold.verified = true;
        let mut silver = seg("silver", "raw", 5000);
        silver.verdict = Some("jury_accept".to_string());
        silver.verdict_transcript = Some("jury trusted".to_string());
        silver.agreement_score = Some(0.9);
        silver.evidence_json = Some(
            serde_json::json!({
                "referenceModelId": "gemini-2.5-pro",
                "selectedModelId": "omniasr-wsl-7b",
                "selectedTranscript": "jury trusted",
                "shouldCommit": true
            })
            .to_string(),
        );
        let review = seg("review", "unreviewed", 5000);
        let reject = seg("reject", "", 5000);

        let summary = training_grade_summary(&[gold, silver, review, reject]);

        assert_eq!(summary.total_segments, 4);
        assert_eq!(summary.training_ready_segments, 2);
        assert_eq!(summary.gold_segments, 1);
        assert_eq!(summary.silver_segments, 1);
        assert_eq!(summary.review_segments, 1);
        assert_eq!(summary.rejected_segments, 1);
    }
}
