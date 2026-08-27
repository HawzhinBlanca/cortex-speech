mod fixtures;

use cortex_speech_app_lib::asr::KurdishAsrService;
use cortex_speech_app_lib::audio::{self, voice_activity_detection};
use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::health;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::{AppSettings, AsrModelSize};
use std::sync::Arc;
use tempfile::TempDir;

fn setup_pipeline() -> (ProcessingPipeline, String, TempDir) {
    let tmp = TempDir::new().unwrap();
    let models_dir = tmp.path().join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    std::fs::create_dir_all(models_dir.join("omniasr-ctc-300m")).unwrap();
    std::fs::write(models_dir.join("omniasr-ctc-300m/tokens.txt"), "# test placeholder\n").unwrap();
    cortex_speech_app_lib::models::init_user_models_dir(models_dir.clone());

    let db_path = tmp.path().join("test.db");
    let db_path_str = db_path.to_str().unwrap().to_string();
    let db = Database::open(&db_path_str).unwrap();
    db.initialize().unwrap();
    drop(db);

    // Select the local CTC-300M engine explicitly: the default engine is now WSL7B, which fails hard
    // without a client script (F2 — no silent downgrade). This test exercises the local import path
    // (VAD → chunk → local ASR), so it must pick the local engine it always effectively used.
    let settings = AppSettings { asr_model_size: AsrModelSize::CTC300M, ..AppSettings::default() };
    let pipeline = ProcessingPipeline::new(
        db_path_str.clone(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(100)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(settings),
        Arc::new(ModelManager::new(models_dir)),
    );
    (pipeline, db_path_str, tmp)
}

#[test]
fn test_e2e_import_produces_segments() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_import_produces_segments");
    let dir = TempDir::new().unwrap();
    fixtures::create_test_wav(&dir.path().join("test1.wav"), 1.0, 16000, 440.0).unwrap();
    fixtures::create_test_wav(&dir.path().join("test2.wav"), 0.5, 16000, 880.0).unwrap();

    let (pipeline, db_path, _tmp) = setup_pipeline();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let evt = events.clone();

    pipeline
        .import_directory(dir.path(), None, move |e| {
            evt.lock().unwrap().push(format!("{e:?}"));
        })
        .unwrap();

    let db = Database::open(&db_path).unwrap();
    let segments = db.get_segments(None).unwrap();
    if segments.len() < 2 {
        let evts = events.lock().unwrap();
        panic!("Should import at least 2 segments, got {}. Events: {:?}", segments.len(), *evts);
    }

    for seg in &segments {
        assert!(!seg.id.is_empty(), "Segment ID should not be empty");
        assert!(!seg.audio_path.is_empty(), "Audio path should not be empty");
        assert!(seg.duration_ms > 0, "Duration should be positive");
    }
}

#[test]
fn test_e2e_vad_detects_speech() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_vad_detects_speech");
    let tmp = TempDir::new().unwrap();
    let wav_path = tmp.path().join("speech.wav");
    fixtures::create_test_wav(&wav_path, 2.0, 16000, 440.0).unwrap();

    let (sample_rate, pcm) = audio::decode_to_pcm(&wav_path).unwrap();

    let result = voice_activity_detection(&pcm, sample_rate, 0.5);
    assert!(result.is_ok(), "VAD should succeed");

    let (segments, _backend) = result.unwrap();
    assert!(!segments.is_empty(), "VAD should detect at least one segment in non-silent audio");
}

#[test]
fn test_e2e_vad_silent_fallback() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_vad_silent_fallback");
    let tmp = TempDir::new().unwrap();
    let wav_path = tmp.path().join("silence.wav");
    fixtures::create_silent_wav(&wav_path, 1.0, 16000).unwrap();

    let (sample_rate, pcm) = audio::decode_to_pcm(&wav_path).unwrap();
    let result = voice_activity_detection(&pcm, sample_rate, 0.5);
    assert!(result.is_ok(), "VAD should succeed even on silent audio");

    let (segments, _backend) = result.unwrap();
    assert!(!segments.is_empty(), "VAD should return fallback segment for silent audio");
}

#[test]
fn test_e2e_asr_no_models() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_asr_no_models");
    let tmp = TempDir::new().unwrap();
    let mut asr = KurdishAsrService::new(tmp.path(), false).unwrap();
    assert!(!asr.is_available(), "ASR should not be available without models");

    let result = asr.transcribe(&[0.0f32; 16000], 16000);
    assert!(result.is_err(), "ASR should return error when models not loaded");
}

