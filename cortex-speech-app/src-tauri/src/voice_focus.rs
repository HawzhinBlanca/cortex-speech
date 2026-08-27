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
use std::sync::Arc;

use sha2::{Digest, Sha256};

pub const VOICE_FOCUS_FILE: &str = "voice_focus.json";

/// Renderer-safe identity for one exact semantic focus set. The data-dir filename, private voice
/// name and segment ids never cross IPC; callers receive only this domain-separated digest.
#[derive(Debug, Clone)]
pub struct VoiceFocusBinding {
    pub focus_id: String,
    pub segment_ids: Arc<HashSet<String>>,
}

const FOCUS_ID_PREFIX: &str = "vf1_";
const FOCUS_ID_DOMAIN: &[u8] = b"cortex.voice-focus.v1\0";

/// Deterministic, platform-independent identity for an unordered focus set. Length-prefixing every
/// UTF-8 id avoids delimiter ambiguity even for legacy ids containing whitespace or newlines.
pub fn opaque_focus_id(ids: &HashSet<String>) -> String {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(FOCUS_ID_DOMAIN);
    digest.update((sorted.len() as u64).to_be_bytes());
    for id in sorted {
        let bytes = id.as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    let hex: String = digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{FOCUS_ID_PREFIX}{hex}")
}

/// Strict public-wire validation. Accepting uppercase, shortened or prefix-free digests would create
/// aliases for one policy identity and make logs/caches unable to compare request authority exactly.
pub fn is_opaque_focus_id(value: &str) -> bool {
    value.len() == FOCUS_ID_PREFIX.len() + 64
        && value.starts_with(FOCUS_ID_PREFIX)
        && value[FOCUS_ID_PREFIX.len()..].bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The refusal a serving path must give while the focus file is present-but-broken. One string, so
/// every queue tells the reviewer the same thing and a grep finds every site that can emit it.
pub const POLICY_BROKEN_PREFIX: &str = "policy file broken, no clips served";

/// Resolve the active voice focus for a serving path, fail-CLOSED, with the refusal already worded.
///
/// THE one entry point every queue must use. `load_focus` is the raw read; this is the policy:
///
///   * `Ok(None)`      — no data dir or no file: serve the full queue, exactly as before either existed;
///   * `Ok(Some(ids))` — an active focus: serve only these clips;
///   * `Err(message)`  — the file exists but cannot be honoured: serve NOTHING, and show this message.
///
/// It exists because the alternative was proven not to work. Each serving path used to spell the
/// three-arm match out for itself, and on 2026-08-20 the owner heard guest clips on the desktop: the
/// phone queue had the logic and the desktop review page had never been given it. A policy that must
/// be re-derived per call site is a policy that will be missed at the next call site — so adding a
/// queue is now a call to this function, and forgetting it is visible as a missing call rather than
/// as a silent hole in the policy.
///
/// Deliberately NOT cached. A review flagged the repeated read as waste (a paging queue re-reads the
/// id list once per page), and a 2-second cache was tried and REVERTED: it made a file that had just
/// become broken keep serving clips for the rest of its TTL, which `a_broken_policy_file_stops_the
/// _queue_instead_of_widening_it` caught immediately. Fail-closed has to mean "on the next fetch",
/// not "soon" — the whole reason this policy exists is that pointing paid reviewers at the wrong
/// audio costs real money per minute. The read is a stat and a parse of one small file against a
/// query that already touches the library; that is not the expensive part of serving a queue.
pub fn resolve(data_dir: Option<&Path>) -> Result<Option<Arc<HashSet<String>>>, String> {
    Ok(resolve_binding(data_dir)?.map(|binding| binding.segment_ids))
}

/// Resolve one exact semantic focus snapshot plus its opaque public identity. Both values are built
/// from the same parsed bytes, so a caller can never validate one file generation and query another.
pub fn resolve_binding(data_dir: Option<&Path>) -> Result<Option<VoiceFocusBinding>, String> {
    let Some(dir) = data_dir else {
        return Ok(None); // no library on disk: nothing to narrow against
    };
    // Pre-worded, so every serving path refuses in the same words.
    Ok(load_focus(dir)
        .map_err(|e| format!("{POLICY_BROKEN_PREFIX}: {e}"))?
        .map(|ids| VoiceFocusBinding { focus_id: opaque_focus_id(&ids), segment_ids: Arc::new(ids) }))
}

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
    parse_focus_text(&text)
}

/// Strict, non-mutating parser shared by live serving and snapshot promotion/preflight.
pub(crate) fn parse_focus_text(text: &str) -> Result<Option<HashSet<String>>, String> {
    let parsed: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        tracing::error!("{VOICE_FOCUS_FILE} is not valid JSON ({e}) — queues serve NOTHING until it is fixed");
        format!("{VOICE_FOCUS_FILE} is not valid JSON: {e}")
    })?;
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
    fn a_broken_focus_refuses_and_the_owners_fix_lands_on_the_very_next_call() {
        // No staleness in either direction: a broken file refuses NOW, and the fix takes effect NOW.
        // A 2-second cache was tried here and reverted — it kept serving clips through a file that
        // had just become broken, which is the failure this policy exists to prevent.
        let dir = dir_with(Some(r#"{ not json"#));
        assert!(resolve(Some(dir.path())).is_err(), "a broken file refuses");
        assert!(
            resolve(Some(dir.path())).unwrap_err().starts_with(POLICY_BROKEN_PREFIX),
            "and refuses in the shared wording every serving path shows"
        );

        std::fs::write(dir.path().join(VOICE_FOCUS_FILE), br#"{"name":"V","segment_ids":["a"]}"#).unwrap();
        let fixed = resolve(Some(dir.path())).expect("the very next call sees the fix, with no TTL to wait out");
        assert_eq!(fixed.unwrap().len(), 1);
    }

    #[test]
    fn no_data_dir_means_no_restriction() {
        assert!(resolve(None).unwrap().is_none(), "no library on disk cannot narrow anything");
    }

    #[test]
    fn non_string_entries_are_skipped_not_fatal() {
        // Dropping junk entries only narrows the queue further — the safe direction — so a list that
        // still holds real ids stays usable.
        let dir = dir_with(Some(r#"{"name":"V","segment_ids":["a", 7, null, "b"]}"#));
        let focus = load_focus(dir.path()).unwrap().unwrap();
        assert_eq!(focus.len(), 2, "the two real ids survive; junk entries are dropped, not fatal");
    }

    #[test]
    fn opaque_id_is_set_deterministic_and_hides_private_policy_fields() {
        let first = dir_with(Some(r#"{"name":"Private Voice","segment_ids":["b","a","a"]}"#));
        let second = dir_with(Some(r#"{"name":"Different private label","segment_ids":["a","b"]}"#));
        let first_binding = resolve_binding(Some(first.path())).unwrap().unwrap();
        let second_binding = resolve_binding(Some(second.path())).unwrap().unwrap();

        assert_eq!(first_binding.focus_id, second_binding.focus_id);
        assert!(is_opaque_focus_id(&first_binding.focus_id));
        assert!(!first_binding.focus_id.contains("Private"));
        assert!(!first_binding.focus_id.contains("Voice"));
    }

    #[test]
    fn opaque_id_changes_for_any_semantic_focus_change_and_has_one_wire_form() {
        let first: HashSet<String> = ["a".to_string(), "bc".to_string()].into_iter().collect();
        let second: HashSet<String> = ["ab".to_string(), "c".to_string()].into_iter().collect();
        assert_ne!(opaque_focus_id(&first), opaque_focus_id(&second), "length prefixes prevent concatenation aliases");

        let valid = opaque_focus_id(&first);
        assert!(is_opaque_focus_id(&valid));
        assert!(!is_opaque_focus_id(valid.trim_start_matches(FOCUS_ID_PREFIX)));
        assert!(!is_opaque_focus_id(&valid.to_ascii_uppercase()));
        assert!(!is_opaque_focus_id(&format!("{valid}0")));
    }
}
