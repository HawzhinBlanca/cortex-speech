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
    /// True only when the EM actually updated abilities (≥10 segments this batch). The caller persists
    /// `model_abilities` only when this holds, so the store never captures un-fit heuristic priors.
    pub abilities_were_fit: bool,
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

/// Build a confusion network for ONE segment: per-anchor-word slots, each holding every model's
/// observed token and the deduped candidate set, by aligning each hypothesis to the highest-ability
/// anchor. Shared by the IRT EM fit and the offline consensus-draft builder so both align identically.
/// Returns `(slots, anchor_model_id)`, or None when there is no usable hypothesis.
fn build_confusion_slots(hyps: &[&SegmentHypothesis]) -> Option<(Vec<Slot>, String)> {
    if hyps.is_empty() {
        return None;
    }
    // Ignore empty-transcript hypotheses ENTIRELY. A model that returned "" must not become the anchor
    // (zero slots ⇒ a degenerate confidence) NOR vote — with its ability weight — for empty at every
    // slot, which would blank the consensus and let an empty transcript auto-accept. If every hypothesis
    // is empty the segment has no signal: return None so the caller omits it (escalates downstream rather
    // than recording a confident empty). Shared by the EM gate and the offline consensus draft, so this
    // input guard protects both. A segment with at least one real hypothesis still yields a draft from the
    // surviving models instead of being dropped.
    let filtered: Vec<&SegmentHypothesis> = hyps.iter().copied().filter(|h| !h.transcript.trim().is_empty()).collect();
    if filtered.is_empty() {
        return None;
    }
    let hyps: &[&SegmentHypothesis] = &filtered;
    // Choose the anchor hypothesis (highest initial ability).
    let anchor_hyp = hyps.iter().max_by(|a, b| {
        get_initial_ability(&a.model_id)
            .partial_cmp(&get_initial_ability(&b.model_id))
            .unwrap_or(std::cmp::Ordering::Equal)
    })?;

    let anchor_words: Vec<String> = anchor_hyp.transcript.split_whitespace().map(|s| s.to_string()).collect();
    let mut slots: Vec<Slot> = anchor_words
        .iter()
        .map(|w| Slot {
            candidates: vec![w.clone(), "".to_string()],
            observations: vec![HypothesisObs { model_id: anchor_hyp.model_id.clone(), observed_token: w.clone() }],
            posteriors: vec![0.0; 2],
        })
        .collect();

    // Align other hypotheses to the anchor.
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
                        slots[anchor_word_idx]
                            .observations
                            .push(HypothesisObs { model_id: h.model_id.clone(), observed_token: change.value.clone() });
                        if !slots[anchor_word_idx].candidates.contains(&change.value) {
                            slots[anchor_word_idx].candidates.push(change.value.clone());
                        }
                        anchor_word_idx += 1;
                    }
                }
                DiffOp::Replace => {
                    let parts: Vec<&str> = change.value.split(" → ").collect();
                    if parts.len() == 2 && anchor_word_idx < slots.len() {
                        let other_word = parts[1].to_string();
                        slots[anchor_word_idx]
                            .observations
                            .push(HypothesisObs { model_id: h.model_id.clone(), observed_token: other_word.clone() });
                        if !slots[anchor_word_idx].candidates.contains(&other_word) {
                            slots[anchor_word_idx].candidates.push(other_word);
                        }
                        anchor_word_idx += 1;
                    }
                }
                DiffOp::Delete => {
                    if anchor_word_idx < slots.len() {
                        slots[anchor_word_idx]
                            .observations
                            .push(HypothesisObs { model_id: h.model_id.clone(), observed_token: "".to_string() });
                        anchor_word_idx += 1;
                    }
                }
                DiffOp::Insert => {}
            }
        }
    }

    // Deduplicate candidates and ensure the empty (deletion) candidate exists; reset posteriors.
    for slot in &mut slots {
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
    }

    Some((slots, anchor_hyp.model_id.clone()))
}

