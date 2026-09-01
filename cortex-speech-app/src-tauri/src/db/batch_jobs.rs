//! Durable authority for exact, crash-recoverable batch transcript mutations.
//!
//! A batch is admitted as one immutable `jobs` header plus an ordered set of immutable before
//! projections. Every segment mutation and its corresponding item evidence are committed in the
//! same SQLite savepoint. Renderer events may describe progress, but only this journal can decide
//! whether work was durably applied, skipped, failed, abandoned, or completed.

use super::*;

mod history;
mod lifecycle;

const BATCH_SCHEMA_V1: i64 = 1;
const MAX_BATCH_ITEMS_V1: usize = 100_000;
/// A worker may retain at most this many immutable before projections at once. Individual
/// projections can contain large transcripts, alignment metadata, and complete hypothesis sets, so
/// the public paging boundary is deliberately fixed rather than caller-selected.
pub const BATCH_PENDING_PAGE_SIZE_V1: usize = 128;
const MAX_BATCH_SEGMENT_TEXT_FIELD_BYTES_V1: usize = 512 * 1024;
const MAX_BATCH_SEGMENT_TEXT_BYTES_V1: usize = 2 * 1024 * 1024;
const MAX_BATCH_PROJECTION_JSON_BYTES_V1: usize = 32 * 1024 * 1024;
const MAX_BATCH_PENDING_PAGE_ENCODED_BYTES_V1: usize = 64 * 1024 * 1024;
const BATCH_EVIDENCE_ERROR: &str = "E_BATCH_EVIDENCE_INVALID";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchJobKindV1 {
    #[serde(rename = "batch_transcribe_v1")]
    Transcribe,
    #[serde(rename = "batch_normalize_v1")]
    Normalize,
}

