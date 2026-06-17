mod fixtures;

use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::AppSettings;
use std::sync::Arc;
use std::sync::Mutex;
use tempfile::{NamedTempFile, TempDir};

fn create_pipeline(db_path: &str) -> (ProcessingPipeline, TempDir) {
    let tmp = TempDir::new().unwrap();
    let models_dir = tmp.path().join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    std::fs::create_dir_all(models_dir.join("omniasr-ctc-300m")).unwrap();
    std::fs::write(models_dir.join("omniasr-ctc-300m/tokens.txt"), "# test placeholder\n").unwrap();
    cortex_speech_app_lib::models::init_user_models_dir(models_dir.clone());

    let pipeline = ProcessingPipeline::new(
        db_path.to_string(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(100)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(AppSettings::default()),
        Arc::new(ModelManager::new(models_dir)),
    );
    (pipeline, tmp)
}

fn setup_db() -> (Database, String) {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let db = Database::open(&path).unwrap();
    db.initialize().unwrap();
    (db, path)
}

#[test]
fn test_import_directory_with_wav_files() {
    let dir = TempDir::new().unwrap();

    // Create several WAV files with different characteristics
    fixtures::create_test_wav(&dir.path().join("speech1.wav"), 0.5, 16000, 440.0).unwrap();
    fixtures::create_test_wav(&dir.path().join("speech2.wav"), 0.3, 44100, 880.0).unwrap();
    fixtures::create_test_wav(&dir.path().join("speech3.wav"), 0.2, 22050, 220.0).unwrap();

    // Create a non-audio file that should be ignored
    std::fs::write(dir.path().join("readme.txt"), b"not audio").unwrap();

    let (_db, db_path) = setup_db();
    let (pipeline, _tmp) = create_pipeline(&db_path);

    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    pipeline
        .import_directory(dir.path(), None, |event| {
            events_clone.lock().unwrap().push(event);
        })
        .unwrap();

    let captured = events.lock().unwrap();

    // Verify we got all pipeline events
    assert!(captured.iter().any(|e| matches!(e, cortex_speech_app_lib::pipeline::PipelineEvent::Started { .. })));
    assert!(captured.iter().any(|e| matches!(e, cortex_speech_app_lib::pipeline::PipelineEvent::Completed { .. })));

    // Find the Completed event and check counts
    let completed = captured
        .iter()
        .find_map(|e| {
            if let cortex_speech_app_lib::pipeline::PipelineEvent::Completed { total, succeeded, failed } = e {
                Some((*total, *succeeded, *failed))
            } else {
                None
            }
        })
        .unwrap();

    assert_eq!(completed.0, 3, "Should have processed 3 audio files");
    assert_eq!(completed.1, 3, "All 3 should succeed");
    assert_eq!(completed.2, 0, "None should fail");

    drop(captured);

    // Verify segments were inserted into the database
    let db = Database::open(&db_path).unwrap();
    let segments = db.get_segments(None).unwrap();
    assert_eq!(segments.len(), 3, "Should have 3 segments in DB");

    // Each segment should have a valid ID and duration
    for seg in &segments {
        assert!(!seg.id.is_empty(), "Segment ID should not be empty");
        assert!(seg.duration_ms > 0, "Duration should be positive for {seg:?}");
        assert!(!seg.audio_path.is_empty(), "Audio path should not be empty");
    }
}

#[test]
fn test_full_pipeline_import_to_delete() {
    let dir = TempDir::new().unwrap();

    // Import phase: create WAV files
    fixtures::create_test_wav(&dir.path().join("step1.wav"), 0.5, 16000, 440.0).unwrap();
    fixtures::create_test_wav(&dir.path().join("step2.wav"), 0.4, 16000, 660.0).unwrap();

    let (_db, db_path) = setup_db();
    let (pipeline, _tmp) = create_pipeline(&db_path);

    pipeline.import_directory(dir.path(), None, |_| {}).unwrap();

    // Get segments
    let db = Database::open(&db_path).unwrap();
    let segments = db.get_segments(None).unwrap();
    assert_eq!(segments.len(), 2);

    // Find a segment by duration
    let seg_440 = segments.iter().find(|s| s.audio_path.contains("step1")).unwrap();
    let seg_660 = segments.iter().find(|s| s.audio_path.contains("step2")).unwrap();

    // Update phase: modify a transcript
    let mut updated = seg_440.clone();
    updated.raw_transcript = "manually edited transcript".to_string();
    updated.verified = true;
    db.insert_segment(&updated).unwrap();

    let fetched = db.get_segment_by_id(&seg_440.id).unwrap().unwrap();
    assert_eq!(fetched.raw_transcript, "manually edited transcript");
    assert!(fetched.verified);

    // Search phase: find the updated segment
    let results = db.search_segments("manually").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, seg_440.id);

    // Delete phase: remove one segment
    db.delete_segment(&seg_660.id).unwrap();
    let remaining = db.get_segments(None).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, seg_440.id);
}

#[test]
fn test_import_directory_empty() {
    let dir = TempDir::new().unwrap();

    let (_db, db_path) = setup_db();
    let (pipeline, _tmp) = create_pipeline(&db_path);

    pipeline.import_directory(dir.path(), None, |_| {}).unwrap();

    let db = Database::open(&db_path).unwrap();
    let segments = db.get_segments(None).unwrap();
    assert_eq!(segments.len(), 0, "No files should produce no segments");
}

#[test]
fn test_import_directory_skips_non_audio() {
    let dir = TempDir::new().unwrap();

    std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.4 dummy").unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"just some text").unwrap();
    std::fs::write(dir.path().join("script.js"), b"console.log('hi');").unwrap();

    let (_db, db_path) = setup_db();
    let (pipeline, _tmp) = create_pipeline(&db_path);

    pipeline.import_directory(dir.path(), None, |_| {}).unwrap();

    let db = Database::open(&db_path).unwrap();
    assert_eq!(db.segment_count().unwrap(), 0, "No audio files should produce 0 segments");
}
