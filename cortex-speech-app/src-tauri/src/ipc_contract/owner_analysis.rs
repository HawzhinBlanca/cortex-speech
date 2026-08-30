//! Typed public contracts for owner-local analytics, model evidence, jury diagnostics, and WSL
//! refinement. Native diagnostics stay in Rust logs; this module exposes only bounded stable errors.

use super::{CommandErrorV1, SuggestedActionV1};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EvalRunV1 {
    pub id: String,
    pub model_id: String,
    pub run_at: String,
    pub num_segs: i64,
    pub wer: f64,
    pub cer: f64,
    pub meta_json: Option<String>,
}

impl From<crate::eval::EvalRun> for EvalRunV1 {
    fn from(value: crate::eval::EvalRun) -> Self {
        Self {
            id: value.id,
            model_id: value.model_id,
            run_at: value.run_at,
            num_segs: value.num_segs,
            wer: value.wer,
            cer: value.cer,
            meta_json: value.meta_json,
        }
    }
}

impl From<EvalRunV1> for crate::eval::EvalRun {
    fn from(value: EvalRunV1) -> Self {
        Self {
            id: value.id,
            model_id: value.model_id,
            run_at: value.run_at,
            num_segs: value.num_segs,
            wer: value.wer,
            cer: value.cer,
            meta_json: value.meta_json,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EvalSegmentResultV1 {
    pub gold_id: String,
    pub audio_path: String,
    pub reference: String,
    pub hypothesis: String,
    pub wer: f64,
    pub cer: f64,
}

impl From<crate::eval::EvalSegmentResult> for EvalSegmentResultV1 {
    fn from(value: crate::eval::EvalSegmentResult) -> Self {
        Self {
            gold_id: value.gold_id,
            audio_path: value.audio_path,
            reference: value.reference,
            hypothesis: value.hypothesis,
            wer: value.wer,
            cer: value.cer,
        }
    }
}

impl From<EvalSegmentResultV1> for crate::eval::EvalSegmentResult {
    fn from(value: EvalSegmentResultV1) -> Self {
        Self {
            gold_id: value.gold_id,
            audio_path: value.audio_path,
            reference: value.reference,
            hypothesis: value.hypothesis,
            wer: value.wer,
            cer: value.cer,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EvalRunResultV1 {
    pub run: EvalRunV1,
    pub segments: Vec<EvalSegmentResultV1>,
}

impl From<crate::eval::EvalRunResult> for EvalRunResultV1 {
    fn from(value: crate::eval::EvalRunResult) -> Self {
        Self { run: value.run.into(), segments: value.segments.into_iter().map(Into::into).collect() }
    }
}

impl From<EvalRunResultV1> for crate::eval::EvalRunResult {
    fn from(value: EvalRunResultV1) -> Self {
        Self { run: value.run.into(), segments: value.segments.into_iter().map(Into::into).collect() }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConfidenceIntervalV1 {
    pub point: f64,
    pub lower: f64,
    pub upper: f64,
    pub confidence: f64,
}

impl From<crate::significance::ConfidenceInterval> for ConfidenceIntervalV1 {
    fn from(value: crate::significance::ConfidenceInterval) -> Self {
        Self { point: value.point, lower: value.lower, upper: value.upper, confidence: value.confidence }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SystemScoreV1 {
    pub model_id: String,
    pub num_segments: usize,
    pub scored_segments: usize,
    pub micro_wer: f64,
    pub micro_cer: f64,
    pub macro_wer: f64,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    pub wer_ci: ConfidenceIntervalV1,
    pub cer_ci: ConfidenceIntervalV1,
}

impl From<crate::scorecard::SystemScore> for SystemScoreV1 {
    fn from(value: crate::scorecard::SystemScore) -> Self {
        Self {
            model_id: value.model_id,
            num_segments: value.num_segments,
            scored_segments: value.scored_segments,
            micro_wer: value.micro_wer,
            micro_cer: value.micro_cer,
            macro_wer: value.macro_wer,
            substitutions: value.substitutions,
            deletions: value.deletions,
            insertions: value.insertions,
            wer_ci: value.wer_ci.into(),
            cer_ci: value.cer_ci.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BaselineComparisonV1 {
    pub baseline_model_id: String,
    pub paired_segments: usize,
    pub baseline_micro_wer: f64,
    pub system_micro_wer: f64,
    pub baseline_micro_cer: f64,
    pub system_micro_cer: f64,
    pub mapsswe_p_value: f64,
    pub significant_at_05: bool,
    pub beats_baseline: bool,
    pub slice_regressions: Vec<String>,
    pub evaluated_slices: usize,
}

impl From<crate::scorecard::BaselineComparison> for BaselineComparisonV1 {
    fn from(value: crate::scorecard::BaselineComparison) -> Self {
        Self {
            baseline_model_id: value.baseline_model_id,
            paired_segments: value.paired_segments,
            baseline_micro_wer: value.baseline_micro_wer,
            system_micro_wer: value.system_micro_wer,
            baseline_micro_cer: value.baseline_micro_cer,
            system_micro_cer: value.system_micro_cer,
            mapsswe_p_value: value.mapsswe_p_value,
            significant_at_05: value.significant_at_05,
            beats_baseline: value.beats_baseline,
            slice_regressions: value.slice_regressions,
            evaluated_slices: value.evaluated_slices,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardWithBaselineV1 {
    pub system: SystemScoreV1,
    pub vs_baseline: BaselineComparisonV1,
    pub bootstrap_resamples: usize,
    pub confidence: f64,
    pub seed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardWithoutBaselineV1 {
    pub system: SystemScoreV1,
    pub bootstrap_resamples: usize,
    pub confidence: f64,
    pub seed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(untagged)]
pub enum ScorecardV1 {
    WithBaseline(ScorecardWithBaselineV1),
    WithoutBaseline(ScorecardWithoutBaselineV1),
}

impl From<crate::scorecard::Scorecard> for ScorecardV1 {
    fn from(value: crate::scorecard::Scorecard) -> Self {
        let system = value.system.into();
        match value.vs_baseline {
            Some(vs_baseline) => Self::WithBaseline(ScorecardWithBaselineV1 {
                system,
                vs_baseline: vs_baseline.into(),
                bootstrap_resamples: value.bootstrap_resamples,
                confidence: value.confidence,
                seed: value.seed,
            }),
            None => Self::WithoutBaseline(ScorecardWithoutBaselineV1 {
                system,
                bootstrap_resamples: value.bootstrap_resamples,
                confidence: value.confidence,
                seed: value.seed,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Loop0ShadowV1 {
    pub total_observations: i64,
    pub would_fire: i64,
    pub fired_but_human_accepted_original: i64,
    pub fired_and_human_edited: i64,
    pub fired_and_human_rejected: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AutoAcceptPrecisionV1 {
    pub t0_accepts: i64,
    pub t1_escalations: i64,
    pub t0_human_confirmed: i64,
    pub t0_human_contradicted: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConformalCalibrationBucketV1 {
    pub bucket: String,
    pub verified_with_reference: i64,
    pub min_needed_at_zero_cer: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConformalCalibrationProgressV1 {
    pub target_error_cer: f64,
    pub per_bucket_delta: f64,
    pub min_needed_at_zero_cer: usize,
    pub buckets: Vec<ConformalCalibrationBucketV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IntelligenceReportV1 {
    pub loop0_shadow: Loop0ShadowV1,
    pub auto_accept_precision: AutoAcceptPrecisionV1,
    pub conformal_calibration: ConformalCalibrationProgressV1,
}

pub(crate) fn decode_intelligence_report(value: serde_json::Value) -> Result<IntelligenceReportV1, String> {
    serde_json::from_value(value).map_err(|_| "intelligence report contract mismatch".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EscalationTrendPointV1 {
    pub date: String,
    pub escalation_rate: f64,
    pub total: i64,
    pub escalated: i64,
}

impl From<crate::jury::EscalationTrendPoint> for EscalationTrendPointV1 {
    fn from(value: crate::jury::EscalationTrendPoint) -> Self {
        Self {
            date: value.date,
            escalation_rate: value.escalation_rate,
            total: value.total,
            escalated: value.escalated,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JuryPipelineModeV1 {
    NotRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JuryPipelineNotRequiredV1 {
    pub mode: JuryPipelineModeV1,
    pub total_input: usize,
    pub t0_auto_accepted: usize,
    pub t0_escalated: usize,
    pub reference_committed: usize,
    pub reference_guarded: usize,
    pub hypothesis_guarded: usize,
    pub t1_committed: usize,
    pub t2_committed: usize,
    pub human_inbox: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JuryPipelineCompletedV1 {
    pub total_input: usize,
    pub t0_auto_accepted: usize,
    pub t0_escalated: usize,
    pub reference_committed: usize,
    pub reference_guarded: usize,
    pub hypothesis_guarded: usize,
    pub t1_committed: usize,
    pub t2_committed: usize,
    pub human_inbox: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(untagged)]
pub enum JuryPipelineReportV1 {
    NotRequired(JuryPipelineNotRequiredV1),
    Completed(JuryPipelineCompletedV1),
}

pub(crate) fn decode_jury_pipeline_report(value: serde_json::Value) -> Result<JuryPipelineReportV1, String> {
    serde_json::from_value(value).map_err(|_| "jury pipeline report contract mismatch".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct T2EvidenceV1 {
    pub tool: String,
    pub result: String,
    pub supports_hypothesis: bool,
}

impl From<crate::jury::t1_judge::Evidence> for T2EvidenceV1 {
    fn from(value: crate::jury::t1_judge::Evidence) -> Self {
        Self { tool: value.tool, result: value.result, supports_hypothesis: value.supports_hypothesis }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct T2VerdictV1 {
    pub transcript: String,
    pub reason: String,
    pub confidence: f64,
    pub evidence: Vec<T2EvidenceV1>,
    pub self_consistency_agreement: bool,
    pub votes: usize,
}

impl From<crate::jury::t2_listener::T2Verdict> for T2VerdictV1 {
    fn from(value: crate::jury::t2_listener::T2Verdict) -> Self {
        Self {
            transcript: value.transcript,
            reason: value.reason,
            confidence: value.confidence,
            evidence: value.evidence.into_iter().map(Into::into).collect(),
            self_consistency_agreement: value.self_consistency_agreement,
            votes: value.votes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct T2ResultV1 {
    pub verdict: Option<T2VerdictV1>,
    pub must_escalate: bool,
    pub error: Option<String>,
}

impl From<crate::jury::t2_listener::T2Result> for T2ResultV1 {
    fn from(value: crate::jury::t2_listener::T2Result) -> Self {
        Self {
            verdict: value.verdict.map(Into::into),
            must_escalate: value.must_escalate,
            // Provider/network details can contain endpoints, request fragments, or credentials.
            // The native warning retains them; the owner UI needs only the stable terminal class.
            error: value.error.map(|_| "T2_JUDGE_UNAVAILABLE".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WslRefinementStartStatusV1 {
    Started,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WslRefinementStartedV1 {
    pub status: WslRefinementStartStatusV1,
}

fn retryable(code: &str, message: &str) -> CommandErrorV1 {
    CommandErrorV1::new(code, message, true).suggested(SuggestedActionV1::Retry)
}

fn health(code: &str, message: &str) -> CommandErrorV1 {
    CommandErrorV1::new(code, message, false).suggested(SuggestedActionV1::OpenHealth)
}

pub(crate) fn owner_analysis_rate_limited(operation: &str) -> CommandErrorV1 {
    let message = match operation {
        "build_scorecard" | "list_eval_runs" | "run_gold_eval_asr" => {
            "Model-evidence analysis is busy. Wait a moment, then retry."
        }
        "compute_signal_anomaly_scores" => "Signal analysis is busy. Wait a moment, then retry.",
        "get_active_learning_queue" | "get_escalation_queue" | "get_escalation_rate_trend" => {
            "Review analytics are busy. Wait a moment, then retry."
        }
        "get_intelligence_report" => "Intelligence reporting is busy. Wait a moment, then retry.",
        "run_jury_pipeline" | "run_t2_for_segment" => "Jury analysis is busy. Wait a moment, then retry.",
        "run_wsl_refinement" => "Champion refinement is busy. Wait a moment, then retry.",
        "rediarize_segments" => "Speaker analysis is busy. Wait a moment, then retry.",
        _ => "This analysis is busy. Wait a moment, then retry.",
    };
    retryable("RATE_LIMITED", message)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum OwnerAnalysisOperationV1 {
    SignalAnomaly,
    ActiveLearningQueue,
    EscalationQueue,
    EscalationTrend,
    IntelligenceReport,
    EvalHistory,
}

pub(crate) fn public_owner_analysis_error(operation: OwnerAnalysisOperationV1, private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "This analysis cannot run while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    let normalized = private_detail.to_ascii_lowercase();
    if normalized.contains("database is locked") || normalized.contains("database is busy") {
        return retryable("DATABASE_BUSY", "The library is busy. Wait a moment, then retry.");
    }
    if private_detail.contains("background task failed") {
        return health(
            "ANALYSIS_WORKER_FAILED",
            "The analysis worker stopped unexpectedly. Open Health before retrying.",
        );
    }
    match operation {
        OwnerAnalysisOperationV1::SignalAnomaly => CommandErrorV1::new(
            "SIGNAL_ANALYSIS_FAILED",
            "Signal anomaly analysis did not complete. Existing segment evidence is unchanged for unfinished clips.",
            false,
        )
        .suggested(SuggestedActionV1::OpenModels),
        OwnerAnalysisOperationV1::ActiveLearningQueue => health(
            "ACTIVE_LEARNING_QUEUE_FAILED",
            "The active-learning queue could not be computed. Open Health before relying on it.",
        ),
        OwnerAnalysisOperationV1::EscalationQueue => health(
            "ESCALATION_QUEUE_FAILED",
            "The escalation queue could not be loaded. Open Health before relying on it.",
        ),
        OwnerAnalysisOperationV1::EscalationTrend => health(
            "ESCALATION_TREND_FAILED",
            "Escalation history could not be loaded. Open Health before relying on it.",
        ),
        OwnerAnalysisOperationV1::IntelligenceReport => health(
            "INTELLIGENCE_REPORT_FAILED",
            "The intelligence report could not be produced. Open Health before relying on it.",
        ),
        OwnerAnalysisOperationV1::EvalHistory => {
            health("EVAL_HISTORY_FAILED", "Evaluation history could not be loaded. Open Health before relying on it.")
        }
    }
}

pub(crate) fn public_gold_eval_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::pipeline::ASR_7B_UNAVAILABLE_TAG) {
        return CommandErrorV1::new(
            crate::pipeline::ASR_7B_UNAVAILABLE_TAG,
            "E_ASR_7B_UNAVAILABLE: The pinned OmniASR-7B champion is unavailable. No evaluation run was published.",
            true,
        )
        .suggested(SuggestedActionV1::OpenModels);
    }
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "Gold evaluation cannot start while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    if private_detail.contains("background task failed") {
        return health(
            "GOLD_EVAL_WORKER_FAILED",
            "The gold-evaluation worker stopped unexpectedly. No completed run was published; open Health.",
        );
    }
    CommandErrorV1::new(
        "GOLD_EVAL_FAILED",
        "Champion gold evaluation failed before a verified run was returned. No completed run was published.",
        false,
    )
    .suggested(SuggestedActionV1::OpenHealth)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum JuryOperationV1 {
    Pipeline,
    T2,
}

pub(crate) fn public_jury_error(operation: JuryOperationV1, private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "Jury analysis cannot run while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    if private_detail.contains("Cloud opt-in is required") {
        return CommandErrorV1::new(
            "CLOUD_CONSENT_REQUIRED",
            "Cloud listening is disabled. Enable informed jury consent in Settings before running T2.",
            false,
        );
    }
    if private_detail.contains("API key is required") || private_detail.contains("key is not configured") {
        return CommandErrorV1::new(
            "JUDGE_API_KEY_REQUIRED",
            "The configured jury provider requires an API key before T2 can run.",
            false,
        );
    }
    if private_detail.contains("Segment not found") {
        return CommandErrorV1::new("SEGMENT_NOT_FOUND", "This clip no longer exists; reload the library.", false)
            .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("no current OmniASR 7B provenance") {
        return CommandErrorV1::new(
            "CHAMPION_PROVENANCE_REQUIRED",
            "T2 requires a current pinned-champion draft for this clip. Open Models and re-transcribe first.",
            false,
        )
        .suggested(SuggestedActionV1::OpenModels);
    }
    if private_detail.contains("Cannot prepare segment audio") {
        return CommandErrorV1::new(
            "JUDGE_AUDIO_UNAVAILABLE",
            "The exact clip audio could not be prepared for T2. Reload the clip or relink its source.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("background task failed") {
        return health("JURY_WORKER_FAILED", "The jury worker stopped unexpectedly. Open Health before retrying.");
    }
    match operation {
        JuryOperationV1::Pipeline => health(
            "JURY_PIPELINE_FAILED",
            "The jury pipeline did not return a complete report. Open Health before relying on its results.",
        ),
        JuryOperationV1::T2 => CommandErrorV1::new(
            "T2_JUDGE_FAILED",
            "The listening judge did not return a verified result. The owner review draft remains authoritative.",
            false,
        ),
    }
}

pub(crate) fn public_wsl_refinement_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "Champion refinement cannot start while database recovery is in progress. Wait, then retry.",
        );
    }
    if private_detail.contains("already running") {
        return retryable(
            "WSL_REFINEMENT_IN_PROGRESS",
            "A champion refinement run is already active. Wait for it to finish or cancel it.",
        );
    }
    if private_detail.contains("not configured") {
        return CommandErrorV1::new(
            "WSL_REFINEMENT_NOT_CONFIGURED",
            "The pinned champion refinement provider is not configured. Open Models before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::OpenModels);
    }
    if private_detail.contains("worker thread") {
        return retryable(
            "WSL_REFINEMENT_WORKER_START_FAILED",
            "The champion refinement worker could not start. No background run was accepted; retry.",
        );
    }
    health("WSL_REFINEMENT_START_FAILED", "Champion refinement could not start. Open Health before retrying.")
}

pub(crate) fn public_rediarization_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "Speaker analysis cannot run while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    if private_detail.contains("Speaker diarization is disabled") {
        return CommandErrorV1::new(
            "DIARIZATION_DISABLED",
            "Speaker diarization is disabled. Enable it in Settings before running speaker analysis.",
            false,
        );
    }
    let normalized = private_detail.to_ascii_lowercase();
    if normalized.contains("database is locked") || normalized.contains("database is busy") {
        return retryable("DATABASE_BUSY", "The library is busy. Wait a moment, then retry.");
    }
    if private_detail.contains("background task failed") {
        return health(
            "REDIARIZATION_WORKER_FAILED",
            "The speaker-analysis worker stopped unexpectedly. Existing speaker labels remain unchanged for unfinished clips.",
        );
    }
    health(
        "REDIARIZATION_FAILED",
        "Speaker analysis did not complete. Existing speaker labels remain unchanged for unfinished clips.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_scrubbed(error: &CommandErrorV1) {
        let wire = serde_json::to_string(error).expect("serialize public error");
        for private in [
            "C:\\private-profile\\Owner\\secret.wav",
            "SELECT * FROM speech_segments",
            "token=owner-secret",
            "https://private-provider.invalid",
        ] {
            assert!(!wire.contains(private), "private diagnostic escaped public wire: {wire}");
        }
        assert!(error.message.chars().count() <= 180, "public message must stay bounded");
    }

    #[test]
    fn analysis_jury_and_refinement_errors_are_bounded_and_scrubbed() {
        let private = "C:\\private-profile\\Owner\\secret.wav SELECT * FROM speech_segments token=owner-secret https://private-provider.invalid";
        let errors = [
            public_owner_analysis_error(OwnerAnalysisOperationV1::SignalAnomaly, private),
            public_owner_analysis_error(OwnerAnalysisOperationV1::ActiveLearningQueue, private),
            public_owner_analysis_error(OwnerAnalysisOperationV1::EscalationQueue, private),
            public_owner_analysis_error(OwnerAnalysisOperationV1::EscalationTrend, private),
            public_owner_analysis_error(OwnerAnalysisOperationV1::IntelligenceReport, private),
            public_owner_analysis_error(OwnerAnalysisOperationV1::EvalHistory, private),
            public_gold_eval_error(private),
            public_jury_error(JuryOperationV1::Pipeline, private),
            public_jury_error(JuryOperationV1::T2, private),
            public_wsl_refinement_error(private),
            public_rediarization_error(private),
        ];
        for error in &errors {
            assert_scrubbed(error);
        }
    }

    #[test]
    fn dynamic_reports_decode_to_the_exact_public_shape() {
        let intelligence = decode_intelligence_report(serde_json::json!({
            "loop0Shadow": {
                "totalObservations": 2,
                "wouldFire": 1,
                "firedButHumanAcceptedOriginal": 0,
                "firedAndHumanEdited": 1,
                "firedAndHumanRejected": 0
            },
            "autoAcceptPrecision": {
                "t0Accepts": 1,
                "t1Escalations": 1,
                "t0HumanConfirmed": 1,
                "t0HumanContradicted": 0
            },
            "conformalCalibration": {
                "targetErrorCer": 0.05,
                "perBucketDelta": 0.02,
                "minNeededAtZeroCer": 100,
                "buckets": []
            }
        }))
        .unwrap();
        assert_eq!(intelligence.loop0_shadow.total_observations, 2);

        let report = decode_jury_pipeline_report(serde_json::json!({
            "mode": "not_required",
            "totalInput": 3,
            "t0AutoAccepted": 0,
            "t0Escalated": 0,
            "referenceCommitted": 0,
            "referenceGuarded": 0,
            "hypothesisGuarded": 0,
            "t1Committed": 0,
            "t2Committed": 0,
            "humanInbox": 3,
            "reason": "owner review remains authoritative"
        }))
        .unwrap();
        let JuryPipelineReportV1::NotRequired(report) = report else {
            panic!("expected the explicit not-required shape")
        };
        assert_eq!(report.mode, JuryPipelineModeV1::NotRequired);
        assert_eq!(report.human_inbox, 3);
    }

    #[test]
    fn t2_success_result_scrubs_provider_failure_detail_without_changing_shape() {
        let public = T2ResultV1::from(crate::jury::t2_listener::T2Result {
            verdict: None,
            must_escalate: true,
            error: Some("token=owner-secret https://private-provider.invalid".into()),
        });
        assert_eq!(public.error.as_deref(), Some("T2_JUDGE_UNAVAILABLE"));
        let wire = serde_json::to_string(&public).unwrap();
        assert!(!wire.contains("owner-secret"));
        assert!(!wire.contains("private-provider"));
    }

    #[test]
    fn champion_unavailable_remains_an_exact_public_hard_stop() {
        let private = format!(
            "{}: worker failed at C:\\private-profile\\Owner\\models\\champion",
            crate::pipeline::ASR_7B_UNAVAILABLE_TAG
        );
        let error = public_gold_eval_error(&private);
        assert_eq!(error.code, crate::pipeline::ASR_7B_UNAVAILABLE_TAG);
        assert!(error.message.contains(crate::pipeline::ASR_7B_UNAVAILABLE_TAG));
        assert!(error.retryable);
        assert_eq!(error.suggested_action, Some(SuggestedActionV1::OpenModels));
        assert_scrubbed(&error);
    }

    #[test]
    fn scorecard_wire_omits_baseline_only_when_no_baseline_exists() {
        let score = SystemScoreV1 {
            model_id: "omniasr-7b@champion".into(),
            num_segments: 1,
            scored_segments: 1,
            micro_wer: 0.0,
            micro_cer: 0.0,
            macro_wer: 0.0,
            substitutions: 0,
            deletions: 0,
            insertions: 0,
            wer_ci: ConfidenceIntervalV1 { point: 0.0, lower: 0.0, upper: 0.0, confidence: 0.95 },
            cer_ci: ConfidenceIntervalV1 { point: 0.0, lower: 0.0, upper: 0.0, confidence: 0.95 },
        };
        let without = serde_json::to_value(ScorecardV1::WithoutBaseline(ScorecardWithoutBaselineV1 {
            system: score.clone(),
            bootstrap_resamples: 1_000,
            confidence: 0.95,
            seed: 42,
        }))
        .unwrap();
        assert!(without.get("vsBaseline").is_none());

        let with = serde_json::to_value(ScorecardV1::WithBaseline(ScorecardWithBaselineV1 {
            system: score,
            vs_baseline: BaselineComparisonV1 {
                baseline_model_id: "baseline".into(),
                paired_segments: 1,
                baseline_micro_wer: 0.1,
                system_micro_wer: 0.0,
                baseline_micro_cer: 0.1,
                system_micro_cer: 0.0,
                mapsswe_p_value: 1.0,
                significant_at_05: false,
                beats_baseline: false,
                slice_regressions: Vec::new(),
                evaluated_slices: 0,
            },
            bootstrap_resamples: 1_000,
            confidence: 0.95,
            seed: 42,
        }))
        .unwrap();
        assert!(with.get("vsBaseline").is_some());
    }

    #[test]
    fn rediarization_maps_lifecycle_failures_without_private_diagnostics() {
        let restore = public_rediarization_error(crate::database_runtime::RESTORE_IN_PROGRESS_MSG);
        assert_eq!(restore.code, "RESTORE_IN_PROGRESS");
        assert!(restore.retryable);

        let disabled = public_rediarization_error("Speaker diarization is disabled in settings");
        assert_eq!(disabled.code, "DIARIZATION_DISABLED");
        assert!(!disabled.retryable);

        let private = public_rediarization_error(
            "database open failed at C:\\private-profile\\Owner\\private.db SELECT token=secret",
        );
        assert_eq!(private.code, "REDIARIZATION_FAILED");
        assert_scrubbed(&private);
    }
}
