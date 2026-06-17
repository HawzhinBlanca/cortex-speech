//! Smoke tests for long-form audiobook MP3 import (decode + chunk planning).
//! Run: CORTEX_AUDIOBOOK_MP3="path\to\file.mp3" cargo test --test audiobook_smoke -- --ignored --nocapture

use cortex_speech_app_lib::audio;
use cortex_speech_app_lib::chunking;
use cortex_speech_app_lib::settings::AppSettings;

fn audiobook_path() -> Option<std::path::PathBuf> {
    std::env::var("CORTEX_AUDIOBOOK_MP3").ok().map(std::path::PathBuf::from).filter(|p| p.exists())
}

#[test]
#[ignore]
fn audiobook_mp3_decode_and_chunk_plan() {
    let path = match audiobook_path() {
        Some(p) => p,
        None => {
            eprintln!("[audiobook_smoke] skip: set CORTEX_AUDIOBOOK_MP3 to an MP3 fixture");
            return;
        }
    };

    eprintln!("[audiobook_smoke] file: {}", path.display());

    let info = audio::check_audio_file(&path).expect("check_audio_file");
    eprintln!(
        "[audiobook_smoke] duration={:.1}min sr={} fmt={}",
        info.duration_ms as f64 / 60_000.0,
        info.sample_rate,
        info.format
    );
    assert!(info.duration_ms > 60_000, "expected long audiobook");

    let settings = AppSettings::default();
    assert!(
        chunking::should_stream_decode(info.duration_ms, settings.max_segment_duration_ms),
        "long audiobook should use streaming decode"
    );

    let mut window_count = 0usize;
    let mut chunk_count = 0usize;
    let start = std::time::Instant::now();

    audio::decode_pcm_windows(&path, audio::DECODE_WINDOW_MS, |window| {
        window_count += 1;
        let (sample_rate, pcm) = audio::ensure_pcm_16khz(window.sample_rate, window.pcm).expect("resample");
        let ranges = chunking::plan_speech_chunks(
            &pcm,
            sample_rate,
            settings.vad_threshold,
            settings.min_segment_duration_ms,
            settings.max_segment_duration_ms,
        )
        .expect("plan chunks");
        chunk_count += ranges.len();
        if window_count <= 3 {
            eprintln!("  window {} offset_ms={} chunks={}", window_count, window.offset_ms, ranges.len());
        }
        Ok(())
    })
    .expect("streaming decode");

    eprintln!(
        "[audiobook_smoke] windows={} total_chunks={} elapsed={:.1}s",
        window_count,
        chunk_count,
        start.elapsed().as_secs_f64()
    );

    assert!(window_count >= 2, "expected multiple decode windows");
    assert!(chunk_count >= 10, "expected many VAD chunks for audiobook, got {chunk_count}");
}

#[test]
#[ignore]
fn audiobook_mp3_fingerprint_reimport_not_duplicate() {
    use cortex_speech_app_lib::fingerprint::AudioFingerprint;

    let path = match audiobook_path() {
        Some(p) => p,
        None => return,
    };

    let (_sr, pcm) = audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(120)).expect("decode");
    let fp = AudioFingerprint::new();

    assert!(
        !fp.check_duplicate(&pcm[..pcm.len().min(160_000)], 16000, Some(&path)),
        "first import window should not be duplicate"
    );
    fp.register(&pcm[..pcm.len().min(160_000)], 16000, Some(&path));

    let is_dup = fp.check_duplicate(&pcm[..pcm.len().min(160_000)], 16000, Some(&path));
    eprintln!("[audiobook_smoke] re-import duplicate={is_dup}");
    assert!(!is_dup, "re-importing the same audiobook must not be rejected as duplicate");
}
