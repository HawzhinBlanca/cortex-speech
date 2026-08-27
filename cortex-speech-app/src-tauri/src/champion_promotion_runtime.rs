//! Production adapter for the durable champion-promotion saga.
//!
//! This module deliberately exposes recovery only. A renderer-facing admission command is not added:
//! until promotion evidence is backend-minted and owner-authorized, "no initiation surface" is the
//! only honest trust boundary. Recovery consumes only the immutable contract already in the job row.

use crate::champion_promotion::{
    self, CanaryIdentity, ExactDeploymentIdentity, PromotionRuntime, PromotionRuntimeError,
};
use crate::db::Database;
use crate::deployment::OMNIASR_7B_FAMILY;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CANARY_SCHEMA: u32 = 1;
const MAX_CANARY_BYTES: u64 = 1024 * 1024;
const MAX_CANARY_CASES: usize = 8;
const WARMUP_DEADLINE: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanarySuite {
    schema: u32,
    suite_id: String,
    cases: Vec<CanaryCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanaryCase {
    segment_id: String,
    source_audio_sha256: String,
    alignment_sha256: String,
}

pub(crate) struct ProductionPromotionRuntime {
    app: tauri::AppHandle,
    db_path: String,
    data_dir: PathBuf,
    _lease: crate::engine_runtime::PromotionLease,
}

impl ProductionPromotionRuntime {
    fn new(app: &tauri::AppHandle, db_path: String, data_dir: PathBuf) -> Result<Self, String> {
        let lease = crate::engine_runtime::acquire_promotion_lease()?;
        Ok(Self { app: app.clone(), db_path, data_dir, _lease: lease })
    }

    fn open_db(&self) -> Result<Database, PromotionRuntimeError> {
        Database::open(&self.db_path).map_err(|error| runtime_error("PROMOTION_DB_OPEN_FAILED", error))
    }

    fn verify_registered_deployment(
        &self,
        db: &Database,
        identity: &ExactDeploymentIdentity,
    ) -> Result<(), PromotionRuntimeError> {
        let row = crate::registry::get_model_version(db, &identity.model_version_id)
            .map_err(|error| runtime_error("PROMOTION_REGISTRY_READ_FAILED", error))?
            .ok_or_else(|| runtime_error("PROMOTION_MODEL_NOT_REGISTERED", &identity.model_version_id))?;
        if row.family != OMNIASR_7B_FAMILY || row.checkpoint_sha256 != identity.deployment_sha256 {
            return Err(runtime_error("PROMOTION_REGISTRY_IDENTITY_MISMATCH", &identity.model_version_id));
        }
        let record = if row.checkpoint_path.starts_with('/') {
            let server = crate::engine_runtime::server_script_path(&self.app)
                .ok_or_else(|| runtime_error("PROMOTION_VERIFIER_UNAVAILABLE", "bundled server script missing"))?;
            crate::deployment::verify_deployment_manifest_wsl(
                &server,
                &row.checkpoint_path,
                &row.checkpoint_sha256,
                &row.id,
                Duration::from_secs(10 * 60),
            )
            .map_err(|error| runtime_error("PROMOTION_DEPLOYMENT_VERIFY_FAILED", error))?
        } else {
            crate::deployment::verify_deployment_manifest(Path::new(&row.checkpoint_path), Some(&row.checkpoint_sha256))
                .map_err(|error| runtime_error("PROMOTION_DEPLOYMENT_VERIFY_FAILED", error))?
                .record()
        };
        if record.manifest.model_id != identity.model_version_id
            || record.deployment_sha256 != identity.deployment_sha256
        {
            return Err(runtime_error("PROMOTION_DEPLOYMENT_IDENTITY_MISMATCH", &identity.model_version_id));
        }
        Ok(())
    }

    fn wait_for_exact_identity(&self, identity: &ExactDeploymentIdentity) -> Result<(), PromotionRuntimeError> {
        let expected = crate::registry::DeploymentIdentity {
            model_version_id: identity.model_version_id.clone(),
            deployment_sha256: identity.deployment_sha256.clone(),
        };
        let deadline = Instant::now() + WARMUP_DEADLINE;
        loop {
            let last = match crate::engine_runtime::query_loaded_champion(&self.app, Duration::from_secs(5)) {
                Ok(loaded) if loaded.matches(&expected) => return Ok(()),
                Ok(loaded) => {
                    format!(
                        "loaded {}/{} instead of {}/{}",
                        loaded.model_version_id,
                        loaded.deployment_sha256,
                        expected.model_version_id,
                        expected.deployment_sha256
                    )
                }
                Err(error) => error,
            };
            if Instant::now() >= deadline {
                return Err(runtime_error("PROMOTION_EXACT_HEALTH_TIMEOUT", last));
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    fn run_canary(
        &self,
        identity: &ExactDeploymentIdentity,
        canary: &CanaryIdentity,
    ) -> Result<(), PromotionRuntimeError> {
        if &canary.expected_model != identity {
            return Err(runtime_error("PROMOTION_CANARY_EXPECTED_MODEL_MISMATCH", &canary.suite_id));
        }
        let suite = load_canary(&self.data_dir, canary)?;
        let client = crate::engine_runtime::resolve_client_script(&self.app)
            .ok_or_else(|| runtime_error("PROMOTION_CLIENT_UNAVAILABLE", "bundled client script missing"))?;
        let db = self.open_db()?;
        for case in suite.cases {
            crate::validation::input::validate_identifier(&case.segment_id)
                .map_err(|error| runtime_error("PROMOTION_CANARY_SEGMENT_INVALID", error))?;
            if !is_sha256(&case.source_audio_sha256) || !is_sha256(&case.alignment_sha256) {
                return Err(runtime_error("PROMOTION_CANARY_CASE_SHA_INVALID", &case.segment_id));
            }
            let segment = db
                .get_segment_by_id(&case.segment_id)
                .map_err(|error| runtime_error("PROMOTION_CANARY_SEGMENT_READ_FAILED", error))?
                .ok_or_else(|| runtime_error("PROMOTION_CANARY_SEGMENT_MISSING", &case.segment_id))?;
            let source_sha = crate::models::compute_file_sha256(Path::new(&segment.audio_path))
                .map_err(|error| runtime_error("PROMOTION_CANARY_AUDIO_HASH_FAILED", error))?;
            if source_sha != case.source_audio_sha256
                || sha256(segment.alignment_json.as_deref().unwrap_or("").as_bytes()) != case.alignment_sha256
            {
                return Err(runtime_error("PROMOTION_CANARY_INPUT_DRIFT", &case.segment_id));
            }
            let result =
                crate::pipeline::run_wsl_segment_transcript_with_script(&client, &case.segment_id, &self.db_path, None)
                    .map_err(|error| runtime_error("PROMOTION_CANARY_TRANSCRIBE_FAILED", error))?;
            if result.raw_transcript.trim().is_empty()
                || result.model_version_id != identity.model_version_id
                || result.deployment_sha256 != identity.deployment_sha256
            {
                return Err(runtime_error("PROMOTION_CANARY_RESULT_MISMATCH", &case.segment_id));
            }
        }
        Ok(())
    }
}

fn load_canary(data_dir: &Path, canary: &CanaryIdentity) -> Result<CanarySuite, PromotionRuntimeError> {
    crate::validation::input::validate_identifier(&canary.suite_id)
        .map_err(|error| runtime_error("PROMOTION_CANARY_ID_INVALID", error))?;
    let path = data_dir.join("promotion_canaries").join(format!("{}.json", canary.suite_id));
    let metadata = path.metadata().map_err(|error| runtime_error("PROMOTION_CANARY_UNAVAILABLE", error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CANARY_BYTES {
        return Err(runtime_error("PROMOTION_CANARY_SIZE_INVALID", path.display()));
    }
    let bytes = std::fs::read(&path).map_err(|error| runtime_error("PROMOTION_CANARY_READ_FAILED", error))?;
    if sha256(&bytes) != canary.suite_sha256 {
        return Err(runtime_error("PROMOTION_CANARY_SHA_MISMATCH", path.display()));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| runtime_error("PROMOTION_CANARY_JSON_INVALID", error))?;
    let mut canonical = serde_json::to_vec(&canonical_json(value.clone()))
        .map_err(|error| runtime_error("PROMOTION_CANARY_CANONICALIZE_FAILED", error))?;
    canonical.push(b'\n');
    if bytes != canonical {
        return Err(runtime_error("PROMOTION_CANARY_NOT_CANONICAL", path.display()));
    }
    let suite: CanarySuite =
        serde_json::from_value(value).map_err(|error| runtime_error("PROMOTION_CANARY_SCHEMA_INVALID", error))?;
    if suite.schema != CANARY_SCHEMA
        || suite.suite_id != canary.suite_id
        || suite.cases.is_empty()
        || suite.cases.len() > MAX_CANARY_CASES
    {
        return Err(runtime_error("PROMOTION_CANARY_CONTRACT_INVALID", path.display()));
    }
    Ok(suite)
}

impl PromotionRuntime for ProductionPromotionRuntime {
    fn set_database_champion(&mut self, identity: &ExactDeploymentIdentity) -> Result<(), PromotionRuntimeError> {
        let db = self.open_db()?;
        self.verify_registered_deployment(&db, identity)?;
        crate::registry::promote_to_champion(&db, &identity.model_version_id)
            .map_err(|error| runtime_error("PROMOTION_DATABASE_SWAP_FAILED", error))?;
        let current = crate::registry::champion_identity(&db, OMNIASR_7B_FAMILY)
            .map_err(|error| runtime_error("PROMOTION_DATABASE_VERIFY_FAILED", error))?;
        if current.as_ref().map(|value| (&value.model_version_id, &value.deployment_sha256))
            != Some((&identity.model_version_id, &identity.deployment_sha256))
        {
            return Err(runtime_error("PROMOTION_DATABASE_VERIFY_FAILED", &identity.model_version_id));
        }
        Ok(())
    }

    fn publish_champion_pointer(&mut self, identity: &ExactDeploymentIdentity) -> Result<(), PromotionRuntimeError> {
        let db = self.open_db()?;
        let current = crate::registry::champion_identity(&db, OMNIASR_7B_FAMILY)
            .map_err(|error| runtime_error("PROMOTION_POINTER_DB_READ_FAILED", error))?;
        if current.as_ref().map(|value| (&value.model_version_id, &value.deployment_sha256))
            != Some((&identity.model_version_id, &identity.deployment_sha256))
        {
            return Err(runtime_error("PROMOTION_POINTER_DB_MISMATCH", &identity.model_version_id));
        }
        crate::registry::sync_champion_pointer(&db, &self.data_dir)
            .map_err(|error| runtime_error("PROMOTION_POINTER_WRITE_FAILED", error))?;
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(self.data_dir.join("champion.json"))
                .map_err(|error| runtime_error("PROMOTION_POINTER_READ_FAILED", error))?,
        )
        .map_err(|error| runtime_error("PROMOTION_POINTER_JSON_INVALID", error))?;
        let selected = value.get("champions").and_then(|families| families.get(OMNIASR_7B_FAMILY));
        if selected.and_then(|item| item.get("modelVersionId")).and_then(|item| item.as_str())
            != Some(identity.model_version_id.as_str())
            || selected.and_then(|item| item.get("deploymentSha256")).and_then(|item| item.as_str())
                != Some(identity.deployment_sha256.as_str())
        {
            return Err(runtime_error("PROMOTION_POINTER_VERIFY_FAILED", &identity.model_version_id));
        }
        Ok(())
    }

    fn restart_champion(&mut self) -> Result<(), PromotionRuntimeError> {
        crate::engine_runtime::restart_current_champion_for_promotion(&self.app)
            .map_err(|error| runtime_error("PROMOTION_RESTART_FAILED", error))
    }

    fn verify_exact_and_canary(
        &mut self,
        identity: &ExactDeploymentIdentity,
        canary: &CanaryIdentity,
    ) -> Result<(), PromotionRuntimeError> {
        self.wait_for_exact_identity(identity)?;
        self.run_canary(identity, canary)
    }
}

/// Resume the one durable promotion before startup publishes a registry pointer or starts
/// supervision. More than one running promotion is itself an invariant violation and fails closed.
pub(crate) fn recover_running_promotions(
    app: &tauri::AppHandle,
    db_path: String,
    data_dir: PathBuf,
) -> Result<usize, String> {
    let mut journal = Database::open(&db_path).map_err(|error| error.to_string())?;
    let jobs = champion_promotion::running_promotions(&journal).map_err(|error| error.to_string())?;
    if jobs.is_empty() {
        return Ok(0);
    }
    if jobs.len() != 1 {
        crate::engine_runtime::set_promotion_recovery_blocked(true);
        return Err(format!("found {} simultaneous running model promotions; refusing ambiguous recovery", jobs.len()));
    }
    let mut runtime = ProductionPromotionRuntime::new(app, db_path, data_dir)?;
    let result = champion_promotion::resume_promotion(&mut journal, &mut runtime, &jobs[0].id);
    match result {
        Ok(_) => Ok(1),
        Err(error) => {
            crate::engine_runtime::set_promotion_recovery_blocked(true);
            Err(error.to_string())
        }
    }
}

fn runtime_error(code: &'static str, detail: impl std::fmt::Display) -> PromotionRuntimeError {
    tracing::error!(promotion_error_code = code, detail = %detail, "champion promotion runtime failure");
    PromotionRuntimeError::new(code)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut fields: Vec<_> = map.into_iter().collect();
            fields.sort_by(|left, right| left.0.cmp(&right.0));
            serde_json::Value::Object(fields.into_iter().map(|(key, value)| (key, canonical_json(value))).collect())
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(values.into_iter().map(canonical_json).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_canary_bytes_and_hash_are_stable() {
        let value = serde_json::json!({"suiteId":"fixed-v1","schema":1,"cases":[]});
        let mut bytes = serde_json::to_vec(&canonical_json(value)).unwrap();
        bytes.push(b'\n');
        assert_eq!(bytes, b"{\"cases\":[],\"schema\":1,\"suiteId\":\"fixed-v1\"}\n");
        assert!(is_sha256(&sha256(&bytes)));
    }

    #[test]
    fn canary_loader_binds_exact_canonical_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("promotion_canaries");
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = b"{\"cases\":[{\"alignmentSha256\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"segmentId\":\"segment-1\",\"sourceAudioSha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}],\"schema\":1,\"suiteId\":\"fixed-v1\"}\n";
        std::fs::write(dir.join("fixed-v1.json"), bytes).unwrap();
        let candidate =
            ExactDeploymentIdentity { model_version_id: "candidate-1".into(), deployment_sha256: "c".repeat(64) };
        let identity =
            CanaryIdentity { suite_id: "fixed-v1".into(), suite_sha256: sha256(bytes), expected_model: candidate };
        assert_eq!(load_canary(tmp.path(), &identity).unwrap().cases.len(), 1);

        std::fs::write(dir.join("fixed-v1.json"), [bytes.as_slice(), b"\n"].concat()).unwrap();
        assert_eq!(load_canary(tmp.path(), &identity).unwrap_err().code, "PROMOTION_CANARY_SHA_MISMATCH");
    }
}
