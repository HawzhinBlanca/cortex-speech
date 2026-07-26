pub mod phonetic;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DiffOp {
    Equal,
    Insert,
    Delete,
    Replace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffChange {
    pub op: DiffOp,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDiff {
    pub raw: String,
    pub annotated: String,
    pub changes: Vec<DiffChange>,
    pub stats: DiffStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffStats {
    pub added_words: usize,
    pub removed_words: usize,
    pub changed_words: usize,
    pub unchanged_words: usize,
    pub similarity: f64,
}

/// Word-level diff using LCS alignment.
/// Produces Equal, Insert, Delete, or Replace operations.
/// Rejects inputs exceeding 10,000 words to prevent O(n*m) OOM.
pub fn compute_diff(raw: &str, annotated: &str) -> TextDiff {
    let _span = crate::telemetry::TRACER.start_span(
        "diff.compute_diff",
        crate::telemetry::Tracer::metadata(vec![
            ("raw_len", raw.len().to_string()),
            ("ann_len", annotated.len().to_string()),
        ]),
    );
    let raw_words: Vec<&str> = raw.split_whitespace().collect();
    let ann_words: Vec<&str> = annotated.split_whitespace().collect();

    if raw_words.len() > 10_000 || ann_words.len() > 10_000 {
        return TextDiff {
            raw: raw.to_string(),
            annotated: annotated.to_string(),
            changes: vec![],
            stats: DiffStats {
                added_words: 0,
                removed_words: 0,
                changed_words: 0,
                unchanged_words: 0,
                similarity: 100.0,
            },
        };
    }

    // Build LCS table
    let lcs_table = build_lcs_table(&raw_words, &ann_words);
    let lcs = extract_lcs(&raw_words, &ann_words, &lcs_table);

    let mut changes = Vec::new();
    let mut ri = 0usize;
    let mut ai = 0usize;
    let mut lcs_idx = 0usize;

    while ri < raw_words.len() || ai < ann_words.len() {
        // A side "is at the LCS" when its current word equals the next common word. That word MUST be
        // emitted as Equal and never consumed into a Replace/Delete/Insert, or the remainder misaligns.
        let raw_is_lcs = ri < raw_words.len() && lcs_idx < lcs.len() && raw_words[ri] == lcs[lcs_idx];
        let ann_is_lcs = ai < ann_words.len() && lcs_idx < lcs.len() && ann_words[ai] == lcs[lcs_idx];

        // Both sides sit on the next common word → Equal.
        if raw_is_lcs && ann_is_lcs {
            changes.push(DiffChange { op: DiffOp::Equal, value: lcs[lcs_idx].to_string() });
            ri += 1;
            ai += 1;
            lcs_idx += 1;
            continue;
        }

        // Replace ONLY when BOTH words diverge from the LCS (a genuine substitution). Replacing while
        // one side is still on its common word would consume that common word — dropping it from the
        // alignment and cascading wrong ops. That was the bug: an insert/delete next to an unchanged
        // word rendered a spurious "x → y" and undercounted similarity (e.g. "a c" → "a b c" scored
        // 33% with a bogus c→b replace instead of 67% with a clean insert of b).
        if ri < raw_words.len() && ai < ann_words.len() && !raw_is_lcs && !ann_is_lcs {
            changes.push(DiffChange { op: DiffOp::Replace, value: format!("{} → {}", raw_words[ri], ann_words[ai]) });
            ri += 1;
            ai += 1;
            continue;
        }

        // A raw word that is not the next common word → Delete; the annotated side waits at its common
        // word so it aligns as Equal next.
        if ri < raw_words.len() && !raw_is_lcs {
            changes.push(DiffChange { op: DiffOp::Delete, value: raw_words[ri].to_string() });
            ri += 1;
            continue;
        }

        // An annotated word that is not the next common word → Insert (the raw side waits at its common
        // word). Also drains the annotated tail once raw is exhausted.
        if ai < ann_words.len() {
            changes.push(DiffChange { op: DiffOp::Insert, value: ann_words[ai].to_string() });
            ai += 1;
            continue;
        }

        // Only raw remains, sitting on a common word with no annotated partner left → Delete it. (Keeps
        // the loop provably progressing; unreachable for a well-formed LCS but a safe backstop.)
        if ri < raw_words.len() {
            changes.push(DiffChange { op: DiffOp::Delete, value: raw_words[ri].to_string() });
            ri += 1;
        }
    }

    // Compute stats
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut changed = 0usize;
    let mut unchanged = 0usize;
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

fn build_lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<usize>> {
    let m = a.len();
    let n = b.len();

    // Memory guard: reject if the table would exceed ~100MB (10K × 10K × 8 bytes ≈ 800MB)
    // We allow up to ~12.5 million cells = ~100MB
    let max_cells = 12_500_000usize;
    if m.saturating_mul(n) > max_cells {
        return vec![vec![0usize; n + 1]; m + 1];
    }

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i - 1] == b[j - 1] { dp[i - 1][j - 1] + 1 } else { dp[i - 1][j].max(dp[i][j - 1]) };
        }
    }
    dp
}

fn extract_lcs<'a>(a: &[&'a str], b: &[&str], dp: &[Vec<usize>]) -> Vec<&'a str> {
    let mut result = Vec::new();
    let mut i = a.len();
    let mut j = b.len();
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            result.push(a[i - 1]);
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] > dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    result.reverse();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The >10,000-word bail-out. `compute_diff` builds an O(n*m) LCS table, so a pathological
    /// input (a pasted book, a runaway ASR loop) would allocate ~n*m cells and hang the UI thread;
    /// past the cap it returns a no-change diff instead. That guard had NO test at all, which
    /// cargo-mutants proved: mutating `||` to `&&` and `>` to `==` on the condition both survived
    /// the entire suite. Either mutation silently disables the protection for the realistic case
    /// where only ONE side is huge — exactly the shape of the runaway-ASR input it exists to stop.
    ///
    /// Pinned here as behaviour, including the honest bit: the bail-out reports similarity 100.0
    /// and zero changes, i.e. it degrades to "no diff computed" rather than blocking.
    #[test]
    fn oversized_input_bails_out_before_building_the_lcs_table() {
        let huge = "w ".repeat(10_001);
        let small = "hello world";

        // Only the RAW side oversized -> must still bail (this is what `&&` would break).
        let d = compute_diff(&huge, small);
        assert!(d.changes.is_empty(), "oversized raw must short-circuit, got {} changes", d.changes.len());
        assert_eq!(d.stats.similarity, 100.0);
        assert_eq!(d.stats.changed_words, 0);

        // Only the ANNOTATED side oversized -> must still bail.
        let d = compute_diff(small, &huge);
        assert!(d.changes.is_empty(), "oversized annotated must short-circuit");
        assert_eq!(d.stats.similarity, 100.0);

        // Exactly at the cap (10,000) is NOT oversized: the guard is `> 10_000`, so this must
        // still diff normally. This is the case the `>` -> `==` mutant flips.
        let at_cap = "w ".repeat(10_000);
        let d = compute_diff(&at_cap, &at_cap);
        assert!(!d.changes.is_empty(), "exactly 10_000 words must still be diffed, not bailed");
        assert!(d.changes.iter().all(|c| c.op == DiffOp::Equal));
    }

    /// Render a diff as a compact op string so a test can pin the EXACT alignment:
    /// `=w` equal, `+w` insert, `-w` delete, `~v` replace.
    fn ops(raw: &str, ann: &str) -> String {
        compute_diff(raw, ann)
            .changes
            .iter()
            .map(|c| {
                let tag = match c.op {
                    DiffOp::Equal => '=',
                    DiffOp::Insert => '+',
                    DiffOp::Delete => '-',
                    DiffOp::Replace => '~',
                };
                format!("{tag}{}", c.value)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// GOLDEN alignments for the LCS diff. Every expectation was produced by running the function
    /// and then read to confirm it is the alignment a reviewer would want; none was guessed.
    ///
    /// Why exact strings instead of properties: a cargo-mutants sweep over this file left 19
    /// mutants alive, and the existing tests were the reason — they asserted "some Replace exists"
    /// or "similarity is within bounds", which stays true when a loop bound or an index arithmetic
    /// operator is quietly wrong. The alignment is what the review UI shows the curator and what
    /// the word-level corrections ledger records, so a silently shifted diff is a real product
    /// defect, not a cosmetic one. Pinning the op sequence is what actually holds it.
    #[test]
    fn diff_alignment_golden() {
        // Pure equality, and the empty edges.
        assert_eq!(ops("a b c", "a b c"), "=a =b =c");
        assert_eq!(ops("", ""), "");
        assert_eq!(ops("a b", ""), "-a -b");
        assert_eq!(ops("", "a b"), "+a +b");

        // A single word changed in the middle keeps both anchors Equal.
        assert_eq!(ops("a x c", "a y c"), "=a ~x → y =c");

        // Insertion and deletion at the head, middle and tail.
        assert_eq!(ops("b c", "a b c"), "+a =b =c");
        assert_eq!(ops("a b c", "b c"), "-a =b =c");
        assert_eq!(ops("a c", "a b c"), "=a +b =c");
        assert_eq!(ops("a b c", "a c"), "=a -b =c");
        assert_eq!(ops("a b", "a b c"), "=a =b +c");
        assert_eq!(ops("a b c", "a b"), "=a =b -c");

        // Repeated words: the LCS is ambiguous, so this pins WHICH alignment is chosen.
        assert_eq!(ops("a a b", "a b"), "=a -a =b");
        assert_eq!(ops("a b", "a a b"), "=a +a =b");

        // No common words at all -> a clean run of replacements, not interleaved noise.
        assert_eq!(ops("x y", "p q"), "~x → p ~y → q");

        // Longer raw than annotated with a shared prefix AND suffix.
        assert_eq!(ops("a b c d e", "a d e"), "=a -b -c =d =e");
    }

    /// The stats block is what the UI badge and the similarity score are computed from, and
    /// `similarity` divides by the total op count — so an off-by-one in any counter is a wrong
    /// number shown to the curator. Pinned exactly (the `+=` -> `-=` / `*=` mutants survived).
    #[test]
    fn diff_stats_golden() {
        let d = compute_diff("a b c d e", "a d e");
        assert_eq!(d.stats.unchanged_words, 3);
        assert_eq!(d.stats.removed_words, 2);
        assert_eq!(d.stats.added_words, 0);
        assert_eq!(d.stats.changed_words, 0);
        assert!((d.stats.similarity - 60.0).abs() < 1e-9, "3 of 5 ops equal => 60%");

        let d = compute_diff("a x c", "a y c");
        assert_eq!((d.stats.unchanged_words, d.stats.changed_words), (2, 1));
        assert!((d.stats.similarity - (2.0 / 3.0 * 100.0)).abs() < 1e-9);

        let d = compute_diff("a c", "a b c");
        assert_eq!((d.stats.unchanged_words, d.stats.added_words), (2, 1));

        // Empty vs empty: no ops at all must report 100%, not a divide-by-zero NaN.
        let d = compute_diff("", "");
        assert!(d.stats.similarity.is_finite() && (d.stats.similarity - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_identical_texts() {
        let diff = compute_diff("hello world", "hello world");
        assert!(diff.changes.iter().all(|c| c.op == DiffOp::Equal));
        assert_eq!(diff.stats.similarity, 100.0);
    }

    #[test]
    fn test_completely_different() {
        let diff = compute_diff("hello world", "goodbye universe");
        assert!(diff.changes.iter().any(|c| c.op == DiffOp::Replace));
    }

    #[test]
    fn test_insertion() {
        let diff = compute_diff("hello world", "hello beautiful world");
        assert!(diff.changes.iter().any(|c| c.op == DiffOp::Insert));
        assert!(diff.changes.iter().any(|c| c.op == DiffOp::Equal));
    }

    #[test]
    fn test_deletion() {
        let diff = compute_diff("hello beautiful world", "hello world");
        assert!(diff.changes.iter().any(|c| c.op == DiffOp::Delete));
    }

    #[test]
    fn test_empty_inputs() {
        let diff = compute_diff("", "");
        assert!(diff.changes.is_empty());
        assert_eq!(diff.stats.similarity, 100.0);
    }

    #[test]
    fn test_similarity_score() {
        let diff = compute_diff("hello world foo bar", "hello world baz qux");
        // 2 of 4 words unchanged (hello, world), 2 replaced (foo→baz, bar→qux)
        assert!((diff.stats.similarity - 50.0).abs() < 0.01, "got {}", diff.stats.similarity);
    }

    #[test]
    fn test_kurdish_text() {
        let raw = "ئەم کەسە لە ساڵەکانی ١٩٥٠دا دەژیا";
        let ann = "ئەم کەسە لە ساڵانی 1950دا دەژیا";
        let diff = compute_diff(raw, ann);
        // Should detect the change between ساڵەکانی and ساڵانی
        // And between ١٩٥٠دا and 1950دا
        assert!(diff.changes.iter().any(|c| c.op == DiffOp::Replace));
    }

    #[test]
    fn test_partial_overlap() {
        let diff = compute_diff("a b c d", "a x c y");
        assert_eq!(diff.stats.unchanged_words, 2); // a, c
        assert_eq!(diff.stats.changed_words, 2); // b→x, d→y
    }

    #[test]
    fn test_mismatched_lengths() {
        let diff = compute_diff("a b", "a b c d e");
        assert_eq!(diff.stats.unchanged_words, 2);
        assert_eq!(diff.stats.added_words, 3);
    }

    #[test]
    fn test_single_word() {
        let diff = compute_diff("hello", "world");
        assert_eq!(diff.stats.changed_words, 1);
        assert_eq!(diff.stats.similarity, 0.0);
    }

    #[test]
    fn insert_next_to_common_word_is_an_insert_not_a_replace() {
        // Regression: the reconstruction used to Replace whenever both sides had content, even when one
        // word was still the next common word. "a c" → "a b c" is a pure insertion of b, so the only
        // change must be one Insert and the two common words stay Equal (similarity 2/3). The old code
        // emitted [Equal a, Replace(c→b), Insert c] — a bogus substitution that scored 1/3.
        let diff = compute_diff("a c", "a b c");
        assert_eq!(diff.stats.unchanged_words, 2, "a and c are unchanged: {:?}", diff.changes);
        assert_eq!(diff.stats.added_words, 1, "exactly b is inserted: {:?}", diff.changes);
        assert_eq!(diff.stats.changed_words, 0, "no word is replaced: {:?}", diff.changes);
        assert_eq!(diff.stats.removed_words, 0, "no word is deleted: {:?}", diff.changes);
        assert!((diff.stats.similarity - 200.0 / 3.0).abs() < 0.01, "got {}", diff.stats.similarity);
    }

    #[test]
    fn delete_next_to_common_word_is_a_delete_not_a_replace() {
        // The mirror case: "a b c" → "a c" is a pure deletion of b. The old code emitted
        // [Equal a, Replace(b→c), Delete c], again undercounting the unchanged words.
        let diff = compute_diff("a b c", "a c");
        assert_eq!(diff.stats.unchanged_words, 2, "a and c are unchanged: {:?}", diff.changes);
        assert_eq!(diff.stats.removed_words, 1, "exactly b is deleted: {:?}", diff.changes);
        assert_eq!(diff.stats.changed_words, 0, "no word is replaced: {:?}", diff.changes);
        assert_eq!(diff.stats.added_words, 0, "no word is inserted: {:?}", diff.changes);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn word() -> impl Strategy<Value = String> {
        prop::string::string_regex("[a-zA-Z]{0,8}").unwrap()
    }

    fn string_pair() -> impl Strategy<Value = (String, String)> {
        (prop::collection::vec(word(), 0..10), prop::collection::vec(word(), 0..10))
            .prop_map(|(raw_words, ann_words)| (raw_words.join(" "), ann_words.join(" ")))
    }

    proptest! {
        #[test]
        fn empty_vs_nonempty_produces_inserts(s in prop::string::string_regex("[a-zA-Z]{1,10}").unwrap()) {
            let diff = compute_diff("", &s);
            prop_assert!(diff.changes.iter().all(|c| c.op == DiffOp::Insert),
                "empty + non-empty should produce only Insert");
        }

        #[test]
        fn nonempty_vs_empty_produces_deletes(s in prop::string::string_regex("[a-zA-Z]{1,10}").unwrap()) {
            let diff = compute_diff(&s, "");
            prop_assert!(diff.changes.iter().all(|c| c.op == DiffOp::Delete),
                "non-empty + empty should produce only Delete");
        }

        #[test]
        fn operations_account_for_all_words((raw, ann) in string_pair()) {
            let raw_words: Vec<&str> = raw.split_whitespace().collect();
            let ann_words: Vec<&str> = ann.split_whitespace().collect();
            let diff = compute_diff(&raw, &ann);

            let eq = diff.changes.iter().filter(|c| c.op == DiffOp::Equal).count();
            let ins = diff.changes.iter().filter(|c| c.op == DiffOp::Insert).count();
            let del = diff.changes.iter().filter(|c| c.op == DiffOp::Delete).count();
            let rep = diff.changes.iter().filter(|c| c.op == DiffOp::Replace).count();

            prop_assert_eq!(eq + del + rep, raw_words.len(),
                "raw words not fully covered by operations");
            prop_assert_eq!(eq + ins + rep, ann_words.len(),
                "annotated words not fully covered by operations");
        }

        #[test]
        fn similarity_bounds((raw, ann) in string_pair()) {
            let diff = compute_diff(&raw, &ann);
            prop_assert!(diff.stats.similarity >= 0.0,
                "similarity {} < 0.0", diff.stats.similarity);
            prop_assert!(diff.stats.similarity <= 100.0,
                "similarity {} > 100.0", diff.stats.similarity);
        }

        #[test]
        fn identical_strings_have_full_similarity(s in prop::string::string_regex("[a-zA-Z ]{0,50}").unwrap()) {
            let diff = compute_diff(&s, &s);
            prop_assert!((diff.stats.similarity - 100.0).abs() < 1e-9,
                "identical strings should have 100% similarity, got {}", diff.stats.similarity);
        }

        #[test]
        fn completely_different_strings_have_zero_similarity(
            a in prop::string::string_regex("[a]{1,5}").unwrap(),
            b in prop::string::string_regex("[b]{1,5}").unwrap()
        ) {
            let diff = compute_diff(&a, &b);
            prop_assert!((diff.stats.similarity - 0.0).abs() < 1e-9,
                "completely different strings should have 0% similarity, got {}", diff.stats.similarity);
        }
    }
}
