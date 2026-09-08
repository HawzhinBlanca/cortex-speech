//! Who may be served a reopened clip (owner direction 2026-09-08).
//!
//! A reopen round holds a clip's old opinions and asks for fresh ones. Without this file every
//! reviewer is handed the reopened clips first, which turned one reviewer's disputed "Looks good"
//! work into everybody's queue. The owner's rule: a reopened clip goes back to the people whose
//! opinion is being re-checked (each redoes their OWN work, served blind) and to the named final
//! reviewers; nobody else sees it (owner, same day: "only herself and Hawzhin"). When the two disagree
//! the clip waits for the owner's adjudication or for a name added to `final_reviewers`; it is never
//! widened by the queue on its own.
//!
//! `<data_dir>/review_reopen_routing.json`:
//!
//! ```json
//! { "final_reviewers": ["Hawzhin"] }
//! ```
//!
//! Missing file: unrestricted (today's behaviour). Present but unreadable or invalid: the restriction
//! stays in force with NO final reviewers (logged) — the file exists to keep disputed work private,
//! so a typo must not publish it, and the redo itself keeps flowing because the held reviewers are
//! read from the round, not from this file.

use super::*;

pub const FILE_NAME: &str = "review_reopen_routing.json";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReopenRouting {
    /// Reviewer keys (`reviewer_key`) who are served every reopened clip.
    pub final_reviewers: HashSet<String>,
}

/// `None` = no file, no restriction. `Some` = restricted (own work + final reviewers + needed thirds).
pub fn load(data_dir: &Path) -> Option<ReopenRouting> {
    let text = match std::fs::read_to_string(data_dir.join(FILE_NAME)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::error!("{FILE_NAME} exists but is unreadable ({error}); reopened clips go only to the reviewers whose work they are");
            return Some(ReopenRouting::default());
        }
    };
    match parse(&text) {
        Ok(routing) => Some(routing),
        Err(error) => {
            tracing::error!(
                "{FILE_NAME} is invalid ({error}); reopened clips go only to the reviewers whose work they are"
            );
            Some(ReopenRouting::default())
        }
    }
}

pub fn parse(text: &str) -> Result<ReopenRouting, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| format!("not valid JSON: {error}"))?;
    let names = value
        .get("final_reviewers")
        .ok_or("missing \"final_reviewers\"")?
        .as_array()
        .ok_or("\"final_reviewers\" must be a list of reviewer names")?
        .iter()
        .map(|item| item.as_str().ok_or("\"final_reviewers\" must contain only strings"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ReopenRouting {
        final_reviewers: names
            .iter()
            .filter(|name| !name.trim().is_empty())
            .map(|name| reviewer_key(Some(name)))
            .collect(),
    })
}

/// Reopened clip → keys of the reviewers whose held (pre-round) opinions the round re-checks: every
/// review event at or below the round's event floor and every pool decision at or below its decision
/// floor. Evidence recorded after the round is fresh and never routes the clip back to its author.
pub(crate) fn held_reviewers_on(conn: &rusqlite::Connection) -> Result<HashMap<String, HashSet<String>>, String> {
    if !reopen::supported_on(conn)? {
        return Ok(HashMap::new());
    }
    let mut statement = conn
        .prepare(
            "SELECT m.segment_id, e.reviewer FROM current_review_reopen_members_v71 m
               JOIN review_events e ON e.segment_id=m.segment_id AND e.id<=m.review_event_floor
              WHERE e.action IN ('accept','edit','reject')
             UNION
             SELECT m.segment_id, d.reviewer FROM current_review_reopen_members_v71 m
               JOIN review_pool_decisions d ON d.segment_id=m.segment_id AND d.id<=m.pool_decision_floor",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| error.to_string())?;
    let mut held: HashMap<String, HashSet<String>> = HashMap::new();
    for row in rows {
        let (segment_id, reviewer) = row.map_err(|error| error.to_string())?;
        held.entry(segment_id).or_default().insert(reviewer_key(Some(&reviewer)));
    }
    Ok(held)
}

/// May `reviewer` (a `reviewer_key`) be served this reopened clip?
pub fn may_serve(routing: &ReopenRouting, held: Option<&HashSet<String>>, reviewer: &str) -> bool {
    routing.final_reviewers.contains(reviewer) || held.is_some_and(|held| held.contains(reviewer))
}

/// Queue helper: true when a routing file is in force, `segment_id` is reopened, and `reviewer` is
/// neither one of its held reviewers nor a final reviewer.
pub(crate) fn excludes(
    routing: Option<&ReopenRouting>,
    held: Option<&HashMap<String, HashSet<String>>>,
    reopened: &HashMap<String, u8>,
    segment_id: &str,
    reviewer: &str,
) -> bool {
    match (routing, held) {
        (Some(routing), Some(held)) => {
            reopened.contains_key(segment_id) && !may_serve(routing, held.get(segment_id), reviewer)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normalizes_names_and_refuses_the_wrong_shape() {
        let routing = parse(r#"{ "final_reviewers": [" Hawzhin ", "", "roza"] }"#).unwrap();
        assert_eq!(routing.final_reviewers, ["hawzhin".to_string(), "roza".to_string()].into_iter().collect());
        assert!(parse(r#"{ "final_reviewers": "Hawzhin" }"#).unwrap_err().contains("must be a list"));
        assert!(parse(r#"{ "final": [] }"#).unwrap_err().contains("missing"));
        assert!(parse("{ broken").unwrap_err().contains("not valid JSON"));
    }

    #[test]
    fn a_broken_file_keeps_the_restriction_and_a_missing_file_lifts_it() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), None);
        std::fs::write(dir.path().join(FILE_NAME), "{ broken").unwrap();
        assert_eq!(load(dir.path()), Some(ReopenRouting::default()), "a typo must not publish disputed work");
        std::fs::write(dir.path().join(FILE_NAME), r#"{ "final_reviewers": ["Hawzhin"] }"#).unwrap();
        assert!(load(dir.path()).unwrap().final_reviewers.contains("hawzhin"));
    }

    #[test]
    fn own_work_and_final_reviewers_are_served_nobody_else() {
        let routing = parse(r#"{ "final_reviewers": ["Hawzhin"] }"#).unwrap();
        let held: HashSet<String> = ["rubar".to_string()].into_iter().collect();
        assert!(may_serve(&routing, Some(&held), "rubar"));
        assert!(may_serve(&routing, Some(&held), "hawzhin"));
        assert!(!may_serve(&routing, Some(&held), "roza"));
        assert!(!may_serve(&routing, None, "roza"));
        let mut all_held = HashMap::new();
        all_held.insert("clip".to_string(), held);
        let reopened: HashMap<String, u8> = [("clip".to_string(), 0)].into_iter().collect();
        assert!(excludes(Some(&routing), Some(&all_held), &reopened, "clip", "roza"));
        assert!(!excludes(Some(&routing), Some(&all_held), &reopened, "clip", "rubar"));
        assert!(
            !excludes(Some(&routing), Some(&all_held), &reopened, "other", "roza"),
            "an ordinary clip is never excluded"
        );
        assert!(!excludes(None, None, &reopened, "clip", "roza"), "no routing file: today's behaviour");
    }
}
