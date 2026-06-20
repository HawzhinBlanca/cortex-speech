//! Calibration assessment — Expected Calibration Error (ECE) + a reliability diagram.
//!
//! The final blueprint flagged this as ENTIRELY MISSING: the conformal certificate and the autonomy
//! dial certify partly against noise until the model's confidence is shown to be CALIBRATED
//! (predicted confidence ≈ actual accuracy). This module MEASURES that honestly from
//! (confidence, was_correct) outcomes; it does not fabricate it. ECE near 0 = well-calibrated; a
//! large ECE means the stated confidence cannot be trusted, and the dial/gate must NOT lean on it.

/// One reliability-diagram bin: how stated confidence compares to real accuracy for predictions whose
/// confidence fell in `[lower, upper)` (the top bin is closed so confidence == 1.0 lands in it).
#[derive(Debug, Clone, PartialEq)]
pub struct ReliabilityBin {
    pub lower: f64,
    pub upper: f64,
    pub count: usize,
    pub avg_confidence: f64,
    pub accuracy: f64,
}

/// The calibration report: ECE (lower = better, 0 = perfectly calibrated) + the per-bin diagram.
#[derive(Debug, Clone)]
pub struct CalibrationReport {
    pub ece: f64,
    pub n: usize,
    pub bins: Vec<ReliabilityBin>,
}

/// Assess calibration from `(confidence in [0,1], was_correct)` pairs over `n_bins` equal-width bins.
///
/// `ECE = Σ_b (count_b / N) · |avg_confidence_b − accuracy_b|`. Empty input → `ece 0, n 0` (honest:
/// no calibration claim is possible with no data). Confidence is clamped to `[0,1]`.
pub fn assess_calibration(predictions: &[(f64, bool)], n_bins: usize) -> CalibrationReport {
    let n_bins = n_bins.max(1);
    let n = predictions.len();
    if n == 0 {
        return CalibrationReport { ece: 0.0, n: 0, bins: Vec::new() };
    }

    let width = 1.0 / n_bins as f64;
    // (count, confidence_sum, correct_count) per bin.
    let mut acc = vec![(0usize, 0.0f64, 0usize); n_bins];
    for &(confidence, correct) in predictions {
        let c = confidence.clamp(0.0, 1.0);
        let idx = ((c / width).floor() as usize).min(n_bins - 1); // c == 1.0 lands in the top bin
        acc[idx].0 += 1;
        acc[idx].1 += c;
        acc[idx].2 += usize::from(correct);
    }

    let mut bins = Vec::with_capacity(n_bins);
    let mut ece = 0.0;
    for (i, (count, conf_sum, correct)) in acc.into_iter().enumerate() {
        let lower = i as f64 * width;
        let upper = lower + width;
        if count == 0 {
            bins.push(ReliabilityBin { lower, upper, count: 0, avg_confidence: 0.0, accuracy: 0.0 });
            continue;
        }
        let avg_confidence = conf_sum / count as f64;
        let accuracy = correct as f64 / count as f64;
        ece += (count as f64 / n as f64) * (avg_confidence - accuracy).abs();
        bins.push(ReliabilityBin { lower, upper, count, avg_confidence, accuracy });
    }

    CalibrationReport { ece, n, bins }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_makes_no_calibration_claim() {
        let r = assess_calibration(&[], 10);
        assert_eq!(r.n, 0);
        assert_eq!(r.ece, 0.0);
        assert!(r.bins.is_empty(), "no data -> no diagram, no claim");
    }

    #[test]
    fn perfectly_calibrated_has_zero_ece() {
        // Stated confidence 0.5 and exactly half correct -> stated == actual -> ECE 0.
        let preds: Vec<(f64, bool)> = (0..100).map(|i| (0.5, i % 2 == 0)).collect();
        let r = assess_calibration(&preds, 10);
        assert!(r.ece < 1e-9, "ece={}", r.ece);
        assert_eq!(r.n, 100);
    }

    #[test]
    fn overconfident_predictions_have_high_ece() {
        // Always claims 0.9 confidence but only half are right -> ECE = |0.9 - 0.5| = 0.4.
        let preds: Vec<(f64, bool)> = (0..100).map(|i| (0.9, i % 2 == 0)).collect();
        let r = assess_calibration(&preds, 10);
        assert!((r.ece - 0.4).abs() < 1e-9, "ece={}", r.ece);
    }

    #[test]
    fn underconfident_predictions_have_high_ece() {
        // Claims 0.1 but 90% are right -> ECE = |0.1 - 0.9| = 0.8.
        let preds: Vec<(f64, bool)> = (0..100).map(|i| (0.1, i % 10 != 0)).collect();
        let r = assess_calibration(&preds, 10);
        assert!((r.ece - 0.8).abs() < 1e-9, "ece={}", r.ece);
    }

    #[test]
    fn confidence_one_lands_in_the_closed_top_bin() {
        let preds = [(1.0, true), (1.0, false)];
        let r = assess_calibration(&preds, 10);
        let top = r.bins.last().unwrap();
        assert_eq!(top.count, 2, "confidence 1.0 must fall in the top bin, not overflow");
        assert!((top.avg_confidence - 1.0).abs() < 1e-9);
        assert!((top.accuracy - 0.5).abs() < 1e-9);
        assert!((r.ece - 0.5).abs() < 1e-9);
    }
}
