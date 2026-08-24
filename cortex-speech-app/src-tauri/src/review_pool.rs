//! Flexible, voice-organized human review pool.
//!
//! The canonical `speech_segments` row still receives the first human verdict. Later reviewers write
//! append-only observations here, so an independent second or third judgement can never overwrite the
//! first answer. Queue selection is coverage-first and reviewer-specific: a person sees clips they have
//! not judged, ordered by the number of distinct effective judgements already attached to each clip.

use crate::db::Database;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

const REVIEW_POOL_BASE_SCHEMA_VERSION: i64 = 62;
pub const REVIEW_POOL_SCHEMA_VERSION: i64 = 63;
pub const REVIEW_POOL_PLAYBACK_GUARD: &str = "content-hash-raw-counter-v3";
const DESKTOP_REVIEWER_KEY: &str = "@desktop-owner";
pub const OWNER_RIGHTS_LICENSE: &str = "owner-full-rights";
pub const OWNER_RIGHTS_CONSENT: &str = "speaker-agreement-paid-unrestricted-public";
pub const OWNER_RIGHTS_PERMITTED_USE: &str = "unrestricted: train, evaluate, publish, redistribute, commercial";
pub const OWNER_RIGHTS_ATTRIBUTION: &str = "Hawzhin (owner) — speakers paid and agreed to full public use";
pub const OWNER_RIGHTS_SOURCE: &str = "owner-supplied recording";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPool {
    pub pool_id: String,
    pub focus_segment_count: usize,
    pub focus_sha256: String,
    pub champion_model_version_id: String,
    pub champion_deployment_sha256: String,
    members: Arc<HashMap<String, PoolMemberEvidence>>,
    member_ids: Arc<HashSet<String>>,
    audio_paths: Arc<HashMap<String, String>>,
    playable_member_ids: Arc<HashSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolMemberEvidence {
    voice_name: String,
    raw_transcript: String,
    model_version_id: String,
    audio_content_hash: String,
    source_start_ms: i64,
    source_end_ms: i64,
    duration_ms: i64,
}

type PoolSourceRow = (String, String, Option<String>, Option<i64>, Option<i64>, i64);

impl ReviewPool {
    pub fn contains(&self, segment_id: &str) -> bool {
        self.members.contains_key(segment_id)
    }

    pub fn voice_for(&self, segment_id: &str) -> Option<&str> {
        self.members.get(segment_id).map(|member| member.voice_name.as_str())
    }

    pub fn segment_ids(&self) -> Arc<HashSet<String>> {
        self.member_ids.clone()
    }

    /// Recheck only a clip that is actually about to be leased. Queue construction uses the
    /// startup-verified availability set so a 20k-clip pool never performs 20k filesystem calls per
    /// request; this last-mile check ensures a file removed after startup still pauses safely before
    /// the reviewer sees or judges it.
    pub fn verify_audio_available(&self, segment_id: &str) -> Result<(), String> {
        let path = self
            .audio_paths
            .get(segment_id)
            .ok_or_else(|| format!("review pool clip {segment_id} has no bound audio path"))?;
        if !Path::new(path).is_file() {
            return Err(format!("review pool clip {segment_id} audio is missing: {path}"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolMemberInput {
    pub segment_id: String,
    pub voice_name: String,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCoverage {
    pub voice_name: String,
    pub total_clips: usize,
    pub zero_reviews: usize,
    pub one_review: usize,
    pub two_reviews: usize,
    pub three_or_more_reviews: usize,
    pub resolved: usize,
    pub needs_third_review: usize,
    pub owner_conflicts: usize,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SegmentResolution {
    pub segment_id: String,
    pub voice_name: String,
    pub status: String,
    pub final_action: Option<String>,
    pub final_transcript: Option<String>,
    pub evidence_sha256: String,
    pub reviewer_count: usize,
    pub agreeing_reviewers: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PoolResolutionSummary {
    pub total_clips: usize,
    pub resolved_clips: usize,
    pub needs_first_or_second_review: usize,
    pub needs_third_review: usize,
    pub owner_conflicts: usize,
}

#[derive(Debug, Clone)]
pub struct OwnerAdjudicationInput<'a> {
    pub segment_id: &'a str,
    pub final_action: &'a str,
    pub final_transcript: Option<&'a str>,
    pub operation_id: &'a str,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RightsStampReport {
    pub recordings: usize,
    pub segments: usize,
    pub stamped_recordings: usize,
    pub already_exact_recordings: usize,
    pub rights_sha256: String,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RightsCoverageReport {
    pub recordings: usize,
    pub segment_rows: usize,
    pub exact_rows: usize,
    pub unstamped_rows: usize,
    pub conflicting_rows: usize,
    pub revoked_rows: usize,
    pub all_exact: bool,
    pub rights_sha256: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VoiceAuthorityDigests {
    pub voice_name: String,
    pub segment_count: usize,
    pub resolution_sha256: String,
    pub reviewer_sha256: String,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCertificateRecord {
    pub id: i64,
    pub pool_id: String,
    pub voice_name: String,
    pub resolution_sha256: String,
    pub rights_sha256: String,
    pub audio_sha256: String,
    pub reviewer_sha256: String,
    pub export_manifest_sha256: String,
    pub export_sha256sums_sha256: String,
    pub certificate_json: String,
    pub certificate_sha256: String,
    pub retained_segments: usize,
    pub rejected_segments: usize,
    pub total_duration_ms: i64,
    pub app_git_sha: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct VoiceCertificateInput<'a> {
    pub voice_name: &'a str,
    pub resolution_sha256: &'a str,
    pub rights_sha256: &'a str,
    pub audio_sha256: &'a str,
    pub reviewer_sha256: &'a str,
    pub export_manifest_sha256: &'a str,
    pub export_sha256sums_sha256: &'a str,
    pub certificate_json: &'a str,
    pub certificate_sha256: &'a str,
    pub retained_segments: usize,
    pub rejected_segments: usize,
    pub total_duration_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct PoolDecisionInput<'a> {
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
pub struct PoolOperationReceipt {
    pub decision_id: i64,
    pub pool_id: String,
    pub segment_id: String,
    pub reviewer: String,
    pub operation_payload_hash: String,
}

fn reviewer_key(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| DESKTOP_REVIEWER_KEY.to_string())
}

fn valid_lower_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_uuid(value: &str, label: &str) -> Result<(), String> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| format!("{label} must be a canonical UUID"))?;
    if parsed.hyphenated().to_string() != value {
        return Err(format!("{label} must be a lowercase hyphenated UUID"));
    }
    Ok(())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ReviewOutcome {
    Retain(String),
    Reject,
}

impl ReviewOutcome {
    fn final_action(&self) -> &'static str {
        match self {
            Self::Retain(_) => "retain",
            Self::Reject => "reject",
        }
    }

    fn final_transcript(&self) -> Option<&str> {
        match self {
            Self::Retain(text) => Some(text),
            Self::Reject => None,
        }
    }

    fn digest_value(&self) -> String {
        match self {
            Self::Retain(text) => format!("retain:{text}"),
            Self::Reject => "reject".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct JudgementEvidence {
    reviewer: String,
    evidence_id: String,
    outcome: ReviewOutcome,
}

#[derive(Debug, Clone)]
struct OwnerAdjudication {
    final_outcome: ReviewOutcome,
    evidence_sha256: String,
}

#[derive(Debug, Clone)]
enum DerivedResolution {
    Pending,
    NeedsThird,
    OwnerConflict,
    Resolved { outcome: ReviewOutcome, agreeing_reviewers: Vec<String>, owner: bool },
}

fn canonical_verbatim_text(value: &str) -> String {
    crate::db::to_nfc(value).trim().to_string()
}

fn outcome_from_action(action: &str, transcript: Option<&str>) -> Result<Option<ReviewOutcome>, String> {
    match action {
        "accept" | "edit" | "human_accept" | "human_edit" => {
            let text = transcript
                .map(canonical_verbatim_text)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| "retained review evidence has no non-blank verbatim transcript".to_string())?;
            Ok(Some(ReviewOutcome::Retain(text)))
        }
        "reject" | "human_reject" => Ok(Some(ReviewOutcome::Reject)),
        "skip" => Ok(None),
        other => Err(format!("unknown review-pool evidence action {other}")),
    }
}

fn evidence_sha256(segment_id: &str, judgements: &HashMap<String, JudgementEvidence>) -> String {
    let mut ordered: Vec<_> = judgements.values().collect();
    ordered.sort_unstable_by(|left, right| left.reviewer.cmp(&right.reviewer));
    let mut digest = Sha256::new();
    hash_field(&mut digest, segment_id.as_bytes());
    for evidence in ordered {
        hash_field(&mut digest, evidence.reviewer.as_bytes());
        hash_field(&mut digest, evidence.evidence_id.as_bytes());
        hash_field(&mut digest, evidence.outcome.digest_value().as_bytes());
    }
    digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

fn member_evidence(members: &HashMap<String, PoolMemberEvidence>) -> Result<(usize, String), String> {
    if members.is_empty() {
        return Err("review pool must contain at least one clip".to_string());
    }
    let mut rows: Vec<(&str, &PoolMemberEvidence)> =
        members.iter().map(|(id, evidence)| (id.as_str(), evidence)).collect();
    rows.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let mut hasher = Sha256::new();
    for (id, evidence) in &rows {
        hash_field(&mut hasher, id.as_bytes());
        hash_field(&mut hasher, evidence.voice_name.as_bytes());
        hash_field(&mut hasher, evidence.raw_transcript.as_bytes());
        hash_field(&mut hasher, evidence.model_version_id.as_bytes());
        hash_field(&mut hasher, evidence.audio_content_hash.as_bytes());
        hasher.update(evidence.source_start_ms.to_be_bytes());
        hasher.update(evidence.source_end_ms.to_be_bytes());
        hasher.update(evidence.duration_ms.to_be_bytes());
    }
    let digest: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    Ok((rows.len(), digest))
}

fn current_champion_7b_identity(db: &Database) -> Result<crate::registry::DeploymentIdentity, String> {
    let identity = crate::registry::champion_identity(db, crate::deployment::OMNIASR_7B_FAMILY)
        .map_err(|error| format!("OmniASR-7B champion registry cannot be read: {error}"))?
        .ok_or_else(|| "OmniASR-7B champion registry has no active champion".to_string())?;
    if identity.model_version_id.trim().is_empty() || !valid_lower_sha256(&identity.deployment_sha256) {
        return Err("OmniASR-7B champion registry identity is invalid".to_string());
    }
    Ok(identity)
}

pub fn current_champion_7b_model_id(db: &Database) -> Result<String, String> {
    Ok(current_champion_7b_identity(db)?.model_version_id)
}

fn with_pool_full_sync<T>(db: &Database, operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    db.connection()
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|error| format!("review pool cannot enable full durability: {error}"))?;
    let result = operation();
    let reset = db.connection().execute_batch("PRAGMA synchronous=NORMAL;");
    match result {
        Ok(value) => {
            reset.map_err(|error| format!("review pool committed but normal sync could not be restored: {error}"))?;
            Ok(value)
        }
        Err(error) => {
            if let Err(reset_error) = reset {
                tracing::warn!("failed to restore SQLite synchronous=NORMAL after review-pool error: {reset_error}");
            }
            Err(error)
        }
    }
}

pub fn load(db: &Database) -> Result<Option<ReviewPool>, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_BASE_SCHEMA_VERSION
    {
        return Ok(None);
    }
    let registry: Option<(String, i64, String, String, String)> = db
        .connection()
        .query_row(
            "SELECT pool_id, focus_segment_count, focus_sha256,
                    champion_model_version_id, champion_deployment_sha256
               FROM review_pool_registry WHERE singleton_key = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .map_err(|error| format!("review pool registry cannot be read: {error}"))?;
    let Some((pool_id, expected_count, expected_sha256, champion_model_version_id, champion_deployment_sha256)) =
        registry
    else {
        let orphan_rows: i64 = db
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM review_pool_members)
                      + (SELECT COUNT(*) FROM review_pool_decisions)
                      + (SELECT COUNT(*) FROM review_pool_reversals)",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("review pool authority cannot be counted: {error}"))?;
        if orphan_rows != 0 {
            return Err("review pool authority exists without its immutable registry".to_string());
        }
        return Ok(None);
    };
    let mut statement = db
        .connection()
        .prepare(
            "SELECT member.segment_id, member.voice_name, member.raw_transcript, member.model_version_id,
                    member.audio_content_hash, member.source_start_ms, member.source_end_ms,
                    member.duration_ms, segment.audio_path
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE member.pool_id=?1 ORDER BY member.segment_id",
        )
        .map_err(|error| format!("review pool members cannot be read: {error}"))?;
    let rows = statement
        .query_map([&pool_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PoolMemberEvidence {
                    voice_name: row.get(1)?,
                    raw_transcript: row.get(2)?,
                    model_version_id: row.get(3)?,
                    audio_content_hash: row.get(4)?,
                    source_start_ms: row.get(5)?,
                    source_end_ms: row.get(6)?,
                    duration_ms: row.get(7)?,
                },
                row.get::<_, String>(8)?,
            ))
        })
        .map_err(|error| format!("review pool members cannot be read: {error}"))?;
    let mut members = HashMap::new();
    let mut audio_paths = HashMap::new();
    let mut playable_member_ids = HashSet::new();
    for row in rows {
        let (segment_id, evidence, audio_path) =
            row.map_err(|error| format!("review pool member is unreadable: {error}"))?;
        if Path::new(&audio_path).is_file() {
            playable_member_ids.insert(segment_id.clone());
        }
        audio_paths.insert(segment_id.clone(), audio_path);
        members.insert(segment_id, evidence);
    }
    let (actual_count, actual_sha256) = member_evidence(&members)?;
    if i64::try_from(actual_count).ok() != Some(expected_count) || actual_sha256 != expected_sha256 {
        return Err("review pool membership does not match its immutable registry digest".to_string());
    }
    let current_champion = current_champion_7b_identity(db)?;
    if current_champion.model_version_id != champion_model_version_id
        || current_champion.deployment_sha256 != champion_deployment_sha256
    {
        return Err("review pool champion identity no longer matches the active OmniASR-7B champion".to_string());
    }
    if members.values().any(|member| member.model_version_id != champion_model_version_id) {
        return Err("review pool contains a draft from outside its frozen champion identity".to_string());
    }
    let member_ids = Arc::new(members.keys().cloned().collect());
    let pool = ReviewPool {
        pool_id,
        focus_segment_count: actual_count,
        focus_sha256: actual_sha256,
        champion_model_version_id,
        champion_deployment_sha256,
        members: Arc::new(members),
        member_ids,
        audio_paths: Arc::new(audio_paths),
        playable_member_ids: Arc::new(playable_member_ids),
    };
    require_live_member_identity(db, &pool)?;
    Ok(Some(pool))
}

/// Cheap request-boundary validation for a pool that was fully digest-verified at Start.
/// The registry and member rows are immutable under schema 62, so checking the bound registry
/// identity and member count avoids re-reading and re-hashing tens of thousands of rows on every
/// queue fetch and decision without weakening the fail-closed session binding.
pub fn registry_matches(db: &Database, bound: &ReviewPool) -> Result<bool, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_BASE_SCHEMA_VERSION
    {
        return Ok(false);
    }
    let current_champion = current_champion_7b_identity(db)?;
    let current: Option<(String, i64, String, String, String, i64)> = db
        .connection()
        .query_row(
            "SELECT registry.pool_id,
                    registry.focus_segment_count,
                    registry.focus_sha256,
                    registry.champion_model_version_id,
                    registry.champion_deployment_sha256,
                    COUNT(member.segment_id)
               FROM review_pool_registry registry
               LEFT JOIN review_pool_members member ON member.pool_id=registry.pool_id
              WHERE registry.singleton_key=1
              GROUP BY registry.pool_id, registry.focus_segment_count, registry.focus_sha256,
                       registry.champion_model_version_id, registry.champion_deployment_sha256",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()
        .map_err(|error| format!("review pool registry cannot be revalidated: {error}"))?;
    Ok(current.is_some_and(|(pool_id, count, sha256, model_id, deployment_sha256, member_count)| {
        pool_id == bound.pool_id
            && usize::try_from(count).ok() == Some(bound.focus_segment_count)
            && sha256 == bound.focus_sha256
            && model_id == bound.champion_model_version_id
            && deployment_sha256 == bound.champion_deployment_sha256
            && current_champion.model_version_id == bound.champion_model_version_id
            && current_champion.deployment_sha256 == bound.champion_deployment_sha256
            && member_count == count
    }))
}

/// Create the one immutable pool generation. Repeating the exact request is an idempotent success;
/// a different request is refused so a live pool can never silently change beneath reviewers.
pub fn activate(db: &Database, pool_id: &str, inputs: &[PoolMemberInput]) -> Result<ReviewPool, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_BASE_SCHEMA_VERSION
    {
        return Err("flexible review pool requires schema 62 or newer".to_string());
    }
    canonical_uuid(pool_id, "review pool id")?;
    let champion = current_champion_7b_identity(db)?;
    let champion_model_id = &champion.model_version_id;
    let mut assignments = HashMap::new();
    for input in inputs {
        let segment_id = input.segment_id.trim();
        let voice_name = input.voice_name.trim();
        if segment_id.is_empty() || voice_name.is_empty() || voice_name.len() > 80 {
            return Err("review pool member has an invalid segment id or voice name".to_string());
        }
        match assignments.insert(segment_id.to_string(), voice_name.to_string()) {
            Some(existing) if existing != voice_name => {
                return Err(format!("segment {segment_id} is assigned to two voice characters"))
            }
            _ => {}
        }
    }
    let mut members = HashMap::new();
    for (segment_id, voice_name) in &assignments {
        let segment: Option<PoolSourceRow> = db
            .connection()
            .query_row(
                "SELECT raw_transcript, COALESCE(model_version_id, ''), audio_content_hash,
                        json_extract(alignment_json, '$.source_start_ms'),
                        json_extract(alignment_json, '$.source_end_ms'), duration_ms
                   FROM speech_segments WHERE id=?1",
                [segment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()
            .map_err(|error| format!("review pool segment {segment_id} cannot be checked: {error}"))?;
        let Some((raw_transcript, model_id, audio_hash, source_start_ms, source_end_ms, duration_ms)) = segment else {
            return Err(format!("review pool segment {segment_id} does not exist"));
        };
        let draft = raw_transcript.trim();
        if draft.is_empty() || (draft.starts_with('[') && draft.ends_with(']')) {
            return Err(format!("review pool segment {segment_id} has no usable champion transcript"));
        }
        if model_id != *champion_model_id {
            return Err(format!(
                "review pool segment {segment_id} is not backed by the current OmniASR-7B champion ({model_id})"
            ));
        }
        let audio_content_hash = audio_hash
            .filter(|hash| valid_lower_sha256(hash))
            .ok_or_else(|| format!("review pool segment {segment_id} has no canonical audio-content hash"))?;
        let (Some(source_start_ms), Some(source_end_ms)) = (source_start_ms, source_end_ms) else {
            return Err(format!("review pool segment {segment_id} has no canonical source span"));
        };
        if source_start_ms < 0 || source_end_ms <= source_start_ms || duration_ms <= 0 {
            return Err(format!("review pool segment {segment_id} has invalid audio timing evidence"));
        }
        members.insert(
            segment_id.clone(),
            PoolMemberEvidence {
                voice_name: voice_name.clone(),
                raw_transcript,
                model_version_id: model_id,
                audio_content_hash,
                source_start_ms,
                source_end_ms,
                duration_ms,
            },
        );
    }
    let mut audio_windows: HashMap<(String, i64, i64), String> = HashMap::new();
    for (segment_id, evidence) in &members {
        let identity = (evidence.audio_content_hash.clone(), evidence.source_start_ms, evidence.source_end_ms);
        if let Some(existing) = audio_windows.insert(identity, segment_id.clone()) {
            return Err(format!(
                "review pool segments {existing} and {segment_id} are the same canonical audio window"
            ));
        }
    }
    let (focus_segment_count, focus_sha256) = member_evidence(&members)?;
    if let Some(existing) = load(db)? {
        if existing.pool_id == pool_id
            && existing.focus_segment_count == focus_segment_count
            && existing.focus_sha256 == focus_sha256
        {
            return Ok(existing);
        }
        return Err("a different immutable review pool is already active".to_string());
    }

    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("review pool activation cannot lock the database: {error}"))?;
        for (segment_id, evidence) in &members {
            let unchanged: bool = tx
                .query_row(
                    "SELECT EXISTS(
                    SELECT 1 FROM speech_segments
                     WHERE id=?1 AND raw_transcript=?2 AND COALESCE(model_version_id, '')=?3
                       AND audio_content_hash=?4
                       AND json_extract(alignment_json, '$.source_start_ms')=?5
                       AND json_extract(alignment_json, '$.source_end_ms')=?6
                       AND duration_ms=?7
                )",
                    rusqlite::params![
                        segment_id,
                        evidence.raw_transcript,
                        evidence.model_version_id,
                        evidence.audio_content_hash,
                        evidence.source_start_ms,
                        evidence.source_end_ms,
                        evidence.duration_ms,
                    ],
                    |row| row.get(0),
                )
                .map_err(|error| format!("review pool segment {segment_id} cannot be checked: {error}"))?;
            if !unchanged {
                return Err(format!("review pool segment {segment_id} changed during activation"));
            }
        }
        tx.execute(
            "INSERT INTO review_pool_registry
             (singleton_key, pool_id, focus_segment_count, focus_sha256,
              champion_model_version_id, champion_deployment_sha256, app_git_sha)
         VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                pool_id,
                focus_segment_count as i64,
                focus_sha256,
                champion.model_version_id,
                champion.deployment_sha256,
                crate::GIT_SHA
            ],
        )
        .map_err(|error| format!("review pool registry cannot be committed: {error}"))?;
        {
            let mut statement = tx
                .prepare(
                    "INSERT INTO review_pool_members
                    (pool_id, segment_id, voice_name, raw_transcript, model_version_id,
                     audio_content_hash, source_start_ms, source_end_ms, duration_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(|error| format!("review pool member writer cannot be prepared: {error}"))?;
            let mut ordered: Vec<_> = members.iter().collect();
            ordered.sort_unstable_by(|left, right| left.0.cmp(right.0));
            for (segment_id, evidence) in ordered {
                statement
                    .execute(rusqlite::params![
                        pool_id,
                        segment_id,
                        evidence.voice_name,
                        evidence.raw_transcript,
                        evidence.model_version_id,
                        evidence.audio_content_hash,
                        evidence.source_start_ms,
                        evidence.source_end_ms,
                        evidence.duration_ms,
                    ])
                    .map_err(|error| format!("review pool member {segment_id} cannot be committed: {error}"))?;
            }
        }
        tx.commit().map_err(|error| format!("review pool activation cannot commit: {error}"))?;
        Ok(())
    })?;
    load(db)?.ok_or_else(|| "review pool disappeared after activation".to_string())
}

#[derive(Default)]
struct SegmentReviewers {
    judged: HashMap<String, JudgementEvidence>,
    seen: HashSet<String>,
}

fn insert_judgement(
    result: &mut HashMap<String, SegmentReviewers>,
    segment_id: String,
    reviewer: String,
    evidence_id: String,
    action: String,
    transcript: Option<String>,
) -> Result<(), String> {
    let key = reviewer_key(Some(&reviewer));
    let entry = result.entry(segment_id.clone()).or_default();
    entry.seen.insert(key.clone());
    let Some(outcome) = outcome_from_action(&action, transcript.as_deref())? else {
        return Ok(());
    };
    let evidence = JudgementEvidence { reviewer: reviewer.trim().to_string(), evidence_id, outcome };
    if entry.judged.insert(key, evidence).is_some() {
        return Err(format!("review pool segment {segment_id} has duplicate effective evidence from one reviewer"));
    }
    Ok(())
}

fn reviewer_sets_on(conn: &rusqlite::Connection) -> Result<HashMap<String, SegmentReviewers>, String> {
    let mut result: HashMap<String, SegmentReviewers> = HashMap::new();
    let mut canonical = conn
        .prepare(
            "SELECT member.segment_id,
                    COALESCE(segment.reviewed_by, '@desktop-owner'),
                    segment.human_decision,
                    CASE WHEN segment.human_decision IN ('accept','edit','human_accept','human_edit')
                         THEN COALESCE(NULLIF(TRIM(segment.verdict_transcript), ''),
                                       NULLIF(TRIM(segment.annotated_transcript), ''),
                                       segment.raw_transcript)
                         ELSE NULL END,
                    segment.review_revision
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE segment.verified=1
                AND segment.human_decision IN ('accept','edit','reject','human_accept','human_edit','human_reject')",
        )
        .map_err(|error| format!("canonical review coverage cannot be read: {error}"))?;
    let rows = canonical
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|error| format!("canonical review coverage cannot be read: {error}"))?;
    for row in rows {
        let (segment_id, reviewer, action, transcript, revision) =
            row.map_err(|error| format!("canonical review coverage is unreadable: {error}"))?;
        insert_judgement(&mut result, segment_id, reviewer, format!("canonical:{revision}"), action, transcript)?;
    }

    let mut independent = conn
        .prepare(
            "SELECT decision.segment_id, decision.reviewer, decision.id,
                    decision.action, decision.submitted_transcript
               FROM effective_review_pool_decisions_v62 decision",
        )
        .map_err(|error| format!("independent pool coverage cannot be read: {error}"))?;
    let rows = independent
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|error| format!("independent pool coverage cannot be read: {error}"))?;
    for row in rows {
        let (segment_id, reviewer, id, action, transcript) =
            row.map_err(|error| format!("independent pool coverage is unreadable: {error}"))?;
        insert_judgement(&mut result, segment_id, reviewer, format!("pool:{id}"), action, transcript)?;
    }

    // Preserve any already-committed v61 blinded judgements if the old sequential campaign was used
    // before this pool superseded its serving policy.
    let mut legacy = conn
        .prepare(
            "SELECT decision.segment_id, decision.reviewer, decision.id,
                    decision.action, decision.submitted_transcript
               FROM effective_independent_review_decisions_v61 decision
               JOIN review_pool_members member ON member.segment_id=decision.segment_id",
        )
        .map_err(|error| format!("legacy independent coverage cannot be read: {error}"))?;
    let rows = legacy
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|error| format!("legacy independent coverage cannot be read: {error}"))?;
    for row in rows {
        let (segment_id, reviewer, id, action, transcript) =
            row.map_err(|error| format!("legacy independent coverage is unreadable: {error}"))?;
        insert_judgement(&mut result, segment_id, reviewer, format!("legacy:{id}"), action, transcript)?;
    }
    Ok(result)
}

fn reviewer_sets(db: &Database) -> Result<HashMap<String, SegmentReviewers>, String> {
    reviewer_sets_on(db.connection())
}

fn owner_adjudications_on(conn: &rusqlite::Connection) -> Result<HashMap<String, Vec<OwnerAdjudication>>, String> {
    let schema_version: i64 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |row| row.get(0))
        .map_err(|error| format!("review-pool schema authority cannot be read: {error}"))?;
    if schema_version < REVIEW_POOL_SCHEMA_VERSION {
        return Ok(HashMap::new());
    }
    let mut statement = conn
        .prepare(
            "SELECT segment_id, final_action, final_transcript, evidence_sha256
               FROM review_pool_owner_adjudications ORDER BY id DESC",
        )
        .map_err(|error| format!("owner adjudications cannot be read: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| format!("owner adjudications cannot be read: {error}"))?;
    let mut result: HashMap<String, Vec<OwnerAdjudication>> = HashMap::new();
    for row in rows {
        let (segment_id, action, transcript, digest) =
            row.map_err(|error| format!("owner adjudication is unreadable: {error}"))?;
        let final_outcome = match action.as_str() {
            "retain" => ReviewOutcome::Retain(
                transcript
                    .map(|value| canonical_verbatim_text(&value))
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| format!("owner adjudication for {segment_id} has no retained transcript"))?,
            ),
            "reject" if transcript.is_none() => ReviewOutcome::Reject,
            _ => return Err(format!("owner adjudication for {segment_id} has invalid outcome evidence")),
        };
        result.entry(segment_id).or_default().push(OwnerAdjudication { final_outcome, evidence_sha256: digest });
    }
    Ok(result)
}

fn derive_resolution(
    segment_id: &str,
    reviewers: Option<&SegmentReviewers>,
    adjudications: Option<&Vec<OwnerAdjudication>>,
) -> (DerivedResolution, String) {
    let empty = HashMap::new();
    let judgements = reviewers.map(|value| &value.judged).unwrap_or(&empty);
    let digest = evidence_sha256(segment_id, judgements);
    if let Some(adjudication) = adjudications.and_then(|rows| rows.iter().find(|row| row.evidence_sha256 == digest)) {
        return (
            DerivedResolution::Resolved {
                outcome: adjudication.final_outcome.clone(),
                agreeing_reviewers: Vec::new(),
                owner: true,
            },
            digest,
        );
    }
    let mut outcomes: HashMap<ReviewOutcome, Vec<String>> = HashMap::new();
    for evidence in judgements.values() {
        outcomes.entry(evidence.outcome.clone()).or_default().push(evidence.reviewer.clone());
    }
    let mut matching: Vec<(ReviewOutcome, Vec<String>)> = outcomes
        .into_iter()
        .filter_map(|(outcome, mut names)| {
            names.sort_unstable_by_key(|name| name.to_ascii_lowercase());
            (names.len() >= 2).then_some((outcome, names))
        })
        .collect();
    matching.sort_unstable_by_key(|entry| entry.0.digest_value());
    if let Some((outcome, agreeing_reviewers)) = matching.into_iter().next() {
        return (DerivedResolution::Resolved { outcome, agreeing_reviewers, owner: false }, digest);
    }
    let resolution = match judgements.len() {
        0 | 1 => DerivedResolution::Pending,
        2 => DerivedResolution::NeedsThird,
        _ => DerivedResolution::OwnerConflict,
    };
    (resolution, digest)
}

fn require_live_member_identity(db: &Database, pool: &ReviewPool) -> Result<(), String> {
    let drifted: Option<String> = db
        .connection()
        .query_row(
            "SELECT member.segment_id
               FROM review_pool_members member
               LEFT JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE member.pool_id=?1 AND (
                    segment.id IS NULL
                 OR segment.raw_transcript IS NOT member.raw_transcript
                 OR COALESCE(segment.model_version_id, '') IS NOT member.model_version_id
                 OR segment.audio_content_hash IS NOT member.audio_content_hash
                 OR json_extract(segment.alignment_json, '$.source_start_ms') IS NOT member.source_start_ms
                 OR json_extract(segment.alignment_json, '$.source_end_ms') IS NOT member.source_end_ms
                 OR segment.duration_ms IS NOT member.duration_ms
              )
              LIMIT 1",
            [&pool.pool_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("review pool member identity cannot be verified: {error}"))?;
    if let Some(segment_id) = drifted {
        return Err(format!(
            "review pool clip {segment_id} changed after activation; review is paused to protect repeated-review identity"
        ));
    }
    let invalid_history: bool = db
        .connection()
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM review_pool_decisions decision
                 JOIN review_pool_members member
                   ON member.pool_id=decision.pool_id AND member.segment_id=decision.segment_id
                 JOIN speech_segments segment ON segment.id=decision.segment_id
                WHERE decision.pool_id=?1 AND (
                      decision.served_transcript <> trim(member.raw_transcript)
                   OR decision.duration_ms <> member.duration_ms
                   OR (decision.action IN ('accept','edit')
                       AND (decision.submitted_transcript IS NULL OR trim(decision.submitted_transcript)=''))
                   OR (decision.action IN ('reject','skip') AND decision.submitted_transcript IS NOT NULL)
                   OR (decision.action='skip' AND (
                         decision.audio_content_hash IS NOT NULL
                      OR decision.source_start_ms IS NOT NULL
                      OR decision.source_end_ms IS NOT NULL
                   ))
                   OR (decision.action<>'skip' AND (
                         decision.audio_content_hash IS NOT member.audio_content_hash
                      OR decision.source_start_ms IS NOT member.source_start_ms
                      OR decision.source_end_ms IS NOT member.source_end_ms
                   ))
                   OR EXISTS (SELECT 1 FROM review_events event
                               WHERE event.operation_id=decision.operation_id)
                   OR EXISTS (SELECT 1 FROM independent_review_decisions legacy
                               WHERE legacy.operation_id=decision.operation_id)
                )
             ) OR EXISTS(
                 SELECT 1 FROM effective_review_pool_decisions_v62 decision
                 JOIN speech_segments segment ON segment.id=decision.segment_id
                WHERE decision.pool_id=?1 AND (
                      segment.verified<>1
                   OR segment.human_decision NOT IN ('accept','edit','reject')
                   OR lower(trim(COALESCE(segment.reviewed_by, '@desktop-owner')))
                      = lower(trim(decision.reviewer))
                   OR EXISTS (
                        SELECT 1 FROM effective_independent_review_decisions_v61 legacy
                         WHERE legacy.segment_id=decision.segment_id
                           AND legacy.reviewer=decision.reviewer COLLATE NOCASE
                   )
                )
             ) OR EXISTS(
                 SELECT 1 FROM effective_review_pool_decisions_v62 decision
                WHERE decision.pool_id=?1
                GROUP BY decision.segment_id, lower(trim(decision.reviewer))
               HAVING COUNT(*)<>1
             )",
            [&pool.pool_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("review pool decision history cannot be verified: {error}"))?;
    if invalid_history {
        return Err("review pool decision history is inconsistent with its frozen clip authority".to_string());
    }
    Ok(())
}

pub fn pending_segment_ids(
    db: &Database,
    pool: &ReviewPool,
    reviewer: &str,
    allowed_dialects: Option<&[String]>,
) -> Result<Vec<String>, String> {
    // `load` performs the full O(pool) identity/history proof once at Start. Schema v62 then makes
    // membership and every clip identity field immutable, while each decision insert re-proves its
    // exact member/audio/reviewer boundary transactionally. Repeating the full 20k-row proof on every
    // queue fetch added ~650 ms without checking any state that a valid writer can change. Keep the
    // cheap request-boundary registry/champion/count proof here; `reviewer_sets` below still validates
    // every effective mutable review outcome and rejects duplicate reviewer evidence.
    if !registry_matches(db, pool)? {
        return Err("review pool registry or OmniASR-7B champion identity changed after Start".to_string());
    }
    let reviewers = reviewer_sets(db)?;
    let adjudications = owner_adjudications_on(db.connection())?;
    let reviewer = reviewer_key(Some(reviewer));
    let mut statement = db
        .connection()
        .prepare(
            "SELECT segment.id, segment.audio_path, COALESCE(segment.created_at, '')
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE member.pool_id=?1
                AND TRIM(COALESCE(segment.raw_transcript, '')) <> ''
                AND NOT (TRIM(segment.raw_transcript) LIKE '[%]')",
        )
        .map_err(|error| format!("review pool queue cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([&pool.pool_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(|error| format!("review pool queue cannot be read: {error}"))?;
    let mut pending: Vec<(usize, String, String)> = Vec::new();
    for row in rows {
        let (segment_id, audio_path, created_at) =
            row.map_err(|error| format!("review pool row is unreadable: {error}"))?;
        let coverage = reviewers.get(&segment_id);
        if coverage.is_some_and(|coverage| coverage.seen.contains(&reviewer)) {
            continue;
        }
        let (resolution, _) = derive_resolution(&segment_id, coverage, adjudications.get(&segment_id));
        if matches!(resolution, DerivedResolution::Resolved { .. } | DerivedResolution::OwnerConflict) {
            continue;
        }
        if !pool.playable_member_ids.contains(&segment_id)
            || !crate::dialect::reviewer_may_judge(allowed_dialects, &audio_path)
        {
            continue;
        }
        pending.push((coverage.map_or(0, |coverage| coverage.judged.len()), created_at, segment_id));
    }
    pending.sort_unstable();
    Ok(pending.into_iter().map(|(_, _, segment_id)| segment_id).collect())
}

pub fn coverage_by_voice(db: &Database) -> Result<Vec<VoiceCoverage>, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let reviewers = reviewer_sets(db)?;
    let adjudications = owner_adjudications_on(db.connection())?;
    let mut by_voice: HashMap<String, VoiceCoverage> = HashMap::new();
    for (segment_id, evidence) in pool.members.iter() {
        let reviews = reviewers.get(segment_id).map_or(0, |value| value.judged.len());
        let entry = by_voice.entry(evidence.voice_name.clone()).or_insert_with(|| VoiceCoverage {
            voice_name: evidence.voice_name.clone(),
            total_clips: 0,
            zero_reviews: 0,
            one_review: 0,
            two_reviews: 0,
            three_or_more_reviews: 0,
            resolved: 0,
            needs_third_review: 0,
            owner_conflicts: 0,
        });
        entry.total_clips += 1;
        match reviews {
            0 => entry.zero_reviews += 1,
            1 => entry.one_review += 1,
            2 => entry.two_reviews += 1,
            _ => entry.three_or_more_reviews += 1,
        }
        let (resolution, _) = derive_resolution(segment_id, reviewers.get(segment_id), adjudications.get(segment_id));
        match resolution {
            DerivedResolution::Resolved { .. } => entry.resolved += 1,
            DerivedResolution::NeedsThird => entry.needs_third_review += 1,
            DerivedResolution::OwnerConflict => entry.owner_conflicts += 1,
            DerivedResolution::Pending => {}
        }
    }
    let mut rows: Vec<VoiceCoverage> = by_voice.into_values().collect();
    rows.sort_unstable_by(|left, right| left.voice_name.cmp(&right.voice_name));
    Ok(rows)
}

pub fn segment_resolutions(db: &Database, voice_name: Option<&str>) -> Result<Vec<SegmentResolution>, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let reviewers = reviewer_sets(db)?;
    let adjudications = owner_adjudications_on(db.connection())?;
    let requested_voice = voice_name.map(str::trim).filter(|value| !value.is_empty());
    let mut rows = Vec::new();
    for (segment_id, member) in pool.members.iter() {
        if requested_voice.is_some_and(|voice| voice != member.voice_name) {
            continue;
        }
        let reviewer_count = reviewers.get(segment_id).map_or(0, |value| value.judged.len());
        let (resolution, evidence_sha256) =
            derive_resolution(segment_id, reviewers.get(segment_id), adjudications.get(segment_id));
        let (status, final_action, final_transcript, agreeing_reviewers) = match resolution {
            DerivedResolution::Pending => ("pending", None, None, Vec::new()),
            DerivedResolution::NeedsThird => ("needsThirdReview", None, None, Vec::new()),
            DerivedResolution::OwnerConflict => ("ownerConflict", None, None, Vec::new()),
            DerivedResolution::Resolved { outcome, agreeing_reviewers, owner } => (
                if owner { "ownerResolved" } else { "resolved" },
                Some(outcome.final_action().to_string()),
                outcome.final_transcript().map(str::to_string),
                agreeing_reviewers,
            ),
        };
        rows.push(SegmentResolution {
            segment_id: segment_id.clone(),
            voice_name: member.voice_name.clone(),
            status: status.to_string(),
            final_action,
            final_transcript,
            evidence_sha256,
            reviewer_count,
            agreeing_reviewers,
        });
    }
    rows.sort_unstable_by(|left, right| {
        left.voice_name.cmp(&right.voice_name).then(left.segment_id.cmp(&right.segment_id))
    });
    Ok(rows)
}

pub fn resolution_summary(db: &Database) -> Result<PoolResolutionSummary, String> {
    let rows = segment_resolutions(db, None)?;
    let mut summary = PoolResolutionSummary {
        total_clips: rows.len(),
        resolved_clips: 0,
        needs_first_or_second_review: 0,
        needs_third_review: 0,
        owner_conflicts: 0,
    };
    for row in rows {
        match row.status.as_str() {
            "resolved" | "ownerResolved" => summary.resolved_clips += 1,
            "needsThirdReview" => summary.needs_third_review += 1,
            "ownerConflict" => summary.owner_conflicts += 1,
            _ => summary.needs_first_or_second_review += 1,
        }
    }
    Ok(summary)
}

pub fn voice_authority_digests(db: &Database, voice_name: &str) -> Result<VoiceAuthorityDigests, String> {
    let voice_name = voice_name.trim();
    if voice_name.is_empty() {
        return Err("voice name cannot be blank".to_string());
    }
    let resolutions = segment_resolutions(db, Some(voice_name))?;
    if resolutions.is_empty() {
        return Err(format!("active review pool has no voice named {voice_name}"));
    }
    let reviewers = reviewer_sets(db)?;
    let mut resolution_digest = Sha256::new();
    let mut reviewer_digest = Sha256::new();
    hash_field(&mut resolution_digest, voice_name.as_bytes());
    hash_field(&mut reviewer_digest, voice_name.as_bytes());
    for resolution in &resolutions {
        hash_field(&mut resolution_digest, resolution.segment_id.as_bytes());
        hash_field(&mut resolution_digest, resolution.status.as_bytes());
        hash_field(&mut resolution_digest, resolution.evidence_sha256.as_bytes());
        hash_field(&mut resolution_digest, resolution.final_action.as_deref().unwrap_or("").as_bytes());
        hash_field(&mut resolution_digest, resolution.final_transcript.as_deref().unwrap_or("").as_bytes());

        hash_field(&mut reviewer_digest, resolution.segment_id.as_bytes());
        let mut evidence: Vec<_> =
            reviewers.get(&resolution.segment_id).map(|value| value.judged.values().collect()).unwrap_or_else(Vec::new);
        evidence.sort_unstable_by(|left, right| {
            reviewer_key(Some(&left.reviewer))
                .cmp(&reviewer_key(Some(&right.reviewer)))
                .then(left.evidence_id.cmp(&right.evidence_id))
        });
        for judgement in evidence {
            hash_field(&mut reviewer_digest, reviewer_key(Some(&judgement.reviewer)).as_bytes());
            hash_field(&mut reviewer_digest, judgement.evidence_id.as_bytes());
            hash_field(&mut reviewer_digest, judgement.outcome.digest_value().as_bytes());
        }
    }
    Ok(VoiceAuthorityDigests {
        voice_name: voice_name.to_string(),
        segment_count: resolutions.len(),
        resolution_sha256: resolution_digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect(),
        reviewer_sha256: reviewer_digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect(),
    })
}

pub fn voice_certificate(db: &Database, voice_name: &str) -> Result<Option<VoiceCertificateRecord>, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_SCHEMA_VERSION {
        return Ok(None);
    }
    db.connection()
        .query_row(
            "SELECT id, pool_id, voice_name, resolution_sha256, rights_sha256, audio_sha256,
                    reviewer_sha256, export_manifest_sha256, export_sha256sums_sha256,
                    certificate_json, certificate_sha256, retained_segments, rejected_segments,
                    total_duration_ms, app_git_sha, created_at_ms
               FROM review_pool_voice_certificates WHERE voice_name=?1 COLLATE BINARY",
            [voice_name.trim()],
            |row| {
                let retained: i64 = row.get(11)?;
                let rejected: i64 = row.get(12)?;
                Ok(VoiceCertificateRecord {
                    id: row.get(0)?,
                    pool_id: row.get(1)?,
                    voice_name: row.get(2)?,
                    resolution_sha256: row.get(3)?,
                    rights_sha256: row.get(4)?,
                    audio_sha256: row.get(5)?,
                    reviewer_sha256: row.get(6)?,
                    export_manifest_sha256: row.get(7)?,
                    export_sha256sums_sha256: row.get(8)?,
                    certificate_json: row.get(9)?,
                    certificate_sha256: row.get(10)?,
                    retained_segments: usize::try_from(retained).unwrap_or(usize::MAX),
                    rejected_segments: usize::try_from(rejected).unwrap_or(usize::MAX),
                    total_duration_ms: row.get(13)?,
                    app_git_sha: row.get(14)?,
                    created_at_ms: row.get(15)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("review-pool voice certificate cannot be read: {error}"))
}

pub fn record_voice_certificate(
    db: &Database,
    input: &VoiceCertificateInput<'_>,
) -> Result<VoiceCertificateRecord, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let voice_name = input.voice_name.trim();
    if voice_name.is_empty() || input.total_duration_ms < 0 || input.created_at_ms <= 0 {
        return Err("voice certificate has invalid identity, duration, or timestamp".to_string());
    }
    for (label, value) in [
        ("resolution", input.resolution_sha256),
        ("rights", input.rights_sha256),
        ("audio", input.audio_sha256),
        ("reviewer", input.reviewer_sha256),
        ("export manifest", input.export_manifest_sha256),
        ("export checksums", input.export_sha256sums_sha256),
        ("certificate", input.certificate_sha256),
    ] {
        if !valid_lower_sha256(value) {
            return Err(format!("voice certificate {label} digest is invalid"));
        }
    }
    serde_json::from_str::<serde_json::Value>(input.certificate_json)
        .map_err(|error| format!("voice certificate JSON is invalid: {error}"))?;
    let actual_certificate_sha: String =
        Sha256::digest(input.certificate_json.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    if actual_certificate_sha != input.certificate_sha256 {
        return Err("voice certificate JSON does not match its digest".to_string());
    }
    let authority = voice_authority_digests(db, voice_name)?;
    if authority.resolution_sha256 != input.resolution_sha256 || authority.reviewer_sha256 != input.reviewer_sha256 {
        return Err("voice certificate does not match current review authority".to_string());
    }
    let resolutions = segment_resolutions(db, Some(voice_name))?;
    if resolutions.iter().any(|row| !matches!(row.status.as_str(), "resolved" | "ownerResolved")) {
        return Err(format!("voice {voice_name} is not fully resolved"));
    }
    let retained = resolutions.iter().filter(|row| row.final_action.as_deref() == Some("retain")).count();
    let rejected = resolutions.iter().filter(|row| row.final_action.as_deref() == Some("reject")).count();
    if retained != input.retained_segments
        || rejected != input.rejected_segments
        || retained + rejected != resolutions.len()
    {
        return Err("voice certificate counts do not match resolved review outcomes".to_string());
    }
    if let Some(existing) = voice_certificate(db, voice_name)? {
        if existing.certificate_sha256 == input.certificate_sha256 {
            return Ok(existing);
        }
        return Err(format!("voice {voice_name} already has a different immutable certificate"));
    }
    with_pool_full_sync(db, || {
        db.connection()
            .execute(
                "INSERT INTO review_pool_voice_certificates
                    (pool_id, voice_name, resolution_sha256, rights_sha256, audio_sha256,
                     reviewer_sha256, export_manifest_sha256, export_sha256sums_sha256,
                     certificate_json, certificate_sha256, retained_segments, rejected_segments,
                     total_duration_ms, app_git_sha, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    pool.pool_id,
                    voice_name,
                    input.resolution_sha256,
                    input.rights_sha256,
                    input.audio_sha256,
                    input.reviewer_sha256,
                    input.export_manifest_sha256,
                    input.export_sha256sums_sha256,
                    input.certificate_json,
                    input.certificate_sha256,
                    i64::try_from(input.retained_segments).map_err(|_| "retained count is too large".to_string())?,
                    i64::try_from(input.rejected_segments).map_err(|_| "rejected count is too large".to_string())?,
                    input.total_duration_ms,
                    crate::GIT_SHA,
                    input.created_at_ms,
                ],
            )
            .map_err(|error| format!("review-pool voice certificate cannot be committed: {error}"))?;
        voice_certificate(db, voice_name)?.ok_or_else(|| "committed voice certificate cannot be reread".to_string())
    })
}

type RightsTuple = (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>);

fn pool_source_rights_on(conn: &rusqlite::Connection) -> Result<BTreeMap<String, Vec<RightsTuple>>, String> {
    let mut statement = conn
        .prepare(
            "SELECT segment.audio_path, segment.rights_license, segment.rights_consent_basis,
                    segment.rights_permitted_use, segment.rights_attribution,
                    segment.rights_source, segment.rights_revoked_at
               FROM speech_segments segment
              WHERE EXISTS (
                    SELECT 1 FROM review_pool_members member
                    JOIN speech_segments pool_segment ON pool_segment.id=member.segment_id
                   WHERE pool_segment.audio_path=segment.audio_path
              )
              ORDER BY segment.audio_path, segment.id",
        )
        .map_err(|error| format!("review-pool recording rights cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ),
            ))
        })
        .map_err(|error| format!("review-pool recording rights cannot be read: {error}"))?;
    let mut result: BTreeMap<String, Vec<RightsTuple>> = BTreeMap::new();
    for row in rows {
        let (path, rights) = row.map_err(|error| format!("review-pool recording rights are unreadable: {error}"))?;
        result.entry(path).or_default().push(rights);
    }
    Ok(result)
}

fn blank(value: &Option<String>) -> bool {
    value.as_deref().map_or(true, |text| text.trim().is_empty())
}

fn exact_owner_rights(rights: &RightsTuple) -> bool {
    rights.0.as_deref() == Some(OWNER_RIGHTS_LICENSE)
        && rights.1.as_deref() == Some(OWNER_RIGHTS_CONSENT)
        && rights.2.as_deref() == Some(OWNER_RIGHTS_PERMITTED_USE)
        && rights.3.as_deref() == Some(OWNER_RIGHTS_ATTRIBUTION)
        && rights.4.as_deref() == Some(OWNER_RIGHTS_SOURCE)
        && blank(&rights.5)
}

fn unstamped_rights(rights: &RightsTuple) -> bool {
    blank(&rights.0) && blank(&rights.1) && blank(&rights.2) && blank(&rights.3) && blank(&rights.4) && blank(&rights.5)
}

fn validate_pool_source_rights(rows: &BTreeMap<String, Vec<RightsTuple>>) -> Result<(usize, usize), String> {
    if rows.is_empty() {
        return Err("active review pool has no source recordings".to_string());
    }
    let mut exact = 0usize;
    let mut unstamped = 0usize;
    for (path, entries) in rows {
        for rights in entries {
            if !blank(&rights.5) {
                return Err(format!("review-pool recording has revoked rights and will not be changed: {path}"));
            }
            if exact_owner_rights(rights) {
                exact += 1;
            } else if unstamped_rights(rights) {
                unstamped += 1;
            } else {
                return Err(format!(
                    "review-pool recording has conflicting rights and will not be overwritten: {path}"
                ));
            }
        }
    }
    Ok((exact, unstamped))
}

fn pool_rights_digest(pool_id: &str, rows: &BTreeMap<String, Vec<RightsTuple>>) -> String {
    let mut digest = Sha256::new();
    hash_field(&mut digest, pool_id.as_bytes());
    for path in rows.keys() {
        hash_field(&mut digest, path.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_LICENSE.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_CONSENT.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_PERMITTED_USE.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_ATTRIBUTION.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_SOURCE.as_bytes());
    }
    digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn rights_coverage(db: &Database) -> Result<RightsCoverageReport, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let rows = pool_source_rights_on(db.connection())?;
    let mut report = RightsCoverageReport {
        recordings: rows.len(),
        segment_rows: 0,
        exact_rows: 0,
        unstamped_rows: 0,
        conflicting_rows: 0,
        revoked_rows: 0,
        all_exact: false,
        rights_sha256: None,
    };
    for entries in rows.values() {
        for rights in entries {
            report.segment_rows += 1;
            if !blank(&rights.5) {
                report.revoked_rows += 1;
            } else if exact_owner_rights(rights) {
                report.exact_rows += 1;
            } else if unstamped_rights(rights) {
                report.unstamped_rows += 1;
            } else {
                report.conflicting_rows += 1;
            }
        }
    }
    report.all_exact = report.recordings > 0 && report.exact_rows == report.segment_rows;
    if report.all_exact {
        report.rights_sha256 = Some(pool_rights_digest(&pool.pool_id, &rows));
    }
    Ok(report)
}

pub fn stamp_owner_supplied_pool_rights(db: &Database) -> Result<RightsStampReport, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_SCHEMA_VERSION {
        return Err("owner rights stamping requires review-pool schema 63".to_string());
    }
    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("review-pool rights stamping cannot lock the database: {error}"))?;
        let before = pool_source_rights_on(&tx)?;
        let (_exact_rows, unstamped_rows) = validate_pool_source_rights(&before)?;
        let stamped_rows = tx
            .execute(
                "UPDATE speech_segments
                    SET rights_license=?1, rights_consent_basis=?2, rights_permitted_use=?3,
                        rights_attribution=?4, rights_source=?5, updated_at=datetime('now')
                  WHERE EXISTS (
                        SELECT 1 FROM review_pool_members member
                        JOIN speech_segments pool_segment ON pool_segment.id=member.segment_id
                       WHERE pool_segment.audio_path=speech_segments.audio_path
                  )
                    AND TRIM(COALESCE(rights_license,''))=''
                    AND TRIM(COALESCE(rights_consent_basis,''))=''
                    AND TRIM(COALESCE(rights_permitted_use,''))=''
                    AND TRIM(COALESCE(rights_attribution,''))=''
                    AND TRIM(COALESCE(rights_source,''))=''
                    AND TRIM(COALESCE(rights_revoked_at,''))=''",
                rusqlite::params![
                    OWNER_RIGHTS_LICENSE,
                    OWNER_RIGHTS_CONSENT,
                    OWNER_RIGHTS_PERMITTED_USE,
                    OWNER_RIGHTS_ATTRIBUTION,
                    OWNER_RIGHTS_SOURCE,
                ],
            )
            .map_err(|error| format!("review-pool rights cannot be stamped: {error}"))?;
        if stamped_rows != unstamped_rows {
            return Err(format!(
                "review-pool rights changed during stamping ({stamped_rows}/{unstamped_rows} rows); transaction refused"
            ));
        }
        let after = pool_source_rights_on(&tx)?;
        validate_pool_source_rights(&after)?;
        if after.values().flatten().any(|rights| !exact_owner_rights(rights)) {
            return Err("review-pool rights are not exact after stamping".to_string());
        }
        let mut segments = 0usize;
        let mut stamped_recordings = 0usize;
        let mut already_exact_recordings = 0usize;
        for (path, entries) in &after {
            segments += entries.len();
            let before_entries =
                before.get(path).ok_or_else(|| format!("recording disappeared during stamping: {path}"))?;
            if before_entries.iter().any(unstamped_rights) {
                stamped_recordings += 1;
            } else {
                already_exact_recordings += 1;
            }
        }
        tx.commit().map_err(|error| format!("review-pool rights stamping cannot commit: {error}"))?;
        Ok(RightsStampReport {
            recordings: after.len(),
            segments,
            stamped_recordings,
            already_exact_recordings,
            rights_sha256: pool_rights_digest(&pool.pool_id, &after),
        })
    })
}

