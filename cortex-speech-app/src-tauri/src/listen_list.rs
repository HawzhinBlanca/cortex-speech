//! Owner listen list: clips a named reviewer should hear FIRST on their own link.
//!
//! `<data_dir>/review_listen_list.json`:
//!
//! ```json
//! { "_comment": "clips the owner wants to judge himself", "Hawzhin": ["lamo_016604", "9658128f-d570-4cdb-9359-c1fbbf7f3bc1"] }
//! ```
//!
//! Entries are pool clip ids or audio file basenames (extension optional, ASCII case-insensitive).
//! Why it exists (owner, 2026-09-06): a review verdict is only ever written by the human who played
//! the clip, so when the owner hears a fault in a specific recording the honest way to record his
//! judgement is to bring that clip to his own reviewer link next. The list re-orders; it never
//! decides. A listed clip still passes every queue rule — pool membership, duplicate exclusion,
//! dialect, resolution, and never a clip the reviewer already judged.
//!
//! A missing file prioritises nothing. A present-but-unreadable file is logged and IGNORED, unlike
//! the dialect roster: this list is a priority hint, not a restriction, so failing open cannot take
//! work away from anyone or route wrong-dialect work; failing closed would stop ten reviewers over a
//! typo in a file that only ever affects one queue's order.

use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const FILE_NAME: &str = "review_listen_list.json";

/// The whole file, reviewer → entries. Missing or broken → empty (logged when broken).
pub fn load(data_dir: &Path) -> HashMap<String, Vec<String>> {
    let path = data_dir.join(FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(error) => {
            tracing::error!("{FILE_NAME} exists but is unreadable ({error}); the listen list is ignored");
            return HashMap::new();
        }
    };
    match parse(&text) {
        Ok(list) => list,
        Err(error) => {
            tracing::error!("{FILE_NAME} is ignored: {error}");
            HashMap::new()
        }
    }
}

/// Strict parser: every non-comment key must hold a list of strings.
pub fn parse(text: &str) -> Result<HashMap<String, Vec<String>>, String> {
    let parsed: HashMap<String, serde_json::Value> =
        serde_json::from_str(text).map_err(|error| format!("not valid JSON: {error}"))?;
    let mut list = HashMap::new();
    for (name, value) in parsed {
        if name.starts_with('_') {
            continue;
        }
        let entries: Option<Vec<String>> = value
            .as_array()
            .and_then(|items| items.iter().map(|item| item.as_str().map(|s| s.trim().to_string())).collect());
        match entries {
            Some(entries) => {
                list.insert(name, entries.into_iter().filter(|entry| !entry.is_empty()).collect());
            }
            None => return Err(format!("entry \"{name}\" must be a list of clip ids or file names")),
        }
    }
    Ok(list)
}

/// Matched the way the session layer matches reviewer names: trim + ASCII case.
pub fn entries_for<'a>(list: &'a HashMap<String, Vec<String>>, reviewer: &str) -> Option<&'a Vec<String>> {
    let want = reviewer.trim();
    list.iter().find(|(name, _)| name.trim().eq_ignore_ascii_case(want)).map(|(_, entries)| entries)
}

/// Lower-case, and strip a known audio extension so `lamo_016604` and `LAMO_016604.wav` are one name.
fn normalized_name(value: &str) -> String {
    let lower = value.trim().to_ascii_lowercase();
    match lower.rsplit_once('.') {
        Some((stem, "wav" | "mp3" | "flac" | "m4a" | "ogg")) => stem.to_string(),
        _ => lower,
    }
}

/// Resolve entries against `(clip id, audio path)` pairs: an entry names a clip by exact id or by the
/// audio file's basename.
pub fn resolve<'a>(entries: &[String], members: impl Iterator<Item = (&'a String, &'a String)>) -> HashSet<String> {
    let wanted: Vec<String> = entries.iter().map(|entry| normalized_name(entry)).collect();
    if wanted.is_empty() {
        return HashSet::new();
    }
    let mut listed = HashSet::new();
    for (id, path) in members {
        let base = normalized_name(Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(""));
        let lower_id = id.to_ascii_lowercase();
        if wanted.iter().any(|want| *want == lower_id || (!base.is_empty() && *want == base)) {
            listed.insert(id.clone());
        }
    }
    listed
}

/// The pool clips `reviewer` should hear first, per the file in `data_dir`.
pub fn listen_first_for(data_dir: &Path, reviewer: &str, pool: &crate::review_pool::ReviewPool) -> HashSet<String> {
    let list = load(data_dir);
    match entries_for(&list, reviewer) {
        Some(entries) => resolve(entries, pool.member_audio_paths()),
        None => HashSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_comment_keys_and_refuses_a_non_list_entry() {
        let list = parse(r#"{ "_comment": "x", "Hawzhin": [" lamo_016604 ", "", "abc"] }"#).unwrap();
        assert_eq!(list.get("Hawzhin"), Some(&vec!["lamo_016604".to_string(), "abc".to_string()]));
        assert!(parse(r#"{ "Roza": "lamo_1" }"#).unwrap_err().contains("must be a list"));
        assert!(parse("not json").unwrap_err().contains("not valid JSON"));
    }

    #[test]
    fn entries_match_ids_and_basenames_case_insensitively_with_or_without_extension() {
        let members = [
            ("13005a14-aaaa".to_string(), r"D:\ZAR_Lamo\wavs\lamo_016604.wav".to_string()),
            ("9658128f-bbbb".to_string(), "/halwest/wavs/halwest_003313.wav".to_string()),
            ("other".to_string(), "/x/lamo_000001.wav".to_string()),
        ];
        let entries = vec!["LAMO_016604".to_string(), "9658128F-BBBB".to_string(), "halwest_003313.WAV".to_string()];
        let listed = resolve(&entries, members.iter().map(|(id, path)| (id, path)));
        assert_eq!(listed, ["13005a14-aaaa".to_string(), "9658128f-bbbb".to_string()].into_iter().collect());
        assert!(resolve(&[], members.iter().map(|(id, path)| (id, path))).is_empty());
    }

    #[test]
    fn reviewer_lookup_is_trimmed_and_ascii_case_insensitive() {
        let list = parse(r#"{ "Hawzhin ": ["a"] }"#).unwrap();
        assert!(entries_for(&list, "hawzhin").is_some());
        assert!(entries_for(&list, "Roza").is_none());
    }

    #[test]
    fn a_missing_or_broken_file_prioritises_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_empty());
        std::fs::write(dir.path().join(FILE_NAME), "{ broken").unwrap();
        assert!(load(dir.path()).is_empty(), "a broken listen list is ignored, never a reason to stop the queue");
    }
}
