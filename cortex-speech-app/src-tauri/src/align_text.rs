//! Anchor-based localization of an ASR hypothesis inside a long reference transcript.
//!
//! Why this exists: in the curated TTS dataset the per-clip audio is recorded in a
//! different order than the master script lists its sentences (audio-order ≠
//! transcript-order), and a single clip's spoken words are a near-verbatim *substring*
//! of the full script. To score that clip's WER we must first find **which contiguous
//! span** of the reference it corresponds to. A global edit distance against the whole
//! script would (correctly transcribed!) clips a catastrophic error rate for the simple
//! reason that the clip only covers a fraction of the text.
//!
//! The fix is classic substring localization by shared n-gram anchors. Because the
//! spoken text is near-verbatim, *unique* reference bigrams that also occur in the
//! hypothesis pin down the offset reliably; each anchor votes for an offset
//! `ref_pos - hyp_pos`, and the consensus (modal) offset places the span. This is far
//! more robust than a naive global alignment and needs no model.
//!
//! Everything here is pure and deterministic — no I/O, no model, no clock.

use crate::wer::normalize_for_metrics;
use std::collections::HashMap;

/// The located reference span for a hypothesis, in **normalized** reference-token indices.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanMatch {
    /// Inclusive start word index into the normalized reference token vector.
    pub ref_start: usize,
    /// Exclusive end word index into the normalized reference token vector.
    pub ref_end: usize,
    /// Consensus offset `ref_pos - hyp_pos` chosen by anchor voting.
    pub offset: isize,
    /// Number of anchors supporting the chosen offset (confidence in the localization).
    pub anchors: usize,
    /// Fraction of hypothesis words recovered in the located span, via LCS ∈ [0, 1].
    /// A high coverage means the localization is trustworthy; a low one means the clip
    /// could not be confidently placed (e.g. mostly mistranscribed or absent).
    pub coverage: f64,
    /// The normalized reference tokens `[ref_start, ref_end)` joined by a single space —
    /// exactly the text a WER metric should compare the hypothesis against.
    pub matched_text: String,
}

/// Tokenize after the shared metric normalization so Arabic/Kurdish orthographic
/// variants (kaf, yeh, ZWNJ, diacritics) collapse identically on both sides.
fn normalized_tokens(text: &str) -> Vec<String> {
    normalize_for_metrics(text).split_whitespace().map(|s| s.to_string()).collect()
}

/// Locate the contiguous reference span that best corresponds to `hypothesis`.
///
/// Returns `None` only when there is no shared anchor at all (no overlap, or either
/// side empty). A returned span always has `ref_start <= ref_end <= ref_token_count`;
/// inspect [`SpanMatch::coverage`] to decide whether to trust it.
pub fn locate_reference_span(reference: &str, hypothesis: &str) -> Option<SpanMatch> {
    let ref_tokens = normalized_tokens(reference);
    let hyp_tokens = normalized_tokens(hypothesis);
    if ref_tokens.is_empty() || hyp_tokens.is_empty() {
        return None;
    }

    // Vote for an offset = ref_pos - hyp_pos from anchors. Prefer unique bigrams (a pair
    // of adjacent words occurring exactly once in the reference) — they pin the offset
    // unambiguously. Fall back to unique unigrams only when no bigram anchor exists
    // (e.g. a one-word hypothesis).
    let mut offset_votes: HashMap<isize, usize> = HashMap::new();

    let mut ref_bigrams: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for i in 0..ref_tokens.len().saturating_sub(1) {
        ref_bigrams.entry((ref_tokens[i].as_str(), ref_tokens[i + 1].as_str())).or_default().push(i);
    }
    for j in 0..hyp_tokens.len().saturating_sub(1) {
        let key = (hyp_tokens[j].as_str(), hyp_tokens[j + 1].as_str());
        if let Some(positions) = ref_bigrams.get(&key) {
            if positions.len() == 1 {
                let offset = positions[0] as isize - j as isize;
                *offset_votes.entry(offset).or_default() += 1;
            }
        }
    }

    if offset_votes.is_empty() {
        let mut ref_unigrams: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, w) in ref_tokens.iter().enumerate() {
            ref_unigrams.entry(w.as_str()).or_default().push(i);
        }
        for (j, w) in hyp_tokens.iter().enumerate() {
            if let Some(positions) = ref_unigrams.get(w.as_str()) {
                if positions.len() == 1 {
                    let offset = positions[0] as isize - j as isize;
                    *offset_votes.entry(offset).or_default() += 1;
                }
            }
        }
    }

    if offset_votes.is_empty() {
        return None;
    }

    // Consensus offset: most votes, ties broken toward the smallest offset for
    // determinism (HashMap iteration order is not stable).
    let mut best_offset = isize::MAX;
    let mut anchors = 0usize;
    for (&offset, &votes) in &offset_votes {
        if votes > anchors || (votes == anchors && offset < best_offset) {
            anchors = votes;
            best_offset = offset;
        }
    }

    let ref_len = ref_tokens.len() as isize;
    let ref_start = best_offset.clamp(0, ref_len) as usize;
    let ref_end = (best_offset + hyp_tokens.len() as isize).clamp(0, ref_len) as usize;
    let ref_end = ref_end.max(ref_start);

    let span = &ref_tokens[ref_start..ref_end];
    let lcs = lcs_len(&hyp_tokens, span);
    let coverage = lcs as f64 / hyp_tokens.len() as f64;
    let matched_text = span.join(" ");

    Some(SpanMatch { ref_start, ref_end, offset: best_offset, anchors, coverage, matched_text })
}

