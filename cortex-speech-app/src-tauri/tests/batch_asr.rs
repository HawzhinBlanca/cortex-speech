use cortex_speech_app_lib::asr::KurdishAsrService;
use cortex_speech_app_lib::audio;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use std::path::{Path, PathBuf};
use std::time::Instant;

const REAL_AUDIO_DIR_ENV: &str = "CORTEX_REAL_AUDIO_DIR";

fn collect_audio_files(dir: &Path) -> Vec<PathBuf> {
    let exts = ["wav", "mp3", "flac", "m4a", "ogg", "aac", "opus", "mp4", "mov", "wma", "webm"];
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_audio_files(&path));
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if exts.contains(&ext.to_lowercase().as_str()) {
                        files.push(path);
                    }
                }
            }
        }
    }
    files.sort();
    files
}

#[test]
#[ignore]
fn test_batch_asr_sample() {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::new("warn")).try_init();

    let Some(chosen) = std::env::var_os(REAL_AUDIO_DIR_ENV).map(PathBuf::from).filter(|p| p.is_dir()) else {
        eprintln!("Set {REAL_AUDIO_DIR_ENV} to run the ignored batch ASR sample.");
        return;
    };

    let all_files = collect_audio_files(&chosen);
    eprintln!("Found {} audio files", all_files.len());

    let model_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    let mut asr = match KurdishAsrService::new(&model_dir, false) {
        Ok(s) if s.is_available() => s,
        _ => {
            eprintln!("ASR unavailable");
            return;
        }
    };

    let normalizer = SoraniNormalizer::new();

    let mut sample = Vec::new();
    let mut by_ext: std::collections::HashMap<String, Vec<PathBuf>> = std::collections::HashMap::new();
    for f in &all_files {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        by_ext.entry(ext).or_default().push(f.clone());
    }
    for files in by_ext.values() {
        for f in files.iter().take(5) {
            if !sample.contains(f) {
                sample.push(f.clone());
            }
        }
    }
    sample.sort();
    eprintln!("Testing {} sample files...", sample.len());

    let mut ok = 0usize;
    let mut err = 0usize;
    let mut empty = 0usize;
    let mut total_chars = 0usize;
    let mut total_time = 0.0f64;

    for (i, path) in sample.iter().enumerate() {
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");

        let decode_result = audio::decode_to_pcm_with_timeout(path, std::time::Duration::from_secs(120));

        match decode_result {
            Ok((sr, pcm)) => {
                let duration = pcm.len() as f64 / sr as f64;
                let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

                let asr_start = Instant::now();
                match asr.transcribe(&f32_pcm, sr) {
                    Ok(transcript) => {
                        let asr_elapsed = asr_start.elapsed().as_secs_f64();
                        let norm = normalizer.normalize(&transcript.0);
                        let tlen = transcript.0.len();
                        let has_kurdish = transcript.0.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c));

                        total_chars += tlen;
                        total_time += asr_elapsed;

                        if tlen == 0 {
                            empty += 1;
                        } else {
                            ok += 1;
                        }
                        let status = if tlen == 0 {
                            "EMPTY"
                        } else if !has_kurdish {
                            "NO_CKB"
                        } else {
                            "OK"
                        };

                        eprintln!(
                            "[{}/{}] {} {} {:.1}s asr={:.1}s {}ch [{}] {:.80}",
                            i + 1,
                            sample.len(),
                            ext,
                            fname,
                            duration,
                            asr_elapsed,
                            tlen,
                            status,
                            transcript.0
                        );
                        if !norm.is_empty() && norm != transcript.0 {
                            eprintln!("  norm: {:.80}", norm);
                        }
                    }
                    Err(e) => {
                        err += 1;
                        eprintln!("[{}/{}] {} ASR ERR: {}", i + 1, sample.len(), fname, e);
                    }
                }
            }
            Err(e) => {
                err += 1;
                eprintln!("[{}/{}] {} DECODE ERR: {}", i + 1, sample.len(), fname, e);
            }
        }
    }

    eprintln!("\n=== BATCH SUMMARY ===");
    eprintln!("Total: {}, ok: {}, empty: {}, errors: {}", sample.len(), ok, empty, err);
    eprintln!("Total chars: {}, Total ASR time: {:.1}s", total_chars, total_time);
    assert!(ok > 0, "At least some files should produce transcripts");
}
