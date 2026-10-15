//! Owner-only promotion authority (v12 plan, Part B step 5).
//!
//! No automatic path may promote an `external_offline` model (`registry::refuse_external_offline_promotion`).
//! The single exception is `src/bin/owner_promote.rs`, which ADMITS a durable promotion job only when
//! all of these hold:
//!
//! 1. an owner decision record (this module's [`OwnerPromotionAuthorization`]) is committed and
//!    byte-identical on the protected `origin/main`;
//! 2. the `canonLine` it quotes is present in `docs/OWNER_CANON.md` on `origin/main` — the file only the
//!    owner may change (`change canon: <item>`, pinned by `test_owner_canon_pins.py`), and the line must
//!    name the candidate's model id and full deployment SHA-256, so it authorizes one exact deployment;
//! 3. a phrase naming that deployment is typed on an interactive terminal;
//! 4. a deployment-bound `promotion_gate.py` verdict is `JUDGED_PASS` for exactly these two identities,
//!    over the held-out eval set whose proof is supplied; the canary suite exists.
//!
//! The record's SHA-256 is pinned in the job contract. The app's startup recovery runs the saga (exact
//! identity + canary, rollback on any failure) and lifts the `external_offline` refusal only for the exact
//! candidate a stored, re-hashed record names ([`load_verified_authorization`]).

use crate::champion_promotion::{ExactDeploymentIdentity, PromotionContract};
use crate::error::{AppError, AppResult};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const AUTHORIZATION_SCHEMA: u32 = 1;
pub const AUTHORIZATION_DECISION: &str = "promote_champion";
pub const AUTHORIZATIONS_DIR: &str = "promotion_authorizations";
const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// The committed owner decision record. Field names are camelCase JSON.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerPromotionAuthorization {
    pub schema: u32,
    pub decision: String,
    pub family: String,
    pub incumbent: ExactDeploymentIdentity,
    pub candidate: ExactDeploymentIdentity,
    /// Exact text the owner wrote in `docs/OWNER_CANON.md`; must start `change canon:`.
    pub canon_line: String,
    pub owner_words: String,
    pub date: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn invalid(detail: impl Into<String>) -> AppError {
    AppError::Validation(format!("owner promotion authorization refused: {}", detail.into()))
}

pub fn parse_authorization(bytes: &[u8]) -> AppResult<OwnerPromotionAuthorization> {
    let auth: OwnerPromotionAuthorization =
        serde_json::from_slice(bytes).map_err(|error| invalid(format!("record is not the schema: {error}")))?;
    if auth.schema != AUTHORIZATION_SCHEMA
        || auth.decision != AUTHORIZATION_DECISION
        || auth.family != crate::deployment::OMNIASR_7B_FAMILY
    {
        return Err(invalid("record must be schema 1, decision promote_champion, family omniasr-7b"));
    }
    for identity in [&auth.incumbent, &auth.candidate] {
        if identity.model_version_id.trim().is_empty() || !is_sha256(&identity.deployment_sha256) {
            return Err(invalid("incumbent/candidate need a model id and a lowercase deployment SHA-256"));
        }
    }
    if auth.candidate == auth.incumbent {
        return Err(invalid("candidate is the incumbent"));
    }
    let line = auth.canon_line.trim();
    if !line.contains("change canon:")
        || !line.contains(&auth.candidate.model_version_id)
        || !line.contains(&auth.candidate.deployment_sha256)
    {
        return Err(invalid(
            "canonLine must quote the owner's `change canon:` naming the candidate model id and full deployment SHA-256",
        ));
    }
    if auth.owner_words.trim().is_empty() || auth.date.len() != 10 {
        return Err(invalid("ownerWords and a YYYY-MM-DD date are required"));
    }
    Ok(auth)
}

/// True when some line of the owner canon contains the record's quoted line verbatim.
pub fn canon_authorizes(canon_text: &str, auth: &OwnerPromotionAuthorization) -> bool {
    let quoted = auth.canon_line.trim();
    canon_text.lines().any(|line| line.contains(quoted))
}

/// What the owner must type, so a confirmation names the exact deployment it authorizes.
pub fn confirmation_phrase(auth: &OwnerPromotionAuthorization) -> String {
    let short = auth.candidate.deployment_sha256.get(..12).unwrap_or("");
    format!("PROMOTE {} {short}", auth.candidate.model_version_id)
}

fn json_str<'a>(value: &'a serde_json::Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(serde_json::Value::as_str)
}