pub fn record_owner_adjudication(
    db: &Database,
    pool: &ReviewPool,
    input: &OwnerAdjudicationInput<'_>,
) -> Result<i64, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_SCHEMA_VERSION {
        return Err("owner adjudication requires review-pool schema 63".to_string());
    }
    if !pool.contains(input.segment_id) {
        return Err("owner adjudication is outside the active review pool".to_string());
    }
    canonical_uuid(input.operation_id, "owner adjudication operation id")?;
    if input.created_at_ms <= 0 {
        return Err("owner adjudication timestamp is invalid".to_string());
    }
    let requested_outcome = match input.final_action {
        "retain" => ReviewOutcome::Retain(
            input
                .final_transcript
                .map(canonical_verbatim_text)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| "retained owner adjudication requires a transcript".to_string())?,
        ),
        "reject" if input.final_transcript.is_none() => ReviewOutcome::Reject,
        _ => return Err("owner adjudication must be retain+text or reject without text".to_string()),
    };
    let replay: Option<(i64, String, Option<String>)> = db
        .connection()
        .query_row(
            "SELECT id, final_action, final_transcript
               FROM review_pool_owner_adjudications WHERE operation_id=?1",
            [input.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| format!("owner adjudication receipt cannot be read: {error}"))?;
    if let Some((id, action, transcript)) = replay {
        let outcome = match action.as_str() {
            "retain" => ReviewOutcome::Retain(canonical_verbatim_text(transcript.as_deref().unwrap_or_default())),
            "reject" => ReviewOutcome::Reject,
            _ => return Err("stored owner adjudication receipt is invalid".to_string()),
        };
        return if outcome == requested_outcome {
            Ok(id)
        } else {
            Err("owner adjudication operation id is already bound to another outcome".to_string())
        };
    }
    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("owner adjudication cannot lock the database: {error}"))?;
        let reviewers = reviewer_sets_on(&tx)?;
        let adjudications = owner_adjudications_on(&tx)?;
        let (resolution, evidence_digest) =
            derive_resolution(input.segment_id, reviewers.get(input.segment_id), adjudications.get(input.segment_id));
        if !matches!(resolution, DerivedResolution::OwnerConflict) {
            return Err("owner adjudication is allowed only after three distinct outcomes".to_string());
        }
        tx.execute(
            "INSERT INTO review_pool_owner_adjudications
             (pool_id, segment_id, final_action, final_transcript, evidence_sha256,
              operation_id, app_git_sha, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                pool.pool_id,
                input.segment_id,
                requested_outcome.final_action(),
                requested_outcome.final_transcript(),
                evidence_digest,
                input.operation_id,
                crate::GIT_SHA,
                input.created_at_ms,
            ],
        )
        .map_err(|error| format!("owner adjudication cannot be written: {error}"))?;
        let id = tx.last_insert_rowid();
        tx.commit().map_err(|error| format!("owner adjudication cannot commit: {error}"))?;
        Ok(id)
    })
}

