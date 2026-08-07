//! Long-import soak test — verifies pipeline stability on multi-minute synthetic audio.

mod fixtures;

use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::{AppSettings, AsrModelSize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;

fn setup_pipeline() -> (ProcessingPipeline, String, TempDir) {
    let tmp = TempDir::new().unwrap();
    let models_dir = tmp.path().join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    std::fs::create_dir_all(models_dir.join("omniasr-ctc-300m")).unwrap();
    std::fs::write(models_dir.join("omniasr-ctc-300m/tokens.txt"), "# test placeholder\n").unwrap();
    cortex_speech_app_lib::models::init_user_models_dir(models_dir.clone());

    let db_path = tmp.path().join("soak.db");
    let db_path_str = db_path.to_str().unwrap().to_string();
    let db = Database::open(&db_path_str).unwrap();
    db.initialize().unwrap();
    drop(db);

    // CTC-300M explicitly: the default engine is now WSL7B (fails hard without a client script, F2);
    // this soak exercises the local import path.
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
fn soak_import_many_minutes_of_synthetic_audio() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("soak", "soak_import_many_minutes_of_synthetic_audio");
    let audio_dir = TempDir::new().unwrap();
    // 12 × 30s clips ≈ 6 minutes total — fast but exercises chunking + DB batching.
    for i in 0..12 {
        let path = audio_dir.path().join(format!("podcast_part_{i:02}.wav"));
        fixtures::create_test_wav(&path, 30.0, 16000, 220.0 + f64::from(i * 17)).unwrap();
    }

    let (pipeline, db_path, _keep) = setup_pipeline();
    let started = Instant::now();
    pipeline.import_directory(audio_dir.path(), None, |_| {}).expect("soak import should succeed");

    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(300), "soak import took too long: {:.1}s", elapsed.as_secs_f64());

    let db = Database::open(&db_path).unwrap();
    let segments = db.get_segments(None).unwrap();
    assert!(!segments.is_empty(), "soak import should produce segments");
    assert!(segments.len() >= 12, "expected at least one segment per file, got {}", segments.len());
}
