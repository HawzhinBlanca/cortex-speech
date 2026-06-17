pub struct AudioQualityMetrics {
    pub rms_db: f64,
    pub clipping_ratio: f64,
    pub snr_db: f64,
}

/// Analyze the audio quality of a mono PCM buffer (16kHz).
pub fn analyze_audio_quality(pcm: &[i16]) -> AudioQualityMetrics {
    if pcm.is_empty() {
        return AudioQualityMetrics { rms_db: -100.0, clipping_ratio: 0.0, snr_db: 0.0 };
    }

    let n = pcm.len();
    let mut sum_sq = 0.0;
    let mut clipped_count = 0;

    for &sample in pcm {
        let val = sample as f64 / 32768.0;
        sum_sq += val * val;
        // Check for clipping at or near the 16-bit integer boundary
        if sample.abs() >= 32760 {
            clipped_count += 1;
        }
    }

    let rms = (sum_sq / n as f64).sqrt();
    let rms_db = if rms > 1e-10 { 20.0 * rms.log10() } else { -100.0 };

    let clipping_ratio = clipped_count as f64 / n as f64;

    // Estimate SNR by dividing the signal into 100ms frames (1600 samples at 16kHz)
    let frame_size = 1600;
    let mut frame_rms = Vec::new();

    for chunk in pcm.chunks(frame_size) {
        if chunk.len() < frame_size / 2 {
            continue; // skip remainder chunks that are too short
        }
        let mut chunk_sum_sq = 0.0;
        for &s in chunk {
            let val = s as f64 / 32768.0;
            chunk_sum_sq += val * val;
        }
        let c_rms = (chunk_sum_sq / chunk.len() as f64).sqrt();
        frame_rms.push(c_rms);
    }

    let snr_db = if frame_rms.len() < 3 {
        0.0
    } else {
        frame_rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // Noise floor: average of lowest 10% of frames
        let noise_count = (frame_rms.len() / 10).max(1);
        let noise_sum: f64 = frame_rms.iter().take(noise_count).sum();
        let noise_rms = noise_sum / noise_count as f64;

        // Signal level: average of highest 10% of frames
        let signal_count = (frame_rms.len() / 10).max(1);
        let signal_sum: f64 = frame_rms.iter().rev().take(signal_count).sum();
        let signal_rms = signal_sum / signal_count as f64;

        if noise_rms > 1e-10 && signal_rms > 1e-10 {
            let ratio = signal_rms / noise_rms;
            (20.0 * ratio.log10()).clamp(0.0, 100.0)
        } else {
            0.0
        }
    };

    AudioQualityMetrics { rms_db, clipping_ratio, snr_db }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_silence() {
        let pcm = vec![0i16; 16000];
        let metrics = analyze_audio_quality(&pcm);
        assert!(metrics.rms_db < -90.0);
        assert_eq!(metrics.clipping_ratio, 0.0);
        assert_eq!(metrics.snr_db, 0.0);
    }

    #[test]
    fn test_analyze_clipping() {
        let mut pcm = vec![0i16; 1000];
        for sample in pcm.iter_mut().take(100) {
            *sample = 32767;
        }
        let metrics = analyze_audio_quality(&pcm);
        assert_eq!(metrics.clipping_ratio, 0.1);
    }
}
