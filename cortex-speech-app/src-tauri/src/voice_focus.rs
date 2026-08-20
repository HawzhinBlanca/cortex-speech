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
//! Semantics (owner instruction 2026-08-20 — present-but-broken policy files fail CLOSED):
//!   * a MISSING file is "no focus" — every reviewer sees the full queue, exactly as before;
//!   * a file that EXISTS but is unreadable, not JSON, or names no ids is `Err` — the queue serves
//!     NOTHING until it is fixed. A file on disk is expressed intent; honouring "unrestricted" when
//!     that intent cannot be read silently points eight paid reviewers at 34 h of audio the owner is
//!     not collecting. Stopped-and-loud beats wrong-and-quiet: the reviewer sees a retryable error,
//!     the owner fixes the file, the very next fetch works;
//!   * junk entries INSIDE a list that still holds real ids are dropped, not fatal — dropping them
//!     only narrows further, which is the safe direction;
//!   * re-read on every queue fetch, so the owner edits the file and the next refill obeys it.
//!
//! The speaker's NAME lives only in this data-dir file and in what the export writes. It never enters
//! tracked code — the repo is public and names stay out of it (hygiene gate).

use std::collections::HashSet;
use std::path::Path;

pub const VOICE_FOCUS_FILE: &str = "voice_focus.json";

/// The set of segment ids a reviewer may currently be served.
///
/// `Ok(None)` = no file, no restriction. `Ok(Some(ids))` = active focus.
/// `Err` = the file EXISTS but cannot be honoured — the caller must serve nothing, not everything.
pub fn load_focus(data_dir: &Path) -> Result<Option<HashSet<String>>, String> {
    let path = data_dir.join(VOICE_FOCUS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            tracing::error!(
                "{VOICE_FOCUS_FILE} exists but is unreadable ({e}) — queues serve NOTHING until it is fixed"
            );
            return Err(format!("{VOICE_FOCUS_FILE} is unreadable: {e}"));
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("{VOICE_FOCUS_FILE} is not valid JSON ({e}) — queues serve NOTHING until it is fixed");
            return Err(format!("{VOICE_FOCUS_FILE} is not valid JSON: {e}"));
        }
    };
    let ids: HashSet<String> = parsed
        .get("segment_ids")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        tracing::error!("{VOICE_FOCUS_FILE} present but names no segment ids — queues serve NOTHING until it is fixed");
        return Err(format!("{VOICE_FOCUS_FILE} names no segment ids"));
    }
    tracing::info!(
        "voice focus active: {} clip(s) for {:?}",
        ids.len(),
        parsed.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)")
    );
    Ok(Some(ids))
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
        assert_eq!(load_focus(dir.path()), Ok(None), "absence must leave every queue exactly as it was");
    }

    #[test]
    fn a_named_list_restricts_to_exactly_those_ids() {
        let dir = dir_with(Some(r#"{"name":"V","segment_ids":["a","b","b"]}"#));
        let focus = load_focus(dir.path()).unwrap().expect("a real list is a focus");
        assert_eq!(focus.len(), 2);
        assert!(focus.contains("a") && focus.contains("b"));
        assert!(!focus.contains("c"));
    }

    /// A file that EXISTS but cannot be honoured must stop the queue, never widen it. The old
    /// contract failed open here, and a corrupted focus would have silently pointed eight paid
    /// reviewers back at the full 34 h library — invisible until the money was spent. An empty
    /// queue with a retryable error is the failure the owner notices in minutes.
    #[test]
    fn malformed_or_empty_focus_fails_closed() {
        for bad in [r#"{ not json"#, r#"{"name":"V"}"#, r#"{"name":"V","segment_ids":[]}"#, r#"{"segment_ids":"a"}"#] {
            let dir = dir_with(Some(bad));
            assert!(
                load_focus(dir.path()).is_err(),
                "{bad:?} exists on disk, so it must serve NOTHING, not everything"
            );
        }
    }

    #[test]
    fn non_string_entries_are_skipped_not_fatal() {
        // Dropping junk entries only narrows the queue further — the safe direction — so a list that
        // still holds real ids stays usable.
        let dir = dir_with(Some(r#"{"name":"V","segment_ids":["a", 7, null, "b"]}"#));
        let focus = load_focus(dir.path()).unwrap().unwrap();
        assert_eq!(focus.len(), 2, "the two real ids survive; junk entries are dropped, not fatal");
    }
}
