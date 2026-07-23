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
/// Zero-width / directional FORMAT characters that carry NO content and NO word boundary (unlike
/// ZWNJ U+200C, handled as a space). Left in place they make two visually identical strings normalize
/// differently, breaking dedup/exact-match and inflating WER/CER, and \u2014 critically for a
/// right-to-left script \u2014 the bidi controls can also silently reorder how a stored/exported string
/// renders. Deleted, not spaced:
///   ZWSP U+200B, ZWJ U+200D, BOM/ZWNBSP U+FEFF,
///   ALM U+061C (Arabic Letter Mark),
///   WORD JOINER U+2060 (the modern replacement for U+FEFF-as-word-joiner, common in text pasted
///   from Word/PDF/web) and the invisible math operators U+2061-U+2064 (Cf, zero-width, no boundary \u2014
///   neither ZWNJ nor White_Space, so nothing else strips them; left in, they break dedup and inflate CER),
///   the explicit bidi formatting marks LRE/RLE/PDF/LRO/RLO U+202A-U+202E,
///   and the bidi isolates LRI/RLI/FSI/PDI U+2066-U+2069.
static ZERO_WIDTH_FORMAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\u061C\u200B\u200D\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]").unwrap());
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

/// Canonical orthography for SHIPPED TRAINING TEXT (the HF `transcription` column and the
/// fine-tune pack `sentence`): char-only unification so ك/ک, ي/ی, ه/ھ variants collapse to one
/// form. Human-typed corrections and ASR output otherwise mix codepoint variants in one dataset,
/// inflating the CTC label space of the retrain corpus (and Arabic-only forms may not even exist
/// in the model's ckb vocab). Numbers and diacritics are left exactly as written — no one-way
/// verbalization ever enters shipped text.
pub fn canonical_training_text(text: &str) -> String {
    static CHAR_ONLY: LazyLock<SoraniNormalizer> = LazyLock::new(SoraniNormalizer::char_only);
    CHAR_ONLY.normalize(text)
}

