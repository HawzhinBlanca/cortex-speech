#![allow(dead_code)]

use hound::{WavSpec, WavWriter};
use std::path::Path;

/// Generate a valid WAV file with a sine wave at the given frequency.
pub fn create_test_wav(path: &Path, duration_secs: f64, sample_rate: u32, frequency: f64) -> hound::Result<()> {
    let spec = WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = WavWriter::create(path, spec)?;
    let num_samples = (sample_rate as f64 * duration_secs) as u64;
    for i in 0..num_samples {
        let t = i as f64 / sample_rate as f64;
        let sample = (i16::MAX as f64 * (2.0 * std::f64::consts::PI * frequency * t).sin()) as i16;
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Generate a valid WAV file with silence (all zeros).
pub fn create_silent_wav(path: &Path, duration_secs: f64, sample_rate: u32) -> hound::Result<()> {
    let spec = WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = WavWriter::create(path, spec)?;
    let num_samples = (sample_rate as f64 * duration_secs) as u64;
    for _ in 0..num_samples {
        writer.write_sample(0i16)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Create a WAV file with only a header and no audio samples.
pub fn create_empty_wav(path: &Path, sample_rate: u32) -> hound::Result<()> {
    let spec = WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let writer = WavWriter::create(path, spec)?;
    writer.finalize()?;
    Ok(())
}
