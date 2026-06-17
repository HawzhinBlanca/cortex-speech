use ndarray::Array2;
use ort::{session::Session, value::Tensor};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub struct OodDetector {
    session: Option<Mutex<Session>>,
    centroid: Vec<f32>,
    threshold: f32,
}

fn lock_session<T>(session: &Mutex<T>) -> MutexGuard<'_, T> {
    session.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned OOD session lock");
        poisoned.into_inner()
    })
}

impl OodDetector {
    pub fn new(models_dir: &Path, threshold: f32) -> Result<Self, String> {
        let model_path = models_dir.join("wavlm_ood.onnx");

        // Default centroid vector for Kurdish target speech (pre-calculated or mock for initialization)
        let mut centroid = vec![0.0f32; 256];
        for (i, value) in centroid.iter_mut().enumerate().take(256) {
            *value = (i as f32 / 256.0).sin();
        }
        let norm = centroid.iter().map(|&x| x * x).sum::<f32>().sqrt();
        for x in &mut centroid {
            *x /= norm;
        }

        if !model_path.exists() {
            tracing::warn!("WavLM OOD model not found at {:?}; using signal processing fallback", model_path);
            return Ok(Self { session: None, centroid, threshold });
        }

        crate::models::init_ort_dylib_path();
        let session = Session::builder()
            .map_err(|e| format!("OOD Session Builder: {e}"))?
            .with_execution_providers([ort::ep::CPU::default().build()])
            .map_err(|e| format!("OOD EP: {e}"))?
            .commit_from_file(&model_path)
            .map_err(|e| format!("Load OOD Model: {e}"))?;

        Ok(Self { session: Some(Mutex::new(session)), centroid, threshold })
    }

    /// Measures the out-of-distribution distance (0.0 to 1.0).
    pub fn compute_ood_score(&self, pcm: &[i16]) -> Result<f64, String> {
        if pcm.is_empty() {
            return Ok(1.0); // Empty audio is completely OOD
        }

        if let Some(ref session_mutex) = self.session {
            let mut session_guard = lock_session(session_mutex);

            let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
            let input_nd =
                Array2::from_shape_vec((1, f32_pcm.len()), f32_pcm).map_err(|e| format!("OOD Input shape: {e}"))?;
            let input_tensor = Tensor::from_array(input_nd).map_err(|e| format!("OOD Tensor: {e}"))?;

            let outputs = session_guard
                .run(ort::inputs!["input_values" => input_tensor])
                .map_err(|e| format!("OOD Inference: {e}"))?;

            let output_tensor = outputs["last_hidden_state"]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("Extract OOD output: {e}"))?;

            let shape = output_tensor.0;
            let data = output_tensor.1;
            let t_dim = shape[1] as usize;
            let d_dim = shape[2] as usize;

            // Average pool over time dimension
            let mut emb = vec![0.0f32; d_dim];
            for t in 0..t_dim {
                let offset = t * d_dim;
                for (d, value) in emb.iter_mut().enumerate().take(d_dim.min(self.centroid.len())) {
                    *value += data[offset + d];
                }
            }
            for value in emb.iter_mut().take(self.centroid.len()) {
                *value /= t_dim as f32;
            }

            // Normalize embedding
            let mut norm = emb.iter().map(|&x| x * x).sum::<f32>().sqrt();
            if norm < 1e-6 {
                norm = 1.0;
            }
            for x in &mut emb {
                *x /= norm;
            }

            // Cosine similarity
            let dot: f32 = emb.iter().zip(self.centroid.iter()).map(|(a, b)| a * b).sum();
            let distance = 1.0 - dot;
            Ok(distance as f64)
        } else {
            // Signal processing heuristic fallback
            Ok(self.fallback_heuristic(pcm))
        }
    }

    /// Returns true if the audio is out-of-distribution.
    pub fn is_ood(&self, pcm: &[i16]) -> Result<bool, String> {
        let score = self.compute_ood_score(pcm)?;
        Ok(score > self.threshold as f64)
    }

    /// Fallback signal processing heuristic: computes OOD distance based on ZCR and energy variance.
    fn fallback_heuristic(&self, pcm: &[i16]) -> f64 {
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

        // Combine factors into a mock cosine distance score [0.0, 1.0]
        let base_distance = 0.15; // clean speech nominal baseline distance
        let final_distance = base_distance + zcr_factor * 0.4 + var_factor * 0.4;
        final_distance.min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_heuristic_speech() {
        let detector = OodDetector::new(Path::new(""), 0.5).unwrap();
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
    fn test_fallback_heuristic_noise() {
        let detector = OodDetector::new(Path::new(""), 0.5).unwrap();
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
    fn ood_session_lock_recovers_poisoned_lock() {
        let session = Mutex::new(41usize);

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = session.lock().expect("lock OOD session");
            panic!("poison OOD session");
        }));

        *lock_session(&session) += 1;

        assert_eq!(*lock_session(&session), 42);
    }
}
