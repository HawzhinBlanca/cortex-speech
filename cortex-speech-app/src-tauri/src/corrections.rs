//! LOOP-0 error-memory extraction from a human correction.
//!
//! When a curator changes a word, we want the system to get that *same* word right next time with
//! NO retraining. The first step is turning a (wrong transcript, human fix) pair into structured
//! per-slot memories: the wrong token, the human's replacement, the canonical ±1 neighbor context
//! (the "slot key" a future decode matches on), and a phonetic key for the wrong token.
//!
//! The alignment is computed over NORMALIZED words, so a pure orthographic variant (Kaf/Yeh/Heh
//! codepoint differences) is a *match*, never a spurious "correction" — the moat is real fixes, not
//! normalization noise. This module is pure (no DB, no I/O); wiring the emitted memories into the
//! `correction_memory` table is a separate, thin step.

use crate::normalizer::SoraniNormalizer;

/// One learned substitution extracted from a human correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstitutionMemory {
    /// The model's wrong token (original surface form, not normalized).
    pub wrong_token: String,
    /// The human's replacement token (original surface form).
    pub human_token: String,
    /// `normalize(left_neighbor)|normalize(right_neighbor)` — the canonical windowed context that a
    /// future decode matches on. Empty sides mean the slot was at a sentence boundary.
    pub slot_key: String,
    /// The normalized wrong token — the key for phonetic similarity-gated firing. The matcher
    /// (`normalized_phonetic_word_distance`) applies g2p internally, so this stores the WORD, not
    /// its phonemes (passing pre-g2p'd phonemes would double-g2p and collapse every distance to 0).
    pub phonetic_key: String,
}

/// The char-only canonical normalizer: folds Kaf/Yeh/Heh/ZWNJ/tatweel/hamza but does NOT verbalize
/// numbers, so slot/phonetic keys are orthographically canonical without turning digits into words.
/// Shared with the training-text exports via `SoraniNormalizer::char_only`.
fn char_only_normalizer() -> SoraniNormalizer {
    SoraniNormalizer::char_only()
}

#[derive(Clone, Copy)]
enum AlignOp {
    /// A matched slot. Neighbor context is now read from the wrong-side immediate neighbors at the
    /// substitution site (so it matches the firing rule), so the match index is no longer needed here.
    Match,
    /// A substituted slot: `a` is the wrong-side word index, `b` the human-side index.
    Sub {
        a: usize,
        b: usize,
    },
    /// A deleted or inserted slot — a placeholder in the path; its indices are never read.
    Del,
    Ins,
}

