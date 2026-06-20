//! LOOP-0 error-memory extraction from a human correction.
//!
//! When a curator changes a word, we want the system to get that *same* word right next time with
//! NO retraining. The first step is turning a (wrong transcript, human fix) pair into structured
//! per-slot memories: the wrong token, the human's replacement, the canonical ±1 neighbor context
//! (the "slot key" a future decode matches on), and a phonetic key for the wrong token.
//!
//! The alignment is computed over NORMALIZED words, so a pure orthographic variant (Kaf/Yeh/Heh
//! codepoint differences) is a *match*, never a spurious "correction" — the moat is real fixes, not
//! normalization noise. This module is pure (no DB, no I/O); wiring the emitted memories into the
//! `correction_memory` table is a separate, thin step.

use crate::normalizer::{NormalizationConfig, SoraniNormalizer};

/// One learned substitution extracted from a human correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstitutionMemory {
    /// The model's wrong token (original surface form, not normalized).
    pub wrong_token: String,
    /// The human's replacement token (original surface form).
    pub human_token: String,
    /// `normalize(left_neighbor)|normalize(right_neighbor)` — the canonical windowed context that a
    /// future decode matches on. Empty sides mean the slot was at a sentence boundary.
    pub slot_key: String,
    /// `g2p(normalize(wrong_token))` — the phonetic key for similarity-gated firing.
    pub phonetic_key: String,
}

/// The char-only canonical normalizer: folds Kaf/Yeh/Heh/ZWNJ/tatweel/hamza but does NOT verbalize
/// numbers, so slot/phonetic keys are orthographically canonical without turning digits into words.
fn char_only_normalizer() -> SoraniNormalizer {
    SoraniNormalizer::with_config(NormalizationConfig {
        normalize_numbers: false,
        verbalize_numbers: false,
        normalize_hamza: true,
        remove_diacritics: false,
    })
}

#[derive(Clone, Copy)]
enum AlignOp {
    /// A matched slot; carries only the `b`-side index used to look up the neighbor context.
    Match { b: usize },
    /// A substituted slot: `a` is the wrong-side word index, `b` the human-side index.
    Sub { a: usize, b: usize },
    /// A deleted or inserted slot — a placeholder in the path; its indices are never read.
    Del,
    Ins,
}

/// Word-level alignment of normalized words `na` onto `nb`, with backtrace. Returns the ordered op
/// sequence (Match/Sub/Del/Ins) — the standard Levenshtein alignment path.
fn align_words(na: &[String], nb: &[String]) -> Vec<AlignOp> {
    let (n, m) = (na.len(), nb.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(na[i - 1] != nb[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let cost = usize::from(na[i - 1] != nb[j - 1]);
            if dp[i][j] == dp[i - 1][j - 1] + cost {
                ops.push(if cost == 0 {
                    AlignOp::Match { b: j - 1 }
                } else {
                    AlignOp::Sub { a: i - 1, b: j - 1 }
                });
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            ops.push(AlignOp::Del);
            i -= 1;
        } else {
            ops.push(AlignOp::Ins);
            j -= 1;
        }
    }
    ops.reverse();
    ops
}

/// Extract one error-memory entry per *substituted* word in a human correction. `wrong` is the model
/// hypothesis, `right` the human fix. Pure insertions/deletions (added or removed words) are not
/// single-token swaps and are skipped — LOOP 0 only learns "this word should have been that word".
pub fn extract_substitution_memories(wrong: &str, right: &str) -> Vec<SubstitutionMemory> {
    let normalizer = char_only_normalizer();
    let a: Vec<&str> = wrong.split_whitespace().collect();
    let b: Vec<&str> = right.split_whitespace().collect();
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let na: Vec<String> = a.iter().map(|w| normalizer.normalize(w)).collect();
    let nb: Vec<String> = b.iter().map(|w| normalizer.normalize(w)).collect();
    let ops = align_words(&na, &nb);

    let mut out = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        let AlignOp::Sub { a: ai, b: bj } = *op else { continue };
        // The nearest matched word on each side is the canonical +/-1 neighbor context.
        let left = ops[..idx]
            .iter()
            .rev()
            .find_map(|o| if let AlignOp::Match { b } = o { Some(nb[*b].clone()) } else { None })
            .unwrap_or_default();
        let right_ctx = ops[idx + 1..]
            .iter()
            .find_map(|o| if let AlignOp::Match { b } = o { Some(nb[*b].clone()) } else { None })
            .unwrap_or_default();
        out.push(SubstitutionMemory {
            slot_key: format!("{left}|{right_ctx}"),
            phonetic_key: crate::normalizer::g2p::g2p(&na[ai]),
            wrong_token: a[ai].to_string(),
            human_token: b[bj].to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_substitution_with_normalized_neighbors() {
        let mems = extract_substitution_memories("ئەو کابرایە چوو", "ئەو پیاوە چوو");
        assert_eq!(mems.len(), 1, "exactly one substituted word: {mems:?}");
        let m = &mems[0];
        assert_eq!(m.wrong_token, "کابرایە");
        assert_eq!(m.human_token, "پیاوە");
        let norm = char_only_normalizer();
        assert_eq!(m.slot_key, format!("{}|{}", norm.normalize("ئەو"), norm.normalize("چوو")));
        assert!(!m.phonetic_key.is_empty(), "a phonetic key is computed for the wrong token");
    }

    #[test]
    fn orthographic_variant_is_not_a_substitution() {
        // The first word differs only by Kaf/Yeh codepoint variants; after folding it is the SAME
        // word, so no spurious "correction" is learned.
        let wrong = "كوردي زمان"; // Arabic Kaf U+0643 + Arabic Yeh U+064A
        let right = "کوردی زمان"; // Kurdish Keheh U+06A9 + Kurdish Yeh U+06CC
        assert!(
            extract_substitution_memories(wrong, right).is_empty(),
            "a pure orthographic variant must not be learned as a correction"
        );
    }

    #[test]
    fn single_word_substitution_has_empty_neighbor_context() {
        let mems = extract_substitution_memories("کوردی", "کوردستان");
        assert_eq!(mems.len(), 1);
        assert_eq!(mems[0].slot_key, "|", "no neighbors -> empty slot context on both sides");
        assert_eq!(mems[0].wrong_token, "کوردی");
        assert_eq!(mems[0].human_token, "کوردستان");
    }

    #[test]
    fn pure_insertions_and_deletions_yield_no_memories() {
        assert!(
            extract_substitution_memories("ئەو چوو", "ئەو زۆر چوو").is_empty(),
            "an added word is not a single-token substitution"
        );
        assert!(
            extract_substitution_memories("ئەو زۆر چوو", "ئەو چوو").is_empty(),
            "a removed word is not a single-token substitution"
        );
    }

    #[test]
    fn empty_inputs_yield_no_memories() {
        assert!(extract_substitution_memories("", "something").is_empty());
        assert!(extract_substitution_memories("something", "").is_empty());
    }

    #[test]
    fn multiple_substitutions_are_each_captured() {
        let mems = extract_substitution_memories("من ساڵی پێنج بووم", "من ساڵی شەش هاتم");
        assert_eq!(mems.len(), 2, "both substituted words are captured: {mems:?}");
        let wrongs: Vec<&str> = mems.iter().map(|m| m.wrong_token.as_str()).collect();
        assert!(wrongs.contains(&"پێنج"), "{wrongs:?}");
        assert!(wrongs.contains(&"بووم"), "{wrongs:?}");
    }
}
