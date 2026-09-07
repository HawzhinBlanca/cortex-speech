//! Redo pass (owner instruction 2026-09-07): a named reviewer is served ONLY the clips they
//! themselves judged, so they correct their own work — starting with "Looks good".
//!
//! Two kinds of clip reach the redo queue:
//!
//! 1. The reviewer's own CANONICAL verdicts (their first opinions). Served with the text they
//!    approved; their new verdict goes through the ordinary canonical writer as their UPDATED opinion
//!    — one reviewer, one opinion, never a second opinion for consensus — paid at the standard
//!    weights. The owner chose that knowingly ("its okay if the app says more payment and counts it,
//!    but we wanna make sure we give them the exact set that they chose Looks Good"): an unpaid redo
//!    would need a new evidence table, because every review event after the pay cutoff must carry
//!    exactly one ledger credit and the restore validator refuses anything else.
//! 2. The reviewer's POOL decisions (their second opinions) that the owner SENT BACK
//!    (`pool_admin send-back`): each is reversed append-only with its pay reversal, exactly as the
//!    phone's own undo does, so the reviewer is no longer "seen" on the clip and can judge it again —
//!    blind, with the raw draft, as a fresh pool decision. Owner rule change 2026-09-07 ("i want to
//!    make rubar's work all second pass … even if we lose some of her work. change the rule now so
//!    we can have those flexibility"). The consensus canon itself is untouched: a clip is still
//!    decided by any two DIFFERENT reviewers, and a reversed decision never counts.
//!
//! A clip the reviewer re-judged since `started_at_ms` (any action, skip included) leaves the queue.
//!
//! `<data_dir>/review_redo.json`:
//!
//! ```json
//! { "_comment": "redo pass", "started_at_ms": 1788700000000, "redo": ["Sara", "Hemn"],
//!   "actions": ["accept"] }
//! ```
//!
//! `actions` (optional, default `["accept"]`) names which of the reviewer's verdict kinds are in
//! scope: `accept` and/or `edit`. A missing file changes nothing. A file that EXISTS but cannot be
//! honoured is `Err`, and the caller serves NOTHING (owner instruction 2026-08-20: a present-but-
//! broken policy file fails CLOSED). Unlike the listen list and difficulty routing this file is a
//! RESTRICTION: failing open would hand a paused reviewer the ordinary paid work the file was written
//! to withhold.

use crate::db::Database;
use crate::review_pool::ReviewPool;
use sha2::{Digest, Sha256};
use std::path::Path;

pub const FILE_NAME: &str = "review_redo.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedoPolicy {
    /// Unix milliseconds; a reviewer's own verdicts at or after this instant count as redo work
    /// already done and take the clip out of their redo queue.
    pub started_at_ms: i64,
    pub reviewers: Vec<String>,
    /// Verdict kinds in scope: `accept` and/or `edit`.
    pub actions: Vec<String>,
}

pub fn load(data_dir: &Path) -> Result<Option<RedoPolicy>, String> {
    let path = data_dir.join(FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            tracing::error!("{FILE_NAME} exists but is unreadable ({error}) — queues serve NOTHING until it is fixed");
            return Err(format!("{FILE_NAME} is unreadable: {error}"));
        }
    };
    parse(&text).map(Some).map_err(|error| {
        tracing::error!("{FILE_NAME} is invalid ({error}) — queues serve NOTHING until it is fixed");
        error
    })
}

