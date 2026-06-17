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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_zero_wer() {
        assert_eq!(compute_wer("hello world", "hello world"), 0.0);
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
