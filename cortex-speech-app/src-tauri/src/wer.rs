//! Word Error Rate (WER) and Character Error Rate (CER) for annotated vs hypothesis text.

/// Default maximum WER (35%) for dataset quality gates.
pub const DEFAULT_MAX_WER: f64 = 0.35;

/// Default maximum CER (20%) for dataset quality gates.
pub const DEFAULT_MAX_CER: f64 = 0.20;

/// Tokenize on whitespace for word-level metrics (works for Sorani Arabic script).
pub fn tokenize_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(|w| w.to_string()).filter(|w| !w.is_empty()).collect()
}

/// Unicode scalar characters for CER (important for Arabic/Kurdish script).
pub fn tokenize_chars(text: &str) -> Vec<char> {
    text.chars().filter(|c| !c.is_whitespace()).collect()
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
        return EditDistanceResult {
            distance: if hyp_words.is_empty() { 0 } else { 1 },
            ref_len: 0,
        };
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
        return EditDistanceResult {
            distance: if hyp_chars.is_empty() { 0 } else { 1 },
            ref_len: 0,
        };
    }

    let distance = levenshtein(&ref_chars, &hyp_chars);
    EditDistanceResult { distance, ref_len: ref_chars.len() }
}

/// Word Error Rate in \[0, 1\]. Returns `1.0` when reference is empty but hypothesis is not.
/// The raw edit-distance ratio is clamped to 1.0 so callers never observe a WER above 100%.
pub fn compute_wer(reference: &str, hypothesis: &str) -> f64 {
    let res = word_edit_distance(reference, hypothesis);
    if res.ref_len == 0 {
        return res.distance as f64;
    }
    (res.distance as f64 / res.ref_len as f64).min(1.0)
}

/// Character Error Rate in \[0, 1\].
/// The raw edit-distance ratio is clamped to 1.0 so callers never observe a CER above 100%.
pub fn compute_cer(reference: &str, hypothesis: &str) -> f64 {
    let res = char_edit_distance(reference, hypothesis);
    if res.ref_len == 0 {
        return res.distance as f64;
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
            assert_eq!(bd.total(), word_edit_distance(r, h).distance, "S+D+I must equal edit distance for ({r:?},{h:?})");
        }
    }

    #[test]
    fn breakdown_reports_true_insertions_for_empty_reference() {
        // word_edit_distance clamps empty-ref to distance 1; the breakdown is HONEST and
        // reports the true insertion count instead.
        let bd = word_error_breakdown("", "one two");
        assert_eq!(bd.insertions, 2);
        assert_eq!(bd.deletions, 0);
        assert_eq!(bd.substitutions, 0);
        assert_eq!(bd.ref_len, 0);
        assert_eq!(bd.total(), 2);
    }

    #[test]
    fn breakdown_rate_is_unclamped_unlike_compute_wer() {
        // Many spurious insertions → honest rate > 1.0, while compute_wer clamps to 1.0.
        let bd = word_error_breakdown("word", "word one two three four five");
        assert!(bd.rate() > 1.0, "unclamped rate should exceed 1.0: {}", bd.rate());
        assert_eq!(compute_wer("word", "word one two three four five"), 1.0);
    }

    #[test]
    fn one_substitution_wer() {
        assert!((compute_wer("hello world", "hello earth") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn empty_reference_full_error() {
        assert_eq!(compute_wer("", "hello"), 1.0);
    }

    #[test]
    fn cer_counts_unicode_chars() {
        assert_eq!(compute_cer("سلاو", "سلاو"), 0.0);
        assert!(compute_cer("سلاو", "سلام") > 0.0);
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize_for_metrics("  Hello   WORLD  "), "hello world");
    }

    #[test]
    fn wer_clamped_at_one_for_many_insertions() {
        // A hypothesis with many extra words should produce WER = 1.0, not > 1.0.
        // Without clamping the raw ratio would be 10/1 = 10.0.
        let wer = compute_wer("word", "word one two three four five six seven eight nine ten");
        assert!(wer <= 1.0, "WER must not exceed 1.0: got {wer}");
        assert!(wer > 0.0, "WER must not be 0.0 for a mismatched hypothesis: got {wer}");
    }

    #[test]
    fn cer_clamped_at_one_for_many_insertions() {
        // A hypothesis with many extra characters should produce CER = 1.0, not > 1.0.
        let cer = compute_cer("ب", "بەئەوەکانی ژیاندا دەبیتە بەرپرسایەتی");
        assert!(cer <= 1.0, "CER must not exceed 1.0: got {cer}");
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
    fn wer_kurdish_identical() {
        // Identical Sorani text should give 0.0 WER.
        assert_eq!(compute_wer("ئەم خەباتە بۆ ئازادی", "ئەم خەباتە بۆ ئازادی"), 0.0);
    }

    #[test]
    fn cer_kurdish_one_substitution() {
        // Single character swap in Kurdish script.
        let cer = compute_cer("کوردی", "کوردێ");
        assert!(cer > 0.0 && cer <= 1.0, "CER must be in (0, 1] for single char substitution: {cer}");
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
}