/// Fits a 1PL Item Response Theory (IRT) model using the Expectation-Maximization (EM) algorithm
/// over multiple transcript hypotheses to compute a phonetic consensus transcript and posterior confidence.
pub fn fit_irt_consensus(hypotheses: &[SegmentHypothesis]) -> IrtResults {
    fit_irt_consensus_with_priors(hypotheses, &HashMap::new())
}

/// Same as [`fit_irt_consensus`] but seeds each model's ability from `priors` when present (else the
/// hardcoded heuristic). An EMPTY `priors` map makes this byte-identical to the heuristic-only path,
/// so the default gate is unchanged; a populated map warm-starts from previously-learned abilities.
/// The EM's regularization still anchors to the base heuristic prior, so learned abilities cannot
/// drift unboundedly across runs.
pub fn fit_irt_consensus_with_priors(hypotheses: &[SegmentHypothesis], priors: &HashMap<String, f64>) -> IrtResults {
    let mut segment_hyps: HashMap<String, Vec<&SegmentHypothesis>> = HashMap::new();
    for h in hypotheses {
        segment_hyps.entry(h.segment_id.clone()).or_default().push(h);
    }

    let mut model_abilities = HashMap::new();
    let mut segment_difficulties = HashMap::new();
    let mut segment_slots_map = HashMap::new();

    // Step 1: Align all hypotheses for each segment into a confusion network (shared builder).
    for (segment_id, hyps) in &segment_hyps {
        let Some((slots, anchor_model)) = build_confusion_slots(hyps) else {
            continue;
        };
        for slot in &slots {
            for obs in &slot.observations {
                model_abilities.entry(obs.model_id.clone()).or_insert_with(|| {
                    priors.get(&obs.model_id).copied().unwrap_or_else(|| get_initial_ability(&obs.model_id))
                });
            }
        }
        segment_difficulties.insert(segment_id.clone(), 0.0);
        segment_slots_map.insert(segment_id.clone(), SegmentSlots { slots, _anchor_model: anchor_model });
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
        if let Some((consensus_text, confidence)) = consensus_from_slots(&seg_slots.slots) {
            consensus_transcripts.insert(segment_id.clone(), consensus_text);
            segment_confidences.insert(segment_id.clone(), confidence);
        }
    }

    IrtResults {
        consensus_transcripts,
        segment_confidences,
        model_abilities,
        segment_difficulties,
        abilities_were_fit: update_abilities,
    }
}

/// Reduce one segment's aligned slots to `(consensus_text, confidence)`, or `None` for a degenerate
/// segment that must be OMITTED rather than scored.
///
/// Per slot the winner is the max-posterior candidate. An EMITTED word (non-empty winner) contributes
/// its posterior to the confidence numerator and its token to the text; a DELETION slot (winner = "")
/// contributes nothing to the numerator but still counts toward the denominator (`slot_count`). So a
/// truncated consensus — many high-posterior deletions, few retained words — is penalized rather than
/// flattered, and confidence reflects the fraction of anchor slots that produced a confident word, never
/// the model's confidence in DELETING them. (Each posterior is in [0,1] and at most one per slot enters
/// the numerator, so confidence stays in [0,1] without a clamp.)
///
/// Returns `None` when there are no slots, or the consensus is all-deletion (empty) — the caller then
/// omits the segment so the T0 gate gets no IRT confidence (escalates) instead of a fabricated perfect
/// score for an empty transcript.
fn consensus_from_slots(slots: &[Slot]) -> Option<(String, f64)> {
    let mut consensus_words = Vec::new();
    let mut total_posterior = 0.0f64;
    let mut slot_count = 0usize;
    for slot in slots {
        let max_idx = slot
            .posteriors
            .iter()
            .enumerate()
            .max_by(|a, b| {
                // On an exact posterior TIE, KEEP the word: demote the empty (deletion) candidate so a
                // word that ties a deletion wins, matching segment_consensus_words (the review draft).
                // Without this, max_by's last-maximal rule picks "" whenever it sits at a later candidate
                // index (it does — build_confusion_slots seeds `[anchor_word, ""]` then pushes more words),
                // so the GATE's consensus (and any auto-accepted text) could silently truncate a word the
                // human's draft kept — the two must agree slot-for-slot.
                let a_empty = slot.candidates.get(a.0).is_some_and(|c| c.is_empty());
                let b_empty = slot.candidates.get(b.0).is_some_and(|c| c.is_empty());
                a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal).then(b_empty.cmp(&a_empty))
            })
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        let best_token = &slot.candidates[max_idx];
        if !best_token.is_empty() {
            consensus_words.push(best_token.clone());
            total_posterior += slot.posteriors[max_idx];
        }
        slot_count += 1;
    }
    let consensus_text = consensus_words.join(" ");
    if slot_count == 0 || consensus_text.trim().is_empty() {
        return None;
    }
    Some((consensus_text, total_posterior / slot_count as f64))
}

