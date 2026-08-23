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
pub const SEQUENTIAL_CAMPAIGN_PROGRESS_SETTINGS_KEY: &str = "review_campaign.sequential_progress.v1";
pub const SEQUENTIAL_CAMPAIGN_MODE: &str = "sequential_first_pass";
pub const SEQUENTIAL_CAMPAIGN_STATUS: &str = "first_pass_active";
pub const SECOND_PASS_REVIEWER: &str = "Alle";

type CampaignTransitionEvidence = (String, String, String, i64, i64, i64, i64, String);
type PairedDecisionEvidence = (i64, String, Option<String>, i64, String, Option<String>);
const MAX_REVIEWER_NAME: usize = 40;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CampaignPhase {
    #[default]
    FirstPassActive,
    SecondPassActive,
    AdjudicationActive,
    Completed,
}

impl CampaignPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstPassActive => "first_pass_active",
            Self::SecondPassActive => "second_pass_active",
            Self::AdjudicationActive => "adjudication_active",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignProgress {
    pub schema_version: u32,
    pub campaign_id: String,
    pub phase: CampaignPhase,
    pub transition_id: String,
    pub first_reviewer: String,
    pub second_reviewer: String,
    pub focus_segment_count: usize,
    pub focus_sha256: String,
    pub max_review_event_id: i64,
    pub independent_decision_count: usize,
    pub adjudication_count: usize,
    pub conflicts_remaining: usize,
}

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
    /// Runtime-only phase evidence loaded from the independently hashed progress setting and its
    /// immutable v61 transition row.  It is deliberately absent from the original policy JSON so
    /// the already-live first-pass contract never needs an in-place rewrite.
    #[serde(skip)]
    pub progress: Option<CampaignProgress>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FocusEvidence {
    pub segment_count: usize,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FirstPassStatus {
    pub focus_segment_count: usize,
    pub completed_segment_count: usize,
    pub pending_segment_count: usize,
    pub max_review_event_id: i64,
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
        self.authorized_reviewer().is_some_and(|name| name.eq_ignore_ascii_case(reviewer.trim()))
    }

    pub fn phase(&self) -> CampaignPhase {
        self.progress.as_ref().map_or(CampaignPhase::FirstPassActive, |progress| progress.phase)
    }

    pub fn authorized_reviewer(&self) -> Option<&str> {
        match self.phase() {
            CampaignPhase::FirstPassActive => Some(&self.reviewer),
            CampaignPhase::SecondPassActive => {
                Some(self.progress.as_ref().map_or(SECOND_PASS_REVIEWER, |progress| progress.second_reviewer.as_str()))
            }
            CampaignPhase::AdjudicationActive | CampaignPhase::Completed => None,
        }
    }

    pub fn is_blinded_second_pass(&self) -> bool {
        self.phase() == CampaignPhase::SecondPassActive
    }

    pub fn is_completed(&self) -> bool {
        self.phase() == CampaignPhase::Completed
    }
}

pub fn parse(raw: &str) -> Result<SequentialReviewCampaign, String> {
    serde_json::from_str::<SequentialReviewCampaign>(raw)
        .map_err(|error| format!("sequential review campaign is invalid: {error}"))?
        .validate()
}

fn campaign_authority_row_count(db: &Database) -> Result<i64, String> {
    let schema_version = crate::migrations::get_current_version(db).map_err(|error| error.to_string())?;
    if schema_version < 61 {
        return Ok(0);
    }
    db.connection()
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM review_campaign_registry)
               + (SELECT COUNT(*) FROM review_campaign_focus)
               + (SELECT COUNT(*) FROM review_campaign_transitions)
               + (SELECT COUNT(*) FROM independent_review_decisions)
               + (SELECT COUNT(*) FROM independent_review_reversals)
               + (SELECT COUNT(*) FROM review_campaign_adjudications)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("campaign database authority cannot be counted: {error}"))
}

fn validate_campaign_authority_scope(db: &Database, campaign_id: &str) -> Result<(), String> {
    let invalid: i64 = db
        .connection()
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM review_campaign_registry WHERE campaign_id <> ?1)
               + (SELECT COUNT(*) FROM review_campaign_focus WHERE campaign_id <> ?1)
               + (SELECT COUNT(*) FROM review_campaign_transitions WHERE campaign_id <> ?1)
               + (SELECT COUNT(*) FROM independent_review_decisions WHERE campaign_id <> ?1)
               + (SELECT COUNT(*) FROM review_campaign_adjudications WHERE campaign_id <> ?1)
               + (SELECT COUNT(*) FROM independent_review_reversals reversal
                    WHERE NOT EXISTS (
                          SELECT 1 FROM independent_review_decisions decision
                           WHERE decision.id = reversal.decision_id
                             AND decision.campaign_id = ?1
                    ))",
            [campaign_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("campaign database authority scope cannot be read: {error}"))?;
    let registries: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM review_campaign_registry", [], |row| row.get(0))
        .map_err(|error| format!("campaign registry scope cannot be read: {error}"))?;
    if invalid != 0 || registries != 1 {
        return Err("campaign database authority is not exclusively bound to the active campaign".to_string());
    }
    Ok(())
}

pub fn load(db: &Database) -> Result<Option<SequentialReviewCampaign>, String> {
    let raw: Option<String> = db
        .connection()
        .query_row("SELECT value FROM settings WHERE key = ?1", [SEQUENTIAL_CAMPAIGN_SETTINGS_KEY], |row| row.get(0))
        .optional()
        .map_err(|error| format!("sequential review campaign cannot be read: {error}"))?;
    let Some(raw) = raw else {
        let orphan_progress: Option<String> = db
            .connection()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [SEQUENTIAL_CAMPAIGN_PROGRESS_SETTINGS_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("sequential review progress cannot be read: {error}"))?;
        if orphan_progress.is_some() {
            return Err("sequential review progress exists without its base campaign policy".to_string());
        }
        if campaign_authority_row_count(db)? != 0 {
            return Err("campaign database authority exists without its base campaign policy".to_string());
        }
        return Ok(None);
    };
    let mut policy = parse(&raw)?;
    let progress_raw: Option<String> = db
        .connection()
        .query_row("SELECT value FROM settings WHERE key = ?1", [SEQUENTIAL_CAMPAIGN_PROGRESS_SETTINGS_KEY], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|error| format!("sequential review progress cannot be read: {error}"))?;
    if let Some(progress_raw) = progress_raw {
        let progress = parse_progress(&progress_raw, &policy)?;
        validate_campaign_authority_scope(db, &policy.campaign_id)?;
        validate_progress_authority(db, &progress_raw, &policy, &progress)?;
        policy.progress = Some(progress);
    } else if campaign_authority_row_count(db)? != 0 {
        return Err("campaign database authority exists before the first-pass transition".to_string());
    }
    Ok(Some(policy))
}

fn canonical_uuid(value: &str, label: &str) -> Result<(), String> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| format!("{label} must be a canonical UUID"))?;
    if parsed.hyphenated().to_string() != value {
        return Err(format!("{label} must be a lowercase hyphenated UUID"));
    }
    Ok(())
}

