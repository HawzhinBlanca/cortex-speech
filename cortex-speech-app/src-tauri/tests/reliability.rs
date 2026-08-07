use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use cortex_speech_app_lib::audio::decode_to_pcm;
use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::cancel::CancellationToken;
use cortex_speech_app_lib::db::{Database, SpeechSegment};
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::flock::InstanceLock;
use cortex_speech_app_lib::health::health_check;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::session::SessionManager;
use cortex_speech_app_lib::settings::{AppSettings, AsrModelSize};
use cortex_speech_app_lib::throttle::RateLimiter;
use tempfile::{NamedTempFile, TempDir};

mod fixtures;

// ── Helpers ────────────────────────────────────────────────────────────

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

fn setup_model_mgr() -> (ModelManager, TempDir) {
    let tmp = TempDir::new().unwrap();
    (ModelManager::new(tmp.path().to_path_buf()), tmp)
}

fn setup_db() -> (Database, String) {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let db = Database::open(&path).unwrap();
    db.initialize().unwrap();
    (db, path)
}

fn create_pipeline(db_path: &str) -> ProcessingPipeline {
    let tmp = TempDir::new().unwrap();
    // CTC-300M explicitly: the default engine is now WSL7B (fails hard without a client script, F2).
    // These reliability tests exercise the local import path, so they pick the local engine.
    let settings = AppSettings { asr_model_size: AsrModelSize::CTC300M, ..AppSettings::default() };
    ProcessingPipeline::new(
        db_path.to_string(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(100)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(settings),
        Arc::new(ModelManager::new(tmp.path().to_path_buf())),
    )
}

// ── 1. Session Survives Crash ─────────────────────────────────────────

#[test]
fn test_session_survives_crash() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_session_survives_crash");
    let tmp = TempDir::new().unwrap();
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();

    for i in 0..5 {
        db.insert_segment(&make_seg(&format!("crash_{i}"), &format!("/audio/{i}.wav"), &format!("transcript {i}")))
            .unwrap();
    }

    let manager = SessionManager::new(tmp.path().to_path_buf());
    manager.save(&db).unwrap();

    let state_path = manager.save_path();
    assert!(state_path.exists(), "Session file should exist after save");

    let fresh_db = Database::open(":memory:").unwrap();
    fresh_db.initialize().unwrap();
    let mut recovered = SessionManager::new(tmp.path().to_path_buf());
    let restored = recovered.restore().unwrap();

    assert!(restored.is_some(), "Session should be restored");
    let state = restored.unwrap();
    assert_eq!(state.segment_count, 5);
    assert_eq!(state.verified_count, 0);
    assert_eq!(state.view_mode, "transcribe");
}

// ── 2. Corrupt Database Detection ─────────────────────────────────────

#[test]
fn test_corrupt_database_detection_valid() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_corrupt_database_detection_valid");
    let (db, _path) = setup_db();
    db.insert_segment(&make_seg("valid_1", "/a.wav", "test")).unwrap();
    db.wal_checkpoint().unwrap();

    let result: String = db.connection().query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
    assert_eq!(result, "ok", "Valid database should pass integrity_check");
}

#[test]
fn test_corrupt_database_detection_corrupted() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_corrupt_database_detection_corrupted");
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();

    {
        let db = Database::open(&path).unwrap();
        db.initialize().unwrap();
        db.insert_segment(&make_seg("corrupt_1", "/a.wav", "data")).unwrap();
        db.wal_checkpoint().unwrap();
    }

    let mut content = std::fs::read(&path).unwrap();
    // The checkpointed DB is at least one 4096-byte page; clobber the SQLite header magic.
    assert!(content.len() > 100, "checkpointed DB must be large enough to corrupt its header");
    for byte in content.iter_mut().take(100) {
        *byte = 0xFF;
    }
    std::fs::write(&path, &content).unwrap();

    // Detection contract: opening a header-corrupted DB must EITHER be rejected outright, OR open
    // but fail PRAGMA integrity_check (never silently report "ok"). Assert in BOTH branches so the
    // test fails if corruption detection regresses. (The previous version nested its only assertion
    // inside two `if let Ok` guards that the injected corruption itself defeats — it asserted nothing.)
    match Database::open(&path) {
        Err(_) => { /* open rejected the clobbered header — corruption detected */ }
        Ok(corrupted_db) => {
            let msg: String = corrupted_db
                .connection()
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .expect("integrity_check query must run on an opened DB");
            assert_ne!(msg, "ok", "corrupted DB must not report integrity 'ok', got: {msg}");
        }
    }
}

// ── 3. Concurrent Instance Prevention ─────────────────────────────────

