mod fixtures;

use cortex_speech_app_lib::audio::{check_audio_file, compute_waveform, decode_to_pcm};
use std::path::Path;
use tempfile::TempDir;

/// Property: decode_to_pcm always returns Ok((16000, pcm)) for any valid WAV,
/// and the PCM length matches the expected duration at 16kHz.
#[test]
fn prop_decode_valid_wav_always_ok() {
    let dir = TempDir::new().unwrap();

    let test_cases = [
        (1.0, 16000u32, 440.0f64),
        (2.0, 44100, 880.0),
        (0.25, 22050, 220.0),
        (0.5, 48000, 1000.0),
        (3.0, 8000, 330.0),
    ];

    for (i, &(duration, sample_rate, frequency)) in test_cases.iter().enumerate() {
        let path = dir.path().join(format!("prop_{i}.wav"));
        fixtures::create_test_wav(&path, duration, sample_rate, frequency).unwrap();

        let result = decode_to_pcm(&path);
        assert!(result.is_ok(), "decode_to_pcm failed for {duration}s {sample_rate}Hz WAV: {:?}", result.err());

        let (sr, pcm) = result.unwrap();
        assert_eq!(sr, 16000, "Sample rate must be 16000 for all inputs");

        let expected_samples = (duration * 16000.0) as usize;
        let tolerance = 2;
        assert!(
            (pcm.len() as isize - expected_samples as isize).abs() <= tolerance as isize,
            "PCM length {len} should be ~{expected_samples} for {duration}s @ 16000Hz",
            len = pcm.len()
        );

        // PCM should contain non-zero samples (sine wave)
        assert!(pcm.iter().any(|&s| s.abs() > 100), "PCM should contain non-silent samples for sine wave input");
    }
}

/// Property: check_audio_file always returns AudioInfo with positive duration
/// for any valid WAV file.
#[test]
fn prop_check_audio_valid_always_returns_info() {
    let dir = TempDir::new().unwrap();

    let test_cases = [(0.1, 8000u32), (0.5, 16000), (1.0, 44100), (2.0, 48000), (5.0, 22050)];

    for (i, &(duration, sample_rate)) in test_cases.iter().enumerate() {
        let path = dir.path().join(format!("check_{i}.wav"));
        fixtures::create_test_wav(&path, duration, sample_rate, 440.0).unwrap();

        let info = check_audio_file(&path).unwrap();
        assert!(info.duration_ms > 0, "Duration must be positive for {duration}s WAV, got {}ms", info.duration_ms);
        assert_eq!(info.channels, 1, "Channel count should be 1");
        assert_eq!(info.format, "wav", "Format should be 'wav'");
    }
}

/// Property: empty WAV files (header only, no samples) return appropriate error.
#[test]
fn prop_empty_wav_returns_error() {
    let dir = TempDir::new().unwrap();

    // An empty WAV has header but no audio data — decode_to_pcm should bail
    let path = dir.path().join("empty_samples.wav");
    fixtures::create_empty_wav(&path, 16000).unwrap();

    let result = decode_to_pcm(&path);
    assert!(result.is_err(), "decode_to_pcm should error on empty WAV (header with no samples)");
}

/// Property: zero-byte files return error from check_audio_file.
#[test]
fn prop_zero_byte_file_returns_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty_file.wav");
    std::fs::write(&path, []).unwrap();
    let result = check_audio_file(&path);
    assert!(result.is_err(), "check_audio_file should error on zero-byte file");
}

/// Property: nonexistent files return error from both decode_to_pcm and check_audio_file.
#[test]
fn prop_nonexistent_file_returns_error() {
    let path = Path::new("C:\\nonexistent_path_that_should_not_exist_12345.wav");
    assert!(decode_to_pcm(path).is_err(), "decode_to_pcm should error on nonexistent file");
    assert!(check_audio_file(path).is_err(), "check_audio_file should error on nonexistent file");
}

/// Property: corrupted WAV header returns error.
#[test]
fn prop_corrupted_wav_header_returns_error() {
    let dir = TempDir::new().unwrap();
    let corrupt_data = b"RIFF----WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x40\x1f\x00\x00\x00\x00\x00\x00\x02\x00\x10\x00data----CORRUPTED";
    let path = dir.path().join("corrupted.wav");
    std::fs::write(&path, corrupt_data).unwrap();

    let result = decode_to_pcm(&path);
    assert!(result.is_err(), "decode_to_pcm should error on corrupted WAV");
}

/// Property: compute_waveform always returns exactly `num_points` values
/// given sufficient PCM data, and all values are in [0.0, 1.0].
#[test]
fn prop_compute_waveform_length_and_range() {
    let test_lengths = [0, 1, 10, 50, 100, 1000];
    for &num_points in &test_lengths {
        let pcm: Vec<i16> = (0..16000)
            .map(|i| (i16::MAX as f64 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16000.0).sin()) as i16)
            .collect();

        let waveform = compute_waveform(&pcm, num_points);

        if num_points == 0 {
            assert!(waveform.is_empty(), "Zero points should return empty");
            continue;
        }

        let expected = num_points.min(pcm.len());
        assert_eq!(waveform.len(), expected, "Waveform should have {expected} points, got {}", waveform.len());

        for &v in &waveform {
            assert!((0.0..=1.0).contains(&v), "RMS value {v} must be in [0.0, 1.0]");
        }
    }
}