fn valid_lower_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_progress(raw: &str, policy: &SequentialReviewCampaign) -> Result<CampaignProgress, String> {
    let mut progress: CampaignProgress =
        serde_json::from_str(raw).map_err(|error| format!("sequential review progress is invalid: {error}"))?;
    progress.first_reviewer = progress.first_reviewer.trim().to_string();
    progress.second_reviewer = progress.second_reviewer.trim().to_string();
    if progress.schema_version != 1
        || progress.campaign_id != policy.campaign_id
        || progress.first_reviewer != "Rubar"
        || progress.second_reviewer != SECOND_PASS_REVIEWER
        || progress.focus_segment_count != policy.focus_segment_count
        || progress.focus_sha256 != policy.focus_sha256
        || progress.max_review_event_id < policy.activated_at_review_event_id
        || !valid_lower_sha256(&progress.focus_sha256)
    {
        return Err("sequential review progress does not match the bound campaign".to_string());
    }
    canonical_uuid(&progress.transition_id, "sequential review transition id")?;
    match progress.phase {
        CampaignPhase::FirstPassActive => {
            return Err("first-pass progress must be represented by the immutable base policy".to_string())
        }
        CampaignPhase::SecondPassActive => {
            if progress.independent_decision_count != 0
                || progress.adjudication_count != 0
                || progress.conflicts_remaining != 0
            {
                return Err("second-pass activation progress contains premature completion counts".to_string());
            }
        }
        CampaignPhase::AdjudicationActive => {
            if progress.independent_decision_count != policy.focus_segment_count
                || progress.conflicts_remaining == 0
                || progress.adjudication_count >= policy.focus_segment_count
            {
                return Err("adjudication progress contains impossible completion counts".to_string());
            }
        }
        CampaignPhase::Completed => {
            if progress.independent_decision_count != policy.focus_segment_count
                || progress.adjudication_count != policy.focus_segment_count
                || progress.conflicts_remaining != 0
            {
                return Err("completed campaign progress is not complete".to_string());
            }
        }
    }
    Ok(progress)
}

