use cortex_speech_app_lib::db::{Database, SpeechSegment};
use tempfile::NamedTempFile;

fn make_seg(id: &str, path: &str, text: &str) -> SpeechSegment {
    SpeechSegment {
        id: id.to_string(),
        created_at: None,
        audio_path: path.to_string(),
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
        ..SpeechSegment::default()
    }
}

fn setup_db() -> (Database, String) {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let db = Database::open(&path).unwrap();
    db.initialize().unwrap();

    db.insert_segment(&make_seg("wal_test_1", "/audio/file1.wav", "hello world")).unwrap();
    db.insert_segment(&make_seg("wal_test_2", "/audio/file2.wav", "test data here")).unwrap();

    (db, path)
}

#[test]
fn test_wal_checkpoint() {
    let (db, _path) = setup_db();
    assert_eq!(db.segment_count().unwrap(), 2);

    db.wal_checkpoint().unwrap();

    // After checkpoint, database should still be usable
    assert_eq!(db.segment_count().unwrap(), 2);
    let seg = db.get_segment_by_id("wal_test_1").unwrap().unwrap();
    assert_eq!(seg.raw_transcript, "hello world");
}

#[test]
fn test_vacuum() {
    let (db, _path) = setup_db();

    // Delete one segment first to create fragmentation
    db.delete_segment("wal_test_2").unwrap();
    assert_eq!(db.segment_count().unwrap(), 1);

    // Vacuum should reclaim space
    db.vacuum().unwrap();

    // Data should still be accessible
    assert_eq!(db.segment_count().unwrap(), 1);
    let seg = db.get_segment_by_id("wal_test_1").unwrap().unwrap();
    assert_eq!(seg.audio_path, "/audio/file1.wav");
}

#[test]
fn test_backup_contains_expected_data() {
    let (db, _path) = setup_db();

    let backup_dir = std::env::temp_dir();
    let backup_path = backup_dir.join("cortex_test_backup_expected.db");
    let _ = std::fs::remove_file(&backup_path);

    db.backup(&backup_path).unwrap();
    assert!(backup_path.exists(), "Backup file should exist");

    let backup_db = Database::open(backup_path.to_str().unwrap()).unwrap();
    backup_db.initialize().unwrap();

    // Verify backup has same data
    assert_eq!(backup_db.segment_count().unwrap(), 2);

    let seg1 = backup_db.get_segment_by_id("wal_test_1").unwrap().unwrap();
    assert_eq!(seg1.raw_transcript, "hello world");
    assert_eq!(seg1.duration_ms, 1000);

    let seg2 = backup_db.get_segment_by_id("wal_test_2").unwrap().unwrap();
    assert_eq!(seg2.raw_transcript, "test data here");

    drop(backup_db);
    let _ = std::fs::remove_file(&backup_path);
}

#[test]
fn test_restore_recovers_all_data() {
    let (db, _path) = setup_db();

    let backup_dir = std::env::temp_dir();
    let backup_path = backup_dir.join("cortex_test_restore_src.db");
    let _ = std::fs::remove_file(&backup_path);

    db.backup(&backup_path).unwrap();

    // Create a fresh database and restore into it
    let restore_tmp = NamedTempFile::new().unwrap();
    let restore_path = restore_tmp.path().to_str().unwrap().to_string();
    let mut restore_db = Database::open(&restore_path).unwrap();
    restore_db.initialize().unwrap();
    assert_eq!(restore_db.segment_count().unwrap(), 0);

    restore_db.restore(&backup_path).unwrap();

    // Verify all data was restored
    assert_eq!(restore_db.segment_count().unwrap(), 2);

    let restored_1 = restore_db.get_segment_by_id("wal_test_1").unwrap().unwrap();
    assert_eq!(restored_1.raw_transcript, "hello world");
    assert_eq!(restored_1.duration_ms, 1000);

    let restored_2 = restore_db.get_segment_by_id("wal_test_2").unwrap().unwrap();
    assert_eq!(restored_2.raw_transcript, "test data here");
    assert_eq!(restored_2.audio_path, "/audio/file2.wav");

    // Verify FTS search still works on restored data
    let search_results = restore_db.search_segments("hello").unwrap();
    assert!(!search_results.is_empty(), "FTS search should work after restore");

    let _ = std::fs::remove_file(&backup_path);
}

#[test]
fn test_backup_and_restore_with_many_segments() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let db = Database::open(&path).unwrap();
    db.initialize().unwrap();

    let mut segments = Vec::new();
    for i in 0..50 {
        segments.push(make_seg(&format!("bulk_{i}"), &format!("/audio/file{i}.wav"), &format!("transcript {i}")));
    }
    db.insert_segments_batch(&segments).unwrap();
    assert_eq!(db.segment_count().unwrap(), 50);

    // Backup
    let backup_path = std::env::temp_dir().join("cortex_test_bulk_backup.db");
    let _ = std::fs::remove_file(&backup_path);
    db.backup(&backup_path).unwrap();

    // Restore
    let restore_tmp = NamedTempFile::new().unwrap();
    let restore_path = restore_tmp.path().to_str().unwrap().to_string();
    let mut restore_db = Database::open(&restore_path).unwrap();
    restore_db.initialize().unwrap();
    restore_db.restore(&backup_path).unwrap();

    assert_eq!(restore_db.segment_count().unwrap(), 50);

    // Spot-check some segments
    for i in (0..50).step_by(10) {
        let seg = restore_db.get_segment_by_id(&format!("bulk_{i}")).unwrap().unwrap();
        assert_eq!(seg.raw_transcript, format!("transcript {i}"));
    }

    let _ = std::fs::remove_file(&backup_path);
}

#[test]
fn test_db_ctc_score_crud() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let db = Database::open(&path).unwrap();
    db.initialize().unwrap();

    let mut seg = make_seg("ctc_test", "/audio/ctc.wav", "کوردستان");
    seg.ctc_score = Some(-1.23);

    db.insert_segment(&seg).unwrap();

    let retrieved = db.get_segment_by_id("ctc_test").unwrap().unwrap();
    assert_eq!(retrieved.ctc_score, Some(-1.23));

    db.update_ctc_score("ctc_test", -4.56).unwrap();

    let retrieved_updated = db.get_segment_by_id("ctc_test").unwrap().unwrap();
    assert_eq!(retrieved_updated.ctc_score, Some(-4.56));
}

#[test]
fn test_db_quality_metrics_crud() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let db = Database::open(&path).unwrap();
    db.initialize().unwrap();

    let mut seg = make_seg("qual_test", "/audio/qual.wav", "کوردستان");
    seg.clipping_ratio = Some(0.123);
    seg.rms_db = Some(-25.4);
    seg.snr_db = Some(30.5);

    db.insert_segment(&seg).unwrap();

    let retrieved = db.get_segment_by_id("qual_test").unwrap().unwrap();
    assert_eq!(retrieved.clipping_ratio, Some(0.123));
    assert_eq!(retrieved.rms_db, Some(-25.4));
    assert_eq!(retrieved.snr_db, Some(30.5));

    db.update_quality_metrics("qual_test", 0.0, -12.3, 45.6).unwrap();

    let retrieved_updated = db.get_segment_by_id("qual_test").unwrap().unwrap();
    assert_eq!(retrieved_updated.clipping_ratio, Some(0.0));
    assert_eq!(retrieved_updated.rms_db, Some(-12.3));
    assert_eq!(retrieved_updated.snr_db, Some(45.6));
}
