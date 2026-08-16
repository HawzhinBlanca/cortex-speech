//! Which Kurdish dialect a recording is in, and which dialects a reviewer is competent to judge.
//!
//! Dialect is a property of the RECORDING, not of the clip: 13,797 clips come from 32 source files,
//! so the map lives at the source and there is no per-row column to disagree with itself.
//!
//! Owner instruction 2026-08-16: reviewers who do not know Hawleri must not be handed Hawleri.
//! Roza, Alle and Sabat had already made 13 decisions on Hawleri clips purely because the queue had
//! nothing else in it — that is the failure this exists to stop.
//!
//! FAILS CLOSED. An unmapped source has dialect `None`, and a reviewer restricted to specific
//! dialects is never served it. A silent mislabel is worse than a missing label: the whole point is
//! that someone judging a dialect they do not speak produces confident wrong verdicts, and those are
//! indistinguishable from good ones downstream.

use std::collections::HashMap;
use std::path::Path;

pub const HAWLERI: &str = "hawleri";
pub const SORANI: &str = "sorani";

/// Path fragment → dialect. Deliberately EXPLICIT, never a guess from a filename shape: adding a
/// corpus means adding a line here, and until then its clips are unmapped and go to nobody who is
/// restricted. Badini and Sorani-Slemani get their entries when such a corpus is actually imported —
/// neither exists in the library today.
const SOURCE_DIALECTS: &[(&str, &str)] = &[
    // "Kawa ba Hawlery" podcast, all 32 episodes — owner-confirmed 2026-08-16 as fully Hawleri.
    ("KBHP", HAWLERI),
    // The original corpus. Its audio is currently missing (see RELIABILITY_FINDINGS_2026-08-15.md),
    // so this maps nothing today and starts working again the moment those files are restored.
    ("SoraniVoice_PC_", SORANI),
];

/// The dialect of the recording this clip was cut from, or `None` when the source is not mapped.
pub fn dialect_of(audio_path: &str) -> Option<&'static str> {
    let name = Path::new(audio_path).file_name().and_then(|n| n.to_str()).unwrap_or(audio_path);
    // Filename first (it identifies the recording), then the full path (for corpora distinguished by
    // the folder they were imported from rather than by their file names).
    SOURCE_DIALECTS
        .iter()
        .find(|(frag, _)| name.contains(frag) || audio_path.contains(frag))
        .map(|(_, dialect)| *dialect)
}

/// Can this reviewer judge this clip? `allowed = None` means unrestricted (the owner, and any
/// reviewer with no entry in the roster) and is the behaviour every reviewer had before this existed.
pub fn reviewer_may_judge(allowed: Option<&[String]>, audio_path: &str) -> bool {
    let Some(allowed) = allowed else { return true };
    match dialect_of(audio_path) {
        Some(d) => allowed.iter().any(|a| a == d),
        // Unmapped source + a restricted reviewer = refuse. Fail closed.
        None => false,
    }
}

/// Reviewer → the dialects they may judge, read from `<data_dir>/reviewer_dialects.json`:
///
/// ```json
/// { "Rubar": ["hawleri", "sorani"], "Roza": ["sorani"] }
/// ```
///
/// A missing file, or a reviewer absent from it, means UNRESTRICTED — so adding this file cannot
/// silently take work away from someone who was already reviewing.
pub fn load_roster(data_dir: &Path) -> HashMap<String, Vec<String>> {
    let path = data_dir.join("reviewer_dialects.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return HashMap::new() };
    match serde_json::from_str::<HashMap<String, Vec<String>>>(&text) {
        Ok(map) => map,
        Err(e) => {
            // Loud, and still unrestricted: a typo in this file must not silently empty every queue.
            tracing::error!(
                "reviewer_dialects.json is not readable ({e}) — every reviewer is unrestricted until it is fixed"
            );
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kbhp_is_hawleri_and_the_original_corpus_is_sorani() {
        assert_eq!(dialect_of(r"D:\x\_batch_remaining\KBHP-EP01.wav"), Some(HAWLERI));
        // Drive-letter-free on purpose: a tracked file may not carry a local profile path
        // (test_windows_repo_hygiene), and the matcher looks at the fragment, not the prefix.
        assert_eq!(dialect_of(r"corpora\SoraniVoice_PC_\_staging_wav\A1-0050_220426.wav"), Some(SORANI));
    }

    #[test]
    fn an_unmapped_source_has_no_dialect_and_reaches_no_restricted_reviewer() {
        assert_eq!(dialect_of(r"D:\new_corpus\unknown-ep1.wav"), None);
        let sorani_only = vec![SORANI.to_string()];
        assert!(!reviewer_may_judge(Some(&sorani_only), r"D:\new_corpus\unknown-ep1.wav"));
    }

    #[test]
    fn a_sorani_only_reviewer_is_never_handed_hawleri() {
        // The exact case: Roza/Alle/Sabat made 13 decisions on Hawleri only because the queue had
        // nothing else. With a roster in place they get nothing rather than the wrong dialect.
        let sorani_only = vec![SORANI.to_string()];
        assert!(!reviewer_may_judge(Some(&sorani_only), r"D:\x\KBHP-EP07.wav"));
        assert!(reviewer_may_judge(Some(&sorani_only), r"C:\x\SoraniVoice_PC_\clip.wav"));
    }

    #[test]
    fn rubar_and_hawzhin_take_both() {
        let both = vec![HAWLERI.to_string(), SORANI.to_string()];
        assert!(reviewer_may_judge(Some(&both), r"D:\x\KBHP-EP07.wav"));
        assert!(reviewer_may_judge(Some(&both), r"C:\x\SoraniVoice_PC_\clip.wav"));
    }

    #[test]
    fn no_roster_entry_means_unrestricted_so_nobody_loses_work_by_adding_the_file() {
        assert!(reviewer_may_judge(None, r"D:\x\KBHP-EP07.wav"));
        assert!(reviewer_may_judge(None, r"D:\totally\unmapped.wav"));
    }
}
