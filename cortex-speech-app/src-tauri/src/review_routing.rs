//! Difficulty routing for the review pool queue: hard clips to the ears the owner trusts most,
//! easy clips to everyone else. Owner instruction 2026-09-07 ("build item 1").
//!
//! WHY. Measured on 2,350 human verdicts (2026-09-07, read-only): a clip whose LOWEST CTC word
//! confidence is under 0.6 was edited by the human 58% of the time (mean draft CER 5.5%); at 0.8 or
//! above, 33% (2.3%). Speaking rate does the same job at the extremes: under 8 or at/above 18
//! characters per second, 57–59% edited, draft CER 3–4× the norm. So the machine already knows which
//! clips are hard before anyone listens. Routing sends those to the reviewers named in the owner's
//! file first and the easy ones to the other reviewers first, so a lenient "Looks good" lands on
//! the clips most likely to be right and a hard clip meets a careful ear at least once.
//!
//! WHAT IT IS NOT. It re-orders inside a decision-distance tier only (`review_pool.rs`): a clip one
//! opinion away from consensus still comes before any fresh clip for every reviewer, the owner
//! listen list still precedes everything, and no reviewer gains or loses decision authority — the
//! consensus canon (any two DIFFERENT reviewers) is untouched. It never writes anything.
//!
//! `<data_dir>/review_routing.json`:
//!
//! ```json
//! { "_comment": "hard clips first for these reviewers; easy first for everyone else",
//!   "hard_first": ["Sara", "Hemn"] }
//! ```
//!
//! A missing file changes nothing for anyone (the difficulty key is then constant). A present but
//! unreadable file is logged and IGNORED, like the listen list: a priority hint cannot take work
//! away or misroute dialects, so failing open costs nothing, while failing closed would stop ten
//! reviewers over a typo in a file that only ever affects order.

use std::path::Path;

pub const FILE_NAME: &str = "review_routing.json";

/// How many least-certain words the phone shows per clip: enough to aim the ear, few enough to
/// stay a hint rather than a second transcript.
pub const UNCERTAIN_WORDS_LIMIT: usize = 6;

/// Lowest word confidence below which a clip counts as hard (58% human-edit rate, measured).
pub const HARD_MIN_WORD_CONFIDENCE: f64 = 0.6;
/// Lowest word confidence at or above which a clip counts as easy (33% human-edit rate, measured).
pub const EASY_MIN_WORD_CONFIDENCE: f64 = 0.8;
/// Speaking-rate band (non-space characters per second) outside which a clip counts as hard.
pub const NORMAL_CPS_MIN: f64 = 8.0;
pub const NORMAL_CPS_MAX_EXCLUSIVE: f64 = 18.0;

/// How one reviewer's queue orders clips of equal decision distance, voice and TTS rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifficultyOrder {
    /// No routing file: the difficulty key is constant and the order is exactly what it was.
    Unchanged,
    /// Named in `hard_first`: hardest clips first.
    HardFirst,
    /// Not named while the file exists: easiest clips first.
    EasyFirst,
}

/// Difficulty bucket of a clip: 0 = hard, 1 = medium or unmeasured, 2 = easy.
///
/// `min_word_confidence` is the lowest CTC word confidence of the draft (None when the clip was
/// never force-aligned — 71% of the pool on 2026-09-07). Speaking rate always exists, so an extreme
/// rate makes a clip hard even without an alignment; an unaligned clip at a normal rate is medium,
/// never easy: absence of evidence is not evidence of an easy clip.
pub fn difficulty_bucket(raw_transcript: &str, duration_ms: i64, min_word_confidence: Option<f64>) -> u8 {
    let chars = raw_transcript.chars().filter(|c| !c.is_whitespace()).count() as f64;
    let seconds = duration_ms.max(1) as f64 / 1000.0;
    let cps = chars / seconds;
    if !(NORMAL_CPS_MIN..NORMAL_CPS_MAX_EXCLUSIVE).contains(&cps) {
        return 0;
    }
    match min_word_confidence {
        Some(confidence) if confidence < HARD_MIN_WORD_CONFIDENCE => 0,
        Some(confidence) if confidence >= EASY_MIN_WORD_CONFIDENCE => 2,
        _ => 1,
    }
}

