//! Which Kurdish dialect a recording is in, and which dialects a reviewer is competent to judge.
//!
//! Dialect is a property of the RECORDING, not of the clip: 13,797 clips come from 32 source files,
//! so the map lives at the source and there is no per-row column to disagree with itself.
//!
//! Owner instruction 2026-08-16: reviewers who do not know Hawleri must not be handed Hawleri.
//! Three Sorani-only reviewers had already made 13 decisions on Hawleri clips purely because the
//! queue had nothing else in it — that is the failure this exists to stop.
//!
//! FAILS CLOSED. An unmapped source has dialect `None`, and a reviewer restricted to specific
//! dialects is never served it. A silent mislabel is worse than a missing label: the whole point is
//! that someone judging a dialect they do not speak produces confident wrong verdicts, and those are
//! indistinguishable from good ones downstream.

use std::collections::HashMap;
use std::path::Path;

pub const HAWLERI: &str = "hawleri";
pub const SORANI: &str = "sorani";
/// Badini. No clips yet — the owner's tree has the folder and named the dialect explicitly as one of
/// the three to keep apart, so the mapping exists before the corpus does rather than after someone
/// wonders why a restricted reviewer's queue is empty.
pub const BADINI: &str = "badini";

/// Path fragment → dialect. Deliberately EXPLICIT, never a guess from a filename shape: adding a
/// corpus means adding a line here, and until then its clips are unmapped and go to nobody who is
/// restricted. Badini and Sorani-Slemani get their entries when such a corpus is actually imported —
/// neither exists in the library today.
const SOURCE_DIALECTS: &[(&str, &str)] = &[
    // "Kawa ba Hawlery" podcast, all 32 episodes — owner-confirmed 2026-08-16 as fully Hawleri.
    ("KBHP", HAWLERI),
    // The owner's organized tree, `D:\Kurdish Corpora\<dialect>\<Corpus>\` — the folder a recording
    // was filed under IS the owner declaring its dialect, so a new corpus dropped into one of these
    // is routed correctly without a code change. Matched WITH the trailing separator, which is what
    // keeps `sorani-hawleri\` from also matching the `sorani\` entry.
    (r"Kurdish Corpora\sorani-hawleri\", HAWLERI),
    (r"Kurdish Corpora\sorani\", SORANI),
    (r"Kurdish Corpora\badini\", BADINI),
    // The original corpus's pre-recovery location. Nothing lives here now (the 1,031 clips were
    // relinked into the tree above), kept so a clip restored from an old snapshot still maps.
    ("SoraniVoice_PC_", SORANI),
    // A prepared single-speaker TTS set living OUTSIDE the organized tree, so the folder rule above
    // cannot route it. OWNER-CONFIRMED Sorani, 2026-08-21 ("Lamo is not hawleri, he is sorani"),
    // which also matches the `ZAR` prefix: the set is cut from the ZarPodcast material, which the
    // owner filed under `Kurdish Corpora\sorani\`. Mapped explicitly because an
    // UNMAPPED source is not a neutral state — `reviewer_may_judge` fails closed, so every restricted
    // reviewer is served nothing from it and the clips sit unreviewable with no error anywhere
    // (measured 2026-08-17, when 535 pending Sorani clips did exactly that to five reviewers).
    // Matched on the `ZAR_Lamo` prefix, not on one folder name, so the curated cuts of this set
    // (`ZAR_Lamo_15H_Gold_TTS`, and whatever the next selection pass is called) are routed the day
    // they appear. The alternative is remembering to add a line per folder, and forgetting means the
    // clips are transcribed and then served to NOBODY — silently, because unmapped fails closed.
    (r"ZAR_Lamo", SORANI),
    // The prepared Kawa single-speaker set declares `"dialect": "Sorani-Hawleri"` in its own
    // `kawa_harvest_report.json`. Keep the exact output-folder mapping so Sorani-only reviewers are
    // never handed it after the voice-organized pool imports the final WAVs.
    (r"Kawa_TTS_Dataset", HAWLERI),
    // The owner's authoritative dataset registry records the prepared Halwest voice as
    // "Standard Sorani" at this exact source folder (Obsidian Brain / Projects /
    // Kurdish Sorani Dataset.md, updated 2026-08-23). The local harvest report proves the speaker
    // and folder but omits dialect, so bind the precise owner-registered folder rather than infer
    // from the speaker name or filename.
    (r"Halwest_TTS_Dataset", SORANI),
];

/// The dialect of the recording this clip was cut from, or `None` when the source is not mapped.
pub fn dialect_of(audio_path: &str) -> Option<&'static str> {
    // Separators normalized first: these paths are Windows-shaped in the database, but a path that
    // reached us through a URL, an import manifest or a test may use '/', and a dialect that depends
    // on which slash a caller happened to use is the silent mislabel this module exists to prevent.
    let normalized = audio_path.replace('/', "\\");
    // Windows paths are case-insensitive. Treating their spelling as case-sensitive made the same
    // recording routable or unroutable depending only on how a drive/folder name was cased by the
    // importer. The configured fragments are ASCII, so ASCII folding is both sufficient and avoids
    // changing any non-ASCII corpus-name bytes.
    let folded = normalized.to_ascii_lowercase();
    let name = Path::new(&folded).file_name().and_then(|n| n.to_str()).unwrap_or(&folded).to_string();
    // Filename first (it identifies the recording), then the full path (for corpora distinguished by
    // the folder they were imported from rather than by their file names).
    SOURCE_DIALECTS
        .iter()
        .find(|(frag, _)| {
            let fragment = frag.to_ascii_lowercase();
            name.contains(&fragment) || folded.contains(&fragment)
        })
        .map(|(_, dialect)| *dialect)
}

/// Can this reviewer judge this clip? `allowed = None` means unrestricted (the owner, and any
/// reviewer with no entry in the roster) and is the behaviour every reviewer had before this existed.
pub fn reviewer_may_judge(allowed: Option<&[String]>, audio_path: &str) -> bool {
    let Some(allowed) = allowed else { return true };
    match dialect_of(audio_path) {
        Some(d) => allowed.iter().any(|a| a.trim().eq_ignore_ascii_case(d)),
        // Unmapped source + a restricted reviewer = refuse. Fail closed.
        None => false,
    }
}

/// Reviewer → the dialects they may judge, read from `<data_dir>/reviewer_dialects.json`:
///
/// ```json
/// { "Rezan": ["hawleri", "sorani"], "Nasrin": ["sorani"] }
/// ```
///
/// A MISSING file, or a reviewer absent from it, means UNRESTRICTED — so adding this file cannot
/// silently take work away from someone who was already reviewing.
///
/// A file that EXISTS but cannot be honoured is `Err`, and the caller must serve NOTHING
/// (owner instruction 2026-08-20 — present-but-broken policy files fail CLOSED). The old contract
/// failed open — "not even JSON" meant every reviewer unrestricted — which is the 2026-08-16
/// incident's failure mode wearing a different trigger: the protection off while the file sits on
/// disk looking like it is on. A stopped queue is noticed in minutes; 13 wrong-dialect decisions
/// were noticed in days.
///
/// Within a file that IS JSON, entries are still judged one by one:
///   * a value that is an array of strings is a roster entry;
///   * a key starting with `_` is a comment and is skipped — MEASURED 2026-08-16, the file shipped
///     with a `"_comment"` string, and parsing the whole map strictly would have made a helpful
///     comment fatal (then: silently fail open; now: needlessly stop the line);
///   * any OTHER non-list value is `Err`. It is a typo'd RESTRICTION — `"Nasrin": "sorani"` — and
///     skipping it quietly un-restricts exactly the reviewer the line was written to restrict.
pub fn load_roster(data_dir: &Path) -> Result<HashMap<String, Vec<String>>, String> {
    let path = data_dir.join("reviewer_dialects.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => {
            tracing::error!(
                "reviewer_dialects.json exists but is unreadable ({e}) — queues serve NOTHING until it is fixed"
            );
            return Err(format!("reviewer_dialects.json is unreadable: {e}"));
        }
    };
    parse_roster_text(&text)
}

/// Strict, non-mutating parser shared by live serving and snapshot promotion/preflight.
pub(crate) fn parse_roster_text(text: &str) -> Result<HashMap<String, Vec<String>>, String> {
    let parsed: HashMap<String, serde_json::Value> = serde_json::from_str(text).map_err(|e| {
        tracing::error!("reviewer_dialects.json is not valid JSON ({e}) — queues serve NOTHING until it is fixed");
        format!("reviewer_dialects.json is not valid JSON: {e}")
    })?;
    let mut roster: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in parsed {
        if name.starts_with('_') {
            tracing::info!("reviewer_dialects.json: ignoring comment key \"{name}\"");
            continue;
        }
        let dialects: Option<Vec<String>> =
            value.as_array().and_then(|items| items.iter().map(|v| v.as_str().map(str::to_string)).collect());
        match dialects {
            Some(dialects) => {
                if dialects.is_empty() {
                    tracing::error!(
                        "reviewer_dialects.json: \"{name}\" has no dialects — queues serve NOTHING until it is fixed"
                    );
                    return Err(format!("entry \"{name}\" must name at least one dialect"));
                }
                let mut normalized_dialects = Vec::with_capacity(dialects.len());
                for dialect in dialects {
                    let normalized = dialect.trim().to_ascii_lowercase();
                    if !matches!(normalized.as_str(), HAWLERI | SORANI | BADINI) {
                        tracing::error!(
                            "reviewer_dialects.json: \"{name}\" contains unknown dialect \"{dialect}\" — queues serve NOTHING until it is fixed"
                        );
                        return Err(format!(
                            "entry \"{name}\" contains unknown dialect \"{dialect}\" (allowed: {HAWLERI}, {SORANI}, {BADINI})"
                        ));
                    }
                    if !normalized_dialects.contains(&normalized) {
                        normalized_dialects.push(normalized);
                    }
                }
                // Two keys that are the same NAME under the session layer's matching (see
                // `allowed_for`) are a broken file: which restriction binds would depend on hash
                // iteration order, and one of them was certainly a typo of the other.
                if let Some(existing) = roster.keys().find(|k| k.trim().eq_ignore_ascii_case(name.trim())) {
                    tracing::error!("reviewer_dialects.json: \"{name}\" and \"{existing}\" name the same reviewer — queues serve NOTHING until it is fixed");
                    return Err(format!("\"{name}\" and \"{existing}\" name the same reviewer"));
                }
                roster.insert(name, normalized_dialects);
            }
            None => {
                tracing::error!("reviewer_dialects.json: \"{name}\" is not a list of dialect names — queues serve NOTHING until it is fixed");
                return Err(format!("entry \"{name}\" is not a list of dialect names (prefix comment keys with _)"));
            }
        }
    }
    Ok(roster)
}

/// The roster entry for `reviewer`, matched the way the SESSION layer matches names: trimmed and
/// ASCII-case-insensitive (`normalize_reviewers` treats "nasrin" and "Nasrin" as the same person).
///
/// 2026-08-20 hunt: the lookup was an exact `HashMap::get`, so a roster key differing from the live
/// session name by case or a stray space LOADED cleanly and bound NOBODY — the reviewer it named
/// was served unrestricted, which is the 2026-08-16 wrong-dialect incident reproduced through a
/// typo'd KEY instead of a typo'd value. The two layers must agree on what "the same name" means.
pub fn allowed_for<'r>(roster: &'r HashMap<String, Vec<String>>, reviewer: &str) -> Option<&'r Vec<String>> {
    let want = reviewer.trim();
    roster.iter().find(|(name, _)| name.trim().eq_ignore_ascii_case(want)).map(|(_, dialects)| dialects)
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
        // The exact case: three Sorani-only reviewers made 13 decisions on Hawleri only because the queue had
        // nothing else. With a roster in place they get nothing rather than the wrong dialect.
        let sorani_only = vec![SORANI.to_string()];
        assert!(!reviewer_may_judge(Some(&sorani_only), r"D:\x\KBHP-EP07.wav"));
        assert!(!reviewer_may_judge(Some(&sorani_only), r"D:\Kawa_TTS_Dataset\wavs\kawa_0001.wav"));
        assert!(reviewer_may_judge(Some(&sorani_only), r"C:\x\SoraniVoice_PC_\clip.wav"));
    }

    #[test]
    fn owner_registered_halwest_folder_is_sorani() {
        let path = r"D:\Halwest_TTS_Dataset\wavs\halwest_000001.wav";
        assert_eq!(dialect_of(path), Some(SORANI));
        assert!(reviewer_may_judge(Some(&[SORANI.to_string()]), path));
        assert!(!reviewer_may_judge(Some(&[HAWLERI.to_string()]), path));
    }

    #[test]
    fn two_unrestricted_reviewers_take_both() {
        let both = vec![HAWLERI.to_string(), SORANI.to_string()];
        assert!(reviewer_may_judge(Some(&both), r"D:\x\KBHP-EP07.wav"));
        assert!(reviewer_may_judge(Some(&both), r"C:\x\SoraniVoice_PC_\clip.wav"));
    }

    #[test]
    fn no_roster_entry_means_unrestricted_so_nobody_loses_work_by_adding_the_file() {
        assert!(reviewer_may_judge(None, r"D:\x\KBHP-EP07.wav"));
        assert!(reviewer_may_judge(None, r"D:\totally\unmapped.wav"));
    }

    #[test]
    fn the_organized_corpus_tree_declares_its_own_dialect() {
        // MEASURED 2026-08-17: the 1,031 recovered clips were relinked into this tree, but the map
        // still only knew their PRE-recovery location — so all 535 pending ones were unmapped, and
        // every Sorani-only reviewer's queue was empty while the owner was paying them to review.
        assert_eq!(dialect_of(r"D:\Kurdish Corpora\sorani\ZarPodcast\clip.wav"), Some(SORANI));
        assert_eq!(dialect_of(r"D:\Kurdish Corpora\sorani\ZarPodcast\2\clip.wav"), Some(SORANI));
        assert_eq!(dialect_of(r"D:\Kurdish Corpora\sorani\Halwest\clip.wav"), Some(SORANI));
        assert_eq!(dialect_of(r"D:\Kurdish Corpora\badini\anything\clip.wav"), Some(BADINI));
    }

    #[test]
    fn sorani_hawleri_is_hawleri_and_never_matches_the_sorani_folder() {
        // The two folder names share a prefix, so a fragment without its trailing separator would
        // file every Hawleri recording as Sorani — handing it to exactly the reviewers who cannot
        // judge it, which is the failure this module exists to prevent, via its own map.
        assert_eq!(dialect_of(r"D:\Kurdish Corpora\sorani-hawleri\KBHP\ep1.wav"), Some(HAWLERI));
        let sorani_only = vec![SORANI.to_string()];
        assert!(!reviewer_may_judge(Some(&sorani_only), r"D:\Kurdish Corpora\sorani-hawleri\KBHP\ep1.wav"));
    }

    #[test]
    fn a_forward_slash_path_maps_to_the_same_dialect() {
        assert_eq!(dialect_of("D:/Kurdish Corpora/sorani/ZarPodcast/clip.wav"), Some(SORANI));
        assert_eq!(dialect_of("D:/Kurdish Corpora/sorani-hawleri/KBHP/ep1.wav"), Some(HAWLERI));
    }

    #[test]
    fn windows_path_casing_cannot_change_the_dialect() {
        assert_eq!(dialect_of(r"d:\KURDISH CORPORA\SORANI\zarpodcast\clip.wav"), Some(SORANI));
        assert_eq!(dialect_of(r"d:\kurdish corpora\SORANI-HAWLERI\kbhp\ep1.wav"), Some(HAWLERI));
        assert_eq!(dialect_of(r"d:\sets\zar_lamo_15h_gold_tts\clip.wav"), Some(SORANI));
    }

    #[test]
    fn a_comment_key_does_not_switch_the_whole_roster_off() {
        // THE INCIDENT, 2026-08-16. The roster file shipped with a "_comment" string explaining how
        // to edit it. Parsed as one strict HashMap<String, Vec<String>> that is a hard error, and the
        // error path is "unrestricted" — so the protection was off from the moment it was installed,
        // for everyone, while the file still read as perfectly correct. Every entry that IS a list of
        // dialects must survive a neighbour that is not.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("reviewer_dialects.json"),
            r#"{ "_comment": "how to edit this file", "Nasrin": ["sorani"], "Rezan": ["hawleri", "sorani"] }"#,
        )
        .unwrap();
        let roster = load_roster(dir.path()).expect("an _-prefixed comment must not break the file");
        assert_eq!(roster.get("Nasrin"), Some(&vec![SORANI.to_string()]), "a real entry must still bind");
        assert_eq!(roster.len(), 2, "the comment is skipped, the two reviewers are kept: {roster:?}");
        assert!(
            !reviewer_may_judge(roster.get("Nasrin").map(Vec::as_slice), r"D:\x\KBHP-EP07.wav"),
            "Nasrin is Sorani-only, so Hawleri must be refused — this assertion failed in production"
        );
    }

    #[test]
    fn a_file_that_is_not_json_at_all_fails_closed_not_open() {
        // Owner instruction 2026-08-20. A broken file used to mean "everyone unrestricted" — the
        // 2026-08-16 incident's outcome by another road. Present-but-unusable now stops the queue
        // loudly instead of switching the guard off quietly.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reviewer_dialects.json"), "{ not json").unwrap();
        assert!(load_roster(dir.path()).is_err(), "a broken roster must stop the line, not unrestrict it");
    }

    #[test]
    fn a_missing_file_is_still_unrestricted() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_roster(dir.path()), Ok(HashMap::new()), "no file = the behaviour before the roster existed");
    }

    #[test]
    fn a_roster_key_binds_its_reviewer_across_case_and_whitespace() {
        // 2026-08-20 hunt: the session layer treats "nasrin" and "Nasrin" as the SAME person, but the
        // roster lookup was an exact HashMap::get — so a key typed in the wrong case loaded cleanly
        // and bound nobody, serving its reviewer the full queue. The 2026-08-16 incident, back
        // through a typo'd KEY.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reviewer_dialects.json"), r#"{ "nasrin ": ["sorani"] }"#).unwrap();
        let roster = load_roster(dir.path()).unwrap();
        let allowed = allowed_for(&roster, "Nasrin").expect("'nasrin ' must bind the live reviewer 'Nasrin'");
        assert_eq!(allowed, &vec![SORANI.to_string()]);
        assert!(allowed_for(&roster, "Rezan").is_none(), "and nobody else");
    }

    #[test]
    fn two_keys_naming_the_same_reviewer_fail_closed() {
        // Which restriction would bind depends on hash iteration order — one is certainly a typo of
        // the other, and a file that cannot say what it means must stop the line, not guess.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reviewer_dialects.json"), r#"{ "Nasrin": ["sorani"], "nasrin": ["hawleri"] }"#)
            .unwrap();
        assert!(load_roster(dir.path()).is_err(), "case-colliding keys are one broken file");
    }

    #[test]
    fn a_typoed_restriction_fails_closed_instead_of_unrestricting_its_reviewer() {
        // `"Nasrin": "sorani"` is a line somebody wrote to RESTRICT Nasrin. Skip-and-log honoured the
        // file while silently un-restricting exactly her — the one reviewer the line was about.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reviewer_dialects.json"), r#"{ "Nasrin": "sorani" }"#).unwrap();
        let err = load_roster(dir.path()).expect_err("a typo'd entry must stop the line");
        assert!(err.contains("Nasrin"), "the error names the broken entry: {err}");
    }

    #[test]
    fn roster_dialects_are_normalized_and_unknown_or_empty_values_fail_closed() {
        let normalized_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            normalized_dir.path().join("reviewer_dialects.json"),
            r#"{ "Nasrin": [" Sorani ", "SORANI", "Hawleri"] }"#,
        )
        .unwrap();
        let roster = load_roster(normalized_dir.path()).unwrap();
        assert_eq!(roster.get("Nasrin"), Some(&vec![SORANI.to_string(), HAWLERI.to_string()]));

        for invalid in [r#"{ "Nasrin": ["soranii"] }"#, r#"{ "Nasrin": [] }"#] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("reviewer_dialects.json"), invalid).unwrap();
            assert!(load_roster(dir.path()).is_err(), "invalid roster entry must stop the line: {invalid}");
        }
    }
}
