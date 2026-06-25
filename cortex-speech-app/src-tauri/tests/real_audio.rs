mod fixtures;

use cortex_speech_app_lib::asr::KurdishAsrService;
use cortex_speech_app_lib::audio::{self, check_audio_file, voice_activity_detection};
use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::eval;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::AppSettings;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;

const REAL_AUDIO_DIR_ENV: &str = "CORTEX_REAL_AUDIO_DIR";
const SUPPORTED_AUDIO_EXTENSIONS: &[&str] =
    &["wav", "mp3", "flac", "m4a", "ogg", "aac", "opus", "mp4", "mov", "webm", "wma"];

fn real_audio_root() -> Option<PathBuf> {
    std::env::var(REAL_AUDIO_DIR_ENV).ok().map(PathBuf::from).filter(|p| p.is_dir())
}

fn fixture_path(name: &str) -> PathBuf {
    let candidate = PathBuf::from(name);
    if candidate.is_absolute() {
        return candidate;
    }
    real_audio_root().unwrap_or_default().join(name)
}

fn fixture_files_with_extensions(extensions: &[&str]) -> Vec<PathBuf> {
    let Some(root) = real_audio_root() else {
        return Vec::new();
    };

    let normalized_extensions: Vec<String> =
        extensions.iter().map(|extension| extension.trim_start_matches('.').to_ascii_lowercase()).collect();
    let mut files = Vec::new();
    collect_fixture_files(&root, &normalized_extensions, &mut files);
    files.sort();
    files
}

fn first_fixture_with_extensions(extensions: &[&str]) -> Option<PathBuf> {
    fixture_files_with_extensions(extensions).into_iter().next()
}

fn collect_fixture_files(dir: &Path, extensions: &[String], files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_fixture_files(&path, extensions, files);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extensions.iter().any(|allowed| allowed == &extension.to_ascii_lowercase()))
            .unwrap_or(false)
        {
            files.push(path);
        }
    }
}

fn setup_pipeline() -> (ProcessingPipeline, String, TempDir) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("real_test.db");
    let db_path_str = db_path.to_str().unwrap().to_string();
    let db = Database::open(&db_path_str).unwrap();
    db.initialize().unwrap();
    drop(db);

    let pipeline = ProcessingPipeline::new(
        db_path_str.clone(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(100)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(AppSettings::default()),
        Arc::new(ModelManager::new(tmp.path().to_path_buf())),
    );
    (pipeline, db_path_str, tmp)
}

// ── Audio Decode Tests ──────────────────────────────────────────────────

#[test]
#[ignore]
fn test_decode_mp4_small() {
    let Some(path) = first_fixture_with_extensions(&["mp4"]) else {
        eprintln!("[mp4] skip: no mp4 fixture found under {REAL_AUDIO_DIR_ENV}");
        return;
    };

    let start = Instant::now();
    let info = check_audio_file(&path).expect("check_audio_file should succeed on mp4");
    eprintln!(
        "[mp4] {} — {}ms, {}Hz, {}ch, format={}",
        path.file_name().unwrap().to_string_lossy(),
        info.duration_ms,
        info.sample_rate,
        info.channels,
        info.format
    );
    assert!(info.duration_ms > 0, "Duration should be > 0");
    assert!(info.sample_rate > 0, "Sample rate should be > 0");

    let (sr, pcm) = audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(30))
        .expect("decode_to_pcm should succeed on mp4");
    eprintln!(
        "[mp4] Decoded: sr={}, samples={}, duration={:.1}s, took {:.2}s",
        sr,
        pcm.len(),
        pcm.len() as f64 / sr as f64,
        start.elapsed().as_secs_f64()
    );
    assert_eq!(sr, 16000, "Should resample to 16kHz");
    assert!(!pcm.is_empty(), "PCM should not be empty");
}