/// Strict parser: `started_at_ms` (positive integer) and `redo` (list of names) are required;
/// `actions` is optional (`accept`/`edit`, default accept); keys starting with `_` are comments;
/// anything else is a typo and therefore an error.
pub fn parse(text: &str) -> Result<RedoPolicy, String> {
    let parsed: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(text).map_err(|error| format!("{FILE_NAME} is not a JSON object: {error}"))?;
    for key in parsed.keys() {
        if !key.starts_with('_') && !matches!(key.as_str(), "started_at_ms" | "redo" | "actions") {
            return Err(format!(
                "{FILE_NAME}: unknown key \"{key}\" (only started_at_ms, redo and actions are understood)"
            ));
        }
    }
    let started_at_ms = parsed
        .get("started_at_ms")
        .and_then(serde_json::Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{FILE_NAME}: \"started_at_ms\" must be a positive integer (unix milliseconds)"))?;
    let string_list = |key: &str| -> Option<Vec<String>> {
        parsed.get(key).and_then(serde_json::Value::as_array).and_then(|items| {
            items.iter().map(|item| item.as_str().map(|s| s.trim().to_string())).collect::<Option<Vec<_>>>()
        })
    };
    let reviewers =
        string_list("redo").ok_or_else(|| format!("{FILE_NAME}: \"redo\" must be a list of reviewer names"))?;
    if reviewers.iter().any(String::is_empty) {
        return Err(format!("{FILE_NAME}: \"redo\" contains an empty name"));
    }
    let actions = match parsed.get("actions") {
        None => vec!["accept".to_string()],
        Some(_) => {
            let actions = string_list("actions")
                .ok_or_else(|| format!("{FILE_NAME}: \"actions\" must be a list of accept and/or edit"))?;
            if actions.is_empty() || actions.iter().any(|action| !matches!(action.as_str(), "accept" | "edit")) {
                return Err(format!("{FILE_NAME}: \"actions\" must name accept and/or edit"));
            }
            actions
        }
    };
    Ok(RedoPolicy { started_at_ms, reviewers, actions })
}

/// The policy when it names `reviewer` (trim + ASCII case, like the session layer), else None.
pub fn active_for<'a>(policy: Option<&'a RedoPolicy>, reviewer: &str) -> Option<&'a RedoPolicy> {
    let policy = policy?;
    let want = reviewer.trim();
    policy.reviewers.iter().any(|name| name.eq_ignore_ascii_case(want)).then_some(policy)
}

/// The redo start for `reviewer` when the policy names them, else None.
pub fn started_at_for(policy: Option<&RedoPolicy>, reviewer: &str) -> Option<i64> {
    active_for(policy, reviewer).map(|policy| policy.started_at_ms)
}

/// Lower-cased, trimmed reviewer identity, exactly as the stored `reviewed_by`/`reviewer` columns
/// are compared everywhere else (`review_pool::reviewer_key`).
fn key(reviewer: &str) -> String {
    reviewer.trim().to_ascii_lowercase()
}

/// SQL list literals for the verdict kinds in scope — built from a fixed vocabulary, never from
/// file text.
fn canonical_decisions_sql(actions: &[String]) -> String {
    let mut values = Vec::new();
    for action in actions {
        match action.as_str() {
            "accept" => values.extend(["'accept'", "'human_accept'"]),
            "edit" => values.extend(["'edit'", "'human_edit'"]),
            _ => {}
        }
    }
    if values.is_empty() {
        values.extend(["'accept'", "'human_accept'"]);
    }
    values.join(",")
}

fn pool_actions_sql(actions: &[String]) -> String {
    let mut values = Vec::new();
    for action in actions {
        match action.as_str() {
            "accept" => values.push("'accept'"),
            "edit" => values.push("'edit'"),
            _ => {}
        }
    }
    if values.is_empty() {
        values.push("'accept'");
    }
    values.join(",")
}

/// Is this clip in `reviewer`'s CANONICAL redo scope right now: a live pool clip whose canonical
/// verdict is this reviewer's own (of a kind in `actions`), with no pool decision of theirs on it?
/// Such a redo stays on the canonical path; everything else in the redo queue is a pool decision.
pub fn is_own_canonical_verdict(
    db: &Database,
    segment_id: &str,
    reviewer: &str,
    actions: &[String],
) -> Result<bool, String> {
    let sql = format!(
        "SELECT EXISTS(
             SELECT 1 FROM review_pool_members member
               JOIN review_pool_registry registry ON registry.pool_id=member.pool_id
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE member.segment_id=?1
                AND NOT EXISTS (SELECT 1 FROM review_pool_duplicate_exclusions x
                                 WHERE x.pool_id=member.pool_id AND x.segment_id=member.segment_id)
                AND segment.verified=1
                AND lower(trim(COALESCE(segment.reviewed_by, '')))=?2
                AND segment.human_decision IN ({decisions})
                AND NOT EXISTS (SELECT 1 FROM review_pool_decisions decision
                                 WHERE decision.segment_id=segment.id AND lower(trim(decision.reviewer))=?2))",
        decisions = canonical_decisions_sql(actions)
    );
    db.connection()
        .query_row(&sql, rusqlite::params![segment_id, key(reviewer)], |row| row.get::<_, bool>(0))
        .map_err(|error| format!("redo scope cannot be checked: {error}"))
}