pub fn operation(db: &Database, operation_id: &str) -> Result<Option<PoolOperationReceipt>, String> {
    db.connection()
        .query_row(
            "SELECT id, pool_id, segment_id, reviewer, operation_payload_hash
               FROM review_pool_decisions WHERE operation_id=?1",
            [operation_id],
            |row| {
                Ok(PoolOperationReceipt {
                    decision_id: row.get(0)?,
                    pool_id: row.get(1)?,
                    segment_id: row.get(2)?,
                    reviewer: row.get(3)?,
                    operation_payload_hash: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("review pool operation receipt cannot be read: {error}"))
}

pub fn reviewer_already_saw(db: &Database, segment_id: &str, reviewer: &str) -> Result<bool, String> {
    let reviewer = reviewer_key(Some(reviewer));
    let seen: i64 = db
        .connection()
        .query_row(
            "SELECT CASE WHEN EXISTS (
                    SELECT 1 FROM speech_segments segment
                     WHERE segment.id=?1 AND segment.verified=1
                       AND segment.human_decision IN ('accept','edit','reject')
                       AND lower(trim(COALESCE(segment.reviewed_by, '@desktop-owner'))) = ?2
                ) OR EXISTS (
                    SELECT 1 FROM effective_review_pool_decisions_v62 decision
                     WHERE decision.segment_id=?1 AND lower(trim(decision.reviewer))=?2
                ) OR EXISTS (
                    SELECT 1 FROM effective_independent_review_decisions_v61 decision
                     WHERE decision.segment_id=?1 AND lower(trim(decision.reviewer))=?2
                ) THEN 1 ELSE 0 END",
            rusqlite::params![segment_id, reviewer],
            |row| row.get(0),
        )
        .map_err(|error| format!("reviewer pool history cannot be checked: {error}"))?;
    Ok(seen == 1)
}

/// Append an independent decision without touching the canonical corpus row.
pub fn record_decision(db: &Database, pool: &ReviewPool, input: &PoolDecisionInput<'_>) -> Result<Option<i64>, String> {
    if !pool.contains(input.segment_id) {
        return Err("decision is outside the active review pool".to_string());
    }
    canonical_uuid(input.operation_id, "review pool decision operation id")?;
    if !valid_lower_sha256(input.operation_payload_hash)
        || input.created_at_ms <= 0
        || input.served_revision < 0
        || input.duration_ms <= 0
        || input.reviewer.trim().is_empty()
    {
        return Err("review pool decision contains invalid identity or timing evidence".to_string());
    }
    match input.action {
        "accept" | "edit" if input.submitted_transcript.is_some_and(|text| !text.trim().is_empty()) => {}
        "reject" | "skip" if input.submitted_transcript.is_none() => {}
        _ => return Err("review pool decision action/transcript is invalid".to_string()),
    }
    let reviewer = reviewer_key(Some(input.reviewer));
    let (changed, decision_id) = with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("review pool decision cannot lock the database: {error}"))?;
        let reviewers = reviewer_sets_on(&tx)?;
        let adjudications = owner_adjudications_on(&tx)?;
        let current = reviewers.get(input.segment_id);
        if current.is_some_and(|state| state.seen.contains(&reviewer)) {
            return Err("review pool decision is duplicated for this reviewer".to_string());
        }
        let (resolution, _) = derive_resolution(input.segment_id, current, adjudications.get(input.segment_id));
        match resolution {
            DerivedResolution::Resolved { .. } => {
                return Err("review pool clip is already resolved".to_string());
            }
            DerivedResolution::OwnerConflict => {
                return Err("review pool clip requires owner adjudication".to_string());
            }
            DerivedResolution::Pending | DerivedResolution::NeedsThird => {}
        }
        let changed = tx
            .execute(
                "INSERT INTO review_pool_decisions
                (pool_id, segment_id, reviewer, action, submitted_transcript, served_transcript,
                 served_revision, audio_content_hash, source_start_ms, source_end_ms, duration_ms,
                 requested_action, requested_transcript, operation_id, operation_payload_hash,
                 app_git_sha, playback_guard_version, created_at_ms)
             SELECT ?1, segment.id, trim(?3), ?4, ?5, ?6, ?7,
                    CASE WHEN ?4='skip' THEN NULL ELSE member.audio_content_hash END,
                    CASE WHEN ?4='skip' THEN NULL ELSE member.source_start_ms END,
                    CASE WHEN ?4='skip' THEN NULL ELSE member.source_end_ms END,
                    member.duration_ms, ?12, ?13, ?14, ?15, ?16, ?17, ?18
               FROM speech_segments segment
               JOIN review_pool_members member ON member.segment_id=segment.id AND member.pool_id=?1
              WHERE segment.id=?2
                AND segment.verified=1
                AND segment.human_decision IN ('accept','edit','reject')
                AND segment.review_revision=?7
                AND segment.raw_transcript=member.raw_transcript
                AND COALESCE(segment.model_version_id, '')=member.model_version_id
                AND segment.audio_content_hash=member.audio_content_hash
                AND json_extract(segment.alignment_json, '$.source_start_ms')=member.source_start_ms
                AND json_extract(segment.alignment_json, '$.source_end_ms')=member.source_end_ms
                AND segment.duration_ms=member.duration_ms
                AND TRIM(member.raw_transcript)=?6
                AND member.duration_ms=?11
                AND (?4='skip' OR (
                     member.audio_content_hash=?8
                     AND member.source_start_ms=?9
                     AND member.source_end_ms=?10
                ))",
                rusqlite::params![
                    pool.pool_id,
                    input.segment_id,
                    input.reviewer,
                    input.action,
                    input.submitted_transcript,
                    input.served_transcript.trim(),
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
                    REVIEW_POOL_PLAYBACK_GUARD,
                    input.created_at_ms,
                ],
            )
            .map_err(|error| format!("review pool decision cannot be written: {error}"))?;
        let decision_id = tx.last_insert_rowid();
        tx.commit().map_err(|error| format!("review pool decision cannot commit: {error}"))?;
        Ok((changed, decision_id))
    })
    .map_err(|error| format!("review pool decision cannot be committed: {error}"))?;
    if changed == 0 {
        return Ok(None);
    }
    Ok(Some(decision_id))
}

