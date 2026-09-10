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
    let roots = family_roots(conn)?;
    let reopened: HashSet<String> = if super::reopen::supported_on(conn)? {
        let mut statement =
            conn.prepare("SELECT segment_id FROM current_review_reopen_members_v71").map_err(|e| e.to_string())?;
        let rows = statement.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    } else {
        HashSet::new()
    };
    let mut seen: HashMap<String, HashSet<String>> = HashMap::new();
    for (id, coverage) in reviewers {
        // Reopening the retained family root authorizes fresh listening even after exposure to a
        // retired twin. The twin itself remains excluded and never contributes a transferred vote.
        // Only a RETIRED twin's exposure is dropped: `family_roots` also maps a live root to itself,
        // and dropping the root's own coverage re-served every reopened clip with twins to the
        // reviewer who had just judged it (live incident 2026-09-08, 18 of 25 verdicts came back).
        if roots.get(id).is_some_and(|root| root != id && reopened.contains(root)) {
            // A disputed ordinary opinion may be rechecked, but reopening a different cut must
            // not erase the owner's exposure. Do not transfer text across unequal boundaries.
            seen.entry(roots[id].clone())
                .or_default()
                .extend(coverage.seen.iter().filter(|reviewer| super::trust::is_owner(reviewer)).cloned());
            continue;
        }
        seen.entry(roots.get(id).unwrap_or(id).clone()).or_default().extend(coverage.seen.iter().cloned());
    }
    Ok(seen)
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
    let reviewers = reviewer_sets_on(conn)?;
    let seen = family_seen_on(conn, &reviewers)?;
    let key = reviewer_key(Some(reviewer));
    if seen.get(segment_id).is_some_and(|members| members.contains(&key)) {
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

#[cfg(test)]
mod owner_exposure_tests {
    use super::*;

    #[test]
    fn reopening_a_root_preserves_its_retired_twins_owner_exposure_only() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations(version INTEGER); INSERT INTO schema_migrations VALUES(71);
            CREATE TABLE review_pool_registry(pool_id TEXT); INSERT INTO review_pool_registry VALUES('pool');
            CREATE TABLE review_pool_duplicate_exclusions(pool_id TEXT,segment_id TEXT,canonical_segment_id TEXT);
            INSERT INTO review_pool_duplicate_exclusions VALUES('pool','twin','root');
            CREATE TABLE current_review_reopen_members_v71(segment_id TEXT);
            INSERT INTO current_review_reopen_members_v71 VALUES('root');",
        )
        .unwrap();
        let mut coverage = SegmentReviewers::default();
        coverage.seen.extend(["hawzhin".to_string(), "rubar".to_string()]);
        let reviewers = HashMap::from([("twin".to_string(), coverage)]);
        let policy = super::super::trust::parse(r#"{"owner":"Hawzhin","trusted":[]}"#).unwrap();
        super::super::trust::with_policy(policy, || {
            assert_eq!(family_seen_on(&conn, &reviewers).unwrap()["root"], HashSet::from(["hawzhin".to_string()]));
        });
    }
}
