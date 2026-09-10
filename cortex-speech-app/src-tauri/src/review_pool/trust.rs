//! Trusted-reviewer canon (owner change 2026-09-10, in the owner's words: "when Hawzhin previews,
//! directly goes to approve export, even lamo and sewa need just one round, if hawzhin and lamo and
//! sewa accept for first round they go to directly export, if they do for second round of course for
//! export, the other reviewers need second pass from reviewers").
//!
//! `<data_dir>/review_trust.json`:
//!
//! ```json
//! { "owner": "Hawzhin", "trusted": ["Lamo", "Sewa"] }
//! ```
//!
//! Resolution order for a clip's fresh judgements (an explicit owner adjudication still wins first):
//! 1. the owner judged → decided by the owner's verdict, whatever anyone else said;
//! 2. else any trusted reviewer judged → decided by their verdict when every trusted verdict agrees;
//!    trusted verdicts that disagree wait for the owner (`OwnerConflict`: served to nobody);
//! 3. else the two-different-reviewers rule of 2026-08-29, unchanged.
//!
//! The policy is read ONCE per process at startup (`install`), never per query: a resolution must
//! not change between two reads of the same database inside one export or restore proof. Missing
//! file = nobody trusted = the two-reviewer canon alone. Unreadable or invalid file = the same, with
//! an error log — a typo must never make a single verdict final by accident.

use super::*;
use std::sync::RwLock;

pub const FILE_NAME: &str = "review_trust.json";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrustPolicy {
    /// `reviewer_key` of the owner, whose single verdict decides a clip.
    pub owner: Option<String>,
    /// `reviewer_key`s whose single verdict decides a clip unless the owner judged it.
    pub trusted: HashSet<String>,
}

static INSTALLED: RwLock<Option<TrustPolicy>> = RwLock::new(None);

#[cfg(test)]
thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<TrustPolicy>> = const { std::cell::RefCell::new(None) };
}

/// Install the process-wide policy (app start, `pool_admin` start). Later installs replace it.
pub fn install(policy: TrustPolicy) {
    *INSTALLED.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(policy);
}

/// The policy in force for this process. Nobody trusted until `install` ran.
pub fn current() -> TrustPolicy {
    #[cfg(test)]
    if let Some(policy) = OVERRIDE.with(|cell| cell.borrow().clone()) {
        return policy;
    }
    INSTALLED.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone().unwrap_or_default()
}

/// Tests: run `body` with `policy` in force on this thread only.
#[cfg(test)]
pub fn with_policy<T>(policy: TrustPolicy, body: impl FnOnce() -> T) -> T {
    OVERRIDE.with(|cell| *cell.borrow_mut() = Some(policy));
    let result = body();
    OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    result
}

pub fn load(data_dir: &Path) -> TrustPolicy {
    let text = match std::fs::read_to_string(data_dir.join(FILE_NAME)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return TrustPolicy::default(),
        Err(error) => {
            tracing::error!("{FILE_NAME} exists but is unreadable ({error}); nobody is trusted, two reviewers decide");
            return TrustPolicy::default();
        }
    };
    match parse(&text) {
        Ok(policy) => policy,
        Err(error) => {
            tracing::error!("{FILE_NAME} is invalid ({error}); nobody is trusted, two reviewers decide");
            TrustPolicy::default()
        }
    }
}

pub fn parse(text: &str) -> Result<TrustPolicy, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| format!("not valid JSON: {error}"))?;
    let owner = match value.get("owner") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(name)) if !name.trim().is_empty() => Some(reviewer_key(Some(name))),
        Some(_) => return Err("\"owner\" must be a reviewer name".into()),
    };
    let trusted = match value.get("trusted") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().ok_or("\"trusted\" must contain only reviewer names"))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("\"trusted\" must be a list of reviewer names".into()),
    };
    let trusted: HashSet<String> =
        trusted.iter().filter(|name| !name.trim().is_empty()).map(|name| reviewer_key(Some(name))).collect();
    if trusted.iter().any(|key| Some(key) == owner.as_ref()) {
        return Err("the owner must not also be listed as trusted".into());
    }
    Ok(TrustPolicy { owner, trusted })
}

/// Steps 1 and 2 of the resolution order, or `None` when only the two-reviewer rule applies.
pub(super) fn resolve(judgements: &HashMap<String, JudgementEvidence>) -> Option<DerivedResolution> {
    let policy = current();
    if let Some(evidence) = policy.owner.as_deref().and_then(|owner| judgements.get(owner)) {
        return Some(DerivedResolution::Resolved {
            outcome: evidence.outcome.clone(),
            agreeing_reviewers: vec![evidence.reviewer.clone()],
            owner: false,
        });
    }
    let mut trusted: Vec<&JudgementEvidence> =
        judgements.iter().filter(|(key, _)| policy.trusted.contains(key.as_str())).map(|(_, e)| e).collect();
    let outcome = trusted.first()?.outcome.clone();
    if trusted.iter().any(|evidence| evidence.outcome != outcome) {
        // Trusted people disagree: the owner settles it; nobody else is served the clip.
        return Some(DerivedResolution::OwnerConflict);
    }
    trusted.sort_unstable_by_key(|evidence| evidence.reviewer.to_ascii_lowercase());
    Some(DerivedResolution::Resolved {
        outcome,
        agreeing_reviewers: trusted.iter().map(|evidence| evidence.reviewer.clone()).collect(),
        owner: false,
    })
}

/// Export authority: a resolution carried by the owner or a trusted reviewer needs no second name.
pub fn authorizes(agreeing_reviewers: &[String]) -> bool {
    let policy = current();
    agreeing_reviewers
        .iter()
        .map(|name| reviewer_key(Some(name)))
        .any(|key| policy.owner.as_deref() == Some(key.as_str()) || policy.trusted.contains(&key))
}