/// A LOOSE match key for "is this correction genuine / is this a no-op edit": lowercase + whitespace-
/// collapse only (NOT Sorani-canonicalized — the db.rs no-op-edit dedup guard compares WITHOUT NFC).
///
/// SINGLE source of truth: the corrections-ledger + agent_examples gate (`db.rs`) and the DPO preference
/// gate (`jury/learning.rs`) both decide "wrong != fix" through THIS key. They previously each had an
/// identical private copy; if one grew NFC/canonicalization and the other did not, a correction could be
/// recorded as a genuine pair in one training channel and dropped as a no-op in the other — silently
/// inconsistent training data. One definition makes that divergence impossible.
pub fn learning_text_key(text: &str) -> String {
    text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
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

    /// Char-only canonical configuration: folds the Sorani codepoint variants (Arabic Kaf ك→ک,
    /// Arabic Yeh ي→ی, Heh forms, ZWNJ/tatweel, hamza) but does NOT touch numbers (no one-way
    /// verbalization) or diacritics. This is the orthography for keys and SHIPPED TRAINING TEXT.
    pub fn char_only() -> Self {
        Self::with_config(NormalizationConfig {
            normalize_numbers: false,
            verbalize_numbers: false,
            normalize_hamza: true,
            remove_diacritics: false,
        })
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

        // Step 3.5: Fold Arabic heh + ZWNJ to the Sorani ە vowel BEFORE the blanket ZWNJ→space below.
        // A ZWNJ right after heh (U+0647) is the on-keyboard encoding of the ە (U+06D5) vowel when a
        // letter follows: the ZWNJ forces heh into its non-joining final form so it reads as ە rather
        // than joining the next letter (e.g. کۆمه‌ڵ = the single word کۆمەڵ). Without this, Step 4 turns
        // that in-word ZWNJ into a space and Step 4.5 then sees the heh as word-final, splitting one word
        // into two tokens with a stray lone consonant ("کۆمە ڵ") — corrupting the exported training text
        // and inflating CER/WER one-sidedly against an ASR hypothesis that emits ە directly. A ZWNJ NOT
        // preceded by heh (a genuine word/morpheme separator, e.g. ئەو‌کەسە) is left for Step 4.
        result = result.replace("\u{0647}\u{200C}", "\u{06D5}");

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

        // Step 10: FINAL NFC. Step 0 canonically orders combining marks, but steps that DELETE a
        // ccc-0 character SITTING BETWEEN two runs of combining marks — tatweel removal (Step 3), the
        // ZWNJ→space / zero-width strips (Steps 4/4b) — then merge those runs into a single run that is
        // no longer canonically ordered (e.g. `ّ ـ َ` → tatweel removed → `ّ َ`, shadda-before-fatha).
        // Without re-NFC the output is non-NFC and NON-IDEMPOTENT: re-normalizing reorders the marks,
        // so the same text yields two byte strings and defeats dedup / FTS / WER-CER equality.
        let result: String = result.nfc().collect();

        // Step 11: Trim
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
    // Native Sorani/Persian-script number separators: ARABIC DECIMAL SEPARATOR U+066B (٫) and ARABIC
    // THOUSANDS SEPARATOR U+066C (٬). The digit-glyph fold above stops at U+0669, so these survive and
    // would split a number mid-stream (e.g. "٣٫١٤" -> "3٫14" parses as two unrelated numbers with a stray
    // separator token). Fold the decimal to '.', and the thousands separator to the Arabic comma U+060C
    // the pipeline already standardizes ASCII ',' to (Step 1) — so the output stays comma-consistent AND
    // the regexes below (which match both '،' and ',') still strip grouped numbers. Without this the
    // metric path scores "1٬000" as wrong vs "1000" (CER inflation) and the verbalizer reads "٣٫١٤" as
    // "three ٫ fourteen". (A single fold: the previous second, contradictory pass — U+066C -> ',' then
    // -> '،' — left the first as dead code and emitted an ASCII comma that escaped the Step-1 unification.)
    result = result.replace('\u{066B}', ".").replace('\u{066C}', "،");

    // Strip thousands separators inside grouped numbers ("1،000" / "1,000" -> "1000") so a formatted
    // number compares equal to its bare form. This runs BEFORE the verbalize early-return below, so it
    // also fixes the metric path (verbalize=false): without it the separator — an Arabic comma by this
    // stage, since step 84 of normalize() rewrites ASCII ',' to '،' — survives into the WER/CER string
    // and counts a correct "1,000" as wrong against a reference "1000". The 3-digit grouping requirement
    // means ordinary prose commas are left untouched.
    static THOUSANDS_SEP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{1,3}(?:[،,]\d{3})+").unwrap());
    let result = THOUSANDS_SEP_RE
        .replace_all(&result, |caps: &regex::Captures| {
            caps[0].chars().filter(|c| *c != '،' && *c != ',').collect::<String>()
        })
        .into_owned();

    if !verbalize {
        return result;
    }

    // Match a WHOLE number — a digit run optionally carrying thousands separators (the ASCII comma
    // gets rewritten to the Arabic comma U+060C in Step 1) and/or a decimal part — rather than a
    // bare `\d+`. Matching `\d+` split "3,000" (by then "3،000") into "3" and "000", verbalizing it
    // as "three، zero" instead of "three thousand".
    static DIGITS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\d{1,3}(?:[،,]\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?").unwrap());

    let expanded = DIGITS_RE.replace_all(&result, |caps: &regex::Captures| {
        // Surround the multi-word expansion with spaces so a numeral glued to a word ("ساڵی٢٠٢٠") or a
        // leftover digit after a thousands-group match cannot FUSE into a single non-word token. The
        // subsequent MULTI_SPACE collapse + trim in `normalize` absorb the redundant spaces.
        format!(" {} ", verbalize_number_token(&caps[0]))
    });

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
    digits.chars().filter_map(|c| c.to_digit(10)).map(|d| num_to_kurdish(d as u64)).collect::<Vec<_>>().join(" ")
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
            // The numeral ١٩٥٠ glued to the suffix دا must keep a word boundary after verbalization
            // (round-12 fix): پەنجا (50) is no longer fused into پەنجادا, so FTS/dedup tokenizes it.
            ("ئەو ڪەسە لە ســـاڵەکانی ١٩٥٠دا دەژیا", "ئەو کەسە لە ساڵەکانی ھەزار و نۆسەد و پەنجا دا دەژیا"),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(n.normalize(input), expected, "Failed for: {input}");
        }
    }

    #[test]
    fn heh_zwnj_folds_to_the_ae_vowel_not_a_word_split() {
        let n = SoraniNormalizer::new();
        // کۆمه‌ڵ ("group/society") typed with Arabic heh (U+0647) + ZWNJ (U+200C) — the on-keyboard way to
        // write the ە vowel when a letter follows (the ZWNJ forces heh's non-joining final form). It must
        // fold to the SINGLE word کۆمەڵ, NOT split into two tokens with a stray lone ڵ ("کۆمە ڵ").
        assert_eq!(
            n.normalize("کۆمه\u{200C}ڵ"),
            "کۆمەڵ",
            "heh+ZWNJ must fold to the ە vowel as one word, not split with an orphaned consonant"
        );
        // Regression guard: a ZWNJ that is a genuine word/morpheme separator (after waw ۆ, not heh) still
        // becomes a space — the case test_kurdish_normalization already pins (ئەو‌کەسە -> ئەو کەسە).
        assert_eq!(n.normalize("ئەو\u{200C}کەسە"), "ئەو کەسە", "a non-heh ZWNJ separator still becomes a space");
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
    fn canonical_training_text_unifies_variants_but_preserves_digits() {
        // Shipped training text: Arabic Kaf/Yeh fold to the Kurdish forms, while digits stay digits
        // (no one-way verbalization may ever enter a shipped sentence).
        let out = canonical_training_text("كوردي ١٤");
        assert!(out.contains('ک') && out.contains('ی'), "Kaf/Yeh unified to Kurdish forms: {out}");
        assert!(!out.contains('ك') && !out.contains('ي'), "no Arabic variants remain: {out}");
        assert!(out.contains("١٤") || out.contains("14"), "digits preserved, never verbalized: {out}");
        assert!(!out.contains("چوارد"), "no number verbalization in shipped text: {out}");
    }

    #[test]
    fn removing_a_separator_between_combining_marks_keeps_output_canonical_and_idempotent() {
        // Regression for the non-idempotence the hostile-alphabet proptest surfaced: a ccc-0 char
        // (tatweel here; ZWNJ/zero-width behave the same) BETWEEN two combining marks keeps NFC from
        // ordering them together at Step 0; deleting that separator later merged them into a
        // non-canonical run (shadda U+0651 ccc33 before fatha U+064E ccc30), so re-normalizing
        // reordered it. The final NFC pass fixes it.
        let n = SoraniNormalizer::new();
        // ماڵ + shadda + TATWEEL + fatha: after tatweel removal the marks are ّ then َ (non-canonical).
        let input = "ماڵ\u{0651}\u{0640}\u{064E}";
        let once = n.normalize(input);
        let twice = n.normalize(&once);
        assert_eq!(once, twice, "normalize must be idempotent after a between-marks separator is removed");
        assert_eq!(once, once.nfc().collect::<String>(), "output must be NFC (marks in canonical ccc order)");
        // The tatweel is gone and the two marks are canonically ordered: fatha (ccc30) before shadda (ccc33).
        assert!(!once.contains('\u{0640}'), "tatweel removed");
        assert!(once.contains('\u{064E}') && once.contains('\u{0651}'), "both marks preserved");
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
    fn native_arabic_number_separators_fold_like_ascii() {
        // Round-22 #8: ARABIC DECIMAL SEPARATOR U+066B (٫) and ARABIC THOUSANDS SEPARATOR U+066C (٬) —
        // the separators actually used inside native Sorani/Persian-script numbers — used to survive
        // un-folded and split a number mid-stream. They must now normalize identically to the ASCII
        // '.'/',' forms, with no stray separator token leaking into the verbalized output.
        let n = SoraniNormalizer::new();
        let dec = "\u{0663}\u{066B}\u{0661}\u{0664}"; // ٣٫١٤  (Arabic digits + U+066B decimal sep)
        let grp = "\u{0663}\u{066C}\u{0660}\u{0660}\u{0660}"; // ٣٬٠٠٠ (Arabic digits + U+066C thousands sep)
        assert_eq!(n.normalize(dec), n.normalize("3.14"), "native decimal separator must fold like ASCII '.'");
        assert_eq!(n.normalize(grp), n.normalize("3,000"), "native thousands separator must fold like ASCII ','");
        assert!(!n.normalize(dec).contains('\u{066B}'), "no stray U+066B leaks");
        assert!(!n.normalize(grp).contains('\u{066C}'), "no stray U+066C leaks");
    }

    #[test]
    fn verbalize_numbers_keeps_word_boundary_around_glued_digits() {
        // Round-12 audit HIGH: a numeral glued to a word ("ساڵی٢٠٢٠", a common Sorani year pattern) used
        // to FUSE the word with the first verbalized number-word ("ساڵیدوو …"), producing a non-word
        // token that corrupts the FTS-indexed normalized_transcript. The expansion must be space-
        // delimited from neighbours, with no leaked leading/trailing/double spaces.
        let n = SoraniNormalizer::new();
        let out = n.normalize("ساڵی٢٠٢٠");
        assert!(out.starts_with("ساڵی "), "word must stay separated from the number: {out:?}");
        assert!(!out.contains("ساڵیدوو"), "word must not fuse with the first number-word: {out:?}");
        assert_eq!(out, out.trim(), "no leading/trailing space leak: {out:?}");
        assert!(!out.contains("  "), "no double spaces leak: {out:?}");

        // A leftover digit after a thousands-group match must also not fuse with the group's last word.
        let grouped = n.normalize("100,0001");
        assert!(!grouped.contains("  "), "no double spaces: {grouped:?}");
        assert_eq!(grouped, grouped.trim());
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

    #[test]
    fn bidi_control_chars_are_stripped() {
        let n = SoraniNormalizer::new();
        // Directional formatting marks / isolates / ALM carry no content but (a) make two visually
        // identical strings dedup differently and (b) can silently reorder RTL rendering. They must be
        // deleted from the canonical form. Wrap the Kurdish word in RLE...PDF and an LRI...PDI isolate
        // plus a leading ALM; the canonical form must be just the letters.
        let out = n.normalize("\u{061C}\u{202B}کورد\u{202C}\u{2066}ی\u{2069}");
        for cp in [
            '\u{061C}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
            '\u{2069}',
        ] {
            assert!(!out.contains(cp), "bidi control U+{:04X} must be stripped, got {out:?}", cp as u32);
        }
        assert_eq!(out, "کوردی", "bidi controls deleted without introducing spaces");
    }

    #[test]
    fn word_joiner_and_invisible_operators_are_stripped() {
        // U+2060 WORD JOINER (the modern replacement for U+FEFF-as-word-joiner, common in text pasted from
        // Word/PDF/web) and the invisible math operators U+2061-U+2064 are Cf, zero-width, and carry NO word
        // boundary — but they are neither ZWNJ (U+200C) nor White_Space, so nothing else removes them. Left
        // in, a transcript with an invisible U+2060 normalizes to a DIFFERENT byte string than the same
        // visible text without it, breaking dedup/exact-match and inflating WER/CER on the very labels that
        // feed retraining. They must be deleted (not spaced — the letters stay joined).
        let n = SoraniNormalizer::new();
        let out = n.normalize("کورد\u{2060}ی\u{2061}\u{2062}\u{2063}\u{2064}");
        for cp in ['\u{2060}', '\u{2061}', '\u{2062}', '\u{2063}', '\u{2064}'] {
            assert!(!out.contains(cp), "invisible format U+{:04X} must be stripped, got {out:?}", cp as u32);
        }
        assert_eq!(out, "کوردی", "a word joiner must be deleted, not turned into a space");
        // Dedup/idempotence: the joiner-bearing string must normalize identically to the clean one.
        assert_eq!(out, n.normalize("کوردی"), "an invisible word joiner must not change the canonical form");
    }

    #[test]
    fn metric_normalizer_collapses_thousands_separator_to_match_bare_number() {
        // The WER/CER path runs with verbalize_numbers=false (keep digits as digits). A correctly
        // transcribed "1,000" must normalize to the SAME string as a reference "1000" — otherwise the
        // surviving separator counts a right answer as wrong and inflates the error rate.
        let metric = SoraniNormalizer::with_config(NormalizationConfig {
            normalize_numbers: true,
            verbalize_numbers: false,
            normalize_hamza: true,
            remove_diacritics: false,
        });
        assert_eq!(metric.normalize("1,000"), metric.normalize("1000"));
        assert_eq!(metric.normalize("1,000,000"), metric.normalize("1000000"));
        // Already-Arabic-comma grouping (or mixed digits) collapses too.
        assert_eq!(metric.normalize("١٢٣,٤٥٦"), metric.normalize("123456"));
        // A genuine prose comma (not a 3-digit grouping) must survive as the Arabic comma, untouched.
        let prose = metric.normalize("سڵاو, دونیا");
        assert!(prose.contains('،'), "prose comma must remain (as ، ): {prose:?}");
    }

    #[test]
    fn learning_text_key_is_case_and_whitespace_insensitive_only() {
        // The shared no-op/genuine-correction key: lowercase + whitespace-collapse, NOTHING else (no NFC,
        // no Sorani folding) — so the db.rs ledger gate and the jury DPO gate agree byte-for-byte.
        assert_eq!(learning_text_key("  Hello   World  "), "hello world");
        assert_eq!(learning_text_key("hello world"), learning_text_key("HELLO\tWORLD"));
        // It does NOT fold orthographic variants (unlike canonical_training_text) — Arabic vs Kurdish Kaf
        // stay DISTINCT keys, so a real orthographic correction is not mistaken for a no-op edit.
        assert_ne!(learning_text_key("كوردی"), learning_text_key("کوردی"));
    }

    #[test]
    fn ungrouped_native_thousands_separator_folds_to_arabic_comma_not_stray_ascii() {
        // A U+066C that is NOT a valid 3-digit grouping (so the thousands regex leaves it in place) must
        // still fold to the pipeline's Arabic comma U+060C — the same form Step 1 rewrites ASCII ',' to —
        // never a stray ASCII ',' that escapes the earlier punctuation unification. Regression guard for
        // the old contradictory two-pass fold (U+066C -> ',' then a dead -> '،') that emitted ASCII.
        let metric = SoraniNormalizer::with_config(NormalizationConfig {
            normalize_numbers: true,
            verbalize_numbers: false,
            normalize_hamza: true,
            remove_diacritics: false,
        });
        let out = metric.normalize("1\u{066C}00"); // only 2 trailing digits -> not a grouping -> kept
        assert!(out.contains('\u{060C}'), "ungrouped U+066C must fold to Arabic comma U+060C: {out:?}");
        assert!(!out.contains(','), "must not leave a stray ASCII comma inconsistent with the pipeline: {out:?}");
    }

    #[test]
    fn verbalize_path_still_reads_grouped_number_as_one_magnitude() {
        // Stripping the separator before the verbalize early-return must not change verbalization:
        // "1,000" and "1000" must verbalize identically (one thousand), not digit-by-digit.
        let n = SoraniNormalizer::new(); // verbalize_numbers = true
        assert_eq!(n.normalize("1,000"), n.normalize("1000"));
    }

    #[test]
    fn native_arabic_number_separators_are_handled() {
        // U+066C (Arabic THOUSANDS separator) and U+066B (Arabic DECIMAL separator) survive the digit
        // fold (they sit just past U+0660-0669) and were previously left in the string — inflating CER
        // and breaking verbalization. Both must now behave like their ASCII counterparts.
        let metric = SoraniNormalizer::with_config(NormalizationConfig {
            normalize_numbers: true,
            verbalize_numbers: false,
            normalize_hamza: true,
            remove_diacritics: false,
        });
        // Metric path: "1٬000" collapses to the bare number; "3٫14" becomes "3.14".
        assert_eq!(metric.normalize("1\u{066C}000"), metric.normalize("1000"));
        assert_eq!(metric.normalize("3\u{066B}14"), metric.normalize("3.14"));

        // Verbalize path: grouped U+066C reads as one magnitude; U+066B decimal reads digit-by-digit.
        let n = SoraniNormalizer::new();
        assert_eq!(n.normalize("1\u{066C}000"), n.normalize("1000"), "U+066C groups one magnitude");
        assert_eq!(n.normalize("3\u{066B}14"), "سێ خاڵ یەک چوار", "U+066B decimal -> three point one four");
    }

    #[test]
    fn python_rust_normalization_equivalence() {
        // M0.1: Verify Python sorani_normalize.py produces byte-identical output to normalizer.rs
        // on a fixture of Sorani edge cases. This test runs if the Python script exists; it skips
        // silently if not (e.g., in CI environments without Python installed).
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let fixture_path = std::path::PathBuf::from(&manifest_dir)
            .parent()
            .map(|p| p.join("scripts/sorani_normalize_fixture.txt"))
            .unwrap_or_else(|| std::path::PathBuf::from("scripts/sorani_normalize_fixture.txt"));

        if !fixture_path.exists() {
            eprintln!("Skipping Python/Rust equivalence test: fixture not found at {}", fixture_path.display());
            return;
        }

        use std::process::Command;
        let rust_norm = SoraniNormalizer::new();

        // Each line is a tab-separated (input, expected_output) pair.
        if let Ok(fixture_content) = std::fs::read_to_string(fixture_path) {
            for (i, line) in fixture_content.lines().enumerate() {
                if line.trim().is_empty() || !line.contains('\t') {
                    continue;
                }
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() < 2 {
                    continue;
                }
                let input = parts[0];
                let expected = parts[1];

                let rust_result = rust_norm.normalize(input);
                assert_eq!(rust_result, expected, "Rust normalizer line {}: input {}", i + 1, input);

                // Optional: run Python script if available (for environments with Python).
                // If Python is not available, the test still passes as long as Rust matches the fixture.
                if let Ok(py_output) = Command::new("python3").arg("scripts/sorani_normalize.py").arg(input).output() {
                    if py_output.status.success() {
                        let py_result = String::from_utf8_lossy(&py_output.stdout).trim().to_string();
                        assert_eq!(
                            py_result,
                            rust_result,
                            "Python/Rust mismatch on line {}: input '{}': Python={}, Rust={}",
                            i + 1,
                            input,
                            py_result,
                            rust_result
                        );
                    }
                }
            }
        }
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
            // ── Hostile-alphabet extension (iter-69 audit): every recent normalizer bug class
            // (separator folding, glued digits, harakat-heh finality, zero-width strips) lived
            // OUTSIDE the alphabet above, so the idempotence property was never exercised on the
            // inputs where it matters. These generate those interactions:
            '\u{0661}', '\u{0669}', // Arabic-Indic digits ١ ٩
            '\u{06F2}', '\u{06F0}', // Extended (Persian) digits ۲ ۰
            '1', '0', '7', // ASCII digits (incl. 0 for leading-zero/ID paths)
            '\u{066B}', '\u{066C}', // Arabic decimal ٫ / thousands ٬ separators
            ',', '.', '?', ';',        // ASCII punctuation the pipeline unifies
            '\u{0629}', // ة teh marbuta (heh-contextual path)
            '\u{0623}', '\u{0625}', '\u{0621}', '\u{0622}', // hamza forms أ إ ء آ
            '\u{064E}', '\u{0651}', // harakat: fatha, shadda (combining, alphabetic)
            '\u{200B}', '\u{200D}', '\u{FEFF}', // ZWSP / ZWJ / BOM (deleted, not spaced)
            '\u{202B}', '\u{2066}', // RLE bidi embed + LRI isolate (deleted)
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
            // The output must already be NFC over the whole hostile alphabet (harakat reordering,
            // composed hamza forms) — pinned on one concrete case in test_nfc_canonical_composition,
            // enforced generatively here.
            prop_assert_eq!(once.clone(), once.nfc().collect::<String>(),
                "normalizer output must be NFC-stable");
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
