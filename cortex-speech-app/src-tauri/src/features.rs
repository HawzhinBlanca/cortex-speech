use ndarray::Array2;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

pub struct FbankExtractor {
    sample_rate: u32,
    frame_length_ms: f32,
    frame_shift_ms: f32,
    num_mel_bins: usize,
    fft_length: usize,
    preemphasis: f32,
    log_floor: f32,
    #[allow(dead_code)]
    mel_low_freq: f32,
    window: Vec<f32>,
    mel_filters: Vec<Vec<f32>>,
    centered: bool,
    use_log10: bool,
    fft_normalized: bool,
    whisper_norm: bool,
}

impl FbankExtractor {
    pub fn new(sample_rate: u32) -> Self {
        Self::new_with_mel(sample_rate, 80)
    }

    pub fn new_with_mel(sample_rate: u32, num_mel_bins: usize) -> Self {
        let fft_length = if num_mel_bins >= 128 { 400 } else { 512 };
        let preemphasis = if num_mel_bins >= 128 { 0.0 } else { 0.97 };
        let mel_low_freq = if num_mel_bins >= 128 { 0.0 } else { 20.0 };
        let log_floor = 1e-10;
        let centered = num_mel_bins >= 128;
        let use_log10 = num_mel_bins >= 128;
        let fft_normalized = num_mel_bins < 128;
        let periodic = num_mel_bins >= 128;
        let whisper_norm = num_mel_bins >= 128;
        Self::new_custom(
            sample_rate,
            num_mel_bins,
            fft_length,
            preemphasis,
            mel_low_freq,
            log_floor,
            centered,
            use_log10,
            fft_normalized,
            periodic,
            whisper_norm,
        )
    }

