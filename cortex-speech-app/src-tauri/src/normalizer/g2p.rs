#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SoundType {
    Consonant,
    Vowel,
    Undetermined,
}

fn is_known_consonant(c: char) -> bool {
    matches!(
        c,
        'ب' | 'پ'
            | 'ت'
            | 'ج'
            | 'چ'
            | 'ح'
            | 'خ'
            | 'د'
            | 'ر'
            | 'ڕ'
            | 'ز'
            | 'ژ'
            | 'س'
            | 'ش'
            | 'ع'
            | 'غ'
            | 'ف'
            | 'ڤ'
            | 'ق'
            | 'ک'
            | 'گ'
            | 'ل'
            | 'ڵ'
            | 'م'
            | 'ن'
            | 'ھ'
            | 'ه'
            | 'ئ'
            // Hamza carriers that survive normalization (only أ/إ fold to ا): glottal-stop consonants.
            // Without a phoneme they drop, collapsing distinct words like سر vs سؤر to identical phonemes.
            | 'ء'
            | 'ؤ'
    )
}

fn is_known_vowel(c: char) -> bool {
    // 'آ' (U+0622, alef-madda) is a long-a vowel that survives normalization (only أ/إ fold to ا), so
    // it must carry a phoneme here or distinct words like بر vs بآر g2p to the SAME phonemes and a
    // correction memory keyed on one silently rewrites the other.
    matches!(c, 'ا' | 'ە' | 'ۆ' | 'ێ' | 'آ')
}

/// A character that actually carries a phoneme: a known consonant, a known vowel, or one of the
/// role-ambiguous letters 'و'/'ی'. Everything else (combining marks/harakat, ZWNJ/ZWJ and other
/// format chars, control chars, digits, punctuation, non-Kurdish letters) is phonetically
/// transparent — it produces no phoneme, so it must be dropped BEFORE role resolution. Leaving it
/// in let a stray mark occupy a role slot and flip the classification of an adjacent 'و'/'ی' (e.g.
/// splitting the long-vowel digraph 'وو' into two consonants).
fn is_phonetic(c: char) -> bool {
    is_known_consonant(c) || is_known_vowel(c) || c == 'و' || c == 'ی'
}

/// Converts a normalized Sorani Kurdish word to a phonetic (phoneme) string.
fn word_to_g2p(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }

    let chars: Vec<char> = word.chars().filter(|&c| is_phonetic(c)).collect();
    let n = chars.len();
    let mut roles = vec![SoundType::Undetermined; n];

    // Pass 1: Assign known roles
    for i in 0..n {
        let c = chars[i];
        if is_known_consonant(c) {
            roles[i] = SoundType::Consonant;
        } else if is_known_vowel(c) {
            roles[i] = SoundType::Vowel;
        }
    }

    // Pass 2: Detect double 'وو' (represented as vowel 'U')
    let mut i = 0;
    while i < n {
        if chars[i] == 'و' && i + 1 < n && chars[i + 1] == 'و' {
            roles[i] = SoundType::Vowel;
            roles[i + 1] = SoundType::Vowel;
            i += 2;
        } else {
            i += 1;
        }
    }

    // Pass 3: Resolve undetermined 'و' and 'ی'
    for i in 0..n {
        if roles[i] != SoundType::Undetermined {
            continue;
        }

        // Beginning of word must be Consonant
        if i == 0 {
            roles[i] = SoundType::Consonant;
            continue;
        }

        // End of word
        if i == n - 1 {
            if roles[i - 1] == SoundType::Vowel {
                roles[i] = SoundType::Consonant;
            } else {
                roles[i] = SoundType::Vowel;
            }
            continue;
        }

        // Middle of word
        // Preceded by vowel -> must be consonant
        if roles[i - 1] == SoundType::Vowel {
            roles[i] = SoundType::Consonant;
            continue;
        }

        // Bounded by consonants -> must be vowel
        let next_is_consonant = match roles[i + 1] {
            SoundType::Consonant => true,
            SoundType::Undetermined => {
                // If followed by another undetermined, let's check further
                if i + 2 < n {
                    roles[i + 2] == SoundType::Vowel
                } else {
                    true
                }
            }
            SoundType::Vowel => false,
        };

        if roles[i - 1] == SoundType::Consonant && next_is_consonant {
            roles[i] = SoundType::Vowel;
        } else {
            roles[i] = SoundType::Consonant;
        }
    }

    // Pass 4: Build phonetic tokens
    let mut phonemes = Vec::new();
    let mut skip_next = false;

    for i in 0..n {
        if skip_next {
            skip_next = false;
            continue;
        }

        let c = chars[i];
        let role = roles[i];

        // Handle double 'وو'
        if c == 'و' && i + 1 < n && chars[i + 1] == 'و' && role == SoundType::Vowel {
            phonemes.push("U"); // Long vowel /uː/
            skip_next = true;
            continue;
        }

        let phone = match c {
            'ب' => "b",
            'پ' => "p",
            'ت' => "t",
            'ج' => "j",
            'چ' => "c",
            'ح' => "H",
            'خ' => "x",
            'د' => "d",
            'ر' => "r",
            'ڕ' => "R",
            'ز' => "z",
            'ژ' => "Z",
            'س' => "s",
            'ش' => "S",
            'ع' => "E",
            'غ' => "G",
            'ف' => "f",
            'ڤ' => "v",
            'ق' => "q",
            'ک' => "k",
            'گ' => "g",
            'ل' => "l",
            'ڵ' => "L",
            'م' => "m",
            'ن' => "n",
            'ھ' | 'ه' => "h",
            'ئ' | 'ء' | 'ؤ' => "Y", // hamza glottal stop (and hamza-on-waw)
            'ا' | 'آ' => "A",       // alef and alef-madda: long a
            'ە' => "a",
            'ێ' => "e",
            'ۆ' => "o",
            'و' => {
                if role == SoundType::Vowel {
                    "u"
                } else {
                    "w"
                }
            }
            'ی' => {
                if role == SoundType::Vowel {
                    "i"
                } else {
                    "y"
                }
            }
            _ => continue, // ignore punctuation or non-kurdish characters in phone string
        };
        phonemes.push(phone);
    }

    phonemes.join(" ")
}