/// The sort key a bucket contributes under `order` (lower sorts first).
pub fn difficulty_rank(order: DifficultyOrder, bucket: u8) -> u8 {
    match order {
        DifficultyOrder::Unchanged => 1,
        DifficultyOrder::HardFirst => bucket,
        DifficultyOrder::EasyFirst => 2u8.saturating_sub(bucket),
    }
}

/// Per-word confidences of a stored alignment, in draft word order, when the alignment carries
/// them. `alignment_json` is `{"words":[{"word":..,"confidence":..},..], ..}` (see `aligner.rs`).
pub fn word_confidences(alignment_json: &str) -> Option<Vec<(String, f64)>> {
    let value: serde_json::Value = serde_json::from_str(alignment_json).ok()?;
    let words = value.get("words")?.as_array()?;
    let mut out = Vec::with_capacity(words.len());
    for word in words {
        let text = word.get("word")?.as_str()?;
        let confidence = word.get("confidence")?.as_f64()?;
        out.push((text.to_string(), confidence));
    }
    (!out.is_empty()).then_some(out)
}

/// Lowest word confidence of a stored alignment, or None when the alignment has no confidences.
pub fn min_word_confidence(alignment_json: &str) -> Option<f64> {
    word_confidences(alignment_json)?.into_iter().map(|(_, c)| c).reduce(f64::min)
}

/// The draft words the aligner was least sure of, in draft order, for the reviewer's eye. Empty
/// when the alignment does not describe exactly this draft (a re-transcribed row whose alignment
/// is stale must not point at the wrong words) or when nothing is below the hard threshold.
pub fn uncertain_words(raw_transcript: &str, alignment_json: &str, limit: usize) -> Vec<String> {
    let Some(words) = word_confidences(alignment_json) else {
        return Vec::new();
    };
    let draft: Vec<&str> = raw_transcript.split_whitespace().collect();
    if draft.len() != words.len() || draft.iter().zip(&words).any(|(d, (w, _))| *d != w) {
        return Vec::new();
    }
    words
        .into_iter()
        .filter(|(_, confidence)| *confidence < HARD_MIN_WORD_CONFIDENCE)
        .map(|(word, _)| word)
        .take(limit)
        .collect()
}

/// This reviewer's order per the routing file in `data_dir`.
pub fn order_for(data_dir: &Path, reviewer: &str) -> DifficultyOrder {
    let path = data_dir.join(FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return DifficultyOrder::Unchanged,
        Err(error) => {
            tracing::error!("{FILE_NAME} exists but is unreadable ({error}); difficulty routing is ignored");
            return DifficultyOrder::Unchanged;
        }
    };
    match parse(&text) {
        Ok(hard_first) => order_from_list(&hard_first, reviewer),
        Err(error) => {
            tracing::error!("{FILE_NAME} is ignored: {error}");
            DifficultyOrder::Unchanged
        }
    }
}

/// Strict parser: `hard_first` must be a list of strings; `_`-prefixed keys are comments.
pub fn parse(text: &str) -> Result<Vec<String>, String> {
    let parsed: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(text).map_err(|error| format!("not valid JSON object: {error}"))?;
    for key in parsed.keys() {
        if !key.starts_with('_') && key != "hard_first" {
            return Err(format!("unknown key \"{key}\" (only hard_first is understood)"));
        }
    }
    let Some(value) = parsed.get("hard_first") else {
        return Err("missing \"hard_first\" list".to_string());
    };
    value
        .as_array()
        .and_then(|items| {
            items.iter().map(|item| item.as_str().map(|s| s.trim().to_string())).collect::<Option<Vec<_>>>()
        })
        .map(|names| names.into_iter().filter(|name| !name.is_empty()).collect())
        .ok_or_else(|| "\"hard_first\" must be a list of reviewer names".to_string())
}

