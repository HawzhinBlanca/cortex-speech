use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn normalizer_is_idempotent(s in "\\PC*") {
        let normalizer = SoraniNormalizer::new();
        let once = normalizer.normalize(&s);
        let twice = normalizer.normalize(&once);
        assert_eq!(once, twice, "Normalizer must be idempotent");
    }

    #[test]
    fn normalizer_never_introduces_arabic_kaf(s in "\\PC*") {
        let normalizer = SoraniNormalizer::new();
        let result = normalizer.normalize(&s);
        assert!(!result.contains('\u{0643}'), "Result must not contain Arabic Kaf");
    }

    #[test]
    fn normalizer_never_introduces_arabic_yeh(s in "\\PC*") {
        let normalizer = SoraniNormalizer::new();
        let result = normalizer.normalize(&s);
        assert!(!result.contains('\u{064A}'), "Result must not contain Arabic Yeh");
    }

    #[test]
    fn normalizer_preserves_standard_kurdish(text in "[کگەڕۆپێچژڤەیحاتنw]+") {
        let normalizer = SoraniNormalizer::new();
        let result = normalizer.normalize(&text);
        assert_eq!(result, text, "Standard Kurdish text must be preserved unchanged");
    }

    #[test]
    fn normalizer_never_introduces_tatweel(s in "\\PC*") {
        let normalizer = SoraniNormalizer::new();
        let result = normalizer.normalize(&s);
        assert!(!result.contains('\u{0640}'), "Result must not contain Tatweel");
    }

    #[test]
    fn normalizer_collapses_multiple_spaces(s in "\\PC*") {
        let normalizer = SoraniNormalizer::new();
        let result = normalizer.normalize(&s);
        assert!(!result.contains("  "), "Result must not contain consecutive spaces");
    }
}
