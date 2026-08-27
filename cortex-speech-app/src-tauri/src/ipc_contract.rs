//! Versioned public IPC wire contracts.
//!
//! These types contain only renderer-safe data. Database errors, SQL, secrets and private absolute
//! paths are mapped to stable codes before crossing this boundary.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeMap;

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
    SpeakerAssignment,
}

impl From<crate::history::HistoryAction> for HistoryActionV1 {
    fn from(action: crate::history::HistoryAction) -> Self {
        match action {
            crate::history::HistoryAction::UpdateSegment => Self::UpdateSegment,
            crate::history::HistoryAction::DeleteSegments => Self::DeleteSegments,
            crate::history::HistoryAction::BatchTranscribe => Self::BatchTranscribe,
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
    Skip,
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
    Halted { operation_id: String, halted_by: String },
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
            crate::commands::get_active_voice_focus_v1,
            crate::commands::get_review_page_v1,
            crate::commands::commit_review_v1,
            crate::commands::mark_segment_unusable_v1,
            crate::commands::begin_desktop_playback_session_v1,
            crate::commands::cancel_desktop_playback_session_v1,
            crate::commands::finalize_desktop_playback_session_v1,
            crate::commands::get_review_draft_v1,
            crate::commands::save_review_draft_v1,
            crate::commands::delete_review_draft_v1,
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
            crate::commands::list_model_versions,
            crate::commands::import_model_checkpoint,
            crate::commands::import_model_deployment,
            crate::commands::bootstrap_legacy_champion,
            crate::commands::get_speaker_inventory_v1,
            crate::commands::update_segment_metadata_v1,
            crate::commands::delete_segments_v1,
            crate::commands::rename_speaker_v1,
            crate::commands::assign_speakers_v1
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
        .typ::<HistoryActionV1>()
        .typ::<HistoryStatusV1>()
        .typ::<HistoryMutationResultV1>()
        .typ::<ReviewItemV1>()
        .typ::<ReviewPageV1>()
        .typ::<ReviewDecisionV1>()
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
}