#[test]
#[ignore]
fn test_decode_mp4_batch_small() {
    let files: Vec<PathBuf> = fixture_files_with_extensions(&["mp4"]).into_iter().take(10).collect();

    if files.is_empty() {
        return;
    }
    eprintln!("[mp4 batch] Testing {} files", files.len());

    let mut ok = 0;
    let mut err = 0;
    for f in &files {
        match audio::decode_to_pcm_with_timeout(f, std::time::Duration::from_secs(30)) {
            Ok((sr, pcm)) => {
                eprintln!(
                    "  OK: {} — {} samples ({:.1}s)",
                    f.file_name().unwrap().to_string_lossy(),
                    pcm.len(),
                    pcm.len() as f64 / sr as f64
                );
                assert_eq!(sr, 16000);
                ok += 1;
            }
            Err(e) => {
                eprintln!("  ERR: {} — {e}", f.file_name().unwrap().to_string_lossy());
                err += 1;
            }
        }
    }
    eprintln!("[mp4 batch] {ok} ok, {err} err out of {} files", files.len());
    assert!(ok > 0, "At least some mp4 files should decode");
}

#[test]
#[ignore]
fn test_decode_any_supported_audio() {
    let Some(path) = first_fixture_with_extensions(SUPPORTED_AUDIO_EXTENSIONS) else {
        eprintln!("[decode any] skip: no supported audio fixture found under {REAL_AUDIO_DIR_ENV}");
        return;
    };

    let info = check_audio_file(&path).expect("check_audio_file should succeed on supported audio fixture");
    eprintln!(
        "[decode any] {} — {}ms, {}Hz, {}ch, format={}",
        path.display(),
        info.duration_ms,
        info.sample_rate,
        info.channels,
        info.format
    );
    assert!(info.duration_ms > 0, "Duration should be > 0");
    assert!(info.sample_rate > 0, "Sample rate should be > 0");

    let (sr, pcm) = audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(30))
        .expect("decode_to_pcm should succeed on supported audio fixture");
    assert_eq!(sr, 16000, "Should resample to 16kHz");
    assert!(!pcm.is_empty(), "PCM should not be empty");
}

#[test]
#[ignore]
fn test_decode_flac_small() {
    let Some(path) = first_fixture_with_extensions(&["flac"]) else {
        eprintln!("[flac] skip: no flac fixture found under {REAL_AUDIO_DIR_ENV}");
        return;
    };

    let start = Instant::now();
    let info = check_audio_file(&path).expect("check_audio_file on flac");
    eprintln!(
        "[flac] {} — {}ms, {}Hz, {}ch",
        path.file_name().unwrap().to_string_lossy(),
        info.duration_ms,
        info.sample_rate,
        info.channels
    );

    let (sr, pcm) =
        audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(120)).expect("decode_to_pcm on flac");
    eprintln!(
        "[flac] Decoded: sr={}, samples={}, duration={:.1}s, took {:.2}s",
        sr,
        pcm.len(),
        pcm.len() as f64 / sr as f64,
        start.elapsed().as_secs_f64()
    );
    assert_eq!(sr, 16000);
    assert!(!pcm.is_empty());
}

#[test]
#[ignore]
fn test_decode_mov_small() {
    let Some(path) = first_fixture_with_extensions(&["mov"]) else {
        eprintln!("[mov] skip: no mov fixture found under {REAL_AUDIO_DIR_ENV}");
        return;
    };

    let start = Instant::now();
    let info = check_audio_file(&path).expect("check_audio_file on mov");
    eprintln!(
        "[mov] {} — {}ms, {}Hz, {}ch",
        path.file_name().unwrap().to_string_lossy(),
        info.duration_ms,
        info.sample_rate,
        info.channels
    );

    let (sr, pcm) =
        audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(30)).expect("decode_to_pcm on mov");
    eprintln!(
        "[mov] Decoded: sr={}, samples={}, duration={:.1}s, took {:.2}s",
        sr,
        pcm.len(),
        pcm.len() as f64 / sr as f64,
        start.elapsed().as_secs_f64()
    );
    assert_eq!(sr, 16000);
    assert!(!pcm.is_empty());
}

#[test]
#[ignore]
fn test_decode_mov_batch() {
    let files: Vec<PathBuf> = fixture_files_with_extensions(&["mov"]).into_iter().take(10).collect();

    if files.is_empty() {
        return;
    }
    eprintln!("[mov batch] Testing {} files", files.len());

    let mut ok = 0;
    let mut err = 0;
    for f in &files {
        match audio::decode_to_pcm_with_timeout(f, std::time::Duration::from_secs(30)) {
            Ok((sr, pcm)) => {
                eprintln!(
                    "  OK: {} — {} samples ({:.1}s)",
                    f.file_name().unwrap().to_string_lossy(),
                    pcm.len(),
                    pcm.len() as f64 / sr as f64
                );
                assert_eq!(sr, 16000);
                ok += 1;
            }
            Err(e) => {
                eprintln!("  ERR: {} — {e}", f.file_name().unwrap().to_string_lossy());
                err += 1;
            }
        }
    }
    eprintln!("[mov batch] {ok} ok, {err} err out of {} files", files.len());
    assert!(ok > 0, "At least some mov files should decode");
}

