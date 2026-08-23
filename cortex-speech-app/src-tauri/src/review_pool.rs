//! Flexible, voice-organized human review pool.
//!
//! The canonical `speech_segments` row still receives the first human verdict. Later reviewers write
//! append-only observations here, so an independent second or third judgement can never overwrite the
//! first answer. Queue selection is coverage-first and reviewer-specific: a person sees clips they have
//! not judged, ordered by the number of distinct effective judgements already attached to each clip.

use crate::db::Database;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

pub const REVIEW_POOL_SCHEMA_VERSION: i64 = 62;
pub const REVIEW_POOL_PLAYBACK_GUARD: &str = "content-hash-raw-counter-v3";
const DESKTOP_REVIEWER_KEY: &str = "@desktop-owner";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPool {
    pub pool_id: String,
    pub focus_segment_count: usize,
    pub focus_sha256: String,
    pub champion_model_version_id: String,
    pub champion_deployment_sha256: String,
    members: Arc<HashMap<String, PoolMemberEvidence>>,
    member_ids: Arc<HashSet<String>>,
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
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_SCHEMA_VERSION {
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
            "SELECT segment_id, voice_name, raw_transcript, model_version_id,
                    audio_content_hash, source_start_ms, source_end_ms, duration_ms
               FROM review_pool_members WHERE pool_id=?1 ORDER BY segment_id",
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
            ))
        })
        .map_err(|error| format!("review pool members cannot be read: {error}"))?;
    let mut members = HashMap::new();
    for row in rows {
        let (segment_id, evidence) = row.map_err(|error| format!("review pool member is unreadable: {error}"))?;
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
    };
    require_live_member_identity(db, &pool)?;
    Ok(Some(pool))
}

/// Cheap request-boundary validation for a pool that was fully digest-verified at Start.
/// The registry and member rows are immutable under schema 62, so checking the bound registry
/// identity and member count avoids re-reading and re-hashing tens of thousands of rows on every
/// queue fetch and decision without weakening the fail-closed session binding.
pub fn registry_matches(db: &Database, bound: &ReviewPool) -> Result<bool, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? != REVIEW_POOL_SCHEMA_VERSION {
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
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_SCHEMA_VERSION {
        return Err("flexible review pool requires schema 62".to_string());
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
        let segment: Option<(String, String, Option<String>, Option<i64>, Option<i64>, i64)> = db
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
    judged: HashSet<String>,
    seen: HashSet<String>,
}

fn reviewer_sets(db: &Database) -> Result<HashMap<String, SegmentReviewers>, String> {
    let mut result: HashMap<String, SegmentReviewers> = HashMap::new();
    let mut canonical = db
        .connection()
        .prepare(
            "SELECT member.segment_id, segment.reviewed_by
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE segment.verified=1 AND segment.human_decision IN ('accept','edit','reject')",
        )
        .map_err(|error| format!("canonical review coverage cannot be read: {error}"))?;
    let rows = canonical
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))
        .map_err(|error| format!("canonical review coverage cannot be read: {error}"))?;
    for row in rows {
        let (segment_id, reviewer) =
            row.map_err(|error| format!("canonical review coverage is unreadable: {error}"))?;
        let key = reviewer_key(reviewer.as_deref());
        let entry = result.entry(segment_id).or_default();
        entry.judged.insert(key.clone());
        entry.seen.insert(key);
    }

    let mut independent = db
        .connection()
        .prepare(
            "SELECT decision.segment_id, decision.reviewer, decision.action
               FROM effective_review_pool_decisions_v62 decision",
        )
        .map_err(|error| format!("independent pool coverage cannot be read: {error}"))?;
    let rows = independent
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .map_err(|error| format!("independent pool coverage cannot be read: {error}"))?;
    for row in rows {
        let (segment_id, reviewer, action) =
            row.map_err(|error| format!("independent pool coverage is unreadable: {error}"))?;
        let key = reviewer_key(Some(&reviewer));
        let entry = result.entry(segment_id).or_default();
        entry.seen.insert(key.clone());
        if action != "skip" {
            entry.judged.insert(key);
        }
    }

    // Preserve any already-committed v61 blinded judgements if the old sequential campaign was used
    // before this pool superseded its serving policy.
    let mut legacy = db
        .connection()
        .prepare(
            "SELECT decision.segment_id, decision.reviewer, decision.action
               FROM effective_independent_review_decisions_v61 decision
               JOIN review_pool_members member ON member.segment_id=decision.segment_id",
        )
        .map_err(|error| format!("legacy independent coverage cannot be read: {error}"))?;
    let rows = legacy
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .map_err(|error| format!("legacy independent coverage cannot be read: {error}"))?;
    for row in rows {
        let (segment_id, reviewer, action) =
            row.map_err(|error| format!("legacy independent coverage is unreadable: {error}"))?;
        let key = reviewer_key(Some(&reviewer));
        let entry = result.entry(segment_id).or_default();
        entry.seen.insert(key.clone());
        if action != "skip" {
            entry.judged.insert(key);
        }
    }
    Ok(result)
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
    require_live_member_identity(db, pool)?;
    let reviewers = reviewer_sets(db)?;
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
        if !Path::new(&audio_path).is_file() || !crate::dialect::reviewer_may_judge(allowed_dialects, &audio_path) {
            continue;
        }
        pending.push((coverage.map_or(0, |coverage| coverage.judged.len()), created_at, segment_id));
    }
    pending.sort_unstable_by(|left, right| left.cmp(right));
    Ok(pending.into_iter().map(|(_, _, segment_id)| segment_id).collect())
}

pub fn coverage_by_voice(db: &Database) -> Result<Vec<VoiceCoverage>, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let reviewers = reviewer_sets(db)?;
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
        });
        entry.total_clips += 1;
        match reviews {
            0 => entry.zero_reviews += 1,
            1 => entry.one_review += 1,
            2 => entry.two_reviews += 1,
            _ => entry.three_or_more_reviews += 1,
        }
    }
    let mut rows: Vec<VoiceCoverage> = by_voice.into_values().collect();
    rows.sort_unstable_by(|left, right| left.voice_name.cmp(&right.voice_name));
    Ok(rows)
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
    let changed = db
        .with_full_sync(|| {
            Ok(db.connection().execute(
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
            )?)
        })
        .map_err(|error| format!("review pool decision cannot be committed: {error}"))?;
    if changed == 0 {
        return Ok(None);
    }
    Ok(Some(db.connection().last_insert_rowid()))
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
    fn pool_orders_least_covered_and_keeps_second_review_append_only() {
        let dir = tempfile::tempdir().unwrap();
        let first_audio = dir.path().join("first.wav");
        let second_audio = dir.path().join("second.wav");
        std::fs::write(&first_audio, b"wav").unwrap();
        std::fs::write(&second_audio, b"wav").unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        assert_eq!(crate::migrations::rollback(&db, 3).unwrap(), vec![62, 61, 60]);
        db.insert_segment_full(&segment("first", &first_audio, Some("Rubar"))).unwrap();
        db.insert_segment_full(&segment("second", &second_audio, None)).unwrap();
        assert_eq!(crate::migrations::run_migrations(&db).unwrap(), vec![60, 61, 62]);
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
}
