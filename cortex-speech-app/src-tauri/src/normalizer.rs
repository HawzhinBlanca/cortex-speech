// Every `unwrap()` in this module is on compile-time-constant input — a static regex
// literal or a fixed Unicode code point — so it is infallible by construction; any
// failure would be a programming error caught immediately by this module's tests. The
// crate denies `clippy::unwrap_used` in production; these are the reviewed exceptions.
#![allow(clippy::unwrap_used)]

pub mod g2p;

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

/// Pure-Rust Sorani Kurdish normalizer implementing the character-mapping rules
/// from the MIT-licensed AsoSoft library. Bypasses copyleft restrictions of KLPT/KurdishHunspell.
static ARABIC_KAF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\u0643\u06AA\u06AC]").unwrap());
static ARABIC_YAH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\u064A\u06D2]").unwrap());
static YEH_TWO_DOTS_BELOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\u06D0").unwrap());
static ALEF_MAKSURA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\u0649").unwrap());
static TATWEEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\u0640").unwrap());
static ZWNJ: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\u200C").unwrap());
/// Zero-width format characters that carry NO word boundary (unlike ZWNJ U+200C, handled as a
/// space): ZWSP (U+200B), ZWJ (U+200D), and BOM/ZWNBSP (U+FEFF). Left in place they make two
/// visually identical strings normalize differently, breaking dedup/exact-match and inflating
/// WER/CER.
static ZERO_WIDTH_FORMAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\u200B\u200D\uFEFF]").unwrap());
static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static ARABIC_HAMZA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\u0623\u0625]").unwrap());
static ARABIC_DIACTIRICS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\u064B-\u065F\u0670]").unwrap());