fn validate_progress_authority(
    db: &Database,
    raw: &str,
    policy: &SequentialReviewCampaign,
    progress: &CampaignProgress,
) -> Result<(), String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < 61 {
        return Err("sequential review progress requires schema 61".to_string());
    }
    let progress_sha256: String = Sha256::digest(raw.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    let registry: Option<(i64, String, String, String, i64, i64)> = db
        .connection()
        .query_row(
            "SELECT focus_segment_count, focus_sha256, first_reviewer, second_reviewer,
                    after_review_event_id, activated_at_review_event_id
               FROM review_campaign_registry WHERE campaign_id = ?1",
            [&policy.campaign_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()
        .map_err(|error| format!("review campaign registry cannot be read: {error}"))?;
    let expected_registry = (
        policy.focus_segment_count as i64,
        policy.focus_sha256.clone(),
        policy.reviewer.clone(),
        SECOND_PASS_REVIEWER.to_string(),
        policy.after_review_event_id,
        policy.activated_at_review_event_id,
    );
    if registry != Some(expected_registry) {
        return Err("review campaign registry does not exactly match the base policy".to_string());
    }
    let transition: Option<CampaignTransitionEvidence> = db
        .connection()
        .query_row(
            "SELECT transition_id, from_phase, to_phase, max_review_event_id,
                    independent_decision_count, adjudication_count, conflicts_remaining, progress_sha256
               FROM review_campaign_transitions
              WHERE campaign_id = ?1 ORDER BY id DESC LIMIT 1",
            [&policy.campaign_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("review campaign transition cannot be read: {error}"))?;
    let Some((transition_id, _from, to, max_event, decisions, adjudications, conflicts, digest)) = transition else {
        return Err("sequential review progress has no immutable transition evidence".to_string());
    };
    if transition_id != progress.transition_id
        || to != progress.phase.as_str()
        || max_event != progress.max_review_event_id
        || decisions != progress.independent_decision_count as i64
        || adjudications != progress.adjudication_count as i64
        || conflicts != progress.conflicts_remaining as i64
        || digest != progress_sha256
    {
        return Err("sequential review progress disagrees with its immutable transition".to_string());
    }
    verify_registered_focus(db, policy)?;
    match progress.phase {
        CampaignPhase::SecondPassActive => {
            verify_first_pass_complete(db, policy)?;
            let adjudications: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM review_campaign_adjudications WHERE campaign_id=?1",
                    [&policy.campaign_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("second-pass adjudication boundary cannot be read: {error}"))?;
            if adjudications != 0 {
                return Err("second-pass campaign contains premature adjudication authority".to_string());
            }
        }
        CampaignPhase::AdjudicationActive => {
            verify_first_pass_complete(db, policy)?;
            verify_independent_pass_complete(db, policy)?;
            let adjudications: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM review_campaign_adjudications WHERE campaign_id=?1",
                    [&policy.campaign_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("adjudication boundary cannot be read: {error}"))?;
            if adjudications != progress.adjudication_count as i64
                || adjudications + progress.conflicts_remaining as i64 != policy.focus_segment_count as i64
            {
                return Err("adjudication progress counts disagree with immutable adjudications".to_string());
            }
        }
        CampaignPhase::Completed => verify_campaign_completion(db, policy)?,
        CampaignPhase::FirstPassActive => unreachable!(),
    }
    Ok(())
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

fn registered_focus_ids(db: &Database, campaign_id: &str) -> Result<Vec<String>, String> {
    let mut statement = db
        .connection()
        .prepare(
            "SELECT segment_id FROM review_campaign_focus
              WHERE campaign_id = ?1 ORDER BY ordinal",
        )
        .map_err(|error| format!("registered campaign focus cannot be read: {error}"))?;
    let rows = statement
        .query_map([campaign_id], |row| row.get(0))
        .map_err(|error| format!("registered campaign focus cannot be queried: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("registered campaign focus cannot be decoded: {error}"))?;
    Ok(rows)
}

pub fn verify_registered_focus(db: &Database, policy: &SequentialReviewCampaign) -> Result<FocusEvidence, String> {
    let ids = registered_focus_ids(db, &policy.campaign_id)?;
    let unique: HashSet<String> = ids.iter().cloned().collect();
    if unique.len() != ids.len() {
        return Err("registered campaign focus contains duplicate segment ids".to_string());
    }
    let evidence = focus_evidence(&unique)?;
    if evidence.segment_count != policy.focus_segment_count || evidence.sha256 != policy.focus_sha256 {
        return Err(format!(
            "registered campaign focus changed: found {} ids/{}, expected {} ids/{}",
            evidence.segment_count, evidence.sha256, policy.focus_segment_count, policy.focus_sha256
        ));
    }
    Ok(evidence)
}

/// Read-only first-pass status for the exact external focus. Activation repeats this proof inside
/// its own IMMEDIATE transaction and compare-and-swap boundary before changing campaign phase.
pub fn first_pass_status_for_focus(
    db: &Database,
    policy: &SequentialReviewCampaign,
    focus_ids: &HashSet<String>,
) -> Result<FirstPassStatus, String> {
    if policy.phase() != CampaignPhase::FirstPassActive || policy.progress.is_some() {
        return Err("external first-pass status is available only before second-pass activation".to_string());
    }
    let evidence = focus_evidence(focus_ids)?;
    if evidence.segment_count != policy.focus_segment_count || evidence.sha256 != policy.focus_sha256 {
        return Err("supplied focus does not match the immutable first-pass policy".to_string());
    }
    let mut sorted: Vec<&str> = focus_ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let focus_json =
        serde_json::to_string(&sorted).map_err(|error| format!("voice focus cannot be encoded: {error}"))?;
    let (completed, maximum): (i64, i64) = db
        .connection()
        .query_row(
            "WITH supplied(segment_id) AS (SELECT value FROM json_each(?1))
             SELECT COUNT(DISTINCT supplied.segment_id), COALESCE(MAX(event.id), 0)
               FROM supplied
               JOIN speech_segments segment ON segment.id = supplied.segment_id
               JOIN effective_human_decision_effects_v60 effect
                 ON effect.segment_id = supplied.segment_id
               JOIN review_events event ON event.id = effect.review_event_id
              WHERE effect.reviewer = ?2 COLLATE NOCASE
                AND event.reviewer = ?2 COLLATE NOCASE
                AND event.source = 'couch'
                AND event.id > ?3
                AND event.action IN ('accept','edit','reject')
                AND effect.action = event.action
                AND segment.reviewed_by = ?2 COLLATE NOCASE
                AND segment.human_decision = effect.action
                AND segment.verified = 1
                AND (
                     (effect.action IN ('accept','edit')
                      AND segment.annotated_transcript IS effect.decision_annotated_transcript
                      AND trim(COALESCE(segment.annotated_transcript, '')) <> '')
                     OR (effect.action = 'reject' AND segment.verdict = 'human_reject')
                )",
            rusqlite::params![focus_json, policy.reviewer, policy.after_review_event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| format!("first-pass status cannot be read: {error}"))?;
    let completed = usize::try_from(completed).map_err(|_| "first-pass completed count is invalid".to_string())?;
    if completed > evidence.segment_count {
        return Err("first-pass completed count exceeds the immutable focus".to_string());
    }
    Ok(FirstPassStatus {
        focus_segment_count: evidence.segment_count,
        completed_segment_count: completed,
        pending_segment_count: evidence.segment_count - completed,
        max_review_event_id: maximum,
    })
}

/// Prove that every bound clip currently carries Rubar's effective, non-reversed phone decision and
/// that the decision was authored after the campaign baseline.  A row-level `verified = 1` flag is
/// intentionally insufficient: legacy desktop state or a stale row could otherwise impersonate a
/// completed paid first pass.
pub fn verify_first_pass_complete(db: &Database, policy: &SequentialReviewCampaign) -> Result<i64, String> {
    let (completed, maximum): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(event.id), 0)
               FROM review_campaign_focus focus
               JOIN speech_segments segment ON segment.id = focus.segment_id
               JOIN effective_human_decision_effects_v60 effect
                 ON effect.segment_id = focus.segment_id
               JOIN review_events event ON event.id = effect.review_event_id
              WHERE focus.campaign_id = ?1
                AND effect.reviewer = ?2 COLLATE NOCASE
                AND event.reviewer = ?2 COLLATE NOCASE
                AND event.source = 'couch'
                AND event.id > ?3
                AND event.action IN ('accept','edit','reject')
                AND effect.action = event.action
                AND segment.reviewed_by = ?2 COLLATE NOCASE
                AND segment.human_decision = effect.action
                AND segment.verified = 1
                AND (
                     (effect.action IN ('accept','edit')
                      AND segment.annotated_transcript IS effect.decision_annotated_transcript
                      AND trim(COALESCE(segment.annotated_transcript, '')) <> '')
                     OR (effect.action = 'reject' AND segment.verdict = 'human_reject')
                )",
            rusqlite::params![policy.campaign_id, policy.reviewer, policy.after_review_event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| format!("first-pass completion cannot be proven: {error}"))?;
    if completed != policy.focus_segment_count as i64 {
        return Err(format!(
            "first pass is incomplete or inconsistent: {completed}/{} clips have effective Rubar phone decisions",
            policy.focus_segment_count
        ));
    }
    Ok(maximum)
}

pub fn independent_pending_segment_ids(
    db: &Database,
    policy: &SequentialReviewCampaign,
) -> Result<Vec<String>, String> {
    if !policy.is_blinded_second_pass() {
        return Err("independent review queue is available only during second_pass_active".to_string());
    }
    let mut statement = db
        .connection()
        .prepare(
            "SELECT focus.segment_id
               FROM review_campaign_focus focus
              WHERE focus.campaign_id = ?1
                AND NOT EXISTS (
                    SELECT 1 FROM effective_independent_review_decisions_v61 decision
                     WHERE decision.campaign_id = focus.campaign_id
                       AND decision.segment_id = focus.segment_id
                )
              ORDER BY focus.ordinal",
        )
        .map_err(|error| format!("independent review queue cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([&policy.campaign_id], |row| row.get(0))
        .map_err(|error| format!("independent review queue cannot be queried: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("independent review queue cannot be decoded: {error}"))?;
    Ok(rows)
}

pub fn independent_segment_pending(
    db: &Database,
    policy: &SequentialReviewCampaign,
    segment_id: &str,
) -> Result<bool, String> {
    if !policy.is_blinded_second_pass() {
        return Ok(false);
    }
    db.connection()
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM review_campaign_focus focus
                  WHERE focus.campaign_id = ?1 AND focus.segment_id = ?2
                    AND NOT EXISTS (
                        SELECT 1 FROM effective_independent_review_decisions_v61 decision
                         WHERE decision.campaign_id = focus.campaign_id
                           AND decision.segment_id = focus.segment_id
                    )
             )",
            rusqlite::params![policy.campaign_id, segment_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("independent review assignment cannot be proven: {error}"))
}

pub fn verify_independent_pass_complete(db: &Database, policy: &SequentialReviewCampaign) -> Result<i64, String> {
    let (count, maximum): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(decision.id), 0)
               FROM review_campaign_focus focus
               JOIN effective_independent_review_decisions_v61 decision
                 ON decision.campaign_id = focus.campaign_id
                AND decision.segment_id = focus.segment_id
              WHERE focus.campaign_id = ?1
                AND decision.reviewer = ?2 COLLATE NOCASE",
            rusqlite::params![policy.campaign_id, SECOND_PASS_REVIEWER],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| format!("independent-pass completion cannot be proven: {error}"))?;
    if count != policy.focus_segment_count as i64 {
        return Err(format!(
            "independent pass is incomplete: {count}/{} clips have effective Alle decisions",
            policy.focus_segment_count
        ));
    }
    Ok(maximum)
}

fn exact_independent_consensus(
    first_action: &str,
    first_text: Option<&str>,
    second_action: &str,
    second_text: Option<&str>,
) -> Option<(String, Option<String>)> {
    if first_action == "reject" && second_action == "reject" {
        return Some(("reject".to_string(), None));
    }
    if matches!(first_action, "accept" | "edit")
        && matches!(second_action, "accept" | "edit")
        && first_text.is_some_and(|first| {
            second_text.is_some_and(|second| {
                crate::normalizer::learning_text_key(first) == crate::normalizer::learning_text_key(second)
            })
        })
    {
        return Some(("retain".to_string(), first_text.map(str::to_string)));
    }
    None
}

pub fn verify_campaign_completion(db: &Database, policy: &SequentialReviewCampaign) -> Result<(), String> {
    verify_registered_focus(db, policy)?;
    verify_first_pass_complete(db, policy)?;
    verify_independent_pass_complete(db, policy)?;
    let mut statement = db
        .connection()
        .prepare(
            "SELECT adjudication.resolution_kind, adjudication.final_action,
                    adjudication.final_transcript, adjudication.adjudicator,
                    first.action, first.decision_annotated_transcript,
                    second.action, second.submitted_transcript
               FROM review_campaign_focus focus
               JOIN review_campaign_adjudications adjudication
                 ON adjudication.campaign_id = focus.campaign_id
                AND adjudication.segment_id = focus.segment_id
               JOIN effective_independent_review_decisions_v61 second
                 ON second.id = adjudication.second_decision_id
               JOIN effective_human_decision_effects_v60 first
                 ON first.review_event_id = adjudication.first_review_event_id
              WHERE focus.campaign_id = ?1
                AND first.segment_id = focus.segment_id
                AND first.reviewer = ?2 COLLATE NOCASE
                AND second.campaign_id = focus.campaign_id
                AND second.segment_id = focus.segment_id
                AND second.reviewer = ?3 COLLATE NOCASE
              ORDER BY focus.ordinal",
        )
        .map_err(|error| format!("campaign adjudication completion cannot be prepared: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![policy.campaign_id, policy.reviewer, SECOND_PASS_REVIEWER], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|error| format!("campaign adjudication completion cannot be queried: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("campaign adjudication completion cannot be decoded: {error}"))?;
    if rows.len() != policy.focus_segment_count {
        return Err(format!(
            "campaign adjudication is incomplete or stale: {}/{} clips are sealed",
            rows.len(),
            policy.focus_segment_count
        ));
    }
    for (kind, final_action, final_text, adjudicator, first_action, first_text, second_action, second_text) in rows {
        match kind.as_str() {
            "exact_agreement" => {
                let exact = exact_independent_consensus(
                    &first_action,
                    first_text.as_deref(),
                    &second_action,
                    second_text.as_deref(),
                )
                .ok_or_else(|| "an automatic adjudication no longer has exact independent agreement".to_string())?;
                if final_action != exact.0
                    || final_text != exact.1
                    || adjudicator != "system:exact-independent-agreement"
                {
                    return Err("an automatic adjudication does not exactly match its independent evidence".to_string());
                }
            }
            "manual" => {
                if adjudicator.to_ascii_lowercase().starts_with("system:") {
                    return Err("a manual adjudication is masquerading as system authority".to_string());
                }
                match final_action.as_str() {
                    "retain" if final_text.as_deref().is_some_and(|text| !text.trim().is_empty()) => {}
                    "reject" if final_text.is_none() => {}
                    _ => return Err("a manual adjudication has an invalid final outcome".to_string()),
                }
            }
            _ => return Err("campaign adjudication has an unknown resolution kind".to_string()),
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct IndependentDecisionInput<'a> {
    pub segment_id: &'a str,
    pub reviewer: &'a str,
    pub action: &'a str,
    pub submitted_transcript: Option<&'a str>,
    pub served_transcript: &'a str,
    pub served_revision: i64,
    pub audio_content_hash: Option<&'a str>,
    pub source_start_ms: Option<i64>,
    pub source_end_ms: Option<i64>,
    pub duration_ms: i64,
    pub requested_action: &'a str,
    pub requested_transcript: &'a str,
    pub operation_id: &'a str,
    pub operation_payload_hash: &'a str,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndependentOperationReceipt {
    pub decision_id: i64,
    pub campaign_id: String,
    pub segment_id: String,
    pub reviewer: String,
    pub operation_payload_hash: String,
}

pub fn independent_operation(db: &Database, operation_id: &str) -> Result<Option<IndependentOperationReceipt>, String> {
    db.connection()
        .query_row(
            "SELECT id, campaign_id, segment_id, reviewer, operation_payload_hash
               FROM independent_review_decisions WHERE operation_id = ?1",
            [operation_id],
            |row| {
                Ok(IndependentOperationReceipt {
                    decision_id: row.get(0)?,
                    campaign_id: row.get(1)?,
                    segment_id: row.get(2)?,
                    reviewer: row.get(3)?,
                    operation_payload_hash: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("independent operation receipt cannot be read: {error}"))
}

/// Append one blinded decision without modifying the first-pass corpus row. Returns `None` when the
/// segment revision/raw draft changed after it was served; the caller must reload and let Alle judge
/// the fresh champion text.
pub fn record_independent_decision(
    db: &Database,
    policy: &SequentialReviewCampaign,
    input: &IndependentDecisionInput<'_>,
) -> Result<Option<i64>, String> {
    if !policy.is_blinded_second_pass() || !policy.matches_reviewer(input.reviewer) {
        return Err("independent decision is outside the active Alle second pass".to_string());
    }
    canonical_uuid(input.operation_id, "independent decision operation id")?;
    if !valid_lower_sha256(input.operation_payload_hash)
        || input.created_at_ms <= 0
        || input.served_revision < 0
        || input.duration_ms < 0
    {
        return Err("independent decision contains invalid operation or timing evidence".to_string());
    }
    match input.action {
        "accept" | "edit" if input.submitted_transcript.is_some_and(|text| !text.trim().is_empty()) => {}
        "reject" | "skip" if input.submitted_transcript.is_none() => {}
        _ => return Err("independent decision action/transcript is invalid".to_string()),
    }
    let changed = db
        .connection()
        .execute(
            "INSERT INTO independent_review_decisions
                (campaign_id, segment_id, reviewer, action, submitted_transcript,
                 served_transcript, served_revision, audio_content_hash, source_start_ms,
                 source_end_ms, duration_ms, requested_action, requested_transcript,
                 operation_id, operation_payload_hash, app_git_sha, playback_guard_version,
                 created_at_ms)
             SELECT ?1, segment.id, ?3, ?4, ?5, ?6, ?7,
                    CASE WHEN ?4 = 'skip' THEN NULL ELSE segment.audio_content_hash END,
                    CASE WHEN ?4 = 'skip' THEN NULL ELSE json_extract(segment.alignment_json, '$.source_start_ms') END,
                    CASE WHEN ?4 = 'skip' THEN NULL ELSE json_extract(segment.alignment_json, '$.source_end_ms') END,
                    segment.duration_ms, ?12, ?13, ?14, ?15, ?16,
                    'content-hash-raw-counter-v3', ?18
               FROM speech_segments segment
              WHERE segment.id = ?2
                AND segment.review_revision = ?7
                AND segment.raw_transcript = ?6
                AND segment.duration_ms = ?11
                AND (?4 = 'skip' OR (
                     segment.audio_content_hash = ?8
                     AND json_extract(segment.alignment_json, '$.source_start_ms') = ?9
                     AND json_extract(segment.alignment_json, '$.source_end_ms') = ?10
                ))",
            rusqlite::params![
                policy.campaign_id,
                input.segment_id,
                input.reviewer,
                input.action,
                input.submitted_transcript,
                input.served_transcript,
                input.served_revision,
                input.audio_content_hash,
                input.source_start_ms,
                input.source_end_ms,
                input.duration_ms,
                input.requested_action,
                input.requested_transcript,
                input.operation_id,
                input.operation_payload_hash,
                crate::GIT_SHA,
                "content-hash-raw-counter-v3",
                input.created_at_ms,
            ],
        )
        .map_err(|error| format!("independent decision cannot be committed: {error}"))?;
    if changed == 0 {
        return Ok(None);
    }
    let id = db.connection().last_insert_rowid();
    Ok(Some(id))
}

pub fn latest_independent_decision(
    db: &Database,
    campaign_id: &str,
    reviewer: &str,
) -> Result<Option<(i64, String, String)>, String> {
    db.connection()
        .query_row(
            "SELECT id, segment_id, operation_id FROM effective_independent_review_decisions_v61
              WHERE campaign_id = ?1 AND reviewer = ?2 COLLATE NOCASE
              ORDER BY id DESC LIMIT 1",
            rusqlite::params![campaign_id, reviewer],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| format!("latest independent decision cannot be read: {error}"))
}

pub fn reverse_independent_decision(
    db: &Database,
    policy: &SequentialReviewCampaign,
    decision_id: i64,
    reviewer: &str,
    operation_id: &str,
    created_at_ms: i64,
) -> Result<(), String> {
    if !policy.is_blinded_second_pass() || !policy.matches_reviewer(reviewer) {
        return Err("independent reversal is outside the active Alle second pass".to_string());
    }
    canonical_uuid(operation_id, "independent reversal operation id")?;
    let existing: Option<(String, String)> = db
        .connection()
        .query_row(
            "SELECT operation_id, reviewer FROM independent_review_reversals WHERE decision_id = ?1",
            [decision_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("independent reversal receipt cannot be read: {error}"))?;
    if let Some((existing_operation, existing_reviewer)) = existing {
        if existing_operation == operation_id && existing_reviewer.eq_ignore_ascii_case(reviewer) {
            return Ok(());
        }
        return Err("independent decision already has another reversal identity".to_string());
    }
    let changed = db
        .connection()
        .execute(
            "INSERT INTO independent_review_reversals
                (decision_id, operation_id, reviewer, created_at_ms)
             SELECT decision.id, ?2, ?3, ?4
               FROM independent_review_decisions decision
              WHERE decision.id = ?1 AND decision.campaign_id = ?5",
            rusqlite::params![decision_id, operation_id, reviewer, created_at_ms, policy.campaign_id],
        )
        .map_err(|error| format!("independent decision cannot be reversed: {error}"))?;
    if changed != 1 {
        return Err("independent decision reversal target is missing or outside this campaign".to_string());
    }
    Ok(())
}

fn progress_json_and_sha256(progress: &CampaignProgress) -> Result<(String, String), String> {
    let raw =
        serde_json::to_string(progress).map_err(|error| format!("campaign progress cannot be serialized: {error}"))?;
    let digest: String = Sha256::digest(raw.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    Ok((raw, digest))
}

/// Atomically register the file-owned focus in SQLite and advance from Rubar to Alle only after the
/// full first pass is proven under an IMMEDIATE transaction. `expected_max_review_event_id` is the
/// operator's compare-and-swap boundary: a decision arriving after preflight aborts the transition.
pub fn activate_second_pass(
    db: &Database,
    focus_ids: &HashSet<String>,
    expected_max_review_event_id: i64,
) -> Result<CampaignProgress, String> {
    let policy = load(db)?.ok_or_else(|| "no sequential review campaign is active".to_string())?;
    if policy.phase() != CampaignPhase::FirstPassActive || policy.progress.is_some() {
        return Err("campaign is not at the first-pass transition boundary".to_string());
    }
    let evidence = focus_evidence(focus_ids)?;
    if evidence.segment_count != policy.focus_segment_count || evidence.sha256 != policy.focus_sha256 {
        return Err("supplied focus does not match the immutable first-pass policy".to_string());
    }
    if expected_max_review_event_id < policy.activated_at_review_event_id {
        return Err("expected review-event boundary predates campaign activation".to_string());
    }
    let transition_id = uuid::Uuid::new_v4().hyphenated().to_string();
    let mut sorted: Vec<&str> = focus_ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
        .map_err(|error| format!("second-pass transition cannot lock the database: {error}"))?;
    let maximum: i64 = tx
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("review-event boundary cannot be read: {error}"))?;
    if maximum != expected_max_review_event_id {
        return Err(format!(
            "review-event compare-and-swap failed: expected {expected_max_review_event_id}, found {maximum}"
        ));
    }
    let existing_progress: i64 = tx
        .query_row("SELECT COUNT(*) FROM settings WHERE key = ?1", [SEQUENTIAL_CAMPAIGN_PROGRESS_SETTINGS_KEY], |row| {
            row.get(0)
        })
        .map_err(|error| format!("campaign progress boundary cannot be read: {error}"))?;
    let existing_registry: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM review_campaign_registry WHERE campaign_id = ?1",
            [&policy.campaign_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("campaign registry boundary cannot be read: {error}"))?;
    if existing_progress != 0 || existing_registry != 0 {
        return Err("campaign transition authority already exists; refusing a duplicate activation".to_string());
    }
    tx.execute(
        "INSERT INTO review_campaign_registry
            (campaign_id, focus_segment_count, focus_sha256, first_reviewer, second_reviewer,
             after_review_event_id, activated_at_review_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            policy.campaign_id,
            policy.focus_segment_count as i64,
            policy.focus_sha256,
            policy.reviewer,
            SECOND_PASS_REVIEWER,
            policy.after_review_event_id,
            policy.activated_at_review_event_id,
        ],
    )
    .map_err(|error| format!("campaign registry cannot be created: {error}"))?;
    {
        let mut insert = tx
            .prepare("INSERT INTO review_campaign_focus(campaign_id, segment_id, ordinal) VALUES(?1, ?2, ?3)")
            .map_err(|error| format!("campaign focus insert cannot be prepared: {error}"))?;
        for (ordinal, id) in sorted.iter().enumerate() {
            insert
                .execute(rusqlite::params![policy.campaign_id, id, ordinal as i64])
                .map_err(|error| format!("campaign focus cannot register {id}: {error}"))?;
        }
    }
    let completed: i64 = tx
        .query_row(
            "SELECT COUNT(*)
               FROM review_campaign_focus focus
               JOIN speech_segments segment ON segment.id = focus.segment_id
               JOIN effective_human_decision_effects_v60 effect
                 ON effect.segment_id = focus.segment_id
               JOIN review_events event ON event.id = effect.review_event_id
              WHERE focus.campaign_id = ?1
                AND effect.reviewer = ?2 COLLATE NOCASE
                AND event.reviewer = ?2 COLLATE NOCASE
                AND event.source = 'couch'
                AND event.id > ?3
                AND event.action IN ('accept','edit','reject')
                AND effect.action = event.action
                AND segment.reviewed_by = ?2 COLLATE NOCASE
                AND segment.human_decision = effect.action
                AND segment.verified = 1
                AND ((effect.action IN ('accept','edit')
                      AND segment.annotated_transcript IS effect.decision_annotated_transcript
                      AND trim(COALESCE(segment.annotated_transcript, '')) <> '')
                     OR (effect.action = 'reject' AND segment.verdict = 'human_reject'))",
            rusqlite::params![policy.campaign_id, policy.reviewer, policy.after_review_event_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("first-pass transition proof cannot be read: {error}"))?;
    if completed != policy.focus_segment_count as i64 {
        return Err(format!(
            "first pass is not complete: {completed}/{} exact focus clips have effective Rubar phone decisions",
            policy.focus_segment_count
        ));
    }
    let progress = CampaignProgress {
        schema_version: 1,
        campaign_id: policy.campaign_id.clone(),
        phase: CampaignPhase::SecondPassActive,
        transition_id: transition_id.clone(),
        first_reviewer: policy.reviewer.clone(),
        second_reviewer: SECOND_PASS_REVIEWER.to_string(),
        focus_segment_count: policy.focus_segment_count,
        focus_sha256: policy.focus_sha256.clone(),
        max_review_event_id: maximum,
        independent_decision_count: 0,
        adjudication_count: 0,
        conflicts_remaining: 0,
    };
    let (progress_raw, progress_sha256) = progress_json_and_sha256(&progress)?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO review_campaign_transitions
            (transition_id, campaign_id, from_phase, to_phase, max_review_event_id,
             independent_decision_count, adjudication_count, conflicts_remaining,
             progress_sha256, created_at_ms)
         VALUES(?1, ?2, 'first_pass_active', 'second_pass_active', ?3, 0, 0, 0, ?4, ?5)",
        rusqlite::params![transition_id, policy.campaign_id, maximum, progress_sha256, now_ms],
    )
    .map_err(|error| format!("second-pass transition evidence cannot be written: {error}"))?;
    tx.execute(
        "INSERT INTO settings(key, value) VALUES(?1, ?2)",
        rusqlite::params![SEQUENTIAL_CAMPAIGN_PROGRESS_SETTINGS_KEY, progress_raw],
    )
    .map_err(|error| format!("second-pass progress setting cannot be written: {error}"))?;
    tx.commit().map_err(|error| format!("second-pass transition cannot commit: {error}"))?;
    // Re-load through the same evidence validator every request will use; a successful write that
    // cannot be re-proven is an error, not a launch claim.
    let loaded = load(db)?.ok_or_else(|| "campaign disappeared after second-pass activation".to_string())?;
    if loaded.progress.as_ref() != Some(&progress) {
        return Err("second-pass activation did not round-trip through campaign authority".to_string());
    }
    Ok(progress)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManualAdjudication {
    pub segment_id: String,
    pub final_action: String,
    pub final_transcript: Option<String>,
    pub adjudicator: String,
}

fn first_and_second_evidence(
    conn: &rusqlite::Connection,
    policy: &SequentialReviewCampaign,
    segment_id: &str,
) -> Result<PairedDecisionEvidence, String> {
    conn.query_row(
        "SELECT event.id, first.action, first.decision_annotated_transcript,
                second.id, second.action, second.submitted_transcript
           FROM effective_human_decision_effects_v60 first
           JOIN review_events event ON event.id = first.review_event_id
           JOIN effective_independent_review_decisions_v61 second
             ON second.campaign_id = ?1 AND second.segment_id = first.segment_id
          WHERE first.segment_id = ?2
            AND first.reviewer = ?3 COLLATE NOCASE
            AND second.reviewer = ?4 COLLATE NOCASE",
        rusqlite::params![policy.campaign_id, segment_id, policy.reviewer, SECOND_PASS_REVIEWER],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )
    .map_err(|error| format!("campaign decision evidence for {segment_id} cannot be read: {error}"))
}

/// Seal exact agreements automatically and apply only explicitly supplied resolutions to conflicts.
/// With no manual inputs this safely advances a completed Alle pass into `adjudication_active` when
/// disagreements remain; it never guesses which human is right.
pub fn adjudicate_and_advance(db: &Database, manual: &[ManualAdjudication]) -> Result<CampaignProgress, String> {
    let policy = load(db)?.ok_or_else(|| "no sequential review campaign is active".to_string())?;
    if !matches!(policy.phase(), CampaignPhase::SecondPassActive | CampaignPhase::AdjudicationActive) {
        return Err("campaign is not ready for second-pass adjudication".to_string());
    }
    verify_independent_pass_complete(db, &policy)?;
    let mut manual_by_id = std::collections::HashMap::new();
    for item in manual {
        let segment_id = item.segment_id.trim().to_string();
        let adjudicator = item.adjudicator.trim().to_string();
        if segment_id.is_empty()
            || adjudicator.is_empty()
            || adjudicator.to_ascii_lowercase().starts_with("system:")
            || manual_by_id.insert(segment_id, item).is_some()
        {
            return Err("manual adjudications contain a blank, duplicate, or reserved system identity".to_string());
        }
        match item.final_action.as_str() {
            "retain" if item.final_transcript.as_deref().is_some_and(|text| !text.trim().is_empty()) => {}
            "reject" if item.final_transcript.is_none() => {}
            _ => return Err("manual adjudication action/transcript is invalid".to_string()),
        }
    }
    let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
        .map_err(|error| format!("campaign adjudication cannot lock the database: {error}"))?;
    let ids: Vec<String> = {
        let mut statement = tx
            .prepare("SELECT segment_id FROM review_campaign_focus WHERE campaign_id=?1 ORDER BY ordinal")
            .map_err(|error| format!("campaign focus cannot be read for adjudication: {error}"))?;
        let rows = statement
            .query_map([&policy.campaign_id], |row| row.get(0))
            .map_err(|error| format!("campaign focus cannot be queried for adjudication: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("campaign focus cannot be decoded for adjudication: {error}"))?;
        rows
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    for segment_id in &ids {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM review_campaign_adjudications
                                WHERE campaign_id=?1 AND segment_id=?2)",
                rusqlite::params![policy.campaign_id, segment_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("adjudication state cannot be read: {error}"))?;
        if exists {
            continue;
        }
        let (first_event_id, first_action, first_text, second_id, second_action, second_text) =
            first_and_second_evidence(&tx, &policy, segment_id)?;
        let exact =
            exact_independent_consensus(&first_action, first_text.as_deref(), &second_action, second_text.as_deref());
        let (kind, final_action, final_transcript, adjudicator) = if let Some((action, transcript)) = exact {
            ("exact_agreement", action, transcript, "system:exact-independent-agreement".to_string())
        } else if let Some(item) = manual_by_id.remove(segment_id) {
            (
                "manual",
                item.final_action.clone(),
                item.final_transcript.as_ref().map(|text| text.trim().to_string()),
                item.adjudicator.trim().to_string(),
            )
        } else {
            continue;
        };
        tx.execute(
            "INSERT INTO review_campaign_adjudications
                (adjudication_id, campaign_id, segment_id, first_review_event_id,
                 second_decision_id, resolution_kind, final_action, final_transcript,
                 adjudicator, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                uuid::Uuid::new_v4().hyphenated().to_string(),
                policy.campaign_id,
                segment_id,
                first_event_id,
                second_id,
                kind,
                final_action,
                final_transcript,
                adjudicator,
                now_ms,
            ],
        )
        .map_err(|error| format!("adjudication for {segment_id} cannot be sealed: {error}"))?;
    }
    if !manual_by_id.is_empty() {
        return Err("manual adjudications name clips that are already sealed or outside the campaign".to_string());
    }
    let adjudication_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM review_campaign_adjudications WHERE campaign_id=?1",
            [&policy.campaign_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("adjudication total cannot be read: {error}"))?;
    let conflicts_remaining = policy.focus_segment_count as i64 - adjudication_count;
    let completed = conflicts_remaining == 0;
    let from_phase = policy.phase().as_str();
    let to_phase = if completed { CampaignPhase::Completed } else { CampaignPhase::AdjudicationActive };
    if policy.phase() == CampaignPhase::AdjudicationActive && !completed {
        return Err(format!("{conflicts_remaining} campaign conflicts still require explicit manual adjudication"));
    }
    let maximum: i64 = tx
        .query_row("SELECT COALESCE(MAX(id),0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("review-event boundary cannot be read: {error}"))?;
    let transition_id = uuid::Uuid::new_v4().hyphenated().to_string();
    let progress = CampaignProgress {
        schema_version: 1,
        campaign_id: policy.campaign_id.clone(),
        phase: to_phase,
        transition_id: transition_id.clone(),
        first_reviewer: policy.reviewer.clone(),
        second_reviewer: SECOND_PASS_REVIEWER.to_string(),
        focus_segment_count: policy.focus_segment_count,
        focus_sha256: policy.focus_sha256.clone(),
        max_review_event_id: maximum,
        independent_decision_count: policy.focus_segment_count,
        adjudication_count: adjudication_count as usize,
        conflicts_remaining: conflicts_remaining as usize,
    };
    let (progress_raw, progress_sha256) = progress_json_and_sha256(&progress)?;
    tx.execute(
        "INSERT INTO review_campaign_transitions
            (transition_id, campaign_id, from_phase, to_phase, max_review_event_id,
             independent_decision_count, adjudication_count, conflicts_remaining,
             progress_sha256, created_at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            transition_id,
            policy.campaign_id,
            from_phase,
            to_phase.as_str(),
            maximum,
            policy.focus_segment_count as i64,
            adjudication_count,
            conflicts_remaining,
            progress_sha256,
            now_ms,
        ],
    )
    .map_err(|error| format!("campaign completion transition cannot be written: {error}"))?;
    tx.execute(
        "UPDATE settings SET value=?2 WHERE key=?1",
        rusqlite::params![SEQUENTIAL_CAMPAIGN_PROGRESS_SETTINGS_KEY, progress_raw],
    )
    .map_err(|error| format!("campaign progress cannot be advanced: {error}"))?;
    tx.commit().map_err(|error| format!("campaign adjudication cannot commit: {error}"))?;
    let loaded = load(db)?.ok_or_else(|| "campaign disappeared after adjudication".to_string())?;
    if loaded.progress.as_ref() != Some(&progress) {
        return Err("campaign adjudication did not round-trip through completion authority".to_string());
    }
    Ok(progress)
}

/// All dataset/training exports remain closed while a provisional first pass is active.  This is
/// called from the underlying Rust exporters, not only the UI command layer, so headless binaries
/// cannot accidentally publish one-reviewer data either.
pub fn require_export_unblocked(db: &Database, operation: &str) -> AppResult<()> {
    match load(db) {
        Ok(None) => Ok(()),
        // Generic/legacy exporters read the mutable speech_segments projection and therefore remain
        // closed even after campaign completion. Only the purpose-bound production exporter is
        // allowed to read the immutable adjudication authority directly.
        Ok(Some(policy)) => Err(AppError::Validation(format!(
            "{operation} blocked: campaign {} is {}; complete independent second review and use the purpose-bound adjudicated ASR/TTS production export",
            policy.campaign_id,
            policy.phase().as_str()
        ))),
        Err(error) => Err(AppError::Validation(format!(
            "{operation} blocked because sequential review policy cannot be proven: {error}"
        ))),
    }
}

pub fn require_finalized_production_export(db: &Database, operation: &str) -> AppResult<SequentialReviewCampaign> {
    let policy = load(db)
        .map_err(|error| AppError::Validation(format!("{operation} blocked: campaign authority failed: {error}")))?
        .ok_or_else(|| AppError::Validation(format!("{operation} blocked: no completed sequential campaign")))?;
    if !policy.is_completed() {
        return Err(AppError::Validation(format!(
            "{operation} blocked: campaign {} is {}",
            policy.campaign_id,
            policy.phase().as_str()
        )));
    }
    verify_campaign_completion(db, &policy)
        .map_err(|error| AppError::Validation(format!("{operation} blocked: {error}")))?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SpeechSegment;

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
            progress: None,
        }
    }

    #[test]
    fn policy_is_strict_and_cannot_disable_provisional_guards() {
        let policy = parse(&serde_json::to_string(&valid_policy()).unwrap()).unwrap();
        assert_eq!(policy.reviewer, "Rubar");
        assert_eq!(policy.authorized_reviewer(), Some("Rubar"));
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

    fn insert_test_campaign_registry(db: &Database, policy: &SequentialReviewCampaign) {
        db.connection()
            .execute(
                "INSERT INTO review_campaign_registry
                    (campaign_id, focus_segment_count, focus_sha256, first_reviewer, second_reviewer,
                     after_review_event_id, activated_at_review_event_id)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    policy.campaign_id,
                    policy.focus_segment_count as i64,
                    policy.focus_sha256,
                    "Rubar",
                    SECOND_PASS_REVIEWER,
                    policy.after_review_event_id,
                    policy.activated_at_review_event_id,
                ],
            )
            .unwrap();
    }

    #[test]
    fn database_authority_without_base_policy_is_refused() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_campaign_registry(&db, &valid_policy());
        let error = load(&db).unwrap_err();
        assert!(error.contains("without its base campaign policy"), "{error}");
    }

    #[test]
    fn database_authority_before_first_pass_transition_is_refused() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let policy = valid_policy();
        db.connection()
            .execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2)",
                [SEQUENTIAL_CAMPAIGN_SETTINGS_KEY, &serde_json::to_string(&policy).unwrap()],
            )
            .unwrap();
        insert_test_campaign_registry(&db, &policy);
        let error = load(&db).unwrap_err();
        assert!(error.contains("before the first-pass transition"), "{error}");
    }

    fn seeded_first_pass(count: usize) -> (Database, HashSet<String>, SequentialReviewCampaign, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let ids: HashSet<String> = (0..count).map(|index| format!("campaign-segment-{index:02}")).collect();
        let focus = focus_evidence(&ids).unwrap();
        let policy = SequentialReviewCampaign {
            schema_version: 1,
            campaign_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            mode: SEQUENTIAL_CAMPAIGN_MODE.into(),
            status: SEQUENTIAL_CAMPAIGN_STATUS.into(),
            reviewer: "Rubar".into(),
            after_review_event_id: 0,
            activated_at_review_event_id: 0,
            focus_segment_count: focus.segment_count,
            focus_sha256: focus.sha256,
            provisional_export_block: true,
            independent_second_pass_required: true,
            progress: None,
        };
        db.connection()
            .execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2)",
                [SEQUENTIAL_CAMPAIGN_SETTINGS_KEY, &serde_json::to_string(&policy).unwrap()],
            )
            .unwrap();
        let mut sorted: Vec<String> = ids.iter().cloned().collect();
        sorted.sort();
        for (index, id) in sorted.iter().enumerate() {
            let raw = format!("champion raw {index}");
            let source_start_ms = 5_000 + index as i64 * 2_000;
            let source_end_ms = source_start_ms + 1_000;
            let audio_path = temp.path().join(format!("{id}.wav"));
            let mut wav = hound::WavWriter::create(
                &audio_path,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 24_000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for sample in 0..24_000 {
                wav.write_sample::<i16>((sample % 97) as i16).unwrap();
            }
            wav.finalize().unwrap();
            db.insert_segment(&SpeechSegment {
                id: id.clone(),
                audio_path: audio_path.to_string_lossy().to_string(),
                raw_transcript: raw.clone(),
                alignment_json: Some(format!(
                    "{{\"source_start_ms\":{source_start_ms},\"source_end_ms\":{source_end_ms}}}"
                )),
                duration_ms: 1_000,
                speaker_id: Some("Lamo".into()),
                model_version_id: Some("omniasr-7b-legacy-c348ade8a816".into()),
                ..Default::default()
            })
            .unwrap();
            let content_hash = crate::export_bundle::current_canonical_pcm_blake3(&audio_path).unwrap();
            db.connection()
                .execute(
                    "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                    rusqlite::params![id, content_hash],
                )
                .unwrap();
            let operation_id = format!("00000000-0000-4000-8000-{:012x}", index + 1);
            let payload_hash = crate::db::review_operation_payload_hash(id, "accept", &raw, "Rubar");
            let served_revision = db.segment_review_revision(id).unwrap().unwrap();
            db.record_phone_human_decision_by_at_revision_with_operation(
                id,
                "accept",
                Some(&raw),
                "Rubar",
                served_revision,
                &operation_id,
                &payload_hash,
            )
            .unwrap()
            .unwrap();
        }
        (db, ids, policy, temp)
    }

    #[test]
    fn second_pass_is_blind_separate_reversible_and_completion_is_proof_gated() {
        let (db, ids, base, _temp) = seeded_first_pass(2);
        let status = first_pass_status_for_focus(&db, &base, &ids).unwrap();
        assert_eq!(status.focus_segment_count, 2);
        assert_eq!(status.completed_segment_count, 2);
        assert_eq!(status.pending_segment_count, 0);
        assert_eq!(status.max_review_event_id, db.max_review_event_id().unwrap());
        let mut wrong_focus = ids.clone();
        wrong_focus.insert("not-in-campaign".into());
        assert!(first_pass_status_for_focus(&db, &base, &wrong_focus).is_err());
        let maximum = db.max_review_event_id().unwrap();
        let activated = activate_second_pass(&db, &ids, maximum).unwrap();
        assert_eq!(activated.phase, CampaignPhase::SecondPassActive);
        let policy = load(&db).unwrap().unwrap();
        assert_eq!(policy.authorized_reviewer(), Some("Alle"));
        assert_eq!(independent_pending_segment_ids(&db, &policy).unwrap().len(), 2);
        assert!(require_export_unblocked(&db, "legacy export").is_err());

        let mut sorted: Vec<String> = ids.iter().cloned().collect();
        sorted.sort();
        for (index, id) in sorted.iter().enumerate() {
            let segment = db.get_segment_by_id(id).unwrap().unwrap();
            let raw = segment.raw_transcript.clone();
            let operation_id = format!("10000000-0000-4000-8000-{:012x}", index + 1);
            let payload_hash = crate::db::review_operation_payload_hash(id, "accept", &raw, "Alle");
            let content_hash = db.segment_audio_content_hash(id).unwrap().unwrap();
            let source_span = db.segment_source_span(id).unwrap().unwrap();
            let served_revision = db.segment_review_revision(id).unwrap().unwrap();
            let input = IndependentDecisionInput {
                segment_id: id,
                reviewer: "Alle",
                action: "accept",
                submitted_transcript: Some(&raw),
                served_transcript: &raw,
                served_revision,
                audio_content_hash: Some(&content_hash),
                source_start_ms: Some(source_span.0),
                source_end_ms: Some(source_span.1),
                duration_ms: 1_000,
                requested_action: "accept",
                requested_transcript: &raw,
                operation_id: &operation_id,
                operation_payload_hash: &payload_hash,
                created_at_ms: 1_000 + index as i64,
            };
            record_independent_decision(&db, &policy, &input).unwrap().unwrap();
            let unchanged = db.get_segment_by_id(id).unwrap().unwrap();
            assert_eq!(unchanged.reviewed_by.as_deref(), Some("Rubar"));
            assert_eq!(unchanged.annotated_transcript.as_deref(), Some(raw.as_str()));
        }
        assert!(independent_pending_segment_ids(&db, &policy).unwrap().is_empty());
        let completed = adjudicate_and_advance(&db, &[]).unwrap();
        assert_eq!(completed.phase, CampaignPhase::Completed);
        assert_eq!(completed.adjudication_count, 2);
        let completed_policy = require_finalized_production_export(&db, "production export").unwrap();
        assert!(completed_policy.is_completed());
        assert!(require_export_unblocked(&db, "legacy export").is_err());

        let raw_progress: String = db
            .connection()
            .query_row("SELECT value FROM settings WHERE key=?1", [SEQUENTIAL_CAMPAIGN_PROGRESS_SETTINGS_KEY], |row| {
                row.get(0)
            })
            .unwrap();
        let mut tampered: serde_json::Value = serde_json::from_str(&raw_progress).unwrap();
        tampered["conflicts_remaining"] = serde_json::json!(1);
        db.connection()
            .execute(
                "UPDATE settings SET value=?2 WHERE key=?1",
                rusqlite::params![SEQUENTIAL_CAMPAIGN_PROGRESS_SETTINGS_KEY, tampered.to_string()],
            )
            .unwrap();
        let tamper_error = load(&db).unwrap_err();
        assert!(
            tamper_error.contains("not complete") || tamper_error.contains("immutable transition"),
            "unexpected tamper failure: {tamper_error}"
        );
        assert_eq!(base.reviewer, "Rubar");
    }

    #[test]
    fn independent_undo_reopens_only_the_second_pass_projection() {
        let (db, ids, _, _temp) = seeded_first_pass(1);
        let maximum = db.max_review_event_id().unwrap();
        activate_second_pass(&db, &ids, maximum).unwrap();
        let policy = load(&db).unwrap().unwrap();
        let id = ids.iter().next().unwrap();
        let segment = db.get_segment_by_id(id).unwrap().unwrap();
        let raw = segment.raw_transcript.clone();
        let content_hash = db.segment_audio_content_hash(id).unwrap().unwrap();
        let source_span = db.segment_source_span(id).unwrap().unwrap();
        let served_revision = db.segment_review_revision(id).unwrap().unwrap();
        let operation_id = "20000000-0000-4000-8000-000000000001";
        let payload_hash = crate::db::review_operation_payload_hash(id, "accept", &raw, "Alle");
        let input = IndependentDecisionInput {
            segment_id: id,
            reviewer: "Alle",
            action: "accept",
            submitted_transcript: Some(&raw),
            served_transcript: &raw,
            served_revision,
            audio_content_hash: Some(&content_hash),
            source_start_ms: Some(source_span.0),
            source_end_ms: Some(source_span.1),
            duration_ms: 1_000,
            requested_action: "accept",
            requested_transcript: &raw,
            operation_id,
            operation_payload_hash: &payload_hash,
            created_at_ms: 2_000,
        };
        let decision_id = record_independent_decision(&db, &policy, &input).unwrap().unwrap();
        assert!(!independent_segment_pending(&db, &policy, id).unwrap());
        reverse_independent_decision(&db, &policy, decision_id, "Alle", operation_id, 3_000).unwrap();
        reverse_independent_decision(&db, &policy, decision_id, "Alle", operation_id, 3_000).unwrap();
        assert!(independent_segment_pending(&db, &policy, id).unwrap());
        let corpus = db.get_segment_by_id(id).unwrap().unwrap();
        assert_eq!(corpus.reviewed_by.as_deref(), Some("Rubar"));
        assert_eq!(db.segment_review_revision(id).unwrap(), Some(served_revision));
        let immutable = db
            .connection()
            .execute("UPDATE independent_review_decisions SET action='reject' WHERE id=?1", [decision_id])
            .unwrap_err()
            .to_string();
        assert!(immutable.contains("append-only"));
    }

    #[test]
    fn purpose_bound_export_is_atomic_rights_gated_and_preserves_tts_master_bytes() {
        let (db, ids, _, temp) = seeded_first_pass(1);
        activate_second_pass(&db, &ids, db.max_review_event_id().unwrap()).unwrap();
        let policy = load(&db).unwrap().unwrap();
        let id = ids.iter().next().unwrap();
        let segment = db.get_segment_by_id(id).unwrap().unwrap();
        let raw = segment.raw_transcript.clone();
        let content_hash = db.segment_audio_content_hash(id).unwrap().unwrap();
        let source_span = db.segment_source_span(id).unwrap().unwrap();
        let revision = db.segment_review_revision(id).unwrap().unwrap();
        let operation_id = "30000000-0000-4000-8000-000000000001";
        let payload_hash = crate::db::review_operation_payload_hash(id, "accept", &raw, "Alle");
        record_independent_decision(
            &db,
            &policy,
            &IndependentDecisionInput {
                segment_id: id,
                reviewer: "Alle",
                action: "accept",
                submitted_transcript: Some(&raw),
                served_transcript: &raw,
                served_revision: revision,
                audio_content_hash: Some(&content_hash),
                source_start_ms: Some(source_span.0),
                source_end_ms: Some(source_span.1),
                duration_ms: 1_000,
                requested_action: "accept",
                requested_transcript: &raw,
                operation_id,
                operation_payload_hash: &payload_hash,
                created_at_ms: 4_000,
            },
        )
        .unwrap()
        .unwrap();
        adjudicate_and_advance(&db, &[]).unwrap();

        let blocked_output = temp.path().join("blocked-production-export");
        let blocked = crate::production_dataset::export_finalized_voice_dataset(
            &db,
            &crate::production_dataset::ProductionDatasetOptions {
                output_dir: blocked_output.to_string_lossy().to_string(),
                voice_name: "Lamo".into(),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(blocked.contains("rights license is missing"), "unexpected rights gate: {blocked}");
        assert!(!blocked_output.exists(), "a failed export must never publish its destination");
        assert!(
            std::fs::read_dir(temp.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".staging-")),
            "a failed export must clean its private staging tree"
        );

        db.set_recording_rights(
            &segment.audio_path,
            &crate::db::RecordingRights {
                license: Some("owner-private".into()),
                consent_basis: Some("explicit_consent".into()),
                permitted_use: Some("train,tts".into()),
                attribution: None,
                source: Some("owner-recorded Lamo session".into()),
                revoked_at: None,
            },
        )
        .unwrap();
        let output = temp.path().join("lamo-production");
        let result = crate::production_dataset::export_finalized_voice_dataset(
            &db,
            &crate::production_dataset::ProductionDatasetOptions {
                output_dir: output.to_string_lossy().to_string(),
                voice_name: "Lamo".into(),
            },
        )
        .unwrap();
        assert_eq!((result.retained_segments, result.rejected_segments, result.total_duration_ms), (1, 0, 1_000));
        let master = output.join("tts/audio_24k_master/000001.wav");
        assert_eq!(std::fs::read(master).unwrap(), std::fs::read(&segment.audio_path).unwrap());
        assert_eq!(hound::WavReader::open(output.join("asr/audio_16k/000001.wav")).unwrap().spec().sample_rate, 16_000);
        assert!(output.join("manifest.json").is_file());
        assert!(output.join("SHA256SUMS").is_file());
        assert!(output.join("_COMPLETE.json").is_file());
    }
}