/// Word-level alignment of normalized words `na` onto `nb`, with backtrace. Returns the ordered op
/// sequence (Match/Sub/Del/Ins) — the standard Levenshtein alignment path.
fn align_words(na: &[String], nb: &[String]) -> Vec<AlignOp> {
    let (n, m) = (na.len(), nb.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(na[i - 1] != nb[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let cost = usize::from(na[i - 1] != nb[j - 1]);
            if dp[i][j] == dp[i - 1][j - 1] + cost {
                ops.push(if cost == 0 { AlignOp::Match } else { AlignOp::Sub { a: i - 1, b: j - 1 } });
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            ops.push(AlignOp::Del);
            i -= 1;
        } else {
            ops.push(AlignOp::Ins);
            j -= 1;
        }
    }
    ops.reverse();
    ops
}

/// Extract one error-memory entry per *substituted* word in a human correction. `wrong` is the model
/// hypothesis, `right` the human fix. Pure insertions/deletions (added or removed words) are not
/// single-token swaps and are skipped — LOOP 0 only learns "this word should have been that word".
pub fn extract_substitution_memories(wrong: &str, right: &str) -> Vec<SubstitutionMemory> {
    let normalizer = char_only_normalizer();
    let a: Vec<&str> = wrong.split_whitespace().collect();
    let b: Vec<&str> = right.split_whitespace().collect();
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let na: Vec<String> = a.iter().map(|w| normalizer.normalize(w)).collect();
    let nb: Vec<String> = b.iter().map(|w| normalizer.normalize(w)).collect();
    let ops = align_words(&na, &nb);

    let mut out = Vec::new();
    for op in &ops {
        let AlignOp::Sub { a: ai, b: bj } = *op else { continue };
        // The +/-1 neighbor context MUST be the IMMEDIATE neighbors in the (normalized) WRONG sequence —
        // the exact key the firing rule (apply_memories) reconstructs from the raw input's adjacent tokens
        // (norm[i-1] / norm[i+1]). The previous code used the nearest *matched* word, which is not
        // computable at firing time (there is no alignment there): for an ADJACENT substitution the
        // immediate neighbor is itself a Sub (no Match), so capture stored key "left|" while firing built
        // "left|right" — they never matched and the memory silently never fired, even on an exact repeat
        // of the same ASR error. For a lone substitution flanked by matches the two definitions coincide,
        // so existing single-sub behavior (and its tests) is unchanged.
        let left = if ai > 0 { na[ai - 1].clone() } else { String::new() };
        let right_ctx = na.get(ai + 1).cloned().unwrap_or_default();
        out.push(SubstitutionMemory {
            slot_key: format!("{left}|{right_ctx}"),
            phonetic_key: na[ai].clone(),
            wrong_token: a[ai].to_string(),
            human_token: b[bj].to_string(),
        });
    }
    out
}

/// A stored LOOP-0 memory as the firing rule sees it (a `correction_memory` row plus its
/// confidence/hit-count gates).
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub wrong_token: String,
    pub human_token: String,
    pub slot_key: String,
    pub phonetic_key: String,
    pub confidence: f64,
    pub hit_count: i64,
}

/// The firing gates. Defaults are the doc's starting points; all are tunable against the gold set's
/// over-trigger rate.
#[derive(Debug, Clone)]
pub struct FiringConfig {
    /// Max normalized phonetic distance between the candidate and the memory's wrong token.
    pub phon_tau: f64,
    /// Min confidence for a memory to fire.
    pub tau_conf: f64,
    /// Min hit_count (independent confirmations) — the anti-one-off guard.
    pub min_hits: i64,
}

impl Default for FiringConfig {
    fn default() -> Self {
        Self { phon_tau: 0.2, tau_conf: 0.6, min_hits: 1 }
    }
}

/// Apply LOOP-0 error memories to a (fused) transcript: for each word, if a memory's slot matches
/// the normalized neighbor context AND the word is phonetically close to the memory's wrong token
/// AND the memory clears the confidence/hit-count gates, replace it with the human token. Pure and
/// deterministic. The gates — exact normalized slot match, phonetic similarity, confidence, and
/// independent-confirmation count — are what keep "fix once -> right forever" from poisoning correct
/// words. Returns the rewritten transcript (whitespace-normalized).
pub fn apply_memories(transcript: &str, memories: &[MemoryEntry], cfg: &FiringConfig) -> String {
    let normalizer = char_only_normalizer();
    let words: Vec<&str> = transcript.split_whitespace().collect();
    if words.is_empty() {
        return String::new();
    }
    let norm: Vec<String> = words.iter().map(|w| normalizer.normalize(w)).collect();
    let mut out: Vec<String> = words.iter().map(|w| (*w).to_string()).collect();

    for (i, word_norm) in norm.iter().enumerate() {
        let left = if i > 0 { norm[i - 1].as_str() } else { "" };
        let right = norm.get(i + 1).map(String::as_str).unwrap_or("");
        let slot_key = format!("{left}|{right}");
        // The closest-sounding memory at this slot that clears every gate wins. The distance
        // function g2p's both inputs internally, so we pass the normalized WORDS, never phonemes.
        let best = memories
            .iter()
            .filter(|m| m.slot_key == slot_key && m.confidence > cfg.tau_conf && m.hit_count >= cfg.min_hits)
            .map(|m| (m, crate::diff::phonetic::normalized_phonetic_word_distance(word_norm, &m.phonetic_key)))
            .filter(|(_, dist)| *dist <= cfg.phon_tau)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        if let Some((m, _)) = best {
            out[i] = m.human_token.clone();
        }
    }
    out.join(" ")
}

/// Word-error count (substitutions + deletions + insertions) between two transcripts, compared on
/// NORMALIZED words — the same alignment the firing rule uses.
fn word_error_count(a: &str, b: &str) -> usize {
    let normalizer = char_only_normalizer();
    let na: Vec<String> = a.split_whitespace().map(|w| normalizer.normalize(w)).collect();
    let nb: Vec<String> = b.split_whitespace().map(|w| normalizer.normalize(w)).collect();
    align_words(&na, &nb).iter().filter(|op| !matches!(op, AlignOp::Match)).count()
}

/// The net effect of LOOP-0 firing on a gold set: the change in total word-error count from applying
/// firing to each ASR output. NEGATIVE means firing helped (fewer errors); POSITIVE means it hurt
/// (over-triggered on already-correct words); zero means no net effect. This is the signal that
/// gates whether firing is safe to enable for a given memory store + threshold config — run it on a
/// held-out gold set and only enable firing (or keep a threshold) while the delta stays <= 0.
pub fn firing_error_delta(gold: &[(String, String)], memories: &[MemoryEntry], cfg: &FiringConfig) -> i64 {
    let mut delta = 0i64;
    for (asr, reference) in gold {
        let before = word_error_count(asr, reference) as i64;
        let fired = apply_memories(asr, memories, cfg);
        let after = word_error_count(&fired, reference) as i64;
        delta += after - before;
    }
    delta
}

/// The Beta(1,1)-posterior mean confidence of a memory given its firing-outcome evidence:
/// `(confirm + 1) / (confirm + override + 2)`. A memory with NO evidence sits at the neutral prior
/// 0.5 — deliberately BELOW the default `tau_conf` 0.6 — so a freshly captured memory must EARN the
/// right to fire by accumulating human confirmations. A memory the human keeps overriding decays
/// toward 0 and drops out of firing eligibility. This is the anti-poisoning mechanism the true-10
/// audit required before `loop0_firing_enabled` may ever go live (previously `confidence` was frozen
/// at 1.0 and the `tau_conf` gate was vacuous).
pub fn beta_confidence(confirm: i64, overrides: i64) -> f64 {
    (confirm as f64 + 1.0) / (confirm as f64 + overrides as f64 + 2.0)
}

/// What a single memory WOULD have done to one segment, judged against the human's own answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOutcome {
    /// Fired and moved the transcript TOWARD the human's answer — evidence the memory was right.
    Confirm,
    /// Fired and moved the transcript AWAY from the human's answer — an over-trigger.
    Override,
    /// Did not fire here, or firing left the word-error count unchanged — no evidence either way.
    Neutral,
}