pub fn latest_decision(
    db: &Database,
    pool_id: &str,
    reviewer: &str,
) -> Result<Option<(i64, String, String, i64)>, String> {
    db.connection()
        .query_row(
            "SELECT id, segment_id, operation_id, created_at_ms
               FROM effective_review_pool_decisions_v62
              WHERE pool_id=?1 AND reviewer=?2 COLLATE NOCASE
              ORDER BY id DESC LIMIT 1",
            rusqlite::params![pool_id, reviewer],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| format!("latest review pool decision cannot be read: {error}"))
}

pub fn reverse_decision(
    db: &Database,
    pool: &ReviewPool,
    decision_id: i64,
    reviewer: &str,
    operation_id: &str,
    created_at_ms: i64,
) -> Result<(), String> {
    canonical_uuid(operation_id, "review pool reversal operation id")?;
    let existing: Option<(String, String)> = db
        .connection()
        .query_row(
            "SELECT operation_id, reviewer FROM review_pool_reversals WHERE decision_id=?1",
            [decision_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("review pool reversal receipt cannot be read: {error}"))?;
    if let Some((existing_operation, existing_reviewer)) = existing {
        if existing_operation == operation_id && existing_reviewer.eq_ignore_ascii_case(reviewer) {
            return Ok(());
        }
        return Err("review pool decision already has another reversal identity".to_string());
    }
    let changed = db
        .with_full_sync(|| {
            Ok(db.connection().execute(
                "INSERT INTO review_pool_reversals(decision_id, operation_id, reviewer, created_at_ms)
             SELECT decision.id, ?2, ?3, ?4 FROM review_pool_decisions decision
              WHERE decision.id=?1 AND decision.pool_id=?5 AND decision.reviewer=?3 COLLATE NOCASE",
                rusqlite::params![decision_id, operation_id, reviewer, created_at_ms, pool.pool_id],
            )?)
        })
        .map_err(|error| format!("review pool decision cannot be reversed: {error}"))?;
    if changed != 1 {
        return Err("review pool reversal target is missing or belongs to another reviewer".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CHAMPION: &str = "omniasr-7b-test-champion";

    fn seed_champion(db: &Database) {
        crate::registry::register_candidate(
            db,
            &crate::registry::NewModelVersion {
                id: TEST_CHAMPION.to_string(),
                family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                model_card_name: Some("test champion".to_string()),
                checkpoint_sha256: "c".repeat(64),
                checkpoint_path: "/test/champion.json".to_string(),
                source: "cortex-finetuned".to_string(),
                license: "owner-full-rights".to_string(),
            },
        )
        .unwrap();
        db.connection().execute("UPDATE model_versions SET status='champion' WHERE id=?1", [TEST_CHAMPION]).unwrap();
    }

    fn segment(id: &str, audio_path: &Path, reviewed_by: Option<&str>) -> crate::db::SpeechSegment {
        crate::db::SpeechSegment {
            id: id.to_string(),
            audio_path: audio_path.to_string_lossy().to_string(),
            raw_transcript: "دەقی چامپیۆن".to_string(),
            annotated_transcript: reviewed_by.map(|_| "دەقی دروست".to_string()),
            verdict: reviewed_by.map(|_| "human_edit".to_string()),
            verdict_transcript: reviewed_by.map(|_| "دەقی دروست".to_string()),
            human_decision: reviewed_by.map(|_| "edit".to_string()),
            reviewed_by: reviewed_by.map(str::to_string),
            verified: reviewed_by.is_some(),
            duration_ms: 1_000,
            model_version_id: Some(TEST_CHAMPION.to_string()),
            alignment_json: Some(r#"{"source_start_ms":0,"source_end_ms":1000}"#.to_string()),
            ..crate::db::SpeechSegment::default()
        }
    }

    fn reviewed_segment(id: &str, audio_path: &Path, reviewer: &str, text: &str) -> crate::db::SpeechSegment {
        let mut value = segment(id, audio_path, Some(reviewer));
        value.annotated_transcript = Some(text.to_string());
        value.verdict_transcript = Some(text.to_string());
        value
    }

    fn one_clip_pool(first_text: &str) -> (tempfile::TempDir, Database, ReviewPool) {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("clip.wav");
        std::fs::write(&audio, b"wav").unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        assert_eq!(crate::migrations::rollback(&db, 4).unwrap(), vec![63, 62, 61, 60]);
        db.insert_segment_full(&reviewed_segment("clip", &audio, "Rubar", first_text)).unwrap();
        assert_eq!(crate::migrations::run_migrations(&db).unwrap(), vec![60, 61, 62, 63]);
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='clip'", ["a".repeat(64)])
            .unwrap();
        let pool = activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174050",
            &[PoolMemberInput { segment_id: "clip".into(), voice_name: "Lamo".into() }],
        )
        .unwrap();
        (dir, db, pool)
    }

    fn decide(db: &Database, pool: &ReviewPool, reviewer: &str, text: &str, operation_id: &str, at: i64) -> i64 {
        let (_, revision) = db.get_segment_by_id_with_revision("clip").unwrap().unwrap();
        record_decision(
            db,
            pool,
            &PoolDecisionInput {
                segment_id: "clip",
                reviewer,
                action: "edit",
                submitted_transcript: Some(text),
                served_transcript: "دەقی چامپیۆن",
                served_revision: revision,
                audio_content_hash: Some(&"a".repeat(64)),
                source_start_ms: Some(0),
                source_end_ms: Some(1_000),
                duration_ms: 1_000,
                requested_action: "edit",
                requested_transcript: text,
                operation_id,
                operation_payload_hash: &"b".repeat(64),
                created_at_ms: at,
            },
        )
        .unwrap()
        .unwrap()
    }

    fn evidence(voice_name: &str) -> PoolMemberEvidence {
        PoolMemberEvidence {
            voice_name: voice_name.to_string(),
            raw_transcript: "دەقی چامپیۆن".to_string(),
            model_version_id: TEST_CHAMPION.to_string(),
            audio_content_hash: "a".repeat(64),
            source_start_ms: 0,
            source_end_ms: 1_000,
            duration_ms: 1_000,
        }
    }

    #[test]
    fn membership_digest_is_order_independent_and_voice_bound() {
        let a = HashMap::from([("b".to_string(), evidence("Kawa")), ("a".to_string(), evidence("Lamo"))]);
        let b = HashMap::from([("a".to_string(), evidence("Lamo")), ("b".to_string(), evidence("Kawa"))]);
        assert_eq!(member_evidence(&a).unwrap(), member_evidence(&b).unwrap());
        let changed = HashMap::from([("a".to_string(), evidence("Halwest")), ("b".to_string(), evidence("Kawa"))]);
        assert_ne!(member_evidence(&a).unwrap().1, member_evidence(&changed).unwrap().1);
    }

    #[test]
    fn pool_resolves_the_exact_registry_champion() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(current_champion_7b_model_id(&db).is_err());
        seed_champion(&db);
        assert_eq!(current_champion_7b_model_id(&db).unwrap(), TEST_CHAMPION);
    }

    #[test]
    fn pool_refuses_a_7b_candidate_that_is_not_the_champion() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("candidate.wav");
        std::fs::write(&audio, b"wav").unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        let candidate_id = "omniasr-7b-stale-candidate";
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: candidate_id.to_string(),
                family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                model_card_name: Some("stale candidate".to_string()),
                checkpoint_sha256: "d".repeat(64),
                checkpoint_path: "/test/candidate.json".to_string(),
                source: "cortex-finetuned".to_string(),
                license: "owner-full-rights".to_string(),
            },
        )
        .unwrap();
        let mut candidate = segment("candidate", &audio, None);
        candidate.model_version_id = Some(candidate_id.to_string());
        db.insert_segment_full(&candidate).unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='candidate'", ["a".repeat(64)])
            .unwrap();
        let error = activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174099",
            &[PoolMemberInput { segment_id: "candidate".into(), voice_name: "Lamo".into() }],
        )
        .unwrap_err();
        assert!(error.contains("current OmniASR-7B champion"), "unexpected refusal: {error}");
    }

    #[test]
    fn last_mile_audio_check_refuses_a_file_removed_after_pool_startup() {
        let (directory, db, pool) = one_clip_pool("دەقی دروست");
        let audio = directory.path().join("clip.wav");
        assert_eq!(pending_segment_ids(&db, &pool, "Alle", None).unwrap(), vec!["clip"]);

        std::fs::remove_file(&audio).unwrap();

        let error = pool.verify_audio_available("clip").unwrap_err();
        assert!(error.contains("audio is missing"), "unexpected refusal: {error}");
    }

    #[test]
    fn pool_orders_least_covered_and_keeps_second_review_append_only() {
        let dir = tempfile::tempdir().unwrap();
        let first_audio = dir.path().join("first.wav");
        let second_audio = dir.path().join("second.wav");
        std::fs::write(&first_audio, b"wav").unwrap();
        std::fs::write(&second_audio, b"wav").unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        assert_eq!(crate::migrations::rollback(&db, 4).unwrap(), vec![63, 62, 61, 60]);
        db.insert_segment_full(&segment("first", &first_audio, Some("Rubar"))).unwrap();
        db.insert_segment_full(&segment("second", &second_audio, None)).unwrap();
        assert_eq!(crate::migrations::run_migrations(&db).unwrap(), vec![60, 61, 62, 63]);
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash=CASE id WHEN 'first' THEN ?1 ELSE ?2 END
                  WHERE id IN ('first','second')",
                rusqlite::params!["a".repeat(64), "b".repeat(64)],
            )
            .unwrap();
        let pool_id = "123e4567-e89b-42d3-a456-426614174000";
        let pool = activate(
            &db,
            pool_id,
            &[
                PoolMemberInput { segment_id: "first".into(), voice_name: "Lamo".into() },
                PoolMemberInput { segment_id: "second".into(), voice_name: "Kawa".into() },
            ],
        )
        .unwrap();

        let identity_error = db
            .connection()
            .execute("UPDATE speech_segments SET raw_transcript='changed draft' WHERE id='first'", [])
            .unwrap_err()
            .to_string();
        assert!(
            identity_error.contains("review pool clip identity is immutable"),
            "unexpected guard: {identity_error}"
        );

        assert_eq!(pending_segment_ids(&db, &pool, "Rubar", None).unwrap(), vec!["second"]);
        assert_eq!(pending_segment_ids(&db, &pool, "Alle", None).unwrap(), vec!["second", "first"]);

        let (_, revision) = db.get_segment_by_id_with_revision("first").unwrap().unwrap();
        let operation_payload_hash = "a".repeat(64);
        let inserted = record_decision(
            &db,
            &pool,
            &PoolDecisionInput {
                segment_id: "first",
                reviewer: "Alle",
                action: "edit",
                submitted_transcript: Some("دەقی دووەم"),
                served_transcript: "دەقی چامپیۆن",
                served_revision: revision,
                audio_content_hash: Some(&operation_payload_hash),
                source_start_ms: Some(0),
                source_end_ms: Some(1_000),
                duration_ms: 1_000,
                requested_action: "edit",
                requested_transcript: "دەقی دووەم",
                operation_id: "123e4567-e89b-42d3-a456-426614174001",
                operation_payload_hash: &operation_payload_hash,
                created_at_ms: 1,
            },
        )
        .unwrap()
        .unwrap();
        let lamo = coverage_by_voice(&db).unwrap().into_iter().find(|row| row.voice_name == "Lamo").unwrap();
        assert_eq!(lamo.two_reviews, 1);
        assert!(!pending_segment_ids(&db, &pool, "Alle", None).unwrap().contains(&"first".to_string()));
        let canonical = db.get_segment_by_id("first").unwrap().unwrap();
        assert_eq!(canonical.reviewed_by.as_deref(), Some("Rubar"));
        assert_eq!(canonical.annotated_transcript.as_deref(), Some("دەقی دروست"));

        reverse_decision(&db, &pool, inserted, "Alle", "123e4567-e89b-42d3-a456-426614174001", 2).unwrap();
        let lamo = coverage_by_voice(&db).unwrap().into_iter().find(|row| row.voice_name == "Lamo").unwrap();
        assert_eq!(lamo.one_review, 1);
        assert!(pending_segment_ids(&db, &pool, "Alle", None).unwrap().contains(&"first".to_string()));

        let next_champion = "omniasr-7b-next-champion";
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: next_champion.to_string(),
                family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                model_card_name: Some("next champion".to_string()),
                checkpoint_sha256: "d".repeat(64),
                checkpoint_path: "/test/next-champion.json".to_string(),
                source: "cortex-finetuned".to_string(),
                license: "owner-full-rights".to_string(),
            },
        )
        .unwrap();
        let tx = db.connection().unchecked_transaction().unwrap();
        tx.execute(
            "UPDATE model_versions SET status='rolled_back' WHERE family=?1 AND status='champion'",
            [crate::deployment::OMNIASR_7B_FAMILY],
        )
        .unwrap();
        tx.execute("UPDATE model_versions SET status='champion' WHERE id=?1", [next_champion]).unwrap();
        tx.commit().unwrap();
        assert!(!registry_matches(&db, &pool).unwrap(), "champion rotation must pause the bound pool");
        assert!(load(&db).unwrap_err().contains("champion identity no longer matches"));
    }

    #[test]
    fn two_exact_outcomes_resolve_and_stop_further_serving() {
        let (_dir, db, pool) = one_clip_pool("دەقی یەکسان");
        decide(&db, &pool, "Alle", "دەقی یەکسان", "123e4567-e89b-42d3-a456-426614174051", 1);
        let row = segment_resolutions(&db, Some("Lamo")).unwrap().pop().unwrap();
        assert_eq!(row.status, "resolved");
        assert_eq!(row.final_action.as_deref(), Some("retain"));
        assert_eq!(row.final_transcript.as_deref(), Some("دەقی یەکسان"));
        assert_eq!(row.reviewer_count, 2);
        assert!(pending_segment_ids(&db, &pool, "Sewa", None).unwrap().is_empty());
    }

    #[test]
    fn disagreement_gets_one_blinded_third_review_then_resolves_by_matching_pair() {
        let (_dir, db, pool) = one_clip_pool("دەقی یەکەم");
        decide(&db, &pool, "Alle", "دەقی دووەم", "123e4567-e89b-42d3-a456-426614174052", 1);
        let row = segment_resolutions(&db, None).unwrap().pop().unwrap();
        assert_eq!(row.status, "needsThirdReview");
        assert_eq!(pending_segment_ids(&db, &pool, "Sewa", None).unwrap(), vec!["clip"]);
        decide(&db, &pool, "Sewa", "دەقی یەکەم", "123e4567-e89b-42d3-a456-426614174053", 2);
        let row = segment_resolutions(&db, None).unwrap().pop().unwrap();
        assert_eq!(row.status, "resolved");
        assert_eq!(row.final_transcript.as_deref(), Some("دەقی یەکەم"));
        assert_eq!(row.agreeing_reviewers, vec!["Rubar", "Sewa"]);
        assert!(pending_segment_ids(&db, &pool, "Roza", None).unwrap().is_empty());
    }

    #[test]
    fn three_distinct_outcomes_require_owner_and_owner_ruling_is_evidence_bound() {
        let (_dir, db, pool) = one_clip_pool("دەقی یەکەم");
        decide(&db, &pool, "Alle", "دەقی دووەم", "123e4567-e89b-42d3-a456-426614174054", 1);
        let third_id = decide(&db, &pool, "Sewa", "دەقی سێیەم", "123e4567-e89b-42d3-a456-426614174055", 2);
        assert_eq!(segment_resolutions(&db, None).unwrap()[0].status, "ownerConflict");
        assert!(pending_segment_ids(&db, &pool, "Roza", None).unwrap().is_empty());
        record_owner_adjudication(
            &db,
            &pool,
            &OwnerAdjudicationInput {
                segment_id: "clip",
                final_action: "retain",
                final_transcript: Some("دەقی یەکەم"),
                operation_id: "123e4567-e89b-42d3-a456-426614174056",
                created_at_ms: 3,
            },
        )
        .unwrap();
        assert_eq!(segment_resolutions(&db, None).unwrap()[0].status, "ownerResolved");
        reverse_decision(&db, &pool, third_id, "Sewa", "123e4567-e89b-42d3-a456-426614174057", 4).unwrap();
        let reopened = &segment_resolutions(&db, None).unwrap()[0];
        assert_eq!(reopened.status, "needsThirdReview");
        assert_eq!(pending_segment_ids(&db, &pool, "Sewa", None).unwrap(), vec!["clip"]);
    }

    #[test]
    fn consensus_is_nfc_and_outer_trim_exact_but_keeps_punctuation_distinct() {
        let (_dir, db, pool) = one_clip_pool("é");
        decide(&db, &pool, "Alle", "  e\u{301}  ", "123e4567-e89b-42d3-a456-426614174058", 1);
        assert_eq!(segment_resolutions(&db, None).unwrap()[0].status, "resolved");

        let (_dir, db, pool) = one_clip_pool("دەق");
        decide(&db, &pool, "Alle", "دەق.", "123e4567-e89b-42d3-a456-426614174059", 1);
        assert_eq!(segment_resolutions(&db, None).unwrap()[0].status, "needsThirdReview");
    }

    #[test]
    fn owner_rights_stamping_is_exact_idempotent_and_pool_source_scoped() {
        let (dir, db, _pool) = one_clip_pool("دەقی یەکسان");
        let shared_source = dir.path().join("clip.wav");
        let outside_source = dir.path().join("outside.wav");
        std::fs::write(&outside_source, b"wav").unwrap();
        db.insert_segment_full(&segment("shared-source-shadow", &shared_source, None)).unwrap();
        db.insert_segment_full(&segment("outside", &outside_source, None)).unwrap();

        let first = stamp_owner_supplied_pool_rights(&db).unwrap();
        assert_eq!(first.recordings, 1);
        assert_eq!(first.segments, 2);
        assert_eq!(first.stamped_recordings, 1);
        assert_eq!(first.already_exact_recordings, 0);
        assert_eq!(first.rights_sha256.len(), 64);

        let exact: (String, String, String, String, String, Option<String>) = db
            .connection()
            .query_row(
                "SELECT rights_license, rights_consent_basis, rights_permitted_use,
                        rights_attribution, rights_source, rights_revoked_at
                   FROM speech_segments WHERE id='shared-source-shadow'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
        assert_eq!(exact.0, OWNER_RIGHTS_LICENSE);
        assert_eq!(exact.1, OWNER_RIGHTS_CONSENT);
        assert_eq!(exact.2, OWNER_RIGHTS_PERMITTED_USE);
        assert_eq!(exact.3, OWNER_RIGHTS_ATTRIBUTION);
        assert_eq!(exact.4, OWNER_RIGHTS_SOURCE);
        assert!(exact.5.is_none());

        let outside_rights: Option<String> = db
            .connection()
            .query_row("SELECT rights_license FROM speech_segments WHERE id='outside'", [], |row| row.get(0))
            .unwrap();
        assert!(outside_rights.is_none(), "unrelated recordings must never be stamped");

        let second = stamp_owner_supplied_pool_rights(&db).unwrap();
        assert_eq!(second.stamped_recordings, 0);
        assert_eq!(second.already_exact_recordings, 1);
        assert_eq!(second.rights_sha256, first.rights_sha256);
    }

    #[test]
    fn owner_rights_stamping_fails_closed_without_partial_writes() {
        let (dir, db, _pool) = one_clip_pool("دەقی یەکسان");
        let shared_source = dir.path().join("clip.wav");
        db.insert_segment_full(&segment("conflict", &shared_source, None)).unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET rights_license='third-party-license' WHERE id='conflict'", [])
            .unwrap();

        let error = stamp_owner_supplied_pool_rights(&db).unwrap_err();
        assert!(error.contains("conflicting rights"), "unexpected refusal: {error}");
        let member_rights: Option<String> = db
            .connection()
            .query_row("SELECT rights_license FROM speech_segments WHERE id='clip'", [], |row| row.get(0))
            .unwrap();
        assert!(member_rights.is_none(), "preflight failure must leave every blank row untouched");
    }

    #[test]
    fn owner_rights_stamping_refuses_revoked_recordings() {
        let (_dir, db, _pool) = one_clip_pool("دەقی یەکسان");
        db.connection()
            .execute("UPDATE speech_segments SET rights_revoked_at='2026-08-24T00:00:00Z' WHERE id='clip'", [])
            .unwrap();
        let error = stamp_owner_supplied_pool_rights(&db).unwrap_err();
        assert!(error.contains("revoked rights"), "unexpected refusal: {error}");
    }
}
