//! jury/t1_judge.rs — Text-based tool judge
//!
//! Fires on T0 disagreements. Uses lexicon lookup, n-gram perplexity,
//! and (optionally) a web-search passthrough to localize the suspect span
//! and fix trivial cases. Hard cases are escalated to T2.

use crate::db::SegmentHypothesis;
use serde::{Deserialize, Serialize};

// ─── Evidence types ───────────────────────────────────────────────────────────

/// A single piece of evidence from a T1 tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub tool: String,
    pub result: String,
    pub supports_hypothesis: bool,
}

/// Decision returned by the T1 judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum T1Decision {
    /// T1 is confident enough to commit a fix.
    Commit { segment_id: String, transcript: String, reason: String, evidence: Vec<Evidence>, confidence: f64 },
    /// T1 cannot resolve the disagreement; escalate to T2 (audio listener).
    EscalateToT2 { segment_id: String, hypotheses: Vec<SegmentHypothesis>, t1_evidence: Vec<Evidence> },
}

// ─── Lexicon lookup ───────────────────────────────────────────────────────────

/// Estimate whether a word is plausible Sorani Kurdish by checking that
/// it contains mostly Unicode characters in the Arabic/Kurdish block
/// (U+0600–U+06FF) and doesn't consist purely of Latin characters.
pub fn lexicon_lookup(word: &str) -> Evidence {
    let total_chars = word.chars().count();
    if total_chars == 0 {
        return Evidence { tool: "lexicon".into(), result: "empty word".into(), supports_hypothesis: false };
    }
    let arabic_block_chars = word.chars().filter(|&c| (c as u32) >= 0x0600 && (c as u32) <= 0x06FF).count();
    let coverage = arabic_block_chars as f64 / total_chars as f64;
    let hit = coverage >= 0.6; // ≥60% Arabic-block chars → plausible Kurdish word
    Evidence {
        tool: "lexicon".into(),
        result: if hit {
            format!("'{word}' looks like valid Sorani (coverage={:.0}%)", coverage * 100.0)
        } else {
            format!("'{word}' may not be Sorani (coverage={:.0}%)", coverage * 100.0)
        },
        supports_hypothesis: hit,
    }
}

// ─── N-gram perplexity ────────────────────────────────────────────────────────

/// Simple character-level n-gram entropy as a perplexity proxy.
/// A real deployment would load a Sorani LM; this is the offline fallback.
pub fn ngram_perplexity(text: &str) -> f64 {
    if text.is_empty() {
        return f64::INFINITY;
    }
    // Character-level trigram entropy from the text itself (bootstrap estimator), with an explicit
    // repetition penalty (below). Raw self-entropy alone is misleading: a looping ASR output collapses
    // onto a few trigrams and so has LOW self-entropy, which would otherwise be rewarded as "fluent".
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 3 {
        return 50.0; // penalize very short texts
    }
    let mut trigrams = std::collections::HashMap::new();
    for w in chars.windows(3) {
        *trigrams.entry(w.to_vec()).or_insert(0u32) += 1;
    }
    let n = trigrams.values().sum::<u32>() as f64;
    let unique = trigrams.len() as f64;
    let entropy: f64 = trigrams
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum();
    // Repetition penalty: the average number of times each distinct trigram recurs (1.0 for all-distinct
    // text, growing as the same trigrams loop), squared. This makes repetition RAISE perplexity — and so
    // lower the fluency score — instead of lowering it via the entropy drop. Squared so heavy looping is
    // strongly penalized while ordinary letter reuse in real prose barely moves the number.
    let repetition_penalty = (n / unique).powi(2);
    // Scale to a pseudo-perplexity in [1, 200]
    (2.0f64.powf(entropy) * repetition_penalty).clamp(1.0, 200.0)
}

// ─── Core T1 judge ───────────────────────────────────────────────────────────