/// Classify what a single `memory` would have done to `original` relative to the human's `reference`
/// answer, using the SAME normalized word-error alignment as [`firing_error_delta`]:
///
/// * fired and the result is CLOSER to the human answer -> [`MemoryOutcome::Confirm`]
/// * fired and the result is FARTHER from the human answer -> [`MemoryOutcome::Override`]
/// * did not fire, or no net change -> [`MemoryOutcome::Neutral`]
///
/// The eligibility gates (`tau_conf`, `min_hits`) are DISABLED here on purpose: confidence is the
/// quantity we are trying to LEARN, so a memory not yet confident enough to fire must still be able
/// to accumulate the confirmations that would lift it over the bar — otherwise a fresh memory at the
/// 0.5 prior could never escape it (a firing deadlock). Only the STRUCTURAL gates (exact slot match
/// + phonetic distance) apply — they decide whether the memory is even relevant to this slot.
pub fn classify_memory_outcome(
    original: &str,
    reference: &str,
    memory: &MemoryEntry,
    cfg: &FiringConfig,
) -> MemoryOutcome {
    let eval_cfg = FiringConfig { phon_tau: cfg.phon_tau, tau_conf: f64::NEG_INFINITY, min_hits: 0 };
    let fired = apply_memories(original, std::slice::from_ref(memory), &eval_cfg);
    let before = word_error_count(original, reference) as i64;
    let after = word_error_count(&fired, reference) as i64;
    match after.cmp(&before) {
        std::cmp::Ordering::Less => MemoryOutcome::Confirm,
        std::cmp::Ordering::Greater => MemoryOutcome::Override,
        std::cmp::Ordering::Equal => MemoryOutcome::Neutral,
    }
}