#[test]
fn test_concurrent_instance_prevention() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_concurrent_instance_prevention");
    let tmp = TempDir::new().unwrap();

    let lock1 = InstanceLock::try_lock(tmp.path());
    assert!(lock1.is_ok(), "First lock should succeed");

    let lock2 = InstanceLock::try_lock(tmp.path());
    assert!(lock2.is_err(), "Second lock should fail when first is held");

    drop(lock1);

    let lock3 = InstanceLock::try_lock(tmp.path());
    assert!(lock3.is_ok(), "Third lock should succeed after first is dropped");
}

// ── 4. Rate Limiter ───────────────────────────────────────────────────

#[test]
fn test_rate_limiter_basic() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_rate_limiter_basic");
    let mut limiter = RateLimiter::new(5);

    for i in 0..5 {
        assert!(limiter.check("test").is_ok(), "Call {} of 5 should be allowed", i + 1);
    }

    assert!(limiter.check("test").is_err(), "6th call within burst should be denied");

    std::thread::sleep(Duration::from_millis(1100));

    assert!(limiter.check("test").is_ok(), "After 1s a new token should be available");
}

#[test]
fn test_rate_limiter_burst_exact() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_rate_limiter_burst_exact");
    let mut limiter = RateLimiter::new(3);

    assert!(limiter.check("test").is_ok(), "burst token 1");
    assert!(limiter.check("test").is_ok(), "burst token 2");
    assert!(limiter.check("test").is_ok(), "burst token 3");
    assert!(limiter.check("test").is_err(), "burst exhausted");
}

// ── 5. Audio Decode Timeout ───────────────────────────────────────────

#[test]
fn test_audio_decode_normal() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_audio_decode_normal");
    let tmp = TempDir::new().unwrap();
    let wav_path = tmp.path().join("normal.wav");
    fixtures::create_test_wav(&wav_path, 0.5, 16000, 440.0).unwrap();

    let result = decode_to_pcm(&wav_path);
    assert!(result.is_ok(), "Normal WAV should decode successfully");
    let (_sr, pcm) = result.unwrap();
    assert!(!pcm.is_empty(), "Decoded PCM should have samples");
}

#[test]
fn test_audio_decode_long_silent_file() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_audio_decode_long_silent_file");
    let tmp = TempDir::new().unwrap();
    let long_wav = tmp.path().join("long_silent.wav");
    fixtures::create_silent_wav(&long_wav, 60.0, 16000).unwrap();

    let result = decode_to_pcm(&long_wav);
    assert!(result.is_ok(), "Long silent file should decode without hang");
    let (_sr, pcm) = result.unwrap();
    assert!(!pcm.is_empty(), "Decoded PCM should contain zero samples");
}

#[test]
fn test_audio_decode_with_timeout_wrapper() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_audio_decode_with_timeout_wrapper");
    let tmp = TempDir::new().unwrap();
    let wav_path = tmp.path().join("timeout_test.wav");
    fixtures::create_silent_wav(&wav_path, 120.0, 16000).unwrap();

    let path = wav_path.clone();
    let handle = std::thread::spawn(move || decode_to_pcm(&path));

    let deadline = Instant::now() + Duration::from_secs(15);
    while !handle.is_finished() {
        if Instant::now() > deadline {
            panic!("Decode timed out: 120s silent WAV did not complete within 15s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let result = handle.join().unwrap();
    assert!(result.is_ok(), "Long silent decode should still succeed");
}

// ── 6. Pipeline Import Rollback ───────────────────────────────────────

#[test]
fn test_pipeline_import_rollback() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_pipeline_import_rollback");
    let dir = TempDir::new().unwrap();
    fixtures::create_test_wav(&dir.path().join("rollback_1.wav"), 0.5, 16000, 440.0).unwrap();
    fixtures::create_test_wav(&dir.path().join("rollback_2.wav"), 0.3, 16000, 880.0).unwrap();
    fixtures::create_test_wav(&dir.path().join("rollback_3.wav"), 0.4, 22050, 330.0).unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let evt = events.clone();

    let (_db, db_path) = setup_db();
    let pipeline = create_pipeline(&db_path);
    pipeline
        .import_directory(dir.path(), None, move |e| {
            evt.lock().unwrap().push(e);
        })
        .unwrap();

    let captured = events.lock().unwrap();
    let started = captured.iter().any(|e| matches!(e, cortex_speech_app_lib::pipeline::PipelineEvent::Started { .. }));
    let completed =
        captured.iter().any(|e| matches!(e, cortex_speech_app_lib::pipeline::PipelineEvent::Completed { .. }));
    assert!(started, "Should emit Started event");
    assert!(completed, "Should emit Completed event");
    drop(captured);

    let check_db = Database::open(&db_path).unwrap();
    assert_eq!(check_db.segment_count().unwrap(), 3, "All 3 WAV files should be imported");
}

#[test]
fn test_pipeline_import_empty_directory() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_pipeline_import_empty_directory");
    let dir = TempDir::new().unwrap();

    let (_db, db_path) = setup_db();
    let pipeline = create_pipeline(&db_path);
    pipeline.import_directory(dir.path(), None, |_| {}).unwrap();

    let check_db = Database::open(&db_path).unwrap();
    assert_eq!(check_db.segment_count().unwrap(), 0, "Empty dir should produce no segments");
}