/// Check the deployment-bound gate verdict and the held-out proof against the authorization.
/// Returns the challenger scorecard SHA-256 the verdict binds.
pub fn verify_gate_evidence(
    verdict_bytes: &[u8],
    eval_proof_bytes: &[u8],
    auth: &OwnerPromotionAuthorization,
) -> AppResult<String> {
    let verdict: serde_json::Value =
        serde_json::from_slice(verdict_bytes).map_err(|error| invalid(format!("gate verdict is not JSON: {error}")))?;
    let proof: serde_json::Value = serde_json::from_slice(eval_proof_bytes)
        .map_err(|error| invalid(format!("held-out eval proof is not JSON: {error}")))?;
    if json_str(&verdict, "/binding") != Some("deployment")
        || json_str(&verdict, "/verdict") != Some("JUDGED_PASS")
        || verdict.pointer("/auto_promotable") != Some(&serde_json::Value::Bool(false))
    {
        return Err(invalid("gate verdict must be a deployment-bound JUDGED_PASS"));
    }
    for (role, identity) in [("champion", &auth.incumbent), ("challenger", &auth.candidate)] {
        if json_str(&verdict, &format!("/identities/{role}/model_id")) != Some(identity.model_version_id.as_str())
            || json_str(&verdict, &format!("/identities/{role}/deployment_sha256"))
                != Some(identity.deployment_sha256.as_str())
        {
            return Err(invalid(format!("gate verdict {role} is not the authorized deployment")));
        }
    }
    let eval_sha = json_str(&verdict, "/eval_manifest_sha256");
    if eval_sha.is_none()
        || eval_sha != json_str(&proof, "/eval_manifest_sha256")
        || json_str(&verdict, "/slices_sha256") != json_str(&proof, "/eval_slices_sha256")
    {
        return Err(invalid("gate verdict was not measured on the held-out set and slices this proof describes"));
    }
    match json_str(&verdict, "/identities/challenger/scorecard_sha256") {
        Some(sha) if is_sha256(sha) => Ok(sha.to_string()),
        _ => Err(invalid("gate verdict has no challenger scorecard SHA-256")),
    }
}

fn authorization_path(data_dir: &Path, sha: &str) -> Option<PathBuf> {
    is_sha256(sha).then(|| data_dir.join(AUTHORIZATIONS_DIR).join(format!("{sha}.json")))
}

