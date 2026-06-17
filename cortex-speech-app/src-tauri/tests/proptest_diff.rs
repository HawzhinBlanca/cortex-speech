use cortex_speech_app_lib::diff;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn diff_of_identical_texts(s in "\\PC{0,100}") {
        let result = diff::compute_diff(&s, &s);
        assert!(result.changes.iter().all(|c| c.op == diff::DiffOp::Equal));
        assert_eq!(result.stats.similarity, 100.0);
    }

    #[test]
    fn diff_is_commutative_in_stats(a in "\\PC{0,50}", b in "\\PC{0,50}") {
        let r1 = diff::compute_diff(&a, &b);
        let r2 = diff::compute_diff(&b, &a);
        // Similarity should be symmetric
        assert!((r1.stats.similarity - r2.stats.similarity).abs() < 0.01);
    }

    #[test]
    fn diff_stats_are_consistent(a in "\\PC{0,50}", b in "\\PC{0,50}") {
        let result = diff::compute_diff(&a, &b);
        let total = result.stats.added_words
            + result.stats.removed_words
            + result.stats.changed_words
            + result.stats.unchanged_words;
        let a_words: Vec<&str> = a.split_whitespace().collect();
        let b_words: Vec<&str> = b.split_whitespace().collect();
        let max_words = a_words.len().max(b_words.len());
        assert!(total == 0 || total >= max_words / 2);
    }

    #[test]
    fn diff_preserves_content(a in "\\PC{0,50}") {
        let result = diff::compute_diff(&a, &a);
        assert_eq!(result.raw, a);
        assert_eq!(result.annotated, a);
    }
}