const PERSIAN_TO_LATIN: &[char] = &['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
const ARABIC_TO_LATIN: &[char] = &['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationConfig {
    pub normalize_numbers: bool,
    pub verbalize_numbers: bool,
    pub normalize_hamza: bool,
    pub remove_diacritics: bool,
}

pub struct SoraniNormalizer {
    config: NormalizationConfig,
}

impl Default for SoraniNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl SoraniNormalizer {
    pub fn new() -> Self {
        Self {
            config: NormalizationConfig {
                normalize_numbers: true,
                verbalize_numbers: true,
                normalize_hamza: true,
                remove_diacritics: false,
            },
        }
    }

    pub fn with_config(config: NormalizationConfig) -> Self {
        Self { config }
    }

    pub fn normalize(&self, text: &str) -> String {
        let _span = crate::telemetry::TRACER.start_span(
            "normalizer.normalize",
            crate::telemetry::Tracer::metadata(vec![("text_len", text.len().to_string())]),
        );

        if text.is_empty() {
            return String::new();
        }

        // Step 0: Unicode canonical composition (NFC) up front, so every downstream rule
        // (and every consumer that reuses this output) sees one canonical form — composed
        // letters, no stray combining sequences. Mirrors the NFC applied in wer.rs.
        let mut result: String = text.nfc().collect();

        // Arabic Punctuation Unification
        result = result.replace(',', "،");
        result = result.replace('?', "؟");
        result = result.replace(';', "؛");

        // Step 1: Standardize Kaf variants
        result = ARABIC_KAF.replace_all(&result, "ک").to_string();

        // Step 2: Standardize Yeh variants (U+06D0 -> Kurdish E, then Arabic Yeh -> Kurdish Yeh)
        result = YEH_TWO_DOTS_BELOW.replace_all(&result, "ێ").to_string();
        result = ARABIC_YAH.replace_all(&result, "ی").to_string();
        result = ALEF_MAKSURA.replace_all(&result, "ی").to_string();

        // Step 2.5: Protect word-final heh with tatweel
        result = protect_word_final_heh_tatweel(&result);

        // Step 3: Remove Tatweel
        result = TATWEEL.replace_all(&result, "").to_string();

        // Step 4: Replace ZWNJ with space
        result = ZWNJ.replace_all(&result, " ").to_string();

        // Step 4b: Strip zero-width joiners/spaces (ZWJ, ZWSP, BOM) that carry no word boundary —
        // unlike ZWNJ they must be deleted, not turned into a space, so the surrounding letters stay
        // joined. Left in, they make two visually identical strings normalize differently.
        result = ZERO_WIDTH_FORMAT.replace_all(&result, "").to_string();

        // Step 4.5: Contextual Word-Final Heh Translation (ه U+0647, ة U+0629)
        result = normalize_heh_contextual(&result);

        // Step 5: Normalize Hamza forms (only أ/إ -> ا, preserve ء and آ)
        if self.config.normalize_hamza {
            result = ARABIC_HAMZA.replace_all(&result, "ا").to_string();
        }

        // Step 7: Remove Arabic diacritics (tashkeel)
        if self.config.remove_diacritics {
            result = ARABIC_DIACTIRICS.replace_all(&result, "").to_string();
        }

        // Step 8: Normalize digits
        if self.config.normalize_numbers {
            result = normalize_digits(&result, self.config.verbalize_numbers);
        }

        // Step 9: Collapse whitespace
        result = MULTI_SPACE.replace_all(&result, " ").to_string();

        // Step 10: Trim
        result.trim().to_string()
    }
}

/// Arabic combining marks (tashkeel/harakat U+064B–U+065F and superscript alef U+0670) that attach
/// to a preceding base letter. They report `is_alphabetic() == true`, so they must be skipped when
/// deciding whether a letter is word-final, otherwise a vocalized word-final heh (ه + harakat) is
/// mis-judged non-final.
fn is_arabic_combining_mark(c: char) -> bool {
    matches!(c, '\u{064B}'..='\u{065F}' | '\u{0670}')
}

/// The next base (non-combining-mark) character at or after `from`, if any.
fn next_base_char(chars: &[char], from: usize) -> Option<char> {
    chars[from..].iter().copied().find(|&c| !is_arabic_combining_mark(c))
}

fn normalize_heh_contextual(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    for i in 0..chars.len() {
        let ch = chars[i];
        if ch == '\u{0647}' {
            // Arabic Heh 'ه'. Skip any harakat that attach to it before testing word-finality.
            let is_final = match next_base_char(&chars, i + 1) {
                None => true,
                Some(next_ch) => !next_ch.is_alphabetic(),
            };
            if is_final {
                result.push('\u{06D5}'); // Kurdish Ae 'ە'
            } else {
                result.push('\u{06BE}'); // Kurdish Heh 'ھ'
            }
        } else if ch == '\u{0629}' {
            // Arabic Teh Marbuta 'ة'
            result.push('\u{06D5}'); // Kurdish Ae 'ە'
        } else {
            result.push(ch);
        }
    }
    result
}

fn protect_word_final_heh_tatweel(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{0647}' {
            // Arabic Heh 'ه'
            // Check if followed by one or more tatweels
            let mut j = i + 1;
            while j < chars.len() && chars[j] == '\u{0640}' {
                j += 1;
            }
            if j > i + 1 {
                // Yes, followed by tatweel(s).
                // Check if the character after the tatweel(s) is word-final, skipping any harakat.
                let is_final = match next_base_char(&chars, j) {
                    None => true,
                    Some(c) => !c.is_alphabetic(),
                };
                if is_final {
                    result.push('\u{06BE}'); // Map to Kurdish Heh 'ھ'
                    i = j; // skip the heh and the tatweels
                    continue;
                }
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

fn normalize_digits(text: &str, verbalize: bool) -> String {
    let mut result = text.to_string();
    // Persian digits -> Latin
    for (i, &latin) in PERSIAN_TO_LATIN.iter().enumerate() {
        let persian_char = char::from_u32(0x06F0 + i as u32).unwrap();
        result = result.replace(persian_char, &latin.to_string());
    }
    // Arabic digits -> Latin
    for (i, &latin) in ARABIC_TO_LATIN.iter().enumerate() {
        let arabic_char = char::from_u32(0x0660 + i as u32).unwrap();
        result = result.replace(arabic_char, &latin.to_string());
    }

    if !verbalize {
        return result;
    }

    // Match a WHOLE number — a digit run optionally carrying thousands separators (the ASCII comma
    // gets rewritten to the Arabic comma U+060C in Step 1) and/or a decimal part — rather than a
    // bare `\d+`. Matching `\d+` split "3,000" (by then "3،000") into "3" and "000", verbalizing it
    // as "three، zero" instead of "three thousand".
    static DIGITS_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\d{1,3}(?:[،,]\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?").unwrap()
    });

    let expanded = DIGITS_RE.replace_all(&result, |caps: &regex::Captures| verbalize_number_token(&caps[0]));

    expanded.into_owned()
}

/// Verbalize a matched numeric token that may carry thousands separators and a decimal part.
/// Integer part is read as a magnitude; the fractional part is read digit-by-digit after خاڵ
/// ("point"); a digit run with leading zeros (e.g. an ID like "007") is read digit-by-digit so the
/// sequence is preserved instead of collapsing to a single value.
fn verbalize_number_token(token: &str) -> String {
    let (int_part_raw, frac_part) = match token.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (token, None),
    };
    // Strip thousands separators, leaving only the digits of the integer part.
    let int_digits: String = int_part_raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if int_digits.is_empty() {
        return token.to_string();
    }

    let leading_zero = int_digits.len() > 1 && int_digits.starts_with('0');
    let mut out = if leading_zero {
        digits_individually(&int_digits)
    } else {
        match int_digits.parse::<u64>() {
            Ok(num) => num_to_kurdish(num),
            // Larger than u64 — read digit-by-digit rather than corrupt the value.
            Err(_) => digits_individually(&int_digits),
        }
    };

    if let Some(frac) = frac_part {
        let frac_digits: String = frac.chars().filter(|c| c.is_ascii_digit()).collect();
        if !frac_digits.is_empty() {
            out.push_str(" خاڵ ");
            out.push_str(&digits_individually(&frac_digits));
        }
    }
    out
}

/// Read a digit string one digit at a time (e.g. "14" -> "یەک چوار").
fn digits_individually(digits: &str) -> String {
    digits
        .chars()
        .filter_map(|c| c.to_digit(10))
        .map(|d| num_to_kurdish(d as u64))
        .collect::<Vec<_>>()
        .join(" ")
}

fn num_to_kurdish(mut n: u64) -> String {
    if n == 0 {
        return "سفر".to_string();
    }

    let units = ["", "یەک", "دوو", "سێ", "چوار", "پێنج", "شەش", "حەوت", "ھەشت", "نۆ"];
    let tens = ["", "", "بیست", "سی", "چل", "پەنجا", "شەست", "حەفتا", "ھەشتا", "نەوەد"];
    let hundreds = ["", "سەد", "دووسەد", "سێسەد", "چوارسەد", "پێنجسەد", "شەشسەد", "حەوتسەد", "ھەشتسەد", "نۆسەد"];

    let teens = |x: u64| match x {
        10 => "دە",
        11 => "یازدە",
        12 => "دوانزدە",
        13 => "سیانزدە",
        14 => "چواردە",
        15 => "پازدە",
        16 => "شازدە",
        17 => "حەڤدە",
        18 => "ھەژدە",
        19 => "نۆزدە",
        _ => "",
    };

    let mut parts = Vec::new();

    if n >= 1_000_000_000 {
        let b = n / 1_000_000_000;
        parts.push(format!("{} ملیار", num_to_kurdish(b)));
        n %= 1_000_000_000;
    }

    if n >= 1_000_000 {
        let m = n / 1_000_000;
        parts.push(format!("{} ملیۆن", num_to_kurdish(m)));
        n %= 1_000_000;
    }

    if n >= 1_000 {
        let t = n / 1_000;
        if t == 1 {
            parts.push("ھەزار".to_string());
        } else {
            parts.push(format!("{} ھەزار", num_to_kurdish(t)));
        }
        n %= 1_000;
    }

    if n >= 100 {
        let h = n / 100;
        parts.push(hundreds[h as usize].to_string());
        n %= 100;
    }

    if n >= 20 {
        let t = n / 10;
        let u = n % 10;
        parts.push(tens[t as usize].to_string());
        if u > 0 {
            parts.push(units[u as usize].to_string());
        }
    } else if n >= 10 {
        parts.push(teens(n).to_string());
    } else if n > 0 {
        parts.push(units[n as usize].to_string());
    }

    parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" و ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kurdish_normalization() {
        let n = SoraniNormalizer::new();
        let cases = vec![
            ("كوردی", "کوردی"),
            ("كوردي", "کوردی"),
            ("على", "علی"),
            ("ســـاڵ", "ساڵ"),
            ("ئەو\u{200C}کەسە", "ئەو کەسە"),
            ("ئەو ڪەسە لە ســـاڵەکانی ١٩٥٠دا دەژیا", "ئەو کەسە لە ساڵەکانی ھەزار و نۆسەد و پەنجادا دەژیا"),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(n.normalize(input), expected, "Failed for: {input}");
        }
    }

    #[test]
    fn test_yeh_two_dots_below_preserved() {
        let n = SoraniNormalizer::new();
        assert_eq!(n.normalize("پێ"), "پێ", "ێ (U+06D0) must be preserved as Kurdish open E");
        assert_eq!(n.normalize("دێ"), "دێ");
        assert_ne!(n.normalize("پێ"), "پی", "ێ should NOT become ی");
    }

    #[test]
    fn test_hamza_normalization() {
        let n = SoraniNormalizer::new();
        let result = n.normalize("أحمد إبراهیم");
        assert!(result.contains("احمد"), "أ -> ا");
        assert!(result.contains("ابرا\u{06BE}یم"), "إ -> ا");
        let result2 = n.normalize("ئەو کەسە");
        assert!(result2.contains("ە"), "Should preserve standard Kurdish Heh");
        assert!(result2.contains("ئ"), "Should preserve ء hamza");
    }

    #[test]
    fn test_diacritics_removal() {
        let n = SoraniNormalizer::with_config(NormalizationConfig {
            normalize_numbers: true,
            verbalize_numbers: true,
            normalize_hamza: true,
            remove_diacritics: true,
        });
        let input = "كُورْدِي";
        let result = n.normalize(input);
        assert!(!result.contains('\u{064C}'), "Fatha should be removed");
        assert!(!result.contains('\u{0652}'), "Sukun should be removed");
    }

    #[test]
    fn test_heh_normalization() {
        let n = SoraniNormalizer::new();
        // ههولێر (starting with non-final ه) -> ھھولێر
        // کۆمەڵه (ending with final ه) -> کۆمەڵە
        // ة -> ە
        assert_eq!(n.normalize("هەر یەک لە کۆمەڵه"), "ھەر یەک لە کۆمەڵە");
        assert_eq!(n.normalize("جامعة"), "جامعە");
        assert_eq!(n.normalize("گوناهـ"), "گوناھ");
        assert_eq!(n.normalize("گوناهــ"), "گوناھ");
        assert_eq!(n.normalize("گوناهـ دار"), "گوناھ دار");
    }

    #[test]
    fn test_nfc_canonical_composition() {
        let n = SoraniNormalizer::new();
        // Decomposed "é" (e + combining acute) must come out canonically composed (U+00E9).
        assert_eq!(n.normalize("e\u{0301}"), "\u{00E9}");
        // Output must already be in NFC: normalizing again is a no-op (composition is stable).
        let once = n.normalize("ئەو کەسە e\u{0301}");
        assert_eq!(once, once.nfc().collect::<String>(), "normalizer output must be NFC");
    }

    #[test]
    fn test_punctuation_unification() {
        let n = SoraniNormalizer::new();
        assert_eq!(n.normalize("ئایا باشی؟ یان نا، با بزانم؛"), "ئایا باشی؟ یان نا، با بزانم؛");
    }

    #[test]
    fn test_verbalize_numbers_toggle() {
        let config = NormalizationConfig {
            normalize_numbers: true,
            verbalize_numbers: false,
            normalize_hamza: true,
            remove_diacritics: false,
        };
        let n = SoraniNormalizer::with_config(config);
        assert_eq!(n.normalize("١٩٥٠"), "1950");
    }

    #[test]
    fn test_persian_digits() {
        let n = SoraniNormalizer::new();
        assert_eq!(n.normalize("۱۲۳"), "سەد و بیست و سێ");
    }

    #[test]
    fn test_arabic_digits() {
        let n = SoraniNormalizer::new();
        assert_eq!(n.normalize("١٢٣"), "سەد و بیست و سێ");
    }

    #[test]
    fn test_mixed_text() {
        let n = SoraniNormalizer::new();
        let input = "ئەم ژمارانە: ١٢٣ و ۴۵۶";
        let expected = "ئەم ژمارانە: سەد و بیست و سێ و چوارسەد و پەنجا و شەش";
        assert_eq!(n.normalize(input), expected);
    }

    #[test]
    fn grouped_and_decimal_numbers_verbalize_correctly() {
        let n = SoraniNormalizer::new();
        // Thousands-grouped integers must read as the full magnitude, not split per comma-group.
        assert_eq!(n.normalize("3,000"), "سێ ھەزار");
        assert_eq!(n.normalize("1,000,000"), "یەک ملیۆن");
        // Decimals read the fractional part digit-by-digit after خاڵ ("point").
        assert_eq!(n.normalize("3.14"), "سێ خاڵ یەک چوار");
        // Leading-zero runs are read digit-by-digit so an ID is preserved, not parsed to 7.
        assert_eq!(n.normalize("007"), "سفر سفر حەوت");
        // Plain numbers still verbalize as before.
        assert_eq!(n.normalize("123"), "سەد و بیست و سێ");
    }

    #[test]
    fn word_final_heh_with_harakat_maps_to_ae_not_medial_heh() {
        let n = SoraniNormalizer::new();
        // A word-final Arabic heh carrying a fatha must still become final ە (U+06D5); the harakat
        // must not fool the finality test into producing medial ھ (U+06BE).
        let out = n.normalize("ماڵه\u{064E}");
        assert!(out.contains('\u{06D5}'), "final heh+harakat must map to ە (U+06D5): {out:?}");
        assert!(!out.contains('\u{06BE}'), "must NOT become medial ھ (U+06BE): {out:?}");
    }

    #[test]
    fn zwj_and_zero_width_format_chars_removed() {
        let n = SoraniNormalizer::new();
        // ZWJ (U+200D), ZWSP (U+200B) and BOM (U+FEFF) must not survive into the canonical form,
        // and (unlike ZWNJ) must NOT introduce a space — the letters stay joined.
        let out = n.normalize("کورد\u{200D}ی\u{200B}\u{FEFF}");
        assert!(!out.contains('\u{200D}'), "ZWJ must be stripped");
        assert!(!out.contains('\u{200B}'), "ZWSP must be stripped");
        assert!(!out.contains('\u{FEFF}'), "BOM must be stripped");
        assert_eq!(out, "کوردی", "zero-width joiners must be deleted, not turned into spaces");
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn kurdish_char() -> impl Strategy<Value = char> {
        prop::sample::select(vec![
            // Kurdish-specific characters
            '\u{06CE}', // ێ
            '\u{06C6}', // ۆ
            '\u{06D5}', // ە
            '\u{0695}', // ڕ
            '\u{0698}', // ژ
            '\u{06A4}', // ڤ
            '\u{06AF}', // گ
            '\u{0686}', // چ
            '\u{067E}', // پ
            // Arabic letters common in Kurdish text
            '\u{0627}', '\u{0628}', '\u{062A}', '\u{062B}', '\u{062C}', '\u{062D}', '\u{062E}', '\u{062F}', '\u{0630}',
            '\u{0631}', '\u{0632}', '\u{0633}', '\u{0634}', '\u{0635}', '\u{0636}', '\u{0637}', '\u{0638}', '\u{0639}',
            '\u{063A}', '\u{0641}', '\u{0642}', '\u{0643}', '\u{0644}', '\u{0645}', '\u{0646}', '\u{0647}', '\u{0648}',
            '\u{064A}', '\u{06CC}', // Special characters
            '\u{0640}', // Tatweel
            '\u{200C}', // ZWNJ
            ' ',
        ])
    }

    fn kurdish_string() -> impl Strategy<Value = String> {
        prop::collection::vec(kurdish_char(), 0..50).prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        #[test]
        fn normalization_idempotent(s in kurdish_string()) {
            // Normalizing twice should give the same result as normalizing once.
            let n = SoraniNormalizer::new();
            let once = n.normalize(&s);
            let twice = n.normalize(&once);
            prop_assert_eq!(&once, &twice,
                "normalize(normalize(s)) must equal normalize(s) — idempotency violated");
        }

        #[test]
        fn normalization_preserves_kurdish_chars(s in kurdish_string()) {
            let n = SoraniNormalizer::new();
            let result = n.normalize(&s);
            for ch in s.chars() {
                match ch {
                    '\u{06CE}' | '\u{06C6}' | '\u{06D5}' | '\u{0695}' | '\u{0698}' |
                    '\u{06A4}' | '\u{06AF}' | '\u{0686}' | '\u{067E}' => {
                        assert!(result.contains(ch),
                            "Kurdish char U+{:04X} should be preserved in normalized output", ch as u32);
                    }
                    _ => {}
                }
            }
        }

        #[test]
        fn zwnj_collapsed(s in kurdish_string()) {
            let n = SoraniNormalizer::new();
            let result = n.normalize(&s);
            prop_assert!(!result.contains('\u{200C}'),
                "ZWNJ should not appear in normalized output");
            prop_assert!(!result.contains("  "),
                "Multiple consecutive spaces should be collapsed");
        }
    }
}