// ── VAD Tests ───────────────────────────────────────────────────────────

#[test]
#[ignore]
fn test_vad_on_real_kurdish_audio() {
    let path = fixture_path("A1-0001_PODCAST-001.mp4");
    if !path.exists() {
        return;
    }

    let (sr, pcm) = audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(30)).expect("decode");
    assert_eq!(sr, 16000);

    let start = Instant::now();
    let vad_segments = voice_activity_detection(&pcm, 16000, 0.5).expect("VAD should succeed on real audio");
    eprintln!(
        "[VAD] Found {} speech segments in {:.1}s audio, took {:.3}s",
        vad_segments.len(),
        pcm.len() as f64 / 16000.0,
        start.elapsed().as_secs_f64()
    );

    for (i, (start_s, end_s)) in vad_segments.iter().enumerate().take(5) {
        let start_sec = *start_s as f64 / 16000.0;
        let end_sec = *end_s as f64 / 16000.0;
        eprintln!("  Segment {}: {:.2}s — {:.2}s ({:.2}s)", i, start_sec, end_sec, end_sec - start_sec);
    }

    assert!(!vad_segments.is_empty(), "Real Kurdish audio should have speech segments");
}

// ── Normalizer Tests ────────────────────────────────────────────────────

#[test]
#[ignore]
fn test_normalizer_on_kurdish_text() {
    let normalizer = SoraniNormalizer::new();

    let test_cases = vec![
        ("دەکتۆر کوردی", "دەکتۆر کوردی"),
        ("كوردی", "کوردی"),           // Arabic Kaf → Kurdish Kaf
        ("يەک", "یەک"),               // Arabic Yeh → Kurdish Yeh
        ("نـــاو", "ناو"),            // Tatweel removed
        ("ڕۆژی   چاکە", "ڕۆژی چاکە"), // Consecutive spaces collapsed
    ];

    for (input, expected) in test_cases {
        let result = normalizer.normalize(input);
        eprintln!("[normalizer] '{}' → '{}'", input, result);
        assert_eq!(result, expected, "Normalization mismatch for '{input}'");
    }

    let twice = normalizer.normalize(&normalizer.normalize("كوردی يەك"));
    let once = normalizer.normalize("كوردی يەك");
    assert_eq!(once, twice, "Normalizer must be idempotent");
}

// ── Full Pipeline Test ──────────────────────────────────────────────────

#[test]
#[ignore]
fn test_pipeline_import_supported_audio_directory() {
    let Some(dir) = real_audio_root() else {
        eprintln!("[real_audio] skip: set {REAL_AUDIO_DIR_ENV} to run pipeline import test");
        return;
    };

    let (pipeline, db_path, _tmp) = setup_pipeline();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let evt = events.clone();

    let start = Instant::now();
    let result = pipeline.import_directory(&dir, None, move |e| {
        evt.lock().unwrap().push(format!("{e:?}"));
    });
    let elapsed = start.elapsed();

    eprintln!("[pipeline] import_directory took {:.2}s", elapsed.as_secs_f64());

    match &result {
        Ok(()) => eprintln!("[pipeline] Import completed successfully"),
        Err(e) => eprintln!("[pipeline] Import error: {e}"),
    }

    let db = Database::open(&db_path).unwrap();
    let segments = db.get_segments(None).unwrap();
    eprintln!("[pipeline] {} segments in DB", segments.len());
    for seg in segments.iter().take(5) {
        eprintln!(
            "  {} — {}ms, transcript='{}', normalized='{}'",
            seg.id,
            seg.duration_ms,
            seg.raw_transcript.chars().take(40).collect::<String>(),
            seg.normalized_transcript
                .as_deref()
                .map(|s| s.chars().take(40).collect::<String>())
                .unwrap_or_else(|| "(none)".to_string())
        );
    }

    let events = events.lock().unwrap();
    let completed = events.iter().find(|e| e.contains("Completed"));
    if let Some(c) = completed {
        eprintln!("[pipeline] Final event: {c}");
    }

    assert!(result.is_ok(), "Pipeline import should succeed");
    assert!(!segments.is_empty(), "Should have imported some segments");
}

