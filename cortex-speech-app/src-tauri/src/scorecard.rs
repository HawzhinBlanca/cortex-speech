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
    /// Macro (per-segment-mean) WER over UNCLAMPED per-segment rates — short clips with
    /// many insertions push it above the (length-weighted) micro WER, which a reviewer
    /// expects to see rather than have hidden by clamping.
    pub macro_wer: f64,
    /// Aggregate word-error decomposition over all segments.
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
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
    /// Human-readable description of any per-condition SLICE on which the challenger REGRESSES vs the
    /// baseline (e.g. a length class), even though the aggregate improved. A non-empty list blocks
    /// promotion: shipping a model that is better on average but worse on a slice trades one
    /// population's accuracy for another. Empty = no slice regressed (or too little data to slice).
    #[serde(default)]
    pub slice_regressions: Vec<String>,
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

/// Aggregate the per-segment word-error decomposition: the macro (per-segment-mean)
/// UNCLAMPED WER plus total substitutions/deletions/insertions across all segments.
fn word_breakdown_aggregate(result: &EvalRunResult) -> (f64, usize, usize, usize) {
    let (mut subs, mut dels, mut ins, mut rate_sum) = (0usize, 0usize, 0usize, 0.0f64);
    for s in &result.segments {
        let bd = wer::word_error_breakdown(&s.reference, &s.hypothesis);
        subs += bd.substitutions;
        dels += bd.deletions;
        ins += bd.insertions;
        rate_sum += bd.rate();
    }
    let macro_wer = if result.segments.is_empty() { 0.0 } else { rate_sum / result.segments.len() as f64 };
    (macro_wer, subs, dels, ins)
}