impl BatchJobKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transcribe => "batch_transcribe_v1",
            Self::Normalize => "batch_normalize_v1",
        }
    }

    fn parse(value: &str) -> AppResult<Self> {
        match value {
            "batch_transcribe_v1" => Ok(Self::Transcribe),
            "batch_normalize_v1" => Ok(Self::Normalize),
            _ => Err(batch_evidence_error(format!("unknown batch kind '{value}'"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BatchJobLifecycleV1 {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl BatchJobLifecycleV1 {
    fn parse(value: &str) -> AppResult<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(batch_evidence_error(format!("unknown batch state '{value}'"))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BatchItemStateV1 {
    Pending,
    Applied,
    Skipped,
    Failed,
    Abandoned,
}

impl BatchItemStateV1 {
    fn parse(value: &str) -> AppResult<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "applied" => Ok(Self::Applied),
            "skipped" => Ok(Self::Skipped),
            "failed" => Ok(Self::Failed),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(batch_evidence_error(format!("unknown batch item state '{value}'"))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }
}

/// Hashed identity of the process allowed to execute one admitted request. The token itself never
/// enters SQLite; only its SHA-256 does.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchExecutorIdentityV1 {
    pub git_sha: String,
    pub token_sha256: String,
    pub attempt_generation: i64,
}

/// The exact eight-key payload enforced by migration 68.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchJobPayloadV1 {
    pub schema: i64,
    pub operation_id: String,
    pub kind: BatchJobKindV1,
    pub request_sha256: String,
    pub config_sha256: String,
    pub executor_git_sha: String,
    pub attempt_generation: i64,
    pub executor_token_sha256: String,
}

/// Complete hypothesis authority. The legacy IPC DTO omits the last two fields and therefore is not
/// sufficient for an exact inverse or a proof hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchStoredHypothesisV1 {
    pub segment_id: String,
    pub model_id: String,
    pub transcript: String,
    pub confidence: Option<f64>,
    pub model_version_id: String,
    pub created_at: String,
}

/// Canonical projection hashed before and after every item mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchSegmentProjectionV1 {
    pub schema: i64,
    pub segment: SpeechSegment,
    pub review_revision: i64,
    pub audio_content_hash: Option<String>,
    pub hypotheses: Vec<BatchStoredHypothesisV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchSourceIdentityV1 {
    schema: i64,
    segment_id: String,
    audio_path: String,
    alignment_json: Option<String>,
    duration_ms: i64,
    audio_content_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchRequestItemAuthorityV1 {
    ordinal: i64,
    segment_id: String,
    base_revision: i64,
    source_identity_sha256: String,
    before_projection_sha256: String,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchRequestAuthorityV1 {
    schema: i64,
    operation_id: String,
    kind: BatchJobKindV1,
    config_sha256: String,
    items: Vec<BatchRequestItemAuthorityV1>,
}

/// Incrementally produces the exact same canonical request digest as `BatchRequestAuthorityV1`
/// without retaining an authority vector proportional to the batch size. Field order is part of the
/// schema-68 evidence contract, so a parity regression below compares this byte stream with serde's
/// canonical struct serialization.
struct BatchRequestDigestV1 {
    hasher: Sha256,
    has_items: bool,
}

impl BatchRequestDigestV1 {
    fn new(operation_id: &str, kind: BatchJobKindV1, config_sha256: &str) -> AppResult<Self> {
        let mut hasher = Sha256::new();
        hasher.update(b"{\"schema\":1,\"operationId\":");
        hasher.update(canonical_json(&operation_id)?.as_bytes());
        hasher.update(b",\"kind\":");
        hasher.update(canonical_json(&kind)?.as_bytes());
        hasher.update(b",\"configSha256\":");
        hasher.update(canonical_json(&config_sha256)?.as_bytes());
        hasher.update(b",\"items\":[");
        Ok(Self { hasher, has_items: false })
    }

    fn push(&mut self, item: &BatchRequestItemAuthorityV1) -> AppResult<()> {
        if self.has_items {
            self.hasher.update(b",");
        }
        self.hasher.update(canonical_json(item)?.as_bytes());
        self.has_items = true;
        Ok(())
    }

    fn finish(mut self) -> String {
        self.hasher.update(b"]}");
        let digest = self.hasher.finalize();
        let mut encoded = String::with_capacity(64);
        for byte in digest {
            encoded.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            encoded.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
        }
        encoded
    }
}

/// Owns one projection while admission hashes or inserts it. The wrapper makes the intended
/// one-projection live set explicit and gives tests a deterministic bounded-live-object probe that
/// does not depend on allocator/RSS behaviour on Windows.
struct BatchAdmissionProjectionV1 {
    projection: BatchSegmentProjectionV1,
    #[cfg(test)]
    _live_probe: BatchAdmissionProjectionLiveProbe,
}

impl BatchAdmissionProjectionV1 {
    fn new(projection: BatchSegmentProjectionV1) -> Self {
        Self {
            projection,
            #[cfg(test)]
            _live_probe: BatchAdmissionProjectionLiveProbe::new(),
        }
    }
}

#[cfg(test)]
thread_local! {
    static BATCH_ADMISSION_PROJECTION_LIVE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static BATCH_ADMISSION_PROJECTION_PEAK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
struct BatchAdmissionProjectionLiveProbe;

#[cfg(test)]
impl BatchAdmissionProjectionLiveProbe {
    fn new() -> Self {
        BATCH_ADMISSION_PROJECTION_LIVE.with(|live| {
            let next = live.get() + 1;
            live.set(next);
            BATCH_ADMISSION_PROJECTION_PEAK.with(|peak| peak.set(peak.get().max(next)));
        });
        Self
    }
}

#[cfg(test)]
impl Drop for BatchAdmissionProjectionLiveProbe {
    fn drop(&mut self) {
        BATCH_ADMISSION_PROJECTION_LIVE.with(|live| live.set(live.get().saturating_sub(1)));
    }
}

/// One fully validated history endpoint. History performs a read-only validation pass and then an
/// apply pass under the same SQLite writer reservation, but deliberately never collects these
/// projection-bearing values. The savepoint still gives the second pass atomic all-or-nothing
/// rollback while peak retained projection authority stays constant with batch cardinality.
struct BatchHistoryPreparedItemV1 {
    ordinal: i64,
    segment_id: String,
    current_revision: i64,
    target: BatchSegmentProjectionV1,
    #[cfg(test)]
    _live_probe: BatchHistoryPreparedLiveProbe,
}

#[cfg(test)]
thread_local! {
    static BATCH_HISTORY_PREPARED_LIVE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static BATCH_HISTORY_PREPARED_PEAK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
struct BatchHistoryPreparedLiveProbe;

#[cfg(test)]
impl BatchHistoryPreparedLiveProbe {
    fn new() -> Self {
        BATCH_HISTORY_PREPARED_LIVE.with(|live| {
            let next = live.get() + 1;
            live.set(next);
            BATCH_HISTORY_PREPARED_PEAK.with(|peak| peak.set(peak.get().max(next)));
        });
        Self
    }
}

#[cfg(test)]
impl Drop for BatchHistoryPreparedLiveProbe {
    fn drop(&mut self) {
        BATCH_HISTORY_PREPARED_LIVE.with(|live| live.set(live.get().saturating_sub(1)));
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BatchItemCountsV1 {
    pub pending: i64,
    pub applied: i64,
    pub skipped: i64,
    pub failed: i64,
    pub abandoned: i64,
}

impl BatchItemCountsV1 {
    fn terminal(&self) -> i64 {
        self.applied + self.skipped + self.failed + self.abandoned
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchJobStatusV1 {
    pub operation_id: String,
    pub kind: BatchJobKindV1,
    pub state: BatchJobLifecycleV1,
    pub total: i64,
    pub completed: i64,
    pub progress: f64,
    pub counts: BatchItemCountsV1,
    pub request_sha256: String,
    pub config_sha256: String,
    pub error_code: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchPendingItemV1 {
    pub ordinal: i64,
    pub segment_id: String,
    pub before: BatchSegmentProjectionV1,
}

/// Input from a side-effect-free champion inference. The journal owns the only canonical write.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchChampionDraftV1 {
    pub raw_transcript: String,
    pub normalized_transcript: Option<String>,
    pub confidence: Option<f64>,
    pub confidence_source: Option<String>,
    pub model_version_id: String,
    pub deployment_sha256: String,
    pub cloud_call: bool,
    pub normalizer_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum BatchItemCommitOutcomeV1 {
    Applied { effect_revision: i64 },
    AlreadyApplied { effect_revision: i64 },
    Skipped { code: String },
    Failed { code: String },
    AlreadyTerminal { state: BatchItemStateV1, code: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum BatchTerminalIntentV1 {
    Succeeded,
    Failed { code: String },
    Cancelled { code: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BatchHistorySideV1 {
    Before,
    After,
}

impl BatchHistorySideV1 {
    fn opposite(self) -> Self {
        match self {
            Self::Before => Self::After,
            Self::After => Self::Before,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchHistoryItemTokenV1 {
    pub ordinal: i64,
    pub segment_id: String,
    pub expected_projection_sha256: String,
    pub expected_review_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchHistoryTokenV1 {
    pub operation_id: String,
    pub kind: BatchJobKindV1,
    pub expected_side: BatchHistorySideV1,
    pub items: Vec<BatchHistoryItemTokenV1>,
}

pub type BatchExecutionHistoryTokenV1 = BatchHistoryTokenV1;

#[derive(Debug)]
struct BatchHeaderV1 {
    operation_id: String,
    kind: BatchJobKindV1,
    state: BatchJobLifecycleV1,
    progress: f64,
    completed: i64,
    total: i64,
    error_code: Option<String>,
    payload_json: String,
    started_at: Option<String>,
    finished_at: Option<String>,
}

#[derive(Debug)]
struct BatchItemAuthorityV1 {
    job_id: String,
    ordinal: i64,
    segment_id: String,
    base_revision: i64,
    source_identity_sha256: String,
    before_projection_json: String,
    before_projection_sha256: String,
    state: BatchItemStateV1,
    after_projection_json: Option<String>,
    after_projection_sha256: Option<String>,
    effect_revision: Option<i64>,
    result_code: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct BatchProjectionFootprintV1 {
    segment_text_bytes: i64,
    largest_segment_field_bytes: i64,
    hypothesis_count: i64,
    hypothesis_transcript_bytes: i64,
    largest_hypothesis_transcript_bytes: i64,
    hypothesis_metadata_bytes: i64,
    largest_model_id_bytes: i64,
    largest_model_version_id_bytes: i64,
    largest_hypothesis_created_at_bytes: i64,
}

fn batch_evidence_error(message: impl Into<String>) -> AppError {
    AppError::Other(format!("{BATCH_EVIDENCE_ERROR}: {}", message.into()))
}

fn checked_footprint_value(value: i64, field: &str) -> AppResult<usize> {
    usize::try_from(value).map_err(|_| {
        batch_evidence_error(format!(
            "E_BATCH_PROJECTION_LIMIT_EXCEEDED: {field} has invalid byte/count metadata {value}"
        ))
    })
}

fn validate_batch_projection_footprint_v1(segment_id: &str, footprint: BatchProjectionFootprintV1) -> AppResult<()> {
    let segment_text_bytes = checked_footprint_value(footprint.segment_text_bytes, "segment text")?;
    let largest_segment_field_bytes =
        checked_footprint_value(footprint.largest_segment_field_bytes, "largest segment field")?;
    let hypothesis_count = checked_footprint_value(footprint.hypothesis_count, "hypothesis count")?;
    let hypothesis_transcript_bytes =
        checked_footprint_value(footprint.hypothesis_transcript_bytes, "hypothesis transcript aggregate")?;
    let largest_hypothesis_transcript_bytes =
        checked_footprint_value(footprint.largest_hypothesis_transcript_bytes, "largest hypothesis transcript")?;
    let hypothesis_metadata_bytes =
        checked_footprint_value(footprint.hypothesis_metadata_bytes, "hypothesis metadata aggregate")?;
    let largest_metadata_field_bytes = checked_footprint_value(
        footprint
            .largest_model_id_bytes
            .max(footprint.largest_model_version_id_bytes)
            .max(footprint.largest_hypothesis_created_at_bytes),
        "hypothesis metadata field",
    )?;

    if segment_text_bytes > MAX_BATCH_SEGMENT_TEXT_BYTES_V1
        || largest_segment_field_bytes > MAX_BATCH_SEGMENT_TEXT_FIELD_BYTES_V1
        || hypothesis_count > MAX_STORED_HYPOTHESES_PER_SEGMENT
        || hypothesis_transcript_bytes > MAX_STORED_HYPOTHESIS_TRANSCRIPT_BYTES_PER_SEGMENT
        || largest_hypothesis_transcript_bytes > MAX_STORED_HYPOTHESIS_TRANSCRIPT_BYTES
        || hypothesis_metadata_bytes > MAX_STORED_HYPOTHESIS_METADATA_BYTES_PER_SEGMENT
        || largest_metadata_field_bytes > MAX_STORED_HYPOTHESIS_METADATA_FIELD_BYTES
    {
        return Err(batch_evidence_error(format!(
            "E_BATCH_PROJECTION_LIMIT_EXCEEDED: segment '{segment_id}' authority is \
             {segment_text_bytes} segment bytes (largest field {largest_segment_field_bytes}), \
             {hypothesis_count} hypotheses, {hypothesis_transcript_bytes} hypothesis transcript bytes \
             (largest {largest_hypothesis_transcript_bytes}), and {hypothesis_metadata_bytes} metadata bytes \
             (largest field {largest_metadata_field_bytes})"
        )));
    }
    Ok(())
}

fn validate_projection_json_length_v1(byte_length: i64, identity: &str) -> AppResult<usize> {
    let byte_length = checked_footprint_value(byte_length, "projection JSON")?;
    if byte_length == 0 || byte_length > MAX_BATCH_PROJECTION_JSON_BYTES_V1 {
        return Err(batch_evidence_error(format!(
            "E_BATCH_PROJECTION_LIMIT_EXCEEDED: {identity} projection JSON is {byte_length} bytes; maximum is \
             {MAX_BATCH_PROJECTION_JSON_BYTES_V1}"
        )));
    }
    Ok(byte_length)
}

fn canonical_json<T: Serialize>(value: &T) -> AppResult<String> {
    serde_json::to_string(value).map_err(AppError::from)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn sha256_json<T: Serialize>(value: &T) -> AppResult<(String, String)> {
    let json = canonical_json(value)?;
    let digest = sha256_bytes(json.as_bytes());
    Ok((json, digest))
}

fn validate_sha256(value: &str, field: &str) -> AppResult<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err(AppError::Validation(format!("{field} must be a canonical lowercase SHA-256")));
    }
    Ok(())
}

fn validate_git_sha(value: &str) -> AppResult<()> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err(AppError::Validation(
            "executor Git SHA must be exactly 40 lowercase hexadecimal characters".into(),
        ));
    }
    Ok(())
}

fn validate_result_code(value: &str) -> AppResult<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(AppError::Validation(
            "batch result code must contain 1-64 uppercase ASCII letters, digits, or underscores".into(),
        ));
    }
    Ok(())
}

fn validate_batch_item_count_v1(item_count: usize) -> AppResult<()> {
    if item_count == 0 || item_count > MAX_BATCH_ITEMS_V1 {
        return Err(AppError::Validation(format!("batch must contain between 1 and {MAX_BATCH_ITEMS_V1} segment ids")));
    }
    Ok(())
}

fn source_identity(projection: &BatchSegmentProjectionV1) -> BatchSourceIdentityV1 {
    BatchSourceIdentityV1 {
        schema: BATCH_SCHEMA_V1,
        segment_id: projection.segment.id.clone(),
        audio_path: projection.segment.audio_path.clone(),
        alignment_json: projection.segment.alignment_json.clone(),
        duration_ms: projection.segment.duration_ms,
        audio_content_hash: projection.audio_content_hash.clone(),
    }
}

fn validate_batch_projection_value_v1(projection: &BatchSegmentProjectionV1) -> AppResult<()> {
    let segment = &projection.segment;
    let segment_fields = [
        segment.id.as_str(),
        segment.created_at.as_deref().unwrap_or(""),
        segment.audio_path.as_str(),
        segment.raw_transcript.as_str(),
        segment.normalized_transcript.as_deref().unwrap_or(""),
        segment.annotated_transcript.as_deref().unwrap_or(""),
        segment.alignment_json.as_deref().unwrap_or(""),
        segment.speaker_id.as_deref().unwrap_or(""),
        segment.split.as_deref().unwrap_or(""),
        segment.verdict.as_deref().unwrap_or(""),
        segment.verdict_transcript.as_deref().unwrap_or(""),
        segment.rationale.as_deref().unwrap_or(""),
        segment.evidence_json.as_deref().unwrap_or(""),
        segment.human_decision.as_deref().unwrap_or(""),
        segment.corrected_at.as_deref().unwrap_or(""),
        segment.alignment_quality.as_deref().unwrap_or(""),
        segment.model_version_id.as_deref().unwrap_or(""),
        segment.confidence_source.as_deref().unwrap_or(""),
        segment.decoder_config_hash.as_deref().unwrap_or(""),
        segment.normalizer_version.as_deref().unwrap_or(""),
        segment.vad_backend.as_deref().unwrap_or(""),
        segment.reviewed_by.as_deref().unwrap_or(""),
        projection.audio_content_hash.as_deref().unwrap_or(""),
    ];
    let segment_text_bytes = segment_fields.iter().try_fold(0usize, |total, field| {
        total.checked_add(field.len()).ok_or_else(|| {
            batch_evidence_error("E_BATCH_PROJECTION_LIMIT_EXCEEDED: segment text byte count overflowed")
        })
    })?;
    let largest_segment_field_bytes = segment_fields.iter().map(|field| field.len()).max().unwrap_or(0);

    let mut hypothesis_transcript_bytes = 0usize;
    let mut hypothesis_metadata_bytes = 0usize;
    let mut largest_hypothesis_transcript_bytes = 0usize;
    let mut largest_metadata_field_bytes = 0usize;
    for hypothesis in &projection.hypotheses {
        validate_stored_hypothesis_payload(&hypothesis.segment_id, &hypothesis.model_id, &hypothesis.transcript)?;
        hypothesis_transcript_bytes = hypothesis_transcript_bytes
            .checked_add(hypothesis.transcript.len())
            .ok_or_else(|| batch_evidence_error("E_BATCH_PROJECTION_LIMIT_EXCEEDED: transcript bytes overflowed"))?;
        largest_hypothesis_transcript_bytes = largest_hypothesis_transcript_bytes.max(hypothesis.transcript.len());
        for metadata in [&hypothesis.model_id, &hypothesis.model_version_id, &hypothesis.created_at] {
            hypothesis_metadata_bytes = hypothesis_metadata_bytes.checked_add(metadata.len()).ok_or_else(|| {
                batch_evidence_error("E_BATCH_PROJECTION_LIMIT_EXCEEDED: hypothesis metadata bytes overflowed")
            })?;
            largest_metadata_field_bytes = largest_metadata_field_bytes.max(metadata.len());
        }
    }
    validate_batch_projection_footprint_v1(
        &segment.id,
        BatchProjectionFootprintV1 {
            segment_text_bytes: i64::try_from(segment_text_bytes).unwrap_or(i64::MAX),
            largest_segment_field_bytes: i64::try_from(largest_segment_field_bytes).unwrap_or(i64::MAX),
            hypothesis_count: i64::try_from(projection.hypotheses.len()).unwrap_or(i64::MAX),
            hypothesis_transcript_bytes: i64::try_from(hypothesis_transcript_bytes).unwrap_or(i64::MAX),
            largest_hypothesis_transcript_bytes: i64::try_from(largest_hypothesis_transcript_bytes).unwrap_or(i64::MAX),
            hypothesis_metadata_bytes: i64::try_from(hypothesis_metadata_bytes).unwrap_or(i64::MAX),
            largest_model_id_bytes: i64::try_from(largest_metadata_field_bytes).unwrap_or(i64::MAX),
            largest_model_version_id_bytes: 0,
            largest_hypothesis_created_at_bytes: 0,
        },
    )
}

fn projection_authority(projection: &BatchSegmentProjectionV1) -> AppResult<(String, String, String)> {
    validate_batch_projection_value_v1(projection)?;
    let (projection_json, projection_sha256) = sha256_json(projection)?;
    validate_projection_json_length_v1(
        i64::try_from(projection_json.len()).unwrap_or(i64::MAX),
        &format!("segment '{}'", projection.segment.id),
    )?;
    let (_, source_identity_sha256) = sha256_json(&source_identity(projection))?;
    Ok((projection_json, projection_sha256, source_identity_sha256))
}

fn projection_semantic_sha256(projection: &BatchSegmentProjectionV1) -> AppResult<String> {
    let mut semantic = projection.clone();
    // Undo/redo is monotonic: content may return to a journal endpoint, but its database-owned
    // revision must never move backward to the historical number stored in that endpoint.
    semantic.review_revision = 0;
    let (_, digest) = sha256_json(&semantic)?;
    Ok(digest)
}

fn segment_is_human_owned(segment: &SpeechSegment) -> bool {
    segment.verified
        || segment.is_gold
        // Presence alone is authoritative. Historical machine-seed incidents left Some("") and
        // Some(machine text); neither may be treated as an unowned NULL baseline.
        || segment.annotated_transcript.is_some()
        || segment.human_decision.as_deref().is_some_and(|value| !value.trim().is_empty())
        || matches!(segment.verdict.as_deref(), Some("human_accept" | "human_edit" | "human_reject"))
        || segment.reviewed_by.as_deref().is_some_and(|value| !value.trim().is_empty())
        || segment.corrected_at.is_some()
}

fn read_batch_header_on(conn: &Connection, operation_id: &str) -> AppResult<Option<BatchHeaderV1>> {
    let row = conn
        .query_row(
            "SELECT id,kind,state,progress,completed,total,error_code,payload_json,started_at,finished_at
               FROM jobs WHERE id=?1 AND kind IN ('batch_transcribe_v1','batch_normalize_v1')",
            [operation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        operation_id,
        kind,
        state,
        progress,
        completed,
        total,
        error_code,
        payload_json,
        started_at,
        finished_at,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(BatchHeaderV1 {
        operation_id,
        kind: BatchJobKindV1::parse(&kind)?,
        state: BatchJobLifecycleV1::parse(&state)?,
        progress,
        completed,
        total,
        error_code,
        payload_json,
        started_at,
        finished_at,
    }))
}

fn read_batch_item_on(conn: &Connection, operation_id: &str, ordinal: i64) -> AppResult<Option<BatchItemAuthorityV1>> {
    let encoded_lengths = conn
        .query_row(
            "SELECT length(CAST(before_projection_json AS BLOB)),
                    COALESCE(length(CAST(after_projection_json AS BLOB)),0)
               FROM batch_job_items_v1 WHERE job_id=?1 AND ordinal=?2",
            params![operation_id, ordinal],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((before_bytes, after_bytes)) = encoded_lengths else {
        return Ok(None);
    };
    validate_projection_json_length_v1(before_bytes, &format!("batch item {operation_id}/{ordinal} before"))?;
    if after_bytes != 0 {
        validate_projection_json_length_v1(after_bytes, &format!("batch item {operation_id}/{ordinal} after"))?;
    }
    let row = conn
        .query_row(
            "SELECT job_id,ordinal,segment_id,base_revision,source_identity_sha256,
                    before_projection_json,before_projection_sha256,state,
                    after_projection_json,after_projection_sha256,effect_revision,result_code
               FROM batch_job_items_v1 WHERE job_id=?1 AND ordinal=?2",
            params![operation_id, ordinal],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                ))
            },
        )
        .optional()?;
    let Some((
        job_id,
        ordinal,
        segment_id,
        base_revision,
        source_identity_sha256,
        before_projection_json,
        before_projection_sha256,
        state,
        after_projection_json,
        after_projection_sha256,
        effect_revision,
        result_code,
    )) = row
    else {
        return Err(batch_evidence_error(format!(
            "batch item {operation_id}/{ordinal} vanished after its encoded lengths were validated"
        )));
    };
    Ok(Some(BatchItemAuthorityV1 {
        job_id,
        ordinal,
        segment_id,
        base_revision,
        source_identity_sha256,
        before_projection_json,
        before_projection_sha256,
        state: BatchItemStateV1::parse(&state)?,
        after_projection_json,
        after_projection_sha256,
        effect_revision,
        result_code,
    }))
}

fn batch_item_counts_on(conn: &Connection, operation_id: &str) -> AppResult<BatchItemCountsV1> {
    conn.query_row(
        "SELECT COALESCE(sum(state='pending'),0),COALESCE(sum(state='applied'),0),
                COALESCE(sum(state='skipped'),0),COALESCE(sum(state='failed'),0),
                COALESCE(sum(state='abandoned'),0)
           FROM batch_job_items_v1 WHERE job_id=?1",
        [operation_id],
        |row| {
            Ok(BatchItemCountsV1 {
                pending: row.get(0)?,
                applied: row.get(1)?,
                skipped: row.get(2)?,
                failed: row.get(3)?,
                abandoned: row.get(4)?,
            })
        },
    )
    .map_err(Into::into)
}

fn status_from_header_on(conn: &Connection, header: BatchHeaderV1) -> AppResult<BatchJobStatusV1> {
    let payload = Database::parse_batch_payload_v1(&header)?;
    let counts = batch_item_counts_on(conn, &header.operation_id)?;
    let count_total = counts.pending + counts.terminal();
    if header.total < 1
        || header.total > MAX_BATCH_ITEMS_V1 as i64
        || count_total != header.total
        || header.completed != counts.terminal()
        || !header.progress.is_finite()
        || (header.progress - (header.completed as f64 / header.total as f64)).abs() > 1e-9
    {
        return Err(batch_evidence_error(format!(
            "job {} header progress disagrees with durable item counts",
            header.operation_id
        )));
    }
    match header.state {
        BatchJobLifecycleV1::Queued => {
            if header.completed != 0 || header.started_at.is_some() || header.finished_at.is_some() {
                return Err(batch_evidence_error(format!("queued job {} has lifecycle residue", header.operation_id)));
            }
        }
        BatchJobLifecycleV1::Running => {
            if header.started_at.is_none() || header.finished_at.is_some() || header.error_code.is_some() {
                return Err(batch_evidence_error(format!(
                    "running job {} has invalid lifecycle fields",
                    header.operation_id
                )));
            }
        }
        BatchJobLifecycleV1::Succeeded => {
            if counts.pending != 0
                || counts.failed != 0
                || counts.abandoned != 0
                || header.finished_at.is_none()
                || header.error_code.is_some()
            {
                return Err(batch_evidence_error(format!(
                    "succeeded job {} contradicts item evidence",
                    header.operation_id
                )));
            }
        }
        BatchJobLifecycleV1::Failed => {
            if counts.pending != 0
                || counts.failed + counts.abandoned == 0
                || header.finished_at.is_none()
                || header.error_code.is_none()
            {
                return Err(batch_evidence_error(format!(
                    "failed job {} contradicts item evidence",
                    header.operation_id
                )));
            }
        }
        BatchJobLifecycleV1::Cancelled => {
            if counts.pending != 0
                || counts.failed != 0
                || counts.abandoned == 0
                || header.finished_at.is_none()
                || header.error_code.is_none()
            {
                return Err(batch_evidence_error(format!(
                    "cancelled job {} contradicts item evidence",
                    header.operation_id
                )));
            }
        }
    }
    Ok(BatchJobStatusV1 {
        operation_id: header.operation_id,
        kind: header.kind,
        state: header.state,
        total: header.total,
        completed: header.completed,
        progress: header.progress,
        counts,
        request_sha256: payload.request_sha256,
        config_sha256: payload.config_sha256,
        error_code: header.error_code,
        started_at: header.started_at,
        finished_at: header.finished_at,
    })
}

impl Database {
    fn require_batch_schema_v1(&self) -> AppResult<()> {
        let version = crate::migrations::get_current_version(self)?;
        if version < 68 {
            return Err(AppError::Validation(format!(
                "durable batch authority requires schema 68, but this database is schema {version}"
            )));
        }
        Ok(())
    }

    fn reserve_batch_writer(&self) -> AppResult<()> {
        // A syntactically real main-database write obtains SQLite's writer reservation even when no
        // row matches. All authority reads that follow therefore share the same write epoch.
        self.conn.execute("UPDATE jobs SET updated_at=updated_at WHERE id=''", [])?;
        Ok(())
    }

    fn batch_projection_footprint_on(
        conn: &Connection,
        segment_id: &str,
    ) -> AppResult<Option<BatchProjectionFootprintV1>> {
        conn.query_row(
            "SELECT
                length(CAST(COALESCE(id,'') AS BLOB))
               +length(CAST(COALESCE(created_at,'') AS BLOB))
               +length(CAST(COALESCE(audio_path,'') AS BLOB))
               +length(CAST(COALESCE(raw_transcript,'') AS BLOB))
               +length(CAST(COALESCE(normalized_transcript,'') AS BLOB))
               +length(CAST(COALESCE(annotated_transcript,'') AS BLOB))
               +length(CAST(COALESCE(alignment_json,'') AS BLOB))
               +length(CAST(COALESCE(speaker_id,'') AS BLOB))
               +length(CAST(COALESCE(split,'') AS BLOB))
               +length(CAST(COALESCE(verdict,'') AS BLOB))
               +length(CAST(COALESCE(verdict_transcript,'') AS BLOB))
               +length(CAST(COALESCE(rationale,'') AS BLOB))
               +length(CAST(COALESCE(evidence_json,'') AS BLOB))
               +length(CAST(COALESCE(human_decision,'') AS BLOB))
               +length(CAST(COALESCE(corrected_at,'') AS BLOB))
               +length(CAST(COALESCE(alignment_quality,'') AS BLOB))
               +length(CAST(COALESCE(model_version_id,'') AS BLOB))
               +length(CAST(COALESCE(confidence_source,'') AS BLOB))
               +length(CAST(COALESCE(decoder_config_hash,'') AS BLOB))
               +length(CAST(COALESCE(normalizer_version,'') AS BLOB))
               +length(CAST(COALESCE(vad_backend,'') AS BLOB))
               +length(CAST(COALESCE(reviewed_by,'') AS BLOB))
               +length(CAST(COALESCE(audio_content_hash,'') AS BLOB)),
                max(
                    length(CAST(COALESCE(id,'') AS BLOB)),
                    length(CAST(COALESCE(created_at,'') AS BLOB)),
                    length(CAST(COALESCE(audio_path,'') AS BLOB)),
                    length(CAST(COALESCE(raw_transcript,'') AS BLOB)),
                    length(CAST(COALESCE(normalized_transcript,'') AS BLOB)),
                    length(CAST(COALESCE(annotated_transcript,'') AS BLOB)),
                    length(CAST(COALESCE(alignment_json,'') AS BLOB)),
                    length(CAST(COALESCE(speaker_id,'') AS BLOB)),
                    length(CAST(COALESCE(split,'') AS BLOB)),
                    length(CAST(COALESCE(verdict,'') AS BLOB)),
                    length(CAST(COALESCE(verdict_transcript,'') AS BLOB)),
                    length(CAST(COALESCE(rationale,'') AS BLOB)),
                    length(CAST(COALESCE(evidence_json,'') AS BLOB)),
                    length(CAST(COALESCE(human_decision,'') AS BLOB)),
                    length(CAST(COALESCE(corrected_at,'') AS BLOB)),
                    length(CAST(COALESCE(alignment_quality,'') AS BLOB)),
                    length(CAST(COALESCE(model_version_id,'') AS BLOB)),
                    length(CAST(COALESCE(confidence_source,'') AS BLOB)),
                    length(CAST(COALESCE(decoder_config_hash,'') AS BLOB)),
                    length(CAST(COALESCE(normalizer_version,'') AS BLOB)),
                    length(CAST(COALESCE(vad_backend,'') AS BLOB)),
                    length(CAST(COALESCE(reviewed_by,'') AS BLOB)),
                    length(CAST(COALESCE(audio_content_hash,'') AS BLOB))
                ),
                (SELECT count(*) FROM segment_hypotheses h WHERE h.segment_id=speech_segments.id),
                (SELECT COALESCE(sum(length(CAST(h.transcript AS BLOB))),0)
                   FROM segment_hypotheses h WHERE h.segment_id=speech_segments.id),
                (SELECT COALESCE(max(length(CAST(h.transcript AS BLOB))),0)
                   FROM segment_hypotheses h WHERE h.segment_id=speech_segments.id),
                (SELECT COALESCE(sum(length(CAST(COALESCE(h.model_id,'') AS BLOB))
                                   +length(CAST(COALESCE(h.model_version_id,'') AS BLOB))
                                   +length(CAST(COALESCE(h.created_at,'') AS BLOB))),0)
                   FROM segment_hypotheses h WHERE h.segment_id=speech_segments.id),
                (SELECT COALESCE(max(length(CAST(COALESCE(h.model_id,'') AS BLOB))),0)
                   FROM segment_hypotheses h WHERE h.segment_id=speech_segments.id),
                (SELECT COALESCE(max(length(CAST(COALESCE(h.model_version_id,'') AS BLOB))),0)
                   FROM segment_hypotheses h WHERE h.segment_id=speech_segments.id),
                (SELECT COALESCE(max(length(CAST(COALESCE(h.created_at,'') AS BLOB))),0)
                   FROM segment_hypotheses h WHERE h.segment_id=speech_segments.id)
               FROM speech_segments WHERE id=?1",
            [segment_id],
            |row| {
                Ok(BatchProjectionFootprintV1 {
                    segment_text_bytes: row.get(0)?,
                    largest_segment_field_bytes: row.get(1)?,
                    hypothesis_count: row.get(2)?,
                    hypothesis_transcript_bytes: row.get(3)?,
                    largest_hypothesis_transcript_bytes: row.get(4)?,
                    hypothesis_metadata_bytes: row.get(5)?,
                    largest_model_id_bytes: row.get(6)?,
                    largest_model_version_id_bytes: row.get(7)?,
                    largest_hypothesis_created_at_bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub(super) fn read_batch_projection_on(
        conn: &Connection,
        segment_id: &str,
    ) -> AppResult<Option<BatchSegmentProjectionV1>> {
        let Some(footprint) = Self::batch_projection_footprint_on(conn, segment_id)? else {
            return Ok(None);
        };
        validate_batch_projection_footprint_v1(segment_id, footprint)?;
        let Some((segment, review_revision, audio_content_hash)) = Self::decision_snapshot_on(conn, segment_id)? else {
            return Err(batch_evidence_error(format!(
                "segment '{segment_id}' vanished after its projection footprint was validated"
            )));
        };
        let mut statement = conn.prepare(
            "SELECT segment_id,model_id,transcript,confidence,model_version_id,created_at
               FROM segment_hypotheses
              WHERE segment_id=?1
              ORDER BY model_id ASC",
        )?;
        let hypotheses = statement
            .query_map([segment_id], |row| {
                Ok(BatchStoredHypothesisV1 {
                    segment_id: row.get(0)?,
                    model_id: row.get(1)?,
                    transcript: row.get(2)?,
                    confidence: row.get(3)?,
                    model_version_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for hypothesis in &hypotheses {
            validate_stored_hypothesis_payload(&hypothesis.segment_id, &hypothesis.model_id, &hypothesis.transcript)?;
        }
        Ok(Some(BatchSegmentProjectionV1 {
            schema: BATCH_SCHEMA_V1,
            segment,
            review_revision,
            audio_content_hash,
            hypotheses,
        }))
    }

    fn read_batch_header_v1(&self, operation_id: &str) -> AppResult<Option<BatchHeaderV1>> {
        read_batch_header_on(&self.conn, operation_id)
    }

    fn parse_batch_payload_v1(header: &BatchHeaderV1) -> AppResult<BatchJobPayloadV1> {
        let payload: BatchJobPayloadV1 = serde_json::from_str(&header.payload_json).map_err(|error| {
            batch_evidence_error(format!("job {} payload cannot be decoded: {error}", header.operation_id))
        })?;
        let canonical = canonical_json(&payload)?;
        if canonical != header.payload_json {
            return Err(batch_evidence_error(format!(
                "job {} payload is not in canonical typed JSON form",
                header.operation_id
            )));
        }
        if payload.schema != BATCH_SCHEMA_V1
            || payload.operation_id != header.operation_id
            || payload.kind != header.kind
            || payload.attempt_generation < 1
        {
            return Err(batch_evidence_error(format!("job {} payload disagrees with its header", header.operation_id)));
        }
        validate_sha256(&payload.request_sha256, "stored batch request hash")
            .map_err(|error| batch_evidence_error(error.to_string()))?;
        validate_sha256(&payload.config_sha256, "stored batch config hash")
            .map_err(|error| batch_evidence_error(error.to_string()))?;
        validate_git_sha(&payload.executor_git_sha).map_err(|error| batch_evidence_error(error.to_string()))?;
        validate_sha256(&payload.executor_token_sha256, "stored executor token hash")
            .map_err(|error| batch_evidence_error(error.to_string()))?;
        Ok(payload)
    }

    fn require_batch_executor_v1(
        header: &BatchHeaderV1,
        executor: &BatchExecutorIdentityV1,
    ) -> AppResult<BatchJobPayloadV1> {
        validate_git_sha(&executor.git_sha)?;
        validate_sha256(&executor.token_sha256, "executor token hash")?;
        if executor.attempt_generation < 1 {
            return Err(AppError::Validation("executor attempt generation must be positive".into()));
        }
        let payload = Self::parse_batch_payload_v1(header)?;
        if payload.executor_git_sha != executor.git_sha
            || payload.attempt_generation != executor.attempt_generation
            || payload.executor_token_sha256 != executor.token_sha256
        {
            return Err(AppError::Validation(
                "E_BATCH_EXECUTOR_MISMATCH: this worker does not own the admitted batch attempt".into(),
            ));
        }
        Ok(payload)
    }

    fn read_batch_item_v1(&self, operation_id: &str, ordinal: i64) -> AppResult<Option<BatchItemAuthorityV1>> {
        read_batch_item_on(&self.conn, operation_id, ordinal)
    }

    fn decode_before_projection_v1(item: &BatchItemAuthorityV1) -> AppResult<BatchSegmentProjectionV1> {
        let projection: BatchSegmentProjectionV1 =
            serde_json::from_str(&item.before_projection_json).map_err(|error| {
                batch_evidence_error(format!(
                    "batch item {}/{} before projection cannot be decoded: {error}",
                    item.job_id, item.ordinal
                ))
            })?;
        let (canonical, projection_sha256, source_sha256) = projection_authority(&projection)?;
        if canonical != item.before_projection_json
            || projection_sha256 != item.before_projection_sha256
            || source_sha256 != item.source_identity_sha256
            || projection.schema != BATCH_SCHEMA_V1
            || projection.segment.id != item.segment_id
            || projection.review_revision != item.base_revision
            || projection.hypotheses.iter().any(|hypothesis| hypothesis.segment_id != item.segment_id)
        {
            return Err(batch_evidence_error(format!(
                "batch item {}/{} before projection authority does not match its identity columns",
                item.job_id, item.ordinal
            )));
        }
        Ok(projection)
    }

    fn decode_after_projection_v1(item: &BatchItemAuthorityV1) -> AppResult<Option<BatchSegmentProjectionV1>> {
        let Some(json) = item.after_projection_json.as_deref() else {
            if item.after_projection_sha256.is_some() || item.effect_revision.is_some() {
                return Err(batch_evidence_error(format!(
                    "batch item {}/{} has partial after authority",
                    item.job_id, item.ordinal
                )));
            }
            return Ok(None);
        };
        let projection: BatchSegmentProjectionV1 = serde_json::from_str(json).map_err(|error| {
            batch_evidence_error(format!(
                "batch item {}/{} after projection cannot be decoded: {error}",
                item.job_id, item.ordinal
            ))
        })?;
        let (canonical, digest, _) = projection_authority(&projection)?;
        if canonical != json
            || item.after_projection_sha256.as_deref() != Some(digest.as_str())
            || projection.schema != BATCH_SCHEMA_V1
            || projection.segment.id != item.segment_id
            || item.effect_revision != Some(projection.review_revision)
            || projection.review_revision <= item.base_revision
            || projection.hypotheses.iter().any(|hypothesis| hypothesis.segment_id != item.segment_id)
        {
            return Err(batch_evidence_error(format!(
                "batch item {}/{} after projection authority does not match its effect columns",
                item.job_id, item.ordinal
            )));
        }
        Ok(Some(projection))
    }

    fn prepare_batch_history_item_v1(
        &self,
        operation_id: &str,
        endpoint: &BatchHistoryItemTokenV1,
        expected_side: BatchHistorySideV1,
    ) -> AppResult<BatchHistoryPreparedItemV1> {
        let item = self
            .read_batch_item_v1(operation_id, endpoint.ordinal)?
            .ok_or_else(|| batch_evidence_error("history token ordinal is absent from the journal"))?;
        if item.state != BatchItemStateV1::Applied || item.segment_id != endpoint.segment_id {
            return Err(AppError::Validation(
                "BATCH_HISTORY_CONFLICT: token endpoint is not the matching applied journal item".into(),
            ));
        }
        let before = Self::decode_before_projection_v1(&item)?;
        let after = Self::decode_after_projection_v1(&item)?
            .ok_or_else(|| batch_evidence_error("applied history item has no after projection"))?;
        // Compute the endpoint digest before loading the current database projection. This drops the
        // non-target historical projection before another full projection is materialized.
        let (expected_semantic_sha256, target) = match expected_side {
            BatchHistorySideV1::Before => (projection_semantic_sha256(&before)?, after),
            BatchHistorySideV1::After => (projection_semantic_sha256(&after)?, before),
        };
        drop(item);
        let current = Self::read_batch_projection_on(&self.conn, &endpoint.segment_id)?
            .ok_or_else(|| AppError::Validation("BATCH_HISTORY_CONFLICT: target segment is missing".into()))?;
        let (_, current_sha256, _) = projection_authority(&current)?;
        if current.review_revision != endpoint.expected_review_revision
            || current_sha256 != endpoint.expected_projection_sha256
            || projection_semantic_sha256(&current)? != expected_semantic_sha256
        {
            return Err(AppError::Validation(format!(
                "BATCH_HISTORY_CONFLICT: segment '{}' changed after the history token was issued",
                endpoint.segment_id
            )));
        }
        Ok(BatchHistoryPreparedItemV1 {
            ordinal: endpoint.ordinal,
            segment_id: endpoint.segment_id.clone(),
            current_revision: current.review_revision,
            target,
            #[cfg(test)]
            _live_probe: BatchHistoryPreparedLiveProbe::new(),
        })
    }

    fn batch_item_counts_v1(&self, operation_id: &str) -> AppResult<BatchItemCountsV1> {
        batch_item_counts_on(&self.conn, operation_id)
    }

    fn status_from_header_v1(&self, header: BatchHeaderV1) -> AppResult<BatchJobStatusV1> {
        status_from_header_on(&self.conn, header)
    }

    /// Admit one exact ordered request. The writer reservation is acquired before reading any
    /// segment, and the header, every before projection, and the running transition commit together.
    pub fn admit_batch_job_v1(
        &self,
        operation_id: &str,
        kind: BatchJobKindV1,
        segment_ids: &[String],
        config_sha256: &str,
        executor: &BatchExecutorIdentityV1,
    ) -> AppResult<BatchJobStatusV1> {
        self.admit_batch_job_v1_inner(operation_id, kind, segment_ids, config_sha256, executor, None)
    }

    /// Cancellation-aware production admission. A cancellation observed during either streaming
    /// pass rolls the one savepoint back, leaving neither a header nor partial item authority.
    pub fn admit_batch_job_v1_cancellable(
        &self,
        operation_id: &str,
        kind: BatchJobKindV1,
        segment_ids: &[String],
        config_sha256: &str,
        executor: &BatchExecutorIdentityV1,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> AppResult<BatchJobStatusV1> {
        self.admit_batch_job_v1_inner(operation_id, kind, segment_ids, config_sha256, executor, Some(cancel))
    }

    fn admit_batch_job_v1_inner(
        &self,
        operation_id: &str,
        kind: BatchJobKindV1,
        segment_ids: &[String],
        config_sha256: &str,
        executor: &BatchExecutorIdentityV1,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<BatchJobStatusV1> {
        let require_not_cancelled = || {
            if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
                Err(AppError::Validation(
                    "BATCH_ADMISSION_CANCELLED: batch admission was cancelled before durable publication".into(),
                ))
            } else {
                Ok(())
            }
        };
        require_not_cancelled()?;
        self.require_batch_schema_v1()?;
        validate_operation_uuid(operation_id)?;
        validate_sha256(config_sha256, "batch config hash")?;
        validate_git_sha(&executor.git_sha)?;
        validate_sha256(&executor.token_sha256, "executor token hash")?;
        if executor.attempt_generation < 1 {
            return Err(AppError::Validation("executor attempt generation must be positive".into()));
        }
        validate_batch_item_count_v1(segment_ids.len())?;
        for segment_id in segment_ids {
            crate::validation::input::validate_identifier(segment_id).map_err(AppError::Validation)?;
        }

        self.conn.execute("SAVEPOINT batch_v1_admit", [])?;
        let admitted = (|| -> AppResult<()> {
            self.reserve_batch_writer()?;

            // Pass one computes the exact ordered request digest while holding SQLite's writer
            // reservation. Only one full projection and one compact item authority are live at a
            // time. The reservation makes the source epoch stable until the savepoint commits.
            let mut request_digest = BatchRequestDigestV1::new(operation_id, kind, config_sha256)?;
            for (ordinal, segment_id) in segment_ids.iter().enumerate() {
                require_not_cancelled()?;
                let projection = BatchAdmissionProjectionV1::new(
                    Self::read_batch_projection_on(&self.conn, segment_id)?.ok_or_else(|| {
                        AppError::Validation(format!(
                            "batch segment '{segment_id}' does not exist; no batch was admitted"
                        ))
                    })?,
                );
                let (_, projection_sha256, source_identity_sha256) = projection_authority(&projection.projection)?;
                request_digest.push(&BatchRequestItemAuthorityV1 {
                    ordinal: ordinal as i64,
                    segment_id: segment_id.clone(),
                    base_revision: projection.projection.review_revision,
                    source_identity_sha256,
                    before_projection_sha256: projection_sha256,
                })?;
            }
            let request_sha256 = request_digest.finish();
            require_not_cancelled()?;
            let payload = BatchJobPayloadV1 {
                schema: BATCH_SCHEMA_V1,
                operation_id: operation_id.to_string(),
                kind,
                request_sha256: request_sha256.clone(),
                config_sha256: config_sha256.to_string(),
                executor_git_sha: executor.git_sha.clone(),
                attempt_generation: executor.attempt_generation,
                executor_token_sha256: executor.token_sha256.clone(),
            };
            let payload_json = canonical_json(&payload)?;
            self.conn.execute(
                "INSERT INTO jobs(id,kind,state,idempotency_key,total,completed,progress,payload_json)
                 VALUES(?1,?2,'queued',?3,?4,0,0.0,?5)",
                params![
                    operation_id,
                    kind.as_str(),
                    format!("batch-v1:{operation_id}"),
                    segment_ids.len() as i64,
                    payload_json,
                ],
            )?;

            // Pass two re-reads the same reserved write epoch, inserts each immutable projection,
            // and independently rebuilds the digest. Any serialization/source disagreement rolls
            // the entire savepoint back before the job can enter `running`.
            let mut inserted_digest = BatchRequestDigestV1::new(operation_id, kind, config_sha256)?;
            for (ordinal, segment_id) in segment_ids.iter().enumerate() {
                require_not_cancelled()?;
                let projection = BatchAdmissionProjectionV1::new(
                    Self::read_batch_projection_on(&self.conn, segment_id)?.ok_or_else(|| {
                        batch_evidence_error(format!(
                            "batch segment '{segment_id}' vanished inside its reserved admission epoch"
                        ))
                    })?,
                );
                let (projection_json, projection_sha256, source_identity_sha256) =
                    projection_authority(&projection.projection)?;
                inserted_digest.push(&BatchRequestItemAuthorityV1 {
                    ordinal: ordinal as i64,
                    segment_id: segment_id.clone(),
                    base_revision: projection.projection.review_revision,
                    source_identity_sha256: source_identity_sha256.clone(),
                    before_projection_sha256: projection_sha256.clone(),
                })?;
                self.conn.execute(
                    "INSERT INTO batch_job_items_v1(
                         job_id,ordinal,segment_id,base_revision,source_identity_sha256,
                         before_projection_json,before_projection_sha256)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        operation_id,
                        ordinal as i64,
                        projection.projection.segment.id,
                        projection.projection.review_revision,
                        source_identity_sha256,
                        projection_json,
                        projection_sha256,
                    ],
                )?;
            }
            if inserted_digest.finish() != request_sha256 {
                return Err(batch_evidence_error(
                    "batch request authority changed inside its reserved admission epoch",
                ));
            }
            require_not_cancelled()?;
            let started = self.conn.execute(
                "UPDATE jobs
                    SET state='running',started_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                        updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE id=?1 AND state='queued'",
                [operation_id],
            )?;
            if started != 1 {
                return Err(batch_evidence_error("admitted batch did not enter running state"));
            }
            Ok(())
        })();
        match admitted {
            Ok(()) => {
                self.release_savepoint("batch_v1_admit")?;
                self.track_write()?;
                self.get_batch_job_status_v1(operation_id)?
                    .ok_or_else(|| batch_evidence_error("admitted batch disappeared immediately after durable commit"))
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("batch_v1_admit");
                Err(error)
            }
        }
    }

    /// Read an efficient, semantically checked status summary. This is the source of truth for UI
    /// completion; event counters are never consulted.
    pub fn get_batch_job_status_v1(&self, operation_id: &str) -> AppResult<Option<BatchJobStatusV1>> {
        self.require_batch_schema_v1()?;
        validate_operation_uuid(operation_id)?;
        self.read_batch_header_v1(operation_id)?.map(|header| self.status_from_header_v1(header)).transpose()
    }

    /// Discover the sole queued/running batch, if any. A trigger-disabled duplicate is corruption,
    /// not an arbitrary winner.
    pub fn active_batch_job_v1(&self) -> AppResult<Option<BatchJobStatusV1>> {
        self.require_batch_schema_v1()?;
        let mut statement = self.conn.prepare(
            "SELECT id FROM jobs
              WHERE kind IN ('batch_transcribe_v1','batch_normalize_v1')
                AND state IN ('queued','running')
              ORDER BY created_at,id",
        )?;
        let ids = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        if ids.len() > 1 {
            return Err(batch_evidence_error(format!("{} live batch headers exist", ids.len())));
        }
        ids.first().map(|id| self.get_batch_job_status_v1(id)).transpose().map(Option::flatten)
    }

    /// Return the next backend-bounded page of ordered pending work after `after_ordinal`. Both the
    /// 128-item cardinality and aggregate canonical-JSON byte budget are checked from SQLite length
    /// metadata before any projection JSON is fetched or decoded. `None` starts at ordinal zero; a
    /// crash-recovered worker can do the same because terminal items are excluded by durable state.
    pub fn pending_batch_item_page_v1(
        &self,
        operation_id: &str,
        after_ordinal: Option<i64>,
    ) -> AppResult<Vec<BatchPendingItemV1>> {
        self.require_batch_schema_v1()?;
        validate_operation_uuid(operation_id)?;
        if after_ordinal.is_some_and(|ordinal| ordinal < 0 || ordinal >= MAX_BATCH_ITEMS_V1 as i64) {
            return Err(AppError::Validation(format!(
                "pending-item cursor must be between 0 and {}",
                MAX_BATCH_ITEMS_V1 - 1
            )));
        }
        let header = self
            .read_batch_header_v1(operation_id)?
            .ok_or_else(|| AppError::Validation("batch operation does not exist".into()))?;
        if header.state.is_terminal() {
            return Ok(Vec::new());
        }
        let mut statement = self.conn.prepare(
            "SELECT ordinal,length(CAST(before_projection_json AS BLOB)) FROM batch_job_items_v1
              WHERE job_id=?1 AND state='pending' AND ordinal>COALESCE(?2,-1)
              ORDER BY ordinal LIMIT ?3",
        )?;
        let candidates = statement
            .query_map(params![operation_id, after_ordinal, BATCH_PENDING_PAGE_SIZE_V1 as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut encoded_bytes = 0usize;
        let mut ordinals = Vec::with_capacity(candidates.len());
        for (ordinal, projection_bytes) in candidates {
            let projection_bytes = validate_projection_json_length_v1(
                projection_bytes,
                &format!("pending batch item {operation_id}/{ordinal} before"),
            )?;
            let next_bytes = encoded_bytes.checked_add(projection_bytes).ok_or_else(|| {
                batch_evidence_error("E_BATCH_PROJECTION_LIMIT_EXCEEDED: pending page byte count overflowed")
            })?;
            if next_bytes > MAX_BATCH_PENDING_PAGE_ENCODED_BYTES_V1 {
                if ordinals.is_empty() {
                    return Err(batch_evidence_error(format!(
                        "E_BATCH_PROJECTION_LIMIT_EXCEEDED: pending batch item {operation_id}/{ordinal} cannot fit \
                         the {MAX_BATCH_PENDING_PAGE_ENCODED_BYTES_V1}-byte page budget"
                    )));
                }
                break;
            }
            encoded_bytes = next_bytes;
            ordinals.push(ordinal);
        }
        let mut pending = Vec::with_capacity(ordinals.len());
        for ordinal in ordinals {
            let item = self
                .read_batch_item_v1(operation_id, ordinal)?
                .ok_or_else(|| batch_evidence_error("pending item vanished during a stable connection read"))?;
            pending.push(BatchPendingItemV1 {
                ordinal,
                segment_id: item.segment_id.clone(),
                before: Self::decode_before_projection_v1(&item)?,
            });
        }
        Ok(pending)
    }

    /// Deep startup/restore validator. It re-hashes every before/after projection and recomputes the
    /// immutable request digest from ordered item authority.
    pub fn validate_batch_job_authority_v1(&self) -> AppResult<()> {
        self.require_batch_schema_v1()?;
        validate_batch_job_authority_on(&self.conn)
    }

    fn mark_batch_item_terminal_v1(
        &self,
        operation_id: &str,
        ordinal: i64,
        state: BatchItemStateV1,
        code: &str,
    ) -> AppResult<()> {
        if !matches!(state, BatchItemStateV1::Skipped | BatchItemStateV1::Failed | BatchItemStateV1::Abandoned) {
            return Err(batch_evidence_error("non-applied item terminalizer received an invalid state"));
        }
        validate_result_code(code)?;
        let changed = self.conn.execute(
            "UPDATE batch_job_items_v1
                SET state=?3,result_code=?4,terminal_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
              WHERE job_id=?1 AND ordinal=?2 AND state='pending'",
            params![operation_id, ordinal, state.as_str(), code],
        )?;
        if changed != 1 {
            return Err(batch_evidence_error(format!(
                "pending item {operation_id}/{ordinal} could not be terminalized"
            )));
        }
        self.advance_batch_progress_v1(operation_id)
    }

    fn advance_batch_progress_v1(&self, operation_id: &str) -> AppResult<()> {
        let (completed, total): (i64, i64) = self.conn.query_row(
            "SELECT count(*),parent.total
               FROM batch_job_items_v1 item
               JOIN jobs parent ON parent.id=item.job_id
              WHERE item.job_id=?1 AND item.state<>'pending'",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let changed = self.conn.execute(
            "UPDATE jobs
                SET completed=?2,progress=(CAST(?2 AS REAL)/CAST(total AS REAL)),
                    updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
              WHERE id=?1 AND state='running' AND total=?3",
            params![operation_id, completed, total],
        )?;
        if changed != 1 {
            return Err(batch_evidence_error(format!("running batch {operation_id} rejected durable progress")));
        }
        Ok(())
    }

    fn compare_current_to_before_v1(
        &self,
        item: &BatchItemAuthorityV1,
    ) -> AppResult<Result<BatchSegmentProjectionV1, &'static str>> {
        let Some(current) = Self::read_batch_projection_on(&self.conn, &item.segment_id)? else {
            return Ok(Err("BATCH_SEGMENT_MISSING"));
        };
        if segment_is_human_owned(&current.segment) {
            return Ok(Err("BATCH_HUMAN_OWNED"));
        }
        let (_, current_projection_sha256, current_source_sha256) = projection_authority(&current)?;
        if current_source_sha256 != item.source_identity_sha256 {
            return Ok(Err("BATCH_SOURCE_CHANGED"));
        }
        if current.review_revision != item.base_revision {
            return Ok(Err("BATCH_REVISION_CHANGED"));
        }
        if current_projection_sha256 != item.before_projection_sha256 {
            return Ok(Err("BATCH_PROJECTION_CHANGED"));
        }
        Ok(Ok(current))
    }

    fn require_batch_not_hard_stopped_v1(&self, operation_id: &str) -> AppResult<()> {
        let failed: bool = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM batch_job_items_v1
                 WHERE job_id=?1 AND state IN ('failed','abandoned')
             )",
            [operation_id],
            |row| row.get(0),
        )?;
        if failed {
            return Err(AppError::Validation(
                "BATCH_HARD_STOPPED: a durable item failure forbids every later batch mutation".into(),
            ));
        }
        Ok(())
    }

    fn existing_normalize_outcome_v1(
        item: &BatchItemAuthorityV1,
        normalized: &str,
        normalizer_version: &str,
    ) -> AppResult<BatchItemCommitOutcomeV1> {
        if item.state == BatchItemStateV1::Applied {
            let after = Self::decode_after_projection_v1(item)?
                .ok_or_else(|| batch_evidence_error("applied normalize item has no after projection"))?;
            if after.segment.normalized_transcript.as_deref() != Some(normalized)
                || after.segment.normalizer_version.as_deref() != Some(normalizer_version)
            {
                return Err(batch_evidence_error(
                    "normalize retry payload disagrees with the already-applied durable result",
                ));
            }
            return Ok(BatchItemCommitOutcomeV1::AlreadyApplied { effect_revision: after.review_revision });
        }
        Ok(BatchItemCommitOutcomeV1::AlreadyTerminal { state: item.state, code: item.result_code.clone() })
    }

    /// Commit one normalized transcript only if the exact before projection remains current.
    pub fn commit_batch_normalization_v1(
        &self,
        operation_id: &str,
        ordinal: i64,
        normalized_transcript: &str,
        normalizer_version: &str,
        executor: &BatchExecutorIdentityV1,
    ) -> AppResult<BatchItemCommitOutcomeV1> {
        self.require_batch_schema_v1()?;
        validate_operation_uuid(operation_id)?;
        crate::validation::input::validate_text(normalized_transcript, 100_000, "Normalized transcript")
            .map_err(AppError::Validation)?;
        crate::validation::input::validate_text(normalizer_version, 128, "Normalizer version")
            .map_err(AppError::Validation)?;
        if normalizer_version.trim().is_empty() {
            return Err(AppError::Validation("normalizer version must not be blank".into()));
        }
        let normalized = to_nfc(normalized_transcript);

        self.conn.execute("SAVEPOINT batch_v1_normalize_item", [])?;
        let result = (|| -> AppResult<BatchItemCommitOutcomeV1> {
            self.reserve_batch_writer()?;
            let header = self
                .read_batch_header_v1(operation_id)?
                .ok_or_else(|| AppError::Validation("batch operation does not exist".into()))?;
            if header.kind != BatchJobKindV1::Normalize {
                return Err(AppError::Validation("batch operation is not a normalize job".into()));
            }
            Self::require_batch_executor_v1(&header, executor)?;
            let item = self
                .read_batch_item_v1(operation_id, ordinal)?
                .ok_or_else(|| AppError::Validation("batch item ordinal does not exist".into()))?;
            Self::decode_before_projection_v1(&item)?;
            if item.state != BatchItemStateV1::Pending {
                return Self::existing_normalize_outcome_v1(&item, &normalized, normalizer_version);
            }
            if header.state != BatchJobLifecycleV1::Running {
                return Err(batch_evidence_error("pending normalize item belongs to a non-running job"));
            }
            self.require_batch_not_hard_stopped_v1(operation_id)?;
            let before = match self.compare_current_to_before_v1(&item)? {
                Ok(before) => before,
                Err(code) => {
                    self.mark_batch_item_terminal_v1(operation_id, ordinal, BatchItemStateV1::Skipped, code)?;
                    return Ok(BatchItemCommitOutcomeV1::Skipped { code: code.to_string() });
                }
            };
            if before.segment.normalized_transcript.as_deref() == Some(normalized.as_str())
                && before.segment.normalizer_version.as_deref() == Some(normalizer_version)
            {
                const CODE: &str = "UNCHANGED";
                self.mark_batch_item_terminal_v1(operation_id, ordinal, BatchItemStateV1::Skipped, CODE)?;
                return Ok(BatchItemCommitOutcomeV1::Skipped { code: CODE.to_string() });
            }
            let changed = self.conn.execute(
                "UPDATE speech_segments
                    SET normalized_transcript=?3,normalizer_version=?4,
                        updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE id=?1 AND review_revision=?2
                    AND verified=0
                    AND (human_decision IS NULL OR human_decision='')
                    AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))
                    AND annotated_transcript IS NULL
                    AND audio_path=?5 AND alignment_json IS ?6 AND duration_ms=?7 AND audio_content_hash IS ?8",
                params![
                    item.segment_id,
                    item.base_revision,
                    normalized,
                    normalizer_version,
                    before.segment.audio_path,
                    before.segment.alignment_json,
                    before.segment.duration_ms,
                    before.audio_content_hash,
                ],
            )?;
            if changed != 1 {
                return Err(batch_evidence_error("normalize compare-and-swap changed no row after exact precheck"));
            }
            let after = Self::read_batch_projection_on(&self.conn, &item.segment_id)?
                .ok_or_else(|| batch_evidence_error("normalized segment disappeared before evidence capture"))?;
            if after.review_revision <= item.base_revision
                || after.segment.normalized_transcript.as_deref() != Some(normalized.as_str())
                || after.segment.normalizer_version.as_deref() != Some(normalizer_version)
            {
                return Err(batch_evidence_error("normalized after projection disagrees with the intended write"));
            }
            let (after_json, after_sha256, _) = projection_authority(&after)?;
            let item_changed = self.conn.execute(
                "UPDATE batch_job_items_v1
                    SET state='applied',after_projection_json=?3,after_projection_sha256=?4,
                        effect_revision=?5,terminal_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE job_id=?1 AND ordinal=?2 AND state='pending'",
                params![operation_id, ordinal, after_json, after_sha256, after.review_revision],
            )?;
            if item_changed != 1 {
                return Err(batch_evidence_error("normalize effect could not claim its pending ledger item"));
            }
            self.advance_batch_progress_v1(operation_id)?;
            Ok(BatchItemCommitOutcomeV1::Applied { effect_revision: after.review_revision })
        })();
        match result {
            Ok(outcome) => {
                self.release_savepoint("batch_v1_normalize_item")?;
                self.track_write()?;
                Ok(outcome)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("batch_v1_normalize_item");
                Err(error)
            }
        }
    }

    fn existing_champion_outcome_v1(
        item: &BatchItemAuthorityV1,
        draft: &BatchChampionDraftV1,
        config_sha256: &str,
    ) -> AppResult<BatchItemCommitOutcomeV1> {
        if item.state == BatchItemStateV1::Applied {
            let after = Self::decode_after_projection_v1(item)?
                .ok_or_else(|| batch_evidence_error("applied champion item has no after projection"))?;
            let confidence_source = draft.confidence_source.as_deref().unwrap_or("unknown");
            let hypothesis_matches = after.hypotheses.len() == 1
                && after.hypotheses[0].model_id == draft.model_version_id
                && after.hypotheses[0].model_version_id == draft.model_version_id
                && after.hypotheses[0].transcript == to_nfc(&draft.raw_transcript)
                && after.hypotheses[0].confidence == draft.confidence;
            if after.segment.raw_transcript != to_nfc(&draft.raw_transcript)
                || after.segment.normalized_transcript != draft.normalized_transcript.as_deref().map(to_nfc)
                || after.segment.confidence != draft.confidence
                || after.segment.confidence_source.as_deref() != Some(confidence_source)
                || after.segment.model_version_id.as_deref() != Some(draft.model_version_id.as_str())
                || after.segment.cloud_call != draft.cloud_call
                || after.segment.decoder_config_hash.as_deref() != Some(config_sha256)
                || after.segment.normalizer_version != draft.normalizer_version
                || !hypothesis_matches
            {
                return Err(batch_evidence_error(
                    "champion retry payload disagrees with the already-applied durable result",
                ));
            }
            return Ok(BatchItemCommitOutcomeV1::AlreadyApplied { effect_revision: after.review_revision });
        }
        Ok(BatchItemCommitOutcomeV1::AlreadyTerminal { state: item.state, code: item.result_code.clone() })
    }

    /// Atomically publish one side-effect-free champion draft, replace its complete hypothesis set,
    /// capture the exact after projection, and advance durable progress.
    pub fn commit_batch_champion_draft_v1(
        &self,
        operation_id: &str,
        ordinal: i64,
        draft: &BatchChampionDraftV1,
        executor: &BatchExecutorIdentityV1,
    ) -> AppResult<BatchItemCommitOutcomeV1> {
        self.require_batch_schema_v1()?;
        validate_operation_uuid(operation_id)?;
        refuse_blank_asr_persist("batch item", &draft.raw_transcript)?;
        if crate::quality::is_placeholder_transcript(&draft.raw_transcript) {
            return Err(AppError::Validation("champion draft is a placeholder, not transcript truth".into()));
        }
        crate::validation::input::validate_text(&draft.raw_transcript, 100_000, "Champion transcript")
            .map_err(AppError::Validation)?;
        if let Some(normalized) = draft.normalized_transcript.as_deref() {
            crate::validation::input::validate_text(normalized, 100_000, "Normalized transcript")
                .map_err(AppError::Validation)?;
        }
        crate::validation::input::validate_identifier(&draft.model_version_id).map_err(AppError::Validation)?;
        validate_sha256(&draft.deployment_sha256, "champion deployment hash")?;
        if draft.normalized_transcript.is_some() != draft.normalizer_version.is_some() {
            return Err(AppError::Validation(
                "normalized champion text and its normalizer version must either both be present or both be absent"
                    .into(),
            ));
        }
        if let Some(version) = draft.normalizer_version.as_deref() {
            crate::validation::input::validate_text(version, 128, "Normalizer version")
                .map_err(AppError::Validation)?;
            if version.trim().is_empty() {
                return Err(AppError::Validation("normalizer version must not be blank".into()));
            }
        }
        if let Some(source) = draft.confidence_source.as_deref() {
            crate::validation::input::validate_text(source, 128, "Confidence source").map_err(AppError::Validation)?;
            if source.trim().is_empty() {
                return Err(AppError::Validation("confidence source must not be blank".into()));
            }
        }
        if draft.confidence.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
            return Err(AppError::Validation("champion confidence must be finite and between 0 and 1".into()));
        }

        self.conn.execute("SAVEPOINT batch_v1_champion_item", [])?;
        let result = (|| -> AppResult<BatchItemCommitOutcomeV1> {
            self.reserve_batch_writer()?;
            let header = self
                .read_batch_header_v1(operation_id)?
                .ok_or_else(|| AppError::Validation("batch operation does not exist".into()))?;
            if header.kind != BatchJobKindV1::Transcribe {
                return Err(AppError::Validation("batch operation is not a transcribe job".into()));
            }
            let payload = Self::require_batch_executor_v1(&header, executor)?;
            let item = self
                .read_batch_item_v1(operation_id, ordinal)?
                .ok_or_else(|| AppError::Validation("batch item ordinal does not exist".into()))?;
            Self::decode_before_projection_v1(&item)?;
            if item.state != BatchItemStateV1::Pending {
                return Self::existing_champion_outcome_v1(&item, draft, &payload.config_sha256);
            }
            if header.state != BatchJobLifecycleV1::Running {
                return Err(batch_evidence_error("pending transcribe item belongs to a non-running job"));
            }
            self.require_batch_not_hard_stopped_v1(operation_id)?;
            let before = match self.compare_current_to_before_v1(&item)? {
                Ok(before) => before,
                Err(code) => {
                    self.mark_batch_item_terminal_v1(operation_id, ordinal, BatchItemStateV1::Skipped, code)?;
                    return Ok(BatchItemCommitOutcomeV1::Skipped { code: code.to_string() });
                }
            };
            let raw = to_nfc(&draft.raw_transcript);
            validate_stored_hypothesis_payload(&item.segment_id, &draft.model_version_id, &raw)?;
            let normalized = draft.normalized_transcript.as_deref().map(to_nfc);
            let champion_is_current: bool = self.conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM model_versions
                     WHERE id=?1 AND family='omniasr-7b' AND status='champion'
                       AND checkpoint_sha256=?2
                 )",
                params![draft.model_version_id, draft.deployment_sha256],
                |row| row.get(0),
            )?;
            if !champion_is_current {
                const CODE: &str = "MODEL_IDENTITY_CHANGED";
                self.mark_batch_item_terminal_v1(operation_id, ordinal, BatchItemStateV1::Failed, CODE)?;
                return Ok(BatchItemCommitOutcomeV1::Failed { code: CODE.to_string() });
            }
            let confidence_source = draft.confidence_source.as_deref().unwrap_or("unknown");
            let changed = self.conn.execute(
                "UPDATE speech_segments
                    SET raw_transcript=?3,normalized_transcript=?4,confidence=?5,
                        confidence_source=?6,model_version_id=?7,cloud_call=?8,
                        decoder_config_hash=?9,normalizer_version=?10,
                        updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE id=?1 AND review_revision=?2
                    AND verified=0
                    AND (human_decision IS NULL OR human_decision='')
                    AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))
                    AND annotated_transcript IS NULL
                    AND audio_path=?11 AND alignment_json IS ?12 AND duration_ms=?13 AND audio_content_hash IS ?14",
                params![
                    item.segment_id,
                    item.base_revision,
                    raw,
                    normalized,
                    draft.confidence,
                    confidence_source,
                    draft.model_version_id,
                    draft.cloud_call as i32,
                    payload.config_sha256,
                    draft.normalizer_version,
                    before.segment.audio_path,
                    before.segment.alignment_json,
                    before.segment.duration_ms,
                    before.audio_content_hash,
                ],
            )?;
            if changed != 1 {
                return Err(batch_evidence_error("champion compare-and-swap changed no row after exact precheck"));
            }
            self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id=?1", [&item.segment_id])?;
            self.conn.execute(
                "INSERT INTO segment_hypotheses(
                     segment_id,model_id,transcript,confidence,model_version_id,created_at)
                 VALUES(?1,?2,?3,?4,?2,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                params![item.segment_id, draft.model_version_id, raw, draft.confidence],
            )?;
            let after = Self::read_batch_projection_on(&self.conn, &item.segment_id)?
                .ok_or_else(|| batch_evidence_error("transcribed segment disappeared before evidence capture"))?;
            if after.review_revision <= item.base_revision
                || after.segment.raw_transcript != raw
                || after.segment.normalized_transcript != normalized
                || after.segment.normalizer_version != draft.normalizer_version
                || after.hypotheses.len() != 1
                || after.hypotheses[0].model_id != draft.model_version_id
                || after.hypotheses[0].model_version_id != draft.model_version_id
                || after.hypotheses[0].transcript != raw
            {
                return Err(batch_evidence_error("champion after projection disagrees with the intended write"));
            }
            let (after_json, after_sha256, _) = projection_authority(&after)?;
            let item_changed = self.conn.execute(
                "UPDATE batch_job_items_v1
                    SET state='applied',after_projection_json=?3,after_projection_sha256=?4,
                        effect_revision=?5,terminal_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE job_id=?1 AND ordinal=?2 AND state='pending'",
                params![operation_id, ordinal, after_json, after_sha256, after.review_revision],
            )?;
            if item_changed != 1 {
                return Err(batch_evidence_error("champion effect could not claim its pending ledger item"));
            }
            self.advance_batch_progress_v1(operation_id)?;
            Ok(BatchItemCommitOutcomeV1::Applied { effect_revision: after.review_revision })
        })();
        match result {
            Ok(outcome) => {
                self.release_savepoint("batch_v1_champion_item")?;
                self.track_write()?;
                Ok(outcome)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("batch_v1_champion_item");
                Err(error)
            }
        }
    }
}

/// Connection-level form used by startup and staged-restore validation before a `Database` facade
/// is available. Schemas before v68 have no batch journal and are intentionally a no-op.
pub(crate) fn validate_batch_job_authority_on(conn: &Connection) -> AppResult<()> {
    let schema_version: i64 =
        conn.query_row("SELECT COALESCE(MAX(version),0) FROM schema_migrations", [], |row| row.get(0))?;
    if schema_version < 68 {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT id FROM jobs WHERE kind IN ('batch_transcribe_v1','batch_normalize_v1') ORDER BY created_at,id",
    )?;
    let job_ids = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut live = 0usize;
    for operation_id in job_ids {
        let status = validate_one_batch_job_authority_on(conn, &operation_id)?;
        if matches!(status.state, BatchJobLifecycleV1::Queued | BatchJobLifecycleV1::Running) {
            live += 1;
        }
    }
    if live > 1 {
        return Err(batch_evidence_error(format!("{live} live batch headers exist")));
    }
    Ok(())
}

/// Validate one immutable batch authority without making normal history latency proportional to
/// every batch ever recorded. The global startup/restore validator above calls this for every id.
fn validate_one_batch_job_authority_on(conn: &Connection, operation_id: &str) -> AppResult<BatchJobStatusV1> {
    let header = read_batch_header_on(conn, operation_id)?
        .ok_or_else(|| AppError::Validation("batch operation does not exist".into()))?;
    let payload = Database::parse_batch_payload_v1(&header)?;
    let status = status_from_header_on(conn, header)?;
    let mut request_digest = BatchRequestDigestV1::new(operation_id, status.kind, &payload.config_sha256)?;
    for ordinal in 0..status.total {
        let item = read_batch_item_on(conn, operation_id, ordinal)?
            .ok_or_else(|| batch_evidence_error(format!("job {operation_id} is missing ordinal {ordinal}")))?;
        let before = Database::decode_before_projection_v1(&item)?;
        match item.state {
            BatchItemStateV1::Pending => {
                if item.after_projection_json.is_some()
                    || item.after_projection_sha256.is_some()
                    || item.effect_revision.is_some()
                    || item.result_code.is_some()
                {
                    return Err(batch_evidence_error(format!(
                        "pending item {operation_id}/{ordinal} carries terminal evidence"
                    )));
                }
            }
            BatchItemStateV1::Applied => {
                if item.result_code.is_some() || Database::decode_after_projection_v1(&item)?.is_none() {
                    return Err(batch_evidence_error(format!(
                        "applied item {operation_id}/{ordinal} lacks exact after authority"
                    )));
                }
            }
            BatchItemStateV1::Skipped | BatchItemStateV1::Failed | BatchItemStateV1::Abandoned => {
                if item.result_code.as_deref().is_none()
                    || item.after_projection_json.is_some()
                    || item.after_projection_sha256.is_some()
                    || item.effect_revision.is_some()
                {
                    return Err(batch_evidence_error(format!(
                        "non-applied terminal item {operation_id}/{ordinal} has malformed evidence"
                    )));
                }
            }
        }
        request_digest.push(&BatchRequestItemAuthorityV1 {
            ordinal,
            segment_id: before.segment.id,
            base_revision: before.review_revision,
            source_identity_sha256: item.source_identity_sha256,
            before_projection_sha256: item.before_projection_sha256,
        })?;
    }
    let request_sha256 = request_digest.finish();
    if request_sha256 != status.request_sha256 {
        return Err(batch_evidence_error(format!("job {operation_id} request digest does not match its items")));
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TOKEN_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const GIT_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const DEPLOYMENT_SHA: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn executor() -> BatchExecutorIdentityV1 {
        BatchExecutorIdentityV1 { git_sha: GIT_SHA.into(), token_sha256: TOKEN_SHA.into(), attempt_generation: 1 }
    }

    fn fixture(ids: &[&str]) -> Database {
        let database = Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        for (index, id) in ids.iter().enumerate() {
            database
                .connection()
                .execute(
                    "INSERT INTO speech_segments(
                         id,audio_path,raw_transcript,duration_ms,audio_content_hash)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![
                        id,
                        format!("C:/audio/{id}.wav"),
                        format!("raw {id}"),
                        1_000 + index as i64,
                        format!("pcm-{id}")
                    ],
                )
                .unwrap();
        }
        database
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn insert_champion(database: &Database) {
        database
            .connection()
            .execute(
                "INSERT INTO model_versions(
                     id,family,checkpoint_sha256,checkpoint_path,source,license,status)
                 VALUES('champion-v1','omniasr-7b',?1,'C:/model','user-finetuned','Apache-2.0','champion')",
                [DEPLOYMENT_SHA],
            )
            .unwrap();
    }

    #[test]
    fn streaming_request_digest_is_canonical_and_supports_the_exact_maximum() {
        let sample_items = vec![
            BatchRequestItemAuthorityV1 {
                ordinal: 0,
                segment_id: "sorani-\u{0695}-0".into(),
                base_revision: 4,
                source_identity_sha256: CONFIG_SHA.into(),
                before_projection_sha256: TOKEN_SHA.into(),
            },
            BatchRequestItemAuthorityV1 {
                ordinal: 1,
                segment_id: "quoted.segment".into(),
                base_revision: 9,
                source_identity_sha256: TOKEN_SHA.into(),
                before_projection_sha256: CONFIG_SHA.into(),
            },
        ];
        let canonical = BatchRequestAuthorityV1 {
            schema: BATCH_SCHEMA_V1,
            operation_id: "00000000-0000-4000-8000-000000000001".into(),
            kind: BatchJobKindV1::Normalize,
            config_sha256: CONFIG_SHA.into(),
            items: sample_items.clone(),
        };
        let (_, expected) = sha256_json(&canonical).unwrap();
        let mut streamed =
            BatchRequestDigestV1::new("00000000-0000-4000-8000-000000000001", BatchJobKindV1::Normalize, CONFIG_SHA)
                .unwrap();
        for item in &sample_items {
            streamed.push(item).unwrap();
        }
        assert_eq!(streamed.finish(), expected);

        assert!(validate_batch_item_count_v1(MAX_BATCH_ITEMS_V1).is_ok());
        assert!(validate_batch_item_count_v1(0).is_err());
        assert!(validate_batch_item_count_v1(MAX_BATCH_ITEMS_V1 + 1).is_err());

        // Exercise the maximum-length digest path itself without constructing a second 100,000-row
        // authority vector. This is a stable bounded-memory proxy for the supported request edge.
        let mut maximum =
            BatchRequestDigestV1::new("00000000-0000-4000-8000-000000000002", BatchJobKindV1::Transcribe, CONFIG_SHA)
                .unwrap();
        for ordinal in 0..MAX_BATCH_ITEMS_V1 {
            maximum
                .push(&BatchRequestItemAuthorityV1 {
                    ordinal: ordinal as i64,
                    segment_id: format!("segment-{ordinal}"),
                    base_revision: ordinal as i64,
                    source_identity_sha256: CONFIG_SHA.into(),
                    before_projection_sha256: TOKEN_SHA.into(),
                })
                .unwrap();
        }
        validate_sha256(&maximum.finish(), "maximum request digest").unwrap();
    }

    #[test]
    fn admission_and_pending_pages_have_fixed_bounded_live_sets_and_exact_order() {
        let count = BATCH_PENDING_PAGE_SIZE_V1 * 2 + 17;
        let database = Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        let large_text = "\u{06a9}".repeat(32 * 1024);
        let mut segment_ids = Vec::with_capacity(count);
        for ordinal in 0..count {
            let segment_id = format!("large-{ordinal:04}");
            database
                .connection()
                .execute(
                    "INSERT INTO speech_segments(id,audio_path,raw_transcript,duration_ms,audio_content_hash)
                     VALUES(?1,?2,?3,1000,?4)",
                    params![segment_id, format!("C:/audio/{ordinal}.wav"), large_text, format!("pcm-{ordinal}")],
                )
                .unwrap();
            segment_ids.push(format!("large-{ordinal:04}"));
        }

        BATCH_ADMISSION_PROJECTION_LIVE.with(|live| live.set(0));
        BATCH_ADMISSION_PROJECTION_PEAK.with(|peak| peak.set(0));
        let operation_id = "10000000-0000-4000-8000-000000000004";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &segment_ids, CONFIG_SHA, &executor())
            .unwrap();
        assert_eq!(BATCH_ADMISSION_PROJECTION_LIVE.with(std::cell::Cell::get), 0);
        assert_eq!(
            BATCH_ADMISSION_PROJECTION_PEAK.with(std::cell::Cell::get),
            1,
            "admission retained more than one full projection at a time"
        );

        let mut cursor = None;
        let mut seen = Vec::with_capacity(count);
        loop {
            let page = database.pending_batch_item_page_v1(operation_id, cursor).unwrap();
            assert!(page.len() <= BATCH_PENDING_PAGE_SIZE_V1);
            if page.is_empty() {
                break;
            }
            for item in &page {
                if let Some(previous) = seen.last() {
                    assert!(item.ordinal > *previous, "pending paging repeated or reordered an ordinal");
                }
                seen.push(item.ordinal);
            }
            cursor = page.last().map(|item| item.ordinal);
        }
        assert_eq!(seen, (0..count as i64).collect::<Vec<_>>());
        assert!(database.pending_batch_item_page_v1(operation_id, Some(MAX_BATCH_ITEMS_V1 as i64)).is_err());
    }

    #[test]
    fn pending_pages_obey_encoded_byte_budget_without_skips_or_reordering() {
        let count = 60usize;
        let database = Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        // One control byte becomes the six-byte JSON escape "\\u0001".
        let escaping_text = "\u{0001}".repeat(200 * 1024);
        let mut segment_ids = Vec::with_capacity(count);
        for ordinal in 0..count {
            let segment_id = format!("byte-page-{ordinal:03}");
            database
                .connection()
                .execute(
                    "INSERT INTO speech_segments(id,audio_path,raw_transcript,duration_ms,audio_content_hash)
                     VALUES(?1,?2,?3,1000,?4)",
                    params![
                        segment_id,
                        format!("C:/audio/byte-page-{ordinal}.wav"),
                        escaping_text,
                        format!("pcm-byte-page-{ordinal}")
                    ],
                )
                .unwrap();
            segment_ids.push(format!("byte-page-{ordinal:03}"));
        }
        let operation_id = "10000000-0000-4000-8000-000000000005";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &segment_ids, CONFIG_SHA, &executor())
            .unwrap();

        let mut cursor = None;
        let mut seen = Vec::with_capacity(count);
        let mut page_sizes = Vec::new();
        loop {
            let page = database.pending_batch_item_page_v1(operation_id, cursor).unwrap();
            if page.is_empty() {
                break;
            }
            let encoded_bytes = page.iter().map(|item| canonical_json(&item.before).unwrap().len()).sum::<usize>();
            assert!(encoded_bytes <= MAX_BATCH_PENDING_PAGE_ENCODED_BYTES_V1);
            page_sizes.push(page.len());
            seen.extend(page.iter().map(|item| item.ordinal));
            cursor = page.last().map(|item| item.ordinal);
        }
        assert!(page_sizes.len() >= 2, "fixture must exercise the byte boundary: {page_sizes:?}");
        assert!(page_sizes[0] < BATCH_PENDING_PAGE_SIZE_V1);
        assert_eq!(seen, (0..count as i64).collect::<Vec<_>>());
    }

    #[test]
    fn oversized_legacy_projection_is_refused_before_full_rust_materialization() {
        let oversized_segment = fixture(&["oversized-segment"]);
        oversized_segment
            .connection()
            .execute(
                "UPDATE speech_segments SET raw_transcript=?2 WHERE id=?1",
                params!["oversized-segment", "x".repeat(MAX_BATCH_SEGMENT_TEXT_FIELD_BYTES_V1 + 1)],
            )
            .unwrap();
        BATCH_ADMISSION_PROJECTION_LIVE.with(|live| live.set(0));
        BATCH_ADMISSION_PROJECTION_PEAK.with(|peak| peak.set(0));
        let segment_error = oversized_segment
            .admit_batch_job_v1(
                "10000000-0000-4000-8000-000000000006",
                BatchJobKindV1::Normalize,
                &ids(&["oversized-segment"]),
                CONFIG_SHA,
                &executor(),
            )
            .expect_err("oversized legacy segment text must fail closed")
            .to_string();
        assert!(segment_error.contains("E_BATCH_PROJECTION_LIMIT_EXCEEDED"), "{segment_error}");
        assert_eq!(BATCH_ADMISSION_PROJECTION_PEAK.with(std::cell::Cell::get), 0);

        let oversized_hypothesis = fixture(&["oversized-hypothesis"]);
        oversized_hypothesis
            .connection()
            .execute(
                "INSERT INTO segment_hypotheses(segment_id,model_id,transcript,confidence)
                 VALUES('oversized-hypothesis','legacy-model',?1,0.5)",
                ["y".repeat(MAX_STORED_HYPOTHESIS_TRANSCRIPT_BYTES + 1)],
            )
            .unwrap();
        BATCH_ADMISSION_PROJECTION_LIVE.with(|live| live.set(0));
        BATCH_ADMISSION_PROJECTION_PEAK.with(|peak| peak.set(0));
        let hypothesis_error = oversized_hypothesis
            .admit_batch_job_v1(
                "10000000-0000-4000-8000-000000000007",
                BatchJobKindV1::Normalize,
                &ids(&["oversized-hypothesis"]),
                CONFIG_SHA,
                &executor(),
            )
            .expect_err("oversized legacy hypothesis must fail closed")
            .to_string();
        assert!(hypothesis_error.contains("E_BATCH_PROJECTION_LIMIT_EXCEEDED"), "{hypothesis_error}");
        assert_eq!(BATCH_ADMISSION_PROJECTION_PEAK.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn projection_limits_accept_the_exact_supported_text_boundaries_without_truncation() {
        let database = fixture(&["exact-projection-boundary"]);
        let exact_segment_text = "s".repeat(MAX_BATCH_SEGMENT_TEXT_FIELD_BYTES_V1);
        database
            .connection()
            .execute(
                "UPDATE speech_segments SET raw_transcript=?2 WHERE id=?1",
                params!["exact-projection-boundary", exact_segment_text],
            )
            .unwrap();
        let exact_hypothesis_text = "h".repeat(MAX_STORED_HYPOTHESIS_TRANSCRIPT_BYTES);
        database
            .insert_hypothesis(&SegmentHypothesis {
                segment_id: "exact-projection-boundary".into(),
                model_id: "exact-boundary-model".into(),
                transcript: exact_hypothesis_text.clone(),
                confidence: Some(0.5),
            })
            .unwrap();
        let operation_id = "10000000-0000-4000-8000-000000000008";
        database
            .admit_batch_job_v1(
                operation_id,
                BatchJobKindV1::Normalize,
                &ids(&["exact-projection-boundary"]),
                CONFIG_SHA,
                &executor(),
            )
            .unwrap();
        let page = database.pending_batch_item_page_v1(operation_id, None).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].before.segment.raw_transcript.len(), MAX_BATCH_SEGMENT_TEXT_FIELD_BYTES_V1);
        assert_eq!(page[0].before.hypotheses[0].transcript, exact_hypothesis_text);
    }

    #[test]
    fn shared_hypothesis_writers_enforce_caps_without_deleting_truth() {
        let database = fixture(&["hypothesis-bounds"]);
        let original = SegmentHypothesis {
            segment_id: "hypothesis-bounds".into(),
            model_id: "original-model".into(),
            transcript: "original truth".into(),
            confidence: Some(0.9),
        };
        database.insert_hypothesis(&original).unwrap();
        let oversized = SegmentHypothesis {
            segment_id: original.segment_id.clone(),
            model_id: "replacement-model".into(),
            transcript: "z".repeat(MAX_STORED_HYPOTHESIS_TRANSCRIPT_BYTES + 1),
            confidence: Some(0.8),
        };
        let replace_error = database
            .replace_hypotheses_with(&oversized)
            .expect_err("oversized replacement must be refused before delete")
            .to_string();
        assert!(replace_error.contains("E_HYPOTHESIS_LIMIT_EXCEEDED"), "{replace_error}");
        let retained = database.get_hypotheses_for_segment("hypothesis-bounds").unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].model_id, "original-model");
        assert_eq!(retained[0].transcript, "original truth");

        for ordinal in 1..MAX_STORED_HYPOTHESES_PER_SEGMENT {
            database
                .insert_hypothesis(&SegmentHypothesis {
                    segment_id: "hypothesis-bounds".into(),
                    model_id: format!("model-{ordinal}"),
                    transcript: "vote".into(),
                    confidence: None,
                })
                .unwrap();
        }
        let count_error = database
            .insert_hypothesis(&SegmentHypothesis {
                segment_id: "hypothesis-bounds".into(),
                model_id: "model-overflow".into(),
                transcript: "vote".into(),
                confidence: None,
            })
            .expect_err("65th hypothesis must fail closed")
            .to_string();
        assert!(count_error.contains("E_HYPOTHESIS_LIMIT_EXCEEDED"), "{count_error}");

        let aggregate_database = fixture(&["hypothesis-aggregate"]);
        let half_cap = "a".repeat(MAX_STORED_HYPOTHESIS_TRANSCRIPT_BYTES / 2);
        for ordinal in 0..8 {
            aggregate_database
                .insert_hypothesis(&SegmentHypothesis {
                    segment_id: "hypothesis-aggregate".into(),
                    model_id: format!("aggregate-{ordinal}"),
                    transcript: half_cap.clone(),
                    confidence: None,
                })
                .unwrap();
        }
        let aggregate_error = aggregate_database
            .insert_hypothesis(&SegmentHypothesis {
                segment_id: "hypothesis-aggregate".into(),
                model_id: "aggregate-overflow".into(),
                transcript: half_cap,
                confidence: None,
            })
            .expect_err("aggregate hypothesis bytes above the cap must fail closed")
            .to_string();
        assert!(aggregate_error.contains("E_HYPOTHESIS_LIMIT_EXCEEDED"), "{aggregate_error}");
    }

    #[test]
    fn admission_is_atomic_ordered_and_refuses_missing_or_duplicate_ids() {
        let database = fixture(&["s1", "s2"]);
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let cancelled_admission = database.admit_batch_job_v1_cancellable(
            "10000000-0000-4000-8000-000000000000",
            BatchJobKindV1::Normalize,
            &ids(&["s1", "s2"]),
            CONFIG_SHA,
            &executor(),
            &cancelled,
        );
        assert!(cancelled_admission
            .expect_err("pre-cancelled admission must not publish a journal")
            .to_string()
            .contains("BATCH_ADMISSION_CANCELLED"));
        let missing = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-000000000001",
            BatchJobKindV1::Normalize,
            &ids(&["s1", "missing"]),
            CONFIG_SHA,
            &executor(),
        );
        assert!(missing.is_err());
        assert_eq!(
            database.connection().query_row("SELECT count(*) FROM jobs", [], |row| row.get::<_, i64>(0)).unwrap(),
            0
        );
        assert_eq!(
            database
                .connection()
                .query_row("SELECT count(*) FROM batch_job_items_v1", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );

        let duplicate = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-000000000002",
            BatchJobKindV1::Normalize,
            &ids(&["s1", "s1"]),
            CONFIG_SHA,
            &executor(),
        );
        assert!(duplicate.is_err());

        let status = database
            .admit_batch_job_v1(
                "10000000-0000-4000-8000-000000000003",
                BatchJobKindV1::Normalize,
                &ids(&["s2", "s1"]),
                CONFIG_SHA,
                &executor(),
            )
            .unwrap();
        assert_eq!(status.state, BatchJobLifecycleV1::Running);
        let ordered = database
            .connection()
            .prepare("SELECT segment_id FROM batch_job_items_v1 ORDER BY ordinal")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(ordered, vec!["s2", "s1"]);
        database.validate_batch_job_authority_v1().unwrap();
    }

    #[test]
    fn normalize_commit_is_revision_safe_progress_exact_and_retry_idempotent() {
        let database = fixture(&["s1", "s2"]);
        let operation_id = "20000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1", "s2"]), CONFIG_SHA, &executor())
            .unwrap();
        let applied = database
            .commit_batch_normalization_v1(operation_id, 0, "normalized one", "normalizer-v1", &executor())
            .unwrap();
        assert!(matches!(applied, BatchItemCommitOutcomeV1::Applied { effect_revision: 1 }));
        let replay = database
            .commit_batch_normalization_v1(operation_id, 0, "normalized one", "normalizer-v1", &executor())
            .unwrap();
        assert!(matches!(replay, BatchItemCommitOutcomeV1::AlreadyApplied { effect_revision: 1 }));
        let status = database.get_batch_job_status_v1(operation_id).unwrap().unwrap();
        assert_eq!(status.completed, 1);
        assert_eq!(status.counts.applied, 1);
        assert!((status.progress - 0.5).abs() < 1e-12);

        database.connection().execute("UPDATE speech_segments SET verified=1 WHERE id='s2'", []).unwrap();
        let skipped = database
            .commit_batch_normalization_v1(operation_id, 1, "must not land", "normalizer-v1", &executor())
            .unwrap();
        assert_eq!(skipped, BatchItemCommitOutcomeV1::Skipped { code: "BATCH_HUMAN_OWNED".into() });
        let unchanged: Option<String> = database
            .connection()
            .query_row("SELECT normalized_transcript FROM speech_segments WHERE id='s2'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(unchanged, None);
        let terminal =
            database.finish_batch_job_v1(operation_id, BatchTerminalIntentV1::Succeeded, &executor()).unwrap();
        assert_eq!(terminal.state, BatchJobLifecycleV1::Succeeded);
        assert_eq!(terminal.counts, BatchItemCountsV1 { applied: 1, skipped: 1, ..Default::default() });
        database.validate_batch_job_authority_v1().unwrap();
    }

    #[test]
    fn normalize_noop_and_present_empty_annotation_are_safe_skips_without_revision_write() {
        let database = fixture(&["s1", "s2"]);
        database
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET normalized_transcript='already',normalizer_version='normalizer-v1'
                  WHERE id='s1'",
                [],
            )
            .unwrap();
        database.connection().execute("UPDATE speech_segments SET annotated_transcript='' WHERE id='s2'", []).unwrap();
        let operation_id = "21000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1", "s2"]), CONFIG_SHA, &executor())
            .unwrap();
        let before_s1: i64 = database
            .connection()
            .query_row("SELECT review_revision FROM speech_segments WHERE id='s1'", [], |row| row.get(0))
            .unwrap();
        let unchanged =
            database.commit_batch_normalization_v1(operation_id, 0, "already", "normalizer-v1", &executor()).unwrap();
        assert_eq!(unchanged, BatchItemCommitOutcomeV1::Skipped { code: "UNCHANGED".into() });
        let after_s1: i64 = database
            .connection()
            .query_row("SELECT review_revision FROM speech_segments WHERE id='s1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after_s1, before_s1, "a normalization no-op must not advance segment authority");

        let before_s2: i64 = database
            .connection()
            .query_row("SELECT review_revision FROM speech_segments WHERE id='s2'", [], |row| row.get(0))
            .unwrap();
        let annotated = database
            .commit_batch_normalization_v1(operation_id, 1, "must not land", "normalizer-v1", &executor())
            .unwrap();
        assert_eq!(annotated, BatchItemCommitOutcomeV1::Skipped { code: "BATCH_HUMAN_OWNED".into() });
        let after_s2: i64 = database
            .connection()
            .query_row("SELECT review_revision FROM speech_segments WHERE id='s2'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after_s2, before_s2);
    }

    #[test]
    fn wrong_executor_token_or_generation_cannot_mutate_or_terminalize() {
        let database = fixture(&["s1"]);
        let operation_id = "22000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        let wrong_token = BatchExecutorIdentityV1 {
            git_sha: GIT_SHA.into(),
            token_sha256: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into(),
            attempt_generation: 1,
        };
        let error = database
            .commit_batch_normalization_v1(operation_id, 0, "forged", "normalizer-v1", &wrong_token)
            .unwrap_err()
            .to_string();
        assert!(error.contains("E_BATCH_EXECUTOR_MISMATCH"), "{error}");
        let stale_generation =
            BatchExecutorIdentityV1 { git_sha: GIT_SHA.into(), token_sha256: TOKEN_SHA.into(), attempt_generation: 2 };
        let error = database
            .finish_batch_job_v1(
                operation_id,
                BatchTerminalIntentV1::Failed { code: "FORGED_STOP".into() },
                &stale_generation,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("E_BATCH_EXECUTOR_MISMATCH"), "{error}");
        let status = database.get_batch_job_status_v1(operation_id).unwrap().unwrap();
        assert_eq!(status.state, BatchJobLifecycleV1::Running);
        assert_eq!(status.counts.pending, 1);
        let normalized: Option<String> = database
            .connection()
            .query_row("SELECT normalized_transcript FROM speech_segments WHERE id='s1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(normalized, None);
    }

    #[test]
    fn initial_history_token_refuses_a_same_value_later_authority_write() {
        let database = fixture(&["s1"]);
        let operation_id = "23000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        database.commit_batch_normalization_v1(operation_id, 0, "normalized", "normalizer-v1", &executor()).unwrap();
        database.finish_batch_job_v1(operation_id, BatchTerminalIntentV1::Succeeded, &executor()).unwrap();
        database
            .connection()
            .execute("UPDATE speech_segments SET normalized_transcript=normalized_transcript WHERE id='s1'", [])
            .unwrap();
        let error = database.batch_job_history_token_v1(operation_id).unwrap_err().to_string();
        assert!(error.contains("BATCH_HISTORY_CONFLICT"), "{error}");
    }

    #[test]
    fn history_inverse_streams_large_projections_with_one_prepared_item_live() {
        let database = Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        let count = 12usize;
        let large_raw = "r".repeat(256 * 1024);
        // Stay just below the existing 100,000-character normalization product boundary while
        // keeping every journal endpoint materially large for the deterministic live-set proof.
        let large_normalized = "n".repeat(96 * 1024);
        let mut segment_ids = Vec::with_capacity(count);
        for ordinal in 0..count {
            let segment_id = format!("history-large-{ordinal:02}");
            database
                .connection()
                .execute(
                    "INSERT INTO speech_segments(id,audio_path,raw_transcript,duration_ms,audio_content_hash)
                     VALUES(?1,?2,?3,1000,?4)",
                    params![
                        segment_id,
                        format!("C:/audio/history-large-{ordinal:02}.wav"),
                        large_raw,
                        format!("pcm-history-large-{ordinal:02}")
                    ],
                )
                .unwrap();
            segment_ids.push(format!("history-large-{ordinal:02}"));
        }
        let operation_id = "23000000-0000-4000-8000-000000000002";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &segment_ids, CONFIG_SHA, &executor())
            .unwrap();
        for ordinal in 0..count {
            database
                .commit_batch_normalization_v1(
                    operation_id,
                    ordinal as i64,
                    &large_normalized,
                    "normalizer-v1",
                    &executor(),
                )
                .unwrap();
        }
        database.finish_batch_job_v1(operation_id, BatchTerminalIntentV1::Succeeded, &executor()).unwrap();
        let after_token = database.batch_job_history_token_v1(operation_id).unwrap().unwrap();

        BATCH_HISTORY_PREPARED_LIVE.with(|live| live.set(0));
        BATCH_HISTORY_PREPARED_PEAK.with(|peak| peak.set(0));
        let before_token = database.apply_batch_job_history_v1(&after_token).unwrap();
        assert_eq!(BATCH_HISTORY_PREPARED_LIVE.with(std::cell::Cell::get), 0);
        assert_eq!(
            BATCH_HISTORY_PREPARED_PEAK.with(std::cell::Cell::get),
            1,
            "history retained more than one full target projection"
        );
        let restored_before: i64 = database
            .connection()
            .query_row("SELECT count(*) FROM speech_segments WHERE normalized_transcript IS NULL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(restored_before, count as i64);

        BATCH_HISTORY_PREPARED_PEAK.with(|peak| peak.set(0));
        let restored_after_token = database.apply_batch_job_history_v1(&before_token).unwrap();
        assert_eq!(restored_after_token.expected_side, BatchHistorySideV1::After);
        assert_eq!(BATCH_HISTORY_PREPARED_LIVE.with(std::cell::Cell::get), 0);
        assert_eq!(BATCH_HISTORY_PREPARED_PEAK.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn streamed_history_inverse_rolls_back_every_prior_item_on_late_write_failure() {
        let database = fixture(&["history-atomic-a", "history-atomic-b"]);
        let operation_id = "23000000-0000-4000-8000-000000000003";
        database
            .admit_batch_job_v1(
                operation_id,
                BatchJobKindV1::Normalize,
                &ids(&["history-atomic-a", "history-atomic-b"]),
                CONFIG_SHA,
                &executor(),
            )
            .unwrap();
        database.commit_batch_normalization_v1(operation_id, 0, "normalized-a", "normalizer-v1", &executor()).unwrap();
        database.commit_batch_normalization_v1(operation_id, 1, "normalized-b", "normalizer-v1", &executor()).unwrap();
        database.finish_batch_job_v1(operation_id, BatchTerminalIntentV1::Succeeded, &executor()).unwrap();
        let after_token = database.batch_job_history_token_v1(operation_id).unwrap().unwrap();
        database
            .connection()
            .execute_batch(
                "CREATE TRIGGER refuse_second_history_inverse
                 BEFORE UPDATE OF normalized_transcript ON speech_segments
                 WHEN OLD.id='history-atomic-b' AND NEW.normalized_transcript IS NULL
                 BEGIN SELECT RAISE(ABORT, 'late history fixture refusal'); END;",
            )
            .unwrap();

        assert!(database.apply_batch_job_history_v1(&after_token).is_err());
        let values = database
            .connection()
            .prepare(
                "SELECT normalized_transcript FROM speech_segments
                 WHERE id IN ('history-atomic-a','history-atomic-b') ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(values, vec![Some("normalized-a".into()), Some("normalized-b".into())]);
        database.connection().execute("DROP TRIGGER refuse_second_history_inverse", []).unwrap();
        database.apply_batch_job_history_v1(&after_token).unwrap();
    }

    #[test]
    fn champion_commit_captures_and_replaces_complete_hypothesis_authority() {
        let database = fixture(&["s1"]);
        insert_champion(&database);
        database
            .connection()
            .execute(
                "INSERT INTO segment_hypotheses(
                     segment_id,model_id,transcript,confidence,model_version_id,created_at)
                 VALUES('s1','old-a','old one',0.2,'old-a@1','2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        database
            .connection()
            .execute(
                "INSERT INTO segment_hypotheses(
                     segment_id,model_id,transcript,confidence,model_version_id,created_at)
                 VALUES('s1','old-b','old two',0.3,'old-b@2','2026-01-02T00:00:00Z')",
                [],
            )
            .unwrap();
        let operation_id = "30000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Transcribe, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        let before: String = database
            .connection()
            .query_row(
                "SELECT before_projection_json FROM batch_job_items_v1 WHERE job_id=?1 AND ordinal=0",
                [operation_id],
                |row| row.get(0),
            )
            .unwrap();
        let before: BatchSegmentProjectionV1 = serde_json::from_str(&before).unwrap();
        assert_eq!(before.hypotheses.len(), 2);
        assert_eq!(before.hypotheses[0].model_version_id, "old-a@1");
        assert_eq!(before.hypotheses[1].created_at, "2026-01-02T00:00:00Z");

        let draft = BatchChampionDraftV1 {
            raw_transcript: "champion raw".into(),
            normalized_transcript: Some("champion normalized".into()),
            confidence: Some(0.91),
            confidence_source: Some("posterior".into()),
            model_version_id: "champion-v1".into(),
            deployment_sha256: DEPLOYMENT_SHA.into(),
            cloud_call: false,
            normalizer_version: Some("normalizer-v1".into()),
        };
        let outcome = database.commit_batch_champion_draft_v1(operation_id, 0, &draft, &executor()).unwrap();
        assert!(matches!(outcome, BatchItemCommitOutcomeV1::Applied { effect_revision: 6 }));
        let after: String = database
            .connection()
            .query_row(
                "SELECT after_projection_json FROM batch_job_items_v1 WHERE job_id=?1 AND ordinal=0",
                [operation_id],
                |row| row.get(0),
            )
            .unwrap();
        let after: BatchSegmentProjectionV1 = serde_json::from_str(&after).unwrap();
        assert_eq!(after.hypotheses.len(), 1);
        assert_eq!(after.hypotheses[0].model_id, "champion-v1");
        assert_eq!(after.hypotheses[0].model_version_id, "champion-v1");
        assert!(!after.hypotheses[0].created_at.is_empty());
        assert_eq!(after.segment.decoder_config_hash.as_deref(), Some(CONFIG_SHA));
        assert_eq!(after.segment.normalizer_version.as_deref(), Some("normalizer-v1"));
        database.finish_batch_job_v1(operation_id, BatchTerminalIntentV1::Succeeded, &executor()).unwrap();
        let initial_token = database.batch_execution_history_token_v1(operation_id).unwrap().unwrap();
        let stale_after_token = initial_token.clone();
        let before_token = database.apply_batch_job_history_v1(&initial_token).unwrap();
        assert_eq!(before_token.expected_side, BatchHistorySideV1::Before);
        let restored_before = Database::read_batch_projection_on(database.connection(), "s1").unwrap().unwrap();
        assert_eq!(projection_semantic_sha256(&restored_before).unwrap(), projection_semantic_sha256(&before).unwrap());
        assert!(database.apply_batch_job_history_v1(&stale_after_token).is_err());
        let after_token = database.apply_batch_job_history_v1(&before_token).unwrap();
        assert_eq!(after_token.expected_side, BatchHistorySideV1::After);
        let restored_after = Database::read_batch_projection_on(database.connection(), "s1").unwrap().unwrap();
        assert_eq!(projection_semantic_sha256(&restored_after).unwrap(), projection_semantic_sha256(&after).unwrap());
        assert!(restored_after.review_revision > restored_before.review_revision);
        database.validate_batch_job_authority_v1().unwrap();
    }

    #[test]
    fn durable_item_failure_globally_stops_later_champion_and_normalize_effects() {
        let database = fixture(&["s1", "s2"]);
        insert_champion(&database);
        let operation_id = "31000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Transcribe, &ids(&["s1", "s2"]), CONFIG_SHA, &executor())
            .unwrap();
        let invalid = BatchChampionDraftV1 {
            raw_transcript: "invalid identity draft".into(),
            normalized_transcript: Some("invalid identity draft".into()),
            confidence: Some(0.5),
            confidence_source: Some("posterior".into()),
            model_version_id: "retired-v1".into(),
            deployment_sha256: DEPLOYMENT_SHA.into(),
            cloud_call: false,
            normalizer_version: Some("normalizer-v1".into()),
        };
        let failed = database.commit_batch_champion_draft_v1(operation_id, 0, &invalid, &executor()).unwrap();
        assert_eq!(failed, BatchItemCommitOutcomeV1::Failed { code: "MODEL_IDENTITY_CHANGED".into() });
        let cancellation_error = database
            .finish_batch_job_v1(
                operation_id,
                BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() },
                &executor(),
            )
            .unwrap_err()
            .to_string();
        assert!(cancellation_error.contains("must remain a failed batch"), "{cancellation_error}");
        let still_running = database.get_batch_job_status_v1(operation_id).unwrap().unwrap();
        assert_eq!(still_running.state, BatchJobLifecycleV1::Running);
        assert_eq!(still_running.counts.failed, 1);
        assert_eq!(still_running.counts.pending, 1);

        let valid = BatchChampionDraftV1 { model_version_id: "champion-v1".into(), ..invalid };
        let error =
            database.commit_batch_champion_draft_v1(operation_id, 1, &valid, &executor()).unwrap_err().to_string();
        assert!(error.contains("BATCH_HARD_STOPPED"), "{error}");
        let s2: String = database
            .connection()
            .query_row("SELECT raw_transcript FROM speech_segments WHERE id='s2'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(s2, "raw s2");
        database
            .finish_batch_job_v1(
                operation_id,
                BatchTerminalIntentV1::Failed { code: "BATCH_TRANSCRIPTION_FAILED".into() },
                &executor(),
            )
            .unwrap();

        let normalize_db = fixture(&["n1", "n2"]);
        let normalize_id = "32000000-0000-4000-8000-000000000001";
        normalize_db
            .admit_batch_job_v1(normalize_id, BatchJobKindV1::Normalize, &ids(&["n1", "n2"]), CONFIG_SHA, &executor())
            .unwrap();
        normalize_db
            .connection()
            .execute(
                "UPDATE batch_job_items_v1
                    SET state='failed',result_code='NORMALIZATION_FAILED',
                        terminal_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE job_id=?1 AND ordinal=0",
                [normalize_id],
            )
            .unwrap();
        normalize_db
            .connection()
            .execute("UPDATE jobs SET completed=1,progress=0.5 WHERE id=?1", [normalize_id])
            .unwrap();
        let error = normalize_db
            .commit_batch_normalization_v1(normalize_id, 1, "must not land", "normalizer-v1", &executor())
            .unwrap_err()
            .to_string();
        assert!(error.contains("BATCH_HARD_STOPPED"), "{error}");
        let unchanged: Option<String> = normalize_db
            .connection()
            .query_row("SELECT normalized_transcript FROM speech_segments WHERE id='n2'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(unchanged, None);
    }

    #[test]
    fn late_cancel_or_panic_after_final_applied_or_skipped_item_canonicalizes_to_success() {
        let database = fixture(&["s1"]);
        let applied_id = "32500000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(applied_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        let applied = database
            .commit_batch_normalization_v1(applied_id, 0, "normalized final", "normalizer-v1", &executor())
            .unwrap();
        assert!(matches!(applied, BatchItemCommitOutcomeV1::Applied { .. }));

        // Exact worker-boundary race: Cancel wins after the last item commit but before the next
        // pending-page read/header finish. There is no remaining work to abandon, so the immutable
        // all-positive ledger is a successful run, not a permanently-running cancelled one.
        let late_cancel = BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() };
        let cancelled_finish = database.finish_batch_job_v1(applied_id, late_cancel.clone(), &executor()).unwrap();
        assert_eq!(cancelled_finish.state, BatchJobLifecycleV1::Succeeded);
        assert_eq!(cancelled_finish.counts.applied, 1);
        assert_eq!(cancelled_finish.counts.abandoned, 0);
        assert_eq!(cancelled_finish.error_code, None);
        assert_eq!(
            database.finish_batch_job_v1(applied_id, late_cancel, &executor()).unwrap().state,
            BatchJobLifecycleV1::Succeeded,
            "a lost response must make the same late-cancel cleanup idempotent"
        );

        let skipped_id = "32500000-0000-4000-8000-000000000002";
        database
            .admit_batch_job_v1(skipped_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        assert_eq!(
            database
                .commit_batch_normalization_v1(skipped_id, 0, "normalized final", "normalizer-v1", &executor(),)
                .unwrap(),
            BatchItemCommitOutcomeV1::Skipped { code: "UNCHANGED".into() }
        );

        // Panic cleanup has the identical boundary: if every item was already applied/skipped, the
        // durable result is complete. The panic signal must not create an impossible failed header.
        let late_panic = BatchTerminalIntentV1::Failed { code: "BATCH_WORKER_PANICKED".into() };
        let panicked_finish = database.finish_batch_job_v1(skipped_id, late_panic.clone(), &executor()).unwrap();
        assert_eq!(panicked_finish.state, BatchJobLifecycleV1::Succeeded);
        assert_eq!(panicked_finish.counts.skipped, 1);
        assert_eq!(panicked_finish.counts.abandoned, 0);
        assert_eq!(panicked_finish.error_code, None);
        assert_eq!(
            database.finish_batch_job_v1(skipped_id, late_panic, &executor()).unwrap().state,
            BatchJobLifecycleV1::Succeeded,
            "a lost response must make the same late-panic cleanup idempotent"
        );
        database.validate_batch_job_authority_v1().unwrap();
    }

    #[test]
    fn cancelled_header_cannot_disguise_failed_item_evidence() {
        let database = fixture(&["s1", "s2"]);
        let operation_id = "33000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1", "s2"]), CONFIG_SHA, &executor())
            .unwrap();
        database
            .connection()
            .execute(
                "UPDATE batch_job_items_v1
                    SET state='failed',result_code='NORMALIZATION_FAILED',
                        terminal_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE job_id=?1 AND ordinal=0",
                [operation_id],
            )
            .unwrap();
        database
            .connection()
            .execute(
                "UPDATE batch_job_items_v1
                    SET state='abandoned',result_code='BATCH_CANCELLED',
                        terminal_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE job_id=?1 AND ordinal=1",
                [operation_id],
            )
            .unwrap();
        database.connection().execute("UPDATE jobs SET completed=2,progress=1.0 WHERE id=?1", [operation_id]).unwrap();

        // The SQL trigger is the final authority even if a future caller bypasses the API.
        let cancelled_header = database.connection().execute(
            "UPDATE jobs
                SET state='cancelled',completed=total,progress=1.0,error_code='BATCH_CANCELLED',
                    finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                    updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
              WHERE id=?1",
            [operation_id],
        );
        assert!(cancelled_header.is_err(), "SQL authority accepted a cancelled job containing failed work");
        let still_running = database.get_batch_job_status_v1(operation_id).unwrap().unwrap();
        assert_eq!(still_running.state, BatchJobLifecycleV1::Running);
        assert_eq!(still_running.counts.failed, 1);
        assert_eq!(still_running.counts.abandoned, 1);
    }

    #[test]
    fn source_or_projection_conflict_never_clobbers_the_segment() {
        let database = fixture(&["s1"]);
        let operation_id = "40000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        database
            .connection()
            .execute("UPDATE speech_segments SET raw_transcript='newer writer' WHERE id='s1'", [])
            .unwrap();
        let outcome = database
            .commit_batch_normalization_v1(operation_id, 0, "stale output", "normalizer-v1", &executor())
            .unwrap();
        assert_eq!(outcome, BatchItemCommitOutcomeV1::Skipped { code: "BATCH_REVISION_CHANGED".into() });
        let current: (String, Option<String>) = database
            .connection()
            .query_row("SELECT raw_transcript,normalized_transcript FROM speech_segments WHERE id='s1'", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(current, ("newer writer".into(), None));
    }

    #[test]
    fn interrupted_run_recovery_abandons_pending_and_preserves_applied_evidence() {
        let database = fixture(&["s1", "s2"]);
        let operation_id = "50000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1", "s2"]), CONFIG_SHA, &executor())
            .unwrap();
        database.commit_batch_normalization_v1(operation_id, 0, "done", "normalizer-v1", &executor()).unwrap();
        let recovered = database.recover_active_batch_job_v1().unwrap().unwrap();
        assert_eq!(recovered.state, BatchJobLifecycleV1::Failed);
        assert_eq!(recovered.error_code.as_deref(), Some("PROCESS_INTERRUPTED"));
        assert_eq!(recovered.counts.applied, 1);
        assert_eq!(recovered.counts.abandoned, 1);
        assert_eq!(recovered.completed, 2);
        assert_eq!(recovered.progress, 1.0);
        assert!(database.active_batch_job_v1().unwrap().is_none());
        database.validate_batch_job_authority_v1().unwrap();

        let failed_database = fixture(&["f1"]);
        insert_champion(&failed_database);
        let failed_operation_id = "51000000-0000-4000-8000-000000000001";
        failed_database
            .admit_batch_job_v1(failed_operation_id, BatchJobKindV1::Transcribe, &ids(&["f1"]), CONFIG_SHA, &executor())
            .unwrap();
        let invalid = BatchChampionDraftV1 {
            raw_transcript: "wrong model".into(),
            normalized_transcript: None,
            confidence: Some(0.5),
            confidence_source: Some("posterior".into()),
            model_version_id: "retired-v1".into(),
            deployment_sha256: DEPLOYMENT_SHA.into(),
            cloud_call: false,
            normalizer_version: None,
        };
        assert_eq!(
            failed_database.commit_batch_champion_draft_v1(failed_operation_id, 0, &invalid, &executor()).unwrap(),
            BatchItemCommitOutcomeV1::Failed { code: "MODEL_IDENTITY_CHANGED".into() }
        );
        let recovered_failure = failed_database.recover_active_batch_job_v1().unwrap().unwrap();
        assert_eq!(recovered_failure.state, BatchJobLifecycleV1::Failed);
        assert_eq!(recovered_failure.error_code.as_deref(), Some("MODEL_IDENTITY_CHANGED"));
        failed_database.validate_batch_job_authority_v1().unwrap();
    }

    #[test]
    fn deep_validator_rejects_structurally_allowed_but_malformed_projection_evidence() {
        let database = fixture(&["s1"]);
        let operation_id = "60000000-0000-4000-8000-000000000001";
        let payload = BatchJobPayloadV1 {
            schema: BATCH_SCHEMA_V1,
            operation_id: operation_id.into(),
            kind: BatchJobKindV1::Normalize,
            request_sha256: CONFIG_SHA.into(),
            config_sha256: CONFIG_SHA.into(),
            executor_git_sha: GIT_SHA.into(),
            attempt_generation: 1,
            executor_token_sha256: TOKEN_SHA.into(),
        };
        let malformed = "{\"schema\":1}";
        let malformed_sha = sha256_bytes(malformed.as_bytes());
        database
            .connection()
            .execute(
                "INSERT INTO jobs(id,kind,state,idempotency_key,total,payload_json)
                 VALUES(?1,'batch_normalize_v1','queued',?2,1,?3)",
                params![operation_id, format!("batch-v1:{operation_id}"), canonical_json(&payload).unwrap()],
            )
            .unwrap();
        database
            .connection()
            .execute(
                "INSERT INTO batch_job_items_v1(
                     job_id,ordinal,segment_id,base_revision,source_identity_sha256,
                     before_projection_json,before_projection_sha256)
                 VALUES(?1,0,'s1',0,?2,?3,?4)",
                params![operation_id, CONFIG_SHA, malformed, malformed_sha],
            )
            .unwrap();
        database
            .connection()
            .execute(
                "UPDATE jobs SET state='running',started_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
                [operation_id],
            )
            .unwrap();
        let error = database.validate_batch_job_authority_v1().unwrap_err().to_string();
        assert!(error.contains(BATCH_EVIDENCE_ERROR), "{error}");
    }

    /// Put the connection into the state a RESTORED/CORRUPTED file can arrive in: every trigger on
    /// the named tables dropped and CHECK constraints ignored. The validators under test exist
    /// precisely because bytes written elsewhere never ran these guards.
    fn unlock_tables(database: &Database, tables: &[&str]) {
        for table in tables {
            let names = database
                .connection()
                .prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name=?1")
                .unwrap()
                .query_map([table], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            for name in names {
                database.connection().execute(&format!("DROP TRIGGER \"{name}\""), []).unwrap();
            }
        }
        database.connection().execute_batch("PRAGMA ignore_check_constraints = ON;").unwrap();
    }

    #[test]
    fn admission_argument_validation_fails_closed_before_any_write() {
        let database = fixture(&["s1"]);
        let bad_uuid = database.admit_batch_job_v1(
            "not-a-uuid",
            BatchJobKindV1::Normalize,
            &ids(&["s1"]),
            CONFIG_SHA,
            &executor(),
        );
        assert!(bad_uuid.unwrap_err().to_string().contains("canonical UUID"));
        let upper_uuid = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-00000000000A",
            BatchJobKindV1::Normalize,
            &ids(&["s1"]),
            CONFIG_SHA,
            &executor(),
        );
        assert!(upper_uuid.unwrap_err().to_string().contains("lowercase hyphenated"));
        let bad_config = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-00000000000a",
            BatchJobKindV1::Normalize,
            &ids(&["s1"]),
            "NOT-A-SHA",
            &executor(),
        );
        assert!(bad_config.unwrap_err().to_string().contains("canonical lowercase SHA-256"));
        let bad_git = BatchExecutorIdentityV1 { git_sha: "abc".into(), ..executor() };
        let git_error = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-00000000000a",
            BatchJobKindV1::Normalize,
            &ids(&["s1"]),
            CONFIG_SHA,
            &bad_git,
        );
        assert!(git_error.unwrap_err().to_string().contains("40 lowercase hexadecimal"));
        let bad_token = BatchExecutorIdentityV1 { token_sha256: "zz".into(), ..executor() };
        let token_error = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-00000000000a",
            BatchJobKindV1::Normalize,
            &ids(&["s1"]),
            CONFIG_SHA,
            &bad_token,
        );
        assert!(token_error.unwrap_err().to_string().contains("canonical lowercase SHA-256"));
        let zero_generation = BatchExecutorIdentityV1 { attempt_generation: 0, ..executor() };
        let generation_error = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-00000000000a",
            BatchJobKindV1::Normalize,
            &ids(&["s1"]),
            CONFIG_SHA,
            &zero_generation,
        );
        assert!(generation_error.unwrap_err().to_string().contains("must be positive"));
        let empty = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-00000000000a",
            BatchJobKindV1::Normalize,
            &[],
            CONFIG_SHA,
            &executor(),
        );
        assert!(empty.unwrap_err().to_string().contains("between 1 and"));
        let blank_id = database.admit_batch_job_v1(
            "10000000-0000-4000-8000-00000000000a",
            BatchJobKindV1::Normalize,
            &ids(&[""]),
            CONFIG_SHA,
            &executor(),
        );
        assert!(blank_id.is_err(), "a blank segment identifier must be refused");

        // Cursor and status/read argument validation on the read side.
        assert!(database.get_batch_job_status_v1("not-a-uuid").is_err());
        let missing_page =
            database.pending_batch_item_page_v1("10000000-0000-4000-8000-00000000000a", None).unwrap_err().to_string();
        assert!(missing_page.contains("does not exist"), "{missing_page}");
        // A negative cursor is refused before the header is even read.
        let negative = database.pending_batch_item_page_v1("10000000-0000-4000-8000-00000000000a", Some(-1));
        assert!(negative.unwrap_err().to_string().contains("pending-item cursor"));

        // None of the refusals may have published anything durable.
        let jobs: i64 = database.connection().query_row("SELECT count(*) FROM jobs", [], |row| row.get(0)).unwrap();
        assert_eq!(jobs, 0, "argument refusals must never leave a header behind");
    }

    #[test]
    fn pre_v68_schema_is_refused_by_every_batch_entry_point() {
        let database = Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        let head = crate::migrations::max_supported_version();
        let expected: Vec<i64> = (68..=head).rev().collect();
        assert_eq!(crate::migrations::rollback(&database, expected.len()).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(&database).unwrap(), 67);

        let operation_id = "70000000-0000-4000-8000-000000000001";
        for error in [
            database
                .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
                .unwrap_err(),
            database.get_batch_job_status_v1(operation_id).unwrap_err(),
            database.pending_batch_item_page_v1(operation_id, None).unwrap_err(),
            database.active_batch_job_v1().unwrap_err(),
            database.validate_batch_job_authority_v1().unwrap_err(),
        ] {
            let message = error.to_string();
            assert!(message.contains("requires schema 68"), "{message}");
        }
        // The connection-level startup form is deliberately a no-op below v68: those files have no
        // batch journal to validate, and refusing them would block every legacy restore.
        validate_batch_job_authority_on(database.connection()).unwrap();
    }

    #[test]
    fn forged_header_lifecycle_or_progress_is_refused_at_the_status_reader() {
        let cases: [(&str, &str); 10] = [
            ("UPDATE jobs SET total=0", "header progress disagrees"),
            ("UPDATE jobs SET total=3", "header progress disagrees"),
            ("UPDATE jobs SET progress=0.75", "header progress disagrees"),
            ("UPDATE jobs SET completed=1", "header progress disagrees"),
            ("UPDATE jobs SET state='queued'", "lifecycle residue"),
            ("UPDATE jobs SET started_at=NULL", "invalid lifecycle fields"),
            ("UPDATE jobs SET state='succeeded',finished_at='2026-08-30T00:00:00Z'", "contradicts item evidence"),
            (
                "UPDATE jobs SET state='failed',finished_at='2026-08-30T00:00:00Z',error_code='X'",
                "contradicts item evidence",
            ),
            (
                "UPDATE jobs SET state='cancelled',finished_at='2026-08-30T00:00:00Z',error_code='X'",
                "contradicts item evidence",
            ),
            ("UPDATE jobs SET state='bogus'", "unknown batch state"),
        ];
        for (sabotage, expected) in cases {
            let database = fixture(&["s1", "s2"]);
            let operation_id = "71000000-0000-4000-8000-000000000001";
            database
                .admit_batch_job_v1(
                    operation_id,
                    BatchJobKindV1::Normalize,
                    &ids(&["s1", "s2"]),
                    CONFIG_SHA,
                    &executor(),
                )
                .unwrap();
            unlock_tables(&database, &["jobs"]);
            assert_eq!(database.connection().execute(sabotage, []).unwrap(), 1, "{sabotage}");
            let error = database.get_batch_job_status_v1(operation_id).unwrap_err().to_string();
            assert!(error.contains(expected), "{sabotage}: expected '{expected}', got: {error}");
        }
    }

    #[test]
    fn forged_payload_authority_is_refused() {
        // Replace the stored payload with `replacement(stored)` and read the job's status back.
        let forge = |replacement: &dyn Fn(&str) -> String| -> String {
            let database = fixture(&["s1"]);
            let operation_id = "72000000-0000-4000-8000-000000000001";
            database
                .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
                .unwrap();
            unlock_tables(&database, &["jobs"]);
            let stored: String = database
                .connection()
                .query_row("SELECT payload_json FROM jobs WHERE id=?1", [operation_id], |row| row.get(0))
                .unwrap();
            database
                .connection()
                .execute("UPDATE jobs SET payload_json=?2 WHERE id=?1", params![operation_id, replacement(&stored)])
                .unwrap();
            database.get_batch_job_status_v1(operation_id).unwrap_err().to_string()
        };
        // Re-serializing through the typed struct keeps the canonical field order, so each case
        // reaches exactly the semantic check it corrupts rather than the canonical-form guard.
        fn retyped(stored: &str, mutate: impl FnOnce(&mut BatchJobPayloadV1)) -> String {
            let mut payload: BatchJobPayloadV1 = serde_json::from_str(stored).unwrap();
            mutate(&mut payload);
            canonical_json(&payload).unwrap()
        }

        let undecodable = forge(&|_| "{}".to_string());
        assert!(undecodable.contains("cannot be decoded"), "{undecodable}");
        let pretty = forge(&|stored| {
            let payload: serde_json::Value = serde_json::from_str(stored).unwrap();
            serde_json::to_string_pretty(&payload).unwrap()
        });
        assert!(pretty.contains("not in canonical typed JSON form"), "{pretty}");
        let wrong_operation = forge(&|stored| {
            retyped(stored, |payload| payload.operation_id = "99999999-0000-4000-8000-000000000009".into())
        });
        assert!(wrong_operation.contains("disagrees with its header"), "{wrong_operation}");
        let wrong_schema = forge(&|stored| retyped(stored, |payload| payload.schema = 2));
        assert!(wrong_schema.contains("disagrees with its header"), "{wrong_schema}");
        let zero_generation = forge(&|stored| retyped(stored, |payload| payload.attempt_generation = 0));
        assert!(zero_generation.contains("disagrees with its header"), "{zero_generation}");
        let bad_request_hash = forge(&|stored| retyped(stored, |payload| payload.request_sha256 = "XYZ".into()));
        assert!(bad_request_hash.contains("canonical lowercase SHA-256"), "{bad_request_hash}");
        let bad_git_sha = forge(&|stored| retyped(stored, |payload| payload.executor_git_sha = "short".into()));
        assert!(bad_git_sha.contains("40 lowercase hexadecimal"), "{bad_git_sha}");
    }

    #[test]
    fn executor_identity_must_be_wellformed_before_ownership_is_even_compared() {
        let database = fixture(&["s1"]);
        let operation_id = "73000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        let bad_git = BatchExecutorIdentityV1 { git_sha: "oops".into(), ..executor() };
        let git_error = database
            .commit_batch_normalization_v1(operation_id, 0, "text", "normalizer-v1", &bad_git)
            .unwrap_err()
            .to_string();
        assert!(git_error.contains("40 lowercase hexadecimal"), "{git_error}");
        let bad_token = BatchExecutorIdentityV1 { token_sha256: "oops".into(), ..executor() };
        let token_error = database
            .commit_batch_normalization_v1(operation_id, 0, "text", "normalizer-v1", &bad_token)
            .unwrap_err()
            .to_string();
        assert!(token_error.contains("canonical lowercase SHA-256"), "{token_error}");
        let zero = BatchExecutorIdentityV1 { attempt_generation: 0, ..executor() };
        let zero_error = database
            .commit_batch_normalization_v1(operation_id, 0, "text", "normalizer-v1", &zero)
            .unwrap_err()
            .to_string();
        assert!(zero_error.contains("must be positive"), "{zero_error}");
        // Malformed identities were refused before comparison — nothing landed, nothing terminalized.
        let status = database.get_batch_job_status_v1(operation_id).unwrap().unwrap();
        assert_eq!(status.counts.pending, 1);
    }

    #[test]
    fn conflict_skips_cover_missing_segment_changed_source_and_changed_projection() {
        // BATCH_SEGMENT_MISSING: the segment vanished between admission and commit.
        let missing_db = fixture(&["gone"]);
        let missing_id = "74000000-0000-4000-8000-000000000001";
        missing_db
            .admit_batch_job_v1(missing_id, BatchJobKindV1::Normalize, &ids(&["gone"]), CONFIG_SHA, &executor())
            .unwrap();
        unlock_tables(&missing_db, &["speech_segments"]);
        missing_db.connection().execute("DELETE FROM speech_segments WHERE id='gone'", []).unwrap();
        let outcome =
            missing_db.commit_batch_normalization_v1(missing_id, 0, "text", "normalizer-v1", &executor()).unwrap();
        assert_eq!(outcome, BatchItemCommitOutcomeV1::Skipped { code: "BATCH_SEGMENT_MISSING".into() });

        // BATCH_SOURCE_CHANGED: the audio identity moved out from under the admitted projection.
        let moved_db = fixture(&["moved"]);
        let moved_id = "74000000-0000-4000-8000-000000000002";
        moved_db
            .admit_batch_job_v1(moved_id, BatchJobKindV1::Normalize, &ids(&["moved"]), CONFIG_SHA, &executor())
            .unwrap();
        moved_db
            .connection()
            .execute("UPDATE speech_segments SET audio_path='C:/audio/elsewhere.wav' WHERE id='moved'", [])
            .unwrap();
        let outcome =
            moved_db.commit_batch_normalization_v1(moved_id, 0, "text", "normalizer-v1", &executor()).unwrap();
        assert_eq!(outcome, BatchItemCommitOutcomeV1::Skipped { code: "BATCH_SOURCE_CHANGED".into() });
        let untouched: Option<String> = moved_db
            .connection()
            .query_row("SELECT normalized_transcript FROM speech_segments WHERE id='moved'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(untouched, None, "a source conflict must never write");

        // BATCH_PROJECTION_CHANGED: same source identity and revision, but the hypothesis authority
        // grew — the projection hash is the only guard that can see it.
        let hypo_db = fixture(&["hypo"]);
        let hypo_id = "74000000-0000-4000-8000-000000000003";
        hypo_db
            .admit_batch_job_v1(hypo_id, BatchJobKindV1::Normalize, &ids(&["hypo"]), CONFIG_SHA, &executor())
            .unwrap();
        hypo_db
            .insert_hypothesis(&SegmentHypothesis {
                segment_id: "hypo".into(),
                model_id: "late-model".into(),
                transcript: "late vote".into(),
                confidence: None,
            })
            .unwrap();
        let revision: i64 = hypo_db
            .connection()
            .query_row("SELECT review_revision FROM speech_segments WHERE id='hypo'", [], |row| row.get(0))
            .unwrap();
        let base: i64 = hypo_db
            .connection()
            .query_row("SELECT base_revision FROM batch_job_items_v1 WHERE job_id=?1", [hypo_id], |row| row.get(0))
            .unwrap();
        let outcome = hypo_db.commit_batch_normalization_v1(hypo_id, 0, "text", "normalizer-v1", &executor()).unwrap();
        if revision == base {
            assert_eq!(outcome, BatchItemCommitOutcomeV1::Skipped { code: "BATCH_PROJECTION_CHANGED".into() });
        } else {
            // If hypothesis writes advance segment authority in this schema, the revision guard is
            // the one that legitimately fires first; either way the stale write must be refused.
            assert_eq!(outcome, BatchItemCommitOutcomeV1::Skipped { code: "BATCH_REVISION_CHANGED".into() });
        }
    }

    #[test]
    fn terminal_item_retries_report_already_terminal_and_divergent_retries_are_refused() {
        let database = fixture(&["s1", "s2"]);
        let operation_id = "75000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1", "s2"]), CONFIG_SHA, &executor())
            .unwrap();
        database.commit_batch_normalization_v1(operation_id, 0, "one", "normalizer-v1", &executor()).unwrap();
        // A retry carrying a DIFFERENT payload is not idempotent replay — it is a contradiction.
        let divergent_text = database
            .commit_batch_normalization_v1(operation_id, 0, "two", "normalizer-v1", &executor())
            .unwrap_err()
            .to_string();
        assert!(divergent_text.contains("disagrees with the already-applied durable result"), "{divergent_text}");
        let divergent_version = database
            .commit_batch_normalization_v1(operation_id, 0, "one", "normalizer-v2", &executor())
            .unwrap_err()
            .to_string();
        assert!(divergent_version.contains("disagrees with the already-applied durable result"), "{divergent_version}");

        // A skipped item replays as AlreadyTerminal with its durable code, whatever the payload.
        database.connection().execute("UPDATE speech_segments SET verified=1 WHERE id='s2'", []).unwrap();
        database.commit_batch_normalization_v1(operation_id, 1, "text", "normalizer-v1", &executor()).unwrap();
        let replay =
            database.commit_batch_normalization_v1(operation_id, 1, "other", "normalizer-v1", &executor()).unwrap();
        assert_eq!(
            replay,
            BatchItemCommitOutcomeV1::AlreadyTerminal {
                state: BatchItemStateV1::Skipped,
                code: Some("BATCH_HUMAN_OWNED".into()),
            }
        );

        // The champion path has the same retry contract.
        let champion_db = fixture(&["c1"]);
        insert_champion(&champion_db);
        let champion_id = "75000000-0000-4000-8000-000000000002";
        champion_db
            .admit_batch_job_v1(champion_id, BatchJobKindV1::Transcribe, &ids(&["c1"]), CONFIG_SHA, &executor())
            .unwrap();
        let draft = BatchChampionDraftV1 {
            raw_transcript: "champion text".into(),
            normalized_transcript: None,
            confidence: Some(0.9),
            confidence_source: Some("posterior".into()),
            model_version_id: "champion-v1".into(),
            deployment_sha256: DEPLOYMENT_SHA.into(),
            cloud_call: false,
            normalizer_version: None,
        };
        champion_db.commit_batch_champion_draft_v1(champion_id, 0, &draft, &executor()).unwrap();
        let same = champion_db.commit_batch_champion_draft_v1(champion_id, 0, &draft, &executor()).unwrap();
        assert!(matches!(same, BatchItemCommitOutcomeV1::AlreadyApplied { .. }));
        let divergent = BatchChampionDraftV1 { raw_transcript: "different text".into(), ..draft };
        let error = champion_db
            .commit_batch_champion_draft_v1(champion_id, 0, &divergent, &executor())
            .unwrap_err()
            .to_string();
        assert!(error.contains("disagrees with the already-applied durable result"), "{error}");
    }

    #[test]
    fn wrong_kind_or_absent_operation_and_ordinal_are_refused() {
        let database = fixture(&["s1"]);
        insert_champion(&database);
        let transcribe_id = "76000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(transcribe_id, BatchJobKindV1::Transcribe, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        let kind_error = database
            .commit_batch_normalization_v1(transcribe_id, 0, "text", "normalizer-v1", &executor())
            .unwrap_err()
            .to_string();
        assert!(kind_error.contains("not a normalize job"), "{kind_error}");
        let draft = BatchChampionDraftV1 {
            raw_transcript: "text".into(),
            normalized_transcript: None,
            confidence: None,
            confidence_source: None,
            model_version_id: "champion-v1".into(),
            deployment_sha256: DEPLOYMENT_SHA.into(),
            cloud_call: false,
            normalizer_version: None,
        };
        let absent_operation = database
            .commit_batch_champion_draft_v1("76000000-0000-4000-8000-00000000ffff", 0, &draft, &executor())
            .unwrap_err()
            .to_string();
        assert!(absent_operation.contains("does not exist"), "{absent_operation}");
        let absent_ordinal =
            database.commit_batch_champion_draft_v1(transcribe_id, 5, &draft, &executor()).unwrap_err().to_string();
        assert!(absent_ordinal.contains("ordinal does not exist"), "{absent_ordinal}");

        let normalize_db = fixture(&["n1"]);
        let normalize_id = "76000000-0000-4000-8000-000000000002";
        normalize_db
            .admit_batch_job_v1(normalize_id, BatchJobKindV1::Normalize, &ids(&["n1"]), CONFIG_SHA, &executor())
            .unwrap();
        let champion_error =
            normalize_db.commit_batch_champion_draft_v1(normalize_id, 0, &draft, &executor()).unwrap_err().to_string();
        assert!(champion_error.contains("not a transcribe job"), "{champion_error}");
    }

    #[test]
    fn champion_draft_field_validation_fails_closed() {
        let database = fixture(&["s1"]);
        insert_champion(&database);
        let operation_id = "77000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Transcribe, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        let valid = BatchChampionDraftV1 {
            raw_transcript: "real transcript".into(),
            normalized_transcript: Some("real transcript".into()),
            confidence: Some(0.5),
            confidence_source: Some("posterior".into()),
            model_version_id: "champion-v1".into(),
            deployment_sha256: DEPLOYMENT_SHA.into(),
            cloud_call: false,
            normalizer_version: Some("normalizer-v1".into()),
        };
        let cases: Vec<(&str, BatchChampionDraftV1, &str)> = vec![
            (
                "blank transcript",
                BatchChampionDraftV1 { raw_transcript: "   ".into(), ..valid.clone() },
                "blank ASR transcript",
            ),
            (
                "placeholder transcript",
                BatchChampionDraftV1 { raw_transcript: "[Pending WSL 7B ASR]".into(), ..valid.clone() },
                "placeholder",
            ),
            (
                "invalid deployment hash",
                BatchChampionDraftV1 { deployment_sha256: "nope".into(), ..valid.clone() },
                "champion deployment hash",
            ),
            (
                "normalized text without a normalizer version",
                BatchChampionDraftV1 { normalizer_version: None, ..valid.clone() },
                "both be present or both be absent",
            ),
            (
                "blank normalizer version",
                BatchChampionDraftV1 { normalizer_version: Some("  ".into()), ..valid.clone() },
                "normalizer version must not be blank",
            ),
            (
                "blank confidence source",
                BatchChampionDraftV1 { confidence_source: Some("  ".into()), ..valid.clone() },
                "confidence source must not be blank",
            ),
            (
                "out-of-range confidence",
                BatchChampionDraftV1 { confidence: Some(1.5), ..valid.clone() },
                "between 0 and 1",
            ),
            (
                "non-finite confidence",
                BatchChampionDraftV1 { confidence: Some(f64::NAN), ..valid.clone() },
                "between 0 and 1",
            ),
        ];
        for (label, draft, expected) in cases {
            let error =
                database.commit_batch_champion_draft_v1(operation_id, 0, &draft, &executor()).unwrap_err().to_string();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }
        // Every refusal happened before any durable write.
        let status = database.get_batch_job_status_v1(operation_id).unwrap().unwrap();
        assert_eq!(status.counts.pending, 1);
        let raw: String = database
            .connection()
            .query_row("SELECT raw_transcript FROM speech_segments WHERE id='s1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(raw, "raw s1");
    }

    #[test]
    fn two_live_batch_headers_are_corruption_not_an_arbitrary_winner() {
        let database = fixture(&["s1", "s2"]);
        let first = "78000000-0000-4000-8000-000000000001";
        database.admit_batch_job_v1(first, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor()).unwrap();
        assert_eq!(database.active_batch_job_v1().unwrap().unwrap().operation_id, first);

        // The one-live-batch invariant is enforced for new writes by a partial unique index; a
        // restored file can nevertheless carry two fully-valid live journals (the index can be
        // absent from bytes written elsewhere). Drop the guard and admit a second real journal.
        unlock_tables(&database, &["jobs"]);
        database.connection().execute("DROP INDEX idx_jobs_one_live_batch_v1", []).unwrap();
        let second = "78000000-0000-4000-8000-000000000002";
        database.admit_batch_job_v1(second, BatchJobKindV1::Normalize, &ids(&["s2"]), CONFIG_SHA, &executor()).unwrap();
        let discovery = database.active_batch_job_v1().unwrap_err().to_string();
        assert!(discovery.contains("live batch headers exist"), "{discovery}");
        let deep = database.validate_batch_job_authority_v1().unwrap_err().to_string();
        assert!(deep.contains("live batch headers exist"), "{deep}");
    }

    #[test]
    fn terminal_jobs_serve_an_empty_pending_page() {
        let database = fixture(&["s1"]);
        let operation_id = "79000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();
        assert_eq!(database.pending_batch_item_page_v1(operation_id, None).unwrap().len(), 1);
        database.commit_batch_normalization_v1(operation_id, 0, "done", "normalizer-v1", &executor()).unwrap();
        database.finish_batch_job_v1(operation_id, BatchTerminalIntentV1::Succeeded, &executor()).unwrap();
        assert!(
            database.pending_batch_item_page_v1(operation_id, None).unwrap().is_empty(),
            "a terminal job must never serve pending work"
        );
        assert!(database.active_batch_job_v1().unwrap().is_none());
    }

    #[test]
    fn item_evidence_corruption_is_refused_by_the_deep_validator() {
        // Each case corrupts exactly one durable evidence invariant on a journal produced entirely
        // by production APIs, then asserts the deep validator names the violated boundary.
        type Prepare = fn(&Database, &str);
        fn admit_only(_database: &Database, _operation_id: &str) {}
        fn apply_first(database: &Database, operation_id: &str) {
            database.commit_batch_normalization_v1(operation_id, 0, "done", "normalizer-v1", &executor()).unwrap();
        }
        fn skip_first(database: &Database, operation_id: &str) {
            database.connection().execute("UPDATE speech_segments SET verified=1 WHERE id='s1'", []).unwrap();
            database.commit_batch_normalization_v1(operation_id, 0, "done", "normalizer-v1", &executor()).unwrap();
        }
        let cases: [(&str, Prepare, &str, &str); 8] = [
            (
                "pending item with terminal evidence",
                admit_only,
                "UPDATE batch_job_items_v1 SET result_code='X'",
                "carries terminal evidence",
            ),
            (
                "empty before projection",
                admit_only,
                "UPDATE batch_job_items_v1 SET before_projection_json=''",
                "E_BATCH_PROJECTION_LIMIT_EXCEEDED",
            ),
            (
                "tampered before projection bytes",
                admit_only,
                "UPDATE batch_job_items_v1 SET before_projection_json=before_projection_json||' '",
                BATCH_EVIDENCE_ERROR,
            ),
            (
                "shifted base revision",
                admit_only,
                "UPDATE batch_job_items_v1 SET base_revision=base_revision+1",
                "does not match its identity columns",
            ),
            (
                "applied item with a result code",
                apply_first,
                "UPDATE batch_job_items_v1 SET result_code='X'",
                "lacks exact after authority",
            ),
            (
                "applied item missing its after projection",
                apply_first,
                "UPDATE batch_job_items_v1 SET after_projection_json=NULL",
                "partial after authority",
            ),
            (
                "applied item missing its effect revision",
                apply_first,
                "UPDATE batch_job_items_v1 SET effect_revision=NULL",
                BATCH_EVIDENCE_ERROR,
            ),
            (
                "skipped item without a result code",
                skip_first,
                "UPDATE batch_job_items_v1 SET result_code=NULL",
                "malformed evidence",
            ),
        ];
        for (label, prepare, sabotage, expected) in cases {
            let database = fixture(&["s1"]);
            let operation_id = "7a000000-0000-4000-8000-000000000001";
            database
                .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
                .unwrap();
            prepare(&database, operation_id);
            database.validate_batch_job_authority_v1().expect("the uncorrupted journal must validate first");
            unlock_tables(&database, &["batch_job_items_v1"]);
            assert_eq!(database.connection().execute(sabotage, []).unwrap(), 1, "{label}");
            let error = database.validate_batch_job_authority_v1().unwrap_err().to_string();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }
    }

    #[test]
    fn evidence_enums_round_trip_and_refuse_unknown_stored_values() {
        for kind in [BatchJobKindV1::Transcribe, BatchJobKindV1::Normalize] {
            assert_eq!(BatchJobKindV1::parse(kind.as_str()).unwrap(), kind);
        }
        let error = BatchJobKindV1::parse("batch_delete_v1").unwrap_err().to_string();
        assert!(error.contains(BATCH_EVIDENCE_ERROR) && error.contains("unknown batch kind"), "{error}");

        for state in [
            BatchJobLifecycleV1::Queued,
            BatchJobLifecycleV1::Running,
            BatchJobLifecycleV1::Succeeded,
            BatchJobLifecycleV1::Failed,
            BatchJobLifecycleV1::Cancelled,
        ] {
            assert_eq!(BatchJobLifecycleV1::parse(state.as_str()).unwrap(), state);
            assert_eq!(
                state.is_terminal(),
                matches!(
                    state,
                    BatchJobLifecycleV1::Succeeded | BatchJobLifecycleV1::Failed | BatchJobLifecycleV1::Cancelled
                )
            );
        }
        assert!(BatchJobLifecycleV1::parse("paused").unwrap_err().to_string().contains("unknown batch state"));

        for state in [
            BatchItemStateV1::Pending,
            BatchItemStateV1::Applied,
            BatchItemStateV1::Skipped,
            BatchItemStateV1::Failed,
            BatchItemStateV1::Abandoned,
        ] {
            assert_eq!(BatchItemStateV1::parse(state.as_str()).unwrap(), state);
        }
        assert!(BatchItemStateV1::parse("retrying").unwrap_err().to_string().contains("unknown batch item state"));

        assert_eq!(BatchHistorySideV1::Before.opposite(), BatchHistorySideV1::After);
        assert_eq!(BatchHistorySideV1::After.opposite(), BatchHistorySideV1::Before);

        let counts = BatchItemCountsV1 { pending: 7, applied: 1, skipped: 2, failed: 3, abandoned: 4 };
        assert_eq!(counts.terminal(), 10, "pending never counts as terminal");
    }

    #[test]
    fn evidence_field_validators_enforce_exact_shapes_on_both_sides() {
        // SHA-256 fields: exact 64 lowercase hex, nothing else.
        validate_sha256(CONFIG_SHA, "config").unwrap();
        for bad in ["", "abc", &CONFIG_SHA[..63], &format!("{}A", &CONFIG_SHA[..63]), &"g".repeat(64)] {
            assert!(validate_sha256(bad, "config").is_err(), "{bad:?} must be refused");
        }

        // Git SHA: exactly 40 lowercase hex.
        validate_git_sha(GIT_SHA).unwrap();
        assert!(validate_git_sha(&GIT_SHA[..39]).is_err());
        assert!(validate_git_sha(&GIT_SHA.to_ascii_uppercase()).is_err());

        // Result codes: 1..=64 uppercase/digit/underscore.
        validate_result_code("E_OK_1").unwrap();
        validate_result_code(&"A".repeat(64)).unwrap();
        assert!(validate_result_code("").is_err());
        assert!(validate_result_code(&"A".repeat(65)).is_err());
        assert!(validate_result_code("lower_case").is_err());
        assert!(validate_result_code("HAS SPACE").is_err());

        // Footprint metadata must be non-negative before it can be compared to a limit.
        assert!(checked_footprint_value(0, "field").is_ok());
        let error = checked_footprint_value(-1, "segment text").unwrap_err().to_string();
        assert!(error.contains("invalid byte/count metadata"), "{error}");

        // Projection JSON lengths: zero and over-bound both refuse.
        assert!(validate_projection_json_length_v1(1, "probe").is_ok());
        assert!(validate_projection_json_length_v1(0, "probe").is_err());
        assert!(validate_projection_json_length_v1((MAX_BATCH_PROJECTION_JSON_BYTES_V1 as i64) + 1, "probe").is_err());
    }

    #[test]
    fn projection_footprint_limits_refuse_each_axis_independently() {
        let within = BatchProjectionFootprintV1 {
            segment_text_bytes: 10,
            largest_segment_field_bytes: 10,
            hypothesis_count: 1,
            hypothesis_transcript_bytes: 10,
            largest_hypothesis_transcript_bytes: 10,
            hypothesis_metadata_bytes: 10,
            largest_model_id_bytes: 10,
            largest_model_version_id_bytes: 10,
            largest_hypothesis_created_at_bytes: 10,
        };
        validate_batch_projection_footprint_v1("s-ok", within).unwrap();

        let over_text =
            BatchProjectionFootprintV1 { segment_text_bytes: (MAX_BATCH_SEGMENT_TEXT_BYTES_V1 as i64) + 1, ..within };
        assert!(validate_batch_projection_footprint_v1("s-text", over_text).is_err());
        let over_field = BatchProjectionFootprintV1 {
            largest_segment_field_bytes: (MAX_BATCH_SEGMENT_TEXT_FIELD_BYTES_V1 as i64) + 1,
            ..within
        };
        assert!(validate_batch_projection_footprint_v1("s-field", over_field).is_err());
        let over_count =
            BatchProjectionFootprintV1 { hypothesis_count: (MAX_STORED_HYPOTHESES_PER_SEGMENT as i64) + 1, ..within };
        assert!(validate_batch_projection_footprint_v1("s-count", over_count).is_err());
        let over_transcripts = BatchProjectionFootprintV1 {
            hypothesis_transcript_bytes: (MAX_STORED_HYPOTHESIS_TRANSCRIPT_BYTES_PER_SEGMENT as i64) + 1,
            ..within
        };
        assert!(validate_batch_projection_footprint_v1("s-agg", over_transcripts).is_err());
        let over_largest = BatchProjectionFootprintV1 {
            largest_hypothesis_transcript_bytes: (MAX_STORED_HYPOTHESIS_TRANSCRIPT_BYTES as i64) + 1,
            ..within
        };
        assert!(validate_batch_projection_footprint_v1("s-largest", over_largest).is_err());
        let over_metadata = BatchProjectionFootprintV1 {
            hypothesis_metadata_bytes: (MAX_STORED_HYPOTHESIS_METADATA_BYTES_PER_SEGMENT as i64) + 1,
            ..within
        };
        assert!(validate_batch_projection_footprint_v1("s-meta", over_metadata).is_err());
        let over_meta_field = BatchProjectionFootprintV1 {
            largest_hypothesis_created_at_bytes: (MAX_STORED_HYPOTHESIS_METADATA_FIELD_BYTES as i64) + 1,
            ..within
        };
        assert!(validate_batch_projection_footprint_v1("s-meta-field", over_meta_field).is_err());
    }

    #[test]
    fn cancellable_admission_is_all_or_nothing() {
        let database = fixture(&["s1", "s2"]);
        let operation_id = "9c000000-0000-4000-8000-000000000001";

        // A cancellation observed before durable publication leaves no header and no item rows.
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let error = database
            .admit_batch_job_v1_cancellable(
                operation_id,
                BatchJobKindV1::Normalize,
                &ids(&["s1", "s2"]),
                CONFIG_SHA,
                &executor(),
                &cancelled,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("BATCH_ADMISSION_CANCELLED"), "{error}");
        let jobs: i64 = database.connection().query_row("SELECT count(*) FROM jobs", [], |row| row.get(0)).unwrap();
        assert_eq!(jobs, 0, "a cancelled admission must publish nothing");
        assert!(database.active_batch_job_v1().unwrap().is_none(), "no live batch after the cancelled admission");
        assert!(database.get_batch_job_status_v1(operation_id).unwrap().is_none());

        // The same identity admits normally once the flag is clear.
        let live = std::sync::atomic::AtomicBool::new(false);
        let status = database
            .admit_batch_job_v1_cancellable(
                operation_id,
                BatchJobKindV1::Normalize,
                &ids(&["s1", "s2"]),
                CONFIG_SHA,
                &executor(),
                &live,
            )
            .unwrap();
        assert_eq!(status.state, BatchJobLifecycleV1::Running);
        assert_eq!(status.total, 2);
        assert_eq!(status.counts.pending, 2);
    }

    #[test]
    fn non_applied_terminalizer_refuses_wrong_states_and_missing_pending_rows() {
        let database = fixture(&["s1"]);
        let operation_id = "9d000000-0000-4000-8000-000000000001";
        database
            .admit_batch_job_v1(operation_id, BatchJobKindV1::Normalize, &ids(&["s1"]), CONFIG_SHA, &executor())
            .unwrap();

        // Applied and Pending are not legal targets for the non-applied terminalizer.
        for state in [BatchItemStateV1::Applied, BatchItemStateV1::Pending] {
            let error = database.mark_batch_item_terminal_v1(operation_id, 0, state, "E_CODE").unwrap_err().to_string();
            assert!(error.contains("invalid state"), "{state:?}: {error}");
        }
        // A malformed result code is refused before any row is touched.
        assert!(database.mark_batch_item_terminal_v1(operation_id, 0, BatchItemStateV1::Skipped, "bad code").is_err());
        // An ordinal with no pending row cannot be terminalized.
        let error = database
            .mark_batch_item_terminal_v1(operation_id, 41, BatchItemStateV1::Skipped, "E_CODE")
            .unwrap_err()
            .to_string();
        assert!(error.contains("could not be terminalized"), "{error}");
        // The one pending row is still pending after every refusal.
        assert_eq!(database.batch_item_counts_v1(operation_id).unwrap().pending, 1);
    }

    #[test]
    fn human_ownership_is_detected_on_every_authority_axis() {
        let unowned = SpeechSegment { id: "own-0".into(), ..SpeechSegment::default() };
        assert!(!segment_is_human_owned(&unowned), "a bare machine row is not human-owned");

        let cases: Vec<(&str, Box<dyn Fn(&mut SpeechSegment)>)> = vec![
            ("verified", Box::new(|seg| seg.verified = true)),
            ("is_gold", Box::new(|seg| seg.is_gold = true)),
            // Presence alone is authoritative, including a historical machine-seeded empty string.
            ("annotated_transcript", Box::new(|seg| seg.annotated_transcript = Some(String::new()))),
            ("human_decision", Box::new(|seg| seg.human_decision = Some("accept".into()))),
            ("verdict", Box::new(|seg| seg.verdict = Some("human_edit".into()))),
            ("reviewed_by", Box::new(|seg| seg.reviewed_by = Some("desktop".into()))),
            ("corrected_at", Box::new(|seg| seg.corrected_at = Some("2026-08-01T00:00:00Z".into()))),
        ];
        for (label, mutate) in cases {
            let mut seg = SpeechSegment { id: "own-1".into(), ..SpeechSegment::default() };
            mutate(&mut seg);
            assert!(segment_is_human_owned(&seg), "{label} must mark the row human-owned");
        }

        // Whitespace-only decision/reviewer values and machine verdicts do NOT mint ownership.
        let mut blank = SpeechSegment { id: "own-2".into(), ..SpeechSegment::default() };
        blank.human_decision = Some("   ".into());
        blank.reviewed_by = Some(" ".into());
        blank.verdict = Some("machine_accept".into());
        assert!(!segment_is_human_owned(&blank), "blank/machine markers are not human authority");
    }
}
