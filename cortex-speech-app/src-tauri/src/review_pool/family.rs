//! Effective reviewer exposure across immutable duplicate-family links.
//!
//! This module owns exposure identity only. Transcript consensus, compensation and operation
//! receipts stay with their original decisions; callers enforce this guard inside their write
//! transaction before any decision, payment or playback-consumption effect.

use super::{reviewer_key, reviewer_sets_for_ids_on, SegmentReviewers};
use std::collections::{HashMap, HashSet};

/// Pool-wide exposure by live root, for the queue (once per fetch). Merges only exposure, NEVER
/// transcript opinions, across the active acoustic family; historical judgements and credits remain
/// bound to the original clip. Owner 2026-09-11: a retired twin's exposure counts for everyone,
/// reopened root or not (until then a reopen dropped every twin exposure but the owner's).
pub(super) fn family_seen_on(
    conn: &rusqlite::Connection,
    reviewers: &HashMap<String, SegmentReviewers>,
) -> Result<HashMap<String, HashSet<String>>, String> {
    let roots = family_roots(conn)?;
    let mut seen: HashMap<String, HashSet<String>> = HashMap::new();
    for (id, coverage) in reviewers {
        seen.entry(roots.get(id).unwrap_or(id).clone()).or_default().extend(coverage.seen.iter().cloned());
    }
    Ok(seen)
}

/// Coverage of one family only — `segment_id` and every retired member that resolves to it — and
/// the reviewers it exposed: the same rule as `family_seen_on`, read for the per-request checks
/// (media, renew, the decision writer). Loading the whole pool's coverage there cost ~35 ms of SQL
/// per audio start and per phone heartbeat (2026-09-11, 20k members, 9k exclusions); one family is
/// under 1 ms and does not grow with the pool.
pub(super) fn family_coverage_on(
    conn: &rusqlite::Connection,
    segment_id: &str,
) -> Result<(HashMap<String, SegmentReviewers>, HashSet<String>), String> {
    // Both CROSS JOINs pin the order family → registry → exclusion: only then does SQLite search the
    // (pool_id, canonical_segment_id) index (0.03 ms) instead of building an automatic index over
    // every exclusion per call (12 ms, measured 2026-09-11). UNION ends a cycle instead of looping.
    let mut statement = conn
        .prepare(
            "WITH RECURSIVE family(id) AS (
                SELECT ?1
                UNION
                SELECT exclusion.segment_id
                  FROM family
                  CROSS JOIN review_pool_registry registry
                  CROSS JOIN review_pool_duplicate_exclusions exclusion
                    ON exclusion.pool_id=registry.pool_id AND exclusion.canonical_segment_id=family.id)
             SELECT id FROM family",
        )
        .map_err(|error| format!("duplicate family identity cannot be read: {error}"))?;
    let ids: Vec<String> = statement
        .query_map([segment_id], |row| row.get(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|error| error.to_string())?;
    let ids_json = serde_json::to_string(&ids).map_err(|error| error.to_string())?;
    let reviewers = reviewer_sets_for_ids_on(conn, Some(&ids_json))?;
    let seen = reviewers.values().flat_map(|coverage| coverage.seen.iter().cloned()).collect();
    Ok((reviewers, seen))
}

/// Retired member → the live canonical clip its family resolves to (chains followed). A live
/// canonical clip is its own root and does not appear as a key.
fn family_roots(conn: &rusqlite::Connection) -> Result<HashMap<String, String>, String> {
    let mut statement = conn
        .prepare(
            "SELECT exclusion.segment_id, exclusion.canonical_segment_id
           FROM review_pool_duplicate_exclusions exclusion
           JOIN review_pool_registry registry ON registry.pool_id=exclusion.pool_id",
        )
        .map_err(|error| format!("duplicate family identity cannot be read: {error}"))?;
    let edges: HashMap<String, String> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|error| error.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|error| error.to_string())?;
    let mut roots: HashMap<String, String> = HashMap::new();
    for id in edges.keys() {
        let mut cursor = id;
        let mut path = HashSet::new();
        let root = loop {
            if let Some(root) = roots.get(cursor) {
                break root.clone();
            }
            if !path.insert(cursor.clone()) {
                return Err("duplicate family identity contains a cycle".into());
            }
            match edges.get(cursor) {
                Some(next) => cursor = next,
                None => break cursor.clone(),
            }
        };
        for member in path {
            roots.insert(member, root.clone());
        }
    }
    Ok(roots)
}

