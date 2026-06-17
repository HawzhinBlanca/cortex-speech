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
        // Both have content matching LCS → Equal
        if ri < raw_words.len()
            && ai < ann_words.len()
            && lcs_idx < lcs.len()
            && raw_words[ri] == lcs[lcs_idx]
            && ann_words[ai] == lcs[lcs_idx]
        {
            changes.push(DiffChange { op: DiffOp::Equal, value: lcs[lcs_idx].to_string() });
            ri += 1;
            ai += 1;
            lcs_idx += 1;
            continue;
        }

        // Both have content but neither matches LCS → Replace
        if ri < raw_words.len() && ai < ann_words.len() {
            changes.push(DiffChange { op: DiffOp::Replace, value: format!("{} → {}", raw_words[ri], ann_words[ai]) });
            ri += 1;
            ai += 1;
            continue;
        }

        // Only raw has content → Delete
        if ri < raw_words.len() {
            changes.push(DiffChange { op: DiffOp::Delete, value: raw_words[ri].to_string() });
            ri += 1;
            continue;
        }

        // Only annotated has content → Insert
        if ai < ann_words.len() {
            changes.push(DiffChange { op: DiffOp::Insert, value: ann_words[ai].to_string() });
            ai += 1;
            continue;
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
