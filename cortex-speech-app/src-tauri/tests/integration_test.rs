use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::diff;
use cortex_speech_app_lib::history::{Command, HistoryAction, HistoryManager};
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use tempfile::NamedTempFile;

fn make_segment(id: &str, text: &str) -> cortex_speech_app_lib::db::SpeechSegment {
    cortex_speech_app_lib::db::SpeechSegment {
        id: id.to_string(),
        created_at: None,
        audio_path: format!("{id}.wav"),
        raw_transcript: text.to_string(),
        normalized_transcript: None,
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1000,
        speaker_id: None,
        verified: false,
        confidence: None,
        ctc_score: None,
        clipping_ratio: None,
        rms_db: None,
        snr_db: None,
        split: None,
        signal_anomaly_score: None,
        ..cortex_speech_app_lib::db::SpeechSegment::default()
    }
}

#[test]
fn test_full_pipeline_flow() {
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap();
    let db = Database::open(db_path).unwrap();
    db.initialize().unwrap();

    // Insert segments
    let seg1 = make_segment("pipe1", "test one");
    let seg2 = make_segment("pipe2", "test two");
    db.insert_segment(&seg1).unwrap();
    db.insert_segment(&seg2).unwrap();

    // Verify retrieval
    let all = db.get_segments(None).unwrap();
    assert_eq!(all.len(), 2);

    // Search
    let results = db.search_segments("test").unwrap();
    assert_eq!(results.len(), 2);

    // Update
    let mut seg = db.get_segment_by_id("pipe1").unwrap().unwrap();
    seg.raw_transcript = "updated text".to_string();
    db.insert_segment(&seg).unwrap();
    let updated = db.get_segment_by_id("pipe1").unwrap().unwrap();
    assert_eq!(updated.raw_transcript, "updated text");

    // Delete
    db.delete_segment("pipe2").unwrap();
    let remaining = db.get_segments(None).unwrap();
    assert_eq!(remaining.len(), 1);

    // Batch delete
    db.insert_segment(&seg2).unwrap();
    db.delete_segments_batch(&["pipe2".to_string()]).unwrap();
    assert_eq!(db.get_segments(None).unwrap().len(), 1);

    // Stats
    let stats = cortex_speech_app_lib::stats::compute_stats(&db).unwrap();
    assert!(stats.total_segments >= 1);
}

#[test]
fn test_undo_redo_integration() {
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap();
    let db = Database::open(db_path).unwrap();
    db.initialize().unwrap();
    let history = HistoryManager::new(100);

    // Insert and track
    let seg = make_segment("integ1", "original");
    db.insert_segment(&seg).unwrap();

    // Update via command pattern
    let mut updated = seg.clone();
    updated.raw_transcript = "modified".to_string();
    let cmd = Command::UpdateSegment {
        segment_id: updated.id.clone(),
        previous: Box::new(seg.clone()),
        current: Box::new(updated.clone()),
    };
    db.insert_segment(&updated).unwrap();
    history.push(cmd);

    // Verify modified
    let curr = db.get_segment_by_id("integ1").unwrap().unwrap();
    assert_eq!(curr.raw_transcript, "modified");

    // Undo
    let desc = history.undo(&db).unwrap();
    assert!(desc.is_some());
    assert_eq!(desc.unwrap(), HistoryAction::UpdateSegment);

    let restored = db.get_segment_by_id("integ1").unwrap().unwrap();
    assert_eq!(restored.raw_transcript, "original");

    // Redo
    let desc = history.redo(&db).unwrap();
    assert!(desc.is_some());
    let redone = db.get_segment_by_id("integ1").unwrap().unwrap();
    assert_eq!(redone.raw_transcript, "modified");
}

#[test]
fn test_diff_integration() {
    let diff_result = diff::compute_diff("hello world from the test suite", "hello universe from the test environment");
    assert!(diff_result.stats.unchanged_words >= 3);
    assert!(diff_result.stats.changed_words >= 1);
    assert!(diff_result.stats.similarity > 0.0);
}

#[test]
fn test_normalizer_integration() {
    let normalizer = SoraniNormalizer::new();

    // Arabic Kaf → Sorani Kaf
    assert_eq!(normalizer.normalize("ك"), "ک");

    // Arabic Yeh → Sorani Yeh
    assert_eq!(normalizer.normalize("ي"), "ی");

    // Digit conversion (default verbalized)
    assert_eq!(normalizer.normalize("١٩٥٠"), "ھەزار و نۆسەد و پەنجا");

    // Digit conversion (with verbalize_numbers = false)
    let non_verbalized = SoraniNormalizer::with_config(cortex_speech_app_lib::normalizer::NormalizationConfig {
        normalize_numbers: true,
        verbalize_numbers: false,
        normalize_hamza: true,
        remove_diacritics: false,
    });
    assert_eq!(non_verbalized.normalize("١٩٥٠"), "1950");

    // Tatweel removal
    assert_eq!(normalizer.normalize("گەورەترین"), "گەورەترین");

    // Multiple spaces
    assert_eq!(normalizer.normalize("ئەم  کەسە"), "ئەم کەسە");
}
