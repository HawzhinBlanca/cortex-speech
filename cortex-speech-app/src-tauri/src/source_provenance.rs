//! Detect audio that was PROCESSED before it reached this app, and say so in the library.
//!
//! The owner's pre-import cleaner (`kurdish-audio-cleaner`) runs a neural separator over a
//! recording, cuts out every non-speech stretch, re-concatenates what survives with 150 ms pauses,
//! and normalises the level. Its output is an ordinary WAV: nothing about the file says any of that
//! happened, and once its clips are in the library an export would describe machine-separated audio
//! in exactly the words used for a raw field recording.
//!
//! The cleaner already writes the facts into a `manifest.json` beside its output. This module finds
//! that manifest and turns it into a [`SourceAudioProvenance`] declaration, so the claim travels
//! with the audio instead of living in someone's memory of which folder was which.
//!
//! Detection is deliberately CONSERVATIVE. It reads a manifest that is actually there; it never
//! infers processing from a directory name like `..._cleaned`, because a guess that is wrong in
//! either direction is worse than no claim: a false positive brands original recordings as
//! processed, and a false negative is the very lie this exists to prevent.

use crate::db::SourceAudioProvenance;
use std::path::Path;

/// The cleaner writes this next to its output tree.
const MANIFEST_NAME: &str = "manifest.json";

/// How far up from the audio file to look for the manifest. The cleaner mirrors the source tree
/// under its output directory, so a clip can sit several levels below the manifest
/// (`<out>/100_nawdari_cihan/01.wav` with `<out>/manifest.json`). Bounded so a stray manifest far up
/// the filesystem — in a home directory, say — can never be attributed to unrelated audio.
const MAX_PARENT_LEVELS: usize = 6;

/// Find the processing declaration for `audio_path`, or `None` when nothing claims it was processed.
///
/// `None` is not a certificate of originality — see [`SourceAudioProvenance`].
pub fn detect(audio_path: &Path) -> Option<SourceAudioProvenance> {
    let manifest_path = find_manifest(audio_path)?;
    let text = std::fs::read_to_string(&manifest_path).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    let cleaning = manifest.get("cleaning")?;

    // `audio_is_processed` is the explicit claim. A manifest without it is some other tool's file
    // that happens to share the name, so make no claim at all.
    if cleaning.get("audio_is_processed").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }

    Some(SourceAudioProvenance {
        audio_path: audio_path.to_string_lossy().to_string(),
        processing: describe(cleaning),
        separator_model: cleaning.get("separator_model").and_then(serde_json::Value::as_str).map(str::to_string),
        // The cleaner cuts non-speech OUT, so unless a manifest says otherwise, assume the timeline
        // was NOT preserved. Assuming the safe direction matters: a consumer that believes an offset
        // maps back to the source when it does not will silently mis-locate every clip.
        timeline_preserved: cleaning.get("timeline_preserved").and_then(serde_json::Value::as_bool).unwrap_or(false),
        manifest_path: Some(manifest_path.to_string_lossy().to_string()),
    })
}

/// One sentence stating what was done, built from whatever the manifest actually declares — never
/// from assumed defaults, so the export repeats the cleaner's own claims and nothing more.
fn describe(cleaning: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    if let Some(model) = cleaning.get("separator_model").and_then(serde_json::Value::as_str) {
        let stem = cleaning.get("separator_stem").and_then(serde_json::Value::as_str).unwrap_or("vocals");
        parts.push(format!("separated with {model} ({stem} stem kept)"));
    }
    if let Some(removed) = cleaning.get("removed").and_then(serde_json::Value::as_str) {
        parts.push(format!("removed: {removed}"));
    }
    if let Some(timeline) = cleaning.get("timeline").and_then(serde_json::Value::as_str) {
        parts.push(timeline.to_string());
    }
    if let Some(lufs) = cleaning.get("loudness_lufs").and_then(serde_json::Value::as_f64) {
        parts.push(format!("loudness normalised to {lufs} LUFS"));
    }
    if let Some(hz) = cleaning.get("resampled_hz").and_then(serde_json::Value::as_u64) {
        parts.push(format!("resampled to {hz} Hz"));
    }
    if parts.is_empty() {
        // The manifest claimed processing but described none of it. Say exactly that rather than
        // inventing a description or, worse, dropping the claim.
        return "processed before import; the source manifest did not say how".to_string();
    }
    parts.join("; ")
}

