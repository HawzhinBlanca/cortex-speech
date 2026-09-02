//! Versioned public IPC wire contracts.
//!
//! These types contain only renderer-safe data. Database errors, SQL, secrets and private absolute
//! paths are mapped to stable codes before crossing this boundary.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeMap;

mod owner_critical;
pub use owner_critical::*;
mod owner_analysis;
pub use owner_analysis::*;
mod review_undo;
pub use review_undo::*;

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(untagged)]
pub enum CommandErrorDetailV1 {
    String(String),
    Number(f64),
    Boolean(bool),
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SuggestedActionV1 {
    Retry,
    OpenHealth,
    OpenModels,
    ReloadClip,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CommandErrorV1 {
    pub schema: u8,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub suggested_action: Option<SuggestedActionV1>,
    pub operation_id: Option<String>,
    #[serde(default)]
    pub details: BTreeMap<String, CommandErrorDetailV1>,
}

impl CommandErrorV1 {
    pub fn new(code: &str, message: &str, retryable: bool) -> Self {
        Self {
            schema: 1,
            code: code.to_string(),
            message: message.to_string(),
            retryable,
            suggested_action: None,
            operation_id: None,
            details: BTreeMap::new(),
        }
    }

    pub fn operation(mut self, operation_id: &str) -> Self {
        self.operation_id = Some(operation_id.to_string());
        self
    }

    pub fn suggested(mut self, action: SuggestedActionV1) -> Self {
        self.suggested_action = Some(action);
        self
    }

    pub fn detail(mut self, key: &str, value: impl Into<CommandErrorDetailV1>) -> Self {
        self.details.insert(key.to_string(), value.into());
        self
    }
}

impl From<String> for CommandErrorDetailV1 {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for CommandErrorDetailV1 {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<i64> for CommandErrorDetailV1 {
    fn from(value: i64) -> Self {
        Self::Number(value as f64)
    }
}

impl From<bool> for CommandErrorDetailV1 {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

/// Public identity for the two long-running local batch domains. The operation is included in
/// status responses and every event so a delayed normalization notification can never settle a
/// transcription run (or vice versa).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BatchOperationV1 {
    Transcribe,
    Normalize,
}

impl From<crate::BatchOperation> for BatchOperationV1 {
    fn from(value: crate::BatchOperation) -> Self {
        match value {
            crate::BatchOperation::Transcribe => Self::Transcribe,
            crate::BatchOperation::Normalize => Self::Normalize,
        }
    }
}

impl From<crate::db::BatchJobKindV1> for BatchOperationV1 {
    fn from(value: crate::db::BatchJobKindV1) -> Self {
        match value {
            crate::db::BatchJobKindV1::Transcribe => Self::Transcribe,
            crate::db::BatchJobKindV1::Normalize => Self::Normalize,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BatchRunStatusV1 {
    /// Exact process-local identity is in cancellable preflight; no durable journal exists yet.
    Starting,
    Running,
    Settled,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BatchRunDispositionV1 {
    Completed,
    Halted,
    Cancelled,
    Panicked,
}

impl From<crate::BatchRunDisposition> for BatchRunDispositionV1 {
    fn from(value: crate::BatchRunDisposition) -> Self {
        match value {
            crate::BatchRunDisposition::Completed => Self::Completed,
            crate::BatchRunDisposition::Halted => Self::Halted,
            crate::BatchRunDisposition::Cancelled => Self::Cancelled,
            crate::BatchRunDisposition::Panicked => Self::Panicked,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BatchRunOutcomeV1 {
    pub disposition: BatchRunDispositionV1,
    pub total: usize,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
    pub abandoned: u32,
    pub cancelled: bool,
    pub error_code: Option<String>,
}

impl From<crate::BatchRunOutcome> for BatchRunOutcomeV1 {
    fn from(value: crate::BatchRunOutcome) -> Self {
        Self {
            disposition: value.disposition.into(),
            total: value.total,
            succeeded: value.succeeded,
            failed: value.failed,
            skipped: value.skipped,
            abandoned: value.abandoned,
            cancelled: value.cancelled,
            error_code: value.error_code,
        }
    }
}

impl From<crate::BatchRunAdmission> for BatchRunStatusV1 {
    fn from(value: crate::BatchRunAdmission) -> Self {
        match value {
            crate::BatchRunAdmission::Running => Self::Running,
            crate::BatchRunAdmission::Settled => Self::Settled,
            crate::BatchRunAdmission::Rejected => Self::Rejected,
            crate::BatchRunAdmission::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BatchRunStatusResponseV1 {
    pub operation_id: String,
    pub operation: Option<BatchOperationV1>,
    pub status: BatchRunStatusV1,
    /// Exact durable request cardinality. Unknown/pre-admission identities have no trusted total.
    pub total: Option<usize>,
    pub outcome: Option<BatchRunOutcomeV1>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BatchStartStatusV1 {
    Started,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BatchStartedV1 {
    pub status: BatchStartStatusV1,
    pub operation_id: String,
    pub operation: BatchOperationV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
pub struct TracingStatsV1 {
    pub total_spans: usize,
    pub failures: usize,
    pub total_duration_ms: f64,
    pub avg_duration_ms: f64,
}

impl From<crate::telemetry::TracingStats> for TracingStatsV1 {
    fn from(value: crate::telemetry::TracingStats) -> Self {
        Self {
            total_spans: value.total_spans,
            failures: value.failures,
            total_duration_ms: value.total_duration_ms,
            avg_duration_ms: value.avg_duration_ms,
        }
    }
}

/// Minimal developer-diagnostic span. Raw metadata and error strings deliberately remain in the
/// backend because they can contain local paths, transcripts, SQL or third-party error payloads.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
pub struct TracingSpanV1 {
    pub operation: String,
    pub start: String,
    pub duration_ms: f64,
    pub success: bool,
}

impl From<crate::telemetry::Span> for TracingSpanV1 {
    fn from(value: crate::telemetry::Span) -> Self {
        Self {
            operation: value.operation.to_string(),
            start: value.start,
            duration_ms: value.duration_ms,
            success: value.success,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
pub struct InferenceKindStatsV1 {
    pub calls: u64,
    pub failures: u64,
    pub p50_ms: f64,
    pub p99_ms: f64,
}

impl From<crate::inference::InferenceKindStats> for InferenceKindStatsV1 {
    fn from(value: crate::inference::InferenceKindStats) -> Self {
        Self { calls: value.calls, failures: value.failures, p50_ms: value.p50_ms, p99_ms: value.p99_ms }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
pub struct InferenceStatsV1 {
    pub vad: InferenceKindStatsV1,
    pub asr: InferenceKindStatsV1,
    pub model_load_ms: f64,
}

impl From<crate::inference::InferenceStatsSnapshot> for InferenceStatsV1 {
    fn from(value: crate::inference::InferenceStatsSnapshot) -> Self {
        Self { vad: value.vad.into(), asr: value.asr.into(), model_load_ms: value.model_load_ms }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct AppHealthV1 {
    pub status: String,
    pub db_size: i64,
    pub uptime: u64,
    pub segment_count: i64,
    pub memory_mb: u64,
    pub primary_asr_model: String,
    pub missing_models: Vec<String>,
    pub missing_optional_models: Vec<String>,
    pub snapshot_last_success_epoch_secs: Option<u64>,
    pub snapshot_consecutive_failures: usize,
    pub free_disk_bytes: Option<u64>,
}

impl From<crate::health::HealthSnapshot> for AppHealthV1 {
    fn from(value: crate::health::HealthSnapshot) -> Self {
        Self {
            status: value.status,
            db_size: value.db_size,
            uptime: value.uptime,
            segment_count: value.segment_count,
            memory_mb: value.memory_mb,
            primary_asr_model: value.primary_asr_model,
            missing_models: value.missing_models,
            missing_optional_models: value.missing_optional_models,
            snapshot_last_success_epoch_secs: value.snapshot_last_success_epoch_secs,
            snapshot_consecutive_failures: value.snapshot_consecutive_failures,
            free_disk_bytes: value.free_disk_bytes,
        }
    }
}

/// Closed stage identities used by both live import events and persisted import history. Unknown
/// legacy/database values remain visible as `unknown`, but their raw text never crosses into the
/// renderer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStageCodeV1 {
    SourceReference,
    AudioChunking,
    MultiModelHypotheses,
    JuryAdjudication,
    AgentReport,
    DatasetPromotion,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStageStatusV1 {
    Running,
    Completed,
    Ready,
    Blocked,
    NeedsReview,
    NotRequired,
    Skipped,
    Failed,
    Degraded,
    Unprocessed,
    Unknown,
}

/// A localization key, not backend-authored prose. It deliberately mirrors the closed status set:
/// the stage code and progress counters provide all additional public context the UI needs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStageDetailCodeV1 {
    Running,
    Completed,
    Ready,
    Blocked,
    NeedsReview,
    NotRequired,
    Skipped,
    Failed,
    Degraded,
    Unprocessed,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentImportSourceV1 {
    File,
    Directory,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgentImportErrorCodeV1 {
    ImportReportFailed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentReadinessCheckCodeV1 {
    SourceReference,
    PrimaryAsr,
    HypothesisCoverage,
    ReadinessSnapshot,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentPromotionBlockerCodeV1 {
    NoSpeechChunks,
    SourceReferenceIncomplete,
    MissingSourceReferenceModels,
    MissingHypothesisCoverage,
    NoTrainingReadySegments,
    Unknown,
}

/// Closed presentation state for ordinary import progress. Backend-authored prose and raw paths
/// never cross this boundary; the renderer localizes the code and receives a sanitized label.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineProgressStatusV1 {
    Resuming,
    Processing,
    ReferenceTranscribing,
    Transcribing,
    Adjudicating,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineProgressV1 {
    pub run_id: String,
    pub current: usize,
    pub total: usize,
    pub file_label: String,
    pub status: PipelineProgressStatusV1,
}

/// Renderer-safe live stage event. Private `detail` remains in native logs / the durable database;
/// the webview gets a closed code and a basename-only file label.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentStageProgressV1 {
    pub run_id: String,
    pub stage: AgentStageCodeV1,
    pub status: AgentStageStatusV1,
    pub file_label: String,
    pub detail_code: AgentStageDetailCodeV1,
    pub current: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentStageEventV1 {
    pub id: i64,
    pub run_id: String,
    pub source: AgentImportSourceV1,
    pub stage: AgentStageCodeV1,
    pub status: AgentStageStatusV1,
    pub file_label: String,
    pub detail_code: AgentStageDetailCodeV1,
    pub current: usize,
    pub total: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentReadinessCheckV1 {
    pub code: AgentReadinessCheckCodeV1,
    pub status: AgentStageStatusV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgenticReadinessV1 {
    pub status: AgentStageStatusV1,
    pub ready: bool,
    pub source_reference_models: Vec<String>,
    pub source_reference_model_count: usize,
    pub available_hypothesis_models: Vec<String>,
    pub available_hypothesis_model_count: usize,
    pub required_hypothesis_models: usize,
    pub checks: Vec<AgentReadinessCheckV1>,
    pub check_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSourceReferenceV1 {
    pub audio_file_label: String,
    pub model_id: String,
    pub audio_content_hash: Option<String>,
    pub audio_size_bytes: Option<i64>,
    pub transcript_file_label: String,
    pub text_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSourceReferenceCoverageV1 {
    pub audio_file_label: String,
    pub required_models: Vec<String>,
    pub required_model_count: usize,
    pub present_models: Vec<String>,
    pub present_model_count: usize,
    pub missing_models: Vec<String>,
    pub missing_model_count: usize,
    pub complete: bool,
}

/// Only aggregate coverage evidence is public. Raw model strings in a corrupt/legacy report cannot
/// become an accidental diagnostic side channel through the blocker panel.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHypothesisCoverageV1 {
    pub minimum_non_empty_model_count: usize,
    pub non_empty_model_count: usize,
    pub passes_minimum: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHypothesisCoverageBlockerV1 {
    pub segment_id: String,
    pub grade: String,
    pub training_ready: bool,
    pub coverage: AgentHypothesisCoverageV1,
}

/// A public orchestration row contains only closed codes and a count. Backend-authored summaries
/// and blocker strings can include paths, segment diagnostics, or database errors and stay native.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentOrchestrationStageV1 {
    pub stage: AgentStageCodeV1,
    pub status: AgentStageStatusV1,
    pub detail_code: AgentStageDetailCodeV1,
    pub blocker_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentLongFileDossierV1 {
    pub audio_file_label: String,
    pub chunk_count: usize,
    pub total_duration_ms: i64,
    pub source_references: Vec<AgentSourceReferenceV1>,
    pub source_reference_count: usize,
    pub source_reference_coverage: AgentSourceReferenceCoverageV1,
    pub hypothesis_model_counts: BTreeMap<String, usize>,
    pub hypothesis_model_kind_count: usize,
    pub verdict_counts: BTreeMap<String, usize>,
    pub verdict_kind_count: usize,
    pub training_ready_segments: usize,
    pub escalated_segments: Vec<String>,
    pub escalated_segment_count: usize,
    pub promotion_status: AgentStageStatusV1,
    pub promotion_blocker_codes: Vec<AgentPromotionBlockerCodeV1>,
    pub promotion_blocker_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentImportSummaryV1 {
    pub total_segments: usize,
    pub agentic_readiness: Option<AgenticReadinessV1>,
    pub source_references: Vec<AgentSourceReferenceV1>,
    pub source_reference_count: usize,
    pub source_reference_required: bool,
    pub required_source_reference_models: Vec<String>,
    pub required_source_reference_model_count: usize,
    pub source_reference_models: Vec<String>,
    pub source_reference_model_count: usize,
    pub source_reference_coverage: Vec<AgentSourceReferenceCoverageV1>,
    pub source_reference_coverage_count: usize,
    pub long_file_dossiers: Vec<AgentLongFileDossierV1>,
    pub long_file_dossier_count: usize,
    pub hypothesis_models: Vec<String>,
    pub hypothesis_model_count: usize,
    pub hypothesis_model_counts: BTreeMap<String, usize>,
    pub hypothesis_model_kind_count: usize,
    pub verdict_counts: BTreeMap<String, usize>,
    pub verdict_kind_count: usize,
    pub escalated_segments: Vec<String>,
    pub escalated_segment_count: usize,
    pub training_grade_summary: crate::quality::TrainingGradeSummary,
    pub training_grade_reason_counts: BTreeMap<String, usize>,
    pub training_grade_reason_kind_count: usize,
    pub hypothesis_coverage_blockers: Vec<AgentHypothesisCoverageBlockerV1>,
    pub hypothesis_coverage_blocker_count: usize,
    pub orchestration_stages: Vec<AgentOrchestrationStageV1>,
    pub orchestration_stage_count: usize,
}

/// The renderer never receives `audio_paths`, the jury JSON, or a free-form persisted error.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentImportReportV1 {
    pub id: String,
    pub agent_run_id: Option<String>,
    pub source: AgentImportSourceV1,
    pub status: AgentStageStatusV1,
    pub summary: AgentImportSummaryV1,
    pub error_code: Option<AgentImportErrorCodeV1>,
    pub created_at: String,
}

const PUBLIC_FILE_LABEL_CHARS: usize = 160;
const PUBLIC_TOKEN_CHARS: usize = 96;
const PUBLIC_REPORT_LIST_PREVIEW: usize = 8;
const PUBLIC_REPORT_MODEL_PREVIEW: usize = 16;
const PUBLIC_REPORT_CHECK_PREVIEW: usize = 16;
const PUBLIC_REPORT_MAP_PREVIEW: usize = 16;
const PUBLIC_JS_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Basename-only, bounded, control-free label shared by command and event boundaries. Both slash
/// styles are split because a restored Windows row can be inspected by a non-Windows test host.
pub(crate) fn public_file_label(private_path: &str, fallback: &str) -> String {
    let basename = private_path.rsplit(['/', '\\']).next().unwrap_or_default();
    let value = basename
        .chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    *character,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
        .take(PUBLIC_FILE_LABEL_CHARS)
        .collect::<String>();
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn public_token(private_value: &str, fallback: &str) -> String {
    let value = private_value.trim();
    if value.is_empty()
        || value.chars().count() > PUBLIC_TOKEN_CHARS
        || value.contains(['/', '\\'])
        || (value.as_bytes().get(1) == Some(&b':') && value.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | ':' | '@'))
    {
        return fallback.to_string();
    }
    value.to_string()
}

fn public_timestamp(private_value: &str) -> String {
    let value = private_value.trim();
    if value.len() <= 48
        && !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, '-' | ':' | 'T' | 'Z' | '.' | '+' | ' '))
    {
        value.to_string()
    } else {
        String::new()
    }
}

fn public_hash(private_value: Option<&str>) -> Option<String> {
    private_value
        .map(str::trim)
        .filter(|value| value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

fn public_wire_count(value: usize) -> usize {
    value.min(u32::MAX as usize)
}

fn public_wire_i64(value: i64) -> i64 {
    value.clamp(0, PUBLIC_JS_SAFE_INTEGER)
}

fn public_token_preview<'a>(values: impl IntoIterator<Item = &'a str>, limit: usize) -> (Vec<String>, usize) {
    let mut seen = std::collections::BTreeSet::new();
    let mut preview = Vec::with_capacity(limit);
    let mut total = 0usize;
    for value in values {
        let token = public_token(value, "unknown");
        if seen.insert(token.clone()) {
            total = total.saturating_add(1);
            if preview.len() < limit {
                preview.push(token);
            }
        }
    }
    (preview, public_wire_count(total))
}

fn public_tokens_preview(values: &[String], limit: usize) -> (Vec<String>, usize) {
    public_token_preview(values.iter().map(String::as_str), limit)
}

fn public_count_map(values: &BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    let mut public = BTreeMap::new();
    for (key, value) in values {
        let key = public_token(key, "unknown");
        let current = public.entry(key).or_insert(0usize);
        *current = public_wire_count(current.saturating_add(*value));
    }
    public
}

fn public_count_map_preview(values: BTreeMap<String, usize>, limit: usize) -> (BTreeMap<String, usize>, usize) {
    let total = public_wire_count(values.len());
    let mut entries = values.into_iter().collect::<Vec<_>>();
    entries.sort_by(|(left_key, left_count), (right_key, right_count)| {
        right_count.cmp(left_count).then_with(|| left_key.cmp(right_key))
    });
    (entries.into_iter().take(limit).map(|(key, value)| (key, public_wire_count(value))).collect(), total)
}

fn public_closed_count_map(
    values: &BTreeMap<String, usize>,
    is_public_code: impl Fn(&str) -> bool,
) -> BTreeMap<String, usize> {
    let mut public = BTreeMap::new();
    for (key, value) in values {
        let key = if is_public_code(key) { key.clone() } else { "unknown".to_string() };
        let current = public.entry(key).or_insert(0usize);
        *current = public_wire_count(current.saturating_add(*value));
    }
    public
}

fn public_verdict_counts(values: &BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    public_closed_count_map(values, |value| {
        matches!(
            value,
            "auto_accept"
                | "jury_accept"
                | "jury_edit"
                | "escalated"
                | "human_accept"
                | "human_edit"
                | "human_reject"
                | "unprocessed"
        )
    })
}

fn public_training_grade_reason_counts(values: &BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    public_closed_count_map(values, |value| {
        if matches!(
            value,
            "human_rejected"
                | "blank_transcript"
                | "placeholder_transcript"
                | "low_confidence_alignment"
                | "energy_heuristic_alignment"
                | "human_verified"
                | "high_confidence_jury_accept"
                | "multi_agent_evidence_verified"
                | "jury_accept_needs_review"
                | "missing_multi_agent_evidence"
                | "not_human_or_high_confidence_agent_verified"
                | "severe_clipping"
                | "clipping_warning"
                | "near_silence"
                | "low_rms_volume"
                | "severe_low_snr"
                | "low_snr"
        ) {
            return true;
        }
        value.strip_prefix("technical_unusable:").is_some_and(crate::quality::is_supported_technical_unusable_reason)
    })
}

fn public_stage_code(value: &str) -> AgentStageCodeV1 {
    match value.trim() {
        "source_reference" => AgentStageCodeV1::SourceReference,
        "audio_chunking" => AgentStageCodeV1::AudioChunking,
        "multi_model_hypotheses" => AgentStageCodeV1::MultiModelHypotheses,
        "jury_adjudication" => AgentStageCodeV1::JuryAdjudication,
        "agent_report" => AgentStageCodeV1::AgentReport,
        "dataset_promotion" => AgentStageCodeV1::DatasetPromotion,
        _ => AgentStageCodeV1::Unknown,
    }
}

fn public_stage_status(value: &str) -> AgentStageStatusV1 {
    match value.trim() {
        "running" => AgentStageStatusV1::Running,
        "completed" => AgentStageStatusV1::Completed,
        "ready" => AgentStageStatusV1::Ready,
        "blocked" => AgentStageStatusV1::Blocked,
        "needs_review" => AgentStageStatusV1::NeedsReview,
        "not_required" => AgentStageStatusV1::NotRequired,
        "skipped" => AgentStageStatusV1::Skipped,
        "failed" => AgentStageStatusV1::Failed,
        "degraded" => AgentStageStatusV1::Degraded,
        "unprocessed" => AgentStageStatusV1::Unprocessed,
        _ => AgentStageStatusV1::Unknown,
    }
}

fn public_detail_code(status: AgentStageStatusV1) -> AgentStageDetailCodeV1 {
    match status {
        AgentStageStatusV1::Running => AgentStageDetailCodeV1::Running,
        AgentStageStatusV1::Completed => AgentStageDetailCodeV1::Completed,
        AgentStageStatusV1::Ready => AgentStageDetailCodeV1::Ready,
        AgentStageStatusV1::Blocked => AgentStageDetailCodeV1::Blocked,
        AgentStageStatusV1::NeedsReview => AgentStageDetailCodeV1::NeedsReview,
        AgentStageStatusV1::NotRequired => AgentStageDetailCodeV1::NotRequired,
        AgentStageStatusV1::Skipped => AgentStageDetailCodeV1::Skipped,
        AgentStageStatusV1::Failed => AgentStageDetailCodeV1::Failed,
        AgentStageStatusV1::Degraded => AgentStageDetailCodeV1::Degraded,
        AgentStageStatusV1::Unprocessed => AgentStageDetailCodeV1::Unprocessed,
        AgentStageStatusV1::Unknown => AgentStageDetailCodeV1::Unknown,
    }
}

fn public_import_source(value: &str) -> AgentImportSourceV1 {
    match value.trim() {
        "file" => AgentImportSourceV1::File,
        "directory" => AgentImportSourceV1::Directory,
        _ => AgentImportSourceV1::Unknown,
    }
}

pub(crate) fn public_agent_stage_progress(
    run_id: &str,
    stage: &str,
    status: &str,
    file: &str,
    current: usize,
    total: usize,
) -> AgentStageProgressV1 {
    let status = public_stage_status(status);
    let current = current.min(u32::MAX as usize);
    let total = total.min(u32::MAX as usize);
    AgentStageProgressV1 {
        run_id: public_token(run_id, "unknown"),
        stage: public_stage_code(stage),
        status,
        file_label: public_file_label(file, ""),
        detail_code: public_detail_code(status),
        current,
        total,
    }
}

pub(crate) fn public_pipeline_progress(
    run_id: &str,
    current: usize,
    total: usize,
    file: &str,
    status: &str,
) -> PipelineProgressV1 {
    let normalized = status.trim().to_ascii_lowercase();
    let status = if normalized.starts_with("already imported") {
        PipelineProgressStatusV1::Resuming
    } else if normalized == "processing..." || normalized == "processing" {
        PipelineProgressStatusV1::Processing
    } else if normalized == "building whole-file reference transcript" {
        PipelineProgressStatusV1::ReferenceTranscribing
    } else if normalized.starts_with("transcribing chunk") {
        PipelineProgressStatusV1::Transcribing
    } else if normalized.starts_with("adjudicat") {
        PipelineProgressStatusV1::Adjudicating
    } else {
        PipelineProgressStatusV1::Unknown
    };
    PipelineProgressV1 {
        run_id: public_token(run_id, "unknown"),
        current: public_wire_count(current),
        total: public_wire_count(total),
        file_label: public_file_label(file, ""),
        status,
    }
}

impl From<&crate::runs::AgentStageEvent> for AgentStageEventV1 {
    fn from(value: &crate::runs::AgentStageEvent) -> Self {
        let progress = public_agent_stage_progress(
            &value.run_id,
            &value.stage,
            &value.status,
            &value.file,
            value.current,
            value.total,
        );
        Self {
            id: public_wire_i64(value.id),
            run_id: public_token(&value.run_id, "unknown"),
            source: public_import_source(&value.source),
            stage: progress.stage,
            status: progress.status,
            file_label: progress.file_label,
            detail_code: progress.detail_code,
            current: progress.current,
            total: progress.total,
            created_at: public_timestamp(&value.created_at),
        }
    }
}

fn public_readiness_check_code(value: &str) -> AgentReadinessCheckCodeV1 {
    match value.trim() {
        "source_reference" => AgentReadinessCheckCodeV1::SourceReference,
        "primary_asr" => AgentReadinessCheckCodeV1::PrimaryAsr,
        "hypothesis_coverage" => AgentReadinessCheckCodeV1::HypothesisCoverage,
        "readiness_snapshot" => AgentReadinessCheckCodeV1::ReadinessSnapshot,
        _ => AgentReadinessCheckCodeV1::Unknown,
    }
}

impl From<&crate::commands::AgenticReadiness> for AgenticReadinessV1 {
    fn from(value: &crate::commands::AgenticReadiness) -> Self {
        let status = public_stage_status(&value.status);
        let (source_reference_models, source_reference_model_count) =
            public_tokens_preview(&value.source_reference_models, PUBLIC_REPORT_MODEL_PREVIEW);
        let (available_hypothesis_models, available_hypothesis_model_count) =
            public_tokens_preview(&value.available_hypothesis_models, PUBLIC_REPORT_MODEL_PREVIEW);
        Self {
            status,
            ready: status == AgentStageStatusV1::Ready && value.ready,
            source_reference_models,
            source_reference_model_count,
            available_hypothesis_models,
            available_hypothesis_model_count,
            required_hypothesis_models: public_wire_count(value.required_hypothesis_models),
            checks: value
                .checks
                .iter()
                .take(PUBLIC_REPORT_CHECK_PREVIEW)
                .map(|check| AgentReadinessCheckV1 {
                    code: public_readiness_check_code(&check.id),
                    status: public_stage_status(&check.status),
                })
                .collect(),
            check_count: public_wire_count(value.checks.len()),
        }
    }
}

fn public_readiness(value: &serde_json::Value) -> Option<AgenticReadinessV1> {
    let object = value.as_object()?;
    let status = public_stage_status(object.get("status").and_then(serde_json::Value::as_str).unwrap_or("unknown"));
    let (source_reference_models, source_reference_model_count) = object
        .get("sourceReferenceModels")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            public_token_preview(values.iter().filter_map(serde_json::Value::as_str), PUBLIC_REPORT_MODEL_PREVIEW)
        })
        .unwrap_or_else(|| (Vec::new(), 0));
    let (available_hypothesis_models, available_hypothesis_model_count) = object
        .get("availableHypothesisModels")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            public_token_preview(values.iter().filter_map(serde_json::Value::as_str), PUBLIC_REPORT_MODEL_PREVIEW)
        })
        .unwrap_or_else(|| (Vec::new(), 0));
    let mut checks = Vec::with_capacity(PUBLIC_REPORT_CHECK_PREVIEW);
    let mut check_count = 0usize;
    if let Some(values) = object.get("checks").and_then(serde_json::Value::as_array) {
        for check in values.iter().filter_map(serde_json::Value::as_object) {
            check_count = check_count.saturating_add(1);
            if checks.len() < PUBLIC_REPORT_CHECK_PREVIEW {
                checks.push(AgentReadinessCheckV1 {
                    code: public_readiness_check_code(
                        check.get("id").and_then(serde_json::Value::as_str).unwrap_or_default(),
                    ),
                    status: public_stage_status(
                        check.get("status").and_then(serde_json::Value::as_str).unwrap_or("unknown"),
                    ),
                });
            }
        }
    }
    Some(AgenticReadinessV1 {
        status,
        ready: status == AgentStageStatusV1::Ready
            && object.get("ready").and_then(serde_json::Value::as_bool).unwrap_or(false),
        source_reference_models,
        source_reference_model_count,
        available_hypothesis_models,
        available_hypothesis_model_count,
        required_hypothesis_models: public_wire_count(
            object
                .get("requiredHypothesisModels")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_default(),
        ),
        checks,
        check_count: public_wire_count(check_count),
    })
}

impl From<&crate::runs::AgentSourceReferenceSummary> for AgentSourceReferenceV1 {
    fn from(value: &crate::runs::AgentSourceReferenceSummary) -> Self {
        Self {
            audio_file_label: public_file_label(&value.audio_path, ""),
            model_id: public_token(&value.model_id, "unknown"),
            audio_content_hash: public_hash(value.audio_content_hash.as_deref()),
            audio_size_bytes: value.audio_size_bytes.filter(|size| *size >= 0).map(public_wire_i64),
            transcript_file_label: public_file_label(&value.transcript_path, ""),
            text_chars: public_wire_count(value.text_chars),
        }
    }
}

impl From<&crate::runs::AgentSourceReferenceCoverage> for AgentSourceReferenceCoverageV1 {
    fn from(value: &crate::runs::AgentSourceReferenceCoverage) -> Self {
        let (required_models, required_model_count) =
            public_tokens_preview(&value.required_models, PUBLIC_REPORT_MODEL_PREVIEW);
        let (present_models, present_model_count) =
            public_tokens_preview(&value.present_models, PUBLIC_REPORT_MODEL_PREVIEW);
        let (missing_models, missing_model_count) =
            public_tokens_preview(&value.missing_models, PUBLIC_REPORT_MODEL_PREVIEW);
        Self {
            audio_file_label: public_file_label(&value.audio_path, ""),
            required_models,
            required_model_count,
            present_models,
            present_model_count,
            missing_models,
            missing_model_count,
            complete: value.complete,
        }
    }
}

impl From<&crate::runs::AgentHypothesisCoverageBlocker> for AgentHypothesisCoverageBlockerV1 {
    fn from(value: &crate::runs::AgentHypothesisCoverageBlocker) -> Self {
        Self {
            segment_id: public_token(&value.segment_id, "unknown"),
            grade: public_token(&value.grade, "unknown"),
            training_ready: value.training_ready,
            coverage: AgentHypothesisCoverageV1 {
                minimum_non_empty_model_count: public_wire_count(value.coverage.minimum_non_empty_model_count),
                non_empty_model_count: public_wire_count(value.coverage.non_empty_model_count),
                passes_minimum: value.coverage.passes_minimum,
            },
        }
    }
}

fn public_promotion_blocker(value: &str) -> AgentPromotionBlockerCodeV1 {
    match value.split(':').next().unwrap_or_default() {
        "no_speech_chunks" => AgentPromotionBlockerCodeV1::NoSpeechChunks,
        "source_reference_incomplete" => AgentPromotionBlockerCodeV1::SourceReferenceIncomplete,
        "missing_source_reference_models" => AgentPromotionBlockerCodeV1::MissingSourceReferenceModels,
        "missing_hypothesis_coverage" => AgentPromotionBlockerCodeV1::MissingHypothesisCoverage,
        "no_training_ready_segments" => AgentPromotionBlockerCodeV1::NoTrainingReadySegments,
        _ => AgentPromotionBlockerCodeV1::Unknown,
    }
}

impl From<&crate::runs::AgentLongFileDossier> for AgentLongFileDossierV1 {
    fn from(value: &crate::runs::AgentLongFileDossier) -> Self {
        let mut promotion_blocker_codes = value
            .promotion_blockers
            .iter()
            .map(|blocker| public_promotion_blocker(blocker))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let promotion_blocker_count = public_wire_count(value.promotion_blockers.len());
        promotion_blocker_codes.truncate(PUBLIC_REPORT_LIST_PREVIEW);
        let (hypothesis_model_counts, hypothesis_model_kind_count) =
            public_count_map_preview(public_count_map(&value.hypothesis_model_counts), PUBLIC_REPORT_MAP_PREVIEW);
        let (verdict_counts, verdict_kind_count) =
            public_count_map_preview(public_verdict_counts(&value.verdict_counts), PUBLIC_REPORT_MAP_PREVIEW);
        let (escalated_segments, escalated_segment_count) =
            public_tokens_preview(&value.escalated_segments, PUBLIC_REPORT_LIST_PREVIEW);
        Self {
            audio_file_label: public_file_label(&value.audio_path, ""),
            chunk_count: public_wire_count(value.chunk_count),
            total_duration_ms: public_wire_i64(value.total_duration_ms),
            source_references: value
                .source_references
                .iter()
                .take(PUBLIC_REPORT_LIST_PREVIEW)
                .map(AgentSourceReferenceV1::from)
                .collect(),
            source_reference_count: public_wire_count(value.source_references.len()),
            source_reference_coverage: AgentSourceReferenceCoverageV1::from(&value.source_reference_coverage),
            hypothesis_model_counts,
            hypothesis_model_kind_count,
            verdict_counts,
            verdict_kind_count,
            training_ready_segments: public_wire_count(value.training_ready_segments),
            escalated_segments,
            escalated_segment_count,
            promotion_status: public_stage_status(&value.promotion_status),
            promotion_blocker_codes,
            promotion_blocker_count,
        }
    }
}

impl From<&crate::runs::AgentOrchestrationStage> for AgentOrchestrationStageV1 {
    fn from(value: &crate::runs::AgentOrchestrationStage) -> Self {
        let status = public_stage_status(&value.status);
        Self {
            stage: public_stage_code(&value.stage),
            status,
            detail_code: public_detail_code(status),
            blocker_count: public_wire_count(value.blocker_count.max(value.blockers.len())),
        }
    }
}

impl From<&crate::runs::AgentImportSummary> for AgentImportSummaryV1 {
    fn from(value: &crate::runs::AgentImportSummary) -> Self {
        let (required_source_reference_models, required_source_reference_model_count) =
            public_tokens_preview(&value.required_source_reference_models, PUBLIC_REPORT_MODEL_PREVIEW);
        let (source_reference_models, source_reference_model_count) =
            public_tokens_preview(&value.source_reference_models, PUBLIC_REPORT_MODEL_PREVIEW);
        let (hypothesis_models, hypothesis_model_count) =
            public_tokens_preview(&value.hypothesis_models, PUBLIC_REPORT_MODEL_PREVIEW);
        let (hypothesis_model_counts, hypothesis_model_kind_count) =
            public_count_map_preview(public_count_map(&value.hypothesis_model_counts), PUBLIC_REPORT_MAP_PREVIEW);
        let (verdict_counts, verdict_kind_count) =
            public_count_map_preview(public_verdict_counts(&value.verdict_counts), PUBLIC_REPORT_MAP_PREVIEW);
        let (escalated_segments, escalated_segment_count) =
            public_tokens_preview(&value.escalated_segments, PUBLIC_REPORT_LIST_PREVIEW);
        let (training_grade_reason_counts, training_grade_reason_kind_count) = public_count_map_preview(
            public_training_grade_reason_counts(&value.training_grade_reason_counts),
            PUBLIC_REPORT_MAP_PREVIEW,
        );
        Self {
            total_segments: public_wire_count(value.total_segments),
            agentic_readiness: value.agentic_readiness.as_ref().and_then(public_readiness),
            source_references: value
                .source_references
                .iter()
                .take(PUBLIC_REPORT_LIST_PREVIEW)
                .map(AgentSourceReferenceV1::from)
                .collect(),
            source_reference_count: public_wire_count(value.source_references.len()),
            source_reference_required: value.source_reference_required,
            required_source_reference_models,
            required_source_reference_model_count,
            source_reference_models,
            source_reference_model_count,
            source_reference_coverage: value
                .source_reference_coverage
                .iter()
                .take(PUBLIC_REPORT_LIST_PREVIEW)
                .map(AgentSourceReferenceCoverageV1::from)
                .collect(),
            source_reference_coverage_count: public_wire_count(value.source_reference_coverage.len()),
            long_file_dossiers: value
                .long_file_dossiers
                .iter()
                .take(PUBLIC_REPORT_LIST_PREVIEW)
                .map(AgentLongFileDossierV1::from)
                .collect(),
            long_file_dossier_count: public_wire_count(value.long_file_dossiers.len()),
            hypothesis_models,
            hypothesis_model_count,
            hypothesis_model_counts,
            hypothesis_model_kind_count,
            verdict_counts,
            verdict_kind_count,
            escalated_segments,
            escalated_segment_count,
            training_grade_summary: crate::quality::TrainingGradeSummary {
                total_segments: public_wire_count(value.training_grade_summary.total_segments),
                training_ready_segments: public_wire_count(value.training_grade_summary.training_ready_segments),
                gold_segments: public_wire_count(value.training_grade_summary.gold_segments),
                silver_segments: public_wire_count(value.training_grade_summary.silver_segments),
                review_segments: public_wire_count(value.training_grade_summary.review_segments),
                rejected_segments: public_wire_count(value.training_grade_summary.rejected_segments),
            },
            training_grade_reason_counts,
            training_grade_reason_kind_count,
            hypothesis_coverage_blockers: value
                .hypothesis_coverage_blockers
                .iter()
                .take(PUBLIC_REPORT_LIST_PREVIEW)
                .map(AgentHypothesisCoverageBlockerV1::from)
                .collect(),
            hypothesis_coverage_blocker_count: public_wire_count(value.hypothesis_coverage_blockers.len()),
            orchestration_stages: value
                .orchestration_stages
                .iter()
                .take(PUBLIC_REPORT_LIST_PREVIEW)
                .map(AgentOrchestrationStageV1::from)
                .collect(),
            orchestration_stage_count: public_wire_count(value.orchestration_stages.len()),
        }
    }
}

impl From<&crate::runs::AgentImportReport> for AgentImportReportV1 {
    fn from(value: &crate::runs::AgentImportReport) -> Self {
        let status = public_stage_status(&value.status);
        Self {
            id: public_token(&value.id, "unknown"),
            agent_run_id: value.agent_run_id.as_deref().map(|id| public_token(id, "unknown")),
            source: public_import_source(&value.source),
            status,
            summary: AgentImportSummaryV1::from(&value.summary),
            error_code: (value.error.is_some() || status == AgentStageStatusV1::Failed)
                .then_some(AgentImportErrorCodeV1::ImportReportFailed),
            created_at: public_timestamp(&value.created_at),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ReviewScope {
    Pending,
    Escalation,
    Search { query: String },
    VoiceFocus { focus_id: String },
}

/// Renderer-safe discovery result for the currently active file-owned voice focus. The identifier
/// is an opaque digest of the exact semantic allow-list; private voice names, ids and paths stay in
/// the owner data directory.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveVoiceFocusV1 {
    pub focus_id: String,
    pub segment_count: i64,
}

/// One compare-and-set metadata edit. `expected` is the exact last server value observed by the
/// renderer; `value` is the requested replacement. Keeping the two fields explicit makes clearing a
/// nullable value distinguishable from omitting that field entirely.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(tag = "field", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SegmentMetadataChangeV1 {
    SpeakerId { expected: Option<String>, value: Option<String> },
    AlignmentJson { expected: Option<String>, value: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSegmentMetadataRequestV1 {
    pub segment_id: String,
    pub changes: Vec<SegmentMetadataChangeV1>,
}

/// Server truth after an atomic metadata compare-and-set. Returning both fields lets the renderer
/// rebase its next edit without trusting its pre-save row or performing a second read.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdatedSegmentMetadataV1 {
    pub segment_id: String,
    pub speaker_id: Option<String>,
    pub alignment_json: Option<String>,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSegmentsRequestV1 {
    pub ids: Vec<String>,
}

/// Idempotent deletion outcome. A response-loss replay may report zero newly deleted rows while
/// still proving the requested final state: every requested id is absent.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeletedSegmentsV1 {
    pub requested_count: usize,
    pub deleted_count: usize,
}

/// One exact speaker group from the library. `None` is the SQL NULL/unassigned group and remains
/// distinct from a literal speaker id such as `"unknown"`.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerInventoryItemV1 {
    pub speaker_id: Option<String>,
    pub segment_count: usize,
    pub total_duration_seconds: f64,
}

/// Compare-and-set request for a whole speaker group. The two expected counts bind the destructive
/// merge confirmation to the exact source and target inventory the renderer displayed.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenameSpeakerRequestV1 {
    pub source_speaker_id: Option<String>,
    pub target_speaker_id: String,
    pub expected_source_count: usize,
    pub expected_target_count: usize,
}

/// Server-confirmed result of one atomic speaker rename or merge.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenamedSpeakerV1 {
    pub source_speaker_id: Option<String>,
    pub target_speaker_id: String,
    pub renamed_count: usize,
    pub target_count: usize,
    pub merged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssignSpeakersRequestV1 {
    pub ids: Vec<String>,
    pub target_speaker_id: Option<String>,
}

/// All-or-nothing batch speaker assignment result. `unchanged_count` makes an exact replay honest
/// without rewriting timestamps or review revisions for rows already at the requested value.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssignedSpeakersV1 {
    pub requested_count: usize,
    pub changed_count: usize,
    pub unchanged_count: usize,
}

/// Stable action identity for global machine/source history. This is intentionally an enum rather
/// than backend-authored display text so every locale owns its own copy and unknown future variants
/// fail at the generated TypeScript boundary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HistoryActionV1 {
    UpdateSegment,
    DeleteSegments,
    BatchTranscribe,
    BatchNormalize,
    SpeakerAssignment,
}

impl From<crate::history::HistoryAction> for HistoryActionV1 {
    fn from(action: crate::history::HistoryAction) -> Self {
        match action {
            crate::history::HistoryAction::UpdateSegment => Self::UpdateSegment,
            crate::history::HistoryAction::DeleteSegments => Self::DeleteSegments,
            crate::history::HistoryAction::BatchTranscribe => Self::BatchTranscribe,
            crate::history::HistoryAction::BatchNormalize => Self::BatchNormalize,
            crate::history::HistoryAction::SpeakerAssignment => Self::SpeakerAssignment,
        }
    }
}

/// One coherent read of both stacks. Two separate boolean calls could describe different moments
/// if a mutation landed between them; this snapshot cannot.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStatusV1 {
    pub undo_action: Option<HistoryActionV1>,
    pub redo_action: Option<HistoryActionV1>,
}

/// Server-confirmed history transition and the exact post-transition stack state. `action = None`
/// is an honest empty-stack no-op, never an ambiguous English fallback.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryMutationResultV1 {
    pub action: Option<HistoryActionV1>,
    pub status: HistoryStatusV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItemV1 {
    pub segment: crate::db::SpeechSegment,
    pub base_revision: i64,
    pub eligible: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPageV1 {
    pub items: Vec<ReviewItemV1>,
    pub total: i64,
    pub next_cursor: Option<String>,
    pub scope_label: String,
    pub focus_narrowed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecisionV1 {
    Accept,
    Edit,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommitReviewRequestV1 {
    pub operation_id: String,
    pub segment_id: String,
    pub base_revision: i64,
    pub decision: ReviewDecisionV1,
    pub transcript: Option<String>,
    pub reason_code: Option<String>,
    pub playback_receipt_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommittedReviewV1 {
    pub segment_id: String,
    pub committed_revision: i64,
    pub authoritative_transcript: String,
    pub decision_id: String,
}

/// Exact compare-and-swap authority for a generic owner review flag. The revision is part of the
/// idempotency payload: reusing an operation UUID with a different revision is a conflict, while an
/// exact response-loss retry can return the original immutable effect.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordReviewFlagRequestV1 {
    pub operation_id: String,
    pub segment_id: String,
    pub base_revision: i64,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecordedReviewFlagV1 {
    pub effect_event_id: i64,
    pub segment_id: String,
    pub prior_revision: i64,
    pub flag_revision: i64,
    pub segment: crate::db::SpeechSegment,
}

/// A closed technical classification, never a human transcript decision. Wire spellings are stable
/// camelCase reason codes so audit/export policy does not depend on localized prose.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TechnicalUnusableReasonV1 {
    DecodeFailed,
    MissingFile,
    PermissionDenied,
    CorruptContainer,
}

impl TechnicalUnusableReasonV1 {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::DecodeFailed => "decodeFailed",
            Self::MissingFile => "missingFile",
            Self::PermissionDenied => "permissionDenied",
            Self::CorruptContainer => "corruptContainer",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MarkSegmentUnusableRequestV1 {
    pub operation_id: String,
    pub segment_id: String,
    pub base_revision: i64,
    pub reason: TechnicalUnusableReasonV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MarkedSegmentUnusableV1 {
    pub segment_id: String,
    pub committed_revision: i64,
    pub reason: TechnicalUnusableReasonV1,
    pub effect_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackIntervalV1 {
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPlaybackSessionV1 {
    pub playback_receipt_id: String,
    pub segment_id: String,
    pub segment_revision: i64,
    pub clip_duration_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPlaybackReceiptV1 {
    pub playback_receipt_id: String,
    pub segment_id: String,
    pub segment_revision: i64,
    pub unique_played_ms: i64,
    pub clip_duration_ms: i64,
    pub coverage_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OperationEventV1 {
    Started { operation_id: String },
    Progress { operation_id: String, completed: u64, total: u64 },
    Completed { operation_id: String },
    Failed { operation_id: String, error: CommandErrorV1 },
    Cancelled { operation_id: String },
    Halted { operation_id: String, error: CommandErrorV1 },
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDraftV1 {
    pub segment_id: String,
    pub base_revision: i64,
    pub text: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(untagged)]
pub enum SettingValueV1 {
    String(String),
    Number(f64),
    Boolean(bool),
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatchV1 {
    pub expected_settings_revision: i64,
    pub changed_fields: BTreeMap<String, SettingValueV1>,
}

/// Renderer-safe settings snapshot. This deliberately omits API-key values and the app's internal
/// data/model/output paths. The revision is an opaque server-owned compare-and-swap token; the
/// renderer must never synthesize it from these fields.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
pub struct RendererSettingsV1 {
    pub asr_model_size: String,
    pub use_finetuned_asr: bool,
    pub vad_threshold: f32,
    pub min_segment_duration_ms: u32,
    pub max_segment_duration_ms: u32,
    pub num_asr_threads: u32,
    pub enable_gpu: bool,
    pub language: String,
    pub export_format: String,
    pub auto_normalize: bool,
    pub verbalize_numbers: bool,
    pub auto_align: bool,
    pub assign_speaker_from_filename: bool,
    pub enable_diarization: bool,
    pub enable_denoising: bool,
    pub autoplay_segments: bool,
    pub max_speakers: u32,
    pub max_wer_threshold: f64,
    pub max_cer_threshold: f64,
    pub enforce_quality_gates: bool,
    pub theme: String,
    pub llm_mode: String,
    pub llm_endpoint: String,
    pub llm_api_key_configured: bool,
    pub cloud_llm_opt_in: bool,
    pub llm_system_prompt: String,
    pub llm_model: String,
    pub external_asr_script_path: String,
    pub hf_train_ratio: f64,
    pub hf_val_ratio: f64,
    pub hf_test_ratio: f64,
    pub hf_split_seed: u64,
    pub hf_speaker_disjoint: bool,
    pub hf_license: String,
    pub jury_cloud_opt_in: bool,
    pub jury_model: String,
    pub jury_provider: String,
    pub source_reference_models: Vec<String>,
    pub jury_self_consistency_n: u32,
    pub jury_autonomy_level: String,
    pub jury_t1_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshotV1 {
    pub settings_revision: i64,
    pub settings: RendererSettingsV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatchResultV1 {
    pub settings_revision: i64,
    pub settings: RendererSettingsV1,
    pub already_applied: bool,
}

/// Consent remains an explicit privacy transaction instead of an ordinary preference field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CloudConsentKindV1 {
    Llm,
    Jury,
}

/// Closed provider selector for the explicit secret mutation command. Unknown strings never reach
/// the secret store, and the key value itself is never returned by any public DTO.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ApiKeyProviderV1 {
    Gemini,
    Openrouter,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SetCloudConsentRequestV1 {
    pub expected_settings_revision: i64,
    pub consent: CloudConsentKindV1,
    pub granted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProofGateResultV1 {
    pub gate_id: String,
    pub status: String,
    pub artifact_hashes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProofRunManifestV1 {
    pub full_git_sha: String,
    pub profile: String,
    pub environment: BTreeMap<String, String>,
    pub gate_registry_hash: String,
    pub results: Vec<ProofGateResultV1>,
    pub logs: BTreeMap<String, String>,
    pub artifact_hashes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProductAttestationV1 {
    pub proof_manifest_sha256: String,
    pub executable_sha256: String,
    pub installer_sha256: Option<String>,
    pub database_schema: i64,
    pub known_defect_digest: String,
    pub release_environment: String,
    pub model_attestation_sha256: Option<String>,
}

/// Minimal complete view-state returned to the renderer after crash/session recovery. Internal
/// versioning, timestamps and reserved panel fields stay backend-owned rather than becoming a
/// permanently optional public contract through `SessionState`'s compatibility defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct SessionStateV1 {
    pub search_query: String,
    pub sort_order: String,
    pub selected_segment_id: Option<String>,
    pub filter_verified: Option<bool>,
    pub segment_count: usize,
    pub verified_count: usize,
}

impl From<crate::session::SessionState> for SessionStateV1 {
    fn from(value: crate::session::SessionState) -> Self {
        Self {
            search_query: value.search_query,
            sort_order: value.sort_order,
            selected_segment_id: value.selected_segment_id,
            filter_verified: value.filter_verified,
            segment_count: value.segment_count,
            verified_count: value.verified_count,
        }
    }
}

/// One registry drives both the typed command metadata and every standalone public contract. Keep
/// generation separate from application startup so a release build never mutates its source tree.
pub fn specta_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            crate::commands::get_review_compensation_overview_v1,
            crate::commands::record_review_compensation_settlement_v1,
            crate::commands::get_active_voice_focus_v1,
            crate::commands::get_review_page_v1,
            crate::commands::commit_review_v1,
            crate::commands::mark_segment_unusable_v1,
            crate::commands::record_review_flag,
            crate::commands::begin_desktop_playback_session_v1,
            crate::commands::cancel_desktop_playback_session_v1,
            crate::commands::finalize_desktop_playback_session_v1,
            crate::commands::get_review_draft_v1,
            crate::commands::reserve_review_draft_write_v1,
            crate::commands::save_review_draft_v1,
            crate::commands::delete_review_draft_v1,
            crate::commands::get_desktop_review_undo_target_v1,
            crate::commands::undo_desktop_review_action_v1,
            crate::commands::get_settings_v1,
            crate::commands::patch_settings_v1,
            crate::commands::set_cloud_consent_v1,
            crate::commands::get_configured_providers,
            crate::commands::set_api_key,
            crate::commands::undo,
            crate::commands::redo,
            crate::commands::get_history_status_v1,
            crate::commands::normalize_text,
            crate::commands::compute_diff,
            crate::commands::get_tracing_stats,
            crate::commands::get_recent_spans,
            crate::commands::clear_tracing_spans,
            crate::commands::save_session,
            crate::commands::restore_session,
            crate::commands::get_inference_stats,
            crate::commands::get_fingerprint_count,
            crate::commands::cancel_operation,
            crate::commands::cancel_wsl_refinement,
            crate::commands::app_health,
            crate::commands::take_last_crash,
            crate::commands::app_git_sha,
            crate::commands::register_media_asset,
            crate::commands::register_review_media_asset,
            crate::commands::get_media_asset_url,
            crate::commands::get_segment,
            crate::commands::get_segments_page,
            crate::commands::get_segment_ids_for_view,
            crate::commands::get_signal_anomaly_segments,
            crate::commands::get_dataset_stats,
            crate::commands::get_dataset_quality,
            crate::commands::get_training_grade_breakdown,
            crate::commands::get_dataset_certificate,
            crate::commands::get_label_quality_lift,
            crate::commands::get_jobs,
            crate::commands::models_status,
            crate::commands::models_download_all,
            crate::commands::get_champion_engine_status,
            crate::commands::start_champion_engine,
            crate::commands::list_agent_import_reports,
            crate::commands::get_agent_import_report_by_run_id,
            crate::commands::list_agent_stage_events,
            crate::commands::check_agentic_readiness,
            crate::commands::list_model_versions,
            crate::commands::import_model_checkpoint,
            crate::commands::import_model_deployment,
            crate::commands::bootstrap_legacy_champion,
            crate::commands::get_speaker_inventory_v1,
            crate::commands::update_segment_metadata_v1,
            crate::commands::delete_segments_v1,
            crate::commands::rename_speaker_v1,
            crate::commands::assign_speakers_v1,
            crate::commands::db_backup,
            crate::commands::db_restore,
            crate::commands::db_vacuum,
            crate::commands::get_quarantine_notice,
            crate::commands::acknowledge_quarantine,
            crate::commands::list_db_snapshots,
            crate::commands::restore_db_from_snapshot,
            crate::commands::open_audio_file,
            crate::commands::import_directory,
            crate::commands::import_audio_file,
            crate::commands::transcribe_segment,
            crate::commands::align_segment,
            crate::commands::get_segment_consensus,
            crate::commands::get_waveform,
            crate::commands::export_dataset,
            crate::commands::export_transcript,
            crate::commands::get_audio_health,
            crate::commands::relink_audio,
            crate::commands::validate_dataset_cmd,
            crate::commands::export_audio,
            crate::commands::merge_dataset_json,
            crate::commands::export_huggingface_dataset,
            crate::commands::create_gold_from_file,
            crate::commands::import_verified_segments_as_gold,
            crate::commands::export_gold_eval_set,
            crate::commands::export_finetune_pack,
            crate::commands::build_scorecard,
            crate::commands::compute_signal_anomaly_scores,
            crate::commands::get_active_learning_queue,
            crate::commands::get_escalation_queue,
            crate::commands::get_escalation_rate_trend,
            crate::commands::get_intelligence_report,
            crate::commands::list_eval_runs,
            crate::commands::run_gold_eval_asr,
            crate::commands::run_jury_pipeline,
            crate::commands::run_t2_for_segment,
            crate::commands::run_wsl_refinement,
            crate::commands::rediarize_segments,
            crate::commands::get_interrupted_import,
            crate::commands::get_import_run_status,
            crate::commands::get_batch_run_status,
            crate::commands::get_active_batch_run,
            crate::commands::acknowledge_batch_run,
            crate::commands::batch_transcribe,
            crate::commands::batch_normalize,
            crate::commands::discard_interrupted_import,
            crate::commands::resume_interrupted_import
        ])
        .typed_error_impl(
            r#"async function typedError<T, E>(result: Promise<T>): Promise<{ status: "ok"; data: T } | { status: "error"; error: E }> {
    try {
        return { status: "ok", data: await result };
    } catch (error: unknown) {
        if (error instanceof Error) throw error;
        return { status: "error", error: error as E };
    }
}"#,
        )
        .typ::<CommandErrorDetailV1>()
        .typ::<SuggestedActionV1>()
        .typ::<BatchOperationV1>()
        .typ::<BatchRunStatusV1>()
        .typ::<BatchRunDispositionV1>()
        .typ::<BatchRunOutcomeV1>()
        .typ::<BatchRunStatusResponseV1>()
        .typ::<BatchStartStatusV1>()
        .typ::<BatchStartedV1>()
        .typ::<TracingStatsV1>()
        .typ::<TracingSpanV1>()
        .typ::<InferenceKindStatsV1>()
        .typ::<InferenceStatsV1>()
        .typ::<SessionStateV1>()
        .typ::<AppHealthV1>()
        .typ::<crate::media::MediaGrant>()
        .typ::<crate::db::SegmentsPage>()
        .typ::<crate::stats::DatasetStats>()
        .typ::<crate::quality::DatasetQuality>()
        .typ::<crate::quality::TrainingGradeBreakdown>()
        .typ::<crate::quality::conformal::ConformalCertificate>()
        .typ::<crate::eval::LabelQualityLift>()
        .typ::<crate::commands::JobStateV1>()
        .typ::<crate::commands::JobV1>()
        .typ::<crate::models::ModelArtifactSourceV1>()
        .typ::<crate::models::ModelStatusEntryV1>()
        .typ::<crate::commands::ModelDownloadSummaryV1>()
        .typ::<crate::commands::EngineStatusV1>()
        .typ::<PipelineProgressV1>()
        .typ::<AgentStageProgressV1>()
        .typ::<AgentStageEventV1>()
        .typ::<AgenticReadinessV1>()
        .typ::<AgentImportReportV1>()
        .typ::<crate::commands::ModelVersionSummaryV1>()
        .typ::<ReviewScope>()
        .typ::<ActiveVoiceFocusV1>()
        .typ::<SegmentMetadataChangeV1>()
        .typ::<UpdateSegmentMetadataRequestV1>()
        .typ::<UpdatedSegmentMetadataV1>()
        .typ::<DeleteSegmentsRequestV1>()
        .typ::<DeletedSegmentsV1>()
        .typ::<SpeakerInventoryItemV1>()
        .typ::<RenameSpeakerRequestV1>()
        .typ::<RenamedSpeakerV1>()
        .typ::<AssignSpeakersRequestV1>()
        .typ::<AssignedSpeakersV1>()
        .typ::<crate::commands::BackupVerificationV1>()
        .typ::<crate::commands::QuarantineNoticeV1>()
        .typ::<crate::commands::SnapshotInfoV1>()
        .typ::<ImportStartStatusV1>()
        .typ::<ImportSourceV1>()
        .typ::<DirectoryImportStartedV1>()
        .typ::<FileImportStartedV1>()
        .typ::<TranscribedSegmentV1>()
        .typ::<WordTimestampV1>()
        .typ::<ConsensusWordV1>()
        .typ::<SegmentConsensusV1>()
        .typ::<MergeDatasetResultV1>()
        .typ::<AudioExportFormatV1>()
        .typ::<AudioExportOptionsV1>()
        .typ::<AudioExportResultV1>()
        .typ::<crate::db::AudioHealth>()
        .typ::<crate::db::RelinkResult>()
        .typ::<crate::validation::ValidationReport>()
        .typ::<crate::validation::ValidationIssue>()
        .typ::<crate::validation::IssueSeverity>()
        .typ::<crate::validation::IssueCategory>()
        .typ::<crate::eval::GoldEvalExport>()
        .typ::<crate::eval::FinetunePackResult>()
        .typ::<EvalRunV1>()
        .typ::<EvalSegmentResultV1>()
        .typ::<EvalRunResultV1>()
        .typ::<Loop0ShadowV1>()
        .typ::<AutoAcceptPrecisionV1>()
        .typ::<ConformalCalibrationBucketV1>()
        .typ::<ConformalCalibrationProgressV1>()
        .typ::<IntelligenceReportV1>()
        .typ::<EscalationTrendPointV1>()
        .typ::<JuryPipelineModeV1>()
        .typ::<JuryPipelineNotRequiredV1>()
        .typ::<JuryPipelineCompletedV1>()
        .typ::<JuryPipelineReportV1>()
        .typ::<T2EvidenceV1>()
        .typ::<T2VerdictV1>()
        .typ::<T2ResultV1>()
        .typ::<WslRefinementStartStatusV1>()
        .typ::<WslRefinementStartedV1>()
        .typ::<crate::commands::ScorecardResponse>()
        .typ::<ConfidenceIntervalV1>()
        .typ::<SystemScoreV1>()
        .typ::<BaselineComparisonV1>()
        .typ::<ScorecardWithBaselineV1>()
        .typ::<ScorecardWithoutBaselineV1>()
        .typ::<ScorecardV1>()
        .typ::<crate::commands::ImportJobV1>()
        .typ::<crate::commands::ImportResumeStatusV1>()
        .typ::<crate::commands::ImportResumeV1>()
        .typ::<HistoryActionV1>()
        .typ::<HistoryStatusV1>()
        .typ::<HistoryMutationResultV1>()
        .typ::<ReviewItemV1>()
        .typ::<ReviewPageV1>()
        .typ::<ReviewDecisionV1>()
        .typ::<RecordReviewFlagRequestV1>()
        .typ::<RecordedReviewFlagV1>()
        .typ::<DesktopHumanDecisionV1>()
        .typ::<DesktopReviewFlagKindV1>()
        .typ::<DesktopReviewUndoTargetV1>()
        .typ::<DesktopReviewUndoBlockReasonV1>()
        .typ::<DesktopReviewUndoAvailabilityV1>()
        .typ::<UndoDesktopReviewRequestV1>()
        .typ::<DesktopReviewUndoEffectKindV1>()
        .typ::<DesktopReviewUndoOutcomeV1>()
        .typ::<TechnicalUnusableReasonV1>()
        .typ::<PlaybackIntervalV1>()
        .typ::<DesktopPlaybackSessionV1>()
        .typ::<DesktopPlaybackReceiptV1>()
        .typ::<OperationEventV1>()
        .typ::<ReviewDraftV1>()
        .typ::<SettingValueV1>()
        .typ::<SettingsPatchV1>()
        .typ::<RendererSettingsV1>()
        .typ::<SettingsSnapshotV1>()
        .typ::<SettingsPatchResultV1>()
        .typ::<CloudConsentKindV1>()
        .typ::<ApiKeyProviderV1>()
        .typ::<SetCloudConsentRequestV1>()
        .typ::<ProofGateResultV1>()
        .typ::<ProofRunManifestV1>()
        .typ::<ProductAttestationV1>()
}

pub fn export_typescript_bindings(path: impl AsRef<std::path::Path>) -> Result<(), String> {
    let path = path.as_ref();
    specta_builder().export(specta_typescript::Typescript::default(), path).map_err(|error| error.to_string())?;
    let generated = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let normalized = generated.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    std::fs::write(path, format!("{}\n", normalized.trim_end())).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_error_wire_shape_is_versioned_camel_case_and_scrubbed() {
        let error = CommandErrorV1::new("STALE_REVISION", "This clip changed; reload it.", false)
            .operation("3f2b32c8-8a17-4d2d-ae5c-d3f4db4927af")
            .suggested(SuggestedActionV1::ReloadClip)
            .detail("expectedRevision", 4_i64)
            .detail("currentRevision", 5_i64);
        let json = serde_json::to_value(error).expect("serialize public error");
        assert_eq!(json["schema"], 1);
        assert_eq!(json["suggestedAction"], "reloadClip");
        assert!(json.get("operationId").is_some());
        assert!(json.get("sql").is_none());
        assert!(json.to_string().find("C:\\").is_none());
    }

    #[test]
    fn operation_event_is_a_stable_discriminated_union() {
        let event = OperationEventV1::Progress { operation_id: "op".to_string(), completed: 2, total: 10 };
        let json = serde_json::to_value(event).expect("serialize operation event");
        assert_eq!(json["type"], "progress");
        assert_eq!(json["operationId"], "op");
        assert_eq!(json["completed"], 2);
    }

    #[test]
    fn voice_focus_wire_contract_is_opaque_and_camel_case() {
        let focus_id = format!("vf1_{}", "a".repeat(64));
        let active = serde_json::to_value(ActiveVoiceFocusV1 { focus_id: focus_id.clone(), segment_count: 42 })
            .expect("serialize active focus");
        assert_eq!(active, serde_json::json!({ "focusId": focus_id.clone(), "segmentCount": 42 }));
        assert!(active.get("name").is_none());
        assert!(active.get("segmentIds").is_none());
        assert!(active.get("path").is_none());

        let scope = serde_json::to_value(ReviewScope::VoiceFocus { focus_id }).expect("serialize exact focus scope");
        assert_eq!(scope["kind"], "voiceFocus");
        assert!(scope.get("focusId").is_some());
    }

    #[test]
    fn session_wire_contract_is_complete_and_omits_internal_recovery_fields() {
        let wire = serde_json::to_value(SessionStateV1 {
            search_query: "query".into(),
            sort_order: "newest".into(),
            selected_segment_id: Some("segment-a".into()),
            filter_verified: Some(false),
            segment_count: 12,
            verified_count: 3,
        })
        .expect("serialize session DTO");
        assert_eq!(wire["search_query"], "query");
        assert_eq!(wire["segment_count"], 12);
        assert!(wire.get("version").is_none());
        assert!(wire.get("last_saved").is_none());
        assert!(wire.get("view_mode").is_none());
    }

    #[test]
    fn technical_unusable_reason_wire_values_are_closed_and_camel_case() {
        for (reason, expected) in [
            (TechnicalUnusableReasonV1::DecodeFailed, "decodeFailed"),
            (TechnicalUnusableReasonV1::MissingFile, "missingFile"),
            (TechnicalUnusableReasonV1::PermissionDenied, "permissionDenied"),
            (TechnicalUnusableReasonV1::CorruptContainer, "corruptContainer"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), expected);
            assert_eq!(reason.as_code(), expected);
        }
        assert!(serde_json::from_str::<TechnicalUnusableReasonV1>(r#""networkError""#).is_err());
        assert!(serde_json::from_str::<TechnicalUnusableReasonV1>(r#""decode_failed""#).is_err());
    }

    #[test]
    fn stage_dtos_never_forward_private_file_ancestry_or_detail() {
        let private = crate::runs::AgentStageEvent {
            id: 8,
            run_id: "run-safe".into(),
            source: "directory".into(),
            stage: "jury_adjudication".into(),
            status: "blocked".into(),
            file: r"D:\private\Wareen\source.wav".into(),
            detail: r"SQL failed at D:\private\cortex-speech.db token=secret".into(),
            current: 2,
            total: 4,
            created_at: "2026-08-28 14:15:16".into(),
        };
        let persisted = serde_json::to_value(AgentStageEventV1::from(&private)).expect("serialize stage DTO");
        let live = serde_json::to_value(public_agent_stage_progress(
            &private.run_id,
            &private.stage,
            &private.status,
            &private.file,
            private.current,
            private.total,
        ))
        .expect("serialize live stage DTO");

        assert_eq!(persisted["fileLabel"], "source.wav");
        assert_eq!(persisted["detailCode"], "blocked");
        assert_eq!(live["stage"], "jury_adjudication");
        assert_eq!(live["runId"], "run-safe");
        assert!(persisted.get("detail").is_none());
        assert!(live.get("detail").is_none());
        let wire = format!("{persisted}{live}");
        for forbidden in ["D:\\", "private", "Wareen", "SQL", "token", "secret", "cortex-speech.db"] {
            assert!(!wire.contains(forbidden), "renderer wire leaked {forbidden}: {wire}");
        }

        let bounded = public_agent_stage_progress("run-safe", "unknown", "unknown", "x.wav", usize::MAX, usize::MAX);
        assert_eq!(bounded.current, u32::MAX as usize);
        assert_eq!(bounded.total, u32::MAX as usize);
    }

    #[test]
    fn public_file_labels_strip_unicode_bidi_formatting_controls() {
        let hostile = format!(r"D:\private\safe{}gnp.exe.wav", '\u{202e}');
        let label = public_file_label(&hostile, "audio item");
        assert_eq!(label, "safegnp.exe.wav");
        assert!(!label.chars().any(|character| matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')));
    }

    #[test]
    fn pipeline_progress_is_closed_bounded_and_filename_safe() {
        let hostile = format!(r"D:\private\safe{}gnp.exe.wav", '\u{202e}');
        let public = public_pipeline_progress(
            "run-safe",
            usize::MAX,
            usize::MAX,
            &hostile,
            r"Transcribing chunk 1/2 D:\private\token=secret",
        );
        let wire = serde_json::to_value(public).expect("serialize progress DTO");

        assert_eq!(wire["runId"], "run-safe");
        assert_eq!(wire["fileLabel"], "safegnp.exe.wav");
        assert_eq!(wire["status"], "transcribing");
        assert_eq!(wire["current"], u32::MAX);
        assert_eq!(wire["total"], u32::MAX);
        let text = wire.to_string();
        for forbidden in ["D:\\", "private", "token", "secret", "202e"] {
            assert!(!text.contains(forbidden), "progress wire leaked {forbidden}: {text}");
        }
    }

    #[test]
    fn import_report_dto_omits_raw_diagnostics_paths_and_free_form_blockers() {
        let source_reference = crate::runs::AgentSourceReferenceSummary {
            audio_path: r"D:\private\Wareen\source.wav".into(),
            model_id: r"D:\private\model.bin".into(),
            audio_content_hash: Some(r"not-a-public-hash D:\private".into()),
            audio_size_bytes: Some(42),
            transcript_path: r"D:\private\Wareen\source.txt".into(),
            text_chars: 99,
        };
        let coverage = crate::runs::AgentSourceReferenceCoverage {
            audio_path: source_reference.audio_path.clone(),
            required_models: vec!["gemini-2.5-pro".into()],
            present_models: vec!["gemini-2.5-pro".into()],
            missing_models: Vec::new(),
            complete: true,
        };
        let blocker = crate::runs::AgentHypothesisCoverageBlocker {
            segment_id: "segment-safe".into(),
            grade: "silver".into(),
            training_ready: true,
            coverage: crate::quality::HypothesisCoverageReport {
                minimum_non_empty_model_count: 2,
                non_empty_model_count: 1,
                passes_minimum: false,
                non_empty_models: vec![r"D:\private\model.bin".into()],
                ignored_models: vec!["token=secret".into()],
            },
        };
        let dossier = crate::runs::AgentLongFileDossier {
            audio_path: source_reference.audio_path.clone(),
            chunk_count: 2,
            total_duration_ms: 1_000,
            source_references: vec![source_reference.clone()],
            source_reference_coverage: coverage.clone(),
            hypothesis_model_counts: BTreeMap::from([(r"D:\private\model.bin".into(), 2)]),
            verdict_counts: BTreeMap::from([("jury_accept".into(), 2), ("supersecret".into(), 3)]),
            training_ready_segments: 1,
            escalated_segments: vec!["segment-safe".into()],
            promotion_status: "blocked".into(),
            promotion_blockers: vec![r"missing_hypothesis_coverage:D:\private\segment-safe".into()],
        };
        let report = crate::runs::AgentImportReport {
            id: "report-safe".into(),
            agent_run_id: Some("run-safe".into()),
            source: "file".into(),
            status: "failed".into(),
            audio_paths: vec![source_reference.audio_path.clone()],
            segment_ids: vec!["segment-safe".into()],
            summary: crate::runs::AgentImportSummary {
                total_segments: 2,
                agentic_readiness: Some(serde_json::json!({
                    "status": "blocked",
                    "ready": false,
                    "sourceReferenceModels": [r"D:\private\model.bin"],
                    "availableHypothesisModels": ["omniasr-wsl-7b"],
                    "requiredHypothesisModels": 2,
                    "checks": [{
                        "id": "primary_asr",
                        "label": r"Private D:\private\model.bin",
                        "status": "blocked",
                        "detail": "SQL token=secret"
                    }]
                })),
                source_references: vec![source_reference],
                source_reference_required: true,
                required_source_reference_models: vec!["gemini-2.5-pro".into()],
                source_reference_models: vec!["gemini-2.5-pro".into()],
                source_reference_coverage: vec![coverage],
                long_file_dossiers: vec![dossier],
                hypothesis_models: vec!["omniasr-wsl-7b".into()],
                hypothesis_model_counts: BTreeMap::from([("omniasr-wsl-7b".into(), 2)]),
                verdict_counts: BTreeMap::from([("jury_accept".into(), 2), ("supersecret".into(), 3)]),
                escalated_segments: vec!["segment-safe".into()],
                training_grade_summary: crate::quality::TrainingGradeSummary::default(),
                training_grade_reason_counts: BTreeMap::from([
                    ("human_verified".into(), 1),
                    ("token=secret SELECT".into(), 7),
                    ("supersecret".into(), 4),
                ]),
                hypothesis_coverage_blockers: vec![blocker],
                orchestration_stages: vec![crate::runs::AgentOrchestrationStage {
                    stage: "dataset_promotion".into(),
                    status: "blocked".into(),
                    summary: r"SQL failed at D:\private\cortex-speech.db token=secret".into(),
                    blocker_count: 1,
                    blockers: vec![r"D:\private\Wareen\source.wav".into()],
                }],
            },
            jury_report: Some(serde_json::json!({ "private": r"D:\private", "token": "secret" })),
            error: Some(r"database error at D:\private\cortex-speech.db token=secret".into()),
            created_at: "2026-08-28 14:15:16".into(),
        };

        let public = serde_json::to_value(AgentImportReportV1::from(&report)).expect("serialize report DTO");
        assert_eq!(public["errorCode"], "IMPORT_REPORT_FAILED");
        assert_eq!(public["summary"]["sourceReferences"][0]["audioFileLabel"], "source.wav");
        assert_eq!(public["summary"]["sourceReferences"][0]["transcriptFileLabel"], "source.txt");
        assert_eq!(public["summary"]["orchestrationStages"][0]["detailCode"], "blocked");
        assert_eq!(public["summary"]["longFileDossiers"][0]["promotionBlockerCodes"][0], "missing_hypothesis_coverage");
        assert_eq!(public["summary"]["trainingGradeReasonCounts"]["unknown"], 11);
        assert_eq!(public["summary"]["verdictCounts"]["unknown"], 3);
        let readiness_check = &public["summary"]["agenticReadiness"]["checks"][0];
        assert!(readiness_check.get("label").is_none());
        assert!(readiness_check.get("detail").is_none());
        let orchestration = &public["summary"]["orchestrationStages"][0];
        assert!(orchestration.get("summary").is_none());
        assert!(orchestration.get("blockers").is_none());
        for omitted in ["audioPaths", "segmentIds", "juryReport", "error"] {
            assert!(public.get(omitted).is_none(), "unexpected raw top-level field {omitted}");
        }
        let wire = public.to_string();
        for forbidden in [
            "D:\\",
            "private",
            "Wareen",
            "SQL",
            "token",
            "secret",
            "supersecret",
            "cortex-speech.db",
            "promotionBlockers",
            "blockers",
            "label",
            "detail\"",
        ] {
            assert!(!wire.contains(forbidden), "renderer report leaked {forbidden}: {wire}");
        }
    }

    #[test]
    fn live_readiness_dto_drops_backend_authored_labels_and_details() {
        let private = crate::commands::AgenticReadiness {
            status: "blocked".into(),
            ready: false,
            source_reference_models: vec![r"D:\private\model.bin".into()],
            available_hypothesis_models: vec!["omniasr-wsl-7b".into()],
            required_hypothesis_models: 2,
            checks: vec![crate::commands::AgenticReadinessCheck {
                id: "primary_asr".into(),
                label: r"Private D:\private\model.bin".into(),
                status: "blocked".into(),
                detail: "SQL token=secret".into(),
            }],
        };
        let wire = serde_json::to_value(AgenticReadinessV1::from(&private)).expect("serialize readiness DTO");
        assert_eq!(wire["sourceReferenceModels"], serde_json::json!(["unknown"]));
        assert_eq!(wire["checks"][0], serde_json::json!({ "code": "primary_asr", "status": "blocked" }));
        let text = wire.to_string();
        for forbidden in ["D:\\", "private", "model.bin", "SQL", "token", "secret", "label", "detail"] {
            assert!(!text.contains(forbidden), "readiness wire leaked {forbidden}: {text}");
        }
    }

    #[test]
    fn renderer_report_numeric_fields_clamp_hostile_maxima() {
        let source_reference = crate::runs::AgentSourceReferenceSummary {
            audio_path: r"D:\private\maximum.wav".into(),
            model_id: "model-safe".into(),
            audio_content_hash: None,
            audio_size_bytes: Some(i64::MAX),
            transcript_path: r"D:\private\maximum.txt".into(),
            text_chars: usize::MAX,
        };
        let coverage = crate::runs::AgentSourceReferenceCoverage {
            audio_path: source_reference.audio_path.clone(),
            required_models: vec!["model-safe".into()],
            present_models: Vec::new(),
            missing_models: vec!["model-safe".into()],
            complete: false,
        };
        let coverage_blocker = crate::runs::AgentHypothesisCoverageBlocker {
            segment_id: "segment-safe".into(),
            grade: "review".into(),
            training_ready: false,
            coverage: crate::quality::HypothesisCoverageReport {
                minimum_non_empty_model_count: usize::MAX,
                non_empty_model_count: usize::MAX,
                passes_minimum: false,
                non_empty_models: Vec::new(),
                ignored_models: Vec::new(),
            },
        };
        let dossier = crate::runs::AgentLongFileDossier {
            audio_path: source_reference.audio_path.clone(),
            chunk_count: usize::MAX,
            total_duration_ms: i64::MAX,
            source_references: vec![source_reference.clone()],
            source_reference_coverage: coverage.clone(),
            hypothesis_model_counts: BTreeMap::from([
                ("model-safe".into(), usize::MAX),
                (r"D:\private\model.bin".into(), usize::MAX),
            ]),
            verdict_counts: BTreeMap::from([
                ("jury_accept".into(), usize::MAX),
                ("private-verdict".into(), usize::MAX),
            ]),
            training_ready_segments: usize::MAX,
            escalated_segments: vec!["segment-safe".into()],
            promotion_status: "blocked".into(),
            promotion_blockers: vec!["missing_hypothesis_coverage:segment-safe".into()],
        };
        let summary = crate::runs::AgentImportSummary {
            total_segments: usize::MAX,
            agentic_readiness: None,
            source_references: vec![source_reference],
            source_reference_required: true,
            required_source_reference_models: vec!["model-safe".into()],
            source_reference_models: vec!["model-safe".into()],
            source_reference_coverage: vec![coverage],
            long_file_dossiers: vec![dossier],
            hypothesis_models: vec!["model-safe".into()],
            hypothesis_model_counts: BTreeMap::from([("model-safe".into(), usize::MAX)]),
            verdict_counts: BTreeMap::from([("jury_accept".into(), usize::MAX)]),
            escalated_segments: vec!["segment-safe".into()],
            training_grade_summary: crate::quality::TrainingGradeSummary {
                total_segments: usize::MAX,
                training_ready_segments: usize::MAX,
                gold_segments: usize::MAX,
                silver_segments: usize::MAX,
                review_segments: usize::MAX,
                rejected_segments: usize::MAX,
            },
            training_grade_reason_counts: BTreeMap::from([("human_verified".into(), usize::MAX)]),
            hypothesis_coverage_blockers: vec![coverage_blocker],
            orchestration_stages: vec![crate::runs::AgentOrchestrationStage {
                stage: "dataset_promotion".into(),
                status: "blocked".into(),
                summary: String::new(),
                blocker_count: usize::MAX,
                blockers: Vec::new(),
            }],
        };

        let public = AgentImportSummaryV1::from(&summary);
        let public_count_max = u32::MAX as usize;
        assert_eq!(public.total_segments, public_count_max);
        assert!(public.hypothesis_model_counts.values().all(|count| *count <= public_count_max));
        assert!(public.verdict_counts.values().all(|count| *count <= public_count_max));
        assert!(public.training_grade_reason_counts.values().all(|count| *count <= public_count_max));
        assert_eq!(public.hypothesis_model_counts["model-safe"], public_count_max);
        assert_eq!(public.verdict_counts["jury_accept"], public_count_max);
        assert_eq!(public.training_grade_reason_counts["human_verified"], public_count_max);

        let grades = &public.training_grade_summary;
        for count in [
            grades.total_segments,
            grades.training_ready_segments,
            grades.gold_segments,
            grades.silver_segments,
            grades.review_segments,
            grades.rejected_segments,
        ] {
            assert_eq!(count, public_count_max);
        }

        let public_source = &public.source_references[0];
        assert_eq!(public_source.audio_size_bytes, Some(PUBLIC_JS_SAFE_INTEGER));
        assert_eq!(public_source.text_chars, public_count_max);
        let public_dossier = &public.long_file_dossiers[0];
        assert_eq!(public_dossier.chunk_count, public_count_max);
        assert_eq!(public_dossier.total_duration_ms, PUBLIC_JS_SAFE_INTEGER);
        assert_eq!(public_dossier.training_ready_segments, public_count_max);
        assert!(public_dossier.hypothesis_model_counts.values().all(|count| *count <= public_count_max));
        assert!(public_dossier.verdict_counts.values().all(|count| *count <= public_count_max));
        let public_coverage = &public.hypothesis_coverage_blockers[0].coverage;
        assert_eq!(public_coverage.minimum_non_empty_model_count, public_count_max);
        assert_eq!(public_coverage.non_empty_model_count, public_count_max);
        assert_eq!(public.orchestration_stages[0].blocker_count, public_count_max);

        let readiness = AgenticReadinessV1::from(&crate::commands::AgenticReadiness {
            status: "blocked".into(),
            ready: false,
            source_reference_models: Vec::new(),
            available_hypothesis_models: Vec::new(),
            required_hypothesis_models: usize::MAX,
            checks: Vec::new(),
        });
        assert_eq!(readiness.required_hypothesis_models, public_count_max);

        let stage_event = AgentStageEventV1::from(&crate::runs::AgentStageEvent {
            id: i64::MAX,
            run_id: "run-safe".into(),
            source: "file".into(),
            stage: "agent_report".into(),
            status: "completed".into(),
            file: "maximum.wav".into(),
            detail: String::new(),
            current: usize::MAX,
            total: usize::MAX,
            created_at: "2026-08-28 14:15:16".into(),
        });
        assert_eq!(stage_event.id, PUBLIC_JS_SAFE_INTEGER);
        assert_eq!(stage_event.current, public_count_max);
        assert_eq!(stage_event.total, public_count_max);
    }

    #[test]
    fn import_report_wire_is_bounded_while_totals_remain_exact() {
        const ITEMS: usize = 10_000;
        let models = (0..ITEMS).map(|index| format!("model-{index:05}")).collect::<Vec<_>>();
        let source_reference = crate::runs::AgentSourceReferenceSummary {
            audio_path: r"D:\private\large.wav".into(),
            model_id: "model-00000".into(),
            audio_content_hash: None,
            audio_size_bytes: Some(42),
            transcript_path: r"D:\private\large.txt".into(),
            text_chars: 24,
        };
        let small_coverage = crate::runs::AgentSourceReferenceCoverage {
            audio_path: source_reference.audio_path.clone(),
            required_models: vec!["model-00000".into()],
            present_models: vec!["model-00000".into()],
            missing_models: Vec::new(),
            complete: true,
        };
        let large_coverage = crate::runs::AgentSourceReferenceCoverage {
            audio_path: source_reference.audio_path.clone(),
            required_models: models.clone(),
            present_models: models.clone(),
            missing_models: models.clone(),
            complete: false,
        };
        let model_counts =
            (0..ITEMS).map(|index| (format!("model-{index:05}"), index.saturating_add(1))).collect::<BTreeMap<_, _>>();
        let small_dossier = crate::runs::AgentLongFileDossier {
            audio_path: source_reference.audio_path.clone(),
            chunk_count: 1,
            total_duration_ms: 1_000,
            source_references: vec![source_reference.clone()],
            source_reference_coverage: small_coverage.clone(),
            hypothesis_model_counts: BTreeMap::from([("model-00000".into(), 1)]),
            verdict_counts: BTreeMap::from([("jury_accept".into(), 1)]),
            training_ready_segments: 1,
            escalated_segments: Vec::new(),
            promotion_status: "ready".into(),
            promotion_blockers: Vec::new(),
        };
        let mut dossiers = vec![small_dossier; ITEMS];
        dossiers[0] = crate::runs::AgentLongFileDossier {
            audio_path: source_reference.audio_path.clone(),
            chunk_count: ITEMS,
            total_duration_ms: 1_000,
            source_references: vec![source_reference.clone(); ITEMS],
            source_reference_coverage: large_coverage,
            hypothesis_model_counts: model_counts.clone(),
            verdict_counts: BTreeMap::from([("jury_accept".into(), ITEMS)]),
            training_ready_segments: ITEMS,
            escalated_segments: models.clone(),
            promotion_status: "blocked".into(),
            promotion_blockers: vec!["missing_hypothesis_coverage:private".into(); ITEMS],
        };
        let checks =
            (0..ITEMS).map(|_| serde_json::json!({ "id": "primary_asr", "status": "ready" })).collect::<Vec<_>>();
        let report = crate::runs::AgentImportReport {
            id: "large-report".into(),
            agent_run_id: Some("large-run".into()),
            source: "file".into(),
            status: "completed".into(),
            audio_paths: Vec::new(),
            segment_ids: Vec::new(),
            summary: crate::runs::AgentImportSummary {
                total_segments: ITEMS,
                agentic_readiness: Some(serde_json::json!({
                    "status": "ready",
                    "ready": true,
                    "sourceReferenceModels": models,
                    "availableHypothesisModels": models,
                    "requiredHypothesisModels": ITEMS,
                    "checks": checks,
                })),
                source_references: vec![source_reference; ITEMS],
                source_reference_required: true,
                required_source_reference_models: models.clone(),
                source_reference_models: models.clone(),
                source_reference_coverage: vec![small_coverage; ITEMS],
                long_file_dossiers: dossiers,
                hypothesis_models: models.clone(),
                hypothesis_model_counts: model_counts,
                verdict_counts: BTreeMap::from([("jury_accept".into(), ITEMS)]),
                escalated_segments: models,
                training_grade_summary: crate::quality::TrainingGradeSummary::default(),
                training_grade_reason_counts: BTreeMap::from([("human_verified".into(), ITEMS)]),
                hypothesis_coverage_blockers: Vec::new(),
                orchestration_stages: vec![
                    crate::runs::AgentOrchestrationStage {
                        stage: "dataset_promotion".into(),
                        status: "ready".into(),
                        summary: String::new(),
                        blocker_count: 0,
                        blockers: Vec::new(),
                    };
                    ITEMS
                ],
            },
            jury_report: None,
            error: None,
            created_at: "2026-08-28 14:15:16".into(),
        };

        let public = AgentImportReportV1::from(&report);
        assert_eq!(public.summary.source_reference_count, ITEMS);
        assert_eq!(public.summary.source_references.len(), PUBLIC_REPORT_LIST_PREVIEW);
        assert_eq!(public.summary.required_source_reference_model_count, ITEMS);
        assert_eq!(public.summary.source_reference_model_count, ITEMS);
        assert_eq!(public.summary.source_reference_coverage_count, ITEMS);
        assert_eq!(public.summary.long_file_dossier_count, ITEMS);
        assert_eq!(public.summary.hypothesis_model_count, ITEMS);
        assert_eq!(public.summary.hypothesis_model_kind_count, ITEMS);
        assert_eq!(public.summary.escalated_segment_count, ITEMS);
        assert_eq!(public.summary.orchestration_stage_count, ITEMS);
        assert_eq!(public.summary.hypothesis_models.len(), PUBLIC_REPORT_MODEL_PREVIEW);
        assert_eq!(public.summary.hypothesis_model_counts.len(), PUBLIC_REPORT_MAP_PREVIEW);
        assert_eq!(public.summary.orchestration_stages.len(), PUBLIC_REPORT_LIST_PREVIEW);

        let readiness = public.summary.agentic_readiness.as_ref().expect("public readiness");
        assert_eq!(readiness.source_reference_model_count, ITEMS);
        assert_eq!(readiness.available_hypothesis_model_count, ITEMS);
        assert_eq!(readiness.check_count, ITEMS);
        assert_eq!(readiness.checks.len(), PUBLIC_REPORT_CHECK_PREVIEW);

        let dossier = &public.summary.long_file_dossiers[0];
        assert_eq!(dossier.source_reference_count, ITEMS);
        assert_eq!(dossier.source_references.len(), PUBLIC_REPORT_LIST_PREVIEW);
        assert_eq!(dossier.hypothesis_model_kind_count, ITEMS);
        assert_eq!(dossier.hypothesis_model_counts.len(), PUBLIC_REPORT_MAP_PREVIEW);
        assert_eq!(dossier.escalated_segment_count, ITEMS);
        assert_eq!(dossier.source_reference_coverage.required_model_count, ITEMS);
        assert_eq!(dossier.source_reference_coverage.present_model_count, ITEMS);
        assert_eq!(dossier.source_reference_coverage.missing_model_count, ITEMS);
        assert_eq!(dossier.source_reference_coverage.required_models.len(), PUBLIC_REPORT_MODEL_PREVIEW);
        assert_eq!(dossier.promotion_blocker_count, ITEMS);
        assert_eq!(dossier.promotion_blocker_codes, vec![AgentPromotionBlockerCodeV1::MissingHypothesisCoverage]);

        let wire = serde_json::to_vec(&public).expect("serialize bounded report");
        assert!(wire.len() < 100_000, "bounded report unexpectedly used {} bytes", wire.len());
    }
}
