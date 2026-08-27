//! Fail-closed operating policy for the small paid-review certification pilot.
//!
//! The pilot is deliberately narrower than the normal eight-reviewer Couch capability: exactly two
//! named reviewers, ten corpus-action slots each, twenty in total, plus exactly two hidden-QC keys
//! per reviewer (at most four additional compensated UI actions). Accept/edit/reject are the desired
//! canary decisions; a zero-pay skip still consumes one corpus safety slot so repeated skips cannot
//! refill queues or consume hidden keys beyond the proven bound. Names and the immutable event-id
//! baseline live in `<data_dir>/review_pilot_policy.json`; the limits are pinned here so a typo in an
//! operations file cannot silently turn a certification sample into an open-ended paid campaign.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;

pub const REVIEW_PILOT_FILE: &str = "review_pilot_policy.json";
/// Snapshot-side proof that the pilot policy was intentionally absent, rather than lost during copy.
pub const REVIEW_PILOT_ABSENT_MARKER_FILE: &str = "review_pilot_policy.absent";
pub const REVIEW_PILOT_ABSENT_MARKER_BYTES: &[u8] = b"review-pilot-policy-absent-v1\n";
/// Live fail-closed barrier written before a snapshot DB/policy restore and removed only after both
/// the dataset and its policy/config state are fully committed.
pub const REVIEW_PILOT_RESTORE_PENDING_FILE: &str = "review_pilot_policy.restore-pending";
pub const REVIEW_PILOT_SCHEMA_VERSION: u32 = 1;
pub const REVIEW_PILOT_REVIEWERS: usize = 2;
pub const REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER: i64 = 10;
pub const REVIEW_PILOT_TOTAL_CORPUS_ACTIONS: i64 = 20;
pub const REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER: i64 = 2;
pub const REVIEW_PILOT_TOTAL_HIDDEN_QC: i64 = 4;
pub const REVIEW_PILOT_MAX_COMPENSATED_UI_ACTIONS: i64 = 24;
/// First database schema that makes hidden-key grants part of the snapshotted SQLite authority.
pub const REVIEW_PILOT_HIDDEN_KEYS_SCHEMA_VERSION: i64 = 59;
pub const CONTROLLED_PILOT_FOCUS_CONTRACT_FILE: &str = "controlled_pilot_focus.json";
const CONTROLLED_PILOT_FOCUS_CONTRACT: &str = include_str!("../../controlled_pilot_focus.json");
const FOCUS_CANONICALIZATION: &str = "utf8_sorted_unique_ids_lf_join_final_lf_v1";
const MAX_REVIEWER_NAME: usize = 40;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PilotFocusContract {
    schema_version: u32,
    segment_id_count: usize,
    sorted_unique_segment_ids_sha256: String,
    canonicalization: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PilotFocusEvidence {
    pub segment_id_count: usize,
    pub sorted_unique_segment_ids_sha256: String,
}

fn parse_focus_contract(raw: &str) -> Result<PilotFocusContract, String> {
    let contract: PilotFocusContract = serde_json::from_str(raw)
        .map_err(|error| format!("{CONTROLLED_PILOT_FOCUS_CONTRACT_FILE} is invalid: {error}"))?;
    if contract.schema_version != 1 {
        return Err(format!("{CONTROLLED_PILOT_FOCUS_CONTRACT_FILE} schema_version must be 1"));
    }
    if contract.segment_id_count == 0 {
        return Err(format!("{CONTROLLED_PILOT_FOCUS_CONTRACT_FILE} segment_id_count must be positive"));
    }
    if contract.sorted_unique_segment_ids_sha256.len() != 64
        || !contract
            .sorted_unique_segment_ids_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{CONTROLLED_PILOT_FOCUS_CONTRACT_FILE} digest must be a canonical lowercase SHA-256"));
    }
    if contract.canonicalization != FOCUS_CANONICALIZATION {
        return Err(format!("{CONTROLLED_PILOT_FOCUS_CONTRACT_FILE} names an unsupported ID canonicalization"));
    }
    Ok(contract)
}