fn find_manifest(audio_path: &Path) -> Option<std::path::PathBuf> {
    let mut dir = audio_path.parent()?;
    for _ in 0..MAX_PARENT_LEVELS {
        let candidate = dir.join(MANIFEST_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    const CLEANER_MANIFEST: &str = r#"{
        "source": "raw", "output": "cleaned",
        "cleaning": {
            "audio_is_processed": true,
            "separator_model": "melband_roformer_big_beta4.ckpt",
            "separator_stem": "vocals",
            "removed": "music, applause, room sound",
            "timeline": "non-speech CUT OUT and speech re-concatenated with 150 ms pauses",
            "resampled_hz": 24000,
            "loudness_lufs": -20.0
        }
    }"#;

    #[test]
    fn a_cleaned_tree_declares_what_was_done_to_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cleaned");
        write(&root.join("manifest.json"), CLEANER_MANIFEST);
        // Nested exactly as the cleaner mirrors the source tree.
        let clip = root.join("100_nawdari_cihan").join("01.wav");
        write(&clip, "not really audio");

        let found = detect(&clip).expect("a cleaned tree must declare itself");
        assert_eq!(found.separator_model.as_deref(), Some("melband_roformer_big_beta4.ckpt"));
        assert!(!found.timeline_preserved, "the cleaner CUTS audio out — offsets do not map back");
        assert!(found.processing.contains("melband_roformer"), "{}", found.processing);
        assert!(found.processing.contains("music"), "{}", found.processing);
        assert!(found.manifest_path.is_some());
    }

    #[test]
    fn nothing_is_claimed_without_a_manifest_that_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");

        // No manifest at all: an ordinary recording.
        let plain = dir.path().join("raw").join("KBHP-EP01.wav");
        write(&plain, "not really audio");
        assert!(detect(&plain).is_none(), "an unmarked recording must not be branded as processed");

        // A directory NAMED like a cleaned tree, but with no manifest. Guessing from the name is
        // exactly the inference this module refuses to make.
        let looks_cleaned = dir.path().join("audiobook-cleaned").join("01.wav");
        write(&looks_cleaned, "not really audio");
        assert!(detect(&looks_cleaned).is_none(), "a directory name is not evidence");

        // Some other tool's manifest.json that makes no processing claim.
        let other = dir.path().join("other");
        write(&other.join("manifest.json"), r#"{"files": 3, "note": "an unrelated manifest"}"#);
        let clip = other.join("01.wav");
        write(&clip, "not really audio");
        assert!(detect(&clip).is_none(), "an unrelated manifest must not be read as a processing claim");

        // A manifest that claims processing but describes none of it still yields a claim — silence
        // about the details must not become silence about the fact.
        let vague = dir.path().join("vague");
        write(&vague.join("manifest.json"), r#"{"cleaning": {"audio_is_processed": true}}"#);
        let vague_clip = vague.join("01.wav");
        write(&vague_clip, "not really audio");
        let found = detect(&vague_clip).expect("the claim survives a manifest with no detail");
        assert!(found.processing.contains("did not say how"), "{}", found.processing);
    }

    #[test]
    fn the_search_upward_is_bounded() {
        // A manifest far above unrelated audio must never be attributed to it.
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("manifest.json"), CLEANER_MANIFEST);
        let deep = dir.path().join("a/b/c/d/e/f/g/h").join("01.wav");
        write(&deep, "not really audio");
        assert!(detect(&deep).is_none(), "the upward search must stop before unrelated ancestors");
    }
}
