//! Flexible, voice-organized human review pool.
//!
//! The canonical `speech_segments` row still receives the first human verdict. Later reviewers write
//! append-only observations here, so an independent second or third judgement can never overwrite the
//! first answer. Queue selection is coverage-first and reviewer-specific: a person sees clips they have
//! not judged, ordered by the number of distinct effective judgements already attached to each clip.

mod authority;
mod dedup;

pub use authority::{
    record_voice_certificate, rights_coverage, stamp_owner_supplied_pool_rights, voice_authority_digests,
    voice_certificate, RightsCoverageReport, RightsStampReport, VoiceAuthorityDigests, VoiceCertificateInput,
    VoiceCertificateRecord, OWNER_RIGHTS_ATTRIBUTION, OWNER_RIGHTS_CONSENT, OWNER_RIGHTS_LICENSE,
    OWNER_RIGHTS_PERMITTED_USE, OWNER_RIGHTS_SOURCE,
};
pub use dedup::{apply_dedup_manifest, dedup_status, PoolDedupStatus};
#[cfg(test)]
pub(crate) use dedup::{canonical_json_bytes, normalized_text_sha256};
use dedup::{load_dedup_binding, RegistryDedupRow};

use crate::db::Database;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

const REVIEW_POOL_BASE_SCHEMA_VERSION: i64 = 62;
pub const REVIEW_POOL_SCHEMA_VERSION: i64 = 63;
const REVIEW_POOL_DEDUP_SCHEMA_VERSION: i64 = 64;
pub const REVIEW_POOL_PLAYBACK_GUARD: &str = "content-hash-raw-counter-v3";
const DESKTOP_REVIEWER_KEY: &str = "@desktop-owner";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPool {
    pub pool_id: String,
    pub focus_segment_count: usize,
    pub focus_sha256: String,
    pub review_segment_count: usize,
    pub excluded_duplicate_count: usize,
    pub duplicate_family_count: usize,
    pub dedup_manifest_sha256: Option<String>,
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
    /// The exact policy-4 playback authority this judgement rides on (`None` only for a skip). It is
    /// consumed in the same transaction as the decision, so one listen authorizes one judgement.
    pub playback_authority_session_id: Option<&'a str>,
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
    let (dedup, excluded_member_ids) = load_dedup_binding(db, &pool_id, actual_count, &actual_sha256)?;
    members.retain(|segment_id, _| !excluded_member_ids.contains(segment_id));
    audio_paths.retain(|segment_id, _| !excluded_member_ids.contains(segment_id));
    playable_member_ids.retain(|segment_id| !excluded_member_ids.contains(segment_id));
    let member_ids = Arc::new(members.keys().cloned().collect());
    let pool = ReviewPool {
        pool_id,
        focus_segment_count: actual_count,
        focus_sha256: actual_sha256,
        review_segment_count: members.len(),
        excluded_duplicate_count: dedup.excluded_segment_count,
        duplicate_family_count: dedup.duplicate_family_count,
        dedup_manifest_sha256: dedup.manifest_sha256,
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
    let schema_version = crate::migrations::get_current_version(db).map_err(|error| error.to_string())?;
    if schema_version < REVIEW_POOL_BASE_SCHEMA_VERSION {
        return Ok(false);
    }
    let current_champion = current_champion_7b_identity(db)?;
    if schema_version < REVIEW_POOL_DEDUP_SCHEMA_VERSION {
        let current: Option<(String, i64, String, String, String, i64)> = db
            .connection()
            .query_row(
                "SELECT registry.pool_id, registry.focus_segment_count, registry.focus_sha256,
                        registry.champion_model_version_id, registry.champion_deployment_sha256,
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
        return Ok(current.is_some_and(|(pool_id, count, sha256, model_id, deployment_sha256, member_count)| {
            pool_id == bound.pool_id
                && usize::try_from(count).ok() == Some(bound.focus_segment_count)
                && sha256 == bound.focus_sha256
                && model_id == bound.champion_model_version_id
                && deployment_sha256 == bound.champion_deployment_sha256
                && current_champion.model_version_id == bound.champion_model_version_id
                && current_champion.deployment_sha256 == bound.champion_deployment_sha256
                && member_count == count
                && bound.dedup_manifest_sha256.is_none()
                && bound.excluded_duplicate_count == 0
                && bound.review_segment_count == bound.focus_segment_count
        }));
    }
    let current: Option<RegistryDedupRow> = db
        .connection()
        .query_row(
            "SELECT registry.pool_id,
                    registry.focus_segment_count,
                    registry.focus_sha256,
                    registry.champion_model_version_id,
                    registry.champion_deployment_sha256,
                    COUNT(member.segment_id),
                    (SELECT manifest_sha256 FROM review_pool_dedup_manifests
                      WHERE pool_id=registry.pool_id),
                    (SELECT COUNT(*) FROM review_pool_duplicate_exclusions exclusion
                      WHERE exclusion.pool_id=registry.pool_id)
               FROM review_pool_registry registry
               LEFT JOIN review_pool_members member ON member.pool_id=registry.pool_id
              WHERE registry.singleton_key=1
              GROUP BY registry.pool_id, registry.focus_segment_count, registry.focus_sha256,
                       registry.champion_model_version_id, registry.champion_deployment_sha256",
            [],
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
        .map_err(|error| format!("review pool registry cannot be revalidated: {error}"))?;
    Ok(current.is_some_and(
        |(pool_id, count, sha256, model_id, deployment_sha256, member_count, dedup_sha256, excluded_count)| {
            pool_id == bound.pool_id
                && usize::try_from(count).ok() == Some(bound.focus_segment_count)
                && sha256 == bound.focus_sha256
                && model_id == bound.champion_model_version_id
                && deployment_sha256 == bound.champion_deployment_sha256
                && current_champion.model_version_id == bound.champion_model_version_id
                && current_champion.deployment_sha256 == bound.champion_deployment_sha256
                && member_count == count
                && dedup_sha256 == bound.dedup_manifest_sha256
                && usize::try_from(excluded_count).ok() == Some(bound.excluded_duplicate_count)
                && usize::try_from(count - excluded_count).ok() == Some(bound.review_segment_count)
        },
    ))
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
                return Err(format!("segment {segment_id} is assigned to two voice characters"));
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

/// Segment ids the ACTIVE pool would ship: members MINUS duplicate exclusions.
///
/// `None` means no pool is registered, which is the pre-pool corpus case and leaves every export
/// scoped exactly as it was before this existed.
///
/// Deliberately lighter than `load`. `load` additionally re-proves champion identity and REFUSES
/// when the registered champion has drifted from the live one — correct for serving a review queue,
/// wrong for deciding export scope: an export must not begin failing because a model pointer moved,
/// and it must never fall back to "no scope" (i.e. the whole library) for that reason. Scope is a
/// membership question, so this asks only the membership tables, with the same duplicate-exclusion
/// clause `pending_segment_ids` serves from — so what ships and what is reviewed cannot disagree.
pub fn exportable_segment_ids(db: &Database) -> Result<Option<HashSet<String>>, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_BASE_SCHEMA_VERSION
    {
        return Ok(None);
    }
    let pool_id: Option<String> = db
        .connection()
        .query_row("SELECT pool_id FROM review_pool_registry WHERE singleton_key = 1", [], |row| row.get(0))
        .optional()
        .map_err(|error| format!("review pool registry cannot be read: {error}"))?;
    let Some(pool_id) = pool_id else {
        return Ok(None);
    };
    let mut statement = db
        .connection()
        .prepare(
            "SELECT member.segment_id
               FROM review_pool_members member
              WHERE member.pool_id = ?1
                AND NOT EXISTS (
                    SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                     WHERE exclusion.pool_id = member.pool_id AND exclusion.segment_id = member.segment_id
                )",
        )
        .map_err(|error| format!("review pool export scope cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([&pool_id], |row| row.get::<_, String>(0))
        .map_err(|error| format!("review pool export scope cannot be read: {error}"))?;
    let mut ids = HashSet::new();
    for row in rows {
        ids.insert(row.map_err(|error| format!("review pool export scope row is unreadable: {error}"))?);
    }
    Ok(Some(ids))
}

/// The segments CONSENSUS has decided: owner canon 2026-08-29 — "a sentence is decided by any two
/// DIFFERENT reviewers".
///
/// Returns only clips whose derived resolution is `Resolved` (two or more distinct reviewers agreed
/// on the same outcome) or `ownerResolved` (an owner adjudication over that exact evidence). A clip
/// with one opinion is `Pending`, two disagreeing opinions are `NeedsThird`, and three-way
/// disagreement is `OwnerConflict` — none of those may ship, because none of them is a decision
/// under the canon.
///
/// This deliberately delegates to `segment_resolutions`, which is the same authority the review
/// queue and the certification report read, so what SHIPS and what the pool CALLS decided can never
/// drift apart. It therefore also inherits `load`'s identity proof and FAILS CLOSED: if the pool
/// cannot be proven, this errors and the export refuses rather than shipping unverified rows. That
/// is the opposite of `exportable_segment_ids`, whose failure mode had to avoid widening scope;
/// here refusing is correct, because an unprovable corpus is not a publishable one.
pub fn consensus_resolved_segment_ids(db: &Database) -> Result<HashSet<String>, String> {
    Ok(segment_resolutions(db, None)?
        .into_iter()
        .filter(|row| matches!(row.status.as_str(), "resolved" | "ownerResolved"))
        .map(|row| row.segment_id)
        .collect())
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
            "SELECT segment.id, segment.audio_path
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE member.pool_id=?1
                AND NOT EXISTS (
                    SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                     WHERE exclusion.pool_id=member.pool_id AND exclusion.segment_id=member.segment_id
                )
                AND TRIM(COALESCE(segment.raw_transcript, '')) <> ''
                AND NOT (TRIM(segment.raw_transcript) LIKE '[%]')",
        )
        .map_err(|error| format!("review pool queue cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([&pool.pool_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| format!("review pool queue cannot be read: {error}"))?;
    let mut pending: Vec<PendingVoiceCandidate> = Vec::new();
    for row in rows {
        let (segment_id, audio_path) = row.map_err(|error| format!("review pool row is unreadable: {error}"))?;
        // A clip that already carries one canonical opinion IS the work the consensus canon wants
        // served next. Until 2026-09-04 a PAY-FENCE MIRROR skipped it here because a pool second
        // opinion was unpaid (measured that day: 1,451 one-opinion clips, zero two-opinion, ten
        // active reviewers). The owner priced second opinions at the first-opinion weights, so
        // `record_decision` now mints the credit and the mirror is gone together with the fence.
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
        // OWNER CANON 2026-08-29: a sentence is DECIDED by any two different reviewers. Serve first
        // the clips that are one opinion away from being decided.
        //
        // This used to sort by judged-count ASCENDING, which served a clip nobody had reviewed
        // BEFORE a clip already holding one opinion — so reviewers fanned out across new audio and
        // almost never met on the same clip. Measured on 2026-08-29: 416 clips holding exactly one
        // review, and ZERO resolved. Breadth-first maximises clips touched; the canon asks for
        // clips DECIDED, and a clip with one opinion is worth strictly more than a fresh one
        // because a single further judgement can retire it.
        //
        // 1 judgement -> needs one more to agree.        2 -> a disagreeing pair awaiting a third.
        // 0 -> untouched, still valuable but furthest from a decision. 3+ -> owner conflict, which
        // no ordinary reviewer can settle, so it sinks to the bottom rather than blocking the queue.
        let judged = coverage.map_or(0, |coverage| coverage.judged.len());
        let distance_to_decision: usize = match judged {
            1 => 0,
            2 => 1,
            0 => 2,
            _ => 3,
        };
        // Preserve decision proximity as the first authority, but do not make a reviewer listen to
        // one import/podcast sequence or one voice for hours. `created_at` is source-ingest order, and
        // the Lamo corpus was imported file-by-file; sorting by it produced runs such as 011978,
        // 011979, 011981, 011982. A digest alone scattered source files, but a pool that is ~70% Lamo
        // still produced thousands of adjacent same-voice transitions and reviewers reasonably
        // described that as hearing "the same sound" again. Keep a content-independent stable digest
        // within each voice, then smoothly interleave the frozen voice buckets inside each priority tier.
        // This remains identical after restart and never lets lower-priority work jump a clip nearer
        // consensus.
        let spread_key: [u8; 32] = Sha256::digest(segment_id.as_bytes()).into();
        let voice_name = pool
            .members
            .get(&segment_id)
            .map(|member| member.voice_name.clone())
            .ok_or_else(|| format!("review pool clip {segment_id} has no frozen voice identity"))?;
        pending.push((distance_to_decision, voice_name, spread_key, segment_id));
    }
    interleave_pending_voices(pending)
}

type PendingVoiceCandidate = (usize, String, [u8; 32], String);
type PendingVoiceItem = ([u8; 32], String);

struct PendingVoiceBucket {
    items: Vec<PendingVoiceItem>,
    weight: i128,
    credit: i128,
}

type PendingVoiceTier = BTreeMap<String, PendingVoiceBucket>;

/// Deterministically spread voices without weakening decision proximity.
///
/// Each tier is independent: a fresh clip can never jump work one opinion from resolution. Smooth
/// weighted round-robin retains the real corpus proportions while spreading minority voices across
/// the whole tier instead of consuming them at the front and leaving one enormous same-voice tail.
/// The streak ceiling is the mathematical lower bound `ceil(largest / (all_other + 1))`, so it never
/// promises a mix the corpus cannot supply. Before choosing a voice, a feasibility check proves the
/// remainder can still meet that ceiling; this prevents a superficially balanced prefix followed by
/// an oversized one-voice tail. The BTreeMap makes ties stable, and reverse-sorted voice vectors let
/// `pop` emit the smallest SHA-256 key in O(1).
fn interleave_pending_voices(candidates: Vec<PendingVoiceCandidate>) -> Result<Vec<String>, String> {
    let mut tiers: BTreeMap<usize, PendingVoiceTier> = BTreeMap::new();
    for (distance, voice_name, spread_key, segment_id) in candidates {
        tiers
            .entry(distance)
            .or_default()
            .entry(voice_name)
            .or_insert_with(|| PendingVoiceBucket { items: Vec::new(), weight: 0, credit: 0 })
            .items
            .push((spread_key, segment_id));
    }

    let mut ordered = Vec::new();
    for voices in tiers.values_mut() {
        for bucket in voices.values_mut() {
            bucket.items.sort_unstable_by(|left, right| right.cmp(left));
            bucket.weight = bucket.items.len() as i128;
        }

        let total: usize = voices.values().map(|bucket| bucket.items.len()).sum();
        let largest = voices.values().map(|bucket| bucket.items.len()).max().unwrap_or(0);
        let alternatives = total.saturating_sub(largest);
        let max_streak = largest.div_ceil(alternatives + 1).max(1);
        let total_weight = total as i128;
        let mut last_voice: Option<String> = None;
        let mut streak = 0usize;

        for _ in 0..total {
            let mut active: Vec<String> =
                voices.iter().filter(|(_, bucket)| !bucket.items.is_empty()).map(|(voice, _)| voice.clone()).collect();
            if active.is_empty() {
                return Err("review pool voice scheduler exhausted before its measured tier".to_string());
            }
            for voice in &active {
                let Some(bucket) = voices.get_mut(voice) else {
                    return Err("review pool voice scheduler lost an active bucket".to_string());
                };
                bucket.credit += bucket.weight;
            }
            active.sort_by(|left, right| {
                let left_credit = voices.get(left).map_or(i128::MIN, |bucket| bucket.credit);
                let right_credit = voices.get(right).map_or(i128::MIN, |bucket| bucket.credit);
                right_credit.cmp(&left_credit).then_with(|| left.cmp(right))
            });
            let mut selected: Option<String> = None;
            for voice in &active {
                let next_streak = if last_voice.as_ref() == Some(voice) { streak + 1 } else { 1 };
                if next_streak > max_streak
                    || !remaining_voice_schedule_is_feasible(voices, voice, next_streak, max_streak)
                {
                    continue;
                }
                selected = Some(voice.clone());
                break;
            }
            let Some(voice) = selected else {
                return Err("review pool voice scheduler cannot satisfy its feasible streak bound".to_string());
            };
            let Some(bucket) = voices.get_mut(&voice) else {
                return Err("review pool voice scheduler lost the selected bucket".to_string());
            };
            let Some((_, segment_id)) = bucket.items.pop() else {
                return Err("review pool voice scheduler selected an empty bucket".to_string());
            };
            bucket.credit -= total_weight;
            if last_voice.as_ref() == Some(&voice) {
                streak += 1;
            } else {
                last_voice = Some(voice);
                streak = 1;
            }
            ordered.push(segment_id);
        }
    }
    Ok(ordered)
}

fn remaining_voice_schedule_is_feasible(
    voices: &PendingVoiceTier,
    selected_voice: &str,
    selected_streak: usize,
    max_streak: usize,
) -> bool {
    let total_remaining = voices.values().map(|bucket| bucket.items.len()).sum::<usize>().saturating_sub(1);
    voices.iter().all(|(voice, bucket)| {
        let remaining = bucket.items.len().saturating_sub(usize::from(voice == selected_voice));
        let others = total_remaining.saturating_sub(remaining);
        let capacity = if voice == selected_voice {
            max_streak.saturating_sub(selected_streak).saturating_add(max_streak.saturating_mul(others))
        } else {
            max_streak.saturating_mul(others.saturating_add(1))
        };
        remaining <= capacity
    })
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
        require_pool_operation_namespace_on(&tx, input.operation_id, false)?;
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
        if changed == 1 && input.action != "skip" {
            // Owner canon 2026-09-04: a second opinion is paid exactly like a first one. Consume the
            // playback authority and mint the credit in THIS transaction, so no committed judgement
            // can exist unpaid and no lost response can pay twice.
            let Some(authority_id) = input.playback_authority_session_id else {
                return Err(
                    "E_NO_PLAYBACK_EVIDENCE: a paid pool judgement requires this reviewer's finalized policy-4 playback authority"
                        .to_string(),
                );
            };
            let (Some(content_hash), Some(source_start_ms), Some(source_end_ms)) =
                (input.audio_content_hash, input.source_start_ms, input.source_end_ms)
            else {
                return Err("review pool decision is missing the audio identity its playback proof binds".to_string());
            };
            // The proof is re-verified against the current row and consumed inside THIS transaction
            // (namespace `independent`, linked back through this row's operation id): a missing,
            // reused, or mismatched authority rolls the judgement, its consumption and its credit
            // back together.
            crate::db::consume_couch_playback_authority_for_pool_decision_on(
                &tx,
                authority_id,
                input.reviewer,
                input.segment_id,
                input.served_revision,
                content_hash,
                source_start_ms,
                source_end_ms,
                input.operation_id,
                input.created_at_ms,
            )
            .map_err(|error| format!("review pool playback authority cannot be consumed: {error}"))?;
            Database::append_review_pool_compensation_tx(
                &tx,
                decision_id,
                input.segment_id,
                input.reviewer,
                input.action,
                input.served_revision,
            )
            .map_err(|error| format!("review pool compensation cannot be written: {error}"))?;
        }
        tx.commit().map_err(|error| format!("review pool decision cannot commit: {error}"))?;
        Ok((changed, decision_id))
    })
    .map_err(|error| format!("review pool decision cannot be committed: {error}"))?;
    if changed == 0 {
        return Ok(None);
    }
    Ok(Some(decision_id))
}

/// Mint a finalized policy-4 Couch playback authority from a SYNTHETIC full-coverage traversal.
///
/// Fixture facility only: test databases, and `pool_admin benchmark-commit` on a disposable clone
/// (that CLI refuses without `--confirm-disposable`). It runs the production finalization — source
/// lease, decoded-PCM identity, revision/span re-resolution under BEGIN IMMEDIATE — so the rows it
/// writes have exactly the shape a phone reviewer's do, but nobody listened. No production code path
/// calls it, and it must never run against the live database.
pub fn mint_synthetic_playback_authority(
    db: &Database,
    reviewer: &str,
    segment_id: &str,
    session_binding_sha256: &str,
) -> Result<String, String> {
    let revision = db
        .segment_review_revision(segment_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("segment {segment_id} has no review revision"))?;
    let segment = db
        .get_segment_by_id(segment_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("segment {segment_id} is missing"))?;
    let audio_content_hash = db
        .segment_audio_content_hash(segment_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("segment {segment_id} has no PCM identity"))?;
    let (source_start_ms, source_end_ms) = db
        .segment_source_span(segment_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("segment {segment_id} has no canonical source span"))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(10_000)
        .max(10_000);
    let authority = crate::db::CouchPlaybackAttemptAuthority {
        playback_receipt_id: uuid::Uuid::new_v4().hyphenated().to_string(),
        media_grant_id: uuid::Uuid::new_v4().hyphenated().to_string(),
        client_attempt_id: uuid::Uuid::new_v4().hyphenated().to_string(),
        session_binding_sha256: session_binding_sha256.to_string(),
        reviewer: reviewer.to_string(),
        segment_id: segment_id.to_string(),
        segment_revision: revision,
        audio_content_hash,
        source_path: std::path::PathBuf::from(&segment.audio_path),
        clip_duration_ms: segment.duration_ms,
        source_start_ms,
        source_end_ms,
        issued_at_ms: now_ms,
        expires_at_ms: now_ms + 60_000,
    };
    db.finalize_couch_playback_attempt_v1(
        &authority,
        &[crate::db::DesktopPlaybackInterval { start_ms: 0, end_ms: segment.duration_ms }],
        segment.duration_ms,
    )
    .map(|receipt| receipt.playback_receipt_id)
    .map_err(|error| error.to_string())
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

/// A pool UUID cannot alias evidence owned by another review table. Call under the same IMMEDIATE
/// transaction as the effect and credit. Same-table replay is checked by the caller and unique keys.
fn require_pool_operation_namespace_on(
    conn: &rusqlite::Connection,
    operation_id: &str,
    reversal: bool,
) -> Result<(), String> {
    let collision: bool = conn
        .query_row(
            "SELECT EXISTS(
             SELECT 1 FROM review_events WHERE operation_id=?1
             UNION ALL SELECT 1 FROM human_decision_effect_events WHERE operation_id=?1
             UNION ALL SELECT 1 FROM human_decision_effect_reversals WHERE operation_id=?1
             UNION ALL SELECT 1 FROM review_flag_effect_events WHERE operation_id=?1
             UNION ALL SELECT 1 FROM review_flag_effect_reversals WHERE operation_id=?1
             UNION ALL SELECT 1 FROM independent_review_decisions WHERE operation_id=?1
             UNION ALL SELECT 1 FROM independent_review_reversals WHERE operation_id=?1
             UNION ALL SELECT 1 FROM review_pool_decisions WHERE operation_id=?1 AND ?2
             UNION ALL SELECT 1 FROM review_pool_reversals WHERE operation_id=?1 AND NOT ?2
         )",
            rusqlite::params![operation_id, reversal],
            |row| row.get(0),
        )
        .map_err(|error| format!("review pool operation namespace cannot be checked: {error}"))?;
    if collision {
        return Err(
            "E_REVIEW_OPERATION_NAMESPACE_COLLISION: pool operation UUID already belongs to other review truth"
                .to_string(),
        );
    }
    Ok(())
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
    let changed = with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("review pool reversal cannot lock the database: {error}"))?;
        require_pool_operation_namespace_on(&tx, operation_id, true)?;
        let changed = tx
            .execute(
                "INSERT INTO review_pool_reversals(decision_id, operation_id, reviewer, created_at_ms)
             SELECT decision.id, ?2, ?3, ?4 FROM review_pool_decisions decision
              WHERE decision.id=?1 AND decision.pool_id=?5 AND decision.reviewer=?3 COLLATE NOCASE",
                rusqlite::params![decision_id, operation_id, reviewer, created_at_ms, pool.pool_id],
            )
            .map_err(|error| format!("review pool reversal cannot be written: {error}"))?;
        if changed == 1 {
            Database::append_review_pool_compensation_reversal_tx(&tx, decision_id, operation_id)
                .map_err(|error| format!("review pool compensation cannot be reversed: {error}"))?;
        }
        tx.commit().map_err(|error| format!("review pool reversal cannot commit: {error}"))?;
        Ok(changed)
    })
    .map_err(|error| format!("review pool decision cannot be reversed: {error}"))?;
    if changed != 1 {
        return Err("review pool reversal target is missing or belongs to another reviewer".to_string());
    }
    Ok(())
}

/// Reverse one exact HTTP-visible pool decision with a distinct durable reversal identity.
///
/// `Ok(None)` is a semantic conflict (wrong reviewer/pool/decision operation, or a stale target that
/// is no longer this reviewer's newest pool action). Storage failures remain `Err`. The newest check
/// reads the append-only base table, not the effective view, so an exact retry after the reversal is
/// still bound to the same decision instead of sliding backward to older effective evidence.
pub fn reverse_decision_addressed(
    db: &Database,
    pool: &ReviewPool,
    decision_id: i64,
    reviewer: &str,
    decision_operation_id: &str,
    reversal_operation_id: &str,
    created_at_ms: i64,
) -> Result<Option<String>, String> {
    if decision_id <= 0 || created_at_ms <= 0 {
        return Ok(None);
    }
    canonical_uuid(decision_operation_id, "review pool decision operation id")?;
    canonical_uuid(reversal_operation_id, "review pool reversal operation id")?;
    if decision_operation_id == reversal_operation_id {
        return Ok(None);
    }
    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("addressed review pool reversal cannot lock the database: {error}"))?;
        let target: Option<(String, String)> = tx
            .query_row(
                "SELECT segment_id, operation_id FROM review_pool_decisions
                  WHERE id=?1 AND pool_id=?2 AND reviewer=?3 COLLATE NOCASE",
                rusqlite::params![decision_id, pool.pool_id, reviewer],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("addressed review pool reversal target cannot be read: {error}"))?;
        let Some((segment_id, stored_decision_operation_id)) = target else {
            return Ok(None);
        };
        if stored_decision_operation_id != decision_operation_id {
            return Ok(None);
        }
        let latest_decision_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM review_pool_decisions
                  WHERE pool_id=?1 AND reviewer=?2 COLLATE NOCASE
                  ORDER BY id DESC LIMIT 1",
                rusqlite::params![pool.pool_id, reviewer],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("latest addressed review pool action cannot be read: {error}"))?;
        if latest_decision_id != Some(decision_id) {
            return Ok(None);
        }
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT operation_id, reviewer FROM review_pool_reversals WHERE decision_id=?1",
                [decision_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("addressed review pool reversal receipt cannot be read: {error}"))?;
        if let Some((stored_reversal_operation_id, stored_reviewer)) = existing {
            return Ok((stored_reversal_operation_id == reversal_operation_id
                && stored_reviewer.eq_ignore_ascii_case(reviewer))
            .then_some(segment_id));
        }
        require_pool_operation_namespace_on(&tx, reversal_operation_id, true)?;
        tx.execute(
            "INSERT INTO review_pool_reversals(decision_id, operation_id, reviewer, created_at_ms)
             VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![decision_id, reversal_operation_id, reviewer, created_at_ms],
        )
        .map_err(|error| format!("addressed review pool reversal cannot be written: {error}"))?;
        Database::append_review_pool_compensation_reversal_tx(&tx, decision_id, reversal_operation_id)
            .map_err(|error| format!("addressed review pool compensation cannot be reversed: {error}"))?;
        tx.commit().map_err(|error| format!("addressed review pool reversal cannot commit: {error}"))?;
        Ok(Some(segment_id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CHAMPION: &str = "omniasr-7b-test-champion";

    fn rollback_fixture_to(db: &Database, target_version: i64) {
        let expected = crate::migrations::MIGRATIONS
            .iter()
            .filter(|migration| migration.version > target_version)
            .rev()
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        assert_eq!(crate::migrations::rollback(db, expected.len()).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(db).unwrap(), target_version);
    }

    fn upgrade_fixture_from(db: &Database, source_version: i64) {
        let expected = crate::migrations::MIGRATIONS
            .iter()
            .filter(|migration| migration.version > source_version)
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        assert_eq!(crate::migrations::run_migrations(db).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(db).unwrap(), crate::migrations::max_supported_version());
    }

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

    thread_local! {
        static CLIP_HASH_SCRATCH: tempfile::TempDir = tempfile::tempdir().expect("clip identity scratch directory");
    }

    /// A real 16 kHz mono WAV whose decoded PCM depends only on `seed`, so a fixture's stored
    /// `audio_content_hash` (`clip_hash(seed)`) is the file's TRUE identity and policy-4 finalization
    /// verifies the source lease exactly as it does for a phone reviewer.
    fn write_clip_wav(path: &Path, seed: &str) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let seed = seed.as_bytes();
        for n in 0..16_000_usize {
            let salt = seed.get(n % seed.len().max(1)).copied().unwrap_or(128) as i16 - 128;
            writer.write_sample(((n % 1000) as i16).wrapping_mul(30).wrapping_add(salt)).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn clip_hash(seed: &str) -> String {
        CLIP_HASH_SCRATCH.with(|dir| {
            let path = dir.path().join(format!("{seed}.wav"));
            if !path.exists() {
                write_clip_wav(&path, seed);
            }
            crate::export_bundle::current_canonical_pcm_blake3(&path).unwrap()
        })
    }

    /// A finalized policy-4 authority for `reviewer` on `segment_id`, bound to the fixture session.
    fn authority(db: &Database, reviewer: &str, segment_id: &str) -> String {
        mint_synthetic_playback_authority(db, reviewer, segment_id, &"f".repeat(64)).unwrap()
    }

    fn one_clip_pool(first_text: &str) -> (tempfile::TempDir, Database, ReviewPool) {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("clip.wav");
        write_clip_wav(&audio, "a");
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        rollback_fixture_to(&db, 59);
        db.insert_segment_full(&reviewed_segment("clip", &audio, "Rubar", first_text)).unwrap();
        upgrade_fixture_from(&db, 59);
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='clip'", [clip_hash("a")])
            .unwrap();
        let pool = activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174050",
            &[PoolMemberInput { segment_id: "clip".into(), voice_name: "Lamo".into() }],
        )
        .unwrap();
        (dir, db, pool)
    }

    fn two_clip_pool(reviewed_segment_id: Option<&str>) -> (tempfile::TempDir, Database, ReviewPool) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        rollback_fixture_to(&db, 59);
        for id in ["a", "b"] {
            let audio = dir.path().join(format!("{id}.wav"));
            write_clip_wav(&audio, id);
            let row = if reviewed_segment_id == Some(id) {
                reviewed_segment(id, &audio, "Rubar", "دەقی دروست")
            } else {
                segment(id, &audio, None)
            };
            db.insert_segment_full(&row).unwrap();
        }
        upgrade_fixture_from(&db, 59);
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='a'", [clip_hash("a")])
            .unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='b'", [clip_hash("b")])
            .unwrap();
        let pool = activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174051",
            &[
                PoolMemberInput { segment_id: "a".into(), voice_name: "Lamo".into() },
                PoolMemberInput { segment_id: "b".into(), voice_name: "Lamo".into() },
            ],
        )
        .unwrap();
        (dir, db, pool)
    }

    fn dedup_manifest(pool: &ReviewPool, canonical: &str, reviewed: Option<&str>, generated_at_ms: i64) -> String {
        let segment_ids = vec!["a".to_string(), "b".to_string()];
        let proof_edges = vec![serde_json::json!({
            "leftSegmentId": "a",
            "rightSegmentId": "b",
            "correlationPpm": 1_000_000,
        })];
        let family_material = serde_json::json!({
            "poolId": &pool.pool_id,
            "proofEdges": &proof_edges,
            "segmentIds": &segment_ids,
        });
        let family_id: String = Sha256::digest(canonical_json_bytes(&family_material).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let members: Vec<_> = segment_ids
            .iter()
            .map(|segment_id| {
                let frozen = pool.members.get(segment_id).unwrap();
                serde_json::json!({
                    "segmentId": segment_id,
                    "voiceName": &frozen.voice_name,
                    "sourceFileName": format!("{segment_id}.wav"),
                    "rawTranscriptSha256": normalized_text_sha256(&frozen.raw_transcript),
                    "audioContentHash": &frozen.audio_content_hash,
                    "sourceStartMs": frozen.source_start_ms,
                    "sourceEndMs": frozen.source_end_ms,
                    "durationMs": frozen.duration_ms,
                    "reviewEvidenceCount": usize::from(reviewed == Some(segment_id.as_str())),
                    "snrMilliDb": null,
                    "clippingPpm": null,
                    "signalAnomalyPpm": null,
                    "confidencePpm": null,
                    "canonical": canonical == segment_id,
                })
            })
            .collect();
        let reason = if reviewed.is_some() {
            "preserve-human-review-evidence"
        } else {
            "best-measured-audio-quality-then-stable-identity"
        };
        let mut value = serde_json::json!({
            "manifestSchema": 1,
            "algorithm": {
                "id": "cortex-cross-file-waveform-correlation-v1",
                "minimumTextCharacters": 25,
                "offsetToleranceMs": 500,
                "minimumTextSimilarityPpm": 900_000,
                "audioDurationToleranceMs": 120,
                "minimumWaveformCorrelationPpm": 980_000,
                "comparisonSampleRateHz": 16_000,
            },
            "pool": {
                "poolId": &pool.pool_id,
                "sourceFocusSegmentCount": pool.focus_segment_count,
                "sourceFocusSha256": &pool.focus_sha256,
                "championModelVersionId": &pool.champion_model_version_id,
                "championDeploymentSha256": &pool.champion_deployment_sha256,
            },
            "summary": {
                "candidateTextGroups": 1,
                "clearedRepeatedTextGroups": 0,
                "duplicateFamilies": 1,
                "excludedMembers": 1,
                "canonicalMembers": 1,
                "unconfirmedRiskGroups": 0,
                "reviewedCanonicalMembers": usize::from(reviewed.is_some()),
            },
            "families": [{
                "familyId": family_id,
                "voiceName": "Lamo",
                "canonicalSegmentId": canonical,
                "canonicalSelectionReason": reason,
                "members": members,
                "proofEdges": proof_edges,
            }],
            "generatedAtMs": generated_at_ms,
        });
        let digest: String =
            Sha256::digest(canonical_json_bytes(&value).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect();
        value.as_object_mut().unwrap().insert("manifestSha256".into(), serde_json::Value::String(digest));
        String::from_utf8(canonical_json_bytes(&value).unwrap()).unwrap()
    }

    fn decide(db: &Database, pool: &ReviewPool, reviewer: &str, text: &str, operation_id: &str, at: i64) -> i64 {
        let (_, revision) = db.get_segment_by_id_with_revision("clip").unwrap().unwrap();
        let authority = authority(db, reviewer, "clip");
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
                audio_content_hash: Some(&clip_hash("a")),
                source_start_ms: Some(0),
                source_end_ms: Some(1_000),
                duration_ms: 1_000,
                requested_action: "edit",
                requested_transcript: text,
                operation_id,
                operation_payload_hash: &"b".repeat(64),
                created_at_ms: at,
                playback_authority_session_id: Some(&authority),
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
            audio_content_hash: clip_hash("a"),
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
    fn dedup_manifest_is_idempotent_and_removes_only_the_proven_duplicate_from_review() {
        let (_dir, db, source_pool) = two_clip_pool(None);
        let manifest = dedup_manifest(&source_pool, "a", None, 1_000);
        let status = apply_dedup_manifest(&db, &manifest).unwrap();
        assert!(status.applied);
        assert_eq!(status.source_segment_count, 2);
        assert_eq!(status.canonical_segment_count, 1);
        assert_eq!(status.excluded_segment_count, 1);
        assert_eq!(status.duplicate_family_count, 1);
        assert_eq!(apply_dedup_manifest(&db, &manifest).unwrap(), status, "exact retry must be idempotent");

        let canonical_pool = load(&db).unwrap().unwrap();
        assert_eq!(canonical_pool.focus_segment_count, 2);
        assert_eq!(canonical_pool.review_segment_count, 1);
        assert!(canonical_pool.contains("a"));
        assert!(!canonical_pool.contains("b"));
        assert!(registry_matches(&db, &canonical_pool).unwrap());
        assert_eq!(pending_segment_ids(&db, &canonical_pool, "Alle", None).unwrap(), vec!["a"]);
        assert!(db
            .connection()
            .execute("UPDATE review_pool_dedup_manifests SET created_at_ms=created_at_ms+1", [])
            .is_err());
        assert!(db
            .connection()
            .execute("DELETE FROM review_pool_duplicate_exclusions WHERE segment_id='b'", [])
            .is_err());
        assert!(record_decision(
            &db,
            &canonical_pool,
            &PoolDecisionInput {
                segment_id: "b",
                reviewer: "Alle",
                action: "accept",
                submitted_transcript: None,
                served_transcript: "دەقی چامپیۆن",
                served_revision: 0,
                audio_content_hash: Some(&clip_hash("b")),
                source_start_ms: Some(0),
                source_end_ms: Some(1_000),
                duration_ms: 1_000,
                requested_action: "accept",
                requested_transcript: "دەقی چامپیۆن",
                operation_id: "123e4567-e89b-42d3-a456-426614174052",
                operation_payload_hash: &"c".repeat(64),
                created_at_ms: 2_000,
                playback_authority_session_id: None,
            },
        )
        .unwrap_err()
        .contains("outside the active review pool"));
        let rollback_through_dedup = crate::migrations::MIGRATIONS
            .iter()
            .filter(|migration| migration.version >= REVIEW_POOL_DEDUP_SCHEMA_VERSION)
            .count();
        assert!(crate::migrations::rollback(&db, rollback_through_dedup)
            .unwrap_err()
            .to_string()
            .contains("CHECK constraint failed"));
    }

    #[test]
    fn excluded_duplicates_retain_rights_revocation_lineage_but_not_review_authority() {
        let (_dir, db, source_pool) = two_clip_pool(None);
        let manifest = dedup_manifest(&source_pool, "a", None, 1_000);
        apply_dedup_manifest(&db, &manifest).unwrap();
        let before_revision: i64 = db
            .connection()
            .query_row("SELECT review_revision FROM speech_segments WHERE id='b'", [], |row| row.get(0))
            .unwrap();

        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET rights_revoked_at='2026-08-24T00:00:00Z', updated_at=datetime('now')
                  WHERE id='b'",
                [],
            )
            .expect("excluded duplicate must retain non-review rights-revocation lineage");
        let (revoked_at, after_revision): (String, i64) = db
            .connection()
            .query_row("SELECT rights_revoked_at, review_revision FROM speech_segments WHERE id='b'", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(revoked_at, "2026-08-24T00:00:00Z");
        assert_eq!(after_revision, before_revision + 1, "metadata changes retain the CAS lineage");
        assert!(db
            .connection()
            .execute("UPDATE speech_segments SET verified=1 WHERE id='b'", [])
            .expect_err("excluded duplicate must never gain canonical review evidence")
            .to_string()
            .contains("excluded duplicate clip cannot receive canonical review evidence"));
    }

    #[test]
    fn dedup_manifest_must_preserve_the_only_reviewed_member_and_is_immutable() {
        let (_dir, db, source_pool) = two_clip_pool(Some("b"));
        let wrong = dedup_manifest(&source_pool, "a", Some("b"), 1_000);
        assert!(apply_dedup_manifest(&db, &wrong).unwrap_err().contains("does not preserve its reviewed member"));
        assert!(!dedup_status(&db).unwrap().applied, "failed validation must write nothing");

        let correct = dedup_manifest(&source_pool, "b", Some("b"), 2_000);
        let applied = apply_dedup_manifest(&db, &correct).unwrap();
        assert!(applied.applied);
        let different = dedup_manifest(&source_pool, "b", Some("b"), 3_000);
        assert!(apply_dedup_manifest(&db, &different).unwrap_err().contains("different immutable dedup manifest"));
        let canonical_pool = load(&db).unwrap().unwrap();
        assert!(!canonical_pool.contains("a"));
        assert!(canonical_pool.contains("b"));
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
    fn only_a_clip_two_different_reviewers_decided_may_be_exported() {
        // OWNER CANON 2026-08-29: "a sentence is decided by any two DIFFERENT reviewers."
        // `one_clip_pool` leaves the clip holding exactly ONE opinion (canonical, by Rubar), which
        // is the state 416 live clips were in on the day this was written while the pool reported
        // resolved=0 -- and the fine-tune export happily emitted 410 of them. One opinion is not a
        // decision, and must not ship.
        let (_dir, db, pool) = one_clip_pool("دەقی چامپیۆن");

        let one_opinion = consensus_resolved_segment_ids(&db).unwrap();
        assert!(
            one_opinion.is_empty(),
            "a single reviewer's opinion is not a decision and must not be exportable: {one_opinion:?}"
        );
        let segment = db.get_segment_by_id("clip").unwrap().unwrap();
        // An empty pack reads as a broken export button. When consensus is the only reason nothing
        // survives, the export must REFUSE and name the count, so the operator learns the rule
        // instead of filing a bug against the exporter.
        let refusal = crate::export::exclude_unexportable_segments(&db, vec![segment.clone()])
            .expect_err("the export root must refuse loudly, not return an empty pack");
        let refusal = refusal.to_string();
        for needle in ["1 of 1", "waiting for a decision", "two DIFFERENT reviewers"] {
            assert!(refusal.contains(needle), "refusal must explain itself; missing {needle:?} in: {refusal}");
        }

        // A SECOND, DIFFERENT reviewer agreeing is what makes it a decision.
        decide(&db, &pool, "Alle", "دەقی چامپیۆن", "60000000-0000-4000-8000-0000000000a1", 2_000);

        let resolved = consensus_resolved_segment_ids(&db).unwrap();
        assert!(resolved.contains("clip"), "two different reviewers agreeing decides the sentence: {resolved:?}");
        let kept = crate::export::exclude_unexportable_segments(&db, vec![segment]).unwrap();
        let ids: Vec<&str> = kept.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["clip"], "a decided sentence must ship: {ids:?}");
    }

    #[test]
    fn pool_spreads_equal_priority_work_instead_of_replaying_import_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        rollback_fixture_to(&db, 59);
        for id in ["oldest", "middle", "newest"] {
            let audio = dir.path().join(format!("{id}.wav"));
            std::fs::write(&audio, b"wav").unwrap();
            db.insert_segment_full(&segment(id, &audio, None)).unwrap();
        }
        upgrade_fixture_from(&db, 59);
        for (id, byte, created_at) in [
            ("oldest", "a", "2026-01-01 00:00:01"),
            ("middle", "b", "2026-01-01 00:00:02"),
            ("newest", "c", "2026-01-01 00:00:03"),
        ] {
            db.connection()
                .execute(
                    "UPDATE speech_segments SET audio_content_hash=?2, created_at=?3 WHERE id=?1",
                    rusqlite::params![id, byte.repeat(64), created_at],
                )
                .unwrap();
        }
        let pool = activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174070",
            &[
                PoolMemberInput { segment_id: "oldest".into(), voice_name: "Lamo".into() },
                PoolMemberInput { segment_id: "middle".into(), voice_name: "Lamo".into() },
                PoolMemberInput { segment_id: "newest".into(), voice_name: "Lamo".into() },
            ],
        )
        .unwrap();

        assert_eq!(
            pending_segment_ids(&db, &pool, "Hemn", None).unwrap(),
            vec!["newest", "middle", "oldest"],
            "equal-priority work follows the pinned SHA-256 spread, not chronological import order"
        );
        let reloaded = load(&db).unwrap().expect("active pool reload");
        assert_eq!(
            pending_segment_ids(&db, &reloaded, "Hemn", None).unwrap(),
            vec!["newest", "middle", "oldest"],
            "the spread must survive an authority reload, not depend on runtime randomness"
        );
    }

    #[test]
    fn pool_interleaves_voices_deterministically_without_crossing_priority_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        rollback_fixture_to(&db, 59);
        let assignments = [
            ("halwest-a", "Halwest", 'a'),
            ("halwest-b", "Halwest", 'b'),
            ("kawa-a", "Kawa", 'c'),
            ("kawa-b", "Kawa", 'd'),
            ("lamo-a", "Lamo", 'e'),
            ("lamo-b", "Lamo", 'f'),
        ];
        for (id, _, _) in assignments {
            let audio = dir.path().join(format!("{id}.wav"));
            std::fs::write(&audio, b"wav").unwrap();
            db.insert_segment_full(&segment(id, &audio, None)).unwrap();
        }
        upgrade_fixture_from(&db, 59);
        for (id, _, hex) in assignments {
            db.connection()
                .execute(
                    "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                    rusqlite::params![id, hex.to_string().repeat(64)],
                )
                .unwrap();
        }
        let inputs: Vec<PoolMemberInput> = assignments
            .iter()
            .map(|(id, voice, _)| PoolMemberInput { segment_id: (*id).into(), voice_name: (*voice).into() })
            .collect();
        let pool = activate(&db, "123e4567-e89b-42d3-a456-426614174071", &inputs).unwrap();

        let first = pending_segment_ids(&db, &pool, "Hemn", None).unwrap();
        let voices: Vec<&str> = first.iter().map(|id| pool.voice_for(id).unwrap()).collect();
        assert_eq!(
            voices,
            vec!["Halwest", "Kawa", "Lamo", "Halwest", "Kawa", "Lamo"],
            "equal-priority work must alternate available voices instead of clustering the largest corpus"
        );
        let reloaded = load(&db).unwrap().expect("active pool reload");
        assert_eq!(
            pending_segment_ids(&db, &reloaded, "Hemn", None).unwrap(),
            first,
            "voice interleaving must remain stable across authority reloads"
        );

        let nearer = interleave_pending_voices(vec![
            (1, "Halwest".into(), [0; 32], "far-halwest".into()),
            (0, "Lamo".into(), [u8::MAX; 32], "near-lamo".into()),
            (1, "Kawa".into(), [u8::MAX; 32], "far-kawa".into()),
        ])
        .unwrap();
        assert_eq!(
            nearer.first().map(String::as_str),
            Some("near-lamo"),
            "voice variety must never let a lower-priority clip jump work nearer consensus"
        );

        let mut skewed = Vec::new();
        for (voice, count, prefix) in [("Halwest", 1, "h"), ("Kawa", 2, "k"), ("Lamo", 8, "l")] {
            for index in 0..count {
                skewed.push((0, voice.to_string(), [u8::try_from(index).unwrap(); 32], format!("{prefix}{index}")));
            }
        }
        let skewed = interleave_pending_voices(skewed).unwrap();
        let skewed_voices: Vec<&str> = skewed
            .iter()
            .map(|id| match id.as_bytes()[0] {
                b'h' => "Halwest",
                b'k' => "Kawa",
                b'l' => "Lamo",
                _ => unreachable!("fixture has an unknown voice"),
            })
            .collect();
        assert_eq!(
            skewed_voices,
            vec!["Lamo", "Lamo", "Kawa", "Lamo", "Lamo", "Halwest", "Lamo", "Lamo", "Kawa", "Lamo", "Lamo"],
            "minority voices must be spread across a skewed tier and the feasible maximum streak must be two"
        );
    }

    #[test]
    fn voice_interleave_meets_the_feasible_streak_bound_across_skew_shapes() {
        for a in 1usize..=8 {
            for b in 1usize..=8 {
                for c in 1usize..=8 {
                    let mut candidates = Vec::new();
                    for (voice, count) in [("A", a), ("B", b), ("C", c)] {
                        for index in 0..count {
                            candidates.push((
                                0,
                                voice.to_string(),
                                [u8::try_from(index).unwrap(); 32],
                                format!("{voice}{index}"),
                            ));
                        }
                    }
                    let ordered = interleave_pending_voices(candidates).unwrap();
                    let total = a + b + c;
                    let largest = a.max(b).max(c);
                    let feasible_bound = largest.div_ceil(total - largest + 1).max(1);
                    let mut maximum_run = 0usize;
                    let mut current_run = 0usize;
                    let mut previous_voice = None;
                    for segment_id in ordered {
                        let voice = segment_id.as_bytes()[0];
                        if previous_voice == Some(voice) {
                            current_run += 1;
                        } else {
                            previous_voice = Some(voice);
                            current_run = 1;
                        }
                        maximum_run = maximum_run.max(current_run);
                    }
                    assert!(
                        maximum_run <= feasible_bound,
                        "counts ({a}, {b}, {c}) produced run {maximum_run} above feasible bound {feasible_bound}"
                    );
                }
            }
        }
    }

    #[test]
    fn pool_serves_the_clips_nearest_a_decision_and_keeps_second_review_append_only() {
        let dir = tempfile::tempdir().unwrap();
        let first_audio = dir.path().join("first.wav");
        let second_audio = dir.path().join("second.wav");
        write_clip_wav(&first_audio, "a");
        write_clip_wav(&second_audio, "b");
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        rollback_fixture_to(&db, 59);
        db.insert_segment_full(&segment("first", &first_audio, Some("Rubar"))).unwrap();
        db.insert_segment_full(&segment("second", &second_audio, None)).unwrap();
        upgrade_fixture_from(&db, 59);
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash=CASE id WHEN 'first' THEN ?1 ELSE ?2 END
                  WHERE id IN ('first','second')",
                rusqlite::params![clip_hash("a"), clip_hash("b")],
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
        // OWNER CANON 2026-08-29: a sentence is decided by any two DIFFERENT reviewers, so the
        // queue serves whatever is NEAREST a decision. "first" already holds one opinion and one
        // more judgement can retire it; "second" is untouched and needs two. This assertion was
        // the reverse until the canon landed -- breadth-first maximised clips TOUCHED while
        // leaving 416 clips holding one review and zero decided.
        assert_eq!(pending_segment_ids(&db, &pool, "Alle", None).unwrap(), vec!["first", "second"]);

        let (_, revision) = db.get_segment_by_id_with_revision("first").unwrap().unwrap();
        let operation_payload_hash = "a".repeat(64);
        let audio_hash = clip_hash("a");
        let authority = authority(&db, "Alle", "first");
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
                audio_content_hash: Some(&audio_hash),
                source_start_ms: Some(0),
                source_end_ms: Some(1_000),
                duration_ms: 1_000,
                requested_action: "edit",
                requested_transcript: "دەقی دووەم",
                operation_id: "123e4567-e89b-42d3-a456-426614174001",
                operation_payload_hash: &operation_payload_hash,
                created_at_ms: 1,
                playback_authority_session_id: Some(&authority),
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

        // Undo is a distinct durable operation, not a second use of the decision UUID.
        reverse_decision(&db, &pool, inserted, "Alle", "123e4567-e89b-42d3-a456-426614174002", 2).unwrap();
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
    fn addressed_pool_undo_replays_the_exact_latest_decision_after_restart() {
        let (_dir, db, pool) = two_clip_pool(None);
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET verified=1, human_decision='edit', verdict='human_edit',
                        verdict_transcript='دەقی دروست', annotated_transcript='دەقی دروست',
                        reviewed_by='Rubar', review_revision=review_revision+1
                  WHERE id IN ('a','b')",
                [],
            )
            .unwrap();
        let decide_segment = |segment_id: &str, operation_id: &str, created_at_ms: i64| {
            let (_, revision) = db.get_segment_by_id_with_revision(segment_id).unwrap().unwrap();
            let audio_hash = clip_hash(segment_id);
            let authority = authority(&db, "Alle", segment_id);
            record_decision(
                &db,
                &pool,
                &PoolDecisionInput {
                    segment_id,
                    reviewer: "Alle",
                    action: "edit",
                    submitted_transcript: Some("دەقی ئەلە"),
                    served_transcript: "دەقی چامپیۆن",
                    served_revision: revision,
                    audio_content_hash: Some(&audio_hash),
                    source_start_ms: Some(0),
                    source_end_ms: Some(1_000),
                    duration_ms: 1_000,
                    requested_action: "edit",
                    requested_transcript: "دەقی ئەلە",
                    operation_id,
                    operation_payload_hash: &"e".repeat(64),
                    created_at_ms,
                    playback_authority_session_id: Some(&authority),
                },
            )
            .unwrap()
            .unwrap()
        };
        let older_operation = "123e4567-e89b-42d3-a456-426614174060";
        let latest_operation = "123e4567-e89b-42d3-a456-426614174061";
        let older = decide_segment("a", older_operation, 10);
        let latest = decide_segment("b", latest_operation, 20);
        let reversal_operation = "123e4567-e89b-42d3-a456-426614174062";

        assert_eq!(
            reverse_decision_addressed(&db, &pool, latest, "Alle", latest_operation, reversal_operation, 30,)
                .unwrap()
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            latest_decision(&db, &pool.pool_id, "Alle").unwrap().map(|value| value.0),
            Some(older),
            "the legacy effective fallback now points at the older decision that must not be touched"
        );

        // Model commit -> lost HTTP response -> app restart -> retry. No in-memory token exists; the
        // exact request coordinates alone must replay the same durable reversal.
        assert_eq!(
            reverse_decision_addressed(&db, &pool, latest, "alle", latest_operation, reversal_operation, 40,)
                .unwrap()
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM review_pool_reversals", [], |row| row.get(0))
                .unwrap(),
            1
        );
        assert!(
            db.connection()
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM effective_review_pool_decisions_v62 WHERE id=?1",
                    [older],
                    |row| row.get(0),
                )
                .unwrap()
                == 1
        );
        assert!(reverse_decision_addressed(
            &db,
            &pool,
            older,
            "Alle",
            older_operation,
            "123e4567-e89b-42d3-a456-426614174063",
            50,
        )
        .unwrap()
        .is_none());
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
    fn every_accept_edit_reject_skip_pair_has_the_exact_consensus_semantics() {
        let actions = ["accept", "edit", "reject", "skip"];
        for first in actions {
            for second in actions {
                let mut all = HashMap::new();
                for (reviewer, action, evidence_id) in [("Rubar", first, "pair:first"), ("Alle", second, "pair:second")]
                {
                    let transcript = matches!(action, "accept" | "edit").then(|| "دەقی یەکسان".to_string());
                    insert_judgement(
                        &mut all,
                        "clip".to_string(),
                        reviewer.to_string(),
                        evidence_id.to_string(),
                        action.to_string(),
                        transcript,
                    )
                    .unwrap();
                }
                let reviewers = all.get("clip").unwrap();
                assert_eq!(reviewers.seen.len(), 2, "{first}+{second} must mark both reviewers seen");
                let (resolution, _) = derive_resolution("clip", Some(reviewers), None);
                let judgement_count = usize::from(first != "skip") + usize::from(second != "skip");
                if judgement_count < 2 {
                    assert!(
                        matches!(resolution, DerivedResolution::Pending),
                        "{first}+{second} must remain pending because skip contributes no judgement"
                    );
                    continue;
                }
                let both_retain = matches!(first, "accept" | "edit") && matches!(second, "accept" | "edit");
                let both_reject = first == "reject" && second == "reject";
                if both_retain || both_reject {
                    assert!(
                        matches!(resolution, DerivedResolution::Resolved { owner: false, .. }),
                        "{first}+{second} must resolve as the same semantic outcome"
                    );
                } else {
                    assert!(
                        matches!(resolution, DerivedResolution::NeedsThird),
                        "{first}+{second} must admit exactly one blinded third judgement"
                    );
                }
            }
        }
    }

    #[test]
    fn reviewer_identity_is_distinct_and_case_trim_normalized() {
        let mut all = HashMap::new();
        insert_judgement(
            &mut all,
            "clip".to_string(),
            " Rubar ".to_string(),
            "first".to_string(),
            "accept".to_string(),
            Some("دەق".to_string()),
        )
        .unwrap();
        let error = insert_judgement(
            &mut all,
            "clip".to_string(),
            "RUBAR".to_string(),
            "second".to_string(),
            "edit".to_string(),
            Some("دەق".to_string()),
        )
        .unwrap_err();
        assert!(error.contains("duplicate effective evidence from one reviewer"), "unexpected refusal: {error}");
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

    fn dummy_pool() -> ReviewPool {
        ReviewPool {
            pool_id: "123e4567-e89b-42d3-a456-426614174900".to_string(),
            focus_segment_count: 1,
            focus_sha256: "a".repeat(64),
            review_segment_count: 1,
            excluded_duplicate_count: 0,
            duplicate_family_count: 0,
            dedup_manifest_sha256: None,
            champion_model_version_id: TEST_CHAMPION.to_string(),
            champion_deployment_sha256: "c".repeat(64),
            members: Arc::new(HashMap::new()),
            member_ids: Arc::new(HashSet::new()),
            audio_paths: Arc::new(HashMap::new()),
            playable_member_ids: Arc::new(HashSet::new()),
        }
    }

    fn mutated_manifest(base: &str, mutate: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut value: serde_json::Value = serde_json::from_str(base).unwrap();
        mutate(&mut value);
        value.as_object_mut().unwrap().remove("manifestSha256");
        let digest: String =
            Sha256::digest(canonical_json_bytes(&value).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect();
        value.as_object_mut().unwrap().insert("manifestSha256".into(), serde_json::Value::String(digest));
        String::from_utf8(canonical_json_bytes(&value).unwrap()).unwrap()
    }

    #[test]
    fn identity_normalization_and_outcome_parsing_are_exact() {
        assert_eq!(reviewer_key(None), DESKTOP_REVIEWER_KEY);
        assert_eq!(reviewer_key(Some("   ")), DESKTOP_REVIEWER_KEY);
        assert_eq!(reviewer_key(Some(" RuBar ")), "rubar");

        assert!(valid_lower_sha256(&"a".repeat(64)));
        assert!(!valid_lower_sha256(&"A".repeat(64)));
        assert!(!valid_lower_sha256(&"a".repeat(63)));
        assert!(!valid_lower_sha256(&"g".repeat(64)));

        let error = outcome_from_action("bogus", None).unwrap_err();
        assert_eq!(error, "unknown review-pool evidence action bogus");
        let error = outcome_from_action("accept", None).unwrap_err();
        assert_eq!(error, "retained review evidence has no non-blank verbatim transcript");
        let error = outcome_from_action("edit", Some("   ")).unwrap_err();
        assert_eq!(error, "retained review evidence has no non-blank verbatim transcript");
        assert_eq!(outcome_from_action("skip", None).unwrap(), None);
        assert_eq!(outcome_from_action("human_reject", None).unwrap(), Some(ReviewOutcome::Reject));
        let retained = outcome_from_action("human_accept", Some("  دەق  ")).unwrap().unwrap();
        assert_eq!(retained, ReviewOutcome::Retain("دەق".to_string()));
        assert_eq!(retained.final_action(), "retain");
        assert_eq!(retained.final_transcript(), Some("دەق"));
        assert_eq!(retained.digest_value(), "retain:دەق");
        assert_eq!(ReviewOutcome::Reject.final_action(), "reject");
        assert_eq!(ReviewOutcome::Reject.final_transcript(), None);
        assert_eq!(ReviewOutcome::Reject.digest_value(), "reject");
    }

    #[test]
    fn inactive_or_pre_schema_pools_refuse_every_authority_entry_point() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(load(&db).unwrap().is_none());
        assert!(exportable_segment_ids(&db).unwrap().is_none());
        assert_eq!(dedup_status(&db).unwrap_err(), "review pool is not active");
        assert_eq!(coverage_by_voice(&db).unwrap_err(), "review pool is not active");
        assert_eq!(segment_resolutions(&db, None).unwrap_err(), "review pool is not active");
        assert_eq!(resolution_summary(&db).unwrap_err(), "review pool is not active");
        assert_eq!(rights_coverage(&db).unwrap_err(), "review pool is not active");
        assert_eq!(stamp_owner_supplied_pool_rights(&db).unwrap_err(), "review pool is not active");
        assert_eq!(consensus_resolved_segment_ids(&db).unwrap_err(), "review pool is not active");
        assert_eq!(voice_authority_digests(&db, "Lamo").unwrap_err(), "review pool is not active");
        assert!(voice_certificate(&db, "Lamo").unwrap().is_none());

        rollback_fixture_to(&db, 59);
        assert!(load(&db).unwrap().is_none(), "schema below 62 must read as no pool, never an error");
        assert!(exportable_segment_ids(&db).unwrap().is_none());
        assert!(!registry_matches(&db, &dummy_pool()).unwrap());
        assert_eq!(
            activate(&db, "123e4567-e89b-42d3-a456-426614174901", &[]).unwrap_err(),
            "flexible review pool requires schema 62 or newer"
        );
        assert_eq!(apply_dedup_manifest(&db, "{}").unwrap_err(), "review-pool duplicate exclusions require schema 64");
        assert_eq!(
            record_owner_adjudication(
                &db,
                &dummy_pool(),
                &OwnerAdjudicationInput {
                    segment_id: "clip",
                    final_action: "reject",
                    final_transcript: None,
                    operation_id: "123e4567-e89b-42d3-a456-426614174902",
                    created_at_ms: 1,
                },
            )
            .unwrap_err(),
            "owner adjudication requires review-pool schema 63"
        );
    }

    #[test]
    fn activation_refuses_each_invalid_request_and_member_shape_then_freezes_one_pool() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        rollback_fixture_to(&db, 59);
        for id in ["good1", "good2", "ph", "nohash", "nospan", "badspan"] {
            let audio = dir.path().join(format!("{id}.wav"));
            std::fs::write(&audio, b"wav").unwrap();
            db.insert_segment_full(&segment(id, &audio, None)).unwrap();
        }
        db.connection().execute("UPDATE speech_segments SET raw_transcript='[مۆسیقا]' WHERE id='ph'", []).unwrap();
        db.connection().execute("UPDATE speech_segments SET alignment_json=NULL WHERE id='nospan'", []).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET alignment_json='{\"source_start_ms\":500,\"source_end_ms\":500}'
                  WHERE id='badspan'",
                [],
            )
            .unwrap();
        upgrade_fixture_from(&db, 59);
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?1
                  WHERE id IN ('good1','good2','ph','nospan','badspan')",
                ["a".repeat(64)],
            )
            .unwrap();

        let pool_id = "123e4567-e89b-42d3-a456-426614174910";
        let member = |segment_id: &str| PoolMemberInput { segment_id: segment_id.into(), voice_name: "Lamo".into() };
        assert_eq!(
            activate(&db, "not-a-uuid", &[member("good1")]).unwrap_err(),
            "review pool id must be a canonical UUID"
        );
        assert_eq!(
            activate(&db, "123E4567-E89B-42D3-A456-426614174910", &[member("good1")]).unwrap_err(),
            "review pool id must be a lowercase hyphenated UUID"
        );
        assert_eq!(
            activate(&db, pool_id, &[PoolMemberInput { segment_id: "  ".into(), voice_name: "Lamo".into() }])
                .unwrap_err(),
            "review pool member has an invalid segment id or voice name"
        );
        assert_eq!(
            activate(&db, pool_id, &[PoolMemberInput { segment_id: "good1".into(), voice_name: " ".into() }])
                .unwrap_err(),
            "review pool member has an invalid segment id or voice name"
        );
        assert_eq!(
            activate(&db, pool_id, &[PoolMemberInput { segment_id: "good1".into(), voice_name: "v".repeat(81) }])
                .unwrap_err(),
            "review pool member has an invalid segment id or voice name"
        );
        assert_eq!(
            activate(
                &db,
                pool_id,
                &[member("good1"), PoolMemberInput { segment_id: "good1".into(), voice_name: "Kawa".into() }],
            )
            .unwrap_err(),
            "segment good1 is assigned to two voice characters"
        );
        assert_eq!(activate(&db, pool_id, &[member("ghost")]).unwrap_err(), "review pool segment ghost does not exist");
        assert_eq!(
            activate(&db, pool_id, &[member("ph")]).unwrap_err(),
            "review pool segment ph has no usable champion transcript"
        );
        assert_eq!(
            activate(&db, pool_id, &[member("nohash")]).unwrap_err(),
            "review pool segment nohash has no canonical audio-content hash"
        );
        assert_eq!(
            activate(&db, pool_id, &[member("nospan")]).unwrap_err(),
            "review pool segment nospan has no canonical source span"
        );
        assert_eq!(
            activate(&db, pool_id, &[member("badspan")]).unwrap_err(),
            "review pool segment badspan has invalid audio timing evidence"
        );
        let window = activate(&db, pool_id, &[member("good1"), member("good2")]).unwrap_err();
        assert!(window.contains("are the same canonical audio window"), "unexpected refusal: {window}");
        assert_eq!(activate(&db, pool_id, &[]).unwrap_err(), "review pool must contain at least one clip");
        assert!(load(&db).unwrap().is_none(), "every refusal above must leave no pool behind");

        // A repeated member row with the SAME voice is not a conflict; the frozen pool holds one clip.
        let pool = activate(&db, pool_id, &[member("good1"), member("good1")]).unwrap();
        assert_eq!(pool.focus_segment_count, 1);
        assert_eq!(pool.voice_for("good1"), Some("Lamo"));

        let replay = activate(&db, pool_id, &[member("good1")]).unwrap();
        assert_eq!(replay.pool_id, pool.pool_id);
        assert_eq!(replay.focus_sha256, pool.focus_sha256);
        assert_eq!(
            activate(&db, pool_id, &[PoolMemberInput { segment_id: "good1".into(), voice_name: "Kawa".into() }])
                .unwrap_err(),
            "a different immutable review pool is already active"
        );
        assert_eq!(
            activate(&db, "123e4567-e89b-42d3-a456-426614174911", &[member("good1")]).unwrap_err(),
            "a different immutable review pool is already active"
        );
    }

    #[test]
    fn registry_matches_rejects_every_drifted_binding_field() {
        let (_dir, db, pool) = one_clip_pool("دەقی دروست");
        assert!(registry_matches(&db, &pool).unwrap());

        let mut drifted = pool.clone();
        drifted.pool_id = "123e4567-e89b-42d3-a456-426614174999".to_string();
        assert!(!registry_matches(&db, &drifted).unwrap());
        let mut drifted = pool.clone();
        drifted.focus_segment_count = 2;
        assert!(!registry_matches(&db, &drifted).unwrap());
        let mut drifted = pool.clone();
        drifted.focus_sha256 = "f".repeat(64);
        assert!(!registry_matches(&db, &drifted).unwrap());
        let mut drifted = pool.clone();
        drifted.champion_model_version_id = "omniasr-7b-other".to_string();
        assert!(!registry_matches(&db, &drifted).unwrap());
        let mut drifted = pool.clone();
        drifted.champion_deployment_sha256 = "f".repeat(64);
        assert!(!registry_matches(&db, &drifted).unwrap());
        let mut drifted = pool.clone();
        drifted.review_segment_count = 0;
        assert!(!registry_matches(&db, &drifted).unwrap());
        let mut drifted = pool.clone();
        drifted.excluded_duplicate_count = 1;
        assert!(!registry_matches(&db, &drifted).unwrap());
        let mut drifted = pool.clone();
        drifted.dedup_manifest_sha256 = Some("f".repeat(64));
        assert!(!registry_matches(&db, &drifted).unwrap());

        let mut drifted = pool.clone();
        drifted.champion_model_version_id = "omniasr-7b-other".to_string();
        assert_eq!(
            pending_segment_ids(&db, &drifted, "Alle", None).unwrap_err(),
            "review pool registry or OmniASR-7B champion identity changed after Start"
        );
    }

    #[test]
    fn verify_audio_available_requires_a_bound_path() {
        let (_dir, db, pool) = one_clip_pool("دەقی دروست");
        pool.verify_audio_available("clip").unwrap();
        assert_eq!(pool.verify_audio_available("ghost").unwrap_err(), "review pool clip ghost has no bound audio path");
        drop(db);
    }

    #[test]
    fn queue_fails_closed_for_restricted_reviewers_and_unplayable_audio() {
        let (dir, db, pool) = one_clip_pool("دەقی دروست");
        assert_eq!(pending_segment_ids(&db, &pool, "Alle", None).unwrap(), vec!["clip"]);
        // The temp-dir source is UNMAPPED, and an unmapped source plus a restricted reviewer must
        // serve NOTHING (fail closed), never guess a dialect.
        assert!(pending_segment_ids(&db, &pool, "Roza", Some(&["hawleri".to_string()])).unwrap().is_empty());
        assert!(pending_segment_ids(&db, &pool, "Roza", Some(&[])).unwrap().is_empty());

        std::fs::remove_file(dir.path().join("clip.wav")).unwrap();
        let reloaded = load(&db).unwrap().unwrap();
        assert!(
            pending_segment_ids(&db, &reloaded, "Alle", None).unwrap().is_empty(),
            "a clip whose audio vanished before Start must never be served"
        );
    }

    #[test]
    fn record_decision_refuses_invalid_identity_actions_and_stale_evidence() {
        let (_dir, db, pool) = one_clip_pool("دەقی یەک");
        let (_, revision) = db.get_segment_by_id_with_revision("clip").unwrap().unwrap();
        let hash = clip_hash("a");
        let payload = "b".repeat(64);
        let base = PoolDecisionInput {
            segment_id: "clip",
            reviewer: "Alle",
            action: "edit",
            submitted_transcript: Some("دەقی دوو"),
            served_transcript: "دەقی چامپیۆن",
            served_revision: revision,
            audio_content_hash: Some(&hash),
            source_start_ms: Some(0),
            source_end_ms: Some(1_000),
            duration_ms: 1_000,
            requested_action: "edit",
            requested_transcript: "دەقی دوو",
            operation_id: "123e4567-e89b-42d3-a456-426614174920",
            operation_payload_hash: &payload,
            created_at_ms: 9,
            playback_authority_session_id: None,
        };

        let mut input = base.clone();
        input.segment_id = "ghost";
        assert_eq!(record_decision(&db, &pool, &input).unwrap_err(), "decision is outside the active review pool");
        let mut input = base.clone();
        input.operation_id = "nope";
        assert_eq!(
            record_decision(&db, &pool, &input).unwrap_err(),
            "review pool decision operation id must be a canonical UUID"
        );
        for mutate in [
            &(|input: &mut PoolDecisionInput| input.operation_payload_hash = "XYZ") as &dyn Fn(&mut PoolDecisionInput),
            &|input: &mut PoolDecisionInput| input.created_at_ms = 0,
            &|input: &mut PoolDecisionInput| input.served_revision = -1,
            &|input: &mut PoolDecisionInput| input.duration_ms = 0,
            &|input: &mut PoolDecisionInput| input.reviewer = "   ",
        ] {
            let mut input = base.clone();
            mutate(&mut input);
            assert_eq!(
                record_decision(&db, &pool, &input).unwrap_err(),
                "review pool decision contains invalid identity or timing evidence"
            );
        }
        for (action, transcript) in [
            ("accept", None),
            ("edit", None),
            ("edit", Some("   ")),
            ("reject", Some("دەق")),
            ("skip", Some("دەق")),
            ("approve", Some("دەق")),
        ] {
            let mut input = base.clone();
            input.action = action;
            input.submitted_transcript = transcript;
            assert_eq!(
                record_decision(&db, &pool, &input).unwrap_err(),
                "review pool decision action/transcript is invalid",
                "action {action} with transcript {transcript:?} must be refused"
            );
        }

        // Stale serving evidence is a semantic no-op (Ok(None)), never a partial write.
        let mut input = base.clone();
        input.served_transcript = "دەقی جیاواز";
        assert!(record_decision(&db, &pool, &input).unwrap().is_none());
        let mut input = base.clone();
        input.served_revision = revision + 5;
        assert!(record_decision(&db, &pool, &input).unwrap().is_none());
        let recorded: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM review_pool_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(recorded, 0, "refused and stale decisions must leave the ledger empty");

        // The canonical first reviewer already judged this clip under any spelling of their name.
        let mut input = base.clone();
        input.reviewer = "  rubar  ";
        let error = record_decision(&db, &pool, &input).unwrap_err();
        assert!(error.contains("review pool decision is duplicated for this reviewer"), "unexpected refusal: {error}");

        decide(&db, &pool, "Alle", "دەقی دوو", "123e4567-e89b-42d3-a456-426614174921", 10);
        let mut input = base.clone();
        input.operation_id = "123e4567-e89b-42d3-a456-426614174922";
        let error = record_decision(&db, &pool, &input).unwrap_err();
        assert!(error.contains("review pool decision is duplicated for this reviewer"), "unexpected refusal: {error}");

        decide(&db, &pool, "Sewa", "دەقی سێ", "123e4567-e89b-42d3-a456-426614174923", 11);
        let mut input = base.clone();
        input.reviewer = "Roza";
        input.operation_id = "123e4567-e89b-42d3-a456-426614174924";
        let error = record_decision(&db, &pool, &input).unwrap_err();
        assert!(error.contains("review pool clip requires owner adjudication"), "unexpected refusal: {error}");

        let (_dir, db, pool) = one_clip_pool("دەقی یەک");
        decide(&db, &pool, "Alle", "دەقی یەک", "123e4567-e89b-42d3-a456-426614174925", 12);
        let (_, revision) = db.get_segment_by_id_with_revision("clip").unwrap().unwrap();
        let mut input = base.clone();
        input.reviewer = "Sewa";
        input.served_revision = revision;
        input.operation_id = "123e4567-e89b-42d3-a456-426614174926";
        let error = record_decision(&db, &pool, &input).unwrap_err();
        assert!(error.contains("review pool clip is already resolved"), "unexpected refusal: {error}");
    }

    #[test]
    fn owner_adjudication_refusals_and_replay_are_exact() {
        let (_dir, db, pool) = one_clip_pool("دەقی یەکەم");
        let adjudicate = |segment_id: &str, action: &str, transcript: Option<&str>, operation_id: &str, at: i64| {
            record_owner_adjudication(
                &db,
                &pool,
                &OwnerAdjudicationInput {
                    segment_id,
                    final_action: action,
                    final_transcript: transcript,
                    operation_id,
                    created_at_ms: at,
                },
            )
        };
        let retain_op = "123e4567-e89b-42d3-a456-426614174930";
        assert_eq!(
            adjudicate("ghost", "reject", None, retain_op, 1).unwrap_err(),
            "owner adjudication is outside the active review pool"
        );
        assert_eq!(
            adjudicate("clip", "reject", None, "nope", 1).unwrap_err(),
            "owner adjudication operation id must be a canonical UUID"
        );
        assert_eq!(
            adjudicate("clip", "reject", None, retain_op, 0).unwrap_err(),
            "owner adjudication timestamp is invalid"
        );
        assert_eq!(
            adjudicate("clip", "retain", None, retain_op, 1).unwrap_err(),
            "retained owner adjudication requires a transcript"
        );
        assert_eq!(
            adjudicate("clip", "retain", Some("   "), retain_op, 1).unwrap_err(),
            "retained owner adjudication requires a transcript"
        );
        assert_eq!(
            adjudicate("clip", "reject", Some("دەق"), retain_op, 1).unwrap_err(),
            "owner adjudication must be retain+text or reject without text"
        );
        assert_eq!(
            adjudicate("clip", "erase", None, retain_op, 1).unwrap_err(),
            "owner adjudication must be retain+text or reject without text"
        );
        // Only three distinct outcomes may summon the owner; one canonical opinion is Pending.
        assert_eq!(
            adjudicate("clip", "reject", None, retain_op, 1).unwrap_err(),
            "owner adjudication is allowed only after three distinct outcomes"
        );

        decide(&db, &pool, "Alle", "دەقی دووەم", "123e4567-e89b-42d3-a456-426614174931", 1);
        decide(&db, &pool, "Sewa", "دەقی سێیەم", "123e4567-e89b-42d3-a456-426614174932", 2);
        let id = adjudicate("clip", "retain", Some("دەقی یەکەم"), retain_op, 3).unwrap();
        assert_eq!(
            adjudicate("clip", "retain", Some("  دەقی یەکەم  "), retain_op, 4).unwrap(),
            id,
            "the exact replayed outcome must return the original receipt"
        );
        assert_eq!(
            adjudicate("clip", "reject", None, retain_op, 4).unwrap_err(),
            "owner adjudication operation id is already bound to another outcome"
        );
        // The clip is now ownerResolved, so a fresh operation id finds no conflict to settle.
        assert_eq!(
            adjudicate("clip", "reject", None, "123e4567-e89b-42d3-a456-426614174933", 5).unwrap_err(),
            "owner adjudication is allowed only after three distinct outcomes"
        );
        assert_eq!(segment_resolutions(&db, None).unwrap()[0].status, "ownerResolved");
    }

    #[test]
    fn corrupted_owner_adjudication_rows_fail_closed_at_read_time() {
        // The insert trigger normally makes these rows unwritable; dropping it models low-level
        // corruption and proves the reader refuses instead of trusting the row.
        let (_dir, db, pool) = one_clip_pool("دەقی یەکەم");
        db.connection().execute("DROP TRIGGER review_pool_owner_adjudication_validate_insert", []).unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_pool_owner_adjudications
                 (pool_id, segment_id, final_action, final_transcript, evidence_sha256,
                  operation_id, app_git_sha, created_at_ms)
                 VALUES(?1, 'clip', 'retain', NULL, ?2, '123e4567-e89b-42d3-a456-426614174940', ?3, 1)",
                rusqlite::params![pool.pool_id, "a".repeat(64), "a".repeat(40)],
            )
            .unwrap();
        assert_eq!(
            segment_resolutions(&db, None).unwrap_err(),
            "owner adjudication for clip has no retained transcript"
        );
        db.connection()
            .execute(
                "INSERT INTO review_pool_owner_adjudications
                 (pool_id, segment_id, final_action, final_transcript, evidence_sha256,
                  operation_id, app_git_sha, created_at_ms)
                 VALUES(?1, 'clip', 'reject', 'دەق', ?2, '123e4567-e89b-42d3-a456-426614174941', ?3, 2)",
                rusqlite::params![pool.pool_id, "b".repeat(64), "a".repeat(40)],
            )
            .unwrap();
        assert_eq!(
            segment_resolutions(&db, None).unwrap_err(),
            "owner adjudication for clip has invalid outcome evidence"
        );
    }

    #[test]
    fn reversal_identity_is_durable_and_reviewer_bound() {
        let (_dir, db, pool) = one_clip_pool("دەقی یەکەم");
        let inserted = decide(&db, &pool, "Alle", "دەقی دووەم", "123e4567-e89b-42d3-a456-426614174950", 1);
        assert_eq!(
            reverse_decision(&db, &pool, inserted, "Alle", "nope", 2).unwrap_err(),
            "review pool reversal operation id must be a canonical UUID"
        );
        let reversal_op = "123e4567-e89b-42d3-a456-426614174951";
        assert_eq!(
            reverse_decision(&db, &pool, inserted, "Sewa", reversal_op, 2).unwrap_err(),
            "review pool reversal target is missing or belongs to another reviewer"
        );
        assert_eq!(
            reverse_decision(&db, &pool, inserted + 99, "Alle", reversal_op, 2).unwrap_err(),
            "review pool reversal target is missing or belongs to another reviewer"
        );
        reverse_decision(&db, &pool, inserted, "Alle", reversal_op, 2).unwrap();
        reverse_decision(&db, &pool, inserted, "ALLE", reversal_op, 3).unwrap();
        assert_eq!(
            reverse_decision(&db, &pool, inserted, "Alle", "123e4567-e89b-42d3-a456-426614174952", 4).unwrap_err(),
            "review pool decision already has another reversal identity"
        );
        let reversals: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM review_pool_reversals", [], |row| row.get(0)).unwrap();
        assert_eq!(reversals, 1, "a replay must never mint a second reversal row");
    }

    #[test]
    fn addressed_reversal_refuses_mismatched_coordinates_without_erring() {
        let (_dir, db, pool) = one_clip_pool("دەقی یەکەم");
        let decision_op = "123e4567-e89b-42d3-a456-426614174960";
        let inserted = decide(&db, &pool, "Alle", "دەقی دووەم", decision_op, 1);
        let reversal_op = "123e4567-e89b-42d3-a456-426614174961";
        assert!(reverse_decision_addressed(&db, &pool, 0, "Alle", decision_op, reversal_op, 2).unwrap().is_none());
        assert!(reverse_decision_addressed(&db, &pool, inserted, "Alle", decision_op, reversal_op, 0)
            .unwrap()
            .is_none());
        assert!(reverse_decision_addressed(&db, &pool, inserted, "Alle", decision_op, decision_op, 2)
            .unwrap()
            .is_none());
        assert!(reverse_decision_addressed(&db, &pool, inserted, "Sewa", decision_op, reversal_op, 2)
            .unwrap()
            .is_none());
        assert!(reverse_decision_addressed(
            &db,
            &pool,
            inserted,
            "Alle",
            "123e4567-e89b-42d3-a456-426614174962",
            reversal_op,
            2,
        )
        .unwrap()
        .is_none());
        assert_eq!(
            reverse_decision_addressed(&db, &pool, inserted, "Alle", "bad", reversal_op, 2).unwrap_err(),
            "review pool decision operation id must be a canonical UUID"
        );
        assert_eq!(
            reverse_decision_addressed(&db, &pool, inserted, "Alle", decision_op, "bad", 2).unwrap_err(),
            "review pool reversal operation id must be a canonical UUID"
        );
        assert_eq!(
            reverse_decision_addressed(&db, &pool, inserted, "Alle", decision_op, reversal_op, 2).unwrap().as_deref(),
            Some("clip")
        );
        // A retry that names a DIFFERENT reversal identity for the same decision is a conflict.
        assert!(reverse_decision_addressed(
            &db,
            &pool,
            inserted,
            "Alle",
            decision_op,
            "123e4567-e89b-42d3-a456-426614174963",
            3,
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn operation_receipts_and_reviewer_visibility_are_exact() {
        let (_dir, db, pool) = one_clip_pool("دەقی یەکەم");
        let operation_id = "123e4567-e89b-42d3-a456-426614174970";
        assert!(operation(&db, operation_id).unwrap().is_none());
        assert!(reviewer_already_saw(&db, "clip", "Rubar").unwrap(), "the canonical first review counts as seen");
        assert!(reviewer_already_saw(&db, "clip", "  RUBAR  ").unwrap());
        assert!(!reviewer_already_saw(&db, "clip", "Alle").unwrap());

        let inserted = decide(&db, &pool, "Alle", "دەقی دووەم", operation_id, 5);
        let receipt = operation(&db, operation_id).unwrap().unwrap();
        assert_eq!(
            receipt,
            PoolOperationReceipt {
                decision_id: inserted,
                pool_id: pool.pool_id.clone(),
                segment_id: "clip".to_string(),
                reviewer: "Alle".to_string(),
                operation_payload_hash: "b".repeat(64),
            }
        );
        assert!(reviewer_already_saw(&db, "clip", "alle").unwrap());
        assert!(!reviewer_already_saw(&db, "clip", "Roza").unwrap());
        assert!(!reviewer_already_saw(&db, "ghost", "Rubar").unwrap());
    }

    #[test]
    fn coverage_buckets_and_resolution_summary_track_every_state() {
        let (_dir, db, pool) = two_clip_pool(None);
        let coverage = |db: &Database| coverage_by_voice(db).unwrap().pop().unwrap();
        assert_eq!(
            coverage(&db),
            VoiceCoverage {
                voice_name: "Lamo".to_string(),
                total_clips: 2,
                zero_reviews: 2,
                one_review: 0,
                two_reviews: 0,
                three_or_more_reviews: 0,
                resolved: 0,
                needs_third_review: 0,
                owner_conflicts: 0,
            }
        );
        assert_eq!(
            resolution_summary(&db).unwrap(),
            PoolResolutionSummary {
                total_clips: 2,
                resolved_clips: 0,
                needs_first_or_second_review: 2,
                needs_third_review: 0,
                owner_conflicts: 0,
            }
        );

        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET verified=1, human_decision='edit', verdict='human_edit',
                        verdict_transcript='دەقی دروست', annotated_transcript='دەقی دروست',
                        reviewed_by='Rubar', review_revision=review_revision+1
                  WHERE id IN ('a','b')",
                [],
            )
            .unwrap();
        let after_first = coverage(&db);
        assert_eq!((after_first.one_review, after_first.zero_reviews), (2, 0));

        let decide_on = |segment_id: &str, reviewer: &str, text: &str, operation_id: &str, at: i64| {
            let (_, revision) = db.get_segment_by_id_with_revision(segment_id).unwrap().unwrap();
            let audio_hash = clip_hash(segment_id);
            let authority = authority(&db, reviewer, segment_id);
            record_decision(
                &db,
                &pool,
                &PoolDecisionInput {
                    segment_id,
                    reviewer,
                    action: "edit",
                    submitted_transcript: Some(text),
                    served_transcript: "دەقی چامپیۆن",
                    served_revision: revision,
                    audio_content_hash: Some(&audio_hash),
                    source_start_ms: Some(0),
                    source_end_ms: Some(1_000),
                    duration_ms: 1_000,
                    requested_action: "edit",
                    requested_transcript: text,
                    operation_id,
                    operation_payload_hash: &"e".repeat(64),
                    created_at_ms: at,
                    playback_authority_session_id: Some(&authority),
                },
            )
            .unwrap()
            .unwrap()
        };
        decide_on("a", "Alle", "دەقی دروست", "123e4567-e89b-42d3-a456-426614174980", 1);
        decide_on("b", "Alle", "دەقی جیاواز", "123e4567-e89b-42d3-a456-426614174981", 2);
        assert_eq!(
            coverage(&db),
            VoiceCoverage {
                voice_name: "Lamo".to_string(),
                total_clips: 2,
                zero_reviews: 0,
                one_review: 0,
                two_reviews: 2,
                three_or_more_reviews: 0,
                resolved: 1,
                needs_third_review: 1,
                owner_conflicts: 0,
            }
        );
        assert_eq!(
            resolution_summary(&db).unwrap(),
            PoolResolutionSummary {
                total_clips: 2,
                resolved_clips: 1,
                needs_first_or_second_review: 0,
                needs_third_review: 1,
                owner_conflicts: 0,
            }
        );
        let resolved =
            segment_resolutions(&db, Some("Lamo")).unwrap().into_iter().find(|row| row.segment_id == "a").unwrap();
        assert_eq!(resolved.status, "resolved");
        assert_eq!(resolved.final_action.as_deref(), Some("retain"));
        assert_eq!(resolved.final_transcript.as_deref(), Some("دەقی دروست"));
        assert_eq!(resolved.agreeing_reviewers, vec!["Alle", "Rubar"]);

        decide_on("b", "Sewa", "دەقی سێیەم", "123e4567-e89b-42d3-a456-426614174982", 3);
        assert_eq!(
            coverage(&db),
            VoiceCoverage {
                voice_name: "Lamo".to_string(),
                total_clips: 2,
                zero_reviews: 0,
                one_review: 0,
                two_reviews: 1,
                three_or_more_reviews: 1,
                resolved: 1,
                needs_third_review: 0,
                owner_conflicts: 1,
            }
        );
        assert_eq!(
            resolution_summary(&db).unwrap(),
            PoolResolutionSummary {
                total_clips: 2,
                resolved_clips: 1,
                needs_first_or_second_review: 0,
                needs_third_review: 0,
                owner_conflicts: 1,
            }
        );
    }

    #[test]
    fn voice_authority_digests_bind_to_evidence_and_refuse_unknown_voices() {
        let (_dir, db, pool) = one_clip_pool("دەقی دروست");
        assert_eq!(voice_authority_digests(&db, "   ").unwrap_err(), "voice name cannot be blank");
        assert_eq!(voice_authority_digests(&db, "Kawa").unwrap_err(), "active review pool has no voice named Kawa");

        let before = voice_authority_digests(&db, "Lamo").unwrap();
        assert_eq!(before.voice_name, "Lamo");
        assert_eq!(before.segment_count, 1);
        assert_eq!(before.resolution_sha256.len(), 64);
        assert_eq!(before.reviewer_sha256.len(), 64);
        assert_eq!(voice_authority_digests(&db, "  Lamo  ").unwrap(), before, "the voice label is trimmed");
        assert_eq!(
            segment_resolutions(&db, Some("  ")).unwrap().len(),
            segment_resolutions(&db, None).unwrap().len(),
            "a blank voice filter means every voice"
        );

        decide(&db, &pool, "Alle", "دەقی جیاواز", "123e4567-e89b-42d3-a456-426614174990", 1);
        let after = voice_authority_digests(&db, "Lamo").unwrap();
        assert_ne!(after.resolution_sha256, before.resolution_sha256, "new evidence must move the resolution digest");
        assert_ne!(after.reviewer_sha256, before.reviewer_sha256, "new evidence must move the reviewer digest");
    }

    #[test]
    fn voice_certificate_input_validation_is_exact_and_pool_authority_bound() {
        let (_dir, db, _pool) = one_clip_pool("دەقی دروست");
        let digest = "a".repeat(64);
        let base = VoiceCertificateInput {
            voice_name: "Lamo",
            resolution_sha256: &digest,
            rights_sha256: &digest,
            audio_sha256: &digest,
            reviewer_sha256: &digest,
            export_manifest_sha256: &digest,
            export_sha256sums_sha256: &digest,
            certificate_json: "{}",
            certificate_sha256: &digest,
            retained_segments: 1,
            rejected_segments: 0,
            total_duration_ms: 0,
            created_at_ms: 1,
        };
        let identity_refusal = "voice certificate has invalid identity, duration, timestamp, or build provenance";
        let mut input = base.clone();
        input.voice_name = "   ";
        assert_eq!(record_voice_certificate(&db, &input).unwrap_err(), identity_refusal);
        let mut input = base.clone();
        input.total_duration_ms = -1;
        assert_eq!(record_voice_certificate(&db, &input).unwrap_err(), identity_refusal);
        let mut input = base.clone();
        input.created_at_ms = 0;
        assert_eq!(record_voice_certificate(&db, &input).unwrap_err(), identity_refusal);

        let bad = "not-a-digest";
        for (label, mutate) in [
            (
                "resolution",
                &(|input: &mut VoiceCertificateInput| input.resolution_sha256 = bad)
                    as &dyn Fn(&mut VoiceCertificateInput),
            ),
            ("rights", &|input: &mut VoiceCertificateInput| input.rights_sha256 = bad),
            ("audio", &|input: &mut VoiceCertificateInput| input.audio_sha256 = bad),
            ("reviewer", &|input: &mut VoiceCertificateInput| input.reviewer_sha256 = bad),
            ("export manifest", &|input: &mut VoiceCertificateInput| input.export_manifest_sha256 = bad),
            ("export checksums", &|input: &mut VoiceCertificateInput| input.export_sha256sums_sha256 = bad),
            ("certificate", &|input: &mut VoiceCertificateInput| input.certificate_sha256 = bad),
        ] {
            let mut input = base.clone();
            mutate(&mut input);
            assert_eq!(
                record_voice_certificate(&db, &input).unwrap_err(),
                format!("voice certificate {label} digest is invalid")
            );
        }

        let mut input = base.clone();
        input.certificate_json = "not json";
        assert!(record_voice_certificate(&db, &input).unwrap_err().starts_with("voice certificate JSON is invalid"));
        // Well-formed digests over a pool with NO applied dedup manifest can never certify.
        assert_eq!(
            record_voice_certificate(&db, &base).unwrap_err(),
            "voice certificate JSON does not match its complete v64 pool authority"
        );
        assert!(voice_certificate(&db, "Lamo").unwrap().is_none(), "every refusal above must record nothing");
    }

    #[test]
    fn dedup_manifest_refusal_arms_are_exact_and_write_nothing() {
        let (_dir, db, pool) = two_clip_pool(None);
        let base = dedup_manifest(&pool, "a", None, 1_000);
        let refuse = |json: &str| apply_dedup_manifest(&db, json).unwrap_err();

        assert!(refuse("{").starts_with("review-pool dedup manifest JSON is invalid"));
        assert_eq!(refuse("{}"), "review-pool dedup manifest has no valid payload digest");
        let mut tampered: serde_json::Value = serde_json::from_str(&base).unwrap();
        tampered["generatedAtMs"] = serde_json::json!(2_000);
        assert_eq!(
            refuse(&serde_json::to_string(&tampered).unwrap()),
            "review-pool dedup manifest payload does not match its digest"
        );
        let unknown_field = mutated_manifest(&base, |value| {
            value.as_object_mut().unwrap().insert("extra".into(), serde_json::json!(1));
        });
        assert!(refuse(&unknown_field).starts_with("review-pool dedup manifest contract is invalid"));

        let canon = "review-pool dedup manifest does not match the frozen pool or algorithm canon";
        assert_eq!(refuse(&mutated_manifest(&base, |value| value["manifestSchema"] = serde_json::json!(2))), canon);
        assert_eq!(
            refuse(&mutated_manifest(&base, |value| value["algorithm"]["id"] = serde_json::json!("other-algo"))),
            canon
        );
        assert_eq!(
            refuse(&mutated_manifest(&base, |value| value["pool"]["poolId"] =
                serde_json::json!("123e4567-e89b-42d3-a456-426614174999"))),
            canon
        );
        assert_eq!(
            refuse(&mutated_manifest(&base, |value| value["summary"]["canonicalMembers"] = serde_json::json!(2))),
            canon
        );

        let cardinality = refuse(&mutated_manifest(&base, |value| {
            let members = value["families"][0]["members"].as_array_mut().unwrap();
            members.truncate(1);
        }));
        assert_eq!(cardinality, "review-pool dedup family has invalid identity or cardinality");
        assert_eq!(
            refuse(&mutated_manifest(&base, |value| value["families"][0]["voiceName"] = serde_json::json!("  "))),
            "review-pool dedup family has invalid identity or cardinality"
        );
        let ambiguous = refuse(&mutated_manifest(&base, |value| {
            value["families"][0]["members"][1]["canonical"] = serde_json::json!(true);
        }));
        assert!(ambiguous.contains("has ambiguous canonical membership"), "unexpected refusal: {ambiguous}");
        assert_eq!(
            refuse(&mutated_manifest(&base, |value| {
                value["families"][0]["members"][1]["segmentId"] = serde_json::json!("z");
            })),
            "dedup member z is outside the active source pool"
        );
        assert_eq!(
            refuse(&mutated_manifest(&base, |value| {
                value["families"][0]["members"][1]["audioContentHash"] = serde_json::json!("f".repeat(64));
            })),
            "dedup member b does not match frozen pool evidence"
        );
        let unranked = refuse(&dedup_manifest(&pool, "b", None, 1_000));
        assert!(unranked.contains("canonical selection is not deterministic"), "unexpected refusal: {unranked}");

        let unsorted = refuse(&mutated_manifest(&base, |value| {
            value["families"][0]["proofEdges"] = serde_json::json!([
                {"leftSegmentId": "b", "rightSegmentId": "a", "correlationPpm": 1_000_000},
                {"leftSegmentId": "a", "rightSegmentId": "b", "correlationPpm": 1_000_000},
            ]);
        }));
        assert!(unsorted.contains("proof edges are not canonical-order"), "unexpected refusal: {unsorted}");
        let self_edge = refuse(&mutated_manifest(&base, |value| {
            value["families"][0]["proofEdges"] =
                serde_json::json!([{"leftSegmentId": "a", "rightSegmentId": "a", "correlationPpm": 1_000_000}]);
        }));
        assert!(self_edge.contains("has invalid waveform proof"), "unexpected refusal: {self_edge}");
        let weak_edge = refuse(&mutated_manifest(&base, |value| {
            value["families"][0]["proofEdges"] =
                serde_json::json!([{"leftSegmentId": "a", "rightSegmentId": "b", "correlationPpm": 979_999}]);
        }));
        assert!(weak_edge.contains("has invalid waveform proof"), "unexpected refusal: {weak_edge}");
        let disconnected = refuse(&mutated_manifest(&base, |value| {
            value["families"][0]["proofEdges"] = serde_json::json!([]);
        }));
        assert!(disconnected.contains("waveform proof is disconnected"), "unexpected refusal: {disconnected}");
        let forged_family = refuse(&mutated_manifest(&base, |value| {
            value["families"][0]["familyId"] = serde_json::json!("f".repeat(64));
        }));
        assert!(forged_family.contains("does not match its proof digest"), "unexpected refusal: {forged_family}");
        assert_eq!(
            refuse(&mutated_manifest(&base, |value| {
                value["summary"]["reviewedCanonicalMembers"] = serde_json::json!(1);
            })),
            "review-pool dedup manifest summary does not match validated families"
        );
        assert_eq!(
            refuse(&mutated_manifest(&base, |value| {
                let family = value["families"][0].clone();
                value["families"] = serde_json::json!([family.clone(), family]);
                value["summary"]["duplicateFamilies"] = serde_json::json!(2);
            })),
            "review-pool dedup families contain duplicate segment membership"
        );

        let status = dedup_status(&db).unwrap();
        assert!(!status.applied, "every refusal above must leave the pool undeduplicated");
        assert_eq!(load(&db).unwrap().unwrap().review_segment_count, 2);

        // A certificate freezes the corpus: even a fully valid manifest is refused afterwards.
        db.connection()
            .execute(
                "INSERT INTO review_pool_voice_certificates
                 (pool_id, voice_name, resolution_sha256, rights_sha256, audio_sha256, reviewer_sha256,
                  export_manifest_sha256, export_sha256sums_sha256, certificate_json, certificate_sha256,
                  retained_segments, rejected_segments, total_duration_ms, app_git_sha, created_at_ms)
                 VALUES(?1, 'Lamo', ?2, ?2, ?2, ?2, ?2, ?2, '{}', ?3, 1, 0, 0, ?4, 1)",
                rusqlite::params![pool.pool_id, "a".repeat(64), "b".repeat(64), "a".repeat(40)],
            )
            .unwrap();
        assert_eq!(refuse(&base), "duplicate exclusions cannot be applied after a voice certificate exists");

        // A manifest for a pool that does not exist on THIS database is refused before any binding.
        let fresh = Database::open(":memory:").unwrap();
        fresh.initialize().unwrap();
        assert_eq!(apply_dedup_manifest(&fresh, &base).unwrap_err(), "review pool is not active");
    }

    #[test]
    fn dedup_manifest_refuses_to_retire_two_reviewed_clips() {
        let (_dir, db, pool) = two_clip_pool(None);
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET verified=1, human_decision='edit', verdict='human_edit',
                        verdict_transcript='دەقی دروست', annotated_transcript='دەقی دروست',
                        reviewed_by='Rubar', review_revision=review_revision+1
                  WHERE id IN ('a','b')",
                [],
            )
            .unwrap();
        let both_reviewed = mutated_manifest(&dedup_manifest(&pool, "a", None, 1_000), |value| {
            value["families"][0]["members"][0]["reviewEvidenceCount"] = serde_json::json!(1);
            value["families"][0]["members"][1]["reviewEvidenceCount"] = serde_json::json!(1);
        });
        let error = apply_dedup_manifest(&db, &both_reviewed).unwrap_err();
        assert!(error.contains("would retire more than one reviewed clip"), "unexpected refusal: {error}");
        assert!(!dedup_status(&db).unwrap().applied);
    }

    #[test]
    fn exportable_scope_is_membership_minus_proven_duplicates() {
        let fresh = Database::open(":memory:").unwrap();
        fresh.initialize().unwrap();
        assert!(exportable_segment_ids(&fresh).unwrap().is_none(), "no pool means untouched export scope");

        let (_dir, db, pool) = two_clip_pool(None);
        let full: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        assert_eq!(exportable_segment_ids(&db).unwrap().unwrap(), full);

        apply_dedup_manifest(&db, &dedup_manifest(&pool, "a", None, 1_000)).unwrap();
        let deduped: HashSet<String> = ["a".to_string()].into_iter().collect();
        assert_eq!(
            exportable_segment_ids(&db).unwrap().unwrap(),
            deduped,
            "a proven duplicate must leave export scope the moment it leaves review scope"
        );
    }

    /// Disk-backed sibling of `one_clip_pool` for tests that roll the schema back and forth: the
    /// canonical clip is already reviewed by Sara, exactly one voice, one recording.
    fn disk_one_clip_pool(pool_uuid: &str, first_text: &str) -> (tempfile::TempDir, Database, ReviewPool) {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("clip.wav");
        write_clip_wav(&audio, "a");
        let db = Database::open(dir.path().join("pool-fixture.db").to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        rollback_fixture_to(&db, 59);
        db.insert_segment_full(&reviewed_segment("clip", &audio, "Sara", first_text)).unwrap();
        upgrade_fixture_from(&db, 59);
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='clip'", [clip_hash("a")])
            .unwrap();
        let pool =
            activate(&db, pool_uuid, &[PoolMemberInput { segment_id: "clip".into(), voice_name: "Lamo".into() }])
                .unwrap();
        (dir, db, pool)
    }

    #[test]
    fn rights_coverage_buckets_track_every_rights_state_and_mint_a_digest_only_when_all_exact() {
        let (dir, db, _pool) = disk_one_clip_pool("123e4567-e89b-42d3-a456-4266141740a0", "دەقی دروست");
        let shared_source = dir.path().join("clip.wav");
        db.insert_segment_full(&segment("conflict-shadow", &shared_source, None)).unwrap();
        db.insert_segment_full(&segment("revoked-shadow", &shared_source, None)).unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET rights_license='third-party-license' WHERE id='conflict-shadow'", [])
            .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET rights_revoked_at='2026-08-30T00:00:00Z' WHERE id='revoked-shadow'",
                [],
            )
            .unwrap();

        let mixed = rights_coverage(&db).unwrap();
        assert_eq!(mixed.recordings, 1, "all three rows share one source recording");
        assert_eq!(mixed.segment_rows, 3);
        assert_eq!(mixed.unstamped_rows, 1, "the pool member itself is still blank");
        assert_eq!(mixed.conflicting_rows, 1);
        assert_eq!(mixed.revoked_rows, 1);
        assert_eq!(mixed.exact_rows, 0);
        assert!(!mixed.all_exact);
        assert!(mixed.rights_sha256.is_none(), "a mixed recording never earns a rights digest");

        db.connection()
            .execute("UPDATE speech_segments SET rights_license=NULL WHERE id='conflict-shadow'", [])
            .unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET rights_revoked_at=NULL WHERE id='revoked-shadow'", [])
            .unwrap();
        let report = stamp_owner_supplied_pool_rights(&db).unwrap();
        assert_eq!((report.recordings, report.segments), (1, 3));
        assert_eq!((report.stamped_recordings, report.already_exact_recordings), (1, 0));

        let exact = rights_coverage(&db).unwrap();
        assert_eq!(exact.exact_rows, 3);
        assert_eq!((exact.unstamped_rows, exact.conflicting_rows, exact.revoked_rows), (0, 0, 0));
        assert!(exact.all_exact);
        assert_eq!(
            exact.rights_sha256.as_deref(),
            Some(report.rights_sha256.as_str()),
            "the coverage digest and the stamping receipt are the same authority"
        );
    }

    #[test]
    fn schema_62_pool_revalidates_but_refuses_serving_and_63_only_authority() {
        let (_dir, db, pool) = disk_one_clip_pool("123e4567-e89b-42d3-a456-4266141740a1", "دەقی دروست");
        rollback_fixture_to(&db, 62);

        let reloaded = load(&db).unwrap().expect("a schema-62 pool is fully loadable");
        assert_eq!(reloaded, pool, "the pre-dedup load path reproduces the exact bound pool");
        assert!(registry_matches(&db, &pool).unwrap(), "the pre-dedup registry branch revalidates the bound pool");
        let mut drifted = pool.clone();
        drifted.dedup_manifest_sha256 = Some("b".repeat(64));
        assert!(!registry_matches(&db, &drifted).unwrap(), "schema 62 can never carry a dedup binding");
        let mut shrunk = pool.clone();
        shrunk.review_segment_count = 0;
        shrunk.excluded_duplicate_count = 1;
        assert!(!registry_matches(&db, &shrunk).unwrap());

        // The modern queue's duplicate-exclusion clause names a v64 table, so serving at schema 62
        // fails closed at prepare time — but only AFTER the pre-dedup registry proof above passed,
        // which is exactly the branch this schema window exists for.
        let queue_error = pending_segment_ids(&db, &pool, "Hemn", None).unwrap_err();
        assert!(queue_error.contains("review pool queue cannot be prepared"), "unexpected refusal: {queue_error}");
        pool.verify_audio_available("clip").unwrap();
        assert!(pool.segment_ids().contains("clip"));
        assert_eq!(pool.voice_for("clip"), Some("Lamo"));
        assert_eq!(pool.voice_for("ghost"), None);
        let resolutions = segment_resolutions(&db, None).unwrap();
        assert_eq!(resolutions[0].status, "pending", "one opinion stays pending with owner authority absent");
        assert_eq!(resolutions[0].reviewer_count, 1);

        assert_eq!(
            stamp_owner_supplied_pool_rights(&db).unwrap_err(),
            "owner rights stamping requires review-pool schema 63"
        );
        assert!(voice_certificate(&db, "Lamo").unwrap().is_none(), "certificates do not exist below schema 63");
        assert_eq!(
            record_owner_adjudication(
                &db,
                &pool,
                &OwnerAdjudicationInput {
                    segment_id: "clip",
                    final_action: "reject",
                    final_transcript: None,
                    operation_id: "123e4567-e89b-42d3-a456-4266141740a9",
                    created_at_ms: 1,
                },
            )
            .unwrap_err(),
            "owner adjudication requires review-pool schema 63"
        );
        assert!(latest_decision(&db, &pool.pool_id, "Hemn").unwrap().is_none());
        assert!(operation(&db, "123e4567-e89b-42d3-a456-4266141740aa").unwrap().is_none());
    }

    /// Two fail-closed arms of `load` nothing else reached. Both model low-level corruption — the
    /// immutability triggers make either shape unwritable through the app — and both must refuse to
    /// hand back a pool whose identity cannot be proven, rather than serve it.
    #[test]
    fn orphaned_pool_authority_and_off_champion_drafts_refuse_to_load() {
        let (_dir, db, pool) = one_clip_pool("دەقی دروست");
        const OTHER_DRAFT: &str = "omniasr-7b-test-other";
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: OTHER_DRAFT.to_string(),
                family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                model_card_name: Some("other draft".to_string()),
                checkpoint_sha256: "d".repeat(64),
                checkpoint_path: "/test/other.json".to_string(),
                source: "cortex-finetuned".to_string(),
                license: "owner-full-rights".to_string(),
            },
        )
        .unwrap();

        // The membership digest covers model_version_id, so a tampered member alone trips the
        // DIGEST arm first. Re-stamping the registry with the recomputed digest is what makes the
        // champion-scope arm the one that fires — it is the last line of defence, not the first.
        let mut drifted = (*pool.members).clone();
        drifted.get_mut("clip").unwrap().model_version_id = OTHER_DRAFT.to_string();
        let (count, digest) = member_evidence(&drifted).unwrap();
        db.connection()
            .execute_batch(
                "DROP TRIGGER review_pool_members_immutable_update;
             DROP TRIGGER review_pool_registry_immutable_update;
             DROP TRIGGER review_pool_registry_immutable_delete;",
            )
            .unwrap();
        db.connection().execute("UPDATE review_pool_members SET model_version_id=?1", [OTHER_DRAFT]).unwrap();
        db.connection()
            .execute(
                "UPDATE review_pool_registry SET focus_segment_count=?1, focus_sha256=?2",
                rusqlite::params![count as i64, digest],
            )
            .unwrap();
        let error = load(&db).unwrap_err();
        assert!(error.contains("draft from outside its frozen champion identity"), "{error}");

        // Authority rows that outlive their registry are not "no pool" — they are an unprovable one.
        db.connection().execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        db.connection().execute("DELETE FROM review_pool_registry", []).unwrap();
        let error = load(&db).unwrap_err();
        assert!(error.contains("without its immutable registry"), "{error}");
        assert!(
            exportable_segment_ids(&db).unwrap().is_none(),
            "the export scope reader must never widen when the registry is gone"
        );
    }

    /// OWNER CANON 2026-08-29: a sentence is decided by any two DIFFERENT reviewers, so the queue
    /// serves whatever is NEAREST a decision. All three live ranks in one queue, including the
    /// middle one nothing else pinned — a disagreeing pair still needs a third opinion, which makes
    /// it worth more than a member no one has judged and less than one already holding a single
    /// opinion.
    ///
    /// The canonical `reviewed_by` row IS judgement one (`reviewer_sets_on` reads it alongside the
    /// pool decisions), and `record_decision` only accepts an observation on a clip that already
    /// carries one — so "untouched" here means a member with no canonical answer at all, not a
    /// member nobody has opened.
    #[test]
    fn the_queue_ranks_a_disagreeing_pair_between_one_opinion_and_an_unjudged_clip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        rollback_fixture_to(&db, 59);
        for (id, canonical_reviewer) in [("fresh", Some("Nechir")), ("needs", Some("Nechir")), ("untouched", None)] {
            let audio = dir.path().join(format!("{id}.wav"));
            write_clip_wav(&audio, id);
            db.insert_segment_full(&segment(id, &audio, canonical_reviewer)).unwrap();
        }
        upgrade_fixture_from(&db, 59);
        for id in ["fresh", "needs", "untouched"] {
            db.connection()
                .execute(
                    "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                    rusqlite::params![id, clip_hash(id)],
                )
                .unwrap();
        }
        let pool = activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174052",
            &[
                PoolMemberInput { segment_id: "fresh".into(), voice_name: "Lamo".into() },
                PoolMemberInput { segment_id: "needs".into(), voice_name: "Lamo".into() },
                PoolMemberInput { segment_id: "untouched".into(), voice_name: "Lamo".into() },
            ],
        )
        .unwrap();

        // Sara disagrees with the canonical answer on "needs": two distinct outcomes from two
        // distinct reviewers is exactly the state a third opinion exists to break.
        let (_, revision) = db.get_segment_by_id_with_revision("needs").unwrap().unwrap();
        let audio_content_hash = pool.members.get("needs").unwrap().audio_content_hash.clone();
        let authority = authority(&db, "Sara", "needs");
        record_decision(
            &db,
            &pool,
            &PoolDecisionInput {
                segment_id: "needs",
                reviewer: "Sara",
                action: "edit",
                submitted_transcript: Some("دەقی جیاواز"),
                served_transcript: "دەقی چامپیۆن",
                served_revision: revision,
                audio_content_hash: Some(&audio_content_hash),
                source_start_ms: Some(0),
                source_end_ms: Some(1_000),
                duration_ms: 1_000,
                requested_action: "edit",
                requested_transcript: "دەقی جیاواز",
                operation_id: "123e4567-e89b-42d3-a456-426614174060",
                operation_payload_hash: &"b".repeat(64),
                created_at_ms: 1,
                playback_authority_session_id: Some(&authority),
            },
        )
        .unwrap()
        .expect("a pool observation on a verified, decided clip is recorded");

        let resolutions = segment_resolutions(&db, None).unwrap();
        let row = |segment_id: &str| {
            resolutions
                .iter()
                .find(|row| row.segment_id == segment_id)
                .unwrap_or_else(|| panic!("{segment_id} is missing from the resolutions"))
        };
        assert_eq!((row("fresh").status.as_str(), row("fresh").reviewer_count), ("pending", 1));
        assert_eq!((row("needs").status.as_str(), row("needs").reviewer_count), ("needsThirdReview", 2));
        assert_eq!((row("untouched").status.as_str(), row("untouched").reviewer_count), ("pending", 0));

        assert_eq!(
            pending_segment_ids(&db, &pool, "Hemn", None).unwrap(),
            vec!["fresh", "needs", "untouched"],
            "nearest a decision first: one opinion, then a disagreeing pair, then an unjudged clip"
        );
        assert_eq!(
            pending_segment_ids(&db, &pool, "Sara", None).unwrap(),
            vec!["fresh", "untouched"],
            "a reviewer never sees a clip they already judged"
        );
        assert_eq!(
            pending_segment_ids(&db, &pool, "  nEcHiR  ", None).unwrap(),
            vec!["untouched"],
            "the canonical answer is that reviewer's judgement, and identity is trim/case normalized"
        );
    }

    /// Independent audit: missing listening proof must be rejected before any paid effect commits.
    #[test]
    fn paid_pool_rejects_missing_listening_authority_without_any_durable_effect() {
        let (_dir, db, pool) = two_clip_pool(None);
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET verified=1, human_decision='edit', verdict='human_edit',
                        verdict_transcript='دەقی دروست', annotated_transcript='دەقی دروست',
                        reviewed_by='Rubar', review_revision=review_revision+1
                  WHERE id='a'",
                [],
            )
            .unwrap();
        let (_, revision) = db.get_segment_by_id_with_revision("a").unwrap().unwrap();
        let audio_hash = db.segment_audio_content_hash("a").unwrap().expect("fixture has canonical PCM identity");
        let payload_hash = "e".repeat(64);
        let durable_counts = || -> (i64, i64, i64) {
            db.connection()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM review_pool_decisions),
                            (SELECT COUNT(*) FROM review_compensation_ledger),
                            (SELECT COUNT(*) FROM playback_authority_consumptions_v4)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        };
        let before = durable_counts();
        let result = record_decision(
            &db,
            &pool,
            &PoolDecisionInput {
                segment_id: "a",
                reviewer: "Alle",
                action: "edit",
                submitted_transcript: Some("دەقی ئەلە"),
                served_transcript: "دەقی چامپیۆن",
                served_revision: revision,
                audio_content_hash: Some(&audio_hash),
                source_start_ms: Some(0),
                source_end_ms: Some(1_000),
                duration_ms: 1_000,
                requested_action: "edit",
                requested_transcript: "دەقی ئەلە",
                operation_id: "123e4567-e89b-42d3-a456-426614174903",
                operation_payload_hash: &payload_hash,
                created_at_ms: 1_000,
                playback_authority_session_id: None,
            },
        );
        assert_eq!(
            durable_counts(),
            before,
            "missing listening proof must leave decisions, credits and consumptions unchanged; writer returned {result:?}"
        );
        let error = result.expect_err("paid non-skip decisions require listening authority at the write boundary");
        assert!(
            error.contains("E_NO_PLAYBACK_EVIDENCE"),
            "must refuse the missing proof, not unrelated fixture drift: {error}"
        );
    }

    /// Owner canon 2026-09-04: "pool second opinions are paid at the same weights as first opinions
    /// (edit 100%, accept 10%, reject 10%)". Pinned as money: one committed pool judgement mints exactly
    /// one ledger credit at the first-opinion weight, a replay mints nothing, the reviewer's undo
    /// appends the exact signed inverse, and a replayed undo appends nothing.
    #[test]
    fn a_pool_second_opinion_is_paid_once_at_the_first_opinion_weight_and_undo_reverses_it() {
        let (_dir, db, pool) = two_clip_pool(None);
        rubar_first_opinions_on_a_and_b(&db);
        let (_, revision) = db.get_segment_by_id_with_revision("a").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let authority = authority(&db, "Alle", "a");
        let decision_operation = "123e4567-e89b-42d3-a456-426614174901";
        let input = PoolDecisionInput {
            segment_id: "a",
            reviewer: "Alle",
            action: "edit",
            submitted_transcript: Some("دەقی ئەلە"),
            served_transcript: "دەقی چامپیۆن",
            served_revision: revision,
            audio_content_hash: Some(&audio_hash),
            source_start_ms: Some(0),
            source_end_ms: Some(1_000),
            duration_ms: 1_000,
            requested_action: "edit",
            requested_transcript: "دەقی ئەلە",
            operation_id: decision_operation,
            operation_payload_hash: &"e".repeat(64),
            created_at_ms: 1_000,
            playback_authority_session_id: Some(&authority),
        };
        let decision_id = record_decision(&db, &pool, &input).unwrap().expect("the second opinion commits");

        type LedgerRow = (String, String, String, Option<i64>, i64, i64, i64, Option<String>);
        let ledger = |reviewer: &str| -> Vec<LedgerRow> {
            let mut statement = db
                .connection()
                .prepare(
                    "SELECT entry_id, entry_key, source, review_event_id, rate_basis_points,
                            entitlement_micro_iqd, delta_micro_iqd, reverses_entry_id
                       FROM review_compensation_ledger
                      WHERE reviewer=?1 COLLATE NOCASE ORDER BY id",
                )
                .unwrap();
            statement
                .query_map([reviewer], |row| {
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
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let credited = ledger("Alle");
        assert_eq!(credited.len(), 1, "exactly one credit for one committed pool judgement: {credited:?}");
        let (entry_id, entry_key, source, review_event_id, bps, entitlement, delta, reverses) = &credited[0];
        assert_eq!(
            entry_key,
            &format!("pool-decision:{decision_id}"),
            "the credit is keyed on the immutable pool decision"
        );
        assert_eq!(source, "couch_pool");
        assert_eq!(*review_event_id, None, "a pool judgement has no review_events row");
        assert_eq!(*bps, 10_000, "an edit earns 100%, exactly like a first-opinion edit");
        // 18,000 IQD per audio hour = 18_000_000_000 micro-IQD/h; a 1,000 ms edit is 5,000,000 micro-IQD.
        assert_eq!((*entitlement, *delta), (5_000_000, 5_000_000));
        assert_eq!(*reverses, None);

        let replay = record_decision(&db, &pool, &input).unwrap_err();
        assert!(replay.contains("duplicated"), "a replayed second opinion is refused, not re-recorded: {replay}");
        assert_eq!(ledger("Alle").len(), 1, "a replay mints nothing");

        let reversal_operation = "123e4567-e89b-42d3-a456-426614174902";
        let reversed =
            reverse_decision_addressed(&db, &pool, decision_id, "Alle", decision_operation, reversal_operation, 2_000)
                .unwrap();
        assert_eq!(reversed.as_deref(), Some("a"));
        let after_undo = ledger("Alle");
        assert_eq!(after_undo.len(), 2, "undo appends exactly one reversal: {after_undo:?}");
        let (_, undo_key, undo_source, _, _, _, undo_delta, undo_reverses) = &after_undo[1];
        assert_eq!(undo_key, &format!("undo:{reversal_operation}"));
        assert_eq!(undo_source, "couch_pool_undo");
        assert_eq!(*undo_delta, -5_000_000, "the reversal is the exact signed inverse");
        assert_eq!(undo_reverses.as_deref(), Some(entry_id.as_str()));
        let balance: i64 = db
            .connection()
            .query_row(
                "SELECT COALESCE(SUM(delta_micro_iqd),0) FROM review_compensation_ledger WHERE reviewer='Alle'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(balance, 0, "an undone second opinion owes nothing");

        let replayed_undo =
            reverse_decision_addressed(&db, &pool, decision_id, "Alle", decision_operation, reversal_operation, 3_000)
                .unwrap();
        assert_eq!(replayed_undo.as_deref(), Some("a"), "a replayed undo is acknowledged");
        assert_eq!(ledger("Alle").len(), 2, "a replayed undo appends nothing");
        assert!(
            ledger("Rubar").is_empty(),
            "the first opinion's owner is untouched by the second opinion's money: {:?}",
            ledger("Rubar")
        );
    }

    /// Rubar's canonical first opinion on both clips of `two_clip_pool`, the state a pool second
    /// opinion is served against.
    fn rubar_first_opinions_on_a_and_b(db: &Database) {
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET verified=1, human_decision='edit', verdict='human_edit',
                        verdict_transcript='دەقی دروست', annotated_transcript='دەقی دروست',
                        reviewed_by='Rubar', review_revision=review_revision+1
                  WHERE id IN ('a','b')",
                [],
            )
            .unwrap();
    }

    const PAYLOAD_HASH: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    const OPS: [&str; 8] = [
        "123e4567-e89b-42d3-a456-426614174930",
        "123e4567-e89b-42d3-a456-426614174931",
        "123e4567-e89b-42d3-a456-426614174932",
        "123e4567-e89b-42d3-a456-426614174933",
        "123e4567-e89b-42d3-a456-426614174934",
        "123e4567-e89b-42d3-a456-426614174935",
        "123e4567-e89b-42d3-a456-426614174936",
        "123e4567-e89b-42d3-a456-426614174937",
    ];

    /// Alle's paid edit on clip `a`, exactly as the phone route would hand it to the writer.
    fn alle_edit_on_a<'a>(
        revision: i64,
        audio_hash: &'a str,
        operation_id: &'a str,
        authority: Option<&'a str>,
    ) -> PoolDecisionInput<'a> {
        PoolDecisionInput {
            segment_id: "a",
            reviewer: "Alle",
            action: "edit",
            submitted_transcript: Some("دەقی ئەلە"),
            served_transcript: "دەقی چامپیۆن",
            served_revision: revision,
            audio_content_hash: Some(audio_hash),
            source_start_ms: Some(0),
            source_end_ms: Some(1_000),
            duration_ms: 1_000,
            requested_action: "edit",
            requested_transcript: "دەقی ئەلە",
            operation_id,
            operation_payload_hash: PAYLOAD_HASH,
            created_at_ms: 1_000,
            playback_authority_session_id: authority,
        }
    }

    /// (pool decisions, pool ledger rows, independent consumptions): the three writes one paid pool
    /// judgement makes, which must land together or not at all.
    fn pool_write_counts(db: &Database) -> (i64, i64, i64) {
        let count = |sql: &str| -> i64 { db.connection().query_row(sql, [], |row| row.get(0)).unwrap() };
        (
            count("SELECT COUNT(*) FROM review_pool_decisions"),
            count("SELECT COUNT(*) FROM review_compensation_ledger WHERE source IN ('couch_pool','couch_pool_undo')"),
            count("SELECT COUNT(*) FROM playback_authority_consumptions_v4 WHERE namespace='independent'"),
        )
    }

    #[test]
    fn pool_rejects_operation_uuid_already_owned_by_canonical_skip() {
        let (_dir, db, pool) = two_clip_pool(None);
        rubar_first_opinions_on_a_and_b(&db);
        db.record_review_event("b", "Sewa", "skip", "couch", 1_000).unwrap();
        let canonical_operation: String = db
            .connection()
            .query_row("SELECT operation_id FROM review_events WHERE segment_id='b' AND reviewer='Sewa'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let revision = db.segment_review_revision("a").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let proof = authority(&db, "Alle", "a");
        let before = pool_write_counts(&db);
        let result =
            record_decision(&db, &pool, &alle_edit_on_a(revision, &audio_hash, &canonical_operation, Some(&proof)));
        assert_eq!(
            pool_write_counts(&db),
            before,
            "pool write must refuse a canonical operation UUID before any durable effect; result={result:?}"
        );
        let error = result.expect_err("canonical operation UUID cannot also identify a pool decision");
        assert!(error.contains("E_REVIEW_OPERATION_NAMESPACE_COLLISION"), "{error}");
        record_decision(&db, &pool, &alle_edit_on_a(revision, &audio_hash, OPS[0], Some(&proof)))
            .unwrap()
            .expect("the same valid proof and decision succeed with a fresh operation UUID");
        assert_eq!(pool_write_counts(&db), (1, 1, 1));
    }

    #[test]
    fn canonical_skip_rejects_operation_uuid_already_owned_by_pool() {
        let (_dir, db, pool) = two_clip_pool(None);
        rubar_first_opinions_on_a_and_b(&db);
        let revision = db.segment_review_revision("a").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let proof = authority(&db, "Alle", "a");
        record_decision(&db, &pool, &alle_edit_on_a(revision, &audio_hash, OPS[0], Some(&proof))).unwrap().unwrap();
        let canonical_counts =
            || -> (i64, i64) {
                db.connection().query_row(
                "SELECT (SELECT COUNT(*) FROM review_events), (SELECT COUNT(*) FROM review_compensation_ledger)",
                [], |row| Ok((row.get(0)?, row.get(1)?)),
            ).unwrap()
            };
        let before = canonical_counts();
        let payload = crate::db::review_operation_payload_hash("b", "skip", "", "Sewa");
        let result = db.record_review_event_with_operation("b", "Sewa", "skip", "couch", 2_000, OPS[0], &payload);
        assert_eq!(
            canonical_counts(),
            before,
            "canonical write must refuse a pool operation UUID before any durable effect; result={result:?}"
        );
        let error = result.expect_err("pool operation UUID cannot also identify a canonical skip").to_string();
        assert!(error.contains("review operation id belongs to the independent pool"), "{error}");
        db.record_review_event_with_operation("b", "Sewa", "skip", "couch", 2_000, OPS[1], &payload)
            .expect("the same valid canonical skip succeeds with a fresh operation UUID");
    }

    #[test]
    fn pool_undo_rejects_operation_uuid_owned_by_canonical_skip() {
        let (_dir, db, pool) = two_clip_pool(None);
        rubar_first_opinions_on_a_and_b(&db);
        let revision = db.segment_review_revision("a").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let proof = authority(&db, "Alle", "a");
        let decision =
            record_decision(&db, &pool, &alle_edit_on_a(revision, &audio_hash, OPS[0], Some(&proof))).unwrap().unwrap();
        db.record_review_event("b", "Sewa", "skip", "couch", 2_000).unwrap();
        let canonical_operation: String = db
            .connection()
            .query_row("SELECT operation_id FROM review_events WHERE segment_id='b' AND reviewer='Sewa'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let before = pool_write_counts(&db);
        let result = reverse_decision_addressed(&db, &pool, decision, "Alle", OPS[0], &canonical_operation, 3_000);
        let startup = db.initialize().map_err(|error| error.to_string());
        assert_eq!(pool_write_counts(&db), before,
            "undo must not append a reversal/credit under a canonical operation UUID; result={result:?}, startup={startup:?}");
        assert!(result.is_err(), "colliding reversal operation must be refused");
        startup.expect("refused undo collision must leave a valid startup state");
    }

    #[test]
    fn canonical_skip_rejects_operation_uuid_owned_by_pool_undo() {
        let (_dir, db, pool) = two_clip_pool(None);
        rubar_first_opinions_on_a_and_b(&db);
        let revision = db.segment_review_revision("a").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let proof = authority(&db, "Alle", "a");
        let decision =
            record_decision(&db, &pool, &alle_edit_on_a(revision, &audio_hash, OPS[0], Some(&proof))).unwrap().unwrap();
        reverse_decision_addressed(&db, &pool, decision, "Alle", OPS[0], OPS[1], 2_000).unwrap().unwrap();
        let count = || -> i64 {
            db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap()
        };
        let before = count();
        let payload = crate::db::review_operation_payload_hash("b", "skip", "", "Sewa");
        let result = db.record_review_event_with_operation("b", "Sewa", "skip", "couch", 3_000, OPS[1], &payload);
        let startup = db.initialize().map_err(|error| error.to_string());
        assert_eq!(
            count(),
            before,
            "canonical skip must not commit under a pool-undo UUID; result={result:?}, startup={startup:?}"
        );
        assert!(result.is_err(), "pool-undo operation must not be reused by a canonical write");
        startup.expect("refused canonical collision must leave a valid startup state");
    }

    #[test]
    fn a_paid_pool_judgement_without_exact_playback_authority_writes_nothing() {
        let (_dir, db, pool) = two_clip_pool(None);
        rubar_first_opinions_on_a_and_b(&db);
        let audio_hash = clip_hash("a");
        let revision = |db: &Database| db.get_segment_by_id_with_revision("a").unwrap().unwrap().1;

        // No authority at all.
        let refused =
            record_decision(&db, &pool, &alle_edit_on_a(revision(&db), &audio_hash, OPS[0], None)).unwrap_err();
        assert!(refused.contains("E_NO_PLAYBACK_EVIDENCE"), "{refused}");
        assert_eq!(pool_write_counts(&db), (0, 0, 0), "a refused judgement leaves no decision, credit or consumption");

        // Somebody else's listening.
        let sewa = authority(&db, "Sewa", "a");
        let refused =
            record_decision(&db, &pool, &alle_edit_on_a(revision(&db), &audio_hash, OPS[1], Some(&sewa))).unwrap_err();
        assert!(refused.contains("E_NO_PLAYBACK_EVIDENCE"), "wrong reviewer: {refused}");
        assert_eq!(pool_write_counts(&db), (0, 0, 0));

        // Alle's listening, but of the other clip.
        let other_clip = authority(&db, "Alle", "b");
        let refused =
            record_decision(&db, &pool, &alle_edit_on_a(revision(&db), &audio_hash, OPS[2], Some(&other_clip)))
                .unwrap_err();
        assert!(refused.contains("E_NO_PLAYBACK_EVIDENCE"), "wrong clip: {refused}");
        assert_eq!(pool_write_counts(&db), (0, 0, 0));

        // Alle listened to this clip, then its revision moved before the judgement landed.
        let stale = authority(&db, "Alle", "a");
        db.connection()
            .execute("UPDATE speech_segments SET review_revision=review_revision+1 WHERE id='a'", [])
            .unwrap();
        let refused =
            record_decision(&db, &pool, &alle_edit_on_a(revision(&db), &audio_hash, OPS[3], Some(&stale))).unwrap_err();
        assert!(refused.contains("E_NO_PLAYBACK_EVIDENCE"), "stale revision: {refused}");
        assert_eq!(pool_write_counts(&db), (0, 0, 0));

        // Exact proof, but the credit cannot be written: the judgement and the consumption roll back
        // with it, so no committed pool opinion can ever exist unpaid.
        let exact = authority(&db, "Alle", "a");
        let triggers: Vec<String> = db
            .connection()
            .prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='review_compensation_policies'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for name in triggers {
            db.connection().execute(&format!("DROP TRIGGER \"{name}\""), []).unwrap();
        }
        db.connection().execute("DELETE FROM review_compensation_policies", []).unwrap();
        let refused =
            record_decision(&db, &pool, &alle_edit_on_a(revision(&db), &audio_hash, OPS[4], Some(&exact))).unwrap_err();
        assert!(refused.contains("compensation"), "credit failure: {refused}");
        assert_eq!(pool_write_counts(&db), (0, 0, 0), "a failed credit rolls the judgement and its consumption back");
    }

    #[test]
    fn a_spent_pool_playback_authority_cannot_pay_twice_but_a_fresh_listen_can() {
        let (_dir, db, pool) = two_clip_pool(None);
        rubar_first_opinions_on_a_and_b(&db);
        let (_, revision) = db.get_segment_by_id_with_revision("a").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let spent = authority(&db, "Alle", "a");
        let first = record_decision(&db, &pool, &alle_edit_on_a(revision, &audio_hash, OPS[0], Some(&spent)))
            .unwrap()
            .expect("a proven second opinion lands");
        assert_eq!(pool_write_counts(&db), (1, 1, 1));

        // Undo frees the reviewer to judge again, not the receipt they already spent.
        reverse_decision(&db, &pool, first, "Alle", OPS[1], 2_000).unwrap();
        assert_eq!(pool_write_counts(&db), (1, 2, 1), "undo appends its reversal and consumes nothing");
        let reused =
            record_decision(&db, &pool, &alle_edit_on_a(revision, &audio_hash, OPS[2], Some(&spent))).unwrap_err();
        assert!(reused.contains("E_PLAYBACK_RECEIPT_CONSUMED"), "{reused}");
        assert_eq!(pool_write_counts(&db), (1, 2, 1), "a spent receipt writes nothing");

        let fresh = authority(&db, "Alle", "a");
        record_decision(&db, &pool, &alle_edit_on_a(revision, &audio_hash, OPS[3], Some(&fresh)))
            .unwrap()
            .expect("a fresh listen pays again");
        assert_eq!(pool_write_counts(&db), (2, 3, 2));
    }

    /// The whole durable life of one paid second opinion: first opinion -> proven second opinion ->
    /// process restart -> settlement -> undo -> restart, with the restore validator accepting the
    /// database at every stage. Startup runs the same policy-4 audit a staged restore does.
    #[test]
    fn a_paid_pool_judgement_survives_reopen_restore_validation_settlement_and_undo() {
        let (dir, db, pool) = disk_one_clip_pool("123e4567-e89b-42d3-a456-4266141740b0", "دەقی دروست");
        let db_path = dir.path().join("pool-fixture.db");
        let validate = crate::restore_service::validate_review_compensation_semantics;
        validate(&db).expect("a pristine pool database is a valid restore target");

        let (_, revision) = db.get_segment_by_id_with_revision("clip").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let authority = authority(&db, "Alle", "clip");
        let mut input = alle_edit_on_a(revision, &audio_hash, OPS[0], Some(&authority));
        input.segment_id = "clip";
        let decision_id = record_decision(&db, &pool, &input).unwrap().expect("the second opinion commits");
        validate(&db).expect("a paid second opinion is a valid restore target");

        drop(db);
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().expect("startup audit accepts the paid pool judgement's consumption");
        let pool = load(&db).unwrap().expect("the pool survives the restart");
        validate(&db).unwrap();

        let credit_id: i64 = db
            .connection()
            .query_row(
                "SELECT id FROM review_compensation_ledger WHERE entry_key=?1",
                [format!("pool-decision:{decision_id}")],
                |row| row.get(0),
            )
            .unwrap();
        let settlement = db.record_review_compensation_settlement("Alle", credit_id, "payout-pool-1").unwrap();
        assert_eq!(settlement.allocated_micro_iqd, 5_000_000, "a 1,000 ms edit at 18,000 IQD/h settles exactly");
        validate(&db).expect("a settled second opinion is a valid restore target");

        assert_eq!(
            reverse_decision_addressed(&db, &pool, decision_id, "Alle", OPS[0], OPS[1], 2_000).unwrap().as_deref(),
            Some("clip")
        );
        validate(&db).expect("an undone second opinion is a valid restore target");

        drop(db);
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().expect("startup audit accepts the undone pool judgement");
        validate(&db).unwrap();
        assert_eq!(pool_write_counts(&db), (1, 2, 1));
        let balance: i64 = db
            .connection()
            .query_row(
                "SELECT COALESCE(SUM(delta_micro_iqd),0) FROM review_compensation_ledger WHERE reviewer='Alle'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(balance, 0, "credit and reversal net to zero; the settled payout stays on the books");
    }

    #[test]
    fn paid_pool_named_snapshot_restore_preserves_proof_credit_settlement_and_later_undo() {
        use crate::database_runtime::RestoreAdmission;
        use crate::recovery::{
            clear_review_pilot_restore_pending, install_snapshot_restore_plan, load_named_restore_pending,
            mark_named_restore_completed,
        };
        use crate::restore_service::prepare_and_restore_named_transaction;

        let (dir, fixture, pool) = disk_one_clip_pool("123e4567-e89b-42d3-a456-4266141740b1", "دەقی دروست");
        let data_dir = dir.path();
        let live_path = data_dir.join("cortex-speech.db");
        fixture.backup(&live_path).unwrap();
        drop(fixture);
        let mut live = Database::open(live_path.to_str().unwrap()).unwrap();
        live.initialize().unwrap();
        let obsolete = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 1000).unwrap().unwrap();

        let (_, revision) = live.get_segment_by_id_with_revision("clip").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let proof = authority(&live, "Alle", "clip");
        let mut input = alle_edit_on_a(revision, &audio_hash, OPS[0], Some(&proof));
        input.segment_id = "clip";
        let decision_id = record_decision(&live, &pool, &input).unwrap().unwrap();
        let credit_id: i64 = live
            .connection()
            .query_row(
                "SELECT id FROM review_compensation_ledger WHERE entry_key=?1",
                [format!("pool-decision:{decision_id}")],
                |row| row.get(0),
            )
            .unwrap();
        let settlement = live.record_review_compensation_settlement("Alle", credit_id, "restored-pool-payout").unwrap();
        assert_eq!(settlement.allocated_micro_iqd, 5_000_000);
        assert_eq!(pool_write_counts(&live), (1, 1, 1));

        // A previously valid backup may not erase subsequently earned money or listening evidence.
        let paid_generation = live.restore_generation_sha256().unwrap();
        let admission = RestoreAdmission::new();
        {
            let reservation = admission.try_reserve().unwrap();
            let error = prepare_and_restore_named_transaction(
                &reservation,
                &mut live,
                data_dir,
                &obsolete,
                &obsolete.join("cortex-speech.db"),
                "snapshot_0000001000",
            )
            .unwrap_err();
            assert!(error.contains("review_compensation_ledger"), "wrong refusal: {error}");
        }
        assert_eq!(live.restore_generation_sha256().unwrap(), paid_generation);
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let pins = data_dir.join("snapshots").join("pinned");
        assert!(!pins.exists() || std::fs::read_dir(&pins).unwrap().next().is_none());

        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 2000).unwrap().unwrap();
        assert!(crate::snapshot::verify_snapshot_manifest_for_restore(&target).unwrap());
        let source = target.join("cortex-speech.db");
        let source_before = std::fs::read(&source).unwrap();
        // Non-reviewed work makes the source and live generations genuinely different. The page
        // replacement must remove it while retaining ALL paid-review authority in the backup.
        live.insert_segment_full(&segment("unreviewed-after-backup", &data_dir.join("clip.wav"), None)).unwrap();
        assert_ne!(live.restore_generation_sha256().unwrap(), paid_generation);
        let reservation = admission.try_reserve().unwrap();
        let plan = prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &target,
            &source,
            "snapshot_0000002000",
        )
        .unwrap();
        assert_eq!(live.restore_generation_sha256().unwrap(), plan.expected_db_generation_sha256);
        assert!(live.get_segment_by_id("unreviewed-after-backup").unwrap().is_none());
        assert_eq!(pool_write_counts(&live), (1, 1, 1));
        crate::restore_service::validate_review_compensation_semantics(&live).unwrap();
        install_snapshot_restore_plan(&plan, data_dir, &crate::settings::AppSettings::default()).unwrap();
        mark_named_restore_completed(data_dir, "snapshot_0000002000", &plan.expected_db_generation_sha256).unwrap();
        clear_review_pilot_restore_pending(data_dir).unwrap();
        reservation.commit_named_restore().unwrap();
        drop(reservation);
        drop(live);

        let restored = Database::open(live_path.to_str().unwrap()).unwrap();
        restored.initialize().expect("real restored pages must pass startup's exact playback/ledger audit");
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        assert_eq!(pool_write_counts(&restored), (1, 1, 1));
        let replayed_settlement =
            restored.record_review_compensation_settlement("Alle", credit_id, "restored-pool-payout").unwrap();
        assert_eq!(replayed_settlement.allocated_micro_iqd, settlement.allocated_micro_iqd);
        let settlement_count: i64 = restored
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_compensation_settlements WHERE payout_reference='restored-pool-payout'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(settlement_count, 1, "restored payout retries must not mint a second settlement");
        let restored_pool = load(&restored).unwrap().unwrap();
        reverse_decision_addressed(&restored, &restored_pool, decision_id, "Alle", OPS[0], OPS[1], 2_000)
            .unwrap()
            .unwrap();
        assert_eq!(pool_write_counts(&restored), (1, 2, 1));
        crate::restore_service::validate_review_compensation_semantics(&restored).unwrap();
        assert!(std::fs::read(&source).unwrap() == source_before, "restore never edits the source backup");
    }

    #[test]
    fn a_forged_or_orphaned_pool_consumption_fails_the_startup_audit() {
        // Orphaned: a genuine receipt consumed by an operation no judgement table knows.
        let (dir, db, pool) = disk_one_clip_pool("123e4567-e89b-42d3-a456-4266141740c0", "دەقی دروست");
        let db_path = dir.path().join("pool-fixture.db");
        let (_, revision) = db.get_segment_by_id_with_revision("clip").unwrap().unwrap();
        let audio_hash = clip_hash("a");
        let genuine = authority(&db, "Alle", "clip");
        let mut input = alle_edit_on_a(revision, &audio_hash, OPS[0], Some(&genuine));
        input.segment_id = "clip";
        record_decision(&db, &pool, &input).unwrap().expect("a genuine paid judgement sits beside the forgery");
        let stray = authority(&db, "Sewa", "clip");
        db.connection()
            .execute(
                "INSERT INTO playback_authority_consumptions_v4
                    (playback_receipt_id,namespace,operation_id,reviewer,segment_id,created_at_ms)
                 VALUES (?1,'independent',?2,'Sewa','clip',5)",
                rusqlite::params![stray, OPS[5]],
            )
            .unwrap();
        drop(db);
        let reopened = Database::open(db_path.to_str().unwrap()).unwrap();
        let error = reopened.initialize().unwrap_err().to_string();
        assert!(error.contains("orphaned or mismatched consumption"), "orphan: {error}");

        // Forged: a consumption that points at a SKIP (which mints nothing and consumes nothing).
        let (dir, db, pool) = disk_one_clip_pool("123e4567-e89b-42d3-a456-4266141740c1", "دەقی دروست");
        let db_path = dir.path().join("pool-fixture.db");
        let (_, revision) = db.get_segment_by_id_with_revision("clip").unwrap().unwrap();
        let mut skip = alle_edit_on_a(revision, &audio_hash, OPS[6], None);
        skip.segment_id = "clip";
        skip.reviewer = "Sewa";
        skip.action = "skip";
        skip.submitted_transcript = None;
        skip.requested_action = "skip";
        record_decision(&db, &pool, &skip).unwrap().expect("a skip is recorded unpaid");
        assert_eq!(pool_write_counts(&db), (1, 0, 0), "a skip consumes no authority and mints no credit");
        let unspent = authority(&db, "Sewa", "clip");
        db.connection()
            .execute(
                "INSERT INTO playback_authority_consumptions_v4
                    (playback_receipt_id,namespace,operation_id,reviewer,segment_id,created_at_ms)
                 VALUES (?1,'independent',?2,'Sewa','clip',6)",
                rusqlite::params![unspent, OPS[6]],
            )
            .unwrap();
        drop(db);
        let reopened = Database::open(db_path.to_str().unwrap()).unwrap();
        let error = reopened.initialize().unwrap_err().to_string();
        assert!(error.contains("orphaned or mismatched consumption"), "skip forgery: {error}");
    }
}