/// Evaluate a set of hypotheses with text-based tools.
///
/// Strategy:
///   1. For each hypothesis compute lexicon coverage and perplexity.
///   2. Score = lexicon_hits / words + 1/log(perplexity).
///   3. If best-scoring hypothesis exceeds the commit threshold → Commit.
///   4. Otherwise → EscalateToT2.
pub fn judge_t1(
    segment_id: &str,
    hypotheses: &[SegmentHypothesis],
    commit_threshold: f64, // e.g. 0.75
) -> T1Decision {
    if hypotheses.is_empty() {
        return T1Decision::EscalateToT2 { segment_id: segment_id.into(), hypotheses: vec![], t1_evidence: vec![] };
    }

    let mut best_score = f64::NEG_INFINITY;
    let mut best_hyp: Option<&SegmentHypothesis> = None;
    let mut all_evidence: Vec<Evidence> = Vec::new();

    for hyp in hypotheses {
        let words: Vec<&str> = hyp.transcript.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }

        // Lexicon check — count fraction of plausibly Kurdish words
        let mut known = 0usize;
        for w in &words {
            let ev = lexicon_lookup(w);
            if ev.supports_hypothesis {
                known += 1;
            }
            all_evidence.push(ev);
        }
        let lex_score = known as f64 / words.len() as f64;

        // Perplexity check
        let ppl = ngram_perplexity(&hyp.transcript);
        let ppl_score = 1.0 / ppl.ln().max(0.01);

        // Combined score — weighted
        let model_prior = hyp.confidence.unwrap_or(0.5);
        let score = 0.4 * lex_score + 0.3 * ppl_score.min(1.0) + 0.3 * model_prior;

        if score > best_score {
            best_score = score;
            best_hyp = Some(hyp);
        }
    }

    let Some(winner) = best_hyp else {
        return T1Decision::EscalateToT2 {
            segment_id: segment_id.into(),
            hypotheses: hypotheses.to_vec(),
            t1_evidence: all_evidence,
        };
    };

    if best_score >= commit_threshold {
        T1Decision::Commit {
            segment_id: segment_id.into(),
            transcript: winner.transcript.clone(),
            reason: format!(
                "T1 text judge selected '{}' (score={:.2}): highest lexicon coverage + fluency",
                winner.model_id, best_score
            ),
            evidence: all_evidence,
            confidence: best_score,
        }
    } else {
        T1Decision::EscalateToT2 {
            segment_id: segment_id.into(),
            hypotheses: hypotheses.to_vec(),
            t1_evidence: all_evidence,
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hyp(seg: &str, model: &str, text: &str, conf: f64) -> SegmentHypothesis {
        SegmentHypothesis {
            segment_id: seg.into(),
            model_id: model.into(),
            transcript: text.into(),
            confidence: Some(conf),
        }
    }

    #[test]
    fn test_perplexity_non_infinite() {
        let ppl = ngram_perplexity("کوردستان ئازاد");
        assert!(ppl.is_finite());
        assert!(ppl > 1.0);
    }

    #[test]
    fn test_perplexity_penalizes_repetition() {
        // A looping ASR output (the same word repeated) must score WORSE (higher perplexity) than a
        // diverse, genuine transcript of comparable length — not better, as raw self-entropy would have
        // it. Otherwise the T1 judge commits hallucinated loops as "fluent".
        let diverse = ngram_perplexity("کوردستان ئازاد و سەربەخۆ");
        let looping = ngram_perplexity("کوردکوردکوردکوردکوردکورد");
        assert!(
            looping > diverse,
            "repetitive text must have higher perplexity (looping={looping:.2}, diverse={diverse:.2})"
        );
    }

    #[test]
    fn test_lexicon_lookup_kurdish() {
        let ev = lexicon_lookup("کوردستان");
        assert!(ev.supports_hypothesis, "Pure Kurdish word should have high coverage");
    }

    #[test]
    fn test_lexicon_lookup_latin() {
        let ev = lexicon_lookup("xyzxyzxyz");
        assert!(!ev.supports_hypothesis, "Pure Latin word should fail Kurdish coverage check");
    }

    #[test]
    fn test_t1_judge_selects_best() {
        let hyps = vec![make_hyp("s1", "good-model", "کوردستان", 0.9), make_hyp("s1", "bad-model", "xyzxyzxyz", 0.3)];
        // With commit_threshold=0.0 it should always commit
        let decision = judge_t1("s1", &hyps, 0.0);
        assert!(matches!(decision, T1Decision::Commit { .. }));
    }

    #[test]
    fn test_t1_judge_escalates_on_threshold() {
        let hyps = vec![make_hyp("s1", "model-a", "کوردستان", 0.4), make_hyp("s1", "model-b", "ئێران", 0.4)];
        // Very high threshold → always escalate
        let decision = judge_t1("s1", &hyps, 99.0);
        assert!(matches!(decision, T1Decision::EscalateToT2 { .. }));
    }
}