#[test]
#[ignore]
fn test_pipeline_process_single_supported_audio() {
    let Some(path) = first_fixture_with_extensions(SUPPORTED_AUDIO_EXTENSIONS) else {
        eprintln!("[single file] skip: no supported audio fixture found under {REAL_AUDIO_DIR_ENV}");
        return;
    };

    let (pipeline, db_path, _tmp) = setup_pipeline();
    let db = Database::open(&db_path).unwrap();

    let start = Instant::now();
    let segments =
        pipeline.process_single_file(&path, &db).expect("process_single_file should succeed on real audio fixture");
    let elapsed = start.elapsed();

    let segment = segments.first().expect("at least one segment");
    eprintln!(
        "[single file] {} segment(s), first {} — {}ms, transcript='{}', took {:.2}s",
        segments.len(),
        segment.id,
        segment.duration_ms,
        segment.raw_transcript.chars().take(40).collect::<String>(),
        elapsed.as_secs_f64()
    );
    eprintln!("[single file] Normalized: '{}'", segment.normalized_transcript.as_deref().unwrap_or("(none)"));

    assert!(segment.duration_ms > 0, "Duration should be > 0");
}

// ── Fbank Feature Extraction on Real Audio ──────────────────────────────

#[test]
#[ignore]
fn test_fbank_on_real_audio() {
    let Some(path) = first_fixture_with_extensions(SUPPORTED_AUDIO_EXTENSIONS) else {
        eprintln!("[fbank] skip: no supported audio fixture found under {REAL_AUDIO_DIR_ENV}");
        return;
    };

    let (_sr, pcm) = audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(30)).expect("decode");

    let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

    let fbank = cortex_speech_app_lib::features::FbankExtractor::new(16000);
    let start = Instant::now();
    let features = fbank.compute(&f32_pcm);
    let elapsed = start.elapsed();

    eprintln!("[fbank] Shape: ({}, {}), took {:.3}s", features.shape()[0], features.shape()[1], elapsed.as_secs_f64());

    let has_nonzero = features.iter().any(|&v| v.abs() > 1e-6);
    assert!(has_nonzero, "Real audio fbank should have non-zero values");
    assert_eq!(features.shape()[1], 80, "Should have 80 mel bins");

    let max_val = features.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    eprintln!("[fbank] Max absolute value: {max_val:.4}");
}

// ── Large File Stress Test ──────────────────────────────────────────────

#[test]
#[ignore]
fn test_decode_large_flac() {
    let Some(path) = first_fixture_with_extensions(&["flac"]) else {
        eprintln!("[large flac] skip: no flac fixture found under {REAL_AUDIO_DIR_ENV}");
        return;
    };

    let info = check_audio_file(&path).expect("check");
    eprintln!(
        "[large flac] {}ms, {}Hz, {}ch — {:.0}MB file",
        info.duration_ms,
        info.sample_rate,
        info.channels,
        std::fs::metadata(&path).unwrap().len() as f64 / 1_048_576.0
    );

    let start = Instant::now();
    let (sr, pcm) =
        audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(300)).expect("decode large flac");
    eprintln!(
        "[large flac] Decoded: sr={}, {} samples ({:.1}s), took {:.1}s",
        sr,
        pcm.len(),
        pcm.len() as f64 / sr as f64,
        start.elapsed().as_secs_f64()
    );
    assert_eq!(sr, 16000);
}

// ── OmniASR on Real Kurdish Audio ──────────────────────────────────────

fn omniasr_fixture() -> String {
    std::env::var("CORTEX_OMNIASR_FIXTURE").unwrap_or_else(|_| "A1-0001_PODCAST-001.mp4".to_string())
}

