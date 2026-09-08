//! Durable owner rounds: old authority is held, not erased or financially reversed.
//! A clip's latest round permanently supersedes earlier opinions; only fresh distinct people vote.

use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReopenItem {
    pub segment_id: String,
    pub revision: i64,
    pub pool_decision_floor: i64,
    pub adjudication_floor: i64,
    pub review_event_floor: i64,
    pub prior_round_seq: i64,
    pub evidence_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReopenPlan {
    pub version: u32,
    pub round_id: String,
    pub pool_id: String,
    pub focus_sha256: String,
    pub dedup_sha256: Option<String>,
    pub reason: String,
    pub priority: u8,
    pub items: Vec<ReopenItem>,
    pub plan_sha256: String,
}

fn sha(value: &impl Serialize) -> Result<String, String> {
    Ok(Sha256::digest(serde_json::to_vec(value).map_err(|e| e.to_string())?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn digest(plan: &ReopenPlan) -> Result<String, String> {
    sha(&(
        plan.version,
        &plan.round_id,
        &plan.pool_id,
        &plan.focus_sha256,
        &plan.dedup_sha256,
        &plan.reason,
        plan.priority,
        &plan.items,
    ))
}

pub(crate) fn supported_on(conn: &rusqlite::Connection) -> Result<bool, String> {
    conn.query_row("SELECT COALESCE(MAX(version),0)>=71 FROM schema_migrations", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// SQL fragment used by the canonical opinion projection, never by historical accounting.
pub(super) fn canonical_clause(conn: &rusqlite::Connection, alias: &str) -> Result<String, String> {
    if !supported_on(conn)? {
        return Ok(String::new());
    }
    Ok(format!(
        " AND NOT EXISTS (SELECT 1 FROM current_review_reopen_members_v71 reopen WHERE reopen.segment_id={alias}.id) "
    ))
}

pub fn revision(db: &Database, segment_id: &str) -> Result<Option<i64>, String> {
    if !supported_on(db.connection())? {
        return Ok(None);
    }
    db.connection()
        .query_row(
            "SELECT target_revision FROM current_review_reopen_members_v71 WHERE segment_id=?1",
            [segment_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
}

pub fn has_rounds(db: &Database) -> Result<bool, String> {
    if !supported_on(db.connection())? {
        return Ok(false);
    }
    db.connection()
        .query_row("SELECT EXISTS(SELECT 1 FROM review_reopen_rounds)", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

pub(crate) fn decision_is_current(db: &Database, segment_id: &str, decision_id: i64) -> Result<bool, String> {
    if !supported_on(db.connection())? {
        return Ok(true);
    }
    db.connection().query_row("SELECT ?2>COALESCE((SELECT pool_decision_floor FROM current_review_reopen_members_v71 WHERE segment_id=?1),0)",
        rusqlite::params![segment_id,decision_id],|r|r.get(0)).map_err(|e|e.to_string())
}

pub(crate) fn priorities(db: &Database) -> Result<HashMap<String, u8>, String> {
    if !supported_on(db.connection())? {
        return Ok(HashMap::new());
    }
    let mut s = db
        .connection()
        .prepare("SELECT segment_id,priority FROM current_review_reopen_members_v71")
        .map_err(|e| e.to_string())?;
    let rows = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string());
    rows
}

fn items_on(conn: &rusqlite::Connection, pool: &ReviewPool, ids: &[String]) -> Result<Vec<ReopenItem>, String> {
    let ids_json = serde_json::to_string(ids).map_err(|e| e.to_string())?;
    let reviewers = reviewer_sets_for_ids_on(conn, Some(&ids_json))?;
    let owners = owner_adjudications_for_ids_on(conn, Some(&ids_json))?;
    ids.iter().map(|id| {
        if !pool.contains(id) { return Err("reopen target is retired or outside the active pool".into()); }
        let (revision, canonical): (i64, String) = conn.query_row(
            "SELECT review_revision, json_array(verified,human_decision,reviewed_by,verdict,
                 verdict_transcript,annotated_transcript,raw_transcript,audio_content_hash,alignment_json,model_version_id,duration_ms)
               FROM speech_segments WHERE id=?1 AND verified=1 AND human_decision IN ('accept','edit','reject')",
            [id], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|e| format!("reopen requires an existing canonical decision: {e}"))?;
        let certified: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM review_pool_voice_certificates c
            JOIN review_pool_members m ON m.pool_id=c.pool_id AND m.voice_name=c.voice_name WHERE m.segment_id=?1)",
            [id], |r| r.get(0)).map_err(|e| e.to_string())?;
        if certified { return Err("certified voice requires explicit certificate revocation before reopening".into()); }
        let (floor, owner_floor, event_floor, prior_round): (i64,i64,i64,i64) = conn.query_row(
            "SELECT (SELECT COALESCE(MAX(id),0) FROM review_pool_decisions WHERE segment_id=?1),
                    (SELECT COALESCE(MAX(id),0) FROM review_pool_owner_adjudications WHERE segment_id=?1),
                    (SELECT COALESCE(MAX(id),0) FROM review_events WHERE segment_id=?1),
                    COALESCE((SELECT round_seq FROM current_review_reopen_members_v71 WHERE segment_id=?1),0)",
            [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|e| e.to_string())?;
        let (_, evidence) = derive_resolution(id, reviewers.get(id), owners.get(id));
        Ok(ReopenItem { segment_id: id.clone(), revision, pool_decision_floor: floor,
            adjudication_floor: owner_floor, review_event_floor: event_floor, prior_round_seq: prior_round,
            evidence_sha256: sha(&(canonical,evidence,owners.get(id).map(|rows| rows.iter()
                .map(|r| (&r.evidence_sha256,r.final_outcome.digest_value())).collect::<Vec<_>>())) )? })
    }).collect()
}

pub fn prepare(
    db: &Database,
    pool: &ReviewPool,
    ids: &[String],
    reason: &str,
    priority: u8,
) -> Result<ReopenPlan, String> {
    if !supported_on(db.connection())? {
        return Err("reopen requires schema 71".into());
    }
    if ids.is_empty() || ids.len() > 10000 || reason.trim().is_empty() || reason.chars().count() > 2000 || priority > 2
    {
        return Err("reopen requires 1..10000 exact clip IDs, a reason, and priority 0..2".into());
    }
    let mut sorted = ids.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != ids.len() {
        return Err("duplicate reopen targets".into());
    }
    let tx = if db.connection().is_autocommit() {
        Some(
            rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Deferred)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    if !registry_matches(db, pool)? {
        return Err("reopen pool identity changed".into());
    }
    let mut plan = ReopenPlan {
        version: 1,
        round_id: uuid::Uuid::new_v4().to_string(),
        pool_id: pool.pool_id.clone(),
        focus_sha256: pool.focus_sha256.clone(),
        dedup_sha256: pool.dedup_manifest_sha256.clone(),
        reason: reason.trim().into(),
        priority,
        items: items_on(db.connection(), pool, &sorted)?,
        plan_sha256: String::new(),
    };
    plan.plan_sha256 = digest(&plan)?;
    if let Some(tx) = tx {
        tx.rollback().map_err(|e| e.to_string())?;
    }
    Ok(plan)
}

/// Quality withdrawal changes no compensation rows. New work uses the existing entitlement policy.
pub fn apply(db: &Database, pool: &ReviewPool, plan: &ReopenPlan, created_at_ms: i64) -> Result<usize, String> {
    canonical_uuid(&plan.round_id, "reopen round id")?;
    if !supported_on(db.connection())?
        || plan.version != 1
        || plan.pool_id != pool.pool_id
        || plan.focus_sha256 != pool.focus_sha256
        || plan.dedup_sha256 != pool.dedup_manifest_sha256
        || plan.items.is_empty()
        || plan.items.len() > 10000
        || plan.priority > 2
        || created_at_ms <= 0
        || plan.reason.trim().is_empty()
        || plan.reason.chars().count() > 2000
        || digest(plan)? != plan.plan_sha256
        || plan.items.windows(2).any(|p| p[0].segment_id >= p[1].segment_id)
    {
        return Err("reopen plan identity or digest is invalid".into());
    }
    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        if !registry_matches(db, pool)? {
            return Err("reopen pool identity changed".into());
        }
        let encoded = serde_json::to_string(plan).map_err(|e| e.to_string())?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT plan_json FROM review_reopen_rounds WHERE round_id=?1 OR plan_sha256=?2",
                rusqlite::params![plan.round_id, plan.plan_sha256],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(prior) = prior {
            if prior != encoded {
                return Err("reopen operation belongs to a different plan".into());
            }
            validate_on(&tx)?;
            tx.rollback().map_err(|e| e.to_string())?;
            return Ok(plan.items.len());
        }
        let ids = plan.items.iter().map(|i| i.segment_id.clone()).collect::<Vec<_>>();
        if items_on(&tx, pool, &ids)? != plan.items {
            return Err("reopen preview is stale; prepare a new exact plan".into());
        }
        tx.execute(
            "INSERT INTO review_reopen_rounds(round_id,pool_id,plan_sha256,plan_json,reason,member_count,created_at_ms)
            VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                plan.round_id,
                plan.pool_id,
                plan.plan_sha256,
                encoded,
                plan.reason,
                plan.items.len() as i64,
                created_at_ms
            ],
        )
        .map_err(|e| e.to_string())?;
        let seq = tx.last_insert_rowid();
        for item in &plan.items {
            tx.execute("INSERT INTO review_reopen_members(round_seq,segment_id,pool_decision_floor,adjudication_floor,
                review_event_floor,expected_revision,target_revision,evidence_sha256,priority) VALUES(?1,?2,?3,?4,?5,?6,?6+1,?7,?8)",
                rusqlite::params![seq,item.segment_id,item.pool_decision_floor,item.adjudication_floor,item.review_event_floor,
                    item.revision,item.evidence_sha256,plan.priority]).map_err(|e|format!("reopen target refused: {e}"))?;
            let changed = tx
                .execute(
                    "UPDATE speech_segments SET review_revision=review_revision+1 WHERE id=?1 AND review_revision=?2",
                    rusqlite::params![item.segment_id, item.revision],
                )
                .map_err(|e| e.to_string())?;
            if changed != 1 {
                return Err("reopen revision changed during apply".into());
            }
        }
        validate_on(&tx)?;
        tx.commit().map_err(|e| format!("reopen commit failed: {e}"))?;
        Ok(plan.items.len())
    })
}

/// Startup/restore must not accept a partial or tampered round as authoritative.
pub(super) fn validate_on(conn: &rusqlite::Connection) -> Result<(), String> {
    if !supported_on(conn)? {
        return Ok(());
    }
    let mut s=conn.prepare("SELECT id,round_id,pool_id,plan_sha256,plan_json,reason,member_count FROM review_reopen_rounds ORDER BY id")
        .map_err(|e|e.to_string())?;
    let rows = s
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (seq, id, pool, hash, json, reason, count) = row.map_err(|e| e.to_string())?;
        let plan: ReopenPlan = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        canonical_uuid(&plan.round_id, "stored reopen round id")?;
        if plan.round_id != id
            || plan.version != 1
            || plan.priority > 2
            || plan.items.is_empty()
            || plan.items.len() > 10000
            || plan.items.windows(2).any(|p| p[0].segment_id >= p[1].segment_id)
            || plan.pool_id != pool
            || plan.plan_sha256 != hash
            || digest(&plan)? != hash
            || plan.reason != reason
            || plan.items.len() as i64 != count
        {
            return Err("reopen round provenance is invalid".into());
        }
        let actual: i64 = conn
            .query_row("SELECT COUNT(*) FROM review_reopen_members WHERE round_seq=?1", [seq], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if actual != count {
            return Err("reopen round membership is incomplete".into());
        }
        for i in &plan.items {
            let exact: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM review_reopen_members m JOIN speech_segments s ON s.id=m.segment_id
                WHERE m.round_seq=?1 AND m.segment_id=?2 AND m.pool_decision_floor=?3 AND m.adjudication_floor=?4
                  AND m.review_event_floor=?5 AND m.expected_revision=?6 AND m.target_revision=?6+1
                  AND m.evidence_sha256=?7 AND m.priority=?8 AND s.review_revision>=m.target_revision)",
                    rusqlite::params![
                        seq,
                        i.segment_id,
                        i.pool_decision_floor,
                        i.adjudication_floor,
                        i.review_event_floor,
                        i.revision,
                        i.evidence_sha256,
                        plan.priority
                    ],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if !exact {
                return Err("reopen member provenance or revision is invalid".into());
            }
        }
    }
    Ok(())
}
