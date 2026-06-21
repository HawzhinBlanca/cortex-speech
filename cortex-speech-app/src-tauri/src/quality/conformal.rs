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
    let cal_scored: Vec<(f64, f64)> = all_segments
        .iter()
        .filter_map(|s| {
            if !s.verified {
                return None;
            }
            let ref_text = s.annotated_transcript.as_deref()?.trim();
            if ref_text.is_empty() {
                return None;
            }
            let cer = compute_cer(ref_text, &s.raw_transcript).min(1.0); // bound to [0,1] for Hoeffding
            Some((compute_nonconformity_score(s), cer))
        })
        .collect();

    let cal_n = cal_scored.len();
    let (threshold, bound, is_calibrated) = calibrate_threshold(&cal_scored, target_error, confidence_level);

    // Certify every segment whose nonconformity is at or below the calibrated threshold.
    let certified_segment_ids: Vec<String> = all_segments
        .iter()
        .filter(|seg| compute_nonconformity_score(seg) <= threshold)
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
            ood_score: None,
            ..SpeechSegment::default()
        }
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
            segs.push(mock_segment(&format!("a{i}"), 0.95, -0.5, true, "کورد", "کورد")); // 0.10, cer 0
        }
        for i in 0..5 {
            segs.push(mock_segment(&format!("g{i}"), 0.90, -1.0, true, "کورد", "کورد")); // 0.20, cer 0
        }
        for i in 0..5 {
            segs.push(mock_segment(&format!("b{i}"), 0.90, -1.0, true, "خراب", "جوان")); // 0.20, cer ~1
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
        let segs: Vec<_> =
            (0..20).map(|i| mock_segment(&format!("c{i}"), 0.9, -1.0, true, "کورد", "کورد")).collect();
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
        let segs: Vec<_> =
            (0..5).map(|i| mock_segment(&format!("c{i}"), 0.9, -1.0, true, "کورد", "کورد")).collect();
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
