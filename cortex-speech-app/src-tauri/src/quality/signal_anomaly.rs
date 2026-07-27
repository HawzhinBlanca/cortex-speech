use std::path::Path;

pub struct SignalAnomalyDetector;

impl SignalAnomalyDetector {
    /// Round-24 #1/#2/#3: the previous WavLM-ONNX path scored signal anomaly as the cosine distance to a
    /// SYNTHETIC sine-wave centroid (`(i/256).sin()`) — a fabricated metric. No real learned
    /// in-distribution Kurdish-speech centroid is computed or shipped anywhere in the tree, the
    /// embedding was truncated to a hard-coded 256 dims (WavLM is 768/1024), and unchecked output
    /// shapes could panic or silently produce a NaN score that read as "in-distribution". Presenting
    /// distance-to-a-sine-wave as a signal-anomaly verdict in the UI and baking it into the exported dataset
    /// violated the project's honesty law. That path is removed until a real learned centroid exists;
    /// Signal-anomaly scoring uses the honest signal-processing heuristic (ZCR + frame-energy variance) below.
    pub fn new(_models_dir: &Path) -> Result<Self, String> {
        Ok(Self)
    }

    /// Measures the out-of-distribution distance (0.0 to 1.0) from a real signal-processing heuristic.
    pub fn compute_signal_anomaly_score(&self, pcm: &[i16]) -> Result<f64, String> {
        if pcm.is_empty() {
            return Ok(1.0); // Empty audio is maximally anomalous
        }
        Ok(self.heuristic_signal_anomaly_score(pcm))
    }