fn order_from_list(hard_first: &[String], reviewer: &str) -> DifficultyOrder {
    let want = reviewer.trim();
    if hard_first.iter().any(|name| name.eq_ignore_ascii_case(want)) {
        DifficultyOrder::HardFirst
    } else {
        DifficultyOrder::EasyFirst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALIGNED: &str = r#"{"chunk_count":1,"chunk_index":0,"source_start_ms":0,"source_end_ms":1000,
        "words":[{"word":"دەقی","confidence":0.93,"start":0.0,"end":0.4},{"word":"چامپیۆن","confidence":0.41,"start":0.4,"end":0.9}]}"#;

    #[test]
    fn buckets_follow_the_measured_thresholds() {
        // "دەقی چامپیۆن" = 11 non-space chars; 1 s → 11 cps, the normal band.
        assert_eq!(difficulty_bucket("دەقی چامپیۆن", 1_000, Some(0.59)), 0, "lowest word under 0.6 is hard");
        assert_eq!(difficulty_bucket("دەقی چامپیۆن", 1_000, Some(0.6)), 1);
        assert_eq!(difficulty_bucket("دەقی چامپیۆن", 1_000, Some(0.8)), 2, "0.8 and above is easy");
        assert_eq!(
            difficulty_bucket("دەقی چامپیۆن", 1_000, None),
            1,
            "unaligned at a normal rate is medium, never easy"
        );
        assert_eq!(
            difficulty_bucket("دەقی چامپیۆن", 2_000, Some(0.95)),
            0,
            "5.5 cps: too slow, hard whatever the words say"
        );
        assert_eq!(difficulty_bucket("دەقی چامپیۆن", 600, Some(0.95)), 0, "18.3 cps: too fast");
        assert_eq!(difficulty_bucket("", 0, None), 0, "an empty or zero-length draft is never easy");
    }

    #[test]
    fn rank_is_constant_without_a_file_and_mirrors_between_the_two_groups() {
        for bucket in 0..=2 {
            assert_eq!(difficulty_rank(DifficultyOrder::Unchanged, bucket), 1);
            assert_eq!(difficulty_rank(DifficultyOrder::HardFirst, bucket), bucket);
            assert_eq!(difficulty_rank(DifficultyOrder::EasyFirst, bucket), 2 - bucket);
        }
    }

    #[test]
    fn alignment_confidences_are_read_and_uncertain_words_only_for_the_matching_draft() {
        assert_eq!(min_word_confidence(ALIGNED), Some(0.41));
        assert_eq!(uncertain_words("دەقی چامپیۆن", ALIGNED, 6), vec!["چامپیۆن".to_string()]);
        assert!(uncertain_words("دەقی نوێ", ALIGNED, 6).is_empty(), "a stale alignment names no words");
        assert!(uncertain_words("دەقی چامپیۆن", ALIGNED, 0).is_empty());
        let meta_only = r#"{"chunk_count":1,"chunk_index":0,"source_start_ms":0,"source_end_ms":1000}"#;
        assert_eq!(min_word_confidence(meta_only), None);
        assert!(uncertain_words("دەقی چامپیۆن", meta_only, 6).is_empty());
        assert_eq!(min_word_confidence("not json"), None);
    }

    #[test]
    fn the_file_decides_the_order_and_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(order_for(dir.path(), "Sara"), DifficultyOrder::Unchanged, "no file: nothing changes");
        std::fs::write(dir.path().join(FILE_NAME), r#"{ "_comment": "x", "hard_first": [" sara ", "Hemn"] }"#).unwrap();
        assert_eq!(order_for(dir.path(), "Sara"), DifficultyOrder::HardFirst, "trimmed, ASCII case-insensitive");
        assert_eq!(order_for(dir.path(), "Roza"), DifficultyOrder::EasyFirst, "everyone else: easy first");
        std::fs::write(dir.path().join(FILE_NAME), "{ broken").unwrap();
        assert_eq!(
            order_for(dir.path(), "Sara"),
            DifficultyOrder::Unchanged,
            "a broken file is ignored, never a reason to stop"
        );
        assert!(parse(r#"{ "hard_first": "Sara" }"#).unwrap_err().contains("must be a list"));
        assert!(parse(r#"{ "easy_first": ["Sara"] }"#).unwrap_err().contains("unknown key"));
        assert!(parse(r#"{ "_note": 1 }"#).unwrap_err().contains("missing"));
    }
}