/// Redo pass (owner 2026-09-07, `review_redo.rs`): a reviewer re-recording their OWN canonical
/// verdict on `segment_id` adds no evidence identity to the family — it is the same one opinion,
/// updated. That is the only "already seen" shape allowed through: every piece of this reviewer's
/// evidence anywhere in the family must be the canonical verdict on this very clip. A pool or
/// legacy decision of theirs, or a canonical verdict on a retired twin, still refuses.
fn only_own_canonical_verdict(
    conn: &rusqlite::Connection,
    reviewers: &HashMap<String, SegmentReviewers>,
    segment_id: &str,
    key: &str,
) -> Result<bool, String> {
    let roots = family_roots(conn)?;
    for (member, coverage) in reviewers {
        let root = roots.get(member).map(String::as_str).unwrap_or(member.as_str());
        if root != segment_id || !coverage.seen.contains(key) {
            continue;
        }
        if member != segment_id {
            return Ok(false);
        }
        match coverage.judged.get(key) {
            Some(evidence) if evidence.evidence_id.starts_with("canonical:") => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// Transaction-bound guard shared by attributed first opinions and independent opinions. This is
/// prospective: no rewritten judgments, transferred agreement, or retroactive payment reversal.
/// Pool and legacy-independent recorders: one reviewer, one piece of evidence per family, no
/// exceptions.
pub(crate) fn require_unseen_pool_family_on(
    conn: &rusqlite::Connection,
    segment_id: &str,
    reviewer: &str,
) -> Result<(), String> {
    require_unseen_pool_family_impl(conn, segment_id, reviewer, false)
}

/// The CANONICAL writer only: the same rule, except that a reviewer re-recording their own canonical
/// verdict on this clip passes (redo pass, owner 2026-09-07). Never wire this into a pool recorder —
/// there the same shape would be a second evidence identity from one reviewer.
pub(crate) fn require_unseen_pool_family_or_own_canonical_on(
    conn: &rusqlite::Connection,
    segment_id: &str,
    reviewer: &str,
) -> Result<(), String> {
    require_unseen_pool_family_impl(conn, segment_id, reviewer, true)
}

fn require_unseen_pool_family_impl(
    conn: &rusqlite::Connection,
    segment_id: &str,
    reviewer: &str,
    allow_own_canonical_update: bool,
) -> Result<(), String> {
    let active: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM review_pool_members member
          JOIN review_pool_registry registry ON registry.pool_id=member.pool_id WHERE member.segment_id=?1)",
            [segment_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("review pool membership cannot be checked: {error}"))?;
    if !active {
        return Ok(());
    }
    let excluded: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM review_pool_duplicate_exclusions WHERE segment_id=?1)",
            [segment_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if excluded {
        return Err("E_REVIEW_FAMILY_RETIRED: this duplicate clip is no longer reviewable".into());
    }
    let (reviewers, seen) = family_coverage_on(conn, segment_id)?;
    let key = reviewer_key(Some(reviewer));
    if seen.contains(&key) {
        if allow_own_canonical_update && only_own_canonical_verdict(conn, &reviewers, segment_id, &key)? {
            return Ok(());
        }
        return Err(
            "E_REVIEW_FAMILY_ALREADY_SEEN: review pool decision is duplicated for this reviewer (recording family)"
                .into(),
        );
    }
    Ok(())
}
