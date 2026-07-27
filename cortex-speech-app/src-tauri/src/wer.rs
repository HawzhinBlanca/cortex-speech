//! Word Error Rate (WER) and Character Error Rate (CER) for annotated vs hypothesis text.
//!
//! IMPORTANT: `compute_wer` / `compute_cer` are analyzers, not trainers.
//! All text must flow through an equivalent normalization path; these functions
//! preserve the existing internal contract.

/// Default maximum WER (35%) for dataset quality gates.
pub const DEFAULT_MAX_WER: f64 = 0.35;

/// Default maximum CER (20%) for dataset quality gates.
pub const DEFAULT_MAX_CER: f64 = 0.20;

/// Tokenize on whitespace for word-level metrics (works for Sorani Arabic script).
pub fn tokenize_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(|w| w.to_string()).filter(|w| !w.is_empty()).collect()
}

/// Unicode scalar characters for CER (important for Arabic/Kurdish script).
///
/// Interior whitespace is KEPT (counted as an ordinary character) so that word-segmentation
/// errors — a real, common Sorani ASR error class (e.g. "هاوڕێ من" vs "هاوڕێمن") — are scored
/// rather than silently collapsing to CER 0. This matches jiwer's default CER definition, which
/// the project's acceptance criteria require us to track. `normalize_for_metrics` already collapses
/// whitespace runs to a single space and trims, so only meaningful interior separators survive.
pub fn tokenize_chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

use crate::normalizer::{NormalizationConfig, SoraniNormalizer};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

static METRICS_NORMALIZER: LazyLock<SoraniNormalizer> = LazyLock::new(|| {
    SoraniNormalizer::with_config(NormalizationConfig {
        normalize_numbers: true,
        verbalize_numbers: false, // Keep numbers as digits for WER/CER calculations
        normalize_hamza: true,
        remove_diacritics: true,
    })
});

/// Normalize text before comparison: Sorani-normalization + Unicode NFC + lowercase + collapse whitespace.
pub fn normalize_for_metrics(text: &str) -> String {
    let normalized = METRICS_NORMALIZER.normalize(text);
    let nfc_normalized: String = normalized.nfc().collect();
    nfc_normalized.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, Copy)]
pub struct EditDistanceResult {
    pub distance: usize,
    pub ref_len: usize,
}

pub fn word_edit_distance(reference: &str, hypothesis: &str) -> EditDistanceResult {
    let reference = normalize_for_metrics(reference);
    let hypothesis = normalize_for_metrics(hypothesis);
    let ref_words = tokenize_words(&reference);
    let hyp_words = tokenize_words(&hypothesis);

    if ref_words.is_empty() {
        // Honest insertion count (one error per spurious hypothesis word) so corpus-level (micro)
        // aggregation in eval.rs sums the true error. `compute_wer` separately clamps the
        // per-utterance display rate to 1.0 for the empty-reference case.
        return EditDistanceResult { distance: hyp_words.len(), ref_len: 0 };
    }

    let distance = levenshtein(&ref_words, &hyp_words);
    EditDistanceResult { distance, ref_len: ref_words.len() }
}

pub fn char_edit_distance(reference: &str, hypothesis: &str) -> EditDistanceResult {
    let reference = normalize_for_metrics(reference);
    let hypothesis = normalize_for_metrics(hypothesis);
    let ref_chars = tokenize_chars(&reference);
    let hyp_chars = tokenize_chars(&hypothesis);

    if ref_chars.is_empty() {
        // Honest insertion count for micro aggregation; `compute_cer` clamps the per-utterance
        // display rate to 1.0 for the empty-reference case.
        return EditDistanceResult { distance: hyp_chars.len(), ref_len: 0 };
    }

    let distance = levenshtein(&ref_chars, &hyp_chars);
    EditDistanceResult { distance, ref_len: ref_chars.len() }
}

/// Word Error Rate in \[0, 1\]. Returns `1.0` when reference is empty but hypothesis is not.
/// The raw edit-distance ratio is clamped to 1.0 so callers never observe a WER above 100%.
pub fn compute_wer(reference: &str, hypothesis: &str) -> f64 {
    let res = word_edit_distance(reference, hypothesis);
    if res.ref_len == 0 {
        // Empty reference: 0.0 if the hypothesis is also empty, else a full-error 1.0. (The raw
        // `res.distance` is the unclamped insertion count for aggregation, not a display rate.)
        return if res.distance == 0 { 0.0 } else { 1.0 };
    }
    (res.distance as f64 / res.ref_len as f64).min(1.0)
}

