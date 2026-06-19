use crate::db::SegmentHypothesis;
use crate::diff::phonetic::compute_phonetic_diff;
use crate::diff::DiffOp;
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Slot {
    candidates: Vec<String>,
    observations: Vec<HypothesisObs>,
    posteriors: Vec<f64>, // maps 1-to-1 with candidates
}

#[derive(Debug, Clone)]
struct HypothesisObs {
    model_id: String,
    observed_token: String,
}

#[derive(Debug, Clone)]
struct SegmentSlots {
    slots: Vec<Slot>,
    _anchor_model: String,
}

pub struct IrtResults {
    pub consensus_transcripts: HashMap<String, String>,
    pub segment_confidences: HashMap<String, f64>,
    pub model_abilities: HashMap<String, f64>,
    pub segment_difficulties: HashMap<String, f64>,
}

/// Helper to assign initial model abilities based on name heuristics.
fn get_initial_ability(model_id: &str) -> f64 {
    let lower = model_id.to_lowercase();
    if lower.contains("gemini") {
        2.0
    } else if lower.contains("7b") {
        1.5
    } else if lower.contains("1b") {
        0.5
    } else if lower.contains("300m") {
        -0.5
    } else if lower.contains("whisper") {
        0.0
    } else if lower.contains("qwen") {
        -1.0
    } else {
        0.0
    }
}

