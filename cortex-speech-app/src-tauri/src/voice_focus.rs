//! Voice focus: point every reviewer's queue at ONE speaker's clips, so their hours build the set
//! the owner is collecting right now.
//!
//! Owner instruction 2026-08-19: this phase collects one podcast host's voice — clean, single-speaker —
//! for voice cloning; guests come later. Reviewing 34 hours of mixed audio to get at one voice wastes
//! paid reviewer time on clips that will not ship in this set. So the queue narrows, the library does
//! not: nothing is relabelled, nothing is deleted, and removing the file restores the full queue.
//!
//! `<data_dir>/voice_focus.json`:
//!
//! ```json
//! { "name": "Some_Voice", "segment_ids": ["<uuid>", "<uuid>", ...] }
//! ```
//!
//! Semantics chosen to match `dialect::load_roster`, for the same reasons:
//!   * a MISSING file is "no focus" — every reviewer sees the full queue, exactly as before;
//!   * an unreadable or malformed file is ALSO "no focus", and is logged loudly — a typo must not
//!     empty eight reviewers' queues;
//!   * an EMPTY `segment_ids` list is no focus either: "focus on nothing" can only be a mistake;
//!   * re-read on every queue fetch, so the owner edits the file and the next refill obeys it.
//!
//! The speaker's NAME lives only in this data-dir file and in what the export writes. It never enters
//! tracked code — the repo is public and names stay out of it (hygiene gate).

use std::collections::HashSet;
use std::path::Path;

pub const VOICE_FOCUS_FILE: &str = "voice_focus.json";

/// The set of segment ids a reviewer may currently be served, or `None` for no restriction.
pub fn load_focus(data_dir: &Path) -> Option<HashSet<String>> {
    let path = data_dir.join(VOICE_FOCUS_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("{VOICE_FOCUS_FILE} is not valid JSON ({e}) — queues are UNRESTRICTED until it is fixed");
            return None;
        }
    };
    let ids: HashSet<String> = parsed
        .get("segment_ids")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        tracing::warn!("{VOICE_FOCUS_FILE} present but names no segment ids — queues are UNRESTRICTED");
        return None;
    }
    tracing::info!(
        "voice focus active: {} clip(s) for {:?}",
        ids.len(),
        parsed.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)")
    );
    Some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(contents: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if let Some(c) = contents {
            std::fs::write(dir.path().join(VOICE_FOCUS_FILE), c).unwrap();
        }
        dir
    }

    #[test]
    fn no_file_means_no_focus() {
        let dir = dir_with(None);
        assert!(load_focus(dir.path()).is_none(), "absence must leave every queue exactly as it was");
    }

    #[test]
    fn a_named_list_restricts_to_exactly_those_ids() {
        let dir = dir_with(Some(r#"{"name":"V","segment_ids":["a","b","b"]}"#));
        let focus = load_focus(dir.path()).expect("a real list is a focus");
        assert_eq!(focus.len(), 2);
        assert!(focus.contains("a") && focus.contains("b"));
        assert!(!focus.contains("c"));
    }

    /// A typo in the file must widen the queue, never empty it: eight paid reviewers with nothing
    /// to do is the expensive failure, and "unrestricted" is the state the app had before the file.
    #[test]
    fn malformed_or_empty_focus_fails_open() {
        for bad in [r#"{ not json"#, r#"{"name":"V"}"#, r#"{"name":"V","segment_ids":[]}"#, r#"{"segment_ids":"a"}"#] {
            let dir = dir_with(Some(bad));
            assert!(load_focus(dir.path()).is_none(), "{bad:?} must not restrict anyone");
        }
    }

    #[test]
    fn non_string_entries_are_skipped_not_fatal() {
        let dir = dir_with(Some(r#"{"name":"V","segment_ids":["a", 7, null, "b"]}"#));
        let focus = load_focus(dir.path()).unwrap();
        assert_eq!(focus.len(), 2, "the two real ids survive; junk entries are dropped, not fatal");
    }
}