/// Character Error Rate in \[0, 1\].
/// The raw edit-distance ratio is clamped to 1.0 so callers never observe a CER above 100%.
pub fn compute_cer(reference: &str, hypothesis: &str) -> f64 {
    let res = char_edit_distance(reference, hypothesis);
    if res.ref_len == 0 {
        return if res.distance == 0 { 0.0 } else { 1.0 };
    }
    (res.distance as f64 / res.ref_len as f64).min(1.0)
}

fn levenshtein<T: Eq>(a: &[T], b: &[T]) -> usize {
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];

    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Substitution / deletion / insertion decomposition of the word-error alignment.
/// `rate()` is the HONEST unclamped error rate — it may exceed 1.0 (e.g. a hypothesis with
/// many spurious insertions), unlike [`compute_wer`] which clamps to 1.0 for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ErrorBreakdown {
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    pub ref_len: usize,
}

impl ErrorBreakdown {
    /// Total errors S + D + I (equals the raw edit distance).
    pub fn total(&self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }

    /// Unclamped error rate `total / ref_len` (can exceed 1.0). An empty reference yields
    /// 0.0 when the hypothesis is also empty, else 1.0.
    pub fn rate(&self) -> f64 {
        if self.ref_len == 0 {
            return if self.total() == 0 { 0.0 } else { 1.0 };
        }
        self.total() as f64 / self.ref_len as f64
    }
}

/// Full O(n·m) edit DP with backtracking to classify each error as a substitution,
/// deletion (a reference token missing from the hypothesis), or insertion (a spurious
/// hypothesis token). `a` is the reference, `b` the hypothesis.
fn levenshtein_breakdown<T: Eq>(a: &[T], b: &[T]) -> (usize, usize, usize) {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
        }
    }

    let (mut i, mut j) = (n, m);
    let (mut subs, mut dels, mut ins) = (0usize, 0usize, 0usize);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && a[i - 1] == b[j - 1] && dp[i][j] == dp[i - 1][j - 1] {
            i -= 1; // match — no error
            j -= 1;
        } else if i > 0 && j > 0 && dp[i][j] == dp[i - 1][j - 1] + 1 {
            subs += 1;
            i -= 1;
            j -= 1;
        } else if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            dels += 1;
            i -= 1;
        } else {
            ins += 1;
            j -= 1;
        }
    }
    (subs, dels, ins)
}

