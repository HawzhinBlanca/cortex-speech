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

    /// The phoneme confusion table, pinned cost by cost.
    ///
    /// Nothing tested it: the first full cargo-mutants sweep left 16 mutants alive inside
    /// `phone_distance` alone, because every existing test only looked at whole-word outcomes
    /// ("these align as Replace"), which survives any individual cost being wrong. These costs are
    /// the substitution weights of the phonetic edit distance, so they decide which ASR hypothesis
    /// wins a slot in the IRT consensus - a wrong weight silently changes which transcript is
    /// committed, and it would never show up as a crash.
    ///
    /// The classes come from the module's design: voicing pairs are cheap (0.3), liquid/length
    /// variants cheaper still (0.2), the nasal pair mid (0.4), and anything unrelated is a full
    /// substitution (1.0).
    #[test]
    fn phone_distance_cost_table_is_exact() {
        // Identity costs nothing.
        for c in ['p', 'b', 'a', 'S', 'R'] {
            assert_eq!(phone_distance(c, c), 0.0, "identical phonemes must cost 0");
        }

        // Voiced/voiceless pairs: 0.3.
        for (x, y) in [('p', 'b'), ('t', 'd'), ('k', 'g'), ('s', 'z'), ('S', 'Z'), ('f', 'v'), ('x', 'G')] {
            assert_eq!(phone_distance(x, y), 0.3, "voicing pair {x}/{y}");
            assert_eq!(phone_distance(y, x), 0.3, "voicing pair must be symmetric: {y}/{x}");
        }

        // Liquid / vowel-length variants: 0.2 (the cheapest non-identity class).
        for (x, y) in [('r', 'R'), ('l', 'L'), ('u', 'U')] {
            assert_eq!(phone_distance(x, y), 0.2, "liquid/length pair {x}/{y}");
            assert_eq!(phone_distance(y, x), 0.2, "must be symmetric: {y}/{x}");
        }

        // Nasals are a distinct, more expensive class: 0.4.
        assert_eq!(phone_distance('m', 'n'), 0.4);
        assert_eq!(phone_distance('n', 'm'), 0.4);

        // Vowel confusions: 0.3.
        for (x, y) in [('a', 'e'), ('A', 'a'), ('o', 'u'), ('i', 'e')] {
            assert_eq!(phone_distance(x, y), 0.3, "vowel pair {x}/{y}");
            assert_eq!(phone_distance(y, x), 0.3, "must be symmetric: {y}/{x}");
        }

        // Anything not in the table is a full substitution. Includes pairs that are individually
        // in the table but not with EACH OTHER - the table is a set of pairs, not an equivalence.
        for (x, y) in [('p', 'z'), ('k', 'm'), ('a', 'u'), ('b', 'd'), ('s', 'S'), ('r', 'l')] {
            assert_eq!(phone_distance(x, y), 1.0, "unrelated pair {x}/{y} must be a full substitution");
        }

        // The ordering the classes encode must hold: length < voicing < nasal < unrelated.
        assert!(phone_distance('r', 'R') < phone_distance('p', 'b'));
        assert!(phone_distance('p', 'b') < phone_distance('m', 'n'));
        assert!(phone_distance('m', 'n') < phone_distance('p', 'z'));
    }

    /// GOLDEN phonetic distances on real Sorani pairs, each chosen to exercise a different edge of
    /// the DP. Values were measured from the real g2p + edit distance, and each is explained by the
    /// phoneme strings so a future reader can re-derive it rather than trust it.
    ///
    /// The previous tests asserted only "< 0.01" or "== 1.0" at the extremes, which is why 16
    /// mutants lived inside `phonetic_word_distance`: the deletion/insertion cost, the
    /// substitution cost lookup, and the normalising divisor were all free to be wrong in the
    /// middle of the range. That range is exactly where this function does its work - it is the
    /// substitution cost that decides ASR hypothesis alignment in the IRT consensus.
    #[test]
    fn phonetic_word_distance_golden() {
        let g2p = |w: &str| crate::normalizer::g2p::g2p(w).replace(" ", "");

        // Identical surface and identical phonemes: zero on both scales.
        assert_eq!(g2p("کوردستان"), "kurdstAn");
        assert_eq!(phonetic_word_distance("کوردستان", "کوردستان"), 0.0);
        assert_eq!(normalized_phonetic_word_distance("کوردستان", "کوردستان"), 0.0);

        // Homophones, different spelling: Arabic heh vs Kurdish heh both -> /hAtn/, so the
        // PHONETIC distance is 0 even though the surfaces differ (the Equal-vs-Replace rule in
        // compute_phonetic_diff is what keeps that honest downstream).
        assert_eq!(g2p("هاتن"), "hAtn");
        assert_eq!(g2p("ھاتن"), "hAtn");
        assert_eq!(phonetic_word_distance("هاتن", "ھاتن"), 0.0);

        // One VOICING substitution: /bayAni/ vs /payAni/ differ only b->p, which the table prices
        // at 0.3. Normalised over 6 phonemes = 0.05. This is the case that proves the table is
        // actually consulted by the DP rather than every substitution costing a flat 1.0.
        assert_eq!((g2p("بەیانی"), g2p("پەیانی")), ("bayAni".into(), "payAni".into()));
        assert!((phonetic_word_distance("بەیانی", "پەیانی") - 0.3).abs() < 1e-12);
        assert!((normalized_phonetic_word_distance("بەیانی", "پەیانی") - 0.05).abs() < 1e-12);

        // One UNRELATED substitution: /danga/ vs /Ranga/ differ d->R, not a table pair, so full
        // cost 1.0; normalised over 5 phonemes = 0.2.
        assert!((phonetic_word_distance("دەنگە", "ڕەنگە") - 1.0).abs() < 1e-12);
        assert!((normalized_phonetic_word_distance("دەنگە", "ڕەنگە") - 0.2).abs() < 1e-12);

        // Two DELETIONS: /sALakAni/ -> /sALAni/ drops "ka", at 1.0 each = 2.0; normalised over the
        // longer form's 8 phonemes = 0.25. Pins the deletion arm of the recurrence and the fact
        // that the divisor is the MAX length, not the min or the sum.
        assert!((phonetic_word_distance("ساڵەکانی", "ساڵانی") - 2.0).abs() < 1e-12);
        assert!((normalized_phonetic_word_distance("ساڵەکانی", "ساڵانی") - 0.25).abs() < 1e-12);

        // Ordering the costs must preserve: voicing slip < unrelated slip < multi-phoneme edit.
        assert!(
            normalized_phonetic_word_distance("بەیانی", "پەیانی") < normalized_phonetic_word_distance("دەنگە", "ڕەنگە")
        );

        // A large ASYMMETRIC length difference leans on the DP's initialised first row/column
        // (row[0] = i, dp[0][j] = j). If either init loop stops one short, the deletion-only path
        // is scored from an uninitialised 0 and the distance collapses.
        let long_short = phonetic_word_distance("کوردستان", "کو");
        assert!(
            long_short >= 5.0,
            "deleting most of /kurdstAn/ must cost about one per dropped phoneme, got {long_short}"
        );
        let norm_ls = normalized_phonetic_word_distance("کوردستان", "کو");
        assert!(norm_ls > 0.6 && norm_ls <= 1.0, "a mostly-deleted word must be far from a match, got {norm_ls}");
    }

    /// Exact op sequences from the phonetic aligner's backtrack.
    ///
    /// The old tests checked `changes.len() == 1` and "op is Replace", which leaves the whole
    /// backtrack free: 29 mutants lived in `compute_phonetic_diff`. The alignment is what the
    /// review UI renders and what irt.rs reads as the other model's token, so a shifted or
    /// mis-ordered op is a product defect.
    #[test]
    fn phonetic_alignment_golden() {
        let ops = |raw: &str, ann: &str| {
            compute_phonetic_diff(raw, ann)
                .changes
                .iter()
                .map(|c| match c.op {
                    DiffOp::Equal => format!("={}", c.value),
                    DiffOp::Insert => format!("+{}", c.value),
                    DiffOp::Delete => format!("-{}", c.value),
                    DiffOp::Replace => format!("~{}", c.value),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };

        // Order is preserved after the reverse(): the FIRST word must come first.
        assert_eq!(ops("کوردستان هاتن", "کوردستان هاتن"), "=کوردستان =هاتن");

        // A near-homophone substitution in the middle keeps both neighbours Equal.
        assert_eq!(ops("کوردستان بەیانی هاتن", "کوردستان پەیانی هاتن"), "=کوردستان ~بەیانی → پەیانی =هاتن");

        // Pure insertion and deletion at the tail.
        assert_eq!(ops("کوردستان", "کوردستان هاتن"), "=کوردستان +هاتن");
        assert_eq!(ops("کوردستان هاتن", "کوردستان"), "=کوردستان -هاتن");

        // Empty sides.
        assert_eq!(ops("", ""), "");
        assert_eq!(ops("هاتن", ""), "-هاتن");
        assert_eq!(ops("", "هاتن"), "+هاتن");

        // Stats follow the ops exactly.
        let d = compute_phonetic_diff("کوردستان بەیانی هاتن", "کوردستان پەیانی هاتن");
        assert_eq!((d.stats.unchanged_words, d.stats.changed_words), (2, 1));
        assert!((d.stats.similarity - (2.0 / 3.0 * 100.0)).abs() < 1e-9);

        // Every counter NON-ZERO at once. The case above leaves added and removed at 0, where
        // `+= 1` and `*= 1` are indistinguishable - which is exactly why those mutants survived.
        let d2 = compute_phonetic_diff("کوردستان بەیانی زۆر", "کوردستان پەیانی هاتن نوێ");
        let st = &d2.stats;
        assert_eq!(
            st.unchanged_words + st.added_words + st.removed_words + st.changed_words,
            d2.changes.len(),
            "the four counters must sum to the op count"
        );
        assert!(st.unchanged_words > 0 && st.changed_words > 0, "expected equal and replaced words");
        assert!(st.added_words > 0, "the longer annotation must register an insertion");

        // And a case with a DELETION, so `removed` is non-zero too - with it at 0 the `+= 1` and
        // `*= 1` mutants on that counter are identical, which is why one of them survived the
        // first pass.
        let d3 = compute_phonetic_diff("کوردستان بەیانی هاتن نوێ", "کوردستان پەیانی");
        let st3 = &d3.stats;
        assert!(st3.removed_words > 0, "a shorter annotation must register deletions");
        assert_eq!(
            st3.unchanged_words + st3.added_words + st3.removed_words + st3.changed_words,
            d3.changes.len(),
            "counters must still sum to the op count when deletions are present"
        );
        assert!(
            (st.similarity - (st.unchanged_words as f64 / d2.changes.len() as f64 * 100.0)).abs() < 1e-9,
            "similarity must be unchanged/total, got {}",
            st.similarity
        );
    }

    /// The >1000-word fallback to the plain LCS differ. `compute_phonetic_diff` is O(m*n) with a
    /// g2p + edit-distance call per cell, so a long transcript would hang the UI; past the cap it
    /// delegates. Nothing covered the boundary, so `>` mutants there survived.
    #[test]
    fn oversized_word_count_falls_back_to_the_plain_differ() {
        // A pair where the two differs DISAGREE, so the test can tell which one ran: these words
        // are phonetically close (one voicing slip), which the phonetic aligner reports as a single
        // Replace, while the plain LCS differ sees two unrelated tokens.
        let phon = compute_phonetic_diff("بەیانی", "پەیانی");
        assert_eq!(phon.changes.len(), 1, "phonetic path aligns a near-homophone as one Replace");

        // 1001 words on one side must take the plain path. Identical content keeps the assertion
        // about WHICH path ran independent of alignment quality: the plain differ marks all Equal.
        let long_raw = vec!["کوردستان"; 1001].join(" ");
        let d = compute_phonetic_diff(&long_raw, &long_raw);
        assert_eq!(d.changes.len(), 1001);
        assert!(d.changes.iter().all(|c| c.op == DiffOp::Equal));

        // NOT pinned here: the exact `> 1000` vs `>= 1000` edge, and the `>` -> `<` inversion.
        // Both need 1000+ words on BOTH sides for the phonetic aligner to actually run, which is a
        // 1,000,000-cell DP with a g2p + inner edit distance per cell - measured at ~27s, in a
        // suite that runs in every CI job and once per mutant during a mutation sweep. Paying that
        // forever to pin an off-by-one on a performance guard is the wrong trade.
        //
        // Checked rather than assumed: on small inputs the phonetic and plain differs were compared
        // directly across reordering, insertion, deletion and near-homophone cases and produced
        // IDENTICAL op sequences every time, so an inverted guard changes nothing observable below
        // the cap - it is near-equivalent there by construction. Above the cap the 1001-word case
        // above would route into the million-cell DP, where the cost itself is the signal.
        // Same call as the 12.5M-cell guard in diff/mod.rs.
    }

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