// ── 7. CancellationToken ──────────────────────────────────────────────

#[test]
fn test_cancellation_token_basic() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_cancellation_token_basic");
    let token = CancellationToken::new();

    assert!(!token.is_cancelled(), "New token should not be cancelled");
    assert!(token.check().is_ok(), "check() on active token should succeed");

    token.cancel();

    assert!(token.is_cancelled(), "Token should be cancelled after cancel()");
    assert!(token.check().is_err(), "check() on cancelled token should fail");
}

#[test]
fn test_cancellation_token_clone_independent() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_cancellation_token_clone_independent");
    let original = CancellationToken::new();
    let cloned = original.child_token();

    assert!(!original.is_cancelled());
    assert!(!cloned.is_cancelled());

    cloned.cancel();

    assert!(original.is_cancelled(), "Cancelling clone should cancel original (shared state)");
    assert!(cloned.is_cancelled());
}

#[test]
fn test_cancellation_token_new_independent() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_cancellation_token_new_independent");
    let token1 = CancellationToken::new();
    let token2 = CancellationToken::new();

    token1.cancel();

    assert!(token1.is_cancelled());
    assert!(!token2.is_cancelled(), "Separate tokens should be independent");
    assert!(token2.check().is_ok());
}

// ── 8. Health Check ───────────────────────────────────────────────────

#[test]
fn test_health_check() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_health_check");
    let (_db, path) = setup_db();
    let (model_mgr, _models_tmp) = setup_model_mgr();

    let db = Database::open(&path).unwrap();
    let health = health_check(&db, &model_mgr, None).unwrap();

    assert_eq!(health["status"], "ok", "Bundled runtime models should satisfy essential health");
    assert!(health.get("db_size").and_then(|v| v.as_i64()).is_some(), "db_size should be present");
    assert!(health.get("uptime").and_then(|v| v.as_i64()).is_some(), "uptime should be present");
    assert!(health.get("segment_count").and_then(|v| v.as_i64()).is_some(), "segment_count should be present");
    assert_eq!(health["segment_count"], 0, "Fresh DB should have 0 segments");
    assert_eq!(health["missing_models"].as_array().map(Vec::len), Some(0));
    assert!(
        health["missing_optional_models"].as_array().map(|items| !items.is_empty()).unwrap_or(false),
        "Optional model inventory should still report missing optional files"
    );
}

#[test]
fn test_health_check_with_data() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_health_check_with_data");
    let (_db, path) = setup_db();
    let (model_mgr, _models_tmp) = setup_model_mgr();

    let db = Database::open(&path).unwrap();
    db.insert_segment(&make_seg("health_1", "/a.wav", "hello")).unwrap();
    db.insert_segment(&make_seg("health_2", "/b.wav", "world")).unwrap();

    let health = health_check(&db, &model_mgr, None).unwrap();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["segment_count"], 2);
    assert!(health["db_size"].as_i64().unwrap_or(0) > 0);
}

// ── 9. Multi-Threaded Lock Ordering ───────────────────────────────────

#[test]
fn test_multi_threaded_lock_ordering_same_order() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_multi_threaded_lock_ordering_same_order");
    let lock_a = Arc::new(Mutex::new(()));
    let lock_b = Arc::new(Mutex::new(()));

    let a1 = lock_a.clone();
    let b1 = lock_b.clone();
    let t1 = std::thread::spawn(move || {
        let _g1 = a1.lock().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let _g2 = b1.lock().unwrap();
        std::thread::sleep(Duration::from_millis(30));
    });

    let a2 = lock_a;
    let b2 = lock_b;
    let t2 = std::thread::spawn(move || {
        let _g1 = a2.lock().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let _g2 = b2.lock().unwrap();
        std::thread::sleep(Duration::from_millis(30));
    });

    // Same slow-CI-runner rationale as the other deadlock detectors in this file: a real deadlock
    // never resolves, so 60s still catches it without false-firing on a loaded runner.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !t1.is_finished() || !t2.is_finished() {
        if Instant::now() > deadline {
            panic!("Deadlock detected: threads did not complete within 60s");
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    t1.join().unwrap();
    t2.join().unwrap();
}

