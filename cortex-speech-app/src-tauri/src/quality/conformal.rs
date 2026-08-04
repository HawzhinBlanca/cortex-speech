use crate::db::SpeechSegment;
use crate::wer::compute_cer;
use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConformalCertificate {
    pub target_error: f64,
    pub confidence_level: f64,
    pub threshold: f64,
    pub total_certified: usize,
    pub certified_segment_ids: Vec<String>,
    pub expected_error_bound: f64,
    pub is_calibrated: bool,
    /// PROVENANCE of the calibration set's per-utterance confidences (honesty, not interpretation): how
    /// many calibration segments carried a real model posterior vs the heuristic/unknown fallback. On the
    /// default offline path (OmniASR CTC exposes no token posteriors) this is `real_posterior == 0` — the
    /// readout must not imply a calibrated-posterior guarantee it does not have. This does NOT change the
    /// threshold/certification (the score also uses the real ctc_score); it just surfaces the basis.
    pub calibration_real_posterior: usize,
    pub calibration_heuristic: usize,
    /// Clips that would otherwise have calibrated, excluded because they carry NEITHER a confidence nor
    /// a ctc_score. Distinct from `calibration_heuristic` (a non-posterior score IS a score); this is the
    /// absence of any signal, which `has_scoreable_confidence` refuses to invent a 1.0 for.
    ///
    /// Surfaced because the UI must be able to say WHY nothing certified. Without it the panel falls
    /// back to "verify at least 10 segments", which on a library like the owner's — 67 verified clips,
    /// every one from a cloud engine that returns no confidence — is advice that cannot work.
    pub calibration_no_confidence: usize,
}

/// The nonconformity score from a (confidence, ctc) pair: S = (1 - confidence) + 0.1·(-ctc).
///
/// This is the SINGLE source of truth for the score. The T0 gate MUST calibrate its threshold on
/// the same score (and the same confidence source) it gates on, otherwise the conformal coverage
/// guarantee is void. The gate routes on the IRT cross-model consensus confidence (the
/// better-calibrated signal); calibrate_threshold is fed that same IRT-based score.
pub fn nonconformity(confidence: f64, ctc_score: Option<f64>) -> f64 {
    // Clamp confidence into the valid posterior range [0,1]. The local ASR path is always in range,
    // but an external (WSL) script can emit an out-of-range or non-finite confidence; left unclamped,
    // e.g. confidence=92.0 yields a negative term that .max(0.0) floors to 0.0 — a falsely MAXIMAL
    // certainty that silently corrupts the conformal coverage guarantee. NaN ⇒ 0.0 (least certain).
    let confidence = if confidence.is_finite() { confidence.clamp(0.0, 1.0) } else { 0.0 };
    let ctc = ctc_score.unwrap_or(-5.0);
    ((1.0 - confidence) + 0.1 * (-ctc)).max(0.0)
}

/// Nonconformity from a segment's OWN per-utterance confidence (used by the standalone dataset
/// certificate, which is a seg.confidence-based artifact distinct from the IRT-based T0 gate).
pub fn compute_nonconformity_score(seg: &SpeechSegment) -> f64 {
    nonconformity(seg.confidence.unwrap_or(0.5), seg.ctc_score)
}

/// Whether a segment carries ANY measured confidence signal to score.
///
/// `compute_nonconformity_score` defaults a missing confidence to 0.5 and a missing ctc_score to -5.0,
/// which is fine as a per-call fallback and ruinous as a population. A clip with NEITHER scores exactly
/// `(1 - 0.5) + 0.1*5 = 1.0` — a constant manufactured from two defaults, identical for every such clip
/// and carrying no information about any of them.
///
/// That is not hypothetical. `pipeline.rs` says of the cloud STT path: *"Scribe returns no per-segment
/// confidence, so `confidence` stays [None]"*, and on the owner's library ALL 144 clips came from an
/// external provider — 0/144 have a confidence, 0/144 a ctc_score. Every one scored 1.0, no cut point
/// could beat the target, the fallback threshold became that same 1.0, and every non-rejected clip
/// "certified": 117 of them, on a number nobody measured.
///
/// A certificate over fabricated scores is worse than no certificate, so such rows now calibrate
/// nothing and certify nothing. They are not errors and not excluded from the library — they simply
/// carry no evidence, and the honest count of what a no-confidence dataset certifies is zero.
pub fn has_scoreable_confidence(seg: &SpeechSegment) -> bool {
    seg.confidence.is_some() || seg.ctc_score.is_some()
}

/// Number of acoustic-condition (SNR) buckets the conformal threshold is calibrated within. A single
/// global threshold is invalid across studio/field/noisy conditions, so the T0 gate calibrates a
/// separate threshold per bucket (sparse buckets fall back to the global one).
pub const N_SNR_BUCKETS: usize = 5;

