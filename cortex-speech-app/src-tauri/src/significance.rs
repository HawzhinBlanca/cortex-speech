//! Statistical significance for ASR evaluation.
//!
//! Point WER/CER numbers are not enough for a defensible scorecard (roadmap M2/M4b):
//! a stranger must be able to tell whether a difference is real and how uncertain the
//! estimate is. This module provides two standard, dependency-free tools:
//!
//! * [`bootstrap_ci`] — a segment-level (paired) bootstrap confidence interval for a
//!   corpus-level (micro) error rate. Resampling whole *segments* preserves the
//!   within-segment correlation of errors that a naive per-token bootstrap would break.
//! * [`mapsswe`] — the Matched-Pair Sentence-Segment Word-Error test (Gillick & Cox,
//!   1989): a paired test for whether two systems differ on the same segments. Returns
//!   a two-sided p-value.
//!
//! Both are **deterministic** (seeded) so a published CI / p-value is reproducible from
//! the same eval rows — a hard requirement for a stranger-reproducible scorecard. We
//! never touch the OS RNG, which would make a published interval unrepeatable.

use serde::{Deserialize, Serialize};

/// Per-segment error counts for one system: edit distance and reference length.
#[derive(Debug, Clone, Copy)]
pub struct SegmentError {
    /// Word (or character) edit distance for this segment.
    pub errors: f64,
    /// Reference length in the same unit (words or characters).
    pub ref_len: f64,
}

impl SegmentError {
    pub fn new(errors: f64, ref_len: f64) -> Self {
        Self { errors, ref_len }
    }
}

/// A small, fast, fully deterministic PRNG (xorshift64*), used only for bootstrap
/// resampling. Seeded so confidence intervals are byte-for-byte reproducible.
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // Avoid the degenerate all-zero state.
        Self { state: seed ^ 0x9E37_79B9_7F4A_7C15 | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform index in `[0, n)`.
    fn index(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
}

/// A bootstrap confidence-interval estimate for an error rate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfidenceInterval {
    pub point: f64,
    pub lower: f64,
    pub upper: f64,
    pub confidence: f64,
}

/// Micro (corpus-level) error rate: `sum(errors) / sum(ref_len)`.
pub fn micro_rate(segments: &[SegmentError]) -> f64 {
    let (err, len) = segments
        .iter()
        .fold((0.0f64, 0.0f64), |(e, l), s| (e + s.errors, l + s.ref_len));
    if len > 0.0 {
        (err / len).min(1.0)
    } else {
        0.0
    }
}

/// Segment-level bootstrap confidence interval for the micro error rate.
///
/// `n_resamples` bootstrap replicas, each resampling `segments.len()` segments with
/// replacement; the interval is the percentile interval at two-sided `confidence`
/// (e.g. `0.95`). Deterministic given `seed`.
pub fn bootstrap_ci(
    segments: &[SegmentError],
    n_resamples: usize,
    confidence: f64,
    seed: u64,
) -> ConfidenceInterval {
    let point = micro_rate(segments);
    let n = segments.len();
    if n == 0 || n_resamples == 0 {
        return ConfidenceInterval { point, lower: point, upper: point, confidence };
    }

    let mut rng = XorShift64::new(seed);
    let mut rates = Vec::with_capacity(n_resamples);
    for _ in 0..n_resamples {
        let mut err = 0.0f64;
        let mut len = 0.0f64;
        for _ in 0..n {
            let s = segments[rng.index(n)];
            err += s.errors;
            len += s.ref_len;
        }
        rates.push(if len > 0.0 { (err / len).min(1.0) } else { 0.0 });
    }
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let alpha = (1.0 - confidence) / 2.0;
    let lower = percentile(&rates, alpha);
    // `upper` is mathematically >= `lower`; `.max` only absorbs <=1-ULP float noise,
    // guaranteeing a well-ordered interval for every possible input.
    let upper = percentile(&rates, 1.0 - alpha).max(lower);
    ConfidenceInterval { point, lower, upper, confidence }
}