/// Converts a full normalized Sorani Kurdish sentence/text to space-separated phonemes.
pub fn g2p(text: &str) -> String {
    let mut result = Vec::new();
    for word in text.split_whitespace() {
        let phonetic_word = word_to_g2p(word);
        if !phonetic_word.is_empty() {
            result.push(phonetic_word);
        }
    }
    result.join("  ") // Use double space between words for clarity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kurdish_g2p() {
        assert_eq!(g2p("کورد"), "k u r d");
        assert_eq!(g2p("خاو"), "x A w");
        assert_eq!(g2p("سەیر"), "s a y r");
        assert_eq!(g2p("یاری"), "y A r i");
        assert_eq!(g2p("پێنج"), "p e n j");
        assert_eq!(g2p("چوار"), "c w A r");
        assert_eq!(g2p("دوو"), "d U");
    }

    #[test]
    fn hamza_carriers_carry_phonemes_so_distinct_words_dont_collapse() {
        // آ (alef-madda), ء (hamza), ؤ (waw-with-hamza) survive normalization, so they MUST produce a
        // phoneme — otherwise distinct words collapse to identical phonemes and a correction memory keyed
        // on one silently rewrites the other.
        assert!(g2p("آب").contains('A'), "alef-madda must produce the long-a phoneme, not be dropped");
        assert_ne!(g2p("بآر"), g2p("بر"), "بآر and بر must NOT g2p to the same phonemes");
        assert_ne!(g2p("سؤر"), g2p("سر"), "سؤر and سر must NOT g2p to the same phonemes");
        assert_ne!(g2p("ئاو"), g2p("او"), "hamza ئ vs none must differ");
    }

    // The 4-pass role resolver does heavy roles[i-1]/roles[i+1]/roles[i+2] indexing.
    // These adversarial inputs exercise every boundary (n=0/1/2, runs of the
    // ambiguous 'و'/'ی', non-Kurdish scripts, diacritics, control chars, multi-byte
    // graphemes) and assert the converter is TOTAL — never panics, always returns.
    #[test]
    fn g2p_is_total_on_adversarial_input() {
        let cases = [
            "",                 // empty
            "   \t\n ",         // whitespace only
            "و",                // lone ambiguous waw (n=1, undetermined at start)
            "ی",                // lone ambiguous yeh
            "وو",               // double waw (Pass-2 pair logic, n=2)
            "ووو",              // triple waw (odd run — last falls through to end-rule)
            "وووو",             // quad waw
            "ییی",              // run of yeh
            "ووییوو",           // alternating ambiguous run
            "ـــ",              // tatweel only
            "\u{200C}\u{200C}", // ZWNJ run
            "ًٌٍَُِّْ",                 // Arabic diacritics only (combining marks)
            "١٢٣٤٥",            // Arabic-Indic digits
            "abcdef",           // Latin (all-undetermined to the Kurdish classifier)
            "a و b ی c",        // mixed scripts with ambiguous letters
            "😀🎧",             // multi-byte emoji graphemes
            "\u{0}\u{1}\u{7}",  // control characters
            "کوردوووووستان",    // long internal waw run inside a real word
        ];
        for c in cases {
            let out = g2p(c); // must not panic
                              // Sanity: output is finite text; empty in → empty out.
            if c.trim().is_empty() {
                assert!(out.is_empty(), "empty/whitespace input should yield empty: {c:?} -> {out:?}");
            }
        }

        // A huge single token must also be handled without panic or pathological blowup.
        let huge = "و".repeat(5000);
        let _ = word_to_g2p(&huge);
    }

    #[test]
    fn combining_marks_dont_split_the_waw_digraph() {
        // A stray sukun (U+0652) between the two waws of the long-vowel digraph 'وو' must NOT break
        // it into two consonants ("d w w"); transparent marks are filtered before role resolution.
        assert_eq!(word_to_g2p("د\u{0648}\u{0652}\u{0648}"), "d U");
        assert_eq!(word_to_g2p("دوو"), "d U", "baseline digraph unchanged");
    }

    /// The grapheme-to-phoneme letter table, pinned letter by letter.
    ///
    /// 28 of the sweep's survivors were literally "delete match arm" here: nothing asserted what
    /// any individual Kurdish letter maps to, so an arm could vanish and every existing test still
    /// passed (a dropped letter silently falls through to the catch-all). g2p output is the
    /// comparison space for the whole refinery - the phonetic edit distance, consensus alignment
    /// and dedup all run on these strings - so one wrong letter quietly corrupts every downstream
    /// distance without ever erroring.
    ///
    /// Frame: consonant + alef, which g2p renders as space-separated phonemes ("b A").
    #[test]
    fn g2p_letter_table_is_exact() {
        let table: &[(char, &str)] = &[
            ('ب', "b"),
            ('پ', "p"),
            ('ت', "t"),
            ('ج', "j"),
            ('چ', "c"),
            ('ح', "H"),
            ('خ', "x"),
            ('د', "d"),
            ('ر', "r"),
            ('ڕ', "R"),
            ('ز', "z"),
            ('ژ', "Z"),
            ('س', "s"),
            ('ش', "S"),
            ('ع', "E"),
            ('غ', "G"),
            ('ف', "f"),
            ('ڤ', "v"),
            ('ق', "q"),
            ('ک', "k"),
            ('گ', "g"),
            ('ل', "l"),
            ('ڵ', "L"),
            ('م', "m"),
            ('ن', "n"),
            ('ھ', "h"),
            ('ه', "h"),
        ];
        for (letter, phoneme) in table {
            let got = g2p(&format!("{letter}ا"));
            assert_eq!(got, format!("{phoneme} A"), "letter {letter} must map to /{phoneme}/");
        }

        // Vowels, in a consonant frame.
        assert_eq!(g2p("بە"), "b a");
        assert_eq!(g2p("بێ"), "b e");
        assert_eq!(g2p("بۆ"), "b o");
        assert_eq!(g2p("بو"), "b u");

        // Kurdish orthography distinguishes plain and emphatic forms; collapsing either pair would
        // make genuinely different words compare as identical.
        assert_ne!(g2p("را"), g2p("ڕا"), "r and R must stay distinct");
        assert_ne!(g2p("لا"), g2p("ڵا"), "l and L must stay distinct");
        // Arabic heh and Kurdish heh are the SAME phoneme - load-bearing for the homophone
        // handling in compute_phonetic_diff.
        assert_eq!(g2p("ها"), g2p("ھا"), "both heh forms are /h/");
    }

    #[test]
    fn transparent_chars_dont_corrupt_neighbor_roles() {
        // A non-phonetic char adjacent to an ambiguous 'و'/'ی' must not change the neighbor's
        // phoneme. Old code left it as an Undetermined role slot that flipped the classification.
        assert_eq!(word_to_g2p("دو"), "d u", "baseline");
        assert_eq!(word_to_g2p("د\u{200C}و"), "d u", "ZWNJ must not flip و to consonant");
        assert_eq!(word_to_g2p("د\u{064B}و"), "d u", "fathatan must not flip و to consonant");
    }
}
