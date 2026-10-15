//! Content-addressed OmniASR-7B deployment manifests.
//!
//! A LoRA adapter alone is not a model identity. The served result is determined by the base
//! checkpoint, adapter, adapter configuration, tokenizer and runtime shape together. This module
//! verifies that complete unit before it may enter the registry or be launched by the champion
//! supervisor.
//!
//! Schema 2 is a FUSED deployment: `base_checkpoint` is the merged checkpoint itself and the adapter
//! fields are absent. It is bound by the exact size + SHA-256 of that checkpoint and the tokenizer;
//! schema-1 (adapter) manifests are unchanged and still require both adapter components.

use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component as PathComponent, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const OMNIASR_7B_FAMILY: &str = "omniasr-7b";
pub const OMNIASR_7B_MODEL_CARD: &str = "soranivoice_omniASR_LLM_7B_v2_local";
pub const DEPLOYMENT_SCHEMA: u32 = 1;
pub const FUSED_DEPLOYMENT_SCHEMA: u32 = 2;
pub const SERVER_PROTOCOL: &str = "cortex-omniasr-adapter";
pub const SERVER_PROTOCOL_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentComponent {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SnapshotIdentity {
    pub id: String,
    pub root_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrainerIdentity {
    pub command: String,
    pub identity: String,
    pub recipe_sha256: String,
    pub environment_sha256: String,
    pub seed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    pub training_contract_sha256: String,
    pub trainer_evidence_sha256: String,
    pub artifact_manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BaseCheckpointIdentity {
    pub id: String,
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServerCompatibility {
    pub protocol: String,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub arch: String,
    pub vocab_size: u32,
    pub target_vocab_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploymentProvenance {
    /// Every new challenger. This is the closed correction -> snapshot -> trainer lineage.
    Flywheel {
        snapshot: SnapshotIdentity,
        training_run_id: String,
        trainer: TrainerIdentity,
        source_evidence: SourceEvidence,
    },
    /// The measured incumbent predates the flywheel and its original training manifest cannot be
    /// reconstructed. It may be bootstrapped once, transparently, but never imported as a new
    /// challenger. This avoids inventing a snapshot/trainer identity that does not exist.
    LegacyBootstrap { measurement_evidence_sha256: String, training_provenance: String },
}

/// The exact handoff emitted by `train_challenger.py` and consumed by registry + server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentManifest {
    pub schema: u32,
    pub model_id: String,
    pub family: String,
    pub model_card: String,
    pub language: String,
    pub provenance: DeploymentProvenance,
    pub base_checkpoint: BaseCheckpointIdentity,
    /// Present exactly when `schema == DEPLOYMENT_SCHEMA`; absent (never null) for a fused deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<DeploymentComponent>,
    pub tokenizer: DeploymentComponent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_config: Option<DeploymentComponent>,
    pub server_compatibility: ServerCompatibility,
    pub runtime: RuntimeIdentity,
    #[serde(rename = "manifestSha256")]
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDeployment {
    pub manifest_path: PathBuf,
    /// SHA-256 over the exact UTF-8 manifest file bytes. This is the registry/pointer identity.
    pub deployment_sha256: String,
    pub manifest: DeploymentManifest,
    pub base_path: PathBuf,
    /// `None` for a fused deployment.
    pub adapter_path: Option<PathBuf>,
    pub tokenizer_path: PathBuf,
    pub adapter_config_path: Option<PathBuf>,
}

/// Path-neutral result used by the registry. Local verification additionally returns canonical
/// component paths; WSL verification cannot represent `/home/...` as Windows `PathBuf`s, but it can
/// return the exact same validated manifest and content identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDeploymentRecord {
    pub manifest_path: String,
    pub deployment_sha256: String,
    pub manifest: DeploymentManifest,
}

impl VerifiedDeployment {
    pub fn record(&self) -> VerifiedDeploymentRecord {
        VerifiedDeploymentRecord {
            manifest_path: self.manifest_path.to_string_lossy().into_owned(),
            deployment_sha256: self.deployment_sha256.clone(),
            manifest: self.manifest.clone(),
        }
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hash_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(values.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

fn canonical_manifest_bytes(value: &serde_json::Value) -> AppResult<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&canonical_json(value))
        .map_err(|error| AppError::Other(format!("canonicalizing deployment manifest: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Deserialize a manifest object and refuse anything the typed form would not reproduce exactly
/// (an explicit `"adapter": null`, for instance), so Rust accepts the same key set the Python
/// verifier's exact-key check accepts.
fn manifest_from_value(value: serde_json::Value) -> AppResult<DeploymentManifest> {
    let manifest: DeploymentManifest = serde_json::from_value(value.clone())
        .map_err(|error| AppError::Validation(format!("deployment manifest schema is invalid: {error}")))?;
    let round_trip = serde_json::to_value(&manifest)
        .map_err(|error| AppError::Other(format!("re-serializing deployment manifest: {error}")))?;
    if round_trip != value {
        return Err(AppError::Validation("deployment manifest has fields its schema does not define".into()));
    }
    Ok(manifest)
}

fn validate_manifest_contract(manifest: &DeploymentManifest) -> AppResult<()> {
    let adapter_shape_ok = match manifest.schema {
        DEPLOYMENT_SCHEMA => manifest.adapter.is_some() && manifest.adapter_config.is_some(),
        FUSED_DEPLOYMENT_SCHEMA => manifest.adapter.is_none() && manifest.adapter_config.is_none(),
        _ => false,
    };
    if !adapter_shape_ok {
        return Err(AppError::Validation(
            "deployment schema 1 requires adapter + adapter_config; fused schema 2 forbids both".into(),
        ));
    }
    if manifest.family != OMNIASR_7B_FAMILY
        || manifest.server_compatibility.protocol != SERVER_PROTOCOL
        || manifest.server_compatibility.version != SERVER_PROTOCOL_VERSION
    {
        return Err(AppError::Validation("unsupported OmniASR deployment schema/family/protocol".into()));
    }
    if manifest.runtime.arch != "7b"
        || manifest.runtime.vocab_size != 10_288
        || manifest.runtime.target_vocab_size != 10_288
    {
        return Err(AppError::Validation(
            "deployment runtime shape does not match the pinned OmniASR-7B contract".into(),
        ));
    }
    for (value, label) in
        [(&manifest.model_id, "model id"), (&manifest.model_card, "model card"), (&manifest.language, "language")]
    {
        validate_text(value, label)?;
    }
    crate::validation::input::validate_identifier(&manifest.model_id).map_err(AppError::Validation)?;
    if manifest.language != "ckb_Arab" {
        return Err(AppError::Validation(
            "deployment manifest language does not match the pinned ckb_Arab contract".into(),
        ));
    }
    if manifest.model_card != OMNIASR_7B_MODEL_CARD {
        return Err(AppError::Validation(format!(
            "deployment manifest model card does not match the pinned OmniASR-7B card '{OMNIASR_7B_MODEL_CARD}'"
        )));
    }
    if !is_sha256(&manifest.manifest_sha256) {
        return Err(AppError::Validation("deployment manifest semantic self-hash is not canonical".into()));
    }
    match &manifest.provenance {
        DeploymentProvenance::Flywheel { snapshot, training_run_id, trainer, source_evidence } => {
            validate_text(&trainer.command, "trainer command")?;
            validate_text(&trainer.identity, "trainer identity")?;
            if !is_sha256(&snapshot.id)
                || !is_sha256(&snapshot.root_sha256)
                || !is_sha256(training_run_id)
                || !is_sha256(&trainer.recipe_sha256)
                || !is_sha256(&trainer.environment_sha256)
                || !is_sha256(&source_evidence.training_contract_sha256)
                || !is_sha256(&source_evidence.trainer_evidence_sha256)
                || !is_sha256(&source_evidence.artifact_manifest_sha256)
            {
                return Err(AppError::Validation(
                    "flywheel deployment has an invalid snapshot, run, trainer, environment, or source identity".into(),
                ));
            }
        }
        DeploymentProvenance::LegacyBootstrap { measurement_evidence_sha256, training_provenance } => {
            if !is_sha256(measurement_evidence_sha256) || training_provenance != "unverifiable" {
                return Err(AppError::Validation(
                    "legacy bootstrap must pin measurement evidence and explicitly mark training provenance unverifiable"
                        .into(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_text(value: &str, label: &str) -> AppResult<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(AppError::Validation(format!(
            "deployment manifest {label} is empty or contains control characters"
        )));
    }
    Ok(())
}

fn resolve_component(root: &Path, component: &DeploymentComponent, label: &str) -> AppResult<PathBuf> {
    if !is_sha256(&component.sha256) {
        return Err(AppError::Validation(format!("deployment {label} has no canonical lowercase SHA-256")));
    }
    if component.size_bytes == 0 {
        return Err(AppError::Validation(format!("deployment {label} declares an empty file")));
    }
    let relative = Path::new(&component.path);
    if relative.is_absolute() || relative.components().any(|part| !matches!(part, PathComponent::Normal(_))) {
        return Err(AppError::Validation(format!(
            "deployment {label} path must be a safe path relative to the manifest: '{}'",
            component.path
        )));
    }
    let root = root.canonicalize().map_err(|error| {
        AppError::Validation(format!("deployment root '{}' cannot be resolved: {error}", root.display()))
    })?;
    let path = root.join(relative).canonicalize().map_err(|error| {
        AppError::Validation(format!("deployment {label} '{}' cannot be resolved: {error}", component.path))
    })?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(AppError::Validation(format!(
            "deployment {label} escapes the deployment root or is not a file: '{}'",
            component.path
        )));
    }
    let metadata = path.metadata().map_err(AppError::Io)?;
    if metadata.len() != component.size_bytes {
        return Err(AppError::Validation(format!(
            "deployment {label} size mismatch: manifest says {}, file is {}",
            component.size_bytes,
            metadata.len()
        )));
    }
    let actual = crate::models::compute_file_sha256(&path)
        .map_err(|error| AppError::Other(format!("hashing deployment {label}: {error}")))?;
    if actual != component.sha256 {
        return Err(AppError::Validation(format!(
            "deployment {label} SHA-256 mismatch: expected {}, got {actual}",
            component.sha256
        )));
    }
    Ok(path)
}

fn resolve_base(root: &Path, base: &BaseCheckpointIdentity) -> AppResult<PathBuf> {
    validate_text(&base.id, "base checkpoint id")?;
    if !is_sha256(&base.sha256) || base.size_bytes == 0 {
        return Err(AppError::Validation(
            "deployment base checkpoint requires a canonical SHA-256 and non-zero size".into(),
        ));
    }
    let raw = Path::new(&base.path);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        if raw.components().any(|part| !matches!(part, PathComponent::Normal(_))) {
            return Err(AppError::Validation("deployment base checkpoint path is unsafe".into()));
        }
        root.join(raw)
    };
    let path = candidate.canonicalize().map_err(|error| {
        AppError::Validation(format!("deployment base checkpoint '{}' cannot be resolved: {error}", base.path))
    })?;
    if !path.is_file() {
        return Err(AppError::Validation(format!("deployment base checkpoint is not a file: '{}'", base.path)));
    }
    let size = path.metadata().map_err(AppError::Io)?.len();
    if size != base.size_bytes {
        return Err(AppError::Validation(format!(
            "deployment base checkpoint size mismatch: manifest says {}, file is {size}",
            base.size_bytes
        )));
    }
    let actual = crate::models::compute_file_sha256(&path)
        .map_err(|error| AppError::Other(format!("hashing deployment base checkpoint: {error}")))?;
    if actual != base.sha256 {
        return Err(AppError::Validation(format!(
            "deployment base checkpoint SHA-256 mismatch: expected {}, got {actual}",
            base.sha256
        )));
    }
    Ok(path)
}

/// Verify the exact manifest bytes, its canonical self-hash and every behavior-determining file.
/// `expected_file_sha256` is normally the registry's `checkpoint_sha256`; supplying it closes the
/// pointer-to-manifest substitution gap.
pub fn verify_deployment_manifest(path: &Path, expected_file_sha256: Option<&str>) -> AppResult<VerifiedDeployment> {
    let metadata = path.metadata().map_err(|error| {
        AppError::Validation(format!("deployment manifest '{}' is unavailable: {error}", path.display()))
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_MANIFEST_BYTES {
        return Err(AppError::Validation(format!(
            "deployment manifest '{}' must be a non-empty file no larger than {MAX_MANIFEST_BYTES} bytes",
            path.display()
        )));
    }
    let bytes = std::fs::read(path).map_err(AppError::Io)?;
    if bytes.contains(&b'\r') {
        return Err(AppError::Validation(
            "deployment manifest is not canonical LF-only UTF-8 (CR/CRLF drift detected)".into(),
        ));
    }
    let deployment_sha256 = hash_bytes(&bytes);
    if let Some(expected) = expected_file_sha256 {
        if !is_sha256(expected) || deployment_sha256 != expected {
            return Err(AppError::Validation(format!(
                "deployment manifest file SHA-256 mismatch: expected {expected}, got {deployment_sha256}"
            )));
        }
    }
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::Validation(format!("deployment manifest is not valid UTF-8 JSON: {error}")))?;
    if bytes != canonical_manifest_bytes(&value)? {
        return Err(AppError::Validation(
            "deployment manifest bytes must be canonical sorted compact JSON followed by exactly one LF".into(),
        ));
    }
    let stated_self_hash = value
        .as_object_mut()
        .and_then(|object| object.remove("manifestSha256"))
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| AppError::Validation("deployment manifest has no manifestSha256".into()))?;
    if !is_sha256(&stated_self_hash) {
        return Err(AppError::Validation("deployment manifest manifestSha256 is not canonical".into()));
    }
    let canonical = serde_json::to_vec(&canonical_json(&value))
        .map_err(|error| AppError::Other(format!("canonicalizing deployment manifest: {error}")))?;
    let actual_self_hash = hash_bytes(&canonical);
    if stated_self_hash != actual_self_hash {
        return Err(AppError::Validation(format!(
            "deployment manifest self-hash mismatch: expected {stated_self_hash}, got {actual_self_hash}"
        )));
    }
    let value_object = value
        .as_object_mut()
        .ok_or_else(|| AppError::Validation("deployment manifest top level must be an object".into()))?;
    value_object.insert("manifestSha256".into(), serde_json::Value::String(stated_self_hash));
    let manifest = manifest_from_value(value)?;

    validate_manifest_contract(&manifest)?;

    let manifest_path = path.canonicalize().map_err(AppError::Io)?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| AppError::Validation("deployment manifest has no parent directory".into()))?;
    let base_path = resolve_base(root, &manifest.base_checkpoint)?;
    let adapter_path =
        manifest.adapter.as_ref().map(|adapter| resolve_component(root, adapter, "adapter")).transpose()?;
    let tokenizer_path = resolve_component(root, &manifest.tokenizer, "tokenizer")?;
    let adapter_config_path =
        manifest.adapter_config.as_ref().map(|config| resolve_component(root, config, "adapter config")).transpose()?;

    Ok(VerifiedDeployment {
        manifest_path,
        deployment_sha256,
        manifest,
        base_path,
        adapter_path,
        tokenizer_path,
        adapter_config_path,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WslComponentIdentity {
    base: String,
    #[serde(default)]
    adapter: Option<String>,
    #[serde(default)]
    adapter_config: Option<String>,
    tokenizer: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WslVerificationReply {
    schema: u32,
    status: String,
    protocol: String,
    protocol_version: u32,
    family: String,
    model_version_id: String,
    deployment_sha256: String,
    manifest_sha256: String,
    component_sha256: WslComponentIdentity,
    language: String,
    provenance_kind: String,
    worker: String,
    manifest: serde_json::Value,
}

fn validate_absolute_wsl_path(path: &str, label: &str) -> AppResult<()> {
    if path.len() > 4096
        || !path.starts_with('/')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(AppError::Validation(format!(
            "{label} must be a bounded absolute WSL path without traversal or control characters"
        )));
    }
    Ok(())
}

fn record_from_wsl_reply(
    stdout: &[u8],
    manifest_wsl_path: &str,
    expected_file_sha256: &str,
    expected_model_id: &str,
) -> AppResult<VerifiedDeploymentRecord> {
    let reply: WslVerificationReply = serde_json::from_slice(stdout)
        .map_err(|error| AppError::Validation(format!("WSL deployment verifier returned invalid JSON: {error}")))?;
    let manifest = manifest_from_value(reply.manifest)?;
    validate_manifest_contract(&manifest)?;
    let provenance_kind = match &manifest.provenance {
        DeploymentProvenance::Flywheel { .. } => "flywheel",
        DeploymentProvenance::LegacyBootstrap { .. } => "legacy_bootstrap",
    };
    if reply.schema != 1
        || reply.status != "verified"
        || reply.protocol != SERVER_PROTOCOL
        || reply.protocol_version != SERVER_PROTOCOL_VERSION
        || reply.family != OMNIASR_7B_FAMILY
        || reply.language != "ckb_Arab"
        || reply.worker != "verifier"
        || reply.provenance_kind != provenance_kind
        || reply.model_version_id != expected_model_id
        || reply.deployment_sha256 != expected_file_sha256
        || manifest.model_id != reply.model_version_id
        || manifest.family != reply.family
        || manifest.language != reply.language
        || manifest.manifest_sha256 != reply.manifest_sha256
        || reply.component_sha256.base != manifest.base_checkpoint.sha256
        || reply.component_sha256.adapter.as_deref() != manifest.adapter.as_ref().map(|c| c.sha256.as_str())
        || reply.component_sha256.adapter_config.as_deref()
            != manifest.adapter_config.as_ref().map(|c| c.sha256.as_str())
        || reply.component_sha256.tokenizer != manifest.tokenizer.sha256
    {
        return Err(AppError::Validation(
            "WSL deployment verifier returned an internally inconsistent identity".into(),
        ));
    }
    Ok(VerifiedDeploymentRecord {
        manifest_path: manifest_wsl_path.to_string(),
        deployment_sha256: reply.deployment_sha256,
        manifest,
    })
}

/// Verify a deployment stored only inside WSL (for example `/home/ai/runs/...`). The bundled server
/// owns the same strict manifest/component verifier used immediately before bind. Arguments are passed
/// one token at a time (never through a shell), stdout/stderr are capped, and the process has a total
/// deadline. Rust independently validates and cross-checks the returned contract before registration.
pub fn verify_deployment_manifest_wsl(
    server_script_windows: &str,
    manifest_wsl_path: &str,
    expected_file_sha256: &str,
    expected_model_id: &str,
    timeout: Duration,
) -> AppResult<VerifiedDeploymentRecord> {
    if !is_sha256(expected_file_sha256) {
        return Err(AppError::Validation("expected WSL deployment SHA-256 is not canonical".into()));
    }
    crate::validation::input::validate_identifier(expected_model_id).map_err(AppError::Validation)?;
    validate_absolute_wsl_path(manifest_wsl_path, "deployment manifest")?;
    let server = Path::new(server_script_windows).canonicalize().map_err(|error| {
        AppError::Validation(format!("bundled deployment verifier '{server_script_windows}' is unavailable: {error}"))
    })?;
    if !server.is_file() {
        return Err(AppError::Validation("bundled deployment verifier is not a file".into()));
    }
    let server_wsl = crate::pipeline::win_path_to_wsl(&server.to_string_lossy());
    validate_absolute_wsl_path(&server_wsl, "bundled deployment verifier")?;
    let python = std::env::var("CORTEX_7B_PYTHON").unwrap_or_else(|_| "/home/ai/.venv-wsl-whisper/bin/python".into());
    validate_absolute_wsl_path(&python, "champion Python executable")?;

    let mut command = Command::new("wsl");
    command
        .arg("--")
        .arg(python)
        .arg(server_wsl)
        .arg("--verify-deployment")
        .arg(manifest_wsl_path)
        .arg("--expected-sha256")
        .arg(expected_file_sha256)
        .arg("--expected-model-id")
        .arg(expected_model_id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child =
        command.spawn().map_err(|error| AppError::Other(format!("launching WSL deployment verifier: {error}")))?;
    const MAX_OUTPUT: u64 = 1024 * 1024;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(ref mut stream) = stdout {
            use std::io::Read;
            let _ = stream.take(MAX_OUTPUT + 1).read_to_end(&mut bytes);
        }
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(ref mut stream) = stderr {
            use std::io::Read;
            let _ = stream.take(MAX_OUTPUT + 1).read_to_end(&mut bytes);
        }
        bytes
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                crate::pipeline::kill_and_reap_wsl_child(&mut child, "deployment verifier timeout");
                let _ = crate::pipeline::join_wsl_pipe_reader(stdout_reader, "deployment verifier stdout");
                let _ = crate::pipeline::join_wsl_pipe_reader(stderr_reader, "deployment verifier stderr");
                return Err(AppError::Other(format!(
                    "WSL deployment verification exceeded its {}s deadline",
                    timeout.as_secs()
                )));
            }
            Err(error) => {
                crate::pipeline::kill_and_reap_wsl_child(&mut child, "deployment verifier wait failure");
                let _ = crate::pipeline::join_wsl_pipe_reader(stdout_reader, "deployment verifier stdout");
                let _ = crate::pipeline::join_wsl_pipe_reader(stderr_reader, "deployment verifier stderr");
                return Err(AppError::Other(format!("waiting for WSL deployment verifier: {error}")));
            }
        }
    };
    let stdout = crate::pipeline::join_wsl_pipe_reader(stdout_reader, "deployment verifier stdout");
    let stderr = crate::pipeline::join_wsl_pipe_reader(stderr_reader, "deployment verifier stderr");
    if stdout.len() > MAX_OUTPUT as usize || stderr.len() > MAX_OUTPUT as usize {
        return Err(AppError::Validation("WSL deployment verifier output exceeded 1 MiB".into()));
    }
    if !status.success() {
        let detail: String = String::from_utf8_lossy(&stderr).chars().take(1000).collect();
        return Err(AppError::Validation(format!("WSL deployment verifier refused the manifest: {}", detail.trim())));
    }
    record_from_wsl_reply(&stdout, manifest_wsl_path, expected_file_sha256, expected_model_id)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    fn digest(bytes: &[u8]) -> String {
        hash_bytes(bytes)
    }

    fn write_component(root: &Path, name: &str, bytes: &[u8]) -> DeploymentComponent {
        std::fs::write(root.join(name), bytes).unwrap();
        DeploymentComponent { path: name.into(), sha256: digest(bytes), size_bytes: bytes.len() as u64 }
    }

    pub(crate) fn write_test_flywheel_manifest(root: &Path, model_id: &str) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let base_bytes = b"base";
        std::fs::write(root.join("base.pt"), base_bytes).unwrap();
        let mut value = serde_json::json!({
            "schema": 1,
            "model_id": model_id,
            "family": "omniasr-7b",
            "model_card": "soranivoice_omniASR_LLM_7B_v2_local",
            "language": "ckb_Arab",
            "provenance": {
                "kind": "flywheel",
                "snapshot": {"id": "1".repeat(64), "root_sha256": "2".repeat(64)},
                "training_run_id": "3".repeat(64),
                "trainer": {
                    "command": "trainer --contract contract.json",
                    "identity": "trainer@sha256:abc",
                    "recipe_sha256": "4".repeat(64),
                    "environment_sha256": "5".repeat(64),
                    "seed": 42
                },
                "source_evidence": {
                    "training_contract_sha256": "6".repeat(64),
                    "trainer_evidence_sha256": "7".repeat(64),
                    "artifact_manifest_sha256": "8".repeat(64)
                }
            },
            "base_checkpoint": {
                "id": "incumbent-1",
                "path": "base.pt",
                "sha256": digest(base_bytes),
                "size_bytes": base_bytes.len()
            },
            "adapter": write_component(root, "adapter_model.safetensors", b"adapter"),
            "tokenizer": write_component(root, "tokenizer.model", b"tokenizer"),
            "adapter_config": write_component(root, "adapter_config.json", b"config"),
            "server_compatibility": {"protocol": "cortex-omniasr-adapter", "version": 1},
            "runtime": {"arch": "7b", "vocab_size": 10288, "target_vocab_size": 10288}
        });
        let self_hash = hash_bytes(&serde_json::to_vec(&canonical_json(&value)).unwrap());
        value.as_object_mut().unwrap().insert("manifestSha256".into(), self_hash.into());
        let path = root.join("deployment_manifest.json");
        let bytes = canonical_manifest_bytes(&value).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&bytes).unwrap();
        path
    }

    /// Rewrite `value` as canonical manifest bytes with a fresh semantic self-hash.
    pub(crate) fn rewrite_test_manifest(path: &Path, mut value: serde_json::Value) {
        value.as_object_mut().unwrap().remove("manifestSha256");
        let self_hash = hash_bytes(&serde_json::to_vec(&canonical_json(&value)).unwrap());
        value.as_object_mut().unwrap().insert("manifestSha256".into(), self_hash.into());
        std::fs::write(path, canonical_manifest_bytes(&value).unwrap()).unwrap();
    }

    /// A schema-2 fused deployment: the merged checkpoint is `base_checkpoint`, no adapter files.
    pub(crate) fn write_test_fused_manifest(root: &Path, model_id: &str) -> PathBuf {
        let path = write_test_flywheel_manifest(root, model_id);
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("adapter");
        object.remove("adapter_config");
        std::fs::remove_file(root.join("adapter_model.safetensors")).unwrap();
        std::fs::remove_file(root.join("adapter_config.json")).unwrap();
        let fused = b"fused-weights";
        std::fs::write(root.join("fused.pt"), fused).unwrap();
        object.insert("schema".into(), FUSED_DEPLOYMENT_SCHEMA.into());
        object.insert(
            "base_checkpoint".into(),
            serde_json::json!({"id": "fused-1", "path": "fused.pt", "sha256": digest(fused), "size_bytes": fused.len()}),
        );
        rewrite_test_manifest(&path, value);
        path
    }

    #[test]
    fn fused_deployment_binds_the_exact_checkpoint_and_tokenizer_without_adapter_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_test_fused_manifest(dir.path(), "fused-1");
        let file_sha = crate::models::compute_file_sha256(&path).unwrap();
        let verified = verify_deployment_manifest(&path, Some(&file_sha)).unwrap();
        assert_eq!(verified.manifest.schema, FUSED_DEPLOYMENT_SCHEMA);
        assert!(verified.adapter_path.is_none() && verified.adapter_config_path.is_none());
        assert!(verified.base_path.ends_with("fused.pt"));
        assert_eq!(verified.manifest.base_checkpoint.sha256, digest(b"fused-weights"));

        for (name, tampered) in [("fused.pt", &b"fused-weightZ"[..]), ("tokenizer.model", &b"tokenizer!"[..])] {
            let original = std::fs::read(dir.path().join(name)).unwrap();
            std::fs::write(dir.path().join(name), tampered).unwrap();
            let error = verify_deployment_manifest(&path, None).unwrap_err().to_string();
            assert!(error.contains("mismatch"), "tampered {name}: {error}");
            std::fs::write(dir.path().join(name), original).unwrap();
        }
        // A sidecar checksum file next to the checkpoint carries no authority over the manifest.
        std::fs::write(dir.path().join("SHA256SUMS"), format!("{}  fused.pt\n", "0".repeat(64))).unwrap();
        assert!(verify_deployment_manifest(&path, Some(&file_sha)).is_ok());
    }

    #[test]
    fn fused_and_adapter_shapes_cannot_be_mixed_or_nulled() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_test_fused_manifest(dir.path(), "fused-1");
        let fused: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let adapter = write_component(dir.path(), "adapter_model.safetensors", b"adapter");
        let mut cases = Vec::new();
        let mut with_adapter = fused.clone();
        with_adapter["adapter"] = serde_json::to_value(&adapter).unwrap();
        cases.push(("fused with an adapter", with_adapter));
        let mut as_schema1 = fused.clone();
        as_schema1["schema"] = 1.into();
        cases.push(("schema 1 without adapters", as_schema1));
        let mut nulled = fused.clone();
        nulled["adapter"] = serde_json::Value::Null;
        cases.push(("explicit null adapter", nulled));
        let mut schema3 = fused;
        schema3["schema"] = 3.into();
        cases.push(("unknown schema", schema3));
        for (label, value) in cases {
            rewrite_test_manifest(&path, value);
            assert!(verify_deployment_manifest(&path, None).is_err(), "{label} was accepted");
        }
    }

    #[test]
    fn adapter_manifest_round_trips_byte_identically() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_test_flywheel_manifest(dir.path(), "challenger-1");
        let bytes = std::fs::read(&path).unwrap();
        let verified = verify_deployment_manifest(&path, None).unwrap();
        let reserialized = canonical_manifest_bytes(&serde_json::to_value(&verified.manifest).unwrap()).unwrap();
        assert_eq!(reserialized, bytes, "the optional adapter fields changed an adapter manifest's bytes");
    }

    #[test]
    fn wsl_verifier_reply_for_a_fused_deployment_is_cross_checked() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_test_fused_manifest(dir.path(), "fused-1");
        let verified = verify_deployment_manifest(&path, None).unwrap();
        let reply = verifier_reply(&verified);
        let wsl_path = "/home/ai/run/deployment_manifest.json";
        let record = record_from_wsl_reply(
            &serde_json::to_vec(&reply).unwrap(),
            wsl_path,
            &verified.deployment_sha256,
            "fused-1",
        )
        .unwrap();
        assert!(record.manifest.adapter.is_none());

        let mut claims_adapter = reply;
        claims_adapter["componentSha256"]["adapter"] = serde_json::Value::String("0".repeat(64));
        assert!(record_from_wsl_reply(
            &serde_json::to_vec(&claims_adapter).unwrap(),
            wsl_path,
            &verified.deployment_sha256,
            "fused-1",
        )
        .is_err());
    }

    #[test]
    fn verifies_the_complete_composite_identity() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_test_flywheel_manifest(dir.path(), "challenger-1");
        let file_sha = crate::models::compute_file_sha256(&path).unwrap();
        let verified = verify_deployment_manifest(&path, Some(&file_sha)).unwrap();
        assert_eq!(verified.manifest.model_id, "challenger-1");
        assert_eq!(verified.deployment_sha256, file_sha);
        assert!(verified.adapter_path.as_ref().unwrap().ends_with("adapter_model.safetensors"));
    }

    #[test]
    fn rejects_component_tampering_even_when_the_manifest_is_unchanged() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_test_flywheel_manifest(dir.path(), "challenger-1");
        std::fs::write(dir.path().join("adapter_model.safetensors"), b"changed").unwrap();
        let error = verify_deployment_manifest(&path, None).unwrap_err().to_string();
        assert!(error.contains("adapter size mismatch") || error.contains("adapter SHA-256 mismatch"), "{error}");
    }

    #[test]
    fn rejects_manifest_byte_drift_and_path_escape() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_test_flywheel_manifest(dir.path(), "challenger-1");
        let expected = crate::models::compute_file_sha256(&path).unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b" ").unwrap();
        let error = verify_deployment_manifest(&path, Some(&expected)).unwrap_err().to_string();
        assert!(error.contains("file SHA-256 mismatch"), "{error}");
    }

    fn verifier_reply(verified: &VerifiedDeployment) -> serde_json::Value {
        serde_json::json!({
            "schema": 1,
            "status": "verified",
            "protocol": SERVER_PROTOCOL,
            "protocolVersion": SERVER_PROTOCOL_VERSION,
            "family": OMNIASR_7B_FAMILY,
            "modelVersionId": verified.manifest.model_id,
            "deploymentSha256": verified.deployment_sha256,
            "manifestSha256": verified.manifest.manifest_sha256,
            "componentSha256": match (&verified.manifest.adapter, &verified.manifest.adapter_config) {
                (Some(adapter), Some(config)) => serde_json::json!({
                    "base": verified.manifest.base_checkpoint.sha256,
                    "adapter": adapter.sha256,
                    "adapterConfig": config.sha256,
                    "tokenizer": verified.manifest.tokenizer.sha256,
                }),
                _ => serde_json::json!({
                    "base": verified.manifest.base_checkpoint.sha256,
                    "tokenizer": verified.manifest.tokenizer.sha256,
                }),
            },
            "language": "ckb_Arab",
            "provenanceKind": "flywheel",
            "worker": "verifier",
            "manifest": verified.manifest,
        })
    }

    #[test]
    fn wsl_verifier_reply_is_cross_checked_not_blindly_trusted() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_test_flywheel_manifest(dir.path(), "challenger-1");
        let verified = verify_deployment_manifest(&path, None).unwrap();
        let reply = verifier_reply(&verified);
        let record = record_from_wsl_reply(
            &serde_json::to_vec(&reply).unwrap(),
            "/home/ai/run/deployment_manifest.json",
            &verified.deployment_sha256,
            "challenger-1",
        )
        .unwrap();
        assert_eq!(record.manifest_path, "/home/ai/run/deployment_manifest.json");
        assert_eq!(record.deployment_sha256, verified.deployment_sha256);

        let mut wrong = reply.clone();
        wrong["componentSha256"]["adapter"] = serde_json::Value::String("0".repeat(64));
        assert!(record_from_wsl_reply(
            &serde_json::to_vec(&wrong).unwrap(),
            "/home/ai/run/deployment_manifest.json",
            &verified.deployment_sha256,
            "challenger-1",
        )
        .is_err());

        let mut extra = reply;
        extra.as_object_mut().unwrap().insert("untrusted".into(), true.into());
        assert!(record_from_wsl_reply(
            &serde_json::to_vec(&extra).unwrap(),
            "/home/ai/run/deployment_manifest.json",
            &verified.deployment_sha256,
            "challenger-1",
        )
        .is_err());
    }

    #[test]
    fn wsl_manifest_paths_are_absolute_bounded_and_traversal_free() {
        assert!(validate_absolute_wsl_path("/home/ai/run/deployment_manifest.json", "manifest").is_ok());
        for bad in ["relative.json", "/home/../escape.json", "/home/./ambiguous.json", "/home/x\n.json"] {
            assert!(validate_absolute_wsl_path(bad, "manifest").is_err(), "accepted unsafe WSL path {bad:?}");
        }
    }
}