/// Indices of the memories that would ACTUALLY fire on `text` — ONE winner per word slot (the closest
/// phonetic match clearing the structural gates), mirroring `apply_memories`'s selection exactly. The
/// eligibility gates are disabled (via `cfg`) so a not-yet-confident memory is still counted. Deduped:
/// a memory that wins several slots appears once.
fn firing_winner_indices(text: &str, memories: &[MemoryEntry], cfg: &FiringConfig) -> Vec<usize> {
    let normalizer = char_only_normalizer();
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let norm: Vec<String> = words.iter().map(|w| normalizer.normalize(w)).collect();
    let mut winners = Vec::new();
    for (i, word_norm) in norm.iter().enumerate() {
        let left = if i > 0 { norm[i - 1].as_str() } else { "" };
        let right = norm.get(i + 1).map(String::as_str).unwrap_or("");
        let slot_key = format!("{left}|{right}");
        let best = memories
            .iter()
            .enumerate()
            .filter(|(_, m)| m.slot_key == slot_key && m.confidence > cfg.tau_conf && m.hit_count >= cfg.min_hits)
            .map(|(idx, m)| (idx, crate::diff::phonetic::normalized_phonetic_word_distance(word_norm, &m.phonetic_key)))
            .filter(|(_, dist)| *dist <= cfg.phon_tau)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((idx, _)) = best {
            if !winners.contains(&idx) {
                winners.push(idx);
            }
        }
    }
    winners
}

/// Classify firing-outcome evidence for a human decision on ONE segment, crediting only the memory
/// that would ACTUALLY fire at each slot (winner-take-all, matching runtime `apply_memories`). This
/// prevents a losing sibling memory in the same slot — e.g. the same wrong token remembered with two
/// different human fixes — from collecting a spurious confirm/override it could never earn at decode
/// time. Returns `(index_into_memories, outcome)` for each firing, non-Neutral memory.
pub fn classify_memory_outcomes(
    original: &str,
    reference: &str,
    memories: &[MemoryEntry],
    cfg: &FiringConfig,
) -> Vec<(usize, MemoryOutcome)> {
    let eval_cfg = FiringConfig { phon_tau: cfg.phon_tau, tau_conf: f64::NEG_INFINITY, min_hits: 0 };
    firing_winner_indices(original, memories, &eval_cfg)
        .into_iter()
        .filter_map(|idx| match classify_memory_outcome(original, reference, &memories[idx], cfg) {
            MemoryOutcome::Neutral => None,
            outcome => Some((idx, outcome)),
        })
        .collect()
}