#[test]
#[ignore]
fn test_omniasr_on_real_audio() {
    let path = fixture_path(&omniasr_fixture());
    if !path.exists() {
        return;
    }

    let model_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    let model_path = model_dir.join("omniasr-ctc-300m/model.int8.onnx");
    let tokens_path = model_dir.join("omniasr-ctc-300m/tokens.txt");
    if !model_path.exists() || !tokens_path.exists() {
        eprintln!("[omniasr] Skipping: OmniASR CTC 300M models not found under {}", model_dir.display());
        return;
    }

    let info = check_audio_file(&path).expect("check_audio_file");
    eprintln!("[omniasr] File: {}", path.display());
    eprintln!("[omniasr] check_audio_file: duration={:.2}s, format={}", info.duration_ms as f64 / 1000.0, info.format);

    let decode_start = Instant::now();
    let (sr, pcm) = audio::decode_to_pcm_with_timeout(&path, std::time::Duration::from_secs(120)).expect("decode");
    let decode_elapsed = decode_start.elapsed();
    let duration_secs = pcm.len() as f64 / sr as f64;
    eprintln!(
        "[omniasr] decode_to_pcm: ok, sr={}, samples={}, duration={:.2}s, took {:.2}s",
        sr,
        pcm.len(),
        duration_secs,
        decode_elapsed.as_secs_f64()
    );
    assert_eq!(sr, 16000);

    let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

    let mut asr = KurdishAsrService::new(&model_dir, false).expect("ASR service should initialize with real models");
    assert!(asr.is_available(), "ASR should be available");

    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::new("info")).try_init();

    eprintln!("[omniasr] Transcribing {:.1}s of Kurdish audio...", duration_secs);
    let start = Instant::now();
    let transcript = asr.transcribe(&f32_pcm, 16000).expect("OmniASR transcription should succeed");
    let elapsed = start.elapsed();

    eprintln!("[omniasr] Transcript ({} chars, confidence: {:?}):\n{}", transcript.0.len(), transcript.1, transcript.0);
    eprintln!("[omniasr] Inference took {:.2}s", elapsed.as_secs_f64());

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/omniasr_transcript.txt");
    let _ = std::fs::write(&out, &transcript.0);
    eprintln!("[omniasr] Wrote transcript to {}", out.display());

    assert!(
        !transcript.0.is_empty() || f32_pcm.iter().all(|&s| s.abs() < 1e-6),
        "Transcript should not be empty for real Kurdish speech (unless audio is silence)"
    );

    assert!(transcript.1.is_some(), "ASR confidence score should be returned (not None)");
    let conf = transcript.1.unwrap();
    assert!(conf > 0.0 && conf <= 1.0, "ASR confidence score {} should be in (0.0, 1.0]", conf);
}

/// Default real-ASR gate on a COMMITTED, CC-BY-licensed fixture (Google FLEURS `ckb_iq`, see
/// tests/fixtures/ATTRIBUTION.md). Unlike the `#[ignore]`d tests above, this needs no external
/// CORTEX_REAL_AUDIO_DIR — the audio is in-repo — so it runs from a fresh clone once the OmniASR
/// model is present (`npm run fetch-models`). It skips cleanly when the model isn't fetched, and
/// asserts the real OmniASR produces a non-blank Kurdish (Arabic-script) transcript (no-fabrication).
#[test]
fn omniasr_on_committed_fleurs_ckb_fixture() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fleurs_ckb_sample.wav");
    if !fixture.exists() {
        eprintln!("[fleurs-gate] committed fixture missing; skipping");
        return;
    }
    let model_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    if !model_dir.join("omniasr-ctc-300m/model.int8.onnx").exists() {
        eprintln!("[fleurs-gate] OmniASR model not present (run `npm run fetch-models`); skipping");
        return;
    }

    let (sr, pcm) = audio::decode_to_pcm_with_timeout(&fixture, std::time::Duration::from_secs(120))
        .expect("decode committed fixture");
    assert_eq!(sr, 16000);
    let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

    let mut asr = KurdishAsrService::new(&model_dir, false).expect("ASR init with real models");
    assert!(asr.is_available(), "ASR should be available");
    let (text, _conf) = asr.transcribe(&f32_pcm, 16000).expect("OmniASR transcription should succeed");
    let trimmed = text.trim();
    eprintln!("[fleurs-gate] transcript ({} chars): {trimmed}", trimmed.len());

    assert!(!trimmed.is_empty(), "real ASR must not return a blank transcript (no-fabrication guard)");
    let arabic = trimmed
        .chars()
        .filter(|c| ('\u{0600}'..='\u{06FF}').contains(c) || ('\u{0750}'..='\u{077F}').contains(c))
        .count();
    assert!(arabic > 0, "expected Kurdish (Arabic-script) output, got: {trimmed}");
}

// ── M3: closed-loop gold eval against the REAL OmniASR engine ───────────