    pub fn new_for_whisper(sample_rate: u32, num_mel_bins: usize) -> Self {
        Self::new_custom(sample_rate, num_mel_bins, 400, 0.0, 0.0, 1e-10, true, true, false, true, true)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_custom(
        sample_rate: u32,
        num_mel_bins: usize,
        fft_length: usize,
        preemphasis: f32,
        mel_low_freq: f32,
        log_floor: f32,
        centered: bool,
        use_log10: bool,
        fft_normalized: bool,
        periodic: bool,
        whisper_norm: bool,
    ) -> Self {
        let frame_length_ms = 25.0;
        let frame_shift_ms = 10.0;
        let mel_high_freq = sample_rate as f32 / 2.0;
        let frame_length = (sample_rate as f32 * frame_length_ms / 1000.0) as usize;
        let window = create_hanning_window(frame_length, periodic);
        let mel_filters = create_mel_filters(num_mel_bins, fft_length, sample_rate, mel_low_freq, mel_high_freq);

        Self {
            sample_rate,
            frame_length_ms,
            frame_shift_ms,
            num_mel_bins,
            fft_length,
            preemphasis,
            log_floor,
            mel_low_freq,
            window,
            mel_filters,
            centered,
            use_log10,
            fft_normalized,
            whisper_norm,
        }
    }

    pub fn num_mel_bins(&self) -> usize {
        self.num_mel_bins
    }

    pub fn compute(&self, audio: &[f32]) -> Array2<f32> {
        if audio.is_empty() {
            return Array2::zeros((0, self.num_mel_bins));
        }

        let preemphasized =
            if self.preemphasis > 0.0 { apply_preemphasis(audio, self.preemphasis) } else { audio.to_vec() };

        let frame_length = (self.sample_rate as f32 * self.frame_length_ms / 1000.0) as usize;
        // Never 0: it is a divisor in the frame-count computation below (a tiny sample
        // rate could otherwise floor it to 0 and divide-by-zero panic).
        let frame_shift = ((self.sample_rate as f32 * self.frame_shift_ms / 1000.0) as usize).max(1);

        let mut features = if self.centered {
            self.compute_centered(&preemphasized, frame_length, frame_shift)
        } else {
            self.compute_standard(&preemphasized, frame_length, frame_shift)
        };

        if self.whisper_norm {
            apply_whisper_mel_normalization(&mut features);
        }

        features
    }

    fn compute_centered(&self, audio: &[f32], frame_length: usize, frame_shift: usize) -> Array2<f32> {
        let pad = self.fft_length / 2;
        let mut padded = vec![0.0f32; pad + audio.len() + pad];
        padded[pad..pad + audio.len()].copy_from_slice(audio);

        let num_frames = 1 + padded.len().saturating_sub(frame_length) / frame_shift;
        let num_frames = if num_frames > 0 { num_frames - 1 } else { num_frames };

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(self.fft_length);

        let mut all_features = Vec::with_capacity(num_frames * self.num_mel_bins);

        for frame_idx in 0..num_frames {
            let start = frame_idx * frame_shift;

            let mut frame = vec![0.0f32; self.fft_length];
            let available = padded.len().saturating_sub(start);
            // Clamp to the frame buffer: an out-of-range fft_length < frame_length config
            // must not slice past the fft-sized buffer.
            let copy_len = frame_length.min(available).min(frame.len());
            if copy_len > 0 {
                frame[..copy_len].copy_from_slice(&padded[start..start + copy_len]);
            }

            // Zip rather than index by window length: the window is frame_length long,
            // which may exceed the fft-sized frame under a degenerate config.
            for (f, w) in frame.iter_mut().zip(self.window.iter()) {
                *f *= w;
            }

            let mut buf: Vec<Complex<f32>> = frame.iter().map(|&s| Complex::new(s, 0.0)).collect();
            fft.process(&mut buf);

            let power = power_spectrum(&buf, self.fft_length, self.fft_normalized);
            let mel_energies = apply_mel_filters(&power, &self.mel_filters);

            for &e in &mel_energies {
                let clamped = e.max(self.log_floor);
                let log_val = if self.use_log10 { clamped.log10() } else { clamped.ln() };
                all_features.push(log_val);
            }
        }

        Array2::from_shape_vec((num_frames, self.num_mel_bins), all_features)
            .unwrap_or_else(|_| Array2::zeros((num_frames, self.num_mel_bins)))
    }

    fn compute_standard(&self, audio: &[f32], frame_length: usize, frame_shift: usize) -> Array2<f32> {
        let num_frames = if audio.len() < frame_length { 1 } else { 1 + (audio.len() - frame_length) / frame_shift };

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(self.fft_length);

        let mut all_features = Vec::with_capacity(num_frames * self.num_mel_bins);

        for frame_idx in 0..num_frames {
            let start = if audio.len() < frame_length { 0 } else { frame_idx * frame_shift };

            let mut frame = vec![0.0f32; self.fft_length];
            let available = audio.len().saturating_sub(start);
            // Clamp to the frame buffer (see compute_centered): guards fft_length < frame_length.
            let copy_len = frame_length.min(available).min(frame.len());
            if copy_len > 0 {
                frame[..copy_len].copy_from_slice(&audio[start..start + copy_len]);
            }

            for (f, w) in frame.iter_mut().zip(self.window.iter()) {
                *f *= w;
            }

            let mut buf: Vec<Complex<f32>> = frame.iter().map(|&s| Complex::new(s, 0.0)).collect();
            fft.process(&mut buf);

            let power = power_spectrum(&buf, self.fft_length, self.fft_normalized);
            let mel_energies = apply_mel_filters(&power, &self.mel_filters);

            for &e in &mel_energies {
                let clamped = e.max(self.log_floor);
                let log_val = if self.use_log10 { clamped.log10() } else { clamped.ln() };
                all_features.push(log_val);
            }
        }

        Array2::from_shape_vec((num_frames, self.num_mel_bins), all_features)
            .unwrap_or_else(|_| Array2::zeros((num_frames, self.num_mel_bins)))
    }
}

fn apply_preemphasis(audio: &[f32], alpha: f32) -> Vec<f32> {
    if audio.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(audio.len());
    out.push(audio[0]);
    for i in 1..audio.len() {
        out.push(audio[i] - alpha * audio[i - 1]);
    }
    out
}

fn create_hanning_window(length: usize, periodic: bool) -> Vec<f32> {
    if length <= 1 {
        return vec![1.0f32; length];
    }
    let denom = if periodic { length as f32 } else { length as f32 - 1.0 };
    (0..length).map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / denom).cos())).collect()
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10.0f32.powf(mel / 2595.0) - 1.0)
}