/// Map an SNR (dBFS) to a calibration bucket: 0 = <5 dB (very noisy), 1 = 5–15, 2 = 15–25,
/// 3 = >25 dB (clean), 4 = unknown (no SNR measured). Also used as the per-condition slice key.
pub fn snr_bucket(snr_db: Option<f64>) -> usize {
    match snr_db {
        None => 4,
        Some(s) if s < 5.0 => 0,
        Some(s) if s < 15.0 => 1,
        Some(s) if s < 25.0 => 2,
        Some(_) => 3,
    }
}

/// Calibrate ONLY the conformal threshold from pre-scored calibration items `(nonconformity, cer)`.
/// The caller supplies the exact score it will gate on, so the Hoeffding bound and the threshold are
/// valid for that score. Returns `(threshold, expected_error_bound, is_calibrated)`.
///
/// Same statistics as calibrate_and_certify: tie-group-boundary cutoffs (so the certified prefix
/// equals the bound's prefix) and a Bonferroni `ln(n/delta)` multiplicity correction.
pub fn calibrate_threshold(scored: &[(f64, f64)], target_error: f64, confidence_level: f64) -> (f64, f64, bool) {
    let delta = (1.0 - confidence_level).max(1e-5);
    if scored.len() < 10 {
        // Conservative cold-start: too little verified data for a guarantee.
        return (0.35, target_error, false);
    }
    let mut sorted: Vec<(f64, f64)> = scored.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let (mut best_k, mut best_threshold, mut best_bound) = (0usize, 0.0, 1.0);
    for k in 1..=n {
        if k < n && sorted[k].0 == sorted[k - 1].0 {
            continue; // only cut at a tie-group boundary
        }
        let empirical_risk = sorted.iter().take(k).map(|x| x.1).sum::<f64>() / k as f64;
        let bound = empirical_risk + ((n as f64 / delta).ln() / (2.0 * k as f64)).sqrt();
        if bound <= target_error {
            best_k = k;
            best_threshold = sorted[k - 1].0;
            best_bound = bound;
        }
    }
    if best_k == 0 {
        // Nothing certifiable at target; fall back to the single best item, flagged uncalibrated.
        return (sorted[0].0, 1.0, false);
    }
    (best_threshold, best_bound, true)
}

/// The MINIMUM calibration-set size at which the Bonferroni-corrected Hoeffding bound can reach
/// `target_error` even with PERFECT data (empirical risk 0): the smallest n satisfying
/// `sqrt(ln(n/delta) / (2n)) <= target_error`, via the fixed point `n = ln(n/delta) / (2t²)`.
///
/// Why this exists (true-10 audit 2026-07-09): at the shipped T0 constants (target 5% CER,
/// per-bucket delta 0.02) this is ≈2,334 perfectly-transcribed verified clips PER SNR BUCKET —
/// the gate is mathematically unreachable at a single user's realistic volumes, and nothing said
/// so. The intelligence report now surfaces this distance honestly instead of the user just
/// experiencing "the jury escalates everything" with no visible reason. This function deliberately
/// does NOT loosen anything — the gate's conservatism is correct; its opacity was the bug.
pub fn min_calibration_n(target_error: f64, delta: f64) -> usize {
    if target_error <= 0.0 {
        return usize::MAX;
    }
    let delta = delta.max(1e-9);
    let t2 = 2.0 * target_error * target_error;
    // Fixed-point iteration; converges in a handful of steps for every realistic input.
    let mut n = 100.0_f64;
    for _ in 0..64 {
        let next = ((n / delta).ln() / t2).max(10.0);
        if (next - n).abs() < 0.5 {
            n = next;
            break;
        }
        n = next;
    }
    n.ceil() as usize
}