/// The reviewer's redo queue, hardest first (`review_routing::difficulty_bucket`) then a
/// content-independent spread:
///   * their own canonical verdicts (kinds in `actions`) in the live pool, not re-judged since
///     `started_at_ms`;
///   * live pool clips where the owner sent back (reversed) one of their pool decisions and they
///     hold no effective decision since, provided the clip still lacks two opinions.
///
/// Playable and within their dialects, like every queue.
pub fn pending_segment_ids(
    db: &Database,
    pool: &ReviewPool,
    reviewer: &str,
    policy: &RedoPolicy,
    allowed_dialects: Option<&[String]>,
) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT segment.id, segment.audio_path, member.raw_transcript, member.duration_ms, segment.alignment_json
           FROM review_pool_members member
           JOIN speech_segments segment ON segment.id=member.segment_id
          WHERE member.pool_id=?1
            AND NOT EXISTS (SELECT 1 FROM review_pool_duplicate_exclusions x
                             WHERE x.pool_id=member.pool_id AND x.segment_id=member.segment_id)
            AND segment.verified=1
            AND lower(trim(COALESCE(segment.reviewed_by, '')))=?2
            AND segment.human_decision IN ({decisions})
            AND NOT EXISTS (SELECT 1 FROM review_pool_decisions decision
                             WHERE decision.segment_id=segment.id AND lower(trim(decision.reviewer))=?2)
            AND NOT EXISTS (SELECT 1 FROM review_events event
                             WHERE event.segment_id=segment.id AND lower(trim(event.reviewer))=?2
                               AND event.timestamp_ms>=?3)
         UNION
         SELECT segment.id, segment.audio_path, member.raw_transcript, member.duration_ms, segment.alignment_json
           FROM review_pool_reversals reversal
           JOIN review_pool_decisions decision ON decision.id=reversal.decision_id
           JOIN review_pool_members member
             ON member.pool_id=decision.pool_id AND member.segment_id=decision.segment_id
           JOIN speech_segments segment ON segment.id=member.segment_id
          WHERE member.pool_id=?1
            AND lower(trim(decision.reviewer))=?2
            AND decision.action IN ({pool_actions})
            AND NOT EXISTS (SELECT 1 FROM review_pool_duplicate_exclusions x
                             WHERE x.pool_id=member.pool_id AND x.segment_id=member.segment_id)
            AND NOT EXISTS (SELECT 1 FROM effective_review_pool_decisions_v62 effective
                             WHERE effective.segment_id=segment.id AND lower(trim(effective.reviewer))=?2)
            AND (SELECT COUNT(*) FROM effective_review_pool_decisions_v62 others
                  WHERE others.segment_id=segment.id)
                + (CASE WHEN segment.verified=1 AND segment.human_decision IS NOT NULL THEN 1 ELSE 0 END) < 2",
        decisions = canonical_decisions_sql(&policy.actions),
        pool_actions = pool_actions_sql(&policy.actions)
    );
    let mut statement =
        db.connection().prepare(&sql).map_err(|error| format!("redo queue cannot be prepared: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![pool.pool_id, key(reviewer), policy.started_at_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|error| format!("redo queue cannot be read: {error}"))?;
    let mut candidates: Vec<(u8, [u8; 32], String)> = Vec::new();
    for row in rows {
        let (segment_id, audio_path, raw_transcript, duration_ms, alignment_json) =
            row.map_err(|error| format!("redo queue row is unreadable: {error}"))?;
        if !pool.is_playable(&segment_id) || !crate::dialect::reviewer_may_judge(allowed_dialects, &audio_path) {
            continue;
        }
        let lowest = alignment_json.as_deref().and_then(crate::review_routing::min_word_confidence);
        let bucket = crate::review_routing::difficulty_bucket(&raw_transcript, duration_ms, lowest);
        let spread: [u8; 32] = Sha256::digest(segment_id.as_bytes()).into();
        candidates.push((bucket, spread, segment_id));
    }
    candidates.sort_unstable();
    candidates.dedup_by(|a, b| a.2 == b.2);
    Ok(candidates.into_iter().map(|(_, _, segment_id)| segment_id).collect())
}

/// One effective pool decision of a reviewer, as listed for `pool_admin send-back`.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SendBackCandidate {
    pub decision_id: i64,
    pub segment_id: String,
    pub action: String,
}

