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

/// Names the test in flight so the NEXT 0xC0000409 abort says which one it was.
///
/// This binary has died twice to `fatal runtime error: Rust cannot catch foreign exceptions` /
/// STATUS_STACK_BUFFER_OVERRUN — a C++ exception escaping the native ONNX/sherpa stack across the FFI
/// boundary. Both times the log recorded only "Running tests\e2e_pipeline.rs" and nothing else, so
/// after two occurrences it is still unknown WHICH test was executing. The process dies before libtest
/// can attribute anything, and it has never reproduced standalone (10/10 clean immediately after the
/// most recent one), so a diagnostic that needs reproduction is worthless here — it has to be armed
/// during the sweep where the crash actually happens.
///
/// Two mechanisms, because either alone is insufficient:
///
///   1. A process-wide MUTEX makes this binary's tests serial, so exactly one is ever in flight. Scoped
///      to THIS binary on purpose: `--test-threads=1` on the whole gate was MEASURED at 365 s vs 140 s
///      for the lib suite alone (2.6x), which is ~14 minutes added to every sweep to instrument one
///      file. The mutex costs this binary a few seconds and nothing else anything.
///   2. An explicitly FLUSHED breadcrumb file. libtest's own "test <name> ... " line is written without
///      a trailing newline, and stdout to a pipe is line-buffered — the exact trap the ledger already
///      documented for Node — so on an abort that line can be lost with everything else in the buffer.
///      A flushed file survives, because the abort cannot unwind what is already on disk.
///
/// On a clean run every START is followed by its END. After an abort the last START stands alone, and
/// that is the answer. Best-effort throughout: a diagnostic that could fail a healthy test would be
/// worse than no diagnostic.
fn crash_breadcrumb(test_name: &'static str) -> impl Drop {
    use std::io::Write;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

    fn note(line: &str) {
        // Beside the other sweep artifacts, so a crash investigation finds it with the FAIL/CRASH logs.
        let path = std::env::temp_dir().join("cortex-verify10").join("e2e_pipeline.breadcrumb.log");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }

    struct Breadcrumb {
        name: &'static str,
        _guard: MutexGuard<'static, ()>,
    }
    impl Drop for Breadcrumb {
        fn drop(&mut self) {
            note(&format!("END   {}", self.name));
        }
    }

    // A poisoned lock means an EARLIER test panicked, which is not a reason to stop naming this one.
    let guard = SERIAL.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|p| p.into_inner());
    note(&format!("START {test_name}"));
    Breadcrumb { name: test_name, _guard: guard }
}

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
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_import_produces_segments");
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
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_vad_detects_speech");
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
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_vad_silent_fallback");
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
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_asr_no_models");
    let tmp = TempDir::new().unwrap();
    let mut asr = KurdishAsrService::new(tmp.path(), false).unwrap();
    assert!(!asr.is_available(), "ASR should not be available without models");

    let result = asr.transcribe(&[0.0f32; 16000], 16000);
    assert!(result.is_err(), "ASR should return error when models not loaded");
}

#[test]
fn test_e2e_normalizer_sorani() {
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_normalizer_sorani");
    let normalizer = SoraniNormalizer::new();

    let input = "ئەم تاقیکردنەیە";
    let result = normalizer.normalize(input);
    assert!(!result.is_empty(), "Normalizer should produce output");
}

#[test]
fn test_e2e_alignment() {
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_alignment");
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
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_model_manager_status");
    let tmp = TempDir::new().unwrap();
    let mgr = ModelManager::new(tmp.path().to_path_buf());
    mgr.ensure_dir().unwrap();

    let status = mgr.status();
    assert!(!status.is_empty(), "Should have model entries");

    let missing = mgr.missing_models();
    assert!(!missing.is_empty(), "Should have missing models in empty dir");
}

#[test]
fn test_e2e_decode_timeout() {
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_decode_timeout");
    let tmp = TempDir::new().unwrap();
    let wav_path = tmp.path().join("normal.wav");
    fixtures::create_test_wav(&wav_path, 0.5, 16000, 440.0).unwrap();

    let result = audio::decode_to_pcm_with_timeout(&wav_path, std::time::Duration::from_secs(5));
    assert!(result.is_ok(), "Normal decode should succeed within timeout");
}

#[test]
fn test_e2e_health_check() {
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_health_check");
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("health.db");
    let db_path_str = db_path.to_str().unwrap().to_string();
    let db = Database::open(&db_path_str).unwrap();
    db.initialize().unwrap();

    let mgr = ModelManager::new(tmp.path().to_path_buf());
    let health = health::health_check(&db, &mgr, None).unwrap();
    assert!(
        health["status"] == "ok" || health["status"] == "models_needed",
        "Health status should be ok or models_needed"
    );
    assert!(health.get("uptime").is_some(), "Health should include uptime");
}

#[test]
fn test_e2e_mixed_audio_formats() {
    let _crash_breadcrumb = crash_breadcrumb("test_e2e_mixed_audio_formats");
    let dir = TempDir::new().unwrap();
    fixtures::create_test_wav(&dir.path().join("16k.wav"), 0.3, 16000, 440.0).unwrap();
    fixtures::create_test_wav(&dir.path().join("44k.wav"), 0.2, 44100, 880.0).unwrap();

    let (pipeline, db_path, _tmp) = setup_pipeline();
    pipeline.import_directory(dir.path(), None, |_| {}).unwrap();

    let db = Database::open(&db_path).unwrap();
    let segments = db.get_segments(None).unwrap();
    assert!(segments.len() >= 2, "Should import both WAV files");
}