/// Linear-interpolated percentile of a pre-sorted slice. `q` in `[0, 1]`.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    match sorted.len() {
        0 => return 0.0,
        1 => return sorted[0],
        _ => {}
    }
    let q = q.clamp(0.0, 1.0);
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi || sorted[lo] == sorted[hi] {
        // Equal endpoints: return the exact value rather than interpolating, which can
        // introduce sub-ULP float noise (e.g. when every resample yields one rate).
        return sorted[lo];
    }
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Matched-Pair Sentence-Segment Word-Error (MAPSSWE) significance test.
///
/// For each segment `i`, `z_i = errors_a_i - errors_b_i` is the difference in errors
/// between system A and B on the *same* segment. Under H0 (no difference) the mean of
/// `z` is 0; by the CLT the standardized mean is ≈ N(0, 1). Returns the two-sided
/// p-value: `≈ 1.0` means indistinguishable, small `p` means a significant difference.
/// Systems are paired by index; the shorter length wins if they differ.
pub fn mapsswe(a: &[SegmentError], b: &[SegmentError]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 1.0;
    }
    let diffs: Vec<f64> = (0..n).map(|i| a[i].errors - b[i].errors).collect();
    let mean = diffs.iter().sum::<f64>() / n as f64;
    let var = diffs.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    if var <= 0.0 {
        // No variance: either a constant non-zero gap (perfectly significant) or
        // identical systems (not significant at all).
        return if mean == 0.0 { 1.0 } else { 0.0 };
    }
    let se = (var / n as f64).sqrt();
    let z = mean / se;
    two_sided_p_from_z(z)
}

/// Two-sided p-value from a z-score via the standard-normal CDF.
fn two_sided_p_from_z(z: f64) -> f64 {
    (2.0 * (1.0 - standard_normal_cdf(z.abs()))).clamp(0.0, 1.0)
}