#[test]
#[ignore]
fn test_gold_eval_real_asr_closes_the_loop() {
    // End-to-end proof of M3: `run_gold_eval_with_transcriber` drives the real OmniASR
    // engine over a gold clip and scores the produced hypothesis — an honest CER from
    // audio, never caller-supplied text. Runs in the nightly real-audio job (the WER/CER
    // math itself is covered by the unit + property tests).
    let path = fixture_path(&omniasr_fixture());
    if !path.exists() {
        eprintln!("[gold-asr] skip: fixture {} not found", path.display());
        return;
    }
    let model_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    if !model_dir.join("omniasr-ctc-300m/model.int8.onnx").exists() {
        eprintln!("[gold-asr] skip: OmniASR models not found under {}", model_dir.display());
        return;
    }

    let db = Database::open(":memory:").expect("open db");
    db.initialize().expect("init db");
    eval::import_gold_segments(
        &db,
        vec![eval::GoldSegmentInput {
            audio_path: path.to_string_lossy().to_string(),
            reference: String::new(), // unknown reference: assert a non-blank hypothesis + valid score
            is_holdout: true,
        }],
    )
    .expect("import gold");

    let mut asr = KurdishAsrService::new(&model_dir, false).expect("ASR init with real models");
    assert!(asr.is_available(), "ASR should be available");

    let result = eval::run_gold_eval_with_transcriber(&db, "omniasr-ctc-300m", |seg| {
        let (sr, pcm) = audio::decode_to_pcm(&seg.audio_path)?;
        let (_sr, pcm16) = audio::ensure_pcm_16khz(sr, pcm)?;
        let f32_pcm: Vec<f32> = pcm16.iter().map(|&s| s as f32 / 32768.0).collect();
        asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
            .map(|(text, _conf)| text)
            .map_err(cortex_speech_app_lib::error::AppError::Other)
    })
    .expect("gold eval with real ASR");

    assert_eq!(result.run.num_segs, 1, "the gold clip should be transcribed and scored");
    let hyp = &result.segments[0].hypothesis;
    eprintln!("[gold-asr] hypothesis ({} chars): {}", hyp.len(), hyp);
    assert!(!hyp.trim().is_empty(), "real OmniASR must produce a non-blank transcript");
    assert!((0.0..=1.0).contains(&result.run.cer), "CER must be a valid rate in [0,1]");
}

/// M1.2 / M2b A/B spike — does the OmniASR-CTC `language="ckb"` hint change the transcript vs no
/// hint? Pure self-comparison on real Kurdish audio (NO human reference needed). Resolves the
/// long-standing `[unverified]` tag on whether the wired hint is actually effective. Gated by env
/// `CORTEX_CKB_AB_CLIP` pointing at a wav:
///   CORTEX_CKB_AB_CLIP=<clip.wav> cargo test --test real_audio ckb_language_hint_ab -- --ignored --nocapture
#[test]
#[ignore]
fn ckb_language_hint_ab() {
    use cortex_speech_app_lib::asr::{AsrLoadConfig, KurdishAsrService};
    use cortex_speech_app_lib::audio;

    let clip = match std::env::var("CORTEX_CKB_AB_CLIP") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("CORTEX_CKB_AB_CLIP not set; skipping ckb A/B spike");
            return;
        }
    };
    let model_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    let (sr, pcm) = audio::decode_to_pcm(std::path::Path::new(&clip)).expect("decode A/B clip");
    let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

    let transcribe_with = |lang: &str| -> String {
        let cfg = AsrLoadConfig { language: lang.to_string(), enable_gpu: false, ..Default::default() };
        let mut asr = KurdishAsrService::new_with_config(&model_dir, &cfg).expect("asr init");
        assert!(asr.is_available(), "ASR must be available (real model present)");
        asr.transcribe(&f32_pcm, sr).expect("transcribe").0
    };

    let t_ckb = transcribe_with("ckb");
    let t_none = transcribe_with("");

    eprintln!("[ckb-ab] hint='ckb' -> {t_ckb}");
    eprintln!("[ckb-ab] hint=''    -> {t_none}");
    if t_ckb == t_none {
        eprintln!(
            "[ckb-ab] FINDING: the ckb language hint is a NO-OP for OmniASR-CTC on this clip (identical output)."
        );
    } else {
        eprintln!("[ckb-ab] FINDING: the ckb language hint CHANGES output (effective on this clip).");
    }
}

