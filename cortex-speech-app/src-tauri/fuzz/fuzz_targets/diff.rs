#![no_main]

use libfuzzer_sys::fuzz_target;
use cortex_speech_app_lib::diff;

fuzz_target!(|data: &[u8]| {
    // Split input bytes roughly in half to produce two strings
    let mid = data.len() / 2;
    let left = &data[..mid];
    let right = &data[mid..];

    let a = String::from_utf8_lossy(left);
    let b = String::from_utf8_lossy(right);

    // Compute diff — should never panic on any input
    let result = diff::compute_diff(&a, &b);

    // Basic invariants:
    // - Similarity is always [0.0, 100.0]
    assert!(
        result.stats.similarity >= 0.0 && result.stats.similarity <= 100.0,
        "Similarity must be in [0, 100], got {}",
        result.stats.similarity
    );

    // - Raw and annotated are preserved
    assert_eq!(result.raw, a, "Raw text must be preserved");
    assert_eq!(result.annotated, b, "Annotated text must be preserved");

    // - Total operations cover all words
    if a.split_whitespace().count() > 10_000 || b.split_whitespace().count() > 10_000 {
        // Large inputs return empty changes with 100% similarity
        assert!(result.changes.is_empty());
        assert_eq!(result.stats.similarity, 100.0);
    }
});