/// Standard-normal CDF Φ(x).
fn standard_normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Error function via Abramowitz & Stegun 7.1.26 (|error| ≤ 1.5e-7).
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
        + 0.254829592)
        * t;
    let y = 1.0 - poly * (-x * x).exp();
    sign * y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wer::compute_wer;

    fn segs(pairs: &[(f64, f64)]) -> Vec<SegmentError> {
        pairs.iter().map(|&(e, l)| SegmentError::new(e, l)).collect()
    }

    #[test]
    fn micro_rate_is_corpus_level() {
        // 1 error / 4 ref + 3 errors / 6 ref => 4/10 = 0.4 (NOT the mean of the two rates).
        let s = segs(&[(1.0, 4.0), (3.0, 6.0)]);
        assert!((micro_rate(&s) - 0.4).abs() < 1e-12);
    }

    #[test]
    fn bootstrap_ci_brackets_point_and_is_deterministic() {
        let s = segs(&[(1.0, 10.0), (2.0, 10.0), (0.0, 10.0), (3.0, 10.0), (1.0, 10.0)]);
        let ci1 = bootstrap_ci(&s, 2000, 0.95, 42);
        let ci2 = bootstrap_ci(&s, 2000, 0.95, 42);
        assert_eq!(ci1, ci2, "same seed must give identical CI (reproducible scorecard)");
        assert!(ci1.lower <= ci1.point && ci1.point <= ci1.upper, "point must lie in the CI");
        assert!(ci1.lower >= 0.0 && ci1.upper <= 1.0, "rate CI must stay in [0,1]");
        assert!(ci1.upper > ci1.lower, "a non-degenerate sample should yield a real interval");
    }

    #[test]
    fn bootstrap_ci_is_degenerate_for_a_perfect_system() {
        let s = segs(&[(0.0, 10.0), (0.0, 8.0), (0.0, 12.0)]);
        let ci = bootstrap_ci(&s, 500, 0.95, 7);
        assert_eq!(ci.point, 0.0);
        assert_eq!(ci.lower, 0.0);
        assert_eq!(ci.upper, 0.0);
    }

    #[test]
    fn mapsswe_identical_systems_p_is_one() {
        let a = segs(&[(1.0, 10.0), (2.0, 10.0), (0.0, 10.0), (3.0, 10.0)]);
        assert_eq!(mapsswe(&a, &a), 1.0);
    }

    #[test]
    fn mapsswe_flags_a_clearly_better_system() {
        // System B has strictly fewer errors on every segment, with some spread.
        let a = segs(&[(5.0, 10.0), (6.0, 10.0), (4.0, 10.0), (7.0, 10.0), (5.0, 10.0), (6.0, 10.0)]);
        let b = segs(&[(1.0, 10.0), (2.0, 10.0), (0.0, 10.0), (3.0, 10.0), (1.0, 10.0), (2.0, 10.0)]);
        let p = mapsswe(&a, &b);
        assert!(p < 0.05, "a consistently better system must be significant, got p={p}");
    }

    #[test]
    fn mapsswe_too_few_segments_is_inconclusive() {
        let a = segs(&[(5.0, 10.0)]);
        let b = segs(&[(0.0, 10.0)]);
        assert_eq!(mapsswe(&a, &b), 1.0, "n<2 cannot be significant");
    }

    #[test]
    fn standard_normal_cdf_known_values() {
        assert!((standard_normal_cdf(0.0) - 0.5).abs() < 1e-6);
        // Φ(1.96) ≈ 0.975 (the 95% two-sided critical value).
        assert!((standard_normal_cdf(1.96) - 0.975).abs() < 1e-3);
        assert!((two_sided_p_from_z(1.96) - 0.05).abs() < 1e-2);
    }

    #[test]
    fn wer_matches_jiwer_reference_values() {
        // Reference WER values produced by the canonical jiwer implementation on ASCII
        // fixtures (Sorani normalization is a no-op on ASCII, so these are exact).
        //   identical                              -> 0.0
        //   1 substitution / 6 ref words           -> 0.16666...
        //   1 deletion     / 4 ref words           -> 0.25
        //   1 insertion    / 3 ref words           -> 0.33333...
        assert!(compute_wer("the cat sat on the mat", "the cat sat on the mat").abs() < 1e-9);
        assert!((compute_wer("the cat sat on the mat", "the cat sat on a mat") - 1.0 / 6.0).abs() < 1e-9);
        assert!((compute_wer("the quick brown fox", "the quick fox") - 0.25).abs() < 1e-9);
        assert!((compute_wer("a b c", "a x b c") - 1.0 / 3.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// A segment with errors in `[0, ref_len]` and a positive reference length.
    fn seg() -> impl Strategy<Value = SegmentError> {
        (1.0f64..100.0).prop_flat_map(|len| (0.0f64..=len).prop_map(move |e| SegmentError::new(e, len)))
    }

    proptest! {
        #[test]
        fn micro_rate_stays_in_unit_interval(segs in proptest::collection::vec(seg(), 1..50)) {
            let r = micro_rate(&segs);
            prop_assert!((0.0..=1.0).contains(&r), "micro_rate out of range: {r}");
        }

        #[test]
        fn bootstrap_ci_is_ordered_and_bounded(
            segs in proptest::collection::vec(seg(), 1..40),
            seed in any::<u64>(),
        ) {
            let ci = bootstrap_ci(&segs, 256, 0.95, seed);
            prop_assert!(ci.lower <= ci.upper, "lower {} > upper {}", ci.lower, ci.upper);
            prop_assert!(ci.lower >= 0.0 && ci.upper <= 1.0);
            prop_assert!((0.0..=1.0).contains(&ci.point));
        }

        #[test]
        fn bootstrap_ci_is_reproducible(
            segs in proptest::collection::vec(seg(), 1..40),
            seed in any::<u64>(),
        ) {
            // The same seed must yield byte-identical intervals — a published CI has to
            // be reproducible from the same eval rows.
            prop_assert_eq!(
                bootstrap_ci(&segs, 128, 0.9, seed),
                bootstrap_ci(&segs, 128, 0.9, seed)
            );
        }

        #[test]
        fn mapsswe_p_value_stays_in_unit_interval(
            a in proptest::collection::vec(seg(), 0..40),
            b in proptest::collection::vec(seg(), 0..40),
        ) {
            let p = mapsswe(&a, &b);
            prop_assert!((0.0..=1.0).contains(&p), "p out of range: {p}");
        }

        #[test]
        fn mapsswe_self_comparison_is_never_significant(a in proptest::collection::vec(seg(), 1..40)) {
            // A system compared to itself has zero per-segment difference -> p = 1.
            prop_assert_eq!(mapsswe(&a, &a), 1.0);
        }
    }
}