    /// Signal-processing anomaly heuristic: an honest (if crude) distance in [0,1] derived from the
    /// zero-crossing rate and frame-level energy variance. High ZCR (white noise / hiss) or very low
    /// energy variance (music / hum / silence) push the score up; clean speech sits near a small
    /// baseline. This is a measured signal property — not a learned model — and is labelled as such.
    fn heuristic_signal_anomaly_score(&self, pcm: &[i16]) -> f64 {
        let n = pcm.len();
        if n < 100 {
            return 1.0;
        }

        // Calculate Zero Crossing Rate (ZCR)
        let mut zero_crossings = 0;
        for i in 1..n {
            if (pcm[i] >= 0 && pcm[i - 1] < 0) || (pcm[i] < 0 && pcm[i - 1] >= 0) {
                zero_crossings += 1;
            }
        }
        let zcr = zero_crossings as f64 / n as f64;

        // Calculate frame-level energy variance
        let frame_size = 160; // 10ms frame at 16kHz
        let mut frame_energies = Vec::new();
        for chunk in pcm.chunks(frame_size) {
            let mut energy = 0.0;
            for &sample in chunk {
                let val = sample as f64 / 32768.0;
                energy += val * val;
            }
            frame_energies.push((energy / chunk.len() as f64).sqrt());
        }

        let m_energy = frame_energies.iter().sum::<f64>() / frame_energies.len() as f64;
        let var_energy = frame_energies
            .iter()
            .map(|&e| {
                let diff = e - m_energy;
                diff * diff
            })
            .sum::<f64>()
            / frame_energies.len() as f64;

        // High ZCR (white noise/hiss) or low energy variance (music/hum/silence) suggests an anomalous signal
        let zcr_factor = if zcr > 0.35 { ((zcr - 0.35) * 5.0).min(1.0) } else { 0.0 };

        let var_factor = if var_energy < 1e-4 { (1.0 - (var_energy * 10000.0)).max(0.0) } else { 0.0 };

        // Combine factors into a [0.0, 1.0] distance on the same scale a cosine distance would use.
        let base_distance = 0.15; // clean speech nominal baseline distance
        let final_distance = base_distance + zcr_factor * 0.4 + var_factor * 0.4;
        final_distance.min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EXACT scores for signals whose ZCR and energy variance are known by construction.
    ///
    /// The whole module scored 35 surviving mutants in the first full cargo-mutants sweep: the only
    /// tests asserted `score < 0.5` / `score > 0.5`, which stays true when the ZCR loop, the frame
    /// energy mean, the variance, or the final weighting is quietly wrong. This score feeds the
    /// audio-quality signal shown next to a clip, so a silently wrong number misdirects review
    /// attention. Each expectation below is derived from the documented formula, not observed and
    /// pasted: base 0.15 + zcr_factor*0.4 + var_factor*0.4, clamped to 1.0.
    #[test]
    fn heuristic_score_is_exact_for_known_signals() {
        let d = SignalAnomalyDetector::new(Path::new("")).unwrap();

        // (1) Digital silence: no sign changes => zcr 0 => zcr_factor 0. All frame energies 0 =>
        // variance 0 < 1e-4 => var_factor = (1 - 0) = 1. Score = 0.15 + 0 + 0.4 = 0.55.
        let silence = vec![0i16; 1000];
        let s = d.heuristic_signal_anomaly_score(&silence);
        assert!((s - 0.55).abs() < 1e-12, "silence must score exactly 0.55, got {s}");

        // (2) Full-scale alternating square wave: a sign change at every sample => zcr ~1 =>
        // zcr_factor clamps to 1. Every frame has identical energy (|v| = 0.5) => variance 0 =>
        // var_factor 1. Score = 0.15 + 0.4 + 0.4 = 0.95.
        let alternating: Vec<i16> = (0..1000).map(|i| if i % 2 == 0 { 16384 } else { -16384 }).collect();
        let s = d.heuristic_signal_anomaly_score(&alternating);
        assert!((s - 0.95).abs() < 1e-12, "alternating full-scale must score exactly 0.95, got {s}");

        // (3) Loud-then-silent, all one polarity: no sign changes => zcr_factor 0, but the frame
        // energies differ wildly => variance >> 1e-4 => var_factor 0. Score = the bare baseline,
        // 0.15. This is the case that pins base_distance and BOTH factor branches being inactive.
        let mut half_loud = vec![16384i16; 500];
        half_loud.extend(std::iter::repeat(0i16).take(500));
        let s = d.heuristic_signal_anomaly_score(&half_loud);
        assert!((s - 0.15).abs() < 1e-12, "high-variance one-polarity signal is the 0.15 baseline, got {s}");
    }

    /// A signal whose energy variance lands INSIDE the (0, 1e-4) window, so `var_factor` is
    /// partially active rather than saturated at 0 or 1.
    ///
    /// This is the case that pins the absolute AMPLITUDE arithmetic. The three saturated signals
    /// above cannot: with variance either exactly 0 or enormous, scaling every sample (the
    /// `/ 32768.0` -> `* 32768.0` mutant) or replacing `val * val` with `val + val` leaves
    /// var_factor pinned at the same end of its range and the score unchanged. Here the 1e-4
    /// threshold is close enough that any magnitude error pushes variance over it and collapses
    /// the score to the bare 0.15 baseline.
    ///
    /// Deliberately a strict-interval assertion rather than a golden constant: what matters is
    /// that the factor is genuinely partial, and that is exactly what the mutants destroy.
    #[test]
    fn partially_active_variance_factor_pins_the_amplitude_math() {
        let d = SignalAnomalyDetector::new(Path::new("")).unwrap();
        // All one polarity => no zero crossings => zcr_factor is 0, isolating the variance path.
        // Two amplitude plateaus give a small, non-zero spread in per-frame RMS.
        let mut pcm = vec![328i16; 480]; // RMS ~= 0.0100
        pcm.extend(std::iter::repeat(131i16).take(480)); // RMS ~= 0.0040
        let s = d.heuristic_signal_anomaly_score(&pcm);
        assert!(
            s > 0.15 + 1e-9 && s < 0.55 - 1e-9,
            "variance must be partially active (strictly between the 0.15 baseline and the \
             0.55 zero-variance score), got {s}"
        );
    }

    /// The `n < 100` short-circuit. Tested only well below the cap, `<` -> `<=` / `==` survive.
    #[test]
    fn too_short_input_is_maximally_anomalous_at_the_exact_boundary() {
        let d = SignalAnomalyDetector::new(Path::new("")).unwrap();
        assert_eq!(d.heuristic_signal_anomaly_score(&[0i16; 99]), 1.0, "99 samples is too short");
        // Exactly 100 must NOT short-circuit: it is silence, so it takes the real path and scores 0.55.
        let s = d.heuristic_signal_anomaly_score(&[0i16; 100]);
        assert!((s - 0.55).abs() < 1e-12, "exactly 100 samples must be scored, not short-circuited: {s}");
    }

    #[test]
    fn test_heuristic_speech() {
        let detector = SignalAnomalyDetector::new(Path::new("")).unwrap();
        // Generate a simulated speech signal (sine wave sweeps/modulated)
        let mut pcm = Vec::new();
        for i in 0..16000 {
            // Modulated sine wave to simulate vocal activity variation
            let sample = (32767.0 * (i as f64 * 0.05).sin() * (i as f64 * 0.001).sin()) as i16;
            pcm.push(sample);
        }
        let score = detector.compute_signal_anomaly_score(&pcm).unwrap();
        // Dynamic speech should have low distance (under threshold)
        assert!(score < 0.5);
    }

    #[test]
    fn test_heuristic_noise() {
        let detector = SignalAnomalyDetector::new(Path::new("")).unwrap();
        // Generate white noise (random high ZCR)
        let mut pcm = Vec::new();
        let mut seed = 12345u64;
        for _ in 0..16000 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let sample = ((seed >> 32) as i16).wrapping_rem(10000);
            pcm.push(sample);
        }
        let score = detector.compute_signal_anomaly_score(&pcm).unwrap();
        // Noise should have higher distance
        assert!(score > 0.4);
    }

    #[test]
    fn signal_anomaly_score_is_always_finite_and_in_range() {
        // Round-24 #3: the signal-anomaly score must always be a finite value in [0,1] — never NaN (which the
        // gate's `score > threshold` would silently treat as in-distribution).
        let detector = SignalAnomalyDetector::new(Path::new("")).unwrap();
        for pcm in [vec![], vec![0i16; 50], vec![0i16; 16000], vec![32767i16; 16000]] {
            let score = detector.compute_signal_anomaly_score(&pcm).unwrap();
            assert!(score.is_finite() && (0.0..=1.0).contains(&score), "score out of range: {score}");
        }
    }
}