/// Is this `reviewer_key` the owner named by the policy?
pub fn is_owner(key: &str) -> bool {
    current().owner.as_deref() == Some(key)
}

/// Is this clip decided right now? Media serving asks before playing a clip a stale batch still
/// holds (audit 2026-09-10 finding 4): a verdict on it would be refused, so the listen is wasted.
pub fn is_decided(db: &Database, segment_id: &str) -> Result<bool, String> {
    let ids = serde_json::to_string(&[segment_id]).map_err(|error| error.to_string())?;
    let reviewers = reviewer_sets_for_ids_on(db.connection(), Some(&ids))?;
    let adjudications = owner_adjudications_for_ids_on(db.connection(), Some(&ids))?;
    let (resolution, _) = derive_resolution(segment_id, reviewers.get(segment_id), adjudications.get(segment_id));
    Ok(matches!(resolution, DerivedResolution::Resolved { .. }))
}

/// Fresh review eligibility for a previously served pool assignment. Media/renewal must also
/// reject an already-seen opinion or a conflict this person cannot settle, not just final clips.
pub fn may_review(db: &Database, segment_id: &str, reviewer: &str) -> Result<bool, String> {
    let ids = serde_json::to_string(&[segment_id]).map_err(|error| error.to_string())?;
    let reviewers = reviewer_sets_for_ids_on(db.connection(), Some(&ids))?;
    let adjudications = owner_adjudications_for_ids_on(db.connection(), Some(&ids))?;
    let current = reviewers.get(segment_id);
    let reviewer = reviewer_key(Some(reviewer));
    let (resolution, _) = derive_resolution(segment_id, current, adjudications.get(segment_id));
    Ok(permits_fresh_opinion(&resolution, current, &reviewer)
        && !super::reviewer_already_saw(db, segment_id, &reviewer)?)
}

pub(super) fn permits_fresh_opinion(
    resolution: &DerivedResolution,
    current: Option<&SegmentReviewers>,
    reviewer: &str,
) -> bool {
    if current.is_some_and(|coverage| coverage.seen.contains(reviewer)) {
        return false;
    }
    match resolution {
        DerivedResolution::Resolved { .. } => false,
        DerivedResolution::OwnerConflict => {
            is_owner(reviewer) && current.map_or(0, |coverage| coverage.judged.len()) < 3
        }
        DerivedResolution::Pending | DerivedResolution::NeedsThird => true,
    }
}

/// For `pool_admin probe` / status output.
pub fn describe() -> serde_json::Value {
    let policy = current();
    let mut trusted: Vec<&String> = policy.trusted.iter().collect();
    trusted.sort();
    serde_json::json!({ "owner": policy.owner, "trusted": trusted })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normalizes_names_and_refuses_the_wrong_shape() {
        let policy = parse(r#"{ "owner": " Hawzhin ", "trusted": ["Lamo", "", "sewa"] }"#).unwrap();
        assert_eq!(policy.owner.as_deref(), Some("hawzhin"));
        assert_eq!(policy.trusted, ["lamo".to_string(), "sewa".to_string()].into_iter().collect());
        assert!(parse(r#"{ "trusted": "Lamo" }"#).unwrap_err().contains("must be a list"));
        assert!(parse(r#"{ "owner": ["Hawzhin"] }"#).unwrap_err().contains("must be a reviewer name"));
        assert!(parse(r#"{ "owner": "Hawzhin", "trusted": ["hawzhin"] }"#).unwrap_err().contains("must not also"));
        assert!(parse("{ broken").unwrap_err().contains("not valid JSON"));
        assert_eq!(parse("{}").unwrap(), TrustPolicy::default(), "an empty object trusts nobody");
    }

    #[test]
    fn a_missing_or_broken_file_trusts_nobody() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), TrustPolicy::default());
        std::fs::write(dir.path().join(FILE_NAME), "{ broken").unwrap();
        assert_eq!(load(dir.path()), TrustPolicy::default(), "a typo must never make one verdict final");
        std::fs::write(dir.path().join(FILE_NAME), r#"{ "owner": "Hawzhin", "trusted": ["Lamo", "Sewa"] }"#).unwrap();
        let policy = load(dir.path());
        assert_eq!(policy.owner.as_deref(), Some("hawzhin"));
        assert!(policy.trusted.contains("lamo") && policy.trusted.contains("sewa"));
    }

    #[test]
    fn authority_and_ownership_follow_the_policy() {
        let policy = parse(r#"{ "owner": "Hawzhin", "trusted": ["Lamo"] }"#).unwrap();
        with_policy(policy, || {
            assert!(authorizes(&["Hawzhin".to_string()]));
            assert!(authorizes(&["lamo".to_string()]));
            assert!(!authorizes(&["Rubar".to_string()]));
            assert!(is_owner("hawzhin") && !is_owner("lamo"));
        });
        with_policy(TrustPolicy::default(), || {
            assert!(!authorizes(&["Hawzhin".to_string()]) && !is_owner("hawzhin"));
        });
    }

    #[test]
    fn without_a_policy_the_two_reviewer_rule_alone_applies() {
        let mut judgements = HashMap::new();
        judgements.insert(
            "hawzhin".to_string(),
            JudgementEvidence {
                reviewer: "Hawzhin".into(),
                evidence_id: "pool:1".into(),
                outcome: ReviewOutcome::Reject,
            },
        );
        with_policy(TrustPolicy::default(), || assert!(resolve(&judgements).is_none()));
    }
}
