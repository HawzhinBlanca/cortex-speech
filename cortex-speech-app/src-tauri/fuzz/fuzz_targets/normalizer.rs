#![no_main]

use libfuzzer_sys::fuzz_target;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let normalizer = SoraniNormalizer::new();

        // Normalize the input — should never panic on any valid UTF-8 string
        let result = normalizer.normalize(s);

        // Basic invariants:
        // - Result should not contain Arabic Kaf
        assert!(!result.contains('\u{0643}'), "Arabic Kaf should be normalized");
        // - Result should not contain Arabic Yeh
        assert!(!result.contains('\u{064A}'), "Arabic Yeh should be normalized");
        // - Result should not contain Tatweel
        assert!(!result.contains('\u{0640}'), "Tatweel should be removed");
        // - Result should not contain consecutive spaces
        assert!(!result.contains("  "), "Consecutive spaces should be collapsed");
        // - Result should never be longer than input (due to character reduction)
        //   Exception: worst-case is ZWNJ -> space (still same length or shorter)
        // - Normalization should be idempotent
        let twice = normalizer.normalize(&result);
        assert_eq!(result, twice, "Normalizer must be idempotent");
    }
});