/// Length of the longest common subsequence of two word slices (rolling two-row DP).
fn lcs_len(a: &[String], b: &[String]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let m = b.len();
    let mut prev = vec![0usize; m + 1];
    let mut curr = vec![0usize; m + 1];
    for ai in a {
        for (j, bj) in b.iter().enumerate() {
            curr[j + 1] = if ai == bj { prev[j] + 1 } else { curr[j].max(prev[j + 1]) };
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locates_mid_reference_substring() {
        let reference = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        let hypothesis = "delta echo foxtrot golf";
        let m = locate_reference_span(reference, hypothesis).expect("should locate");
        assert_eq!(m.ref_start, 3);
        assert_eq!(m.ref_end, 7);
        assert_eq!(m.matched_text, "delta echo foxtrot golf");
        assert!((m.coverage - 1.0).abs() < 1e-9);
        assert!(m.anchors >= 1);
    }

    #[test]
    fn locates_later_block_when_audio_order_differs() {
        // The whole script: block A first, block B second. The clip's audio is block B —
        // recorded out of script order. Localization must find block B's span, not be
        // dragged to the front of the script.
        let reference = "one two three four five six seven eight nine ten \
                         eleven twelve thirteen fourteen fifteen sixteen";
        let hypothesis = "twelve thirteen fourteen fifteen";
        let m = locate_reference_span(reference, hypothesis).expect("should locate");
        assert_eq!(m.ref_start, 11);
        assert_eq!(m.ref_end, 15);
        assert!((m.coverage - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tolerates_a_single_substitution() {
        // Middle word mistranscribed; flanking unique bigrams still pin the offset.
        let reference = "the quick brown fox jumps over the lazy dog today";
        let hypothesis = "quick brown FROG jumps over";
        let m = locate_reference_span(reference, hypothesis).expect("should locate");
        assert_eq!(m.offset, 1);
        assert_eq!(m.ref_start, 1);
        assert_eq!(m.ref_end, 6);
        // 4 of 5 hypothesis words recovered.
        assert!((m.coverage - 0.8).abs() < 1e-9, "coverage was {}", m.coverage);
    }

    #[test]
    fn ignores_ambiguous_repeated_phrase() {
        // "na na" appears twice → its bigram is not unique and must not vote. The unique
        // "red green" / "green blue" anchors carry the localization to the right copy.
        let reference = "na na zero red green blue na na one yellow";
        let hypothesis = "red green blue";
        let m = locate_reference_span(reference, hypothesis).expect("should locate");
        assert_eq!(m.ref_start, 3);
        assert_eq!(m.ref_end, 6);
        assert!((m.coverage - 1.0).abs() < 1e-9);
    }

    #[test]
    fn single_word_hypothesis_uses_unigram_anchor() {
        let reference = "apple banana cherry date elderberry";
        let hypothesis = "cherry";
        let m = locate_reference_span(reference, hypothesis).expect("should locate");
        assert_eq!(m.ref_start, 2);
        assert_eq!(m.ref_end, 3);
        assert!((m.coverage - 1.0).abs() < 1e-9);
    }

    #[test]
    fn no_overlap_returns_none() {
        let m = locate_reference_span("alpha beta gamma delta", "xi omicron pi rho");
        assert!(m.is_none());
    }

    #[test]
    fn empty_inputs_return_none() {
        assert!(locate_reference_span("", "hello").is_none());
        assert!(locate_reference_span("hello", "").is_none());
        assert!(locate_reference_span("", "").is_none());
    }

    #[test]
    fn locates_across_orthographic_variants() {
        // Reference uses Kurdish kaf (ک U+06A9); hypothesis uses Arabic kaf (ك U+0643)
        // for the same words. Metric normalization collapses them, so the span still
        // anchors. This is the real ASR-vs-script script-mismatch case.
        let reference = "سڵاو ئەمە کوردستانە و کتێبەکە لێرەیە بۆ هەموان";
        let hypothesis = "كوردستانە و كتێبەكە لێرەیە";
        let m = locate_reference_span(reference, hypothesis).expect("should locate");
        assert_eq!(m.ref_start, 2);
        assert_eq!(m.ref_end, 6);
        assert!((m.coverage - 1.0).abs() < 1e-9, "coverage was {}", m.coverage);
    }

    #[test]
    fn span_indices_stay_in_bounds_when_hypothesis_overruns() {
        // Hypothesis starts near the end and is longer than what remains; the span must
        // clamp to the reference length, never panic or index past the end.
        let reference = "a b c d e f g h";
        let hypothesis = "g h i j k l";
        let m = locate_reference_span(reference, hypothesis).expect("should locate");
        assert!(m.ref_end <= 8);
        assert!(m.ref_start <= m.ref_end);
        assert_eq!(m.ref_start, 6);
        assert_eq!(m.ref_end, 8);
    }
}