/// One word of the offline consensus DRAFT, with how strongly the models agreed on it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsensusWord {
    /// The winning token (ability-weighted vote across the aligned hypotheses).
    pub text: String,
    /// 0..1 weighted share of model "mass" on the winner — 1.0 = unanimous; low = the models disagreed
    /// here (the review UI highlights low-agreement words so the eye lands on likely errors first).
    pub agreement: f64,
    /// How many distinct models produced the winning token (for a plain "2/3"-style readout).
    pub models_agreeing: usize,
    /// Total distinct models that voted on this slot.
    pub total_models: usize,
    /// The other tokens the remaining models produced here (what each alternative said).
    pub alternatives: Vec<String>,
}

/// Ability-weighted vote weight for a model. `exp2(ability)` makes the strongest model (OmniASR 7B,
/// ability 1.5 → 2.83) outweigh the two architecturally-KIN CTC models combined (0.5 → 1.41 and
/// −0.5 → 0.71, summing 2.12) so a correlated CTC-pair error can't override the 7B; yet two
/// independent agreements still beat a lone outlier.
fn model_vote_weight(model_id: &str) -> f64 {
    get_initial_ability(model_id).exp2()
}

/// Build an offline best-of-N consensus DRAFT for ONE segment's hypotheses, word by word, with a
/// per-word agreement signal. No cloud, no EM — an ability-weighted vote over the same confusion
/// network the IRT gate uses. Empty (deletion-consensus) slots are dropped from the draft.
pub fn segment_consensus_words(hypotheses: &[SegmentHypothesis]) -> Vec<ConsensusWord> {
    let refs: Vec<&SegmentHypothesis> = hypotheses.iter().collect();
    let Some((slots, _anchor)) = build_confusion_slots(&refs) else {
        return Vec::new();
    };
    let total_models = {
        let mut ids = std::collections::HashSet::new();
        for slot in &slots {
            for obs in &slot.observations {
                ids.insert(obs.model_id.as_str());
            }
        }
        ids.len().max(1)
    };

    let mut out = Vec::new();
    for slot in &slots {
        let mut tally: HashMap<&str, (f64, usize)> = HashMap::new();
        for obs in &slot.observations {
            let entry = tally.entry(obs.observed_token.as_str()).or_insert((0.0, 0));
            entry.0 += model_vote_weight(&obs.model_id);
            entry.1 += 1;
        }
        if tally.is_empty() {
            continue;
        }
        let total_weight: f64 = tally.values().map(|(w, _)| *w).sum();
        // Pick the winner DETERMINISTICALLY (sorting, not HashMap::max_by which is randomized per
        // process and last-wins on ties — that flipped the draft between runs). The empty (deletion)
        // token is pushed LAST when any real word exists, so a word that ties a peer's deletion is
        // KEPT (and flagged contested), never silently dropped.
        let any_non_empty = tally.keys().any(|t| !t.is_empty());
        let mut candidates: Vec<(&str, f64, usize)> = tally.iter().map(|(t, (w, c))| (*t, *w, *c)).collect();
        candidates.sort_by(|a, b| {
            let a_demoted = a.0.is_empty() && any_non_empty;
            let b_demoted = b.0.is_empty() && any_non_empty;
            a_demoted
                .cmp(&b_demoted)
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)) // higher weight first
                .then(b.2.cmp(&a.2)) // then more models
                .then(a.0.cmp(b.0)) // then token, for a fully stable order
        });
        let (winner, winner_weight, winner_count) = candidates[0];
        if winner.is_empty() {
            continue; // every model deleted here — omit the word from the draft
        }
        let agreement = if total_weight > 0.0 { winner_weight / total_weight } else { 0.0 };
        let alternatives: Vec<String> = candidates
            .iter()
            .filter(|(t, _, _)| *t != winner && !t.is_empty())
            .map(|(t, _, _)| t.to_string())
            .collect();
        out.push(ConsensusWord {
            text: winner.to_string(),
            agreement,
            models_agreeing: winner_count,
            total_models,
            alternatives,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(candidates: &[&str], posteriors: &[f64]) -> Slot {
        Slot {
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            observations: Vec::new(),
            posteriors: posteriors.to_vec(),
        }
    }

    #[test]
    fn empty_priors_match_heuristic_and_priors_warm_start_seeding() {
        let hyps = vec![
            SegmentHypothesis {
                segment_id: "s1".into(),
                model_id: "omniasr-ctc-300m".into(),
                transcript: "ئەمە دەنگە".into(),
                confidence: Some(0.8),
            },
            SegmentHypothesis {
                segment_id: "s1".into(),
                model_id: "custom-x".into(),
                transcript: "ئەمە دەنگ".into(),
                confidence: Some(0.7),
            },
        ];
        // Backward-compat: the wrapper == the empty-priors variant, so the default gate is unchanged.
        let a = fit_irt_consensus(&hyps);
        let b = fit_irt_consensus_with_priors(&hyps, &HashMap::new());
        assert_eq!(a.model_abilities, b.model_abilities);
        assert!(!a.abilities_were_fit, "one segment (<10) must NOT fit abilities");
        let heuristic = *a.model_abilities.get("custom-x").expect("custom-x present");
        // Warm-start: a stored prior seeds the ability (single segment ⇒ EM doesn't update ⇒ stays the seed).
        let priors = HashMap::from([("custom-x".to_string(), 2.5)]);
        let c = fit_irt_consensus_with_priors(&hyps, &priors);
        let warm = *c.model_abilities.get("custom-x").expect("custom-x present");
        assert!((warm - 2.5).abs() < 1e-9, "warm-start must seed custom-x from the stored 2.5, got {warm}");
        assert!((warm - heuristic).abs() > 1e-6, "warm-started ability must differ from the heuristic seed");
    }

    #[test]
    fn deletion_slots_penalize_confidence_instead_of_inflating_it() {
        // A full 2-word consensus: both slots emit, confidence = mean posterior.
        let full = consensus_from_slots(&[slot(&["ئەمە", ""], &[0.9, 0.1]), slot(&["دەنگە", ""], &[0.8, 0.2])]);
        let (full_text, full_conf) = full.expect("full consensus must be scored");
        assert_eq!(full_text, "ئەمە دەنگە");
        assert!((full_conf - 0.85).abs() < 1e-9, "full confidence = (0.9+0.8)/2: {full_conf}");

        // A TRUNCATED consensus: slot 1 emits (0.9) but slot 2 is won by a HIGH-posterior deletion (0.95).
        // The deletion must NOT inflate confidence — it counts only in the denominator. So confidence is
        // 0.9/2 = 0.45, NOT (0.9+0.95)/2 = 0.925. This is the exact bug: a cut-off transcript scoring near
        // the same as a full one, which let the T0 gate auto-accept it.
        let trunc = consensus_from_slots(&[slot(&["ئەمە", ""], &[0.9, 0.1]), slot(&["دەنگە", ""], &[0.05, 0.95])]);
        let (trunc_text, trunc_conf) = trunc.expect("truncated consensus must still be scored");
        assert_eq!(trunc_text, "ئەمە", "the high-posterior deletion truncates the trailing word");
        assert!((trunc_conf - 0.45).abs() < 1e-9, "deletion posterior must not inflate confidence: {trunc_conf}");
        assert!(trunc_conf < full_conf, "a truncated consensus must score lower than the full one");
    }

    #[test]
    fn all_deletion_consensus_is_omitted() {
        // Every slot won by "" -> empty consensus -> None (omitted, not a fabricated score).
        assert!(consensus_from_slots(&[slot(&["x", ""], &[0.1, 0.9]), slot(&["y", ""], &[0.2, 0.8])]).is_none());
        assert!(consensus_from_slots(&[]).is_none());
    }

    #[test]
    fn gate_consensus_keeps_a_word_over_a_tied_deletion_like_the_draft() {
        // The GATE (consensus_from_slots) must break a word-vs-deletion posterior TIE the same way the
        // review DRAFT (segment_consensus_words) does — KEEP the word — so an auto-accepted consensus can
        // never silently truncate a word the human's draft kept. `build_confusion_slots` seeds candidates
        // as `[anchor_word, ""]`, so "" sits at the later index; before the tie-break fix, max_by's
        // last-maximal rule picked "" here and the whole consensus collapsed to None (all-deletion).
        let (text, conf) = consensus_from_slots(&[slot(&["ئەمە", ""], &[0.5, 0.5])])
            .expect("a word tying a deletion must yield a consensus, not an all-deletion None");
        assert_eq!(text, "ئەمە", "the gate must KEEP a word that ties a deletion, matching the draft");
        assert!((conf - 0.5).abs() < 1e-9, "the kept word contributes its 0.5 posterior over 1 slot");
    }

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
    fn segment_consensus_draft_votes_by_ability_and_flags_disagreement() {
        // Anchor = the 7B (ability 1.5). On word 2 it says "دەنگە" while the two architecturally-KIN
        // CTC models both say "ڕەنگە". The 7B must still win the draft (exp2-weighted 2.83 > 1.41+0.71),
        // and that word must be flagged lower-agreement so review highlights it first.
        let h = |m: &str, t: &str| SegmentHypothesis {
            segment_id: "s".to_string(),
            model_id: m.to_string(),
            transcript: t.to_string(),
            confidence: None,
        };
        let hyps = vec![
            h("omniasr-wsl-7b", "ئەمە دەنگە"),
            h("omniasr-ctc-1b", "ئەمە ڕەنگە"),
            h("omniasr-ctc-300m", "ئەمە ڕەنگە"),
        ];
        let words = segment_consensus_words(&hyps);
        let draft: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(draft, vec!["ئەمە", "دەنگە"], "7B wins the kin-pair slot");
        assert!((words[0].agreement - 1.0).abs() < 1e-9, "unanimous first word is full agreement");
        assert!(words[1].agreement < 0.7, "contested word is lower-agreement: {}", words[1].agreement);
        assert_eq!(words[1].models_agreeing, 1, "only the 7B produced the winning token");
        assert!(words[1].alternatives.contains(&"ڕەنگە".to_string()), "alternative shows what the CTC pair said");
    }

    #[test]
    fn segment_consensus_is_deterministic_and_keeps_a_word_over_a_tied_deletion() {
        let h = |m: &str, t: &str| SegmentHypothesis {
            segment_id: "s".to_string(),
            model_id: m.to_string(),
            transcript: t.to_string(),
            confidence: None,
        };
        // Both models are ability 0.0 (weight exp2(0)=1.0). The anchor (whisper-x, last in the ability
        // tie) keeps "alpha"; scribe-v2 deletes it -> a 1.0-vs-1.0 tie on that slot. The word must be
        // KEPT (an empty deletion token never wins over a real word), and the draft must be IDENTICAL
        // across many runs (the old HashMap::max_by was randomized per process and flipped the result).
        let hyps = vec![h("scribe-v2", "shared"), h("whisper-x", "shared alpha")];
        let first: Vec<String> = segment_consensus_words(&hyps).into_iter().map(|w| w.text).collect();
        assert_eq!(
            first,
            vec!["shared".to_string(), "alpha".to_string()],
            "a real word ties a deletion -> keep the word"
        );
        for _ in 0..50 {
            let again: Vec<String> = segment_consensus_words(&hyps).into_iter().map(|w| w.text).collect();
            assert_eq!(again, first, "consensus draft must be deterministic across runs");
        }
    }

    #[test]
    fn all_empty_segment_is_omitted_not_given_perfect_confidence() {
        // When EVERY model returns a blank/whitespace transcript the segment has no signal. The empty-
        // hypothesis filter drops them all, build_confusion_slots returns None, and the segment is omitted
        // entirely — never emitted with a fabricated confidence 1.0 (which let the T0 gate auto-accept an
        // EMPTY transcript at perfect confidence). With no IRT confidence the gate escalates and falls
        // back to the raw transcript.
        let blank_strong = SegmentHypothesis {
            segment_id: "segX".to_string(),
            model_id: "omniasr-7b".to_string(), // ability 1.5 -> would be the anchor
            transcript: "   ".to_string(),      // whitespace-only
            confidence: Some(0.9),
        };
        let blank_weak = SegmentHypothesis {
            segment_id: "segX".to_string(),
            model_id: "omniasr-300m".to_string(), // ability -0.5
            transcript: "".to_string(),
            confidence: Some(0.5),
        };
        let res = fit_irt_consensus(&[blank_strong, blank_weak]);
        assert!(
            !res.segment_confidences.contains_key("segX"),
            "all-empty segment must be omitted, never given a fabricated confidence"
        );
        assert!(
            !res.consensus_transcripts.contains_key("segX"),
            "no empty consensus transcript should be emitted for an all-empty segment"
        );
    }

    #[test]
    fn empty_anchor_recovers_consensus_from_the_surviving_model_at_sub_max_confidence() {
        // The highest-ability model returns an empty transcript: without the input filter it became the
        // anchor, yielding zero slots and a hard-coded confidence of 1.0 with an EMPTY consensus — which
        // could auto-accept an empty transcript at the T0 gate. The filter drops the empty model, so the
        // anchor is the surviving real model: the segment is RECOVERED (a real consensus the human can
        // check) rather than omitted, and a degenerate segment never reports maximal confidence.
        let empty_strong = SegmentHypothesis {
            segment_id: "seg1".to_string(),
            model_id: "omniasr-ctc-1b".to_string(), // initial ability 0.5 — would be the anchor
            transcript: "".to_string(),
            confidence: Some(0.5),
        };
        let real_weak = SegmentHypothesis {
            segment_id: "seg1".to_string(),
            model_id: "omniasr-ctc-300m".to_string(), // initial ability -0.5
            transcript: "ئەمە دەنگە".to_string(),
            confidence: Some(0.4),
        };
        let res = fit_irt_consensus(&[empty_strong, real_weak]);
        let consensus = res.consensus_transcripts.get("seg1").expect("segment present");
        assert!(!consensus.is_empty(), "consensus must come from the non-empty hypothesis, not the empty anchor");
        let conf = res.segment_confidences.get("seg1").copied().expect("confidence present");
        assert!(conf < 1.0, "a segment with an empty strong model must not report maximal confidence; got {conf}");
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