/// Build a scorecard from a gold-eval result (and an optional baseline run).
pub fn build_scorecard(
    result: &EvalRunResult,
    baseline: Option<&EvalRunResult>,
    opts: ScorecardOptions,
) -> Scorecard {
    let word_errs = word_errors(result);
    let char_errs = char_errors(result);
    let (macro_wer, substitutions, deletions, insertions) = word_breakdown_aggregate(result);

    let system = SystemScore {
        model_id: result.run.model_id.clone(),
        num_segments: result.segments.len(),
        micro_wer: micro_rate(&word_errs),
        micro_cer: micro_rate(&char_errs),
        macro_wer,
        substitutions,
        deletions,
        insertions,
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
/// Utterance-length difficulty slice — a per-condition slice that needs no extra gold metadata.
fn length_slice(reference: &str) -> &'static str {
    match reference.split_whitespace().count() {
        0..=4 => "short (≤4 words)",
        5..=15 => "medium (5–15 words)",
        _ => "long (>15 words)",
    }
}

fn compare_to_baseline(system: &EvalRunResult, baseline: &EvalRunResult) -> BaselineComparison {
    let base_by_id: HashMap<&str, &crate::eval::EvalSegmentResult> =
        baseline.segments.iter().map(|s| (s.gold_id.as_str(), s)).collect();

    let mut sys_errs = Vec::new();
    let mut base_errs = Vec::new();
    // Per-condition slice accumulation (challenger vs baseline word errors, grouped by slice).
    let mut by_slice: std::collections::BTreeMap<&'static str, (Vec<SegmentError>, Vec<SegmentError>)> =
        std::collections::BTreeMap::new();
    for s in &system.segments {
        if let Some(b) = base_by_id.get(s.gold_id.as_str()) {
            let se = word_error(&s.reference, &s.hypothesis);
            let be = word_error(&b.reference, &b.hypothesis);
            sys_errs.push(se);
            base_errs.push(be);
            let slot = by_slice.entry(length_slice(&s.reference)).or_default();
            slot.0.push(se);
            slot.1.push(be);
        }
    }

    let system_micro_wer = micro_rate(&sys_errs);
    let baseline_micro_wer = micro_rate(&base_errs);
    let p = mapsswe(&sys_errs, &base_errs);
    let significant = p < 0.05;

    // A challenger that beats the aggregate must not REGRESS a slice with enough data to be meaningful.
    const MIN_SLICE_SEGS: usize = 5;
    const SLICE_REGRESSION_TOL: f64 = 0.05; // absolute WER
    let mut slice_regressions = Vec::new();
    for (name, (s_errs, b_errs)) in &by_slice {
        if s_errs.len() < MIN_SLICE_SEGS {
            continue; // too few to slice without noise
        }
        let s_wer = micro_rate(s_errs);
        let b_wer = micro_rate(b_errs);
        if s_wer > b_wer + SLICE_REGRESSION_TOL {
            slice_regressions.push(format!(
                "{name}: challenger WER {s_wer:.4} vs baseline {b_wer:.4} over {} segments",
                s_errs.len()
            ));
        }
    }

    BaselineComparison {
        baseline_model_id: baseline.run.model_id.clone(),
        paired_segments: sys_errs.len(),
        baseline_micro_wer,
        system_micro_wer,
        mapsswe_p_value: p,
        significant_at_05: significant,
        beats_baseline: significant && system_micro_wer < baseline_micro_wer,
        slice_regressions,
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
    out.push_str(&format!("| **WER** (macro) | {} | — |\n", pct(s.macro_wer)));
    out.push_str(&format!(
        "\nWord errors: **{}** substitutions · **{}** deletions · **{}** insertions\n",
        s.substitutions, s.deletions, s.insertions
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

/// "Annotation drift": how much human reviewers had to change the raw ASR output.
///
/// The human-corrected transcript is the reference and the raw ASR transcript the
/// hypothesis, so a *low* drift WER means the model needed little correction (high
/// quality) and a *high* drift means heavy human editing. Unlike the gold scorecard this
/// needs no held-out eval run — it reads directly off the dataset the user is already
/// curating, and like the gold scorecard it carries bootstrap CIs so the figure is
/// defensible rather than a bare average. Deterministic given the seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationDriftScorecard {
    /// Number of segments carrying a non-empty human annotation (the basis of the figure).
    pub num_segments: usize,
    pub micro_wer: f64,
    pub micro_cer: f64,
    pub wer_ci: ConfidenceInterval,
    pub cer_ci: ConfidenceInterval,
    pub bootstrap_resamples: usize,
    pub confidence: f64,
    pub seed: u64,
}

/// Build the annotation-drift scorecard from the dataset's segments. Segments without a
/// (non-blank) human annotation are skipped — they have no reference to score against.
pub fn annotation_drift_scorecard(
    segments: &[crate::db::SpeechSegment],
    opts: ScorecardOptions,
) -> AnnotationDriftScorecard {
    let mut word_errs = Vec::new();
    let mut char_errs = Vec::new();
    for seg in segments {
        let reference = match seg.annotated_transcript.as_deref() {
            Some(r) if !r.trim().is_empty() => r,
            _ => continue,
        };
        word_errs.push(word_error(reference, &seg.raw_transcript));
        char_errs.push(char_error(reference, &seg.raw_transcript));
    }

    AnnotationDriftScorecard {
        num_segments: word_errs.len(),
        micro_wer: micro_rate(&word_errs),
        micro_cer: micro_rate(&char_errs),
        wer_ci: bootstrap_ci(&word_errs, opts.bootstrap_resamples, opts.confidence, opts.seed),
        // Vary the seed for CER so its interval is not an artifact of the WER resampling.
        cer_ci: bootstrap_ci(&char_errs, opts.bootstrap_resamples, opts.confidence, opts.seed ^ 0x9E37_79B9),
        bootstrap_resamples: opts.bootstrap_resamples,
        confidence: opts.confidence,
        seed: opts.seed,
    }
}

/// The verdict of the gold regression gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionGateResult {
    pub passed: bool,
    pub reasons: Vec<String>,
}

/// Decide whether `candidate` REGRESSES against a frozen `baseline` scorecard on the gold set. A
/// metric fails only if its increase exceeds the candidate's bootstrap CI half-width — i.e. the
/// regression is beyond measurement noise, not an unlucky wobble. Both micro-WER and micro-CER are
/// gated (CER is the Sorani-primary metric, WER secondary). This is the gate that, once a frozen
/// gold set + a checked-in baseline scorecard exist, makes a worse pipeline change un-mergeable:
/// wire it into the de-`#[ignore]`'d gold_wer_eval test as a PR-blocking assertion.
pub fn check_gold_regression(candidate: &Scorecard, baseline: &Scorecard) -> RegressionGateResult {
    let mut reasons = Vec::new();
    let mut passed = true;

    let half_width = |ci: &crate::significance::ConfidenceInterval| ((ci.upper - ci.lower) / 2.0).max(0.0);

    let wer_band = half_width(&candidate.system.wer_ci);
    let wer_delta = candidate.system.micro_wer - baseline.system.micro_wer;
    if wer_delta > wer_band {
        passed = false;
        reasons.push(format!(
            "WER REGRESSED: {:.4} vs baseline {:.4} (+{:.4} exceeds CI noise band {:.4})",
            candidate.system.micro_wer, baseline.system.micro_wer, wer_delta, wer_band
        ));
    } else {
        reasons.push(format!(
            "WER ok: {:.4} vs baseline {:.4} (change {:+.4} within noise band {:.4})",
            candidate.system.micro_wer, baseline.system.micro_wer, wer_delta, wer_band
        ));
    }

    let cer_band = half_width(&candidate.system.cer_ci);
    let cer_delta = candidate.system.micro_cer - baseline.system.micro_cer;
    if cer_delta > cer_band {
        passed = false;
        reasons.push(format!(
            "CER REGRESSED: {:.4} vs baseline {:.4} (+{:.4} exceeds CI noise band {:.4})",
            candidate.system.micro_cer, baseline.system.micro_cer, cer_delta, cer_band
        ));
    } else {
        reasons.push(format!(
            "CER ok: {:.4} vs baseline {:.4} (change {:+.4} within noise band {:.4})",
            candidate.system.micro_cer, baseline.system.micro_cer, cer_delta, cer_band
        ));
    }

    RegressionGateResult { passed, reasons }
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

    #[test]
    fn gold_regression_gate_blocks_a_worse_change_but_allows_noise() {
        let pairs = [
            ("/a/1.wav", "ساڵی نوێ پیرۆز بێت"),
            ("/a/2.wav", "ئەو لە کوردستان دەژی"),
            ("/a/3.wav", "کتێبەکە زۆر باش بوو"),
        ];
        let perfect: Vec<&str> = pairs.iter().map(|(_, r)| *r).collect();
        let opts = ScorecardOptions::default();

        let baseline = build_scorecard(&eval_with(&pairs, &perfect, "baseline"), None, opts);

        // An identical candidate -> no regression -> PASS.
        let same = build_scorecard(&eval_with(&pairs, &perfect, "cand-same"), None, opts);
        assert!(
            check_gold_regression(&same, &baseline).passed,
            "an identical result must not be flagged as a regression"
        );

        // A clearly worse candidate (every hypothesis wrong) -> regression beyond the CI -> FAIL.
        let garbage = ["نا", "نا", "نا"];
        let worse = build_scorecard(&eval_with(&pairs, &garbage, "cand-worse"), None, opts);
        let verdict = check_gold_regression(&worse, &baseline);
        assert!(!verdict.passed, "a clearly worse change must be blocked: {:?}", verdict.reasons);
        assert!(verdict.reasons.iter().any(|r| r.contains("REGRESSED")), "{:?}", verdict.reasons);
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
        // Distinct audio paths per segment: gold rows are per-file and re-marking the same file is now
        // idempotent, so identical paths would collapse into a single gold row.
        let paths: Vec<String> = (0..12).map(|i| format!("/x{i}.wav")).collect();
        let pairs: Vec<(&str, &str)> = paths.iter().map(|p| (p.as_str(), "one two three four five")).collect();
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
        let paths: Vec<String> = (0..12).map(|i| format!("/x{i}.wav")).collect();
        let pairs: Vec<(&str, &str)> = paths.iter().map(|p| (p.as_str(), "one two three")).collect();
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

    #[test]
    fn scorecard_reports_macro_wer_and_sdi_decomposition() {
        // seg1 perfect; seg2 = "x" → "x y z" (2 insertions). micro is length-weighted
        // (2/5 = 0.4); macro is the per-segment mean of UNCLAMPED rates ((0 + 2.0)/2 = 1.0).
        let result = eval_with(&[("/a.wav", "a b c d"), ("/b.wav", "x")], &["a b c d", "x y z"], "omniasr-ctc-300m");
        let sc = build_scorecard(&result, None, ScorecardOptions::default());

        assert_eq!(sc.system.substitutions, 0);
        assert_eq!(sc.system.deletions, 0);
        assert_eq!(sc.system.insertions, 2);
        assert!((sc.system.micro_wer - 0.4).abs() < 1e-9, "micro was {}", sc.system.micro_wer);
        assert!((sc.system.macro_wer - 1.0).abs() < 1e-9, "macro was {}", sc.system.macro_wer);

        // The decomposition + macro survive into the rendered + serialized artifact.
        let md = render_markdown(&sc);
        assert!(md.contains("macro"));
        assert!(md.contains("**2** insertions"));
        let back: Scorecard = serde_json::from_str(&serde_json::to_string(&sc).unwrap()).unwrap();
        assert_eq!(back.system.insertions, 2);
    }

    fn seg_with(raw: &str, annotated: Option<&str>) -> crate::db::SpeechSegment {
        crate::db::SpeechSegment {
            raw_transcript: raw.to_string(),
            annotated_transcript: annotated.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn annotation_drift_scores_only_annotated_segments() {
        let segs = vec![
            // Human kept the ASR output verbatim → 0 drift.
            seg_with("ئەمە باشە", Some("ئەمە باشە")),
            // One word of two changed by the reviewer → 1 error over 2 reference words.
            seg_with("ئەمە خراپە", Some("ئەمە باشە")),
            // No annotation and blank annotation → both excluded (no reference to score).
            seg_with("هیچ شت", None),
            seg_with("هیچ شت", Some("   ")),
        ];
        let card = annotation_drift_scorecard(&segs, ScorecardOptions::default());

        assert_eq!(card.num_segments, 2, "only the two non-blank annotated segments count");
        // 1 word error over 4 reference words = 0.25 micro WER.
        assert!((card.micro_wer - 0.25).abs() < 1e-9, "micro_wer was {}", card.micro_wer);
        // The bootstrap CI brackets the point estimate and is well-ordered in [0, 1].
        assert!(card.wer_ci.lower <= card.micro_wer && card.micro_wer <= card.wer_ci.upper);
        assert!(card.cer_ci.lower >= 0.0 && card.cer_ci.upper <= 1.0);
    }

    #[test]
    fn annotation_drift_empty_is_zeroed_not_nan() {
        // No annotated segments must yield a finite, zeroed scorecard (no 0/0 = NaN).
        let card = annotation_drift_scorecard(&[seg_with("ئەمە", None)], ScorecardOptions::default());
        assert_eq!(card.num_segments, 0);
        assert_eq!(card.micro_wer, 0.0);
        assert_eq!(card.micro_cer, 0.0);
        assert!(card.micro_wer.is_finite() && card.micro_cer.is_finite());

        // Round-trips through JSON (it is an IPC return type).
        let json = serde_json::to_string(&card).unwrap();
        let back: AnnotationDriftScorecard = serde_json::from_str(&json).unwrap();
        assert_eq!(back.num_segments, 0);
    }
}
