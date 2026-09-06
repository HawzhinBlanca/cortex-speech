//! Effective reviewer exposure across immutable duplicate-family links.
//!
//! This module owns exposure identity only. Transcript consensus, compensation and operation
//! receipts stay with their original decisions; callers enforce this guard inside their write
//! transaction before any decision, payment or playback-consumption effect.

use super::{reviewer_key, reviewer_sets_on, SegmentReviewers};
use std::collections::{HashMap, HashSet};

/// Merge only exposure, NEVER transcript opinions, across the active acoustic family. Historical
/// judgments/credits remain bound to the original clip. Effective views make undo reversible.
pub(super) fn family_seen_on(
    conn: &rusqlite::Connection,
    reviewers: &HashMap<String, SegmentReviewers>,
) -> Result<HashMap<String, HashSet<String>>, String> {
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
    let mut seen: HashMap<String, HashSet<String>> = HashMap::new();
    for (id, coverage) in reviewers {
        seen.entry(roots.get(id).unwrap_or(id).clone()).or_default().extend(coverage.seen.iter().cloned());
    }
    Ok(seen)
}

/// Transaction-bound guard shared by attributed first opinions and independent opinions. This is
/// prospective: no rewritten judgments, transferred agreement, or retroactive payment reversal.
pub(crate) fn require_unseen_pool_family_on(
    conn: &rusqlite::Connection,
    segment_id: &str,
    reviewer: &str,
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
    let seen = family_seen_on(conn, &reviewer_sets_on(conn)?)?;
    if seen.get(segment_id).is_some_and(|members| members.contains(&reviewer_key(Some(reviewer)))) {
        return Err(
            "E_REVIEW_FAMILY_ALREADY_SEEN: review pool decision is duplicated for this reviewer (recording family)"
                .into(),
        );
    }
    Ok(())
}
