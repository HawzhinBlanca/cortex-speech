mod fixtures;

use cortex_speech_app_lib::audio::{check_audio_file, compute_waveform, decode_to_pcm, voice_activity_detection};
use tempfile::TempDir;

#[test]
fn test_decode_to_pcm_on_valid_wav() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("valid.wav");
    fixtures::create_test_wav(&path, 1.0, 44100, 440.0).unwrap();

    let result = decode_to_pcm(&path);
    assert!(result.is_ok(), "decode_to_pcm should succeed on valid WAV: {:?}", result.err());

    let (sample_rate, pcm) = result.unwrap();
    assert_eq!(sample_rate, 16000, "Should be resampled to 16kHz");

    let expected_samples = 16000usize; // 1 sec at 16kHz
    assert!(
        (pcm.len() as isize - expected_samples as isize).abs() < 2,
        "PCM length {} should be close to {expected_samples}",
        pcm.len()
    );
    assert!(pcm.iter().any(|&s| s.abs() > 100), "Should contain non-silent samples");
}

#[test]
fn test_decode_to_pcm_resamples_different_rates() {
    let dir = TempDir::new().unwrap();

    for (rate, name) in &[(8000u32, "low.wav"), (22050, "mid.wav"), (44100, "high.wav"), (48000, "max.wav")] {
        let path = dir.path().join(name);
        fixtures::create_test_wav(&path, 0.5, *rate, 440.0).unwrap();

        let result = decode_to_pcm(path);
        assert!(result.is_ok(), "Failed for sample rate {rate}: {:?}", result.err());

        let (sr, pcm) = result.unwrap();
        assert_eq!(sr, 16000, "Should always be 16kHz, got {sr} for input {rate}");
        assert!(!pcm.is_empty(), "PCM data should not be empty for {rate}Hz input");
    }
}

#[test]
fn test_check_audio_file_valid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("valid.wav");
    fixtures::create_test_wav(&path, 0.5, 16000, 440.0).unwrap();

    let info = check_audio_file(&path).unwrap();
    assert_eq!(info.sample_rate, 16000);
    assert!(info.duration_ms > 0);
    assert_eq!(info.channels, 1);
    assert_eq!(info.format, "wav");
}

#[test]
fn test_check_audio_file_nonexistent() {
    let result = check_audio_file("C:\\nonexistent\\file_that_does_not_exist.wav");
    assert!(result.is_err(), "Should error on nonexistent file");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"), "Error should mention 'not found': {err}");
}

#[test]
fn test_check_audio_file_zero_bytes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.wav");
    std::fs::write(&path, []).unwrap();

    let result = check_audio_file(&path);
    assert!(result.is_err(), "Should error on empty file");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("empty"), "Error should mention 'empty': {err}");
}

#[test]
fn test_check_audio_file_corrupted() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("corrupt.wav");
    std::fs::write(&path, b"RIFFthis is not a valid wav file\x00\x00\x00\x00").unwrap();

    let result = check_audio_file(&path);
    assert!(result.is_err(), "Should error on corrupted file");
}

#[test]
fn test_compute_waveform_on_generated_audio() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sine.wav");
    fixtures::create_test_wav(&path, 0.5, 16000, 440.0).unwrap();

    let (_sr, pcm) = decode_to_pcm(&path).unwrap();
    let num_points = 50;
    let waveform = compute_waveform(&pcm, num_points);

    assert_eq!(waveform.len(), num_points, "Waveform should have exactly {num_points} points");
    assert!(waveform.iter().any(|&v| v > 0.1), "Waveform RMS should detect the sine wave");
}

#[test]
fn test_compute_waveform_returns_empty_for_empty_pcm() {
    let waveform = compute_waveform(&[], 100);
    assert!(waveform.is_empty());
}

#[test]
fn test_compute_waveform_zero_points() {
    let pcm = vec![0i16; 16000];
    let waveform = compute_waveform(&pcm, 0);
    assert!(waveform.is_empty());
}

#[test]
fn test_vad_silence_returns_entire_buffer() {
    let pcm = vec![0i16; 48000];
    let segments = voice_activity_detection(&pcm, 16000, 0.5).unwrap();

    // Silence should produce a single segment covering the whole buffer
    assert_eq!(segments.len(), 1, "Silence should produce exactly one segment");
    assert_eq!(segments[0].0, 0, "Segment should start at 0");
    assert_eq!(segments[0].1, 48000, "Segment should cover the entire buffer");
}

#[test]
fn test_vad_signal_detects_speech() {
    let pcm: Vec<i16> = (0..48000)
        .map(|i| (i16::MAX as f64 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16000.0).sin()) as i16)
        .collect();

    let segments = voice_activity_detection(&pcm, 16000, 0.5).unwrap();

    // Signal should be detected as speech (at least some region)
    assert!(!segments.is_empty(), "VAD should detect speech regions");
    for &(start, end) in &segments {
        assert!(end > start, "Each segment must have positive length");
    }
}

#[test]
fn test_vad_returns_empty_for_empty_input() {
    let segments = voice_activity_detection(&[], 16000, 0.5).unwrap();
    assert!(segments.is_empty(), "Empty input should produce no segments");
}

#[test]
fn test_vad_uses_different_thresholds() {
    let pcm: Vec<i16> = (0..16000)
        .map(|i| (i16::MAX as f64 * 0.01 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16000.0).sin()) as i16)
        .collect();

    // Low threshold should detect more speech than high threshold
    let low_seg = voice_activity_detection(&pcm, 16000, 0.01).unwrap();
    let high_seg = voice_activity_detection(&pcm, 16000, 10.0).unwrap();

    let low_speech = low_seg.iter().map(|&(s, e)| e - s).sum::<usize>();
    let high_speech = high_seg.iter().map(|&(s, e)| e - s).sum::<usize>();

    assert!(low_speech >= high_speech, "Lower threshold should detect at least as much speech as high threshold");
}