#[test]
fn test_e2e_normalizer_sorani() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_normalizer_sorani");
    let normalizer = SoraniNormalizer::new();

    let input = "ئەم تاقیکردنەیە";
    let result = normalizer.normalize(input);
    assert!(!result.is_empty(), "Normalizer should produce output");
}

#[test]
fn test_e2e_alignment() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_alignment");
    let tmp = TempDir::new().unwrap();
    let wav_path = tmp.path().join("align.wav");
    fixtures::create_test_wav(&wav_path, 1.0, 16000, 440.0).unwrap();

    let (sample_rate, pcm) = audio::decode_to_pcm(&wav_path).unwrap();

    let aligner = cortex_speech_app_lib::aligner::ForcedAligner::new(tmp.path(), false).unwrap();
    let (timestamps, quality) = aligner.align(&pcm, sample_rate, "ئەم تاقیکردنەیە").unwrap();

    assert!(!timestamps.is_empty(), "Aligner should return timestamps");
    // No model is present in this temp dir, so the result is the heuristic fallback.
    assert_eq!(quality, cortex_speech_app_lib::aligner::AlignmentQuality::EnergyHeuristic);

    for ts in &timestamps {
        assert!(ts.end > ts.start, "Word end should be after start");
        assert!(ts.confidence > 0.0, "Confidence should be positive");
    }
}

#[test]
fn test_e2e_model_manager_status() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_model_manager_status");
    let tmp = TempDir::new().unwrap();
    let mgr = ModelManager::new(tmp.path().to_path_buf());
    mgr.ensure_dir().unwrap();

    let status = mgr.status();
    assert!(!status.is_empty(), "Should have model entries");

    // An empty user directory deliberately falls back to the bundled runtime models.  Whether any
    // optional model is missing therefore depends on the exact build being tested and must not be
    // asserted here.  What is invariant is that `status()` and `missing_models()` report the same
    // availability truth, and that the empty user directory is never falsely named as the source.
    let mut missing_from_api = mgr.missing_models().into_iter().map(|model| model.name).collect::<Vec<_>>();
    missing_from_api.sort_unstable();
    let mut missing_from_status = status
        .iter()
        .filter(|entry| entry.get("downloaded").and_then(serde_json::Value::as_bool) == Some(false))
        .map(|entry| entry.get("name").and_then(serde_json::Value::as_str).expect("model status has a name"))
        .collect::<Vec<_>>();
    missing_from_status.sort_unstable();
    assert_eq!(missing_from_status, missing_from_api, "status and missing-model APIs must agree");
    assert!(
        status.iter().all(|entry| entry.get("source").and_then(serde_json::Value::as_str) != Some("user")),
        "an empty user model directory must not be reported as the model source"
    );
}

#[test]
fn test_e2e_decode_timeout() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_decode_timeout");
    let tmp = TempDir::new().unwrap();
    let wav_path = tmp.path().join("normal.wav");
    fixtures::create_test_wav(&wav_path, 0.5, 16000, 440.0).unwrap();

    let result = audio::decode_to_pcm_with_timeout(&wav_path, std::time::Duration::from_secs(5));
    assert!(result.is_ok(), "Normal decode should succeed within timeout");
}

#[test]
fn test_e2e_health_check() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_health_check");
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("health.db");
    let db_path_str = db_path.to_str().unwrap().to_string();
    let db = Database::open(&db_path_str).unwrap();
    db.initialize().unwrap();

    let mgr = ModelManager::new(tmp.path().to_path_buf());
    let health =
        health::health_check(&db, &mgr, &cortex_speech_app_lib::settings::AppSettings::default(), None).unwrap();
    assert!(health.status == "ok" || health.status == "models_needed", "Health status should be ok or models_needed");
    assert_eq!(health.segment_count, 0, "fresh health database should be empty");
}

#[test]
fn test_e2e_mixed_audio_formats() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("e2e_pipeline", "test_e2e_mixed_audio_formats");
    let dir = TempDir::new().unwrap();
    fixtures::create_test_wav(&dir.path().join("16k.wav"), 0.3, 16000, 440.0).unwrap();
    fixtures::create_test_wav(&dir.path().join("44k.wav"), 0.2, 44100, 880.0).unwrap();

    let (pipeline, db_path, _tmp) = setup_pipeline();
    pipeline.import_directory(dir.path(), None, |_| {}).unwrap();

    let db = Database::open(&db_path).unwrap();
    let segments = db.get_segments(None).unwrap();
    assert!(segments.len() >= 2, "Should import both WAV files");
}
