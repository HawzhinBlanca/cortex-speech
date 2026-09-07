//! Redo pass (owner instruction 2026-09-07): a named reviewer is served ONLY the clips they
//! themselves marked "Looks good" on the canonical path, so they correct their own work.
//!
//! Each redo verdict goes through the ordinary canonical writer (`couch/decisions.rs`): it is that
//! reviewer's UPDATED opinion on the clip — one reviewer, one opinion, never a second opinion for
//! consensus — and it is paid at the standard weights. The owner chose that knowingly ("its okay if
//! the app says more payment and counts it, but we wanna make sure we give them the exact set that
//! they chose Looks Good"): an unpaid redo would need a new evidence table, because every review
//! event after the pay cutoff must carry exactly one ledger credit and the restore validator refuses
//! anything else.
//!
//! Scope = the reviewer's own canonical accepts among live pool clips. A "Looks good" given as a
//! POOL decision is out of scope by design: pool evidence is append-only and one reviewer judges a
//! clip once there; re-judging it canonically would give the pool two pieces of evidence from one
//! reviewer, which `reviewer_sets` refuses for everyone. A clip the reviewer re-judged since
//! `started_at_ms` (any action, skip included) leaves the queue.
//!
//! `<data_dir>/review_redo.json`:
//!
//! ```json
//! { "_comment": "redo pass", "started_at_ms": 1788700000000, "redo": ["Sara", "Hemn"] }
//! ```
//!
//! A missing file changes nothing. A file that EXISTS but cannot be honoured is `Err`, and the
//! caller serves NOTHING (owner instruction 2026-08-20: a present-but-broken policy file fails
//! CLOSED). Unlike the listen list and difficulty routing this file is a RESTRICTION: failing open
//! would hand a paused reviewer the ordinary paid work the file was written to withhold.

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
/// keys starting with `_` are comments; anything else is a typo and therefore an error.
pub fn parse(text: &str) -> Result<RedoPolicy, String> {
    let parsed: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(text).map_err(|error| format!("{FILE_NAME} is not a JSON object: {error}"))?;
    for key in parsed.keys() {
        if !key.starts_with('_') && key != "started_at_ms" && key != "redo" {
            return Err(format!("{FILE_NAME}: unknown key \"{key}\" (only started_at_ms and redo are understood)"));
        }
    }
    let started_at_ms = parsed
        .get("started_at_ms")
        .and_then(serde_json::Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{FILE_NAME}: \"started_at_ms\" must be a positive integer (unix milliseconds)"))?;
    let reviewers = parsed
        .get("redo")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items.iter().map(|item| item.as_str().map(|s| s.trim().to_string())).collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| format!("{FILE_NAME}: \"redo\" must be a list of reviewer names"))?;
    if reviewers.iter().any(String::is_empty) {
        return Err(format!("{FILE_NAME}: \"redo\" contains an empty name"));
    }
    Ok(RedoPolicy { started_at_ms, reviewers })
}

/// The redo start for `reviewer` when the policy names them (trim + ASCII case, like the session
/// layer), else None.
pub fn started_at_for(policy: Option<&RedoPolicy>, reviewer: &str) -> Option<i64> {
    let policy = policy?;
    let want = reviewer.trim();
    policy.reviewers.iter().any(|name| name.eq_ignore_ascii_case(want)).then_some(policy.started_at_ms)
}

/// Lower-cased, trimmed reviewer identity, exactly as the stored `reviewed_by`/`reviewer` columns
/// are compared everywhere else (`review_pool::reviewer_key`).
fn key(reviewer: &str) -> String {
    reviewer.trim().to_ascii_lowercase()
}

/// Is this clip in `reviewer`'s redo scope right now: a live pool clip whose canonical verdict is
/// this reviewer's own "Looks good", with no pool decision of theirs on it?
pub fn is_own_canonical_accept(db: &Database, segment_id: &str, reviewer: &str) -> Result<bool, String> {
    db.connection()
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM review_pool_members member
                   JOIN review_pool_registry registry ON registry.pool_id=member.pool_id
                   JOIN speech_segments segment ON segment.id=member.segment_id
                  WHERE member.segment_id=?1
                    AND NOT EXISTS (SELECT 1 FROM review_pool_duplicate_exclusions x
                                     WHERE x.pool_id=member.pool_id AND x.segment_id=member.segment_id)
                    AND segment.verified=1
                    AND lower(trim(COALESCE(segment.reviewed_by, '')))=?2
                    AND segment.human_decision IN ('accept','human_accept')
                    AND NOT EXISTS (SELECT 1 FROM review_pool_decisions decision
                                     WHERE decision.segment_id=segment.id AND lower(trim(decision.reviewer))=?2))",
            rusqlite::params![segment_id, key(reviewer)],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| format!("redo scope cannot be checked: {error}"))
}

/// The reviewer's redo queue: their own canonical "Looks good" clips in the live pool that they
/// have not re-judged since `started_at_ms`, playable, within their dialects, hardest first
/// (`review_routing::difficulty_bucket`) then a content-independent spread.
pub fn pending_segment_ids(
    db: &Database,
    pool: &ReviewPool,
    reviewer: &str,
    started_at_ms: i64,
    allowed_dialects: Option<&[String]>,
) -> Result<Vec<String>, String> {
    let mut statement = db
        .connection()
        .prepare(
            "SELECT segment.id, segment.audio_path, member.raw_transcript, member.duration_ms, segment.alignment_json
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE member.pool_id=?1
                AND NOT EXISTS (SELECT 1 FROM review_pool_duplicate_exclusions x
                                 WHERE x.pool_id=member.pool_id AND x.segment_id=member.segment_id)
                AND segment.verified=1
                AND lower(trim(COALESCE(segment.reviewed_by, '')))=?2
                AND segment.human_decision IN ('accept','human_accept')
                AND NOT EXISTS (SELECT 1 FROM review_pool_decisions decision
                                 WHERE decision.segment_id=segment.id AND lower(trim(decision.reviewer))=?2)
                AND NOT EXISTS (SELECT 1 FROM review_events event
                                 WHERE event.segment_id=segment.id AND lower(trim(event.reviewer))=?2
                                   AND event.timestamp_ms>=?3)",
        )
        .map_err(|error| format!("redo queue cannot be prepared: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![pool.pool_id, key(reviewer), started_at_ms], |row| {
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
    Ok(candidates.into_iter().map(|(_, _, segment_id)| segment_id).collect())
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
        assert_eq!(started_at_for(Some(&policy), "sara"), Some(1_788_700_000_000), "ASCII case-insensitive");
        assert_eq!(started_at_for(Some(&policy), "Roza"), None);
        assert_eq!(started_at_for(None, "Sara"), None);
        assert!(parse(r#"{ "redo": ["Sara"] }"#).unwrap_err().contains("started_at_ms"));
        assert!(parse(r#"{ "started_at_ms": 0, "redo": ["Sara"] }"#).unwrap_err().contains("positive"));
        assert!(parse(r#"{ "started_at_ms": 5, "redo": "Sara" }"#).unwrap_err().contains("list"));
        assert!(parse(r#"{ "started_at_ms": 5, "redo": [""] }"#).unwrap_err().contains("empty"));
        assert!(parse(r#"{ "started_at_ms": 5, "redo": [], "reviewers": ["Sara"] }"#)
            .unwrap_err()
            .contains("unknown key"));
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), None, "no file: nothing changes");
        std::fs::write(dir.path().join(FILE_NAME), "{ broken").unwrap();
        assert!(load(dir.path()).is_err(), "a broken restriction file must stop the line, not fail open");
    }
}
