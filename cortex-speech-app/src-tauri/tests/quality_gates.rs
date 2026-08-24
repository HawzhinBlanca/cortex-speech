//! CI quality gate tests — WER/CER thresholds must pass on fixture data.

mod fixtures;

use cortex_speech_app_lib::db::{Database, SpeechSegment};
use cortex_speech_app_lib::quality;
use cortex_speech_app_lib::settings::AppSettings;
use cortex_speech_app_lib::validation::{validate_dataset_with_settings, IssueCategory};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn annotated_seg(dir: &Path, id: &str, reference: &str, hypothesis: &str) -> SpeechSegment {
    let wav = dir.join(format!("{id}.wav"));
    fixtures::create_test_wav(&wav, 0.5, 16000, 440.0).expect("wav");
    SpeechSegment {
        id: id.to_string(),
        created_at: None,
        audio_path: wav.to_string_lossy().into_owned(),
        raw_transcript: hypothesis.to_string(),
        normalized_transcript: Some(hypothesis.to_string()),
        annotated_transcript: Some(reference.to_string()),
        alignment_json: None,
        duration_ms: 2000,
        speaker_id: Some("SPEAKER_00".to_string()),
        verified: true,
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

fn setup_db_with_audio() -> (Database, TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let audio_dir = tmp.path().to_path_buf();
    let db_path = tmp.path().join("quality.db");
    let db = Database::open(db_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    // These fixtures intentionally exercise the historical annotated-row quality metric, not the
    // schema-v60 human-decision write boundary. Author them only on the exact legacy schema where
    // whole-row review fields were legal; production schemas remain fail-closed.
    assert_eq!(cortex_speech_app_lib::migrations::rollback(&db, 6).unwrap(), vec![65, 64, 63, 62, 61, 60]);
    (db, tmp, audio_dir)
}

#[test]
fn quality_gate_passes_on_matching_annotations() {
    let (db, _tmp, dir) = setup_db_with_audio();
    db.insert_segment(&annotated_seg(&dir, "g1", "سلاو دنیا", "سلاو دنیا")).unwrap();

    let settings = AppSettings {
        enforce_quality_gates: true,
        max_wer_threshold: 0.35,
        max_cer_threshold: 0.20,
        ..AppSettings::default()
    };

    let quality = quality::compute_quality_with_settings(&db, &settings).unwrap();
    assert!(quality.quality_gate_passed);
    assert_eq!(quality.segments_above_wer_threshold, 0);

    let report = validate_dataset_with_settings(&db, &settings).unwrap();
    assert!(report.errors.is_empty());
}

#[test]
fn quality_gate_fails_on_high_wer() {
    let (db, _tmp, dir) = setup_db_with_audio();
    db.insert_segment(&annotated_seg(&dir, "g2", "one two three four five", "completely different words here"))
        .unwrap();

    let settings = AppSettings {
        enforce_quality_gates: true,
        max_wer_threshold: 0.20,
        max_cer_threshold: 0.20,
        ..AppSettings::default()
    };

    let quality = quality::compute_quality_with_settings(&db, &settings).unwrap();
    assert!(!quality.quality_gate_passed);
    assert!(quality.segments_above_wer_threshold >= 1);

    let report = validate_dataset_with_settings(&db, &settings).unwrap();
    assert!(report
        .errors
        .iter()
        .any(|i| i.category == IssueCategory::QualityGateFailed || i.category == IssueCategory::HighWer));
}

#[test]
fn high_wer_is_warning_when_gates_disabled() {
    let (db, _tmp, dir) = setup_db_with_audio();
    db.insert_segment(&annotated_seg(&dir, "g3", "hello world", "goodbye moon")).unwrap();

    let settings = AppSettings { enforce_quality_gates: false, max_wer_threshold: 0.10, ..AppSettings::default() };

    let report = validate_dataset_with_settings(&db, &settings).unwrap();
    assert!(report.errors.is_empty());
    assert!(report.warnings.iter().any(|i| i.category == IssueCategory::HighWer));
}