fn create_mel_filters(
    num_mel_bins: usize,
    fft_length: usize,
    sample_rate: u32,
    low_freq: f32,
    high_freq: f32,
) -> Vec<Vec<f32>> {
    let num_fft_bins = fft_length / 2 + 1;
    let mel_low = hz_to_mel(low_freq);
    let mel_high = hz_to_mel(high_freq);

    let mel_points: Vec<f32> =
        (0..=num_mel_bins + 1).map(|i| mel_low + (mel_high - mel_low) * i as f32 / (num_mel_bins + 1) as f32).collect();

    let bin_points: Vec<f32> =
        mel_points.iter().map(|&m| fft_length as f32 * mel_to_hz(m) / sample_rate as f32).collect();

    let mut filters = Vec::with_capacity(num_mel_bins);
    for i in 0..num_mel_bins {
        let left = bin_points[i];
        let center = bin_points[i + 1];
        let right = bin_points[i + 2];

        let mut filter = vec![0.0f32; num_fft_bins];
        for (j, val) in filter.iter_mut().enumerate() {
            let jf = j as f32;
            if jf >= left && jf <= center && center > left {
                *val = (jf - left) / (center - left);
            } else if jf > center && jf <= right && right > center {
                *val = (right - jf) / (right - center);
            }
        }
        filters.push(filter);
    }

    filters
}

fn power_spectrum(buf: &[Complex<f32>], fft_length: usize, normalized: bool) -> Vec<f32> {
    let num_bins = fft_length / 2 + 1;
    let scale = if normalized { 1.0 / fft_length as f32 } else { 1.0 };
    (0..num_bins)
        .map(|i| {
            let re = buf[i].re * scale;
            let im = buf[i].im * scale;
            re * re + im * im
        })
        .collect()
}

fn apply_mel_filters(power: &[f32], filters: &[Vec<f32>]) -> Vec<f32> {
    filters.iter().map(|f| f.iter().zip(power.iter()).map(|(w, &p)| w * p).sum()).collect()
}

