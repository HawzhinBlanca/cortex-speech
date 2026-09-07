# Reviewer Spelling Convention — ڕێنووسی یەکگرتووی پێداچوونەوە (v1.1 DRAFT, supplement to the Verbatim Conventions v1.0)

**Status: DRAFT for owner approval (2026-09-06). Not in force until the owner approves it.**
When approved this becomes **Sorani Verbatim Conventions v1.1**: `SORANI_VERBATIM_CONVENTIONS_v1.md`
keeps deciding *which words* are written; this page decides *how a said word is spelled and spaced*.
It binds every reviewer, the owner, and every machine second-look (champion draft, cloud judge) alike.

> **The one rule — یاسای یەکەم**
> Never change **which** word was said. This page only fixes **how** a said word is spelled.
> هەرگیز وشەی وتراو مەگۆڕە؛ ئەم پەڕە تەنیا شێوەی نووسینی وشەی وتراو دیاری دەکات.

## 0. Why this page exists (measured on the live verdicts, read-only, 2026-09-06)

Across 3,311 human verdict texts and 1,302 recorded edits:

- The characters reviewers most often insert or delete are, in order, **space, ە, ی, ا, و, ئ, ێ**.
  Good reviewers disagree mostly about **spacing/joining and vowel spelling**, not about the words.
  v1.0 has no spacing rule; §2 below is the gap it fills.
- Hamza on vowel-initial words is already near-universal (3,512 with vs 25 without in edits).
- Word-initial `ڕ` is already the practice (868 vs 32 thin `ر`).
- Arabic `ك`/`ي` are essentially absent (1 and 9 occurrences); ZWNJ is absent (0).
- Digits appear in **2.8%** of verdicts (93 texts), against v1.0 §4 which says numbers are words.
- Punctuation appears in **4.3%** of verdicts (144 texts), against v1.0 §5 which says do not add it.
- The elongated filler tokens prescribed by `ANNOTATION_GUIDELINES.md` §2 (`ئەمم`, `ئاا`) occur
  **zero** times; reviewers write fillers as v1.0 §2 says (as heard).

So the rules below lock in what reviewers already do, fill the spacing gap, and resolve two places
where the repo's two guideline documents contradict each other (§3 and §5, marked **CONTRADICTION**).

## 1. Letters — پیتەکان

| Rule | Write | Never | ڕوونکردنەوە |
|---|---|---|---|
| Kurdish kaf and yeh | `ک` (U+06A9), `ی` (U+06CC) | `ك` `ي` `ى` (Arabic) | پیتی کوردی، نەک عەرەبی |
| Vowel ە vs consonant ه | `ە` (U+06D5) for the vowel: `کۆمەڵ`, `ئەو`; `ه`/`ھ` for the /h/ sound: `هەولێر`, `هەموو` | `ه` + ZWNJ to imitate `ە` | ە بزوێنە، ه کۆنسۆنانتە |
| Hamza on vowel-initial words | `ئ` starts every word that begins with a vowel sound: `ئاو`, `ئەو`, `ئێمە`, `ئێستا`, `ئۆتۆمبێل`, `ئیسە` | `او`, `ەو`, `ێمە` | هەر وشەیەک بە بزوێن دەستپێبکات بە ئ دەنووسرێت |
| Thick and thin r | `ڕ` at the start of every word (Sorani has no thin word-initial r): `ڕۆژ`, `ڕاست`; inside words as pronounced: `کوڕ` vs `کار` | word-initial `ر` | لە سەرەتای وشە هەمیشە ڕ |
| Thick and thin l | `ڵ` / `ل` as pronounced: `گوڵ` vs `گول`, `ماڵ` vs `مال` | — | وەک دەبیسترێت |
| Long u | `وو` for the long vowel: `بوو`, `چوو`, `دوور`; single `و` for the consonant and the short vowel: `وشە`, `کورد` | `بو` for `بوو` | وو بۆ دەنگی درێژ |
| Diacritics (tashkeel) | none | `َ ِ ُ ّ` | بێ زەبر و زێر |
| Foreign words | v1.0 §3 stands: the word that was said, in Kurdish letters as pronounced (`تەلەفون`, `کۆمپیوتەر`); a word spoken *as English* may be written in Latin as v1.0 allows (0 of 3,311 verdicts have done so) | translating it | وشەی وتراو، نەک وەرگێڕان |

## 2. Spacing and joining — بۆشایی و پێکەوەنووسین (the new rule)

1. **One plain space between words. No ZWNJ (U+200C) anywhere.** The app turns a ZWNJ into a
   space, so a ZWNJ inside a word silently splits it into two tokens.
2. **Attached (no space):** verb prefixes `دە-` `نا-` `بـ-` `مە-` (`دەڕۆم`, `ناچم`); definite and
   plural endings `-ەکە` `-ەکان` `-ان` (`کتێبەکە`, `منداڵەکان`); person and possessive endings
   `-م` `-ت` `-ی` `-مان` `-تان` `-یان` (`کتێبم`, `ماڵیان`); emphatic `-یش` (`منیش`); indefinite
   `-ێک` / `-ێ` (`پیاوێک`, `پیاوێ` — the form that was said).
3. **Separate (a space):** prepositions and postpositions `لە` `بە` `بۆ` `لەگەڵ` `لەسەر` `تا`
   (`لە ماڵ`, `بۆ تۆ`); the conjunction `و` (`من و تۆ`, never `منو تۆ`); `کە`, `ئەگەر`, `بەڵام`;
   pronouns `ئەو`, `ئەمە`.
