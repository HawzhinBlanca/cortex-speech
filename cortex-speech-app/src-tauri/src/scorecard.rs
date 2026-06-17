//! Publishable ASR scorecard — the synthesis of the gold-eval runner (M3) and the
//! significance layer (M2) into one reproducible artifact.
//!
//! Raw WER/CER numbers are not a defensible claim; a stranger can't tell whether a
//! result is solid or noise. A scorecard makes the claim trustworthy:
//!
//! * corpus-level (micro) WER and CER, each with a **bootstrap confidence interval**;
//! * an optional **MAPSSWE significance test** against a named baseline, with a
//!   `beats_baseline` flag that is true only when the system is both *lower* and
//!   *significantly* lower (p < 0.05) — i.e. it really beats the baseline, not by luck.
//!
//! Everything is deterministic (seeded), so the same eval rows reproduce the same
//! scorecard byte-for-byte — the core requirement of a stranger-reproducible number.

use crate::eval::EvalRunResult;
use crate::significance::{bootstrap_ci, mapsswe, micro_rate, ConfidenceInterval, SegmentError};
use crate::wer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Controls the (deterministic) bootstrap. Defaults: 2000 resamples, 95% CI, seed 0.
#[derive(Debug, Clone, Copy)]
pub struct ScorecardOptions {
    pub bootstrap_resamples: usize,
    pub confidence: f64,
    pub seed: u64,
}

impl Default for ScorecardOptions {
    fn default() -> Self {
        Self { bootstrap_resamples: 2000, confidence: 0.95, seed: 0xC0FFEE }
    }
}

/// One system's accuracy with quantified uncertainty.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemScore {
    pub model_id: String,
    pub num_segments: usize,
    pub micro_wer: f64,
    pub micro_cer: f64,
    pub wer_ci: ConfidenceInterval,
    pub cer_ci: ConfidenceInterval,
}

/// A paired significance comparison against a baseline system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineComparison {
    pub baseline_model_id: String,
    /// Number of segments present in BOTH runs (the valid basis for a paired test).
    pub paired_segments: usize,
    pub baseline_micro_wer: f64,
    pub system_micro_wer: f64,
    pub mapsswe_p_value: f64,
    pub significant_at_05: bool,
    /// True only if the system's paired WER is lower AND the difference is significant.
    pub beats_baseline: bool,
}

/// A complete, reproducible scorecard.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scorecard {
    pub system: SystemScore,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vs_baseline: Option<BaselineComparison>,
    pub bootstrap_resamples: usize,
    pub confidence: f64,
    pub seed: u64,
}