fn apply_whisper_mel_normalization(mel: &mut Array2<f32>) {
    let max_val = mel.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    if !max_val.is_finite() {
        return;
    }
    for val in mel.iter_mut() {
        *val = val.max(max_val - 8.0);
        *val = (*val + 4.0) / 4.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fbank_empty_audio() {
        let fbank = FbankExtractor::new(16000);
        let features = fbank.compute(&[]);
        assert_eq!(features.shape(), &[0, 80]);
    }

    #[test]
    fn fbank_short_audio() {
        let fbank = FbankExtractor::new(16000);
        let audio = vec![0.0f32; 100];
        let features = fbank.compute(&audio);
        assert_eq!(features.shape()[1], 80);
        assert!(features.shape()[0] >= 1);
    }

    #[test]
    fn compute_total_under_degenerate_small_fft_config() {
        // fft_length (64) smaller than the fixed 25ms frame_length (400 @ 16kHz) is a
        // constructible-but-nonsensical config; combined with short audio it used to
        // underflow (centered path) or index past the fft-sized frame buffer and window
        // (both paths). Mel extraction must be total: a well-shaped, finite matrix and
        // never a panic, for any config the public constructor accepts.
        for centered in [true, false] {
            let ex = FbankExtractor::new_custom(
                16000, 80, 64, 0.0, 0.0, 1e-10, centered, true, true, false, false,
            );
            let audio = vec![0.1f32; 100];
            let feats = ex.compute(&audio);
            assert_eq!(feats.ncols(), 80, "centered={centered}");
            assert!(feats.iter().all(|v| v.is_finite()), "centered={centered}");
        }
    }

    #[test]
    fn fbank_sine_wave() {
        let fbank = FbankExtractor::new(16000);
        let audio: Vec<f32> =
            (0..16000).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin()).collect();
        let features = fbank.compute(&audio);
        assert!(features.shape()[0] > 0);
        assert_eq!(features.shape()[1], 80);
        let has_nonzero = features.iter().any(|&v| v.abs() > 1e-6);
        assert!(has_nonzero, "fbank of sine wave should have non-zero energy");
    }

    #[test]
    fn fbank_num_frames() {
        let fbank = FbankExtractor::new(16000);
        let audio = vec![0.0f32; 16000];
        let features = fbank.compute(&audio);
        let expected_frames = 1 + (16000 - 400) / 160;
        assert_eq!(features.shape()[0], expected_frames);
    }

    #[test]
    fn hz_mel_roundtrip() {
        let hz = 1000.0f32;
        let mel = hz_to_mel(hz);
        let hz_back = mel_to_hz(mel);
        assert!((hz - hz_back).abs() < 0.01);
    }

    #[test]
    fn mel_filters_sum_shape() {
        let filters = create_mel_filters(80, 512, 16000, 20.0, 8000.0);
        assert_eq!(filters.len(), 80);
        assert_eq!(filters[0].len(), 257);
    }

    #[test]
    fn whisper_mel_128_bins() {
        let fbank = FbankExtractor::new_for_whisper(16000, 128);
        assert_eq!(fbank.num_mel_bins(), 128);
        let audio: Vec<f32> =
            (0..16000).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin()).collect();
        let features = fbank.compute(&audio);
        assert_eq!(features.shape()[1], 128);
        assert!(features.shape()[0] > 0);
        let has_nonzero = features.iter().any(|&v| v.abs() > 1e-6);
        assert!(has_nonzero, "Whisper mel of sine wave should have non-zero energy");
    }

    #[test]
    fn whisper_mel_no_preemphasis() {
        let fbank = FbankExtractor::new_for_whisper(16000, 128);
        assert_eq!(fbank.preemphasis, 0.0);
    }

    #[test]
    fn whisper_mel_fft_400() {
        let fbank = FbankExtractor::new_for_whisper(16000, 128);
        assert_eq!(fbank.fft_length, 400);
    }

    #[test]
    fn new_with_mel_128_uses_whisper_params() {
        let fbank = FbankExtractor::new_with_mel(16000, 128);
        assert_eq!(fbank.fft_length, 400);
        assert_eq!(fbank.preemphasis, 0.0);
        assert_eq!(fbank.mel_low_freq, 0.0);
        assert!((fbank.log_floor - 1e-10).abs() < 1e-15);
        assert!(fbank.centered);
        assert!(fbank.use_log10);
        assert!(!fbank.fft_normalized);
        assert!(fbank.whisper_norm);
    }

    #[test]
    fn new_with_mel_80_uses_standard_params() {
        let fbank = FbankExtractor::new_with_mel(16000, 80);
        assert_eq!(fbank.fft_length, 512);
        assert_eq!(fbank.preemphasis, 0.97);
        assert_eq!(fbank.mel_low_freq, 20.0);
        assert!(!fbank.centered);
        assert!(!fbank.whisper_norm);
    }

    #[test]
    fn whisper_mel_frames_from_30s() {
        let fbank = FbankExtractor::new_for_whisper(16000, 128);
        let audio = vec![0.0f32; 480000];
        let features = fbank.compute(&audio);
        assert_eq!(features.shape()[0], 3000);
        assert_eq!(features.shape()[1], 128);
    }

    #[test]
    fn whisper_mel_finite_values() {
        let fbank = FbankExtractor::new_for_whisper(16000, 128);
        let audio: Vec<f32> =
            (0..16000).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin()).collect();
        let features = fbank.compute(&audio);
        for val in features.iter() {
            assert!(val.is_finite(), "Whisper mel feature should be finite");
        }
    }

    #[test]
    fn whisper_mel_normalization_range() {
        let fbank = FbankExtractor::new_for_whisper(16000, 128);
        let audio: Vec<f32> =
            (0..16000).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin()).collect();
        let features = fbank.compute(&audio);
        let max_val = features.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let min_val = features.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!(max_val <= 2.0, "Whisper mel max should be <= 2.0 after normalization, got {max_val}");
        assert!(min_val >= -2.0, "Whisper mel min should be >= -2.0 after normalization, got {min_val}");
        assert!(
            (max_val - min_val) <= 2.1,
            "Whisper mel range should be ~2 after normalization, got range {}",
            max_val - min_val
        );
    }

    #[test]
    fn standard_mel_non_centered() {
        let fbank = FbankExtractor::new(16000);
        let audio = vec![0.0f32; 480000];
        let features = fbank.compute(&audio);
        let expected = 1 + (480000 - 400) / 160;
        assert_eq!(features.shape()[0], expected);
    }
}