/// M1.3 — first real Central Kurdish CER/WER scorecard: runs the live OmniASR-CTC engine on a gold
/// set of human-transcribed clips and scores it against the verified Sorani references. Gated by
/// env `CORTEX_GOLD_MANIFEST` = a TSV of `<audio_path>\t<reference_sentence>` per line.
///   CORTEX_GOLD_MANIFEST=<manifest.tsv> cargo test --manifest-path src-tauri/Cargo.toml \
///       --test real_audio ckb_scorecard_on_gold -- --ignored --nocapture
#[test]
#[ignore]
fn ckb_scorecard_on_gold() {
    use cortex_speech_app_lib::asr::{AsrLoadConfig, KurdishAsrService};
    use cortex_speech_app_lib::{audio, wer};

    let manifest = match std::env::var("CORTEX_GOLD_MANIFEST") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("CORTEX_GOLD_MANIFEST not set; skipping scorecard");
            return;
        }
    };
    let model_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    let cfg = AsrLoadConfig { language: "ckb".into(), enable_gpu: false, ..Default::default() };
    let mut asr = KurdishAsrService::new_with_config(&model_dir, &cfg).expect("asr init");
    assert!(asr.is_available(), "ASR must be available (real model present)");

    // Optional per-clip results file (gender, age, edit-distance counts) for post-hoc bootstrap CI
    // and per-group fairness — computed without re-running the (slow) ASR.
    let results_path = std::env::var("CORTEX_GOLD_RESULTS").ok();

    let text = std::fs::read_to_string(&manifest).expect("read gold manifest");
    let (mut t_cd, mut t_crl, mut t_wd, mut t_wrl, mut n) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut rows = String::from("gender\tage\tscript\tchar_dist\tchar_ref_len\tword_dist\tword_ref_len\n");
    let mut hyp_rows = String::new();
    for (i, line) in text.lines().enumerate() {
        // manifest: <wav_path>\t<reference>[\t<gender>[\t<age>]]
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 2 {
            continue;
        }
        let (path, reference) = (cols[0], cols[1]);
        let gender = cols.get(2).copied().unwrap_or("");
        let age = cols.get(3).copied().unwrap_or("");
        let (sr, pcm) = match audio::decode_to_pcm(std::path::Path::new(path)) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[gold] decode failed {path}: {e}");
                continue;
            }
        };
        let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let hyp = asr.transcribe(&f32_pcm, sr).map(|t| t.0).unwrap_or_default();
        // Classify the hypothesis's dominant script: does the model output Kurdish (Arabic block) or
        // romanize to Latin? This isolates "recognition quality" from "wrong script" in the CER.
        let arab = hyp.chars().filter(|c| ('\u{0600}'..='\u{06FF}').contains(c)).count();
        let latin = hyp.chars().filter(|c| c.is_ascii_alphabetic()).count();
        let script = if hyp.trim().is_empty() {
            "empty"
        } else if arab >= latin {
            "arab"
        } else {
            "latin"
        };
        let cd = wer::char_edit_distance(reference, &hyp);
        let wd = wer::word_edit_distance(reference, &hyp);
        t_cd += cd.distance;
        t_crl += cd.ref_len;
        t_wd += wd.distance;
        t_wrl += wd.ref_len;
        n += 1;
        rows.push_str(&format!(
            "{gender}\t{age}\t{script}\t{}\t{}\t{}\t{}\n",
            cd.distance, cd.ref_len, wd.distance, wd.ref_len
        ));
        hyp_rows.push_str(&format!("{script}\t{reference}\t{hyp}\n"));
        if i < 5 {
            eprintln!("[gold] ref={reference} | hyp={hyp}");
        }
    }
    let micro_cer = t_cd as f64 / t_crl.max(1) as f64;
    let micro_wer = t_wd as f64 / t_wrl.max(1) as f64;
    if let Some(rp) = results_path {
        std::fs::write(&rp, rows).expect("write per-clip results");
        eprintln!("[gold] per-clip results -> {rp}");
    }
    if let Ok(hp) = std::env::var("CORTEX_GOLD_HYPS") {
        std::fs::write(&hp, hyp_rows).ok();
        eprintln!("[gold] ref/hyp pairs -> {hp}");
    }
    eprintln!("[scorecard] N={n} micro_CER={micro_cer:.4} micro_WER={micro_wer:.4} (ckb, OmniASR-CTC-300M)");
}