#[test]
fn test_multi_threaded_lock_ordering_opposite_order() {
    let _crash_breadcrumb =
        fixtures::crash_breadcrumb("reliability", "test_multi_threaded_lock_ordering_opposite_order");
    let lock_a = Arc::new(Mutex::new(()));
    let lock_b = Arc::new(Mutex::new(()));
    let barrier = Arc::new(Barrier::new(2));

    let a1 = lock_a.clone();
    let b1 = lock_b.clone();
    let b1_barrier = barrier.clone();
    let t1 = std::thread::spawn(move || {
        let _g1 = a1.lock().unwrap();
        b1_barrier.wait();
        std::thread::sleep(Duration::from_millis(50));
        let _result = b1.try_lock();
    });

    let a2 = lock_a;
    let b2 = lock_b;
    let b2_barrier = barrier;
    let t2 = std::thread::spawn(move || {
        let _g1 = b2.lock().unwrap();
        b2_barrier.wait();
        std::thread::sleep(Duration::from_millis(50));
        let _result = a2.try_lock();
    });

    // Same slow-CI-runner rationale as test_concurrent_db_access_no_deadlock: a real deadlock
    // never resolves, so 60s still catches it without false-firing on a loaded runner.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !t1.is_finished() || !t2.is_finished() {
        if Instant::now() > deadline {
            panic!("Threads did not complete within 60s (possible deadlock)");
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    t1.join().unwrap();
    t2.join().unwrap();
}

#[test]
fn test_concurrent_db_access_no_deadlock() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_concurrent_db_access_no_deadlock");
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let db = Arc::new(Mutex::new(Database::open(&path).unwrap()));
    db.lock().unwrap().initialize().unwrap();

    let db1 = db.clone();
    let t1 = std::thread::spawn(move || {
        for i in 0..10 {
            let db = db1.lock().unwrap();
            db.insert_segment(&make_seg(&format!("concurrent_t1_{i}"), &format!("/t1/{i}.wav"), "thread1")).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    });

    let db2 = db;
    let t2 = std::thread::spawn(move || {
        for i in 0..10 {
            let db = db2.lock().unwrap();
            db.insert_segment(&make_seg(&format!("concurrent_t2_{i}"), &format!("/t2/{i}.wav"), "thread2")).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    });

    // A real deadlock never resolves, so a generous deadline still catches it while a tight one
    // false-fires on slow shared CI runners: 5s failed the 2026-07-08 main Release Gate (and one
    // branch run) on disk-bound inserts that complete in 0.18s locally. 60s keeps the detector
    // honest without the flake.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !t1.is_finished() || !t2.is_finished() {
        if Instant::now() > deadline {
            panic!("Concurrent DB access deadlocked within 60s");
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    t1.join().unwrap();
    t2.join().unwrap();
}

// ── Additional edge-case tests ────────────────────────────────────────

#[test]
fn test_instance_lock_drop_releases() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_instance_lock_drop_releases");
    let tmp = TempDir::new().unwrap();

    {
        let _lock = InstanceLock::try_lock(tmp.path()).unwrap();
    }

    let _relock = InstanceLock::try_lock(tmp.path()).expect("Should be able to reacquire after drop");
}

#[test]
fn test_rate_limiter_zero_rate() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_rate_limiter_zero_rate");
    let mut limiter = RateLimiter::new(0);
    assert!(limiter.check("test").is_err(), "Zero rate should never allow");
    std::thread::sleep(Duration::from_millis(500));
    assert!(limiter.check("test").is_err(), "Zero rate should never allow even after waiting");
}

#[test]
fn test_cancellation_token_multiple_cancel_idempotent() {
    let _crash_breadcrumb =
        fixtures::crash_breadcrumb("reliability", "test_cancellation_token_multiple_cancel_idempotent");
    let token = CancellationToken::new();
    token.cancel();
    token.cancel();
    token.cancel();
    assert!(token.is_cancelled());
    assert!(token.check().is_err());
}

#[test]
fn test_health_check_empty_db_fields() {
    let _crash_breadcrumb = fixtures::crash_breadcrumb("reliability", "test_health_check_empty_db_fields");
    let (_db, path) = setup_db();
    let (model_mgr, _models_tmp) = setup_model_mgr();
    let db = Database::open(&path).unwrap();
    let health = health_check(&db, &model_mgr, None).unwrap();

    let obj = health.as_object().expect("health_check should return JSON object");
    assert!(obj.contains_key("status"), "Must contain 'status'");
    assert!(obj.contains_key("db_size"), "Must contain 'db_size'");
    assert!(obj.contains_key("uptime"), "Must contain 'uptime'");
    assert!(obj.contains_key("segment_count"), "Must contain 'segment_count'");
}