/// Calibrates the conformal threshold using verified segments as the calibration set.
/// If there are fewer than 10 verified segments, it falls back to a default heuristic.
pub fn calibrate_and_certify(
    all_segments: &[SpeechSegment],
    target_error: f64,     // e.g., 0.05 for 5% CER
    confidence_level: f64, // e.g., 0.95 for 95% confidence
) -> ConformalCertificate {
    // Build the (nonconformity, cer) calibration set from verified segments with a non-empty
    // reference, scored on the segment's own confidence (this is the seg.confidence-based DATASET
    // certificate; the IRT-based T0 gate calibrates separately via calibrate_threshold).
    // Provenance of the calibration confidences — a fact surfaced for honesty, computed over the SAME
    // membership rule as cal_scored (verified + non-empty reference). `real_posterior` requires the exact
    // token stored by asr::ConfidenceSource; anything else (heuristic, legacy `unknown`, or missing) is
    // the fallback.
    let mut calibration_real_posterior = 0usize;
    let mut calibration_heuristic = 0usize;
    let mut calibration_no_confidence = 0usize;
    let cal_scored: Vec<(f64, f64)> = all_segments
        .iter()
        .filter_map(|s| {
            // 'mark bad' sets verified=true (to leave the review queue) and KEEPS the machine draft as
            // annotated_transcript, so a human-REJECTED clip satisfies the verified+non-empty-reference
            // membership. It must NOT calibrate the certificate (its ~0 CER tightens the error bound over
            // data the human discarded) — matching every sibling gate (export/export_bundle/jury) and the
            // db.rs C3 count, which apply this exact exclusion. (round-25 hunt: conformal was the lone omission.)
            if !s.verified || crate::quality::is_human_rejected(s) {
                return None;
            }
            // No measured confidence and no acoustic score => the nonconformity would be a constant
            // manufactured from two defaults. Calibrating on that is calibrating on nothing.
            if !has_scoreable_confidence(s) {
                calibration_no_confidence += 1;
                return None;
            }
            let ref_text = s.annotated_transcript.as_deref()?.trim();
            if ref_text.is_empty() {
                return None;
            }
            if s.confidence_source.as_deref() == Some("real_posterior") {
                calibration_real_posterior += 1;
            } else {
                calibration_heuristic += 1;
            }
            let cer = compute_cer(ref_text, &s.raw_transcript).min(1.0); // bound to [0,1] for Hoeffding
            Some((compute_nonconformity_score(s), cer))
        })
        .collect();

    let cal_n = cal_scored.len();
    let (threshold, bound, is_calibrated) = calibrate_threshold(&cal_scored, target_error, confidence_level);

    // Certify every segment whose nonconformity is at or below the calibrated threshold — but NEVER a
    // human-rejected clip: 'mark bad' keeps verified=true + the draft, so it would otherwise pass the
    // nonconformity gate and be vouched as good over the reviewer's explicit rejection.
    let certified_segment_ids: Vec<String> = all_segments
        .iter()
        .filter(|seg| {
            // Same rule on the certify side: a clip with no confidence signal cannot be vouched for by
            // a threshold, because the score it would be compared against was invented, not measured.
            !crate::quality::is_human_rejected(seg)
                && has_scoreable_confidence(seg)
                && compute_nonconformity_score(seg) <= threshold
        })
        .map(|seg| seg.id.clone())
        .collect();

    let expected_error_bound = if is_calibrated {
        bound
    } else if cal_n < 10 {
        target_error // nominal under cold-start
    } else {
        1.0 // had enough data but nothing certifiable at target
    };

    ConformalCertificate {
        target_error,
        confidence_level,
        threshold,
        total_certified: certified_segment_ids.len(),
        certified_segment_ids,
        expected_error_bound,
        is_calibrated,
        calibration_real_posterior,
        calibration_heuristic,
        calibration_no_confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn mock_segment(id: &str, confidence: f64, ctc: f64, verified: bool, raw: &str, ann: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: "".to_string(),
            raw_transcript: raw.to_string(),
            normalized_transcript: None,
            annotated_transcript: Some(ann.to_string()),
            alignment_json: None,
            duration_ms: 1000,
            speaker_id: None,
            verified,
            confidence: Some(confidence),
            ctc_score: Some(ctc),
            clipping_ratio: None,
            rms_db: None,
            snr_db: None,
            split: None,
            signal_anomaly_score: None,
            ..SpeechSegment::default()
        }
    }

    #[test]
    fn certificate_discloses_calibration_confidence_provenance() {
        // Honesty/provenance: the certificate must surface whether its calibration confidences were real
        // model posteriors or the heuristic/unknown fallback (the default offline CTC path has none), so
        // the readout cannot imply a calibrated-posterior guarantee it does not have. This is a FACT over
        // the calibration-set membership (verified + non-empty reference), not a behavior change.
        let mut real = mock_segment("r", 0.9, -1.0, true, "text", "text");
        real.confidence_source = Some("real_posterior".to_string());
        let mut heur = mock_segment("h", 0.9, -1.0, true, "text", "text");
        heur.confidence_source = Some("heuristic".to_string());
        let unknown = mock_segment("u", 0.9, -1.0, true, "text", "text"); // confidence_source None (legacy)
        let unverified = mock_segment("x", 0.9, -1.0, false, "text", "text"); // NOT in the calibration set

        let cert = calibrate_and_certify(&[real, heur, unknown, unverified], 0.3, 0.90);
        assert_eq!(cert.calibration_real_posterior, 1, "only the real_posterior segment counts as real");
        assert_eq!(
            cert.calibration_heuristic, 2,
            "heuristic + legacy-unknown are the fallback; the unverified segment is excluded entirely"
        );
    }

    #[test]
    fn human_rejected_clips_never_pollute_or_get_certified() {
        // Round-25 hunt: 'mark bad' sets verified=true and KEEPS the machine draft as
        // annotated_transcript (verdict='human_reject'), so a rejected clip satisfies the
        // verified+non-empty-reference membership. It must NOT enter the calibration set (its ~0 CER
        // would tighten the error bound over discarded data) NOR be certified as good — matching every
        // sibling gate. conformal was the lone path that omitted is_human_rejected.
        let good = mock_segment("g", 0.9, -1.0, true, "text", "text");
        let mut rejected = mock_segment("bad", 0.9, -1.0, true, "text", "text");
        rejected.verdict = Some("human_reject".to_string());

        let cert = calibrate_and_certify(&[good, rejected], 0.3, 0.90);
        assert!(
            !cert.certified_segment_ids.iter().any(|id| id == "bad"),
            "a human-rejected clip must never be certified as good: {:?}",
            cert.certified_segment_ids
        );
        assert_eq!(
            cert.calibration_real_posterior + cert.calibration_heuristic,
            1,
            "only the one good clip is in the calibration set; the rejected clip is excluded"
        );
    }

    #[test]
    fn a_dataset_with_no_confidence_data_cannot_certify_anything() {
        // The owner's live library on 2026-08-03: `confidence` and `ctc_score` NULL on all 144 rows.
        // Both defaults then apply to EVERY segment — nonconformity((1-0.5) + 0.1*5) = 1.0 — so the
        // score carries no per-segment information at all. Every clip is indistinguishable.
        //
        // What that did to the dashboard: no cut point can beat the target (the Hoeffding slack alone
        // is ~0.29 at n=40 against a 5% target), so `best_k == 0`, the fallback threshold becomes that
        // same 1.0, and every non-rejected segment passes it. 144 - 27 rejects = 117 rows rendered as
        // "Certified Segments" in success-cyan. That number was the count of non-rejected segments
        // wearing a statistical word.
        let flat: Vec<SpeechSegment> = (0..40)
            .map(|i| {
                let mut s = mock_segment(&format!("s{i}"), 0.9, -1.0, true, "ئەمڕۆ هەوا خۆشە", "ئەمڕۆ هەوا زۆر خۆشە");
                s.confidence = None; // <- the whole point: no posterior, no acoustic score
                s.ctc_score = None;
                s
            })
            .collect();

        let scores: Vec<f64> = flat.iter().map(compute_nonconformity_score).collect();
        assert!(
            scores.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-12),
            "every segment must score identically when both inputs are missing: {scores:?}"
        );
        assert!((scores[0] - 1.0).abs() < 1e-12, "the two defaults produce exactly 1.0, got {}", scores[0]);

        let cert = calibrate_and_certify(&flat, 0.05, 0.95);
        assert!(
            !cert.is_calibrated,
            "a degenerate score set must never report itself calibrated (bound {})",
            cert.expected_error_bound
        );
        // CHANGED, not weakened. This assertion used to read `total_certified == flat.len()` and
        // documented the tautology: with every score a manufactured 1.0, the fallback threshold was
        // also 1.0 and all 40 "certified". Hiding that count in the UI treated the symptom.
        // `has_scoreable_confidence` treats the cause: a clip with neither a confidence nor a ctc_score
        // has nothing to score, so it calibrates nothing and certifies nothing. Zero is the honest
        // count of what a no-confidence dataset certifies, and it is now the count the payload carries.
        assert_eq!(
            cert.total_certified, 0,
            "a clip with no measured confidence must not be certified against an invented score"
        );
        assert_eq!(cert.calibration_real_posterior + cert.calibration_heuristic, 0, "and none of it calibrates");
    }

    #[test]
    fn a_scoreable_dataset_still_refuses_to_certify_the_clips_that_have_no_confidence() {
        // The case the all-empty test CANNOT see. There, the calibration guard alone empties the set,
        // `calibrate_threshold` takes its <10 cold-start path and returns 0.35, and the manufactured
        // 1.0 fails that anyway — so removing the CERTIFY-side guard changed nothing and the fail-before
        // passed. A guard that is only ever redundant is not a guard.
        //
        // So: calibrate on 40 real clips whose scores run ABOVE 1.0, at a loose target the Hoeffding
        // slack can actually meet (~0.29 at n=40, hence 0.5 rather than 0.05). The threshold then lands
        // at or above the 1.0 that a no-confidence clip manufactures, and only the certify-side guard
        // keeps such clips out.
        let mut segs: Vec<SpeechSegment> = (0..40)
            .map(|i| {
                // ctc -8.0 => nonconformity (1-0.5) + 0.8 = 1.3, comfortably above the fabricated 1.0.
                let mut s = mock_segment(&format!("real{i}"), 0.5, -8.0, true, "ئەمڕۆ باشە", "ئەمڕۆ باشە");
                s.raw_transcript = "ئەمڕۆ باشە".to_string(); // CER 0 keeps the empirical risk low
                s
            })
            .collect();
        // Two clips from a provider that returns no confidence at all (the owner's whole library).
        for i in 0..2 {
            let mut blind = mock_segment(&format!("blind{i}"), 0.5, -5.0, true, "ئەمڕۆ باشە", "ئەمڕۆ باشە");
            blind.confidence = None;
            blind.ctc_score = None;
            segs.push(blind);
        }

        let cert = calibrate_and_certify(&segs, 0.5, 0.95);
        assert!(cert.is_calibrated, "the 40 real clips must calibrate, or this test proves nothing");
        assert!(
            cert.threshold >= 1.0,
            "the threshold must reach the fabricated score for the guard to be load-bearing (got {})",
            cert.threshold
        );
        for i in 0..2 {
            assert!(
                !cert.certified_segment_ids.contains(&format!("blind{i}")),
                "a clip with no measured confidence certified against an invented score"
            );
        }
        assert_eq!(cert.total_certified, 40, "only the clips that carry a real signal are certified");
    }

    #[test]
    fn nonconformity_clamps_out_of_range_confidence() {
        // Round-10 audit MEDIUM: an external (WSL) script could emit confidence as a percentage (92.0),
        // which UNCLAMPED yields ((1-92)+0.5).max(0)=0.0 — the SMALLEST (most-certain) nonconformity,
        // silently certifying a possibly-wrong segment. Out-of-range confidence must NOT read as maximal
        // certainty: clamp to [0,1] so 92.0 behaves like 1.0, and NaN like 0.0 (least certain).
        let one = nonconformity(1.0, None);
        let pct = nonconformity(92.0, None);
        assert!((one - pct).abs() < 1e-9, "92.0 must clamp to 1.0: {one} vs {pct}");

        let zero_conf = nonconformity(0.0, None);
        let nan = nonconformity(f64::NAN, None);
        assert!((nan - zero_conf).abs() < 1e-9, "NaN confidence must be treated as 0.0 (least certain)");

        // The clamped high-confidence score is genuinely SMALLER than the low-confidence score —
        // i.e. 92.0 no longer masquerades as the most-certain (0.0) score.
        assert!(pct < zero_conf, "higher confidence must yield a lower nonconformity");
        assert!(pct > 0.0, "a clamped 1.0 confidence still carries the ctc term, not a falsely-zero score");
    }

    #[test]
    fn nonconformity_is_the_shared_formula() {
        // The gate and the calibration MUST use the identical formula, or the conformal guarantee is
        // void. compute_nonconformity_score is just nonconformity() applied to the segment's own
        // confidence; the gate feeds it the IRT confidence instead — same function, same shape.
        let seg = mock_segment("x", 0.9, -1.0, true, "a", "a");
        assert!((compute_nonconformity_score(&seg) - nonconformity(0.9, Some(-1.0))).abs() < 1e-12);
        // Threshold calibration over the same scored items the dataset cert builds gives the same cut.
        let scored: Vec<(f64, f64)> = (0..40)
            .map(|i| {
                let conf = if i % 4 == 0 { 0.4 } else { 0.95 };
                let cer = if i % 4 == 0 { 1.0 } else { 0.0 };
                (nonconformity(conf, Some(-1.0)), cer)
            })
            .collect();
        // Target 0.4: 30 clean points give a Bonferroni-corrected bound ~0.316 (sqrt(ln(400)/60)),
        // which certifies at 0.4 but not at a tighter 0.3 — the corrected math, same as the gate.
        let (t, _b, cal) = calibrate_threshold(&scored, 0.4, 0.90);
        assert!(cal, "30 clean spread points should calibrate at target 0.4");
        assert!(t > 0.0);
    }

    #[test]
    fn min_calibration_n_matches_the_shipped_gate_distance() {
        // At the shipped T0 constants (5% target, Bonferroni per-bucket delta 0.02) the certify
        // condition needs ~2,334 zero-CER verified clips in ONE bucket — pin the order of magnitude
        // so a constants change is reflected in the surfaced distance, and sanity-check the bound:
        // at n = min_n the Hoeffding term must be <= target, and at n/2 it must not be.
        let n = min_calibration_n(0.05, 0.02);
        assert!((2_000..3_000).contains(&n), "expected ~2,334, got {n}");
        let hoeff = |k: f64| ((k / 0.02_f64).ln() / (2.0 * k)).sqrt();
        assert!(hoeff(n as f64) <= 0.05 + 1e-9);
        assert!(hoeff(n as f64 / 2.0) > 0.05);
        // A lenient target is reachable with far less data — the honest "on-ramp" the report shows.
        assert!(min_calibration_n(0.15, 0.02) < 300);
        assert_eq!(min_calibration_n(0.0, 0.02), usize::MAX, "zero target is never reachable");
    }

    /// Boundary-exact tests for the conformal machinery. This module decides which transcripts are
    /// auto-accepted WITHOUT a human ever reading them, so "roughly right" is not a standard it can
    /// be held to. The first full cargo-mutants sweep left 16 mutants alive here, every one of them
    /// because the existing tests checked ranges and orderings rather than values and edges.
    #[test]
    fn nonconformity_pins_the_formula_not_just_its_shape() {
        // The existing test used ctc = -1.0, where the 0.1 COEFFICIENT is invisible:
        // 0.1 * 1.0 == 0.1 / 1.0, so `*` -> `/` survived. Any other ctc separates them.
        // S = (1 - confidence) + 0.1 * (-ctc)
        let s = nonconformity(0.9, Some(-5.0));
        assert!((s - 0.6).abs() < 1e-12, "0.1 + 0.1*5 = 0.6, got {s}"); // `/` would give 0.12
        let s = nonconformity(0.5, Some(-10.0));
        assert!((s - 1.5).abs() < 1e-12, "0.5 + 0.1*10 = 1.5, got {s}"); // `/` would give 0.51

        // Missing ctc defaults to -5.0 -- pinned, because the default IS the offline CTC path.
        let s = nonconformity(0.9, None);
        assert!((s - 0.6).abs() < 1e-12, "None must behave as ctc = -5.0, got {s}");
    }

    #[test]
    fn snr_bucket_edges_are_exact() {
        // The cutoffs are `< 5.0`, `< 15.0`, `< 25.0`. The existing test sampled 2/10/20/30 --
        // comfortably inside each band, so `<` -> `<=` at every edge survived. A segment sitting
        // exactly on a boundary must fall in the UPPER band, or it is calibrated against the wrong
        // acoustic condition and the per-condition coverage guarantee silently does not apply.
        assert_eq!(snr_bucket(Some(4.999)), 0);
        assert_eq!(snr_bucket(Some(5.0)), 1, "exactly 5 dB is NOT the <5 very-noisy band");
        assert_eq!(snr_bucket(Some(14.999)), 1);
        assert_eq!(snr_bucket(Some(15.0)), 2, "exactly 15 dB belongs to the upper band");
        assert_eq!(snr_bucket(Some(24.999)), 2);
        assert_eq!(snr_bucket(Some(25.0)), 3, "exactly 25 dB is clean");
    }

    #[test]
    fn min_calibration_n_is_exact_not_approximate() {
        // Pinned to the value, not a 2_000..3_000 window: the window let `-` -> `+` and
        // `<` -> `<=` mutants inside the fixed-point iteration survive. This number is quoted to
        // the user as "how far away the gate is", so drift in it is a wrong claim, not a nuance.
        assert_eq!(min_calibration_n(0.05, 0.02), 2334);
        assert_eq!(min_calibration_n(0.15, 0.02), 206);
        assert_eq!(min_calibration_n(0.0, 0.02), usize::MAX);
    }

    #[test]
    fn calibrate_threshold_pins_the_risk_math() {
        // A fixed, hand-checkable calibration set: 30 clean items at score 0.10, 10 wrong at 0.50.
        // Deterministic, so the chosen cutoff and the reported bound are exact values.
        let mut scored: Vec<(f64, f64)> = (0..30).map(|_| (0.10, 0.0)).collect();
        scored.extend((0..10).map(|_| (0.50, 1.0)));
        // Target 0.40, not 0.30: cutting at the 30 clean points gives a Bonferroni-corrected bound
        // of sqrt(ln(400)/60) ~= 0.316, so a 0.30 target certifies NOTHING here. That is the gate
        // being correctly conservative, and it is worth pinning that it refuses (below).
        let (threshold, bound, calibrated) = calibrate_threshold(&scored, 0.40, 0.90);
        assert!(calibrated, "40 well-separated points must calibrate at target 0.40");
        assert!((threshold - 0.10).abs() < 1e-12, "cutoff must sit at the clean tie-group, got {threshold}");
        // bound = empirical_risk + sqrt(ln(n/delta) / (2k)) = 0 + sqrt(ln(400)/60)
        let expected = ((40.0f64 / 0.1).ln() / (2.0 * 30.0)).sqrt();
        assert!((bound - expected).abs() < 1e-12, "expected {expected}, got {bound}");
        assert!(bound <= 0.40, "a calibrated bound may never exceed the target");

        // The same data at a target BELOW that bound must refuse to certify rather than stretch.
        let (_, _, cal_tight) = calibrate_threshold(&scored, 0.30, 0.90);
        assert!(!cal_tight, "target 0.30 < achievable bound 0.316 must report uncalibrated");

        // Under 10 items is the cold-start path: uncalibrated, fixed 0.35 heuristic threshold.
        let (t, b, cal) = calibrate_threshold(&[(0.1, 0.0); 9], 0.05, 0.90);
        assert!(!cal);
        assert!((t - 0.35).abs() < 1e-12, "cold-start threshold is 0.35, got {t}");
        assert!((b - 0.05).abs() < 1e-12, "cold-start reports the target as the bound, got {b}");

        // EXACTLY 10 is the first non-cold-start size (the check is `< 10`). Tested at 9 only, the
        // `<` -> `<=` mutant is invisible because 9 is cold-start either way.
        let (t10, _, _) = calibrate_threshold(&[(0.1, 0.0); 10], 0.9, 0.90);
        assert!((t10 - 0.35).abs() > 1e-12, "10 items must NOT take the cold-start path, got {t10}");

        // The empirical risk is a MEAN (sum / k). With an all-zero-risk prefix `sum * k` and
        // `sum / k` are both 0, so that mutant needs a certified prefix carrying real error:
        // 20 items at risk 0.5 => mean 0.5, and the reported bound must exceed it.
        let risky: Vec<(f64, f64)> = (0..20).map(|_| (0.10, 0.5)).collect();
        let (_, rb, rcal) = calibrate_threshold(&risky, 0.9, 0.90);
        assert!(rcal, "20 items at target 0.9 must calibrate");
        let expected_risk_bound = 0.5 + ((20.0f64 / 0.1).ln() / (2.0 * 20.0)).sqrt();
        assert!(
            (rb - expected_risk_bound).abs() < 1e-12,
            "bound must be MEAN risk 0.5 plus slack ({expected_risk_bound}), got {rb}"
        );
    }

    #[test]
    fn snr_buckets_partition_by_condition() {
        assert_eq!(snr_bucket(None), 4, "unknown SNR");
        assert_eq!(snr_bucket(Some(2.0)), 0, "very noisy");
        assert_eq!(snr_bucket(Some(10.0)), 1);
        assert_eq!(snr_bucket(Some(20.0)), 2);
        assert_eq!(snr_bucket(Some(30.0)), 3, "clean");
    }

    #[test]
    fn test_nonconformity_score() {
        let seg1 = mock_segment("1", 0.9, -1.0, true, "test", "test");
        // (1.0 - 0.9) + 0.1 * 1.0 = 0.1 + 0.1 = 0.2
        assert!((compute_nonconformity_score(&seg1) - 0.2).abs() < 1e-5);
    }

    #[test]
    fn test_conformal_risk_control() {
        let mut segments = Vec::new();
        // 50 verified correct segments (low error, low nonconformity)
        for i in 1..=50 {
            segments.push(mock_segment(&format!("c{}", i), 0.95, -0.5, true, "کورد", "کورد"));
        }
        // 10 verified incorrect segments (high error, high nonconformity)
        for i in 51..=60 {
            segments.push(mock_segment(&format!("c{}", i), 0.4, -6.0, true, "خراب", "جوان"));
        }

        // Add 5 unverified segments
        for i in 1..=5 {
            segments.push(mock_segment(&format!("u{}", i), 0.95, -0.5, false, "کورد", ""));
        }

        // Target 0.3: with the Bonferroni-corrected slack the 50 clean calibration points give a
        // bound of ~0.253 (sqrt(ln(600)/100)), so they certify at 0.3 but no longer at 0.2 — the
        // corrected math correctly refuses the tighter target on this little data.
        let cert = calibrate_and_certify(&segments, 0.3, 0.90);
        assert!(cert.is_calibrated);
        assert!(cert.threshold > 0.0);
        // The unverified segments have low score so they should be certified
        assert!(cert.certified_segment_ids.contains(&"u1".to_string()));
    }

    /// Bug #10: a calibration cutoff chosen INSIDE a tie group whose tail is fully wrong must not
    /// yield a "calibrated" certificate whose bound never counted those admitted tied errors.
    #[test]
    fn tied_boundary_errors_cannot_escape_the_bound() {
        // 30 clean items at score 0.10, then a tie group of 10 at score 0.20 whose first 5 are
        // clean (cer 0) and last 5 are fully wrong (cer ~1.0). Rust's stable sort keeps the clean
        // tie members first, so the OLD code could pick k=35 (bound over the clean prefix ~0.18)
        // while certifying `score <= 0.20` — silently admitting the 5 wrong tied items. The fix
        // forbids mid-tie cutoffs, so at a tight 0.2 target this data must report UNCALIBRATED.
        let mut segs = Vec::new();
        for i in 0..30 {
            segs.push(mock_segment(&format!("a{i}"), 0.95, -0.5, true, "کورد", "کورد"));
            // 0.10, cer 0
        }
        for i in 0..5 {
            segs.push(mock_segment(&format!("g{i}"), 0.90, -1.0, true, "کورد", "کورد"));
            // 0.20, cer 0
        }
        for i in 0..5 {
            segs.push(mock_segment(&format!("b{i}"), 0.90, -1.0, true, "خراب", "جوان"));
            // 0.20, cer ~1
        }
        let cert = calibrate_and_certify(&segs, 0.2, 0.90);
        assert!(
            !cert.is_calibrated,
            "a cutoff inside a fully-wrong tie-group tail must not produce a calibrated certificate"
        );
    }

    /// Bug #11: the calibrated bound must use the union-bound ln(n/delta) slack (the cutoff is
    /// selected over the same data across n candidates), not the single-hypothesis ln(1/delta).
    #[test]
    fn calibrated_bound_uses_bonferroni_multiplicity_correction() {
        // 20 perfect items (cer 0), all tied → only cutoff k=20. For n=20, delta=0.1 the corrected
        // bound is sqrt(ln(200)/40) ≈ 0.3640; the old uncorrected form would give ≈ 0.2397.
        let segs: Vec<_> = (0..20).map(|i| mock_segment(&format!("c{i}"), 0.9, -1.0, true, "کورد", "کورد")).collect();
        let cert = calibrate_and_certify(&segs, 0.5, 0.90);
        assert!(cert.is_calibrated);
        let expected = ((20.0f64 / 0.1).ln() / (2.0 * 20.0)).sqrt();
        assert!(
            (cert.expected_error_bound - expected).abs() < 1e-9,
            "bound {} must equal Bonferroni-corrected {expected}",
            cert.expected_error_bound
        );
        assert!(cert.expected_error_bound > 0.30, "must be looser than the old uncorrected ~0.24 bound");
    }

    /// The whole point of the conformal certificate: when calibrated, the reported
    /// Hoeffding upper bound must not exceed the requested target error.
    #[test]
    fn calibrated_certificate_bound_never_exceeds_target() {
        let mut segs = Vec::new();
        for i in 0..40 {
            let correct = i % 5 != 0; // 80% correct
            let (raw, ann) = if correct { ("کورد", "کورد") } else { ("خراب", "جوان") };
            segs.push(mock_segment(&format!("c{i}"), 0.9, -1.0, true, raw, ann));
        }
        let cert = calibrate_and_certify(&segs, 0.3, 0.90);
        if cert.is_calibrated {
            assert!(
                cert.expected_error_bound <= 0.3 + 1e-9,
                "calibrated Hoeffding bound {} must not exceed target 0.3",
                cert.expected_error_bound
            );
        }
    }

    /// Monotonicity: a more lenient target error can only certify more (never fewer)
    /// segments — a sanity property the threshold search must preserve.
    #[test]
    fn more_lenient_target_certifies_at_least_as_many() {
        let mut segs = Vec::new();
        for i in 0..50 {
            let correct = i % 4 != 0;
            let (raw, ann) = if correct { ("کورد", "کورد") } else { ("خراب", "جوان") };
            let ctc = if correct { -1.0 } else { -8.0 }; // spread the nonconformity scores
            segs.push(mock_segment(&format!("c{i}"), 0.9, ctc, true, raw, ann));
        }
        let strict = calibrate_and_certify(&segs, 0.1, 0.90);
        let lenient = calibrate_and_certify(&segs, 0.4, 0.90);
        assert!(
            lenient.total_certified >= strict.total_certified,
            "lenient target certified {} but strict certified {} (must be monotone)",
            lenient.total_certified,
            strict.total_certified
        );
    }

    /// With too little calibration data the certificate must declare itself
    /// uncalibrated and fall back to the conservative heuristic threshold.
    #[test]
    fn too_few_verified_falls_back_to_uncalibrated() {
        let segs: Vec<_> = (0..5).map(|i| mock_segment(&format!("c{i}"), 0.9, -1.0, true, "کورد", "کورد")).collect();
        let cert = calibrate_and_certify(&segs, 0.2, 0.90);
        assert!(!cert.is_calibrated, "fewer than 10 verified segments must be uncalibrated");
        assert!((cert.threshold - 0.35).abs() < 1e-9, "fallback heuristic threshold is 0.35");
    }

    proptest! {
        #[test]
        fn nonconformity_score_is_always_nonnegative(
            conf in -2.0f64..2.0,
            ctc in -20.0f64..20.0,
        ) {
            let s = mock_segment("p", conf, ctc, true, "a", "a");
            prop_assert!(compute_nonconformity_score(&s) >= 0.0);
        }
    }
}