/// The reviewer's EFFECTIVE pool decisions of the given kinds — what `send-back` would reverse.
pub fn send_back_candidates(
    db: &Database,
    pool: &ReviewPool,
    reviewer: &str,
    actions: &[String],
) -> Result<Vec<SendBackCandidate>, String> {
    let sql = format!(
        "SELECT decision.id, decision.segment_id, decision.action
           FROM effective_review_pool_decisions_v62 decision
          WHERE decision.pool_id=?1 AND lower(trim(decision.reviewer))=?2 AND decision.action IN ({pool_actions})
          ORDER BY decision.id",
        pool_actions = pool_actions_sql(actions)
    );
    let mut statement =
        db.connection().prepare(&sql).map_err(|error| format!("send-back candidates cannot be prepared: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![pool.pool_id, key(reviewer)], |row| {
            Ok(SendBackCandidate { decision_id: row.get(0)?, segment_id: row.get(1)?, action: row.get(2)? })
        })
        .map_err(|error| format!("send-back candidates cannot be read: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|error| format!("send-back candidate is unreadable: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_strict_and_the_file_fails_closed() {
        let policy =
            parse(r#"{ "_comment": "x", "started_at_ms": 1788700000000, "redo": [" Sara ", "Hemn"] }"#).unwrap();
        assert_eq!(policy.started_at_ms, 1_788_700_000_000);
        assert_eq!(policy.reviewers, vec!["Sara".to_string(), "Hemn".to_string()]);
        assert_eq!(policy.actions, vec!["accept".to_string()], "accepts only, unless the owner widens it");
        assert_eq!(started_at_for(Some(&policy), "sara"), Some(1_788_700_000_000), "ASCII case-insensitive");
        assert_eq!(started_at_for(Some(&policy), "Roza"), None);
        assert_eq!(started_at_for(None, "Sara"), None);
        let wide = parse(r#"{ "started_at_ms": 5, "redo": ["Sara"], "actions": ["accept", "edit"] }"#).unwrap();
        assert_eq!(wide.actions, vec!["accept".to_string(), "edit".to_string()]);
        assert_eq!(canonical_decisions_sql(&wide.actions), "'accept','human_accept','edit','human_edit'");
        assert_eq!(pool_actions_sql(&wide.actions), "'accept','edit'");
        assert!(parse(r#"{ "redo": ["Sara"] }"#).unwrap_err().contains("started_at_ms"));
        assert!(parse(r#"{ "started_at_ms": 0, "redo": ["Sara"] }"#).unwrap_err().contains("positive"));
        assert!(parse(r#"{ "started_at_ms": 5, "redo": "Sara" }"#).unwrap_err().contains("list"));
        assert!(parse(r#"{ "started_at_ms": 5, "redo": [""] }"#).unwrap_err().contains("empty"));
        assert!(parse(r#"{ "started_at_ms": 5, "redo": ["Sara"], "actions": ["reject"] }"#)
            .unwrap_err()
            .contains("accept"));
        assert!(parse(r#"{ "started_at_ms": 5, "redo": ["Sara"], "actions": [] }"#).unwrap_err().contains("accept"));
        assert!(parse(r#"{ "started_at_ms": 5, "redo": [], "reviewers": ["Sara"] }"#)
            .unwrap_err()
            .contains("unknown key"));
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), None, "no file: nothing changes");
        std::fs::write(dir.path().join(FILE_NAME), "{ broken").unwrap();
        assert!(load(dir.path()).is_err(), "a broken restriction file must stop the line, not fail open");
    }
}