/// Fits a 1PL Item Response Theory (IRT) model using the Expectation-Maximization (EM) algorithm
/// over multiple transcript hypotheses to compute a phonetic consensus transcript and posterior confidence.
pub fn fit_irt_consensus(hypotheses: &[SegmentHypothesis]) -> IrtResults {
    let mut segment_hyps: HashMap<String, Vec<&SegmentHypothesis>> = HashMap::new();
    for h in hypotheses {
        segment_hyps.entry(h.segment_id.clone()).or_default().push(h);
    }

    let mut model_abilities = HashMap::new();
    let mut segment_difficulties = HashMap::new();
    let mut segment_slots_map = HashMap::new();

    // Step 1: Align all hypotheses for each segment to construct slots (confusion network)
    for (segment_id, hyps) in &segment_hyps {
        if hyps.is_empty() {
            continue;
        }

        // Choose the anchor hypothesis (highest initial ability)
        let Some(anchor_hyp) = hyps
            .iter()
            .max_by(|a, b| {
                let ab_a = get_initial_ability(&a.model_id);
                let ab_b = get_initial_ability(&b.model_id);
                ab_a.partial_cmp(&ab_b).unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            // `hyps` is non-empty (guarded above), so `max_by` is always `Some`;
            // handle the impossible `None` without panicking.
            continue;
        };

        let anchor_words: Vec<String> = anchor_hyp.transcript.split_whitespace().map(|s| s.to_string()).collect();

        let mut slots: Vec<Slot> = anchor_words
            .iter()
            .map(|w| Slot {
                candidates: vec![w.clone(), "".to_string()],
                observations: vec![HypothesisObs { model_id: anchor_hyp.model_id.clone(), observed_token: w.clone() }],
                posteriors: vec![0.0; 2],
            })
            .collect();

        // Align other hypotheses to the anchor
        for h in hyps {
            if h.model_id == anchor_hyp.model_id {
                continue;
            }

            let diff = compute_phonetic_diff(&anchor_hyp.transcript, &h.transcript);
            let mut anchor_word_idx = 0;

            for change in &diff.changes {
                match change.op {
                    DiffOp::Equal => {
                        if anchor_word_idx < slots.len() {
                            slots[anchor_word_idx].observations.push(HypothesisObs {
                                model_id: h.model_id.clone(),
                                observed_token: change.value.clone(),
                            });
                            if !slots[anchor_word_idx].candidates.contains(&change.value) {
                                slots[anchor_word_idx].candidates.push(change.value.clone());
                            }
                            anchor_word_idx += 1;
                        }
                    }
                    DiffOp::Replace => {
                        // "anchor_word → other_word"
                        let parts: Vec<&str> = change.value.split(" → ").collect();
                        if parts.len() == 2 && anchor_word_idx < slots.len() {
                            let other_word = parts[1].to_string();
                            slots[anchor_word_idx].observations.push(HypothesisObs {
                                model_id: h.model_id.clone(),
                                observed_token: other_word.clone(),
                            });
                            if !slots[anchor_word_idx].candidates.contains(&other_word) {
                                slots[anchor_word_idx].candidates.push(other_word);
                            }
                            anchor_word_idx += 1;
                        }
                    }
                    DiffOp::Delete => {
                        // Deleted from other, meaning other outputted empty string in this anchor slot
                        if anchor_word_idx < slots.len() {
                            slots[anchor_word_idx]
                                .observations
                                .push(HypothesisObs { model_id: h.model_id.clone(), observed_token: "".to_string() });
                            anchor_word_idx += 1;
                        }
                    }
                    DiffOp::Insert => {
                        // Insertions in other are ignored to keep the anchor slots consistent
                    }
                }
            }
        }

        // Initialize candidate posteriors and ensure model_abilities keys exist
        for slot in &mut slots {
            // Deduplicate candidates
            let mut unique = Vec::new();
            for c in &slot.candidates {
                if !unique.contains(c) {
                    unique.push(c.clone());
                }
            }
            if !unique.contains(&"".to_string()) {
                unique.push("".to_string());
            }
            slot.candidates = unique;
            slot.posteriors = vec![0.0; slot.candidates.len()];

            for obs in &slot.observations {
                model_abilities.entry(obs.model_id.clone()).or_insert_with(|| get_initial_ability(&obs.model_id));
            }
        }

        segment_difficulties.insert(segment_id.clone(), 0.0);
        segment_slots_map
            .insert(segment_id.clone(), SegmentSlots { slots, _anchor_model: anchor_hyp.model_id.clone() });
    }

    let beta = 0.5f64;
    let learning_rate = 0.01f64;
    let iterations = 50;
    let update_abilities = segment_slots_map.len() >= 10;

    // Step 2: Expectation-Maximization solver loop
    for _iter in 0..iterations {
        // --- E-step: Compute candidate posteriors ---
        for (segment_id, seg_slots) in &mut segment_slots_map {
            let b_i = *segment_difficulties.get(segment_id).unwrap_or(&0.0);

            for slot in &mut seg_slots.slots {
                let mut log_posteriors = vec![0.0f64; slot.candidates.len()];

                for (v_idx, v) in slot.candidates.iter().enumerate() {
                    let mut log_p = 0.0f64;

                    for obs in &slot.observations {
                        let theta_j = *model_abilities.get(&obs.model_id).unwrap_or(&0.0);
                        let p_ij = 1.0 / (1.0 + (-(theta_j - b_i)).exp());

                        if &obs.observed_token == v {
                            log_p += p_ij.ln();
                        } else {
                            log_p += ((1.0 - p_ij) * beta).ln();
                        }
                    }
                    log_posteriors[v_idx] = log_p;
                }

                // Softmax normalization
                let max_log = log_posteriors.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let mut sum_exp = 0.0f64;
                let mut exp_vals = vec![0.0f64; slot.candidates.len()];
                for (v_idx, exp_val) in exp_vals.iter_mut().enumerate().take(slot.candidates.len()) {
                    let val = (log_posteriors[v_idx] - max_log).exp();
                    *exp_val = val;
                    sum_exp += val;
                }

                if sum_exp > 0.0 {
                    for (posterior, exp_val) in slot.posteriors.iter_mut().zip(exp_vals.iter()) {
                        *posterior = *exp_val / sum_exp;
                    }
                } else {
                    for posterior in &mut slot.posteriors {
                        *posterior = 1.0 / slot.candidates.len() as f64;
                    }
                }
            }
        }

        // --- M-step: Perform gradient updates on ability and difficulty ---
        let mut grad_theta = HashMap::new();
        let mut grad_b = HashMap::new();

        // Accumulate gradients in a deterministic segment order. grad_theta sums a
        // per-model gradient across segments, and f64 addition is not associative,
        // so iterating segment_slots_map in HashMap order (randomized per run) would
        // make the fitted abilities — and the published confidences they drive —
        // nondeterministic, breaking the reproducible-scorecard guarantee.
        let mut m_step_segments: Vec<(&String, &SegmentSlots)> = segment_slots_map.iter().collect();
        m_step_segments.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (segment_id, seg_slots) in m_step_segments {
            let b_i = *segment_difficulties.get(segment_id).unwrap_or(&0.0);

            for slot in &seg_slots.slots {
                for obs in &slot.observations {
                    let theta_j = *model_abilities.get(&obs.model_id).unwrap_or(&0.0);
                    let p_ij = 1.0 / (1.0 + (-(theta_j - b_i)).exp());

                    // Get posterior for the observed candidate
                    let w_val = slot
                        .candidates
                        .iter()
                        .position(|c| c == &obs.observed_token)
                        .map(|idx| slot.posteriors[idx])
                        .unwrap_or(0.0);

                    let grad = w_val - p_ij;
                    *grad_theta.entry(obs.model_id.clone()).or_insert(0.0) += grad;
                    *grad_b.entry(segment_id.clone()).or_insert(0.0) += p_ij - w_val;
                }
            }
        }

        // Apply gradient updates
        if update_abilities {
            for (model_id, grad) in grad_theta {
                if let Some(ability) = model_abilities.get_mut(&model_id) {
                    let prior = get_initial_ability(&model_id);
                    let reg = 1.0 * (*ability - prior);
                    *ability += learning_rate * (grad - reg);
                    *ability = (*ability).clamp(-3.0, 3.0);
                }
            }
        }
        for (segment_id, grad) in grad_b {
            if let Some(diff) = segment_difficulties.get_mut(&segment_id) {
                *diff += learning_rate * grad;
                *diff = (*diff).clamp(-3.0, 3.0);
            }
        }
    }

    // Step 3: Extract consensus transcripts and segment confidences
    let mut consensus_transcripts = HashMap::new();
    let mut segment_confidences = HashMap::new();

    for (segment_id, seg_slots) in &segment_slots_map {
        let mut consensus_words = Vec::new();
        let mut total_posterior = 0.0f64;
        let mut slot_count = 0usize;

        for slot in &seg_slots.slots {
            // Find candidate index with max posterior
            let max_idx = slot
                .posteriors
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(idx, _)| idx)
                .unwrap_or(0);

            let best_token = &slot.candidates[max_idx];
            let posterior = slot.posteriors[max_idx];

            if !best_token.is_empty() {
                consensus_words.push(best_token.clone());
            }
            total_posterior += posterior;
            slot_count += 1;
        }

        let consensus_text = consensus_words.join(" ");
        let confidence = if slot_count > 0 { total_posterior / slot_count as f64 } else { 1.0 };

        consensus_transcripts.insert(segment_id.clone(), consensus_text);
        segment_confidences.insert(segment_id.clone(), confidence);
    }

    IrtResults { consensus_transcripts, segment_confidences, model_abilities, segment_difficulties }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_irt_consensus() {
        let h1 = SegmentHypothesis {
            segment_id: "seg1".to_string(),
            model_id: "gemini".to_string(),
            transcript: "ئەمە دەنگە".to_string(),
            confidence: Some(0.95),
        };
        let h2 = SegmentHypothesis {
            segment_id: "seg1".to_string(),
            model_id: "whisper-noisy".to_string(),
            transcript: "ئەمە ڕەنگە".to_string(),
            confidence: Some(0.4),
        };
        let h3 = SegmentHypothesis {
            segment_id: "seg1".to_string(),
            model_id: "qwen-weak".to_string(),
            transcript: "ئەمە ڕەنگە".to_string(),
            confidence: Some(0.3),
        };

        // Even though two weak models agreed on "ڕەنگە", gemini (highly weighted) should prevail on "دەنگە"
        let res = fit_irt_consensus(&[h1, h2, h3]);
        let consensus = res.consensus_transcripts.get("seg1").unwrap();
        assert_eq!(consensus, "ئەمە دەنگە");

        // Verify gemini ability is higher than whisper / qwen
        let ability_gemini = res.model_abilities.get("gemini").unwrap();
        let ability_whisper = res.model_abilities.get("whisper-noisy").unwrap();
        assert!(ability_gemini > ability_whisper);
    }

    #[test]
    fn fit_irt_consensus_is_deterministic_across_runs() {
        // 12 segments (>= the 10-segment threshold that enables ability updates),
        // two models each with diverging transcripts so the M-step accumulates
        // nonzero per-model gradients across segments. The gradient sum is over
        // segment_slots_map (a HashMap whose iteration order is randomized per run),
        // and f64 addition is not associative — so without a deterministic order the
        // resulting abilities and published confidences differ bit-for-bit per run,
        // breaking the "reproducible scorecard" guarantee.
        let mut hyps = Vec::new();
        for i in 0..12u32 {
            let seg = format!("seg{i:02}");
            // Vary the transcripts per segment so each contributes a DIFFERENT
            // per-model gradient; a constant fixture would sum identically in any
            // order and hide the HashMap-ordering bug.
            let agree = "خۆش ".repeat((i % 3 + 1) as usize);
            let disagree = "خراپ ".repeat((i % 4 + 1) as usize);
            hyps.push(SegmentHypothesis {
                segment_id: seg.clone(),
                model_id: "gemini".to_string(),
                transcript: format!("ئەمە دەنگە {agree}باشە"),
                confidence: Some(0.9),
            });
            hyps.push(SegmentHypothesis {
                segment_id: seg,
                model_id: "whisper-noisy".to_string(),
                transcript: format!("ئەمە ڕەنگە {disagree}گەورە"),
                confidence: Some(0.4),
            });
        }

        let snapshot = |r: &IrtResults| {
            let mut conf: Vec<(String, f64)> = r.segment_confidences.iter().map(|(k, v)| (k.clone(), *v)).collect();
            conf.sort_by(|a, b| a.0.cmp(&b.0));
            let mut ab: Vec<(String, f64)> = r.model_abilities.iter().map(|(k, v)| (k.clone(), *v)).collect();
            ab.sort_by(|a, b| a.0.cmp(&b.0));
            (conf, ab)
        };

        let a = snapshot(&fit_irt_consensus(&hyps));
        let b = snapshot(&fit_irt_consensus(&hyps));
        let c = snapshot(&fit_irt_consensus(&hyps));
        assert_eq!(a, b, "IRT confidences/abilities must be reproducible run-to-run");
        assert_eq!(b, c, "IRT confidences/abilities must be reproducible run-to-run");
    }
}
