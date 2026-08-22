//! Durable operating contract for a sequential paid-review campaign.
//!
//! The first pass is intentionally not a final dataset: one named reviewer works the full bound
//! focus, every decision remains attributable and payable, and every export/training boundary stays
//! closed until an independent second pass is implemented and completed.  The policy lives inside
//! SQLite's existing `settings` table, so the database and its recovery snapshots cannot separate
//! reviewer work from the rule that keeps that work provisional.

use crate::db::Database;
use crate::error::{AppError, AppResult};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;

pub const SEQUENTIAL_CAMPAIGN_SETTINGS_KEY: &str = "review_campaign.sequential_first_pass.v1";
pub const SEQUENTIAL_CAMPAIGN_MODE: &str = "sequential_first_pass";
pub const SEQUENTIAL_CAMPAIGN_STATUS: &str = "first_pass_active";
const MAX_REVIEWER_NAME: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SequentialReviewCampaign {
    pub schema_version: u32,
    pub campaign_id: String,
    pub mode: String,
    pub status: String,
    pub reviewer: String,
    /// Includes any valid first-pass work deliberately retained from the preceding canary.
    pub after_review_event_id: i64,
    /// Maximum event id observed by the offline activation transaction.
    pub activated_at_review_event_id: i64,
    pub focus_segment_count: usize,
    pub focus_sha256: String,
    pub provisional_export_block: bool,
    pub independent_second_pass_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusEvidence {
    pub segment_count: usize,
    pub sha256: String,
}

impl SequentialReviewCampaign {
    fn validate(mut self) -> Result<Self, String> {
        self.reviewer = self.reviewer.trim().to_string();
        if self.schema_version != 1 {
            return Err("sequential review campaign schema_version must be 1".to_string());
        }
        let parsed = uuid::Uuid::parse_str(&self.campaign_id)
            .map_err(|_| "sequential review campaign id must be a canonical UUID".to_string())?;
        if parsed.hyphenated().to_string() != self.campaign_id {
            return Err("sequential review campaign id must be a lowercase hyphenated UUID".to_string());
        }
        if self.mode != SEQUENTIAL_CAMPAIGN_MODE || self.status != SEQUENTIAL_CAMPAIGN_STATUS {
            return Err("sequential review campaign mode/status is unsupported".to_string());
        }
        if self.reviewer.is_empty()
            || self.reviewer.chars().count() > MAX_REVIEWER_NAME
            || self.reviewer.chars().any(char::is_control)
        {
            return Err("sequential review campaign contains an invalid reviewer".to_string());
        }
        if !self.reviewer.eq_ignore_ascii_case("Rubar") {
            return Err("sequential first pass is authorized only for Rubar".to_string());
        }
        self.reviewer = "Rubar".to_string();
        if self.after_review_event_id < 0
            || self.activated_at_review_event_id < self.after_review_event_id
            || self.focus_segment_count == 0
        {
            return Err("sequential review campaign contains an invalid event boundary or focus size".to_string());
        }
        if self.focus_sha256.len() != 64
            || !self.focus_sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("sequential review campaign focus digest must be lowercase SHA-256".to_string());
        }
        if !self.provisional_export_block || !self.independent_second_pass_required {
            return Err(
                "sequential first-pass work must remain export-blocked pending an independent second pass".to_string()
            );
        }
        Ok(self)
    }

    pub fn matches_reviewer(&self, reviewer: &str) -> bool {
        self.reviewer.eq_ignore_ascii_case(reviewer.trim())
    }
}

pub fn parse(raw: &str) -> Result<SequentialReviewCampaign, String> {
    serde_json::from_str::<SequentialReviewCampaign>(raw)
        .map_err(|error| format!("sequential review campaign is invalid: {error}"))?
        .validate()
}

pub fn load(db: &Database) -> Result<Option<SequentialReviewCampaign>, String> {
    let raw: Option<String> = db
        .connection()
        .query_row("SELECT value FROM settings WHERE key = ?1", [SEQUENTIAL_CAMPAIGN_SETTINGS_KEY], |row| row.get(0))
        .optional()
        .map_err(|error| format!("sequential review campaign cannot be read: {error}"))?;
    raw.as_deref().map(parse).transpose()
}

pub fn focus_evidence(ids: &HashSet<String>) -> Result<FocusEvidence, String> {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut digest = Sha256::new();
    for id in sorted {
        if id.is_empty() || id.contains('\n') || id.contains('\r') {
            return Err("voice focus contains an empty or newline-bearing segment id".to_string());
        }
        digest.update(id.as_bytes());
        digest.update(b"\n");
    }
    Ok(FocusEvidence {
        segment_count: ids.len(),
        sha256: digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect(),
    })
}