fn focus_evidence(ids: &HashSet<String>) -> Result<PilotFocusEvidence, String> {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut digest = Sha256::new();
    for id in sorted {
        if id.is_empty() || id.contains('\n') || id.contains('\r') {
            return Err(format!(
                "{} contains an empty or newline-bearing segment id",
                crate::voice_focus::VOICE_FOCUS_FILE
            ));
        }
        digest.update(id.as_bytes());
        digest.update(b"\n");
    }
    let digest = digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(PilotFocusEvidence { segment_id_count: ids.len(), sorted_unique_segment_ids_sha256: digest })
}

fn validate_focus_against_contract(
    ids: &HashSet<String>,
    contract: &PilotFocusContract,
) -> Result<PilotFocusEvidence, String> {
    let actual = focus_evidence(ids)?;
    if actual.segment_id_count != contract.segment_id_count {
        return Err(format!(
            "controlled-pilot voice focus has {} unique ids; expected exactly {}",
            actual.segment_id_count, contract.segment_id_count
        ));
    }
    if actual.sorted_unique_segment_ids_sha256 != contract.sorted_unique_segment_ids_sha256 {
        return Err(format!(
            "controlled-pilot voice focus digest mismatch: found {}, expected {}",
            actual.sorted_unique_segment_ids_sha256, contract.sorted_unique_segment_ids_sha256
        ));
    }
    Ok(actual)
}