/// Word-level S/D/I decomposition after the shared metric normalization.
pub fn word_error_breakdown(reference: &str, hypothesis: &str) -> ErrorBreakdown {
    let reference = normalize_for_metrics(reference);
    let hypothesis = normalize_for_metrics(hypothesis);
    let ref_words = tokenize_words(&reference);
    let hyp_words = tokenize_words(&hypothesis);
    let (substitutions, deletions, insertions) = levenshtein_breakdown(&ref_words, &hyp_words);
    ErrorBreakdown { substitutions, deletions, insertions, ref_len: ref_words.len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty-input guards in the metric core.
    ///
    /// These were the only genuinely uncovered PRODUCTION lines in wer.rs (its headline 80.62%
    /// is otherwise depressed by the `#[ignore]`d, opt-in `emit_crossval_vectors` tool, which is
    /// test code). They are the divide-by-zero and empty-sequence branches every CER/WER number
    /// funnels through: an empty gold reference is not hypothetical - it is what a blank or
    /// not-yet-transcribed row looks like, and a wrong answer here silently poisons an average.
    #[test]
    fn empty_reference_and_hypothesis_edge_cases() {
        // rate(): an empty reference scores 0.0 only when the hypothesis is also empty, else 1.0.
        // Anything else would divide by zero.
        let both_empty = word_error_breakdown("", "");
        assert_eq!(both_empty.ref_len, 0);
        assert_eq!(both_empty.total(), 0);
        assert_eq!(both_empty.rate(), 0.0, "empty vs empty is a perfect match, not an error");

        let spurious = word_error_breakdown("", "hello world");
        assert_eq!(spurious.ref_len, 0);
        assert!(spurious.total() > 0, "words invented against an empty reference are errors");
        assert_eq!(spurious.rate(), 1.0, "errors against an empty reference saturate at 1.0");
        assert!(spurious.rate().is_finite(), "must never divide by zero");

        // The levenshtein empty-sequence early returns: distance equals the other side's length.
        let del_all = word_error_breakdown("hello world", "");
        assert_eq!(del_all.total(), 2, "both reference words are deletions");
        assert_eq!(del_all.deletions, 2);
        assert_eq!(del_all.insertions, 0);
        assert_eq!(del_all.rate(), 1.0);

        let ins_all = word_error_breakdown("", "a b c");
        assert_eq!(ins_all.insertions, 3, "every hypothesis word is an insertion");
        assert_eq!(ins_all.deletions, 0);

        // And the same guards through the public rate functions.
        assert_eq!(compute_wer("", ""), 0.0);
        assert_eq!(compute_cer("", ""), 0.0);
        assert_eq!(compute_wer("", "x"), 1.0);
        assert_eq!(compute_cer("", "x"), 1.0);
        assert!(compute_wer("x", "").is_finite() && compute_cer("x", "").is_finite());
    }

    #[test]
    fn identical_text_zero_wer() {
        assert_eq!(compute_wer("hello world", "hello world"), 0.0);
    }

    #[test]
    fn cer_counts_unicode_chars_not_utf8_bytes() {
        // Each of these Sorani letters is 2 bytes in UTF-8, so a byte-level edit
        // distance would report ref_len 6 and count every multi-byte edit as 2+,
        // inflating CER for the target language. CER must operate on Unicode chars.
        assert_eq!("ابج".len(), 6, "sanity: 3 Sorani letters are 6 UTF-8 bytes");

        // One-character substitution (ج -> د): a single edit over a 3-char reference.
        let sub = char_edit_distance("ابج", "ابد");
        assert_eq!(sub.ref_len, 3, "ref_len must be the character count, not the byte count");
        assert_eq!(sub.distance, 1, "one multi-byte-char substitution is a single edit");
        assert!((compute_cer("ابج", "ابد") - 1.0 / 3.0).abs() < 1e-12);

        // Deleting one multi-byte char is also a single edit, not its byte length.
        let del = char_edit_distance("ابج", "اب");
        assert_eq!(del.ref_len, 3);
        assert_eq!(del.distance, 1, "deleting one multi-byte char is a single edit");
    }

    #[test]
    fn breakdown_classifies_substitution_and_deletion() {
        // ref "a b c d" vs hyp "a x c": b->x substitution, d deleted, nothing inserted.
        let bd = word_error_breakdown("a b c d", "a x c");
        assert_eq!(bd.substitutions, 1);
        assert_eq!(bd.deletions, 1);
        assert_eq!(bd.insertions, 0);
        assert_eq!(bd.ref_len, 4);
        assert_eq!(bd.total(), 2);
        assert!((bd.rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn breakdown_classifies_insertion() {
        let bd = word_error_breakdown("a b", "a b c");
        assert_eq!(bd.insertions, 1);
        assert_eq!(bd.substitutions, 0);
        assert_eq!(bd.deletions, 0);
        assert_eq!(bd.total(), 1);
    }

    #[test]
    fn breakdown_total_equals_edit_distance_for_nonempty_ref() {
        for (r, h) in [
            ("the quick brown fox", "the slow brown cat jumped"),
            ("کوردی زمانی شیرینە", "کوردی زمان شیرین"),
            ("a b c", ""),
        ] {
            let bd = word_error_breakdown(r, h);
            assert_eq!(
                bd.total(),
                word_edit_distance(r, h).distance,
                "S+D+I must equal edit distance for ({r:?},{h:?})"
            );
        }
    }

    #[test]
    fn empty_reference_distance_is_honest_insertion_count() {
        // The raw edit-distance reports the true insertion count (for micro aggregation in eval.rs),
        // while compute_wer/compute_cer still clamp the per-utterance display rate to 1.0.
        assert_eq!(word_edit_distance("", "one two three").distance, 3);
        assert_eq!(word_edit_distance("", "").distance, 0);
        assert_eq!(char_edit_distance("", "abc").distance, 3);
        // Display rate stays clamped at 1.0 regardless of how many words were hallucinated.
        assert_eq!(compute_wer("", "one two three"), 1.0);
        assert_eq!(compute_cer("", "abc"), 1.0);
        assert_eq!(compute_wer("", ""), 0.0);
    }

    #[test]
    fn cer_counts_word_segmentation_errors() {
        // A space-merge is a real Sorani ASR error and must NOT score CER 0 (the old whitespace-
        // stripping tokenizer collapsed both sides to the same char stream). Matches jiwer, which
        // counts the interior space as an ordinary character: "ab cd" -> "abcd" = 1 del / 5 = 0.2.
        assert!((compute_cer("ab cd", "abcd") - 0.2).abs() < 1e-9, "got {}", compute_cer("ab cd", "abcd"));
        assert!(compute_cer("هاوڕێ من", "هاوڕێمن") > 0.0, "word-merge in Sorani must register as CER > 0");
    }

    #[test]
    fn breakdown_reports_true_insertions_for_empty_reference() {
        // The breakdown is HONEST and reports the true insertion count.
        let bd = word_error_breakdown("", "one two");
        assert_eq!(bd.insertions, 2);
        assert_eq!(bd.deletions, 0);
        assert_eq!(bd.substitutions, 0);
        assert_eq!(bd.ref_len, 0);
        assert_eq!(bd.total(), 2);
    }

    #[test]
    fn breakdown_rate_is_unclamped_unlike_compute_wer() {
        // Many spurious insertions -> honest rate > 1.0, while compute_wer clamps to 1.0.
        let bd = word_error_breakdown("word", "word one two three four five");
        assert!(bd.rate() > 1.0, "unclamped rate should exceed 1.0: {}", bd.rate());
        assert_eq!(compute_wer("word", "word one two three four five"), 1.0);
    }

    #[test]
    fn breakdown_pins_sdi_split_at_the_all_deletion_and_all_insertion_boundaries() {
        // The backtrace's `j -= 1` (insertion) branch must never be reached with j == 0, and the
        // all-deletion / all-insertion extremes must attribute EVERY error to the right bucket — not
        // merely keep total() correct. A refactor that mislabeled deletions as insertions (or vice
        // versa) would keep the edit distance right while silently corrupting the S/D/I breakdown that
        // feeds the honesty-critical error report; this pins both extremes so that can't slip through.
        let all_del = word_error_breakdown("alpha beta gamma", ""); // hyp empty -> j hits 0
        assert_eq!(
            (all_del.substitutions, all_del.deletions, all_del.insertions, all_del.ref_len),
            (0, 3, 0, 3),
            "every reference word with no hypothesis is a DELETION"
        );
        let all_ins = word_error_breakdown("", "alpha beta gamma"); // ref empty -> i is 0
        assert_eq!(
            (all_ins.substitutions, all_ins.deletions, all_ins.insertions, all_ins.ref_len),
            (0, 0, 3, 0),
            "every hypothesis word with no reference is an INSERTION"
        );
    }

    #[test]
    fn one_substitution_wer() {
        assert!((compute_wer("hello world", "hello earth") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn wer_kurdish_identical() {
        assert_eq!(compute_wer("کوردی", "کوردی"), 0.0);
    }

    #[test]
    fn cer_kurdish_one_substitution() {
        let cer = compute_cer("کوردی", "کوردێ");
        assert!(cer > 0.0 && cer <= 1.0, "CER must be in (0, 1] for single char substitution: {cer}");
    }

    #[test]
    fn wer_returns_zero_for_both_empty() {
        assert_eq!(compute_wer("", ""), 0.0);
    }

    #[test]
    fn cer_returns_zero_for_both_empty() {
        assert_eq!(compute_cer("", ""), 0.0);
    }

    #[test]
    fn wer_clamped_at_one_for_many_insertions() {
        assert_eq!(compute_wer("word", "word one two three four five"), 1.0);
    }

    #[test]
    fn cer_clamped_at_one_for_many_insertions() {
        assert_eq!(compute_cer("k", "k one two three four five"), 1.0);
    }

    #[test]
    fn normalize_collapses_whitespace() {
        let got = super::normalize_for_metrics("  hello    world  ");
        assert_eq!(got, "hello world");
    }

    #[test]
    fn test_orthographic_equivalence() {
        // Kaf equivalence
        assert_eq!(compute_wer("كوردستان", "کوردستان"), 0.0);
        assert_eq!(compute_cer("كوردستان", "کوردستان"), 0.0);

        // Yeh equivalence
        assert_eq!(compute_wer("على", "علی"), 0.0);
        assert_eq!(compute_cer("على", "علی"), 0.0);

        // ZWNJ equivalence
        assert_eq!(compute_wer("ئەو\u{200C}کەسە", "ئەو کەسە"), 0.0);
        assert_eq!(compute_cer("ئەو\u{200C}کەسە", "ئەو کەسە"), 0.0);

        // Diacritics equivalence
        assert_eq!(compute_wer("كُورْدِي", "کوردی"), 0.0);
        assert_eq!(compute_cer("كُورْدِي", "کوردی"), 0.0);
    }

    /// REGRESSION FIXTURE — externally cross-validated against jiwer 4.0.0.
    ///
    /// These are this repo's `compute_wer`/`compute_cer` outputs AND they are confirmed to match
    /// the independent `jiwer` library within 1e-6 on identical (Rust-normalized) input by
    /// `scripts/crossval_jiwer.py` (blueprint M1.1) — so this is no longer self-referential.
    /// (The two empty-reference cases are a documented convention divergence: Rust clamps WER/CER
    /// to 1.0, jiwer returns the raw insertion count.) Regenerate the cross-val vectors with the
    /// `emit_crossval_vectors` test. Any change to `normalize_for_metrics`/`compute_wer`/`compute_cer`
    /// that moves a line below is a regression to investigate before merging.
    #[test]
    fn jiwer_fixture_matches_reference_values() {
        let mut failures = Vec::new();

        for (reference, hypothesis, expected_wer, expected_cer) in [
            ("hello world", "hello world", 0.0000000000, 0.0000000000),
            ("hello world", "hello earth", 0.5000000000, 0.3636363636),
            ("hello world", "hello", 0.5000000000, 0.5454545455),
            ("", "hello world", 1.0000000000, 1.0000000000),
            ("", "", 0.0000000000, 0.0000000000),
            ("a b c", "a b d", 0.3333333333, 0.2000000000),
            ("کوردی", "کوردی", 0.0000000000, 0.0000000000),
            ("کوردی", "کوردێ", 1.0000000000, 0.2000000000),
            ("hello   world", "hello world", 0.0000000000, 0.0000000000),
            ("Hello World", "hello world", 0.0000000000, 0.0000000000),
            ("ab cd", "abcd", 1.0000000000, 0.2000000000),
        ] {
            let wer = compute_wer(reference, hypothesis);
            let cer = compute_cer(reference, hypothesis);

            if (wer - expected_wer).abs() > 1e-9 {
                failures.push(format!(
                    "WER mismatch for reference={reference:?}, hypothesis={hypothesis:?}: got={wer}, expected={expected_wer}"
                ));
            }

            if (cer - expected_cer).abs() > 1e-9 {
                failures.push(format!(
                    "CER mismatch for reference={reference:?}, hypothesis={hypothesis:?}: got={cer}, expected={expected_cer}"
                ));
            }
        }

        if !failures.is_empty() {
            panic!("jiwer fixture regression:\n{}", failures.join("\n"));
        }
    }

    /// Minimal JSON string escaper (avoids a serde dependency in this emit-only test).
    fn json_str(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// Emit cross-validation vectors for the EXTERNAL jiwer check (`scripts/crossval_jiwer.py`).
    /// Writes this repo's ACTUAL normalized strings + WER/CER so the Python side runs jiwer on
    /// IDENTICAL (Rust-normalized) input — isolating the metric math from normalization. This is
    /// what makes the cross-check real rather than self-referential (blueprint M1.1).
    ///
    /// Run explicitly:
    ///   CORTEX_EMIT_CROSSVAL=1 cargo test --manifest-path src-tauri/Cargo.toml --lib \
    ///       emit_crossval_vectors -- --ignored --nocapture
    #[test]
    #[ignore]
    fn emit_crossval_vectors() {
        if std::env::var("CORTEX_EMIT_CROSSVAL").is_err() {
            return;
        }
        let pairs = [
            ("hello world", "hello world"),
            ("hello world", "hello earth"),
            ("hello world", "hello"),
            ("", "hello world"),
            ("", ""),
            ("a b c", "a b d"),
            ("کوردی", "کوردی"),
            ("کوردی", "کوردێ"),
            ("hello   world", "hello world"),
            ("Hello World", "hello world"),
            ("ab cd", "abcd"),
            // orthographic-equivalence cases (Rust normalization should drive these to 0):
            ("كوردستان", "کوردستان"),
            ("ئەو\u{200C}کەسە", "ئەو کەسە"),
            ("كُورْدِي", "کوردی"),
        ];
        let mut rows = Vec::new();
        for (r, h) in pairs {
            let nr = super::normalize_for_metrics(r);
            let nh = super::normalize_for_metrics(h);
            let wer = compute_wer(r, h);
            let cer = compute_cer(r, h);
            rows.push(format!(
                "  {{\"reference\": {}, \"hypothesis\": {}, \"norm_reference\": {}, \"norm_hypothesis\": {}, \"rust_wer\": {:.10}, \"rust_cer\": {:.10}}}",
                json_str(r),
                json_str(h),
                json_str(&nr),
                json_str(&nh),
                wer,
                cer
            ));
        }
        let out = format!("[\n{}\n]\n", rows.join(",\n"));
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/crossval_vectors.json");
        std::fs::write(path, out).expect("write crossval vectors");
        eprintln!("wrote crossval vectors -> {path}");
    }
}
