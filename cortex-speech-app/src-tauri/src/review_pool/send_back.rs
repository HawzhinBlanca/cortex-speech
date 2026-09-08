//! Owner-only, exact-plan batch reversal. No row-by-row partial commits and no implicit pay reset.
//! This handles pool observations only; it is NOT a canonical-dispute hold or general reopen round.

use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendBackItem {
    pub decision_id: i64,
    pub segment_id: String,
    pub reviewer: String,
    pub semantic_action: String,
    pub requested_action: String,
    pub decision_operation_id: String,
    pub segment_revision: i64,
    pub evidence_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendBackPlan {
    pub version: u32,
    pub pool_id: String,
    pub focus_sha256: String,
    pub items: Vec<SendBackItem>,
    pub plan_sha256: String,
}

fn plan_hash(plan: &SendBackPlan) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(plan.version, &plan.pool_id, &plan.focus_sha256, &plan.items))
        .map_err(|error| format!("cannot encode send-back plan: {error}"))?;
    Ok(Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect())
}

fn reversal_id(plan: &SendBackPlan, item: &SendBackItem) -> String {
    let digest = Sha256::digest(format!("cortex-pool-send-back-v1:{}:{}", plan.plan_sha256, item.decision_id));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC variant, version 8 (application-defined). Stable retries cannot mint a second reversal.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn item_on(conn: &rusqlite::Connection, pool: &ReviewPool, id: i64) -> Result<SendBackItem, String> {
    let item = conn
        .query_row(
            "SELECT decision.id, decision.segment_id, decision.reviewer, decision.action,
                decision.requested_action, decision.operation_id, segment.review_revision
           FROM effective_review_pool_decisions_v62 decision
           JOIN speech_segments segment ON segment.id=decision.segment_id
          WHERE decision.id=?1 AND decision.pool_id=?2",
            rusqlite::params![id, pool.pool_id],
            |row| {
                Ok(SendBackItem {
                    decision_id: row.get(0)?,
                    segment_id: row.get(1)?,
                    reviewer: row.get(2)?,
                    semantic_action: row.get(3)?,
                    requested_action: row.get(4)?,
                    decision_operation_id: row.get(5)?,
                    segment_revision: row.get(6)?,
                    evidence_sha256: String::new(),
                })
            },
        )
        .optional()
        .map_err(|error| format!("cannot read send-back decision: {error}"))?
        .ok_or("send-back decision is missing or no longer effective")?;
    if !pool.contains(&item.segment_id) || !matches!(item.semantic_action.as_str(), "accept" | "edit") {
        return Err("send-back only permits retained pool accept/edit decisions".into());
    }
    Ok(item)
}

fn items_on(conn: &rusqlite::Connection, pool: &ReviewPool, ids: &[i64]) -> Result<Vec<SendBackItem>, String> {
    let reviewers = reviewer_sets_on(conn)?;
    let adjudications = owner_adjudications_on(conn)?;
    ids.iter()
        .map(|id| {
            let mut item = item_on(conn, pool, *id)?;
            let (_, digest) = derive_resolution(
                &item.segment_id,
                reviewers.get(&item.segment_id),
                adjudications.get(&item.segment_id),
            );
            // Bind owner rulings as well as the active opinions; neither can change after preview.
            item.evidence_sha256 = Sha256::digest(
                serde_json::to_vec(&(
                    digest,
                    adjudications.get(&item.segment_id).map(|rows| {
                        rows.iter()
                            .map(|row| (&row.evidence_sha256, row.final_outcome.digest_value()))
                            .collect::<Vec<_>>()
                    }),
                ))
                .map_err(|error| error.to_string())?,
            )
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
            Ok(item)
        })
        .collect()
}

/// Read one consistent snapshot and freeze exact effective decision IDs. Does not write the DB.
pub fn prepare_send_back_plan(db: &Database, pool: &ReviewPool, decision_ids: &[i64]) -> Result<SendBackPlan, String> {
    if decision_ids.is_empty() || decision_ids.len() > 10_000 || decision_ids.iter().any(|id| *id <= 0) {
        return Err("send-back plan requires 1..10000 positive decision IDs".into());
    }
    let mut ids = decision_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != decision_ids.len() {
        return Err("send-back plan contains duplicate decision IDs".into());
    }
    // Operator read-only handles already hold a consistent transaction. Do not nest BEGIN or
    // finish a caller-owned snapshot; writable/test handles get a local read transaction.
    let tx = if db.connection().is_autocommit() {
        Some(
            rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Deferred)
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    if !registry_matches(db, pool)? {
        return Err("send-back pool identity changed".into());
    }
    let items = items_on(db.connection(), pool, &ids)?;
    let mut plan = SendBackPlan {
        version: 1,
        pool_id: pool.pool_id.clone(),
        focus_sha256: pool.focus_sha256.clone(),
        items,
        plan_sha256: String::new(),
    };
    plan.plan_sha256 = plan_hash(&plan)?;
    if let Some(tx) = tx {
        tx.rollback().map_err(|error| error.to_string())?;
    }
    Ok(plan)
}

/// Exact retry returns the original item count. Any stale item or write/pay error rolls back ALL
/// reversals and revision changes. Existing pay policy appends signed adjustments, never erases pay.
pub fn apply_send_back_plan(
    db: &Database,
    pool: &ReviewPool,
    plan: &SendBackPlan,
    created_at_ms: i64,
    acknowledge_pay_adjustments: bool,
) -> Result<usize, String> {
    if !acknowledge_pay_adjustments {
        return Err("send-back requires explicit acknowledgment of signed pay adjustments".into());
    }
    if plan.version != 1
        || created_at_ms <= 0
        || plan.pool_id != pool.pool_id
        || plan.focus_sha256 != pool.focus_sha256
        || plan.items.is_empty()
        || plan.items.len() > 10_000
        || plan_hash(plan)? != plan.plan_sha256
        || plan.items.windows(2).any(|pair| pair[0].decision_id >= pair[1].decision_id)
    {
        return Err("send-back plan identity or digest is invalid".into());
    }
    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        if !registry_matches(db, pool)? {
            return Err("send-back pool identity changed".into());
        }
        let mut replayed = 0;
        for item in &plan.items {
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT operation_id,reviewer FROM review_pool_reversals WHERE decision_id=?1",
                    [item.decision_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            if let Some((operation, reviewer)) = existing {
                if operation != reversal_id(plan, item) || reviewer != item.reviewer {
                    return Err("send-back target was reversed by another operation".into());
                }
                replayed += 1;
            }
        }
        if replayed == plan.items.len() {
            tx.rollback().map_err(|error| error.to_string())?;
            return Ok(replayed);
        }
        if replayed != 0 {
            return Err("send-back receipt set is incomplete; operator investigation required".into());
        }
        let ids: Vec<_> = plan.items.iter().map(|item| item.decision_id).collect();
        if items_on(&tx, pool, &ids)? != plan.items {
            return Err("send-back plan is stale; prepare a new exact preview".into());
        }
        for item in &plan.items {
            let operation = reversal_id(plan, item);
            require_pool_operation_namespace_on(&tx, &operation, true)?;
            tx.execute("INSERT INTO review_pool_reversals(decision_id,operation_id,reviewer,created_at_ms) VALUES(?1,?2,?3,?4)",
                rusqlite::params![item.decision_id, operation, item.reviewer, created_at_ms])
                .map_err(|error| format!("send-back reversal refused: {error}"))?;
            Database::append_review_pool_compensation_reversal_tx(&tx, item.decision_id, &operation)
                .map_err(|error| format!("send-back pay adjustment refused: {error}"))?;
        }
        // A cached old page/playback receipt cannot submit into the reopened state. Do not change
        // audio or transcript columns, and bump once per distinct clip, not once per opinion.
        let mut advanced = HashSet::new();
        for item in &plan.items {
            if advanced.insert(&item.segment_id) {
                let changed = tx.execute("UPDATE speech_segments SET review_revision=review_revision+1 WHERE id=?1 AND review_revision=?2",
                    rusqlite::params![item.segment_id, item.segment_revision]).map_err(|error| error.to_string())?;
                if changed != 1 {
                    return Err("send-back revision changed during apply".into());
                }
            }
        }
        tx.commit().map_err(|error| format!("send-back commit failed: {error}"))?;
        Ok(plan.items.len())
    })
}