pub fn validate_focus(data_dir: &Path, policy: &SequentialReviewCampaign) -> Result<FocusEvidence, String> {
    let ids = crate::voice_focus::load_focus(data_dir)?
        .ok_or_else(|| "voice_focus.json is required during sequential review".to_string())?;
    let evidence = focus_evidence(&ids)?;
    if evidence.segment_count != policy.focus_segment_count || evidence.sha256 != policy.focus_sha256 {
        return Err(format!(
            "sequential review focus changed: found {} ids/{}, expected {} ids/{}",
            evidence.segment_count, evidence.sha256, policy.focus_segment_count, policy.focus_sha256
        ));
    }
    Ok(evidence)
}

/// All dataset/training exports remain closed while a provisional first pass is active.  This is
/// called from the underlying Rust exporters, not only the UI command layer, so headless binaries
/// cannot accidentally publish one-reviewer data either.
pub fn require_export_unblocked(db: &Database, operation: &str) -> AppResult<()> {
    match load(db) {
        Ok(None) => Ok(()),
        Ok(Some(policy)) => Err(AppError::Validation(format!(
            "{operation} blocked: campaign {} is a provisional Rubar first pass; complete independent second review before exporting or training",
            policy.campaign_id
        ))),
        Err(error) => Err(AppError::Validation(format!(
            "{operation} blocked because sequential review policy cannot be proven: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_policy() -> SequentialReviewCampaign {
        SequentialReviewCampaign {
            schema_version: 1,
            campaign_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            mode: SEQUENTIAL_CAMPAIGN_MODE.into(),
            status: SEQUENTIAL_CAMPAIGN_STATUS.into(),
            reviewer: " Rubar ".into(),
            after_review_event_id: 863,
            activated_at_review_event_id: 875,
            focus_segment_count: 2,
            focus_sha256: "a".repeat(64),
            provisional_export_block: true,
            independent_second_pass_required: true,
        }
    }

    #[test]
    fn policy_is_strict_and_cannot_disable_provisional_guards() {
        let policy = parse(&serde_json::to_string(&valid_policy()).unwrap()).unwrap();
        assert_eq!(policy.reviewer, "Rubar");
        assert!(policy.matches_reviewer("rUbAr"));
        let mut export_open = valid_policy();
        export_open.provisional_export_block = false;
        let mut no_second_pass = valid_policy();
        no_second_pass.independent_second_pass_required = false;
        let mut wrong_reviewer = valid_policy();
        wrong_reviewer.reviewer = "Alle".into();
        for bad in [export_open, no_second_pass, wrong_reviewer] {
            assert!(parse(&serde_json::to_string(&bad).unwrap()).is_err());
        }
        let mut unknown: serde_json::Value = serde_json::to_value(valid_policy()).unwrap();
        unknown["optionalExport"] = serde_json::json!(true);
        assert!(parse(&unknown.to_string()).is_err());
    }

    #[test]
    fn focus_digest_is_order_independent_and_final_lf_framed() {
        let a = HashSet::from(["b".to_string(), "a".to_string()]);
        let b = HashSet::from(["a".to_string(), "b".to_string()]);
        assert_eq!(focus_evidence(&a).unwrap(), focus_evidence(&b).unwrap());
        let expected = Sha256::digest(b"a\nb\n");
        let expected_hex: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(focus_evidence(&a).unwrap().sha256, expected_hex);
    }

    #[test]
    fn database_policy_is_fail_closed_at_the_underlying_export_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("campaign.db");
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        assert!(require_export_unblocked(&db, "test export").is_ok());
        db.connection()
            .execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)",
                [SEQUENTIAL_CAMPAIGN_SETTINGS_KEY, &serde_json::to_string(&valid_policy()).unwrap()],
            )
            .unwrap();
        let loaded = load(&db).unwrap().unwrap();
        assert_eq!(loaded.reviewer, "Rubar");
        let error = require_export_unblocked(&db, "test export").unwrap_err().to_string();
        assert!(error.contains("blocked") && error.contains("independent second review"));

        db.connection()
            .execute("UPDATE settings SET value = '{broken' WHERE key = ?1", [SEQUENTIAL_CAMPAIGN_SETTINGS_KEY])
            .unwrap();
        let error = require_export_unblocked(&db, "test export").unwrap_err().to_string();
        assert!(error.contains("cannot be proven"), "{error}");
    }
}