4. **Compound and light verbs:** parts separate unless the compound is one lexical word in common
   spelling: `پێم کرد`, `دەستی پێ کرد`; but `هەڵگرتن`, `دانیشتن`, `وەرگرتن` are one word.
5. **Ezafe / linking `ی`:** attached to the head noun: `کتێبی من`, `ماڵی گەورە`.
6. When two spellings are both common and both satisfy rules 1–5, **keep the draft's spelling**.
   Re-spacing a correct word is not a correction (see §6).

## 3. Numbers — ژمارەکان — **CONTRADICTION resolved in favour of v1.0**

`SORANI_VERBATIM_CONVENTIONS_v1.md` §4 (ACTIVE): numbers are written **as the words that were
said**, never as digits. `ANNOTATION_GUIDELINES.md` §1 says "digits as digits (500)". These
conflict. **v1.0 governs**: it is dated, versioned, marked ACTIVE, matches what a text-to-speech
set needs, and 97.2% of verdicts already comply.

- `دوو هەزار و چواردە`, `بیست و پێنج`, `سەد و پەنجا` — words, as spoken, spaced as spoken.
- A false start inside a number follows the `X-` fragment rule of `ANNOTATION_GUIDELINES.md`.
- **Cleanup**: the 93 verdicts containing digits go to a finalizer-grade reviewer through the listen
  list for a digit-to-word pass (the champion draft emits digits; those were accepted as-is).
- `ANNOTATION_GUIDELINES.md` §1 "Numbers" row is to be amended to point here.

## 4. Punctuation — خاڵبەندی (v1.0 §5 restated)

- **Do not add punctuation.** A clip with none is fully correct; a machine cannot hear a comma.
- If a draft carries a stray `.` `،` `؟`, removing it is allowed but **never a reason to edit on
  its own**. The scorer counts punctuation, so machine calibration strips it on both sides.

## 5. Fillers and false starts — **CONTRADICTION flagged for the owner**

`ANNOTATION_GUIDELINES.md` §2 prescribes elongated machine-safe filler tokens (`ئەمم`, `ئاا`,
`ئەە`) and says never write `ئەم` for a hesitation; `SORANI_VERBATIM_CONVENTIONS_v1.md` §2 says
write fillers as heard (`ئێە`, `ئەم`). Reviewers follow v1.0: the elongated tokens occur 0 times
in 3,311 verdicts. **Consequence**: the premium builder's filler filter, which keys on the
elongated tokens, currently filters nothing. Recommendation: keep v1.0 (as heard) for the
transcript, and treat filler detection as a downstream, audio-informed step rather than a spelling
rule. The owner decides; until then v1.0 stands and nobody is marked wrong for either spelling.

False starts keep the `X-` rule (`دە- دەڕوات`). Repeats are written as many times as said (v1.0 §1).

## 6. What the scorer forgives and what it counts — چی هەژمار دەکرێت

`normalize_for_metrics` (Rust, `wer.rs` + `normalizer.rs`, `sorani-normalizer-v1`, AsoSoft-derived)
folds **both** texts before any CER/WER or hidden-check comparison:

| Forgiven (free) | Counted (a real difference) |
|---|---|
| Arabic `ك`→`ک`, `ي`/`ى`→`ی`, `ې`→`ێ` | hamza present vs absent (`ئەو` vs `ەو`) |
| `ه`+ZWNJ → `ە`; word-final `ه`/`ة` → `ە` | `ڕ` vs `ر`, `ڵ` vs `ل` |
| ZWNJ → space; zero-width and bidi marks removed | `وو` vs `و` |
| tatweel and diacritics removed | any spacing or joining difference |
| `٠-٩`/`۰-۹` → `0-9` | digits vs number words |
| `,`→`،`, `?`→`؟`, `;`→`؛` | presence vs absence of punctuation |
| multiple spaces → one; letter case; NFC | every letter of every word |

- **Never edit a clip only for a forgiven difference.** It changes nothing downstream, and the
  no-op key records it as an unchanged accept, not an edit.
- **Do edit** for anything in the right column: that is what a training set and a hidden check see.

## 7. Priority when in doubt — لە کاتی گومان

1. The right **words**, all of them, only them (v1.0).
2. The spelling of §1 and the spacing of §2.
3. Keep the draft's spacing when both forms are common.
4. Never punctuation alone; never a forgiven difference alone.
5. Not sure of a word after listening twice? **Reject** (خراپ), do not guess.

## 8. For machine second-looks — بۆ پێداچوونەوەی ماشینی

Any model asked to check a draft receives v1.0 and this page verbatim and is scored against human
text under §6 with punctuation stripped (`MACHINE_SECOND_LOOK_CALIBRATION_PLAN.md`). A machine that
edits for forgiven differences, adds punctuation, converts dialect, or "standardizes" a spoken
word is mis-calibrated, not stricter. Machine output is evidence only: it never becomes a review
decision or the annotated transcript (owner canon 2026-08-12 and 2026-08-29).

---
Sources: repo `normalizer.rs` / `wer.rs` (AsoSoft-derived character rules, MIT);
`SORANI_VERBATIM_CONVENTIONS_v1.md`; `ANNOTATION_GUIDELINES.md`; live verdict statistics above;
AsoSoft Library normalization (ە/هـ/ی/ک standardization, heh+ZWNJ→ە, word-initial ر→ڕ, numeral
unification, conjunctive و separation); the KUTED Sorani corpus paper (arXiv 2604.00613), whose
standardization halved a corpus's unique-token count mostly through joined/non-joined word forms
and affix allomorphs — the same two classes our reviewers disagree on; the "transcription policy as
a latent variable" result (arXiv 2607.18934) already cited by v1.0.
