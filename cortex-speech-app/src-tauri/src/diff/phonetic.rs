use crate::diff::{DiffChange, DiffOp, DiffStats, TextDiff};

fn phone_distance(p1: char, p2: char) -> f64 {
    if p1 == p2 {
        return 0.0;
    }
    match (p1, p2) {
        ('p', 'b') | ('b', 'p') => 0.3,
        ('t', 'd') | ('d', 't') => 0.3,
        ('k', 'g') | ('g', 'k') => 0.3,
        ('s', 'z') | ('z', 's') => 0.3,
        ('S', 'Z') | ('Z', 'S') => 0.3,
        ('f', 'v') | ('v', 'f') => 0.3,
        ('x', 'G') | ('G', 'x') => 0.3,
        ('r', 'R') | ('R', 'r') => 0.2,
        ('l', 'L') | ('L', 'l') => 0.2,
        ('m', 'n') | ('n', 'm') => 0.4,
        ('a', 'e') | ('e', 'a') => 0.3,
        ('A', 'a') | ('a', 'A') => 0.3,
        ('o', 'u') | ('u', 'o') => 0.3,
        ('u', 'U') | ('U', 'u') => 0.2,
        ('i', 'e') | ('e', 'i') => 0.3,
        _ => 1.0,
    }
}