/// Levenshtein edit distance over token slices — the same computation `wer::compute_*`
/// performs, exposing the raw (distance, ref_len) the bootstrap needs.
fn edit_distance<T: Eq>(a: &[T], b: &[T]) -> usize {
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

fn word_error(reference: &str, hypothesis: &str) -> SegmentError {
    let rw = wer::tokenize_words(&wer::normalize_for_metrics(reference));
    let hw = wer::tokenize_words(&wer::normalize_for_metrics(hypothesis));
    SegmentError::new(edit_distance(&rw, &hw) as f64, rw.len() as f64)
}

fn char_error(reference: &str, hypothesis: &str) -> SegmentError {
    let rc = wer::tokenize_chars(&wer::normalize_for_metrics(reference));
    let hc = wer::tokenize_chars(&wer::normalize_for_metrics(hypothesis));
    SegmentError::new(edit_distance(&rc, &hc) as f64, rc.len() as f64)
}

fn word_errors(result: &EvalRunResult) -> Vec<SegmentError> {
    result.segments.iter().map(|s| word_error(&s.reference, &s.hypothesis)).collect()
}

fn char_errors(result: &EvalRunResult) -> Vec<SegmentError> {
    result.segments.iter().map(|s| char_error(&s.reference, &s.hypothesis)).collect()
}

/// Build a scorecard from a gold-eval result (and an optional baseline run).
pub fn build_scorecard(
    result: &EvalRunResult,
    baseline: Option<&EvalRunResult>,
    opts: ScorecardOptions,
) -> Scorecard {
    let word_errs = word_errors(result);
    let char_errs = char_errors(result);

    let system = SystemScore {
        model_id: result.run.model_id.clone(),
        num_segments: result.segments.len(),
        micro_wer: micro_rate(&word_errs),
        micro_cer: micro_rate(&char_errs),
        wer_ci: bootstrap_ci(&word_errs, opts.bootstrap_resamples, opts.confidence, opts.seed),
        // Vary the seed for CER so its interval is not an artifact of the WER resampling.
        cer_ci: bootstrap_ci(&char_errs, opts.bootstrap_resamples, opts.confidence, opts.seed ^ 0x9E37_79B9),
    };

    let vs_baseline = baseline.map(|b| compare_to_baseline(result, b));

    Scorecard {
        system,
        vs_baseline,
        bootstrap_resamples: opts.bootstrap_resamples,
        confidence: opts.confidence,
        seed: opts.seed,
    }
}

/// Paired comparison: align segments by `gold_id`, compute per-segment word errors for
/// both systems on the shared set, and run MAPSSWE. Only the intersection is used —
/// the only statistically valid basis for a paired test.
fn compare_to_baseline(system: &EvalRunResult, baseline: &EvalRunResult) -> BaselineComparison {
    let base_by_id: HashMap<&str, &crate::eval::EvalSegmentResult> =
        baseline.segments.iter().map(|s| (s.gold_id.as_str(), s)).collect();

    let mut sys_errs = Vec::new();
    let mut base_errs = Vec::new();
    for s in &system.segments {
        if let Some(b) = base_by_id.get(s.gold_id.as_str()) {
            sys_errs.push(word_error(&s.reference, &s.hypothesis));
            base_errs.push(word_error(&b.reference, &b.hypothesis));
        }
    }

    let system_micro_wer = micro_rate(&sys_errs);
    let baseline_micro_wer = micro_rate(&base_errs);
    let p = mapsswe(&sys_errs, &base_errs);
    let significant = p < 0.05;

    BaselineComparison {
        baseline_model_id: baseline.run.model_id.clone(),
        paired_segments: sys_errs.len(),
        baseline_micro_wer,
        system_micro_wer,
        mapsswe_p_value: p,
        significant_at_05: significant,
        beats_baseline: significant && system_micro_wer < baseline_micro_wer,
    }
}

fn pct(x: f64) -> String {
    format!("{:.2}%", x * 100.0)
}

/// Render the scorecard as a self-explanatory Markdown table (for README / model card).
pub fn render_markdown(sc: &Scorecard) -> String {
    let s = &sc.system;
    let mut out = String::new();
    out.push_str(&format!("## ASR Scorecard — `{}`\n\n", s.model_id));
    out.push_str(&format!(
        "Held-out gold segments: **{}** · {:.0}% bootstrap CIs ({} resamples, seed `{}`)\n\n",
        s.num_segments,
        sc.confidence * 100.0,
        sc.bootstrap_resamples,
        sc.seed
    ));
    out.push_str("| Metric | Value | 95% CI |\n|---|---|---|\n");
    out.push_str(&format!(
        "| **WER** (micro) | {} | [{}, {}] |\n",
        pct(s.micro_wer),
        pct(s.wer_ci.lower),
        pct(s.wer_ci.upper)
    ));
    out.push_str(&format!(
        "| **CER** (micro) | {} | [{}, {}] |\n",
        pct(s.micro_cer),
        pct(s.cer_ci.lower),
        pct(s.cer_ci.upper)
    ));
    if let Some(b) = &sc.vs_baseline {
        out.push_str(&format!(
            "\n**vs `{}`** ({} paired segments): system WER {} vs baseline {} — MAPSSWE p = {:.4} → {}\n",
            b.baseline_model_id,
            b.paired_segments,
            pct(b.system_micro_wer),
            pct(b.baseline_micro_wer),
            b.mapsswe_p_value,
            if b.beats_baseline {
                "**significantly beats baseline** ✅"
            } else if b.significant_at_05 {
                "significantly different (not better)"
            } else {
                "no significant difference"
            }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::eval::{import_gold_segments, list_gold_segments, run_gold_eval, GoldSegmentInput};

    fn open_mem_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db
    }

    /// Build an eval result by importing gold (audio_path, reference) rows and scoring
    /// the supplied hypotheses through the real eval path.
    fn eval_with(pairs: &[(&str, &str)], hyps: &[&str], model_id: &str) -> EvalRunResult {
        let db = open_mem_db();
        let inputs: Vec<GoldSegmentInput> = pairs
            .iter()
            .map(|(audio, reference)| GoldSegmentInput {
                audio_path: (*audio).to_string(),
                reference: (*reference).to_string(),
                is_holdout: true,
            })
            .collect();
        import_gold_segments(&db, inputs).unwrap();
        let gold = list_gold_segments(&db).unwrap();
        let hypotheses: Vec<(String, String)> =
            gold.iter().zip(hyps).map(|(g, h)| (g.id.clone(), (*h).to_string())).collect();
        run_gold_eval(&db, model_id, hypotheses).unwrap()
    }

    /// Evaluate two hypothesis sets against the SAME gold set, so the `gold_id`s align
    /// for a paired comparison — exactly how a real two-model eval is run.
    fn eval_pair(
        pairs: &[(&str, &str)],
        sys_hyps: &[&str],
        base_hyps: &[&str],
        sys_id: &str,
        base_id: &str,
    ) -> (EvalRunResult, EvalRunResult) {
        let db = open_mem_db();
        let inputs: Vec<GoldSegmentInput> = pairs
            .iter()
            .map(|(audio, reference)| GoldSegmentInput {
                audio_path: (*audio).to_string(),
                reference: (*reference).to_string(),
                is_holdout: true,
            })
            .collect();
        import_gold_segments(&db, inputs).unwrap();
        let gold = list_gold_segments(&db).unwrap();
        let sys_h: Vec<(String, String)> =
            gold.iter().zip(sys_hyps).map(|(g, h)| (g.id.clone(), (*h).to_string())).collect();
        let base_h: Vec<(String, String)> =
            gold.iter().zip(base_hyps).map(|(g, h)| (g.id.clone(), (*h).to_string())).collect();
        let sys = run_gold_eval(&db, sys_id, sys_h).unwrap();
        let base = run_gold_eval(&db, base_id, base_h).unwrap();
        (sys, base)
    }

    #[test]
    fn perfect_system_scores_zero_with_tight_ci() {
        let result = eval_with(
            &[("/a.wav", "the cat sat"), ("/b.wav", "on the mat")],
            &["the cat sat", "on the mat"],
            "perfect",
        );
        let sc = build_scorecard(&result, None, ScorecardOptions::default());
        assert!(sc.system.micro_wer < 1e-9);
        assert!(sc.system.micro_cer < 1e-9);
        assert_eq!(sc.system.wer_ci.lower, 0.0);
        assert_eq!(sc.system.wer_ci.upper, 0.0);
        assert!(sc.vs_baseline.is_none());
    }

    #[test]
    fn ci_brackets_the_point_estimate() {
        let result = eval_with(
            &[("/a.wav", "alpha beta gamma"), ("/b.wav", "delta epsilon zeta"), ("/c.wav", "eta theta iota")],
            &["alpha beta gamma", "delta wrong zeta", "eta theta wrong"],
            "m",
        );
        let sc = build_scorecard(&result, None, ScorecardOptions::default());
        assert!(sc.system.wer_ci.lower <= sc.system.micro_wer + 1e-9);
        assert!(sc.system.micro_wer <= sc.system.wer_ci.upper + 1e-9);
    }

    #[test]
    fn beats_baseline_only_when_lower_and_significant() {
        let pairs: Vec<(&str, &str)> = (0..12)
            .map(|_| ("/x.wav", "one two three four five"))
            .collect();
        // System: perfect on every segment. Baseline: one error on every segment.
        let sys_hyps: Vec<&str> = (0..12).map(|_| "one two three four five").collect();
        let base_hyps: Vec<&str> = (0..12).map(|_| "one two three four WRONG").collect();

        let (sys, base) = eval_pair(&pairs, &sys_hyps, &base_hyps, "candidate", "whisper-baseline");

        let sc = build_scorecard(&sys, Some(&base), ScorecardOptions::default());
        let cmp = sc.vs_baseline.expect("baseline comparison present");
        assert_eq!(cmp.paired_segments, 12);
        assert!(cmp.system_micro_wer < cmp.baseline_micro_wer);
        assert!(cmp.significant_at_05, "a consistent per-segment win must be significant (p={})", cmp.mapsswe_p_value);
        assert!(cmp.beats_baseline);
    }

    #[test]
    fn identical_system_does_not_beat_itself() {
        let pairs: Vec<(&str, &str)> = (0..12).map(|_| ("/x.wav", "one two three")).collect();
        let hyps: Vec<&str> = (0..12).map(|_| "one two WRONG").collect();
        let (a, b) = eval_pair(&pairs, &hyps, &hyps, "a", "b");
        let sc = build_scorecard(&a, Some(&b), ScorecardOptions::default());
        let cmp = sc.vs_baseline.unwrap();
        assert!((cmp.mapsswe_p_value - 1.0).abs() < 1e-9, "identical systems → p = 1");
        assert!(!cmp.beats_baseline);
    }

    #[test]
    fn markdown_is_self_explanatory_and_serializable() {
        let result = eval_with(&[("/a.wav", "the cat sat")], &["the dog sat"], "omniasr-ctc-300m");
        let sc = build_scorecard(&result, None, ScorecardOptions::default());
        let md = render_markdown(&sc);
        assert!(md.contains("omniasr-ctc-300m"));
        assert!(md.contains("WER"));
        assert!(md.contains("CI"));
        // The whole scorecard round-trips through JSON (it is an IPC return type).
        let json = serde_json::to_string(&sc).unwrap();
        let back: Scorecard = serde_json::from_str(&json).unwrap();
        assert_eq!(back.system.model_id, "omniasr-ctc-300m");
    }
}