/// Store the record content-addressed under the data dir; returns its SHA-256.
pub fn store_authorization(data_dir: &Path, bytes: &[u8]) -> AppResult<String> {
    let sha = sha256_hex(bytes);
    let path = authorization_path(data_dir, &sha).ok_or_else(|| invalid("could not derive the record path"))?;
    let dir = data_dir.join(AUTHORIZATIONS_DIR);
    std::fs::create_dir_all(&dir).map_err(AppError::Io)?;
    let tmp = dir.join(format!("{sha}.json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(AppError::Io)?;
    crate::atomic_file::replace_file(&tmp, &path).map_err(AppError::Io)?;
    Ok(sha)
}

/// The candidate an admitted job may promote despite `external_offline` provenance, or `None`.
/// Only a stored record whose bytes still hash to the pinned SHA and whose incumbent + candidate are
/// exactly the contract's grants anything; every other case leaves the automatic refusal in force.
pub fn load_verified_authorization(
    data_dir: &Path,
    sha: &str,
    contract: &PromotionContract,
) -> Option<ExactDeploymentIdentity> {
    let path = authorization_path(data_dir, sha)?;
    if path.metadata().ok()?.len() > MAX_RECORD_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if sha256_hex(&bytes) != sha {
        return None;
    }
    let auth = parse_authorization(&bytes).ok()?;
    (auth.candidate == contract.candidate && auth.incumbent == contract.incumbent).then_some(auth.candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::champion_promotion::CanaryIdentity;

    fn identity(id: &str, fill: char) -> ExactDeploymentIdentity {
        ExactDeploymentIdentity { model_version_id: id.into(), deployment_sha256: fill.to_string().repeat(64) }
    }

    fn record(canon_line: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "decision": "promote_champion",
            "family": "omniasr-7b",
            "incumbent": {"modelVersionId": "omniasr-7b-legacy-c348ade8a816", "deploymentSha256": "a".repeat(64)},
            "candidate": {"modelVersionId": "omniasr-7b-external-0123456789abcdef01234567", "deploymentSha256": "b".repeat(64)},
            "canonLine": canon_line,
            "ownerWords": "promote v12",
            "date": "2026-10-20",
        }))
        .unwrap()
    }

    fn good_line() -> String {
        format!(
            "change canon: promote omniasr-7b-external-0123456789abcdef01234567 deployment {} to champion",
            "b".repeat(64)
        )
    }

    fn contract(candidate: ExactDeploymentIdentity) -> PromotionContract {
        PromotionContract {
            incumbent: identity("omniasr-7b-legacy-c348ade8a816", 'a'),
            candidate: candidate.clone(),
            scorecard_sha256: "c".repeat(64),
            verdict_sha256: "d".repeat(64),
            canary: CanaryIdentity {
                suite_id: "suite".into(),
                suite_sha256: "e".repeat(64),
                expected_model: candidate,
            },
            owner_authorization_sha256: None,
        }
    }

    #[test]
    fn a_record_must_quote_a_canon_line_naming_the_exact_deployment() {
        let auth = parse_authorization(&record(&good_line())).unwrap();
        assert_eq!(confirmation_phrase(&auth), "PROMOTE omniasr-7b-external-0123456789abcdef01234567 bbbbbbbbbbbb");
        assert!(canon_authorizes(&format!("- **v12** (owner \"{}\", 2026-10-20)\n", good_line()), &auth));
        assert!(!canon_authorizes("# OWNER CANON\n- nothing about v12\n", &auth));
        for bad in [
            "promote omniasr-7b-external-0123456789abcdef01234567".to_string(), // no change canon:
            format!("change canon: promote v12 deployment {}", "b".repeat(64)), // no model id
            "change canon: promote omniasr-7b-external-0123456789abcdef01234567".to_string(), // no sha
        ] {
            assert!(parse_authorization(&record(&bad)).is_err(), "{bad}");
        }
        let mut extra: serde_json::Value = serde_json::from_slice(&record(&good_line())).unwrap();
        extra["approvedBy"] = "agent".into();
        assert!(parse_authorization(&serde_json::to_vec(&extra).unwrap()).is_err(), "unknown fields are refused");
    }

    #[test]
    fn gate_evidence_must_be_a_deployment_bound_pass_for_the_same_identities_and_eval_set() {
        let auth = parse_authorization(&record(&good_line())).unwrap();
        let proof = serde_json::json!({"eval_manifest_sha256": "1".repeat(64), "eval_slices_sha256": "2".repeat(64)});
        let verdict = serde_json::json!({
            "binding": "deployment", "verdict": "JUDGED_PASS", "auto_promotable": false,
            "eval_manifest_sha256": "1".repeat(64), "slices_sha256": "2".repeat(64),
            "identities": {
                "champion": {"model_id": auth.incumbent.model_version_id, "deployment_sha256": auth.incumbent.deployment_sha256},
                "challenger": {"model_id": auth.candidate.model_version_id, "deployment_sha256": auth.candidate.deployment_sha256,
                               "scorecard_sha256": "3".repeat(64)},
            },
        });
        let proof_bytes = serde_json::to_vec(&proof).unwrap();
        let check =
            |value: &serde_json::Value| verify_gate_evidence(&serde_json::to_vec(value).unwrap(), &proof_bytes, &auth);
        assert_eq!(check(&verdict).unwrap(), "3".repeat(64));
        for (pointer, value) in [
            ("/verdict", serde_json::json!("PROMOTE")),
            ("/verdict", serde_json::json!("REJECT")),
            ("/binding", serde_json::json!("run_record")),
            ("/auto_promotable", serde_json::json!(true)),
            ("/identities/challenger/deployment_sha256", serde_json::json!("f".repeat(64))),
            ("/identities/champion/model_id", serde_json::json!("someone-else")),
            ("/eval_manifest_sha256", serde_json::json!("9".repeat(64))),
            ("/slices_sha256", serde_json::json!("9".repeat(64))),
        ] {
            let mut tampered = verdict.clone();
            *tampered.pointer_mut(pointer).unwrap() = value;
            assert!(check(&tampered).is_err(), "{pointer} must be refused");
        }
    }

    #[test]
    fn recovery_grants_only_the_exact_candidate_of_an_unmodified_stored_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let bytes = record(&good_line());
        let sha = store_authorization(dir.path(), &bytes).unwrap();
        assert_eq!(sha, sha256_hex(&bytes));
        let candidate = identity("omniasr-7b-external-0123456789abcdef01234567", 'b');
        assert_eq!(
            load_verified_authorization(dir.path(), &sha, &contract(candidate.clone())),
            Some(candidate.clone())
        );
        let other = identity("omniasr-7b-external-ffffffffffffffffffffffff", 'b');
        assert_eq!(load_verified_authorization(dir.path(), &sha, &contract(other)), None, "a different candidate");
        assert_eq!(load_verified_authorization(dir.path(), &"0".repeat(64), &contract(candidate.clone())), None);
        assert_eq!(load_verified_authorization(dir.path(), "../x", &contract(candidate.clone())), None);
        std::fs::write(dir.path().join(AUTHORIZATIONS_DIR).join(format!("{sha}.json")), b"{}").unwrap();
        assert_eq!(load_verified_authorization(dir.path(), &sha, &contract(candidate)), None, "edited record");
    }
}