/// Compute the phonetic Levenshtein edit distance between the phoneme sequences of two words.
pub fn phonetic_word_distance(w1: &str, w2: &str) -> f64 {
    if w1 == w2 {
        return 0.0;
    }
    let p1 = crate::normalizer::g2p::g2p(w1).replace(" ", "");
    let p2 = crate::normalizer::g2p::g2p(w2).replace(" ", "");
    if p1 == p2 {
        return 0.0;
    }

    let chars1: Vec<char> = p1.chars().collect();
    let chars2: Vec<char> = p2.chars().collect();
    let m = chars1.len();
    let n = chars2.len();

    let mut dp = vec![vec![0.0f64; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate().take(m + 1) {
        row[0] = i as f64;
    }
    for (j, cell) in dp[0].iter_mut().enumerate().take(n + 1) {
        *cell = j as f64;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = phone_distance(chars1[i - 1], chars2[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1.0) // deletion
                .min(dp[i][j - 1] + 1.0) // insertion
                .min(dp[i - 1][j - 1] + cost); // substitution
        }
    }

    dp[m][n]
}

/// Compute the normalized phonetic edit distance between two words (0.0 to 1.0).
pub fn normalized_phonetic_word_distance(w1: &str, w2: &str) -> f64 {
    let len1 = crate::normalizer::g2p::g2p(w1).replace(" ", "").chars().count();
    let len2 = crate::normalizer::g2p::g2p(w2).replace(" ", "").chars().count();
    let max_len = len1.max(len2);
    if max_len == 0 {
        // Neither word carries phonetic content (digits / Latin / symbols g2p to nothing). Two DIFFERENT
        // surface tokens then have NO phonetic evidence of a match — returning 0.0 here let a LOOP-0
        // correction memory keyed on one numeric/Latin token fire on a DIFFERENT one (e.g. rewrite a
        // correct "٢٠٢٥" to the "٢٠٢٣" a past edit happened to learn). Only an exact surface match counts.
        return if w1 == w2 { 0.0 } else { 1.0 };
    }
    phonetic_word_distance(w1, w2) / max_len as f64
}

/// Aligns two word sequences using normalized phonetic word distance as substitution cost.
pub fn compute_phonetic_diff(raw: &str, annotated: &str) -> TextDiff {
    let raw_words: Vec<&str> = raw.split_whitespace().collect();
    let ann_words: Vec<&str> = annotated.split_whitespace().collect();

    let m = raw_words.len();
    let n = ann_words.len();

    if m > 1000 || n > 1000 {
        // Fallback for extremely large sequences to prevent performance issues
        return crate::diff::compute_diff(raw, annotated);
    }

    let mut dp = vec![vec![0.0f64; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate().take(m + 1) {
        row[0] = i as f64;
    }
    for (j, cell) in dp[0].iter_mut().enumerate().take(n + 1) {
        *cell = j as f64;
    }

    for i in 1..=m {
        for j in 1..=n {
            let sub_cost = normalized_phonetic_word_distance(raw_words[i - 1], ann_words[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1.0).min(dp[i][j - 1] + 1.0).min(dp[i - 1][j - 1] + sub_cost);
        }
    }

    // Backtrack to build diff changes
    let mut changes = Vec::new();
    let mut i = m;
    let mut j = n;

    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let sub_cost = normalized_phonetic_word_distance(raw_words[i - 1], ann_words[j - 1]);
            let current = dp[i][j];
            if (current - (dp[i - 1][j - 1] + sub_cost)).abs() < 1e-5 {
                // `Equal` must mean the surfaces are IDENTICAL, not merely homophones. Two words can
                // align at sub_cost 0 yet differ in spelling (e.g. Arabic heh ه vs Kurdish heh ھ,
                // which g2p both map to "h"). Emitting Equal there would carry only the raw word and
                // silently discard the annotated surface form — and a consumer like irt.rs records
                // change.value as the other model's token, so it would log the wrong spelling.
                if sub_cost == 0.0 && raw_words[i - 1] == ann_words[j - 1] {
                    changes.push(DiffChange { op: DiffOp::Equal, value: raw_words[i - 1].to_string() });
                } else {
                    changes.push(DiffChange {
                        op: DiffOp::Replace,
                        value: format!("{} → {}", raw_words[i - 1], ann_words[j - 1]),
                    });
                }
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 {
            let current = dp[i][j];
            if (current - (dp[i - 1][j] + 1.0)).abs() < 1e-5 {
                changes.push(DiffChange { op: DiffOp::Delete, value: raw_words[i - 1].to_string() });
                i -= 1;
                continue;
            }
        }
        if j > 0 {
            changes.push(DiffChange { op: DiffOp::Insert, value: ann_words[j - 1].to_string() });
            j -= 1;
        }
    }

    changes.reverse();

    // Compute stats
    let mut added = 0;
    let mut removed = 0;
    let mut changed = 0;
    let mut unchanged = 0;
    for c in &changes {
        match c.op {
            DiffOp::Equal => unchanged += 1,
            DiffOp::Insert => added += 1,
            DiffOp::Delete => removed += 1,
            DiffOp::Replace => changed += 1,
        }
    }

    let total = added + removed + changed + unchanged;
    let similarity = if total == 0 { 100.0 } else { unchanged as f64 / total as f64 * 100.0 };

    TextDiff {
        raw: raw.to_string(),
        annotated: annotated.to_string(),
        changes,
        stats: DiffStats {
            added_words: added,
            removed_words: removed,
            changed_words: changed,
            unchanged_words: unchanged,
            similarity,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phonetic_diff() {
        let raw = "کوردستان";
        let ann = "کوردستان";
        let diff = compute_phonetic_diff(raw, ann);
        assert_eq!(diff.stats.similarity, 100.0);

        let raw2 = "ساڵەکانی";
        let ann2 = "ساڵانی";
        let diff2 = compute_phonetic_diff(raw2, ann2);
        // They should align via Replace because of phonetic closeness
        assert_eq!(diff2.changes.len(), 1);
        assert!(matches!(diff2.changes[0].op, DiffOp::Replace));
    }

    #[test]
    fn distinct_non_phonetic_tokens_are_max_distance_not_a_match() {
        // Two DIFFERENT tokens that both g2p to nothing (digits / Latin / symbols) must NOT score as a
        // phonetic match — otherwise a LOOP-0 correction memory keyed on one number rewrites a different
        // one. Only an exact surface match is distance 0.
        assert_eq!(normalized_phonetic_word_distance("٢٠٢٥", "٢٠٢٤"), 1.0);
        assert_eq!(normalized_phonetic_word_distance("abc", "xyz"), 1.0);
        assert_eq!(normalized_phonetic_word_distance("٢٠٢٤", "٢٠٢٤"), 0.0); // identical surface = real match
                                                                            // A real Kurdish word pair still aligns on phonetic closeness (regression guard).
        assert!(normalized_phonetic_word_distance("کوردستان", "کوردستان") < 0.01);
    }

    #[test]
    fn homophone_with_different_spelling_is_replace_not_equal() {
        // Arabic heh (ه U+0647) vs Kurdish heh (ھ U+06BE): identical phonemes, different spelling.
        // Must be a Replace that preserves the annotated surface form — Equal would discard it and a
        // consumer (irt.rs) would record the wrong token as the other model's output.
        let diff = compute_phonetic_diff("هاتن", "ھاتن");
        assert_eq!(diff.changes.len(), 1);
        assert!(
            matches!(diff.changes[0].op, DiffOp::Replace),
            "phonetically-equal but textually-different words must be Replace, got {:?}",
            diff.changes[0].op
        );
        assert!(diff.changes[0].value.contains("ھاتن"), "annotated surface form must be preserved");

        // Truly identical words remain Equal.
        let same = compute_phonetic_diff("هاتن", "هاتن");
        assert!(matches!(same.changes[0].op, DiffOp::Equal));
    }
}
