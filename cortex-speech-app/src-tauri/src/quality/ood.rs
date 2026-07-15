use std::path::Path;

pub struct OodDetector;

impl OodDetector {
    /// Round-24 #1/#2/#3: the previous WavLM-ONNX path scored OOD as the cosine distance to a
    /// SYNTHETIC sine-wave centroid (`(i/256).sin()`) — a fabricated metric. No real learned
    /// in-distribution Kurdish-speech centroid is computed or shipped anywhere in the tree, the
    /// embedding was truncated to a hard-coded 256 dims (WavLM is 768/1024), and unchecked output
    /// shapes could panic or silently produce a NaN score that read as "in-distribution". Presenting
    /// distance-to-a-sine-wave as an OOD verdict in the UI and baking it into the exported dataset
    /// violated the project's honesty law. That path is removed until a real learned centroid exists;
    /// OOD scoring uses the honest signal-processing heuristic (ZCR + frame-energy variance) below.
    pub fn new(_models_dir: &Path) -> Result<Self, String> {
        Ok(Self)
    }

    /// Measures the out-of-distribution distance (0.0 to 1.0) from a real signal-processing heuristic.
    pub fn compute_ood_score(&self, pcm: &[i16]) -> Result<f64, String> {
        if pcm.is_empty() {
            return Ok(1.0); // Empty audio is completely OOD
        }
        Ok(self.heuristic_ood_score(pcm))
    }

    /// Signal-processing OOD heuristic: an honest (if crude) distance in [0,1] derived from the
    /// zero-crossing rate and frame-level energy variance. High ZCR (white noise / hiss) or very low
    /// energy variance (music / hum / silence) push the score up; clean speech sits near a small
    /// baseline. This is a measured signal property — not a learned model — and is labelled as such.
    fn heuristic_ood_score(&self, pcm: &[i16]) -> f64 {
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

        // High ZCR (white noise/hiss) or low energy variance (music/hum/silence) suggests OOD
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

    #[test]
    fn test_heuristic_speech() {
        let detector = OodDetector::new(Path::new("")).unwrap();
        // Generate a simulated speech signal (sine wave sweeps/modulated)
        let mut pcm = Vec::new();
        for i in 0..16000 {
            // Modulated sine wave to simulate vocal activity variation
            let sample = (32767.0 * (i as f64 * 0.05).sin() * (i as f64 * 0.001).sin()) as i16;
            pcm.push(sample);
        }
        let score = detector.compute_ood_score(&pcm).unwrap();
        // Dynamic speech should have low distance (under threshold)
        assert!(score < 0.5);
    }

    #[test]
    fn test_heuristic_noise() {
        let detector = OodDetector::new(Path::new("")).unwrap();
        // Generate white noise (random high ZCR)
        let mut pcm = Vec::new();
        let mut seed = 12345u64;
        for _ in 0..16000 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let sample = ((seed >> 32) as i16).wrapping_rem(10000);
            pcm.push(sample);
        }
        let score = detector.compute_ood_score(&pcm).unwrap();
        // Noise should have higher distance
        assert!(score > 0.4);
    }

    #[test]
    fn ood_score_is_always_finite_and_in_range() {
        // Round-24 #3: the OOD score must always be a finite value in [0,1] — never NaN (which the
        // gate's `score > threshold` would silently treat as in-distribution).
        let detector = OodDetector::new(Path::new("")).unwrap();
        for pcm in [vec![], vec![0i16; 50], vec![0i16; 16000], vec![32767i16; 16000]] {
            let score = detector.compute_ood_score(&pcm).unwrap();
            assert!(score.is_finite() && (0.0..=1.0).contains(&score), "score out of range: {score}");
        }
    }
}