/// Human-readable descriptions of the memories that would ACTUALLY fire on `transcript` under `cfg`
/// (real eligibility gates + winner-take-all), each as `wrong->human @[slot]`. This is the PROVENANCE
/// of a LOOP-0 rewrite: without it a fired correction is indistinguishable from raw ASR output. Empty
/// when nothing fires.
pub fn fired_memories_summary(transcript: &str, memories: &[MemoryEntry], cfg: &FiringConfig) -> Vec<String> {
    firing_winner_indices(transcript, memories, cfg)
        .into_iter()
        .map(|i| {
            let m = &memories[i];
            format!("{}->{} @[{}]", m.wrong_token, m.human_token, m.slot_key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a firing-ready MemoryEntry by capturing it from a correction (round-trip realism).
    fn captured_entry(wrong_sentence: &str, right_sentence: &str, confidence: f64, hit_count: i64) -> MemoryEntry {
        let m = extract_substitution_memories(wrong_sentence, right_sentence).remove(0);
        MemoryEntry {
            wrong_token: m.wrong_token,
            human_token: m.human_token,
            slot_key: m.slot_key,
            phonetic_key: m.phonetic_key,
            confidence,
            hit_count,
        }
    }

    #[test]
    fn extracts_single_substitution_with_normalized_neighbors() {
        let mems = extract_substitution_memories("ئەو کابرایە چوو", "ئەو پیاوە چوو");
        assert_eq!(mems.len(), 1, "exactly one substituted word: {mems:?}");
        let m = &mems[0];
        assert_eq!(m.wrong_token, "کابرایە");
        assert_eq!(m.human_token, "پیاوە");
        let norm = char_only_normalizer();
        assert_eq!(m.slot_key, format!("{}|{}", norm.normalize("ئەو"), norm.normalize("چوو")));
        assert!(!m.phonetic_key.is_empty(), "a phonetic key is computed for the wrong token");
    }

    #[test]
    fn orthographic_variant_is_not_a_substitution() {
        // The first word differs only by Kaf/Yeh codepoint variants; after folding it is the SAME
        // word, so no spurious "correction" is learned.
        let wrong = "كوردي زمان"; // Arabic Kaf U+0643 + Arabic Yeh U+064A
        let right = "کوردی زمان"; // Kurdish Keheh U+06A9 + Kurdish Yeh U+06CC
        assert!(
            extract_substitution_memories(wrong, right).is_empty(),
            "a pure orthographic variant must not be learned as a correction"
        );
    }

    #[test]
    fn single_word_substitution_has_empty_neighbor_context() {
        let mems = extract_substitution_memories("کوردی", "کوردستان");
        assert_eq!(mems.len(), 1);
        assert_eq!(mems[0].slot_key, "|", "no neighbors -> empty slot context on both sides");
        assert_eq!(mems[0].wrong_token, "کوردی");
        assert_eq!(mems[0].human_token, "کوردستان");
    }

    #[test]
    fn pure_insertions_and_deletions_yield_no_memories() {
        assert!(
            extract_substitution_memories("ئەو چوو", "ئەو زۆر چوو").is_empty(),
            "an added word is not a single-token substitution"
        );
        assert!(
            extract_substitution_memories("ئەو زۆر چوو", "ئەو چوو").is_empty(),
            "a removed word is not a single-token substitution"
        );
    }

    #[test]
    fn empty_inputs_yield_no_memories() {
        assert!(extract_substitution_memories("", "something").is_empty());
        assert!(extract_substitution_memories("something", "").is_empty());
    }

    #[test]
    fn multiple_substitutions_are_each_captured() {
        let mems = extract_substitution_memories("من ساڵی پێنج بووم", "من ساڵی شەش هاتم");
        assert_eq!(mems.len(), 2, "both substituted words are captured: {mems:?}");
        let wrongs: Vec<&str> = mems.iter().map(|m| m.wrong_token.as_str()).collect();
        assert!(wrongs.contains(&"پێنج"), "{wrongs:?}");
        assert!(wrongs.contains(&"بووم"), "{wrongs:?}");
    }

    // --- LOOP-0 firing ---

    #[test]
    fn memory_fires_and_corrects_the_same_confusion() {
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 1);
        let out = apply_memories("ئەو ساڵە باش بوو", &[entry], &FiringConfig::default());
        assert_eq!(out, "ئەو ساڵە خراپ بوو", "the remembered fix must fire on the same confusion");
    }

    #[test]
    fn adjacent_substitutions_round_trip_and_fire() {
        // Two ADJACENT substitutions (پێنج->شەش then بووم->هاتم, with no matched word between them). Before
        // the neighbor-key fix, capture stored a nearest-Match key ("ساڵی|") that firing's immediate-
        // neighbor key ("ساڵی|بووم") never matched, so NEITHER memory fired — even on an exact repeat of
        // the same ASR output. Both must now fire and reproduce the human correction in full.
        let mems = extract_substitution_memories("من ساڵی پێنج بووم", "من ساڵی شەش هاتم");
        assert_eq!(mems.len(), 2);
        let entries: Vec<MemoryEntry> = mems
            .into_iter()
            .map(|m| MemoryEntry {
                wrong_token: m.wrong_token,
                human_token: m.human_token,
                slot_key: m.slot_key,
                phonetic_key: m.phonetic_key,
                confidence: 1.0,
                hit_count: 1,
            })
            .collect();
        let out = apply_memories("من ساڵی پێنج بووم", &entries, &FiringConfig::default());
        assert_eq!(out, "من ساڵی شەش هاتم", "both adjacent substitutions must fire on the same input");
    }

    #[test]
    fn memory_does_not_fire_in_a_different_slot() {
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 1);
        // "باش" appears but in a different neighbor context -> slot mismatch -> no fire.
        let out = apply_memories("زۆر باش نییە", &[entry], &FiringConfig::default());
        assert_eq!(out, "زۆر باش نییە", "a different slot must not fire");
    }

    #[test]
    fn low_confidence_memory_does_not_fire() {
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 0.5, 5); // below tau_conf 0.6
        let out = apply_memories("ئەو ساڵە باش بوو", &[entry], &FiringConfig::default());
        assert_eq!(out, "ئەو ساڵە باش بوو", "a low-confidence memory must not fire");
    }

    #[test]
    fn unconfirmed_memory_does_not_fire() {
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 0); // hit_count 0 < min 1
        let out = apply_memories("ئەو ساڵە باش بوو", &[entry], &FiringConfig::default());
        assert_eq!(out, "ئەو ساڵە باش بوو", "an unconfirmed (hit_count 0) memory must not fire");
    }

    #[test]
    fn phonetically_distant_word_in_same_slot_does_not_fire() {
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 1);
        // Same slot "ساڵە|بوو" but the word sounds nothing like "باش" -> the phonetic gate blocks it.
        let out = apply_memories("ئەو ساڵە گەورە بوو", &[entry], &FiringConfig::default());
        assert_eq!(out, "ئەو ساڵە گەورە بوو", "a phonetically distant word must not be rewritten");
    }

    #[test]
    fn firing_error_delta_is_negative_when_firing_helps() {
        // The ASR repeated the SAME error the memory remembers; the gold reference is the human fix,
        // so firing corrects a real error and lowers the error count.
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 1);
        let gold = vec![("ئەو ساڵە باش بوو".to_string(), "ئەو ساڵە خراپ بوو".to_string())];
        assert!(
            firing_error_delta(&gold, &[entry], &FiringConfig::default()) < 0,
            "firing that corrects a real error must lower the error count"
        );
    }

    #[test]
    fn firing_error_delta_is_positive_on_over_trigger() {
        // The memory rewrites باش->خراپ in this slot, but the gold reference KEEPS باش (the ASR was
        // already correct). Firing wrongly changes it -> the error count goes UP.
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 1);
        let gold = vec![("ئەو ساڵە باش بوو".to_string(), "ئەو ساڵە باش بوو".to_string())];
        assert!(
            firing_error_delta(&gold, &[entry], &FiringConfig::default()) > 0,
            "firing on an already-correct word must raise the error count (over-trigger signal)"
        );
    }

    #[test]
    fn firing_error_delta_is_zero_without_memories() {
        let gold = vec![("ئەو باش بوو".to_string(), "ئەو خراپ بوو".to_string())];
        assert_eq!(firing_error_delta(&gold, &[], &FiringConfig::default()), 0, "no memories -> no change");
    }

    #[test]
    fn near_homophone_in_same_slot_fires() {
        // The generalization the phonetic gate exists for: the decode produced "پاش" (a p/b
        // mishearing of the remembered wrong token "باش") in the same slot -> it is close enough
        // to fire and gets the human fix. This is "learn the confusion, not just the exact string".
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 1);
        let out = apply_memories("ئەو ساڵە پاش بوو", &[entry], &FiringConfig::default());
        assert_eq!(out, "ئەو ساڵە خراپ بوو", "a near-homophone of the wrong token must fire");
    }

    // --- evidence-based confidence (Beta(1,1) posterior over confirm/override) ---

    #[test]
    fn beta_confidence_prior_is_below_tau_and_evidence_moves_it() {
        let tau = FiringConfig::default().tau_conf;
        assert!((beta_confidence(0, 0) - 0.5).abs() < 1e-9, "no evidence -> neutral 0.5 prior");
        assert!(beta_confidence(0, 0) < tau, "the prior sits below tau_conf: a fresh memory cannot fire");
        assert!(beta_confidence(3, 0) > tau, "confirmations lift confidence above tau_conf");
        assert!(beta_confidence(0, 3) < tau, "overrides push confidence below tau_conf");
        // Monotone in each argument.
        assert!(beta_confidence(5, 0) > beta_confidence(1, 0), "more confirms -> higher");
        assert!(beta_confidence(0, 5) < beta_confidence(0, 1), "more overrides -> lower");
    }

    #[test]
    fn classify_memory_outcome_reads_evidence_even_below_tau_conf() {
        // The memory's stored confidence is 0.5 (below tau_conf): classify must STILL read its
        // structural match, or a fresh memory could never accumulate the evidence to climb the gate.
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 0.5, 0);
        let cfg = FiringConfig::default();
        // Human edited the same confusion -> the memory's output matches the human answer -> Confirm.
        assert_eq!(
            classify_memory_outcome("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", &entry, &cfg),
            MemoryOutcome::Confirm,
        );
        // Human ACCEPTED the original (reference == original) -> the memory over-triggers -> Override.
        assert_eq!(
            classify_memory_outcome("ئەو ساڵە باش بوو", "ئەو ساڵە باش بوو", &entry, &cfg),
            MemoryOutcome::Override,
        );
        // The memory's slot does not occur here -> it never fires -> Neutral (no evidence).
        assert_eq!(classify_memory_outcome("شتێکی جیاواز", "شتێکی جیاواز", &entry, &cfg), MemoryOutcome::Neutral,);
    }

    #[test]
    fn classify_memory_outcomes_credits_only_the_slot_winner() {
        // Two sibling memories in the SAME slot (ساڵە|بوو), both near "باش" phonetically. Only the one
        // the runtime would fire (the exact match) may collect evidence — the loser earns nothing.
        let m1 = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 1); // wrong=باش (exact)
        let m2 = captured_entry("ئەو ساڵە پاش بوو", "ئەو ساڵە چاک بوو", 1.0, 1); // wrong=پاش (near, loses)
        assert_eq!(m1.slot_key, m2.slot_key, "both memories occupy the same slot");
        let mems = vec![m1, m2];
        let cfg = FiringConfig::default();
        // Human ACCEPTED the original "…باش…": the winner over-triggered. Exactly ONE credit (the winner).
        let outcomes = classify_memory_outcomes("ئەو ساڵە باش بوو", "ئەو ساڵە باش بوو", &mems, &cfg);
        assert_eq!(outcomes.len(), 1, "only the slot winner is credited, not the losing sibling: {outcomes:?}");
        assert_eq!(outcomes[0].0, 0, "the exact-match memory (index 0) wins the slot");
        assert_eq!(outcomes[0].1, MemoryOutcome::Override, "accept-of-original -> over-trigger");
    }

    #[test]
    fn fired_memories_summary_attributes_the_rewrite() {
        let entry = captured_entry("ئەو ساڵە باش بوو", "ئەو ساڵە خراپ بوو", 1.0, 1);
        let cfg = FiringConfig::default();
        // Fires on the same confusion -> provenance names the wrong->human swap at its slot.
        let fired = fired_memories_summary("ئەو ساڵە باش بوو", std::slice::from_ref(&entry), &cfg);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].contains("باش->خراپ"), "provenance names the applied correction: {fired:?}");
        // Nothing fires on unrelated text -> empty provenance.
        assert!(fired_memories_summary("شتێکی جیاواز", std::slice::from_ref(&entry), &cfg).is_empty());
    }
}