#[cfg(test)]
fn test_focus_contracts() -> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, PilotFocusContract>>
{
    static CONTRACTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, PilotFocusContract>>,
    > = std::sync::OnceLock::new();
    CONTRACTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn expected_focus_contract(_data_dir: &Path) -> Result<PilotFocusContract, String> {
    #[cfg(test)]
    {
        let contracts = test_focus_contracts().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(contract) = contracts.get(_data_dir).cloned() {
            return Ok(contract);
        }
        // Snapshot tests copy/rename the exact synthetic focus into staging and promoted trees.
        // Recognize only a byte-semantically matching contract already registered by an explicit
        // `install_test_focus` call; production has neither this registry nor any override path.
        if let Ok(Some(ids)) = crate::voice_focus::load_focus(_data_dir) {
            if let Ok(actual) = focus_evidence(&ids) {
                if let Some(contract) = contracts.values().find(|candidate| {
                    candidate.segment_id_count == actual.segment_id_count
                        && candidate.sorted_unique_segment_ids_sha256 == actual.sorted_unique_segment_ids_sha256
                }) {
                    return Ok(contract.clone());
                }
            }
        }
    }
    parse_focus_contract(CONTROLLED_PILOT_FOCUS_CONTRACT)
}

/// Prove that an active controlled pilot is bound to the exact owner-authorized focus set.
/// Snapshot/restore preflight may call this for a policy-bearing recovery tree before promotion.
pub(crate) fn validate_controlled_focus(data_dir: &Path) -> Result<PilotFocusEvidence, String> {
    let contract = expected_focus_contract(data_dir)?;
    let ids = crate::voice_focus::load_focus(data_dir)?.ok_or_else(|| {
        format!("{} is required while controlled review is active", crate::voice_focus::VOICE_FOCUS_FILE)
    })?;
    validate_focus_against_contract(&ids, &contract)
}

/// Install a small, explicit focus plus its matching expectation for unit tests. The production
/// binary has no override path; it always parses the embedded tracked contract above.
#[cfg(test)]
pub(crate) fn install_test_focus<I, S>(data_dir: &Path, ids: I)
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let ids: HashSet<String> = ids.into_iter().map(Into::into).collect();
    let evidence = focus_evidence(&ids).expect("test focus ids must be canonical");
    let contract = PilotFocusContract {
        schema_version: 1,
        segment_id_count: evidence.segment_id_count,
        sorted_unique_segment_ids_sha256: evidence.sorted_unique_segment_ids_sha256,
        canonicalization: FOCUS_CANONICALIZATION.to_string(),
    };
    std::fs::write(
        data_dir.join(crate::voice_focus::VOICE_FOCUS_FILE),
        serde_json::to_vec(&serde_json::json!({ "name": "test", "segment_ids": ids })).expect("test focus serializes"),
    )
    .expect("test focus writes");
    test_focus_contracts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(data_dir.to_path_buf(), contract);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewPilotReviewer {
    pub name: String,
    pub max_corpus_actions: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewPilotPolicy {
    pub schema_version: u32,
    /// The greatest `review_events.id` that existed immediately before the pilot was armed.
    /// Only later `source='couch'` accept/edit/reject/skip events consume the cap.
    pub after_review_event_id: i64,
    pub max_total_corpus_actions: i64,
    pub reviewers: Vec<ReviewPilotReviewer>,
}

impl ReviewPilotPolicy {
    fn validate_and_canonicalize(mut self) -> Result<Self, String> {
        if self.schema_version != REVIEW_PILOT_SCHEMA_VERSION {
            return Err(format!("{REVIEW_PILOT_FILE} schema_version must be {REVIEW_PILOT_SCHEMA_VERSION}"));
        }
        if self.after_review_event_id < 0 {
            return Err(format!("{REVIEW_PILOT_FILE} after_review_event_id must be non-negative"));
        }
        if self.max_total_corpus_actions != REVIEW_PILOT_TOTAL_CORPUS_ACTIONS {
            return Err(format!(
                "{REVIEW_PILOT_FILE} must cap this certification pilot at exactly {REVIEW_PILOT_TOTAL_CORPUS_ACTIONS} corpus actions"
            ));
        }
        if self.reviewers.len() != REVIEW_PILOT_REVIEWERS {
            return Err(format!("{REVIEW_PILOT_FILE} must name exactly {REVIEW_PILOT_REVIEWERS} reviewers"));
        }
        for reviewer in &mut self.reviewers {
            reviewer.name = reviewer.name.trim().to_string();
            if reviewer.name.is_empty()
                || reviewer.name.chars().count() > MAX_REVIEWER_NAME
                || reviewer.name.chars().any(char::is_control)
            {
                return Err(format!("{REVIEW_PILOT_FILE} contains an invalid reviewer name"));
            }
            if reviewer.max_corpus_actions != REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER {
                return Err(format!(
                    "{REVIEW_PILOT_FILE} must cap each reviewer at exactly {REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER} corpus actions"
                ));
            }
        }
        if self.reviewers[0].name.eq_ignore_ascii_case(&self.reviewers[1].name) {
            return Err(format!("{REVIEW_PILOT_FILE} reviewer names must be distinct"));
        }
        self.reviewers.sort_by_key(|reviewer| reviewer.name.to_ascii_lowercase());
        Ok(self)
    }

    pub fn cap_for(&self, reviewer: &str) -> Option<i64> {
        self.reviewers
            .iter()
            .find(|entry| entry.name.trim().eq_ignore_ascii_case(reviewer.trim()))
            .map(|entry| entry.max_corpus_actions)
    }

    pub fn reviewer_names(&self) -> Vec<String> {
        self.reviewers.iter().map(|entry| entry.name.clone()).collect()
    }

    pub fn matches_session(&self, names: &[String]) -> bool {
        names.len() == self.reviewers.len()
            && self
                .reviewers
                .iter()
                .all(|entry| names.iter().any(|name| name.trim().eq_ignore_ascii_case(entry.name.trim())))
    }

    /// Stable identity of the validated policy's semantic fields.
    ///
    /// This deliberately does not hash the source JSON bytes: harmless whitespace, reviewer order,
    /// and ASCII case are not different policies under the authorization rules.  Length-framed
    /// fields and a domain separator make the digest unambiguous and safe to persist as the durable
    /// namespace for hidden-check reservations.
    pub fn policy_sha256(&self) -> Result<String, String> {
        let policy = self.clone().validate_and_canonicalize()?;
        let mut digest = Sha256::new();
        digest.update(b"cortex-review-pilot-policy-v1\0");
        digest.update(policy.schema_version.to_be_bytes());
        digest.update(policy.after_review_event_id.to_be_bytes());
        digest.update(policy.max_total_corpus_actions.to_be_bytes());
        digest.update((policy.reviewers.len() as u64).to_be_bytes());
        for reviewer in policy.reviewers {
            let canonical_name = reviewer.name.to_ascii_lowercase();
            let name_bytes = canonical_name.as_bytes();
            digest.update((name_bytes.len() as u64).to_be_bytes());
            digest.update(name_bytes);
            digest.update(reviewer.max_corpus_actions.to_be_bytes());
        }
        Ok(digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

pub fn parse(raw: &str) -> Result<ReviewPilotPolicy, String> {
    let parsed: ReviewPilotPolicy =
        serde_json::from_str(raw).map_err(|error| format!("{REVIEW_PILOT_FILE} is invalid: {error}"))?;
    parsed.validate_and_canonicalize()
}

/// Missing means the ordinary (non-pilot) Couch operating mode. Present-but-broken always errors;
/// an expressed paid-operation policy is never interpreted as unrestricted access. A durable
/// restore-pending marker is stronger than either state: it means DB/policy recovery was interrupted,
/// so paid review stays unavailable until the restore is completed or explicitly repaired.
pub fn load(data_dir: &Path) -> Result<Option<ReviewPilotPolicy>, String> {
    let pending_path = data_dir.join(REVIEW_PILOT_RESTORE_PENDING_FILE);
    crate::atomic_file::recover_interrupted_replace(&pending_path)
        .map_err(|error| format!("controlled review restore barrier is unreadable: {error}"))?;
    if pending_path.exists() {
        return Err("controlled review is blocked because a database/policy restore did not finish; retry or repair the restore before serving paid work".to_string());
    }

    let path = data_dir.join(REVIEW_PILOT_FILE);
    crate::atomic_file::recover_interrupted_replace(&path)
        .map_err(|error| format!("{REVIEW_PILOT_FILE} interrupted-write recovery failed: {error}"))?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{REVIEW_PILOT_FILE} is unreadable: {error}")),
    };
    let policy = parse(&raw)?;
    validate_controlled_focus(data_dir)?;
    Ok(Some(policy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_json(after: i64) -> String {
        format!(
            r#"{{
              "schema_version": 1,
              "after_review_event_id": {after},
              "max_total_corpus_actions": 20,
              "reviewers": [
                {{"name": " Karwan ", "max_corpus_actions": 10}},
                {{"name": "Chiman", "max_corpus_actions": 10}}
              ]
            }}"#
        )
    }

    #[test]
    fn a_valid_policy_is_canonical_and_binds_exactly_two_reviewers() {
        let dir = tempfile::tempdir().unwrap();
        install_test_focus(dir.path(), ["segment-a", "segment-b"]);
        std::fs::write(dir.path().join(REVIEW_PILOT_FILE), policy_json(863)).unwrap();
        let policy = load(dir.path()).unwrap().unwrap();
        assert_eq!(policy.after_review_event_id, 863);
        assert_eq!(policy.reviewer_names(), vec!["Chiman", "Karwan"]);
        assert_eq!(policy.cap_for(" kArWan "), Some(10));
        assert!(policy.matches_session(&["Karwan".into(), "chiman".into()]));
        assert!(!policy.matches_session(&["Karwan".into()]));
    }

    #[test]
    fn policy_digest_is_stable_for_equivalent_semantics_and_changes_with_the_baseline() {
        let policy = parse(&policy_json(863)).unwrap();
        let digest = policy.policy_sha256().unwrap();
        // Golden vector over the tracked FICTIONAL roster. Recomputed when the fixture names were
        // de-identified for public release, and verified names-only: the independent Python
        // implementation (scripts/review_pilot_hidden_contract.policy_sha256) reproduces the previous
        // hex from the previous names and this hex from these names, so the canonical serialisation
        // (sorted, lower-cased, whitespace-trimmed) is unchanged — only the input moved.
        assert_eq!(digest, "27dc8ed1866f311e4e16a3221b746efa64537b5e619fb6f9ebf76cf54723378e");
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));

        let mut equivalent = policy.clone();
        equivalent.reviewers.reverse();
        equivalent.reviewers[0].name = format!("  {}  ", equivalent.reviewers[0].name.to_ascii_uppercase());
        equivalent.reviewers[1].name = equivalent.reviewers[1].name.to_ascii_uppercase();
        assert_eq!(equivalent.policy_sha256().unwrap(), digest);

        let mut later = policy;
        later.after_review_event_id += 1;
        assert_ne!(later.policy_sha256().unwrap(), digest);
    }

    #[test]
    fn absence_is_normal_but_every_broken_or_weakened_policy_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Ok(None));
        install_test_focus(dir.path(), ["segment-a"]);
        let valid: serde_json::Value = serde_json::from_str(&policy_json(0)).unwrap();
        let mut wrong_total = valid.clone();
        wrong_total["max_total_corpus_actions"] = serde_json::json!(21);
        let mut wrong_reviewer_cap = valid.clone();
        wrong_reviewer_cap["reviewers"][0]["max_corpus_actions"] = serde_json::json!(20);
        let mut duplicate_reviewer = valid.clone();
        duplicate_reviewer["reviewers"][1]["name"] = duplicate_reviewer["reviewers"][0]["name"].clone();
        let mut third_reviewer = valid.clone();
        let mut added = third_reviewer["reviewers"][0].clone();
        added["name"] = serde_json::json!("Rezan");
        third_reviewer["reviewers"].as_array_mut().unwrap().push(added);
        let mut unknown_field = valid;
        unknown_field.as_object_mut().unwrap().insert("typo".into(), serde_json::json!(true));
        let bad_policies = [wrong_total, wrong_reviewer_cap, duplicate_reviewer, third_reviewer, unknown_field]
            .map(|value| serde_json::to_string(&value).unwrap());
        for bad in ["{not json}".to_string(), policy_json(-1)].into_iter().chain(bad_policies) {
            std::fs::write(dir.path().join(REVIEW_PILOT_FILE), bad).unwrap();
            assert!(load(dir.path()).is_err(), "broken policy was accepted");
        }
    }

    #[test]
    fn embedded_focus_contract_is_the_exact_owner_authorized_union() {
        let contract = parse_focus_contract(CONTROLLED_PILOT_FOCUS_CONTRACT).unwrap();
        assert_eq!(contract.segment_id_count, 8_278);
        assert_eq!(
            contract.sorted_unique_segment_ids_sha256,
            "9f7876c04ee7add77673f938460a5631056712b35a156c0d76b0cd7dca7ef3a7"
        );
    }

    #[test]
    fn focus_binding_rejects_8273_8275_and_same_count_wrong_id() {
        let baseline: HashSet<String> = (0..8_274).map(|index| format!("segment-{index:05}")).collect();
        let baseline_evidence = focus_evidence(&baseline).unwrap();
        let contract = PilotFocusContract {
            schema_version: 1,
            segment_id_count: baseline_evidence.segment_id_count,
            sorted_unique_segment_ids_sha256: baseline_evidence.sorted_unique_segment_ids_sha256,
            canonicalization: FOCUS_CANONICALIZATION.to_string(),
        };
        assert!(validate_focus_against_contract(&baseline, &contract).is_ok());

        let mut short = baseline.clone();
        short.remove("segment-08273");
        let short_error = validate_focus_against_contract(&short, &contract).unwrap_err();
        assert!(short_error.contains("8273"), "{short_error}");

        let mut long = baseline.clone();
        long.insert("segment-extra".to_string());
        let long_error = validate_focus_against_contract(&long, &contract).unwrap_err();
        assert!(long_error.contains("8275"), "{long_error}");

        let mut wrong = baseline;
        wrong.remove("segment-08273");
        wrong.insert("segment-wrong".to_string());
        let wrong_error = validate_focus_against_contract(&wrong, &contract).unwrap_err();
        assert!(wrong_error.contains("digest mismatch"), "{wrong_error}");
    }

    #[test]
    fn present_policy_requires_a_present_unbroken_matching_focus() {
        let dir = tempfile::tempdir().unwrap();
        install_test_focus(dir.path(), ["segment-a", "segment-b"]);
        std::fs::write(dir.path().join(REVIEW_PILOT_FILE), policy_json(863)).unwrap();
        assert!(load(dir.path()).is_ok());

        std::fs::remove_file(dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE)).unwrap();
        assert!(load(dir.path()).unwrap_err().contains("is required"));
        std::fs::write(dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE), r#"{"segment_ids":["segment-a"]}"#)
            .unwrap();
        assert!(load(dir.path()).unwrap_err().contains("expected exactly 2"));
        std::fs::write(dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE), b"{broken").unwrap();
        assert!(load(dir.path()).unwrap_err().contains("not valid JSON"));
    }
}
