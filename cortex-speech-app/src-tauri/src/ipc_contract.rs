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

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ReviewScope {
    Pending,
    Escalation,
    Search { query: String },
    VoiceFocus { focus_id: String },
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
    pub playback_receipt_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommittedReviewV1 {
    pub segment_id: String,
    pub committed_revision: i64,
    pub authoritative_transcript: String,
    pub decision_id: String,
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

/// One registry drives both the typed command metadata and every standalone public contract. Keep
/// generation separate from application startup so a release build never mutates its source tree.
pub fn specta_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            crate::commands::get_review_page_v1,
            crate::commands::commit_review_v1,
            crate::commands::get_review_draft_v1,
            crate::commands::save_review_draft_v1,
            crate::commands::delete_review_draft_v1
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
        .typ::<ReviewScope>()
        .typ::<ReviewItemV1>()
        .typ::<ReviewPageV1>()
        .typ::<ReviewDecisionV1>()
        .typ::<OperationEventV1>()
        .typ::<ReviewDraftV1>()
        .typ::<SettingValueV1>()
        .typ::<SettingsPatchV1>()
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
}
