use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::{AppSettings, AsrModelSize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;

#[path = "fixtures/mod.rs"]
mod fixtures;

#[test]
fn test_user_podcast_file() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("user_data_test", "test_user_podcast_file");
    let Some(path) = std::env::var_os("CORTEX_USER_PODCAST_FILE").map(PathBuf::from) else {
        println!("Set CORTEX_USER_PODCAST_FILE to run this real-data test; skipping gracefully");
        return;
    };
    if !path.exists() {
        println!("User audio file not found: {}; skipping test gracefully", path.display());
        return;
    }

    println!("==> Testing on real data: {}", path.display());

    // 1. Setup environment
    let tmp = TempDir::new().expect("create temp dir");
    let db_path = tmp.path().join("user_test.db");
    let db_path_str = db_path.to_str().unwrap().to_string();

    let db = Database::open(&db_path_str).expect("open db");
    db.initialize().expect("init db");
    drop(db);

    // Use models from the project's models directory
    let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let models_dir = project_root.join("models");
    println!("==> Using models from: {}", models_dir.display());

    let pipeline = ProcessingPipeline::new(
        db_path_str.clone(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(100)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(AppSettings {
            // CTC-300M explicitly: default WSL7B fails hard without a client script (F2).
            asr_model_size: AsrModelSize::CTC300M,
            vad_threshold: 0.5,
            min_segment_duration_ms: 2000,
            max_segment_duration_ms: 15000,
            ..AppSettings::default()
        }),
        Arc::new(ModelManager::new(models_dir)),
    );

    // 2. Warm up ASR
    println!("==> Warming up ASR...");
    pipeline.warmup_asr().expect("warmup asr");

    // 3. Run Pipeline
    println!("==> Starting processing pipeline...");
    let start = Instant::now();
    let db = Database::open(&db_path_str).expect("reopen db");

    let segments = pipeline.process_single_file(&path, &db).expect("pipeline processing failed");

    let elapsed = start.elapsed();
    println!("==> Processing complete in {:.2}s", elapsed.as_secs_f64());
    println!("==> Found {} speech segments", segments.len());

    // 4. Analysis and Dataset Generation
    let mut total_duration_ms = 0;
    for (i, seg) in segments.iter().enumerate() {
        total_duration_ms += seg.duration_ms;
        println!("[Segment {}] Duration: {}ms, ID: {}", i, seg.duration_ms, seg.id);
        println!("   Raw: {}", seg.raw_transcript);
        if let Some(ref norm) = seg.normalized_transcript {
            println!("   Norm: {}", norm);
        }
    }

    println!("==> Total speech duration: {:.2}s", total_duration_ms as f64 / 1000.0);

    // 5. Export evidence under target/ so real-data runs do not dirty the source root.
    let export_path = user_dataset_export_path(project_root);
    let export_dir = export_path.parent().expect("export path parent");
    std::fs::create_dir_all(export_dir).expect("create target export dir");
    let json = serde_json::to_string_pretty(&segments).expect("serialize segments");
    std::fs::write(&export_path, json).expect("write export");

    println!("==> Dataset exported to: {}", export_path.display());

    let non_empty_transcripts = segments.iter().filter(|seg| !seg.raw_transcript.trim().is_empty()).count();
    assert!(
        non_empty_transcripts > 0,
        "real-data ASR produced {non_empty_transcripts}/{} non-empty transcripts; exported evidence at {}",
        segments.len(),
        export_path.display()
    );
}

#[test]
fn user_dataset_export_path_stays_under_target() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("user_data_test", "user_dataset_export_path_stays_under_target");
    let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(user_dataset_export_path(project_root), project_root.join("target").join("user_dataset_output.json"));
}

fn user_dataset_export_path(project_root: &Path) -> PathBuf {
    project_root.join("target").join("user_dataset_output.json")
}
