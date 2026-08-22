mod fixtures;

use cortex_speech_app_lib::db::{Database, SegmentHypothesis, SpeechSegment};
use cortex_speech_app_lib::jury;
use tempfile::NamedTempFile;

fn make_seg(id: &str, path: &str, text: &str) -> SpeechSegment {
    SpeechSegment {
        id: id.to_string(),
        created_at: None,
        audio_path: path.to_string(),
        raw_transcript: text.to_string(),
        normalized_transcript: Some(text.to_string()),
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 2000,
        speaker_id: None,
        verified: false,
        confidence: Some(0.8),
        ctc_score: None,
        clipping_ratio: None,
        rms_db: None,
        snr_db: None,
        split: None,
        signal_anomaly_score: None,
        verdict: None,
        verdict_transcript: None,
        rationale: None,
        evidence_json: None,
        agreement_score: None,
        escalated: false,
        human_decision: None,
        corrected_at: None,
        is_gold: false,
        alignment_quality: None,
        ..SpeechSegment::default()
    }
}

#[test]
fn test_production_schema_keeps_t0_t1_advisory_until_human_review() {
    let tmp_db = NamedTempFile::new().unwrap();
    let db_path = tmp_db.path().to_str().unwrap().to_string();
    let db = Database::open(&db_path).unwrap();
    db.initialize().unwrap();

    let tmp_audio = NamedTempFile::new().unwrap();
    fixtures::create_test_wav(tmp_audio.path(), 1.0, 16000, 440.0).unwrap();
    let audio_path = tmp_audio.path().to_str().unwrap().to_string();

    // Insert segment and hypotheses (with some disagreement)
    let seg_id = "test_jury_seg_1";
    let seg = make_seg(seg_id, &audio_path, "کوردستان");
    db.insert_segment(&seg).unwrap();

    // Debug: check segment insertion
    let inserted_seg = db.get_segment_by_id(seg_id).unwrap();
    println!("Inserted segment: {:?}", inserted_seg);
    let all_segs = db.get_segments(None).unwrap();
    println!("All segments in DB: {:?}", all_segs);

    // Insert hypotheses
    let hyp1 = SegmentHypothesis {
        segment_id: seg_id.to_string(),
        model_id: "model1".to_string(),
        transcript: "کوردستان".to_string(),
        confidence: Some(0.9),
    };
    let hyp2 = SegmentHypothesis {
        segment_id: seg_id.to_string(),
        model_id: "model2".to_string(),
        transcript: "ئێران".to_string(),
        confidence: Some(0.45),
    };
    db.connection()
        .execute(
            "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![hyp1.segment_id, hyp1.model_id, hyp1.transcript, hyp1.confidence],
        )
        .unwrap();
    db.connection()
        .execute(
            "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![hyp2.segment_id, hyp2.model_id, hyp2.transcript, hyp2.confidence],
        )
        .unwrap();

    // Production schema v60+ deliberately refuses all legacy machine-verdict commits.
    let t0_error =
        jury::run_t0_gate(&db, &[seg_id.to_string()], &cortex_speech_app_lib::settings::AutonLevel::ActConfirm, false)
            .unwrap_err()
            .to_string();
    assert!(t0_error.contains("machine jury writes are disabled"), "unexpected boundary error: {t0_error}");

    // Refusal is atomic: the row remains undecided for evidence-backed human review.
    let seg_after_t0 = db.get_segment_by_id(seg_id).unwrap().unwrap();
    assert!(seg_after_t0.verdict.is_none());
    assert!(!seg_after_t0.escalated);

    // 2. T1 text judge - let's run T1 judge on the hypotheses
    let t1_threshold = 0.85;
    let decision = jury::t1_judge::judge_t1(seg_id, &[hyp1.clone(), hyp2.clone()], t1_threshold);

    // T1 remains advisory at schema v60+: machine verdict writers cannot mutate a row after the
    // evidence boundary. Prove the judge returns a typed decision without bypassing that boundary.
    match decision {
        jury::t1_judge::T1Decision::Commit { transcript, reason, .. } => {
            assert!(!transcript.trim().is_empty());
            assert!(!reason.trim().is_empty());
        }
        jury::t1_judge::T1Decision::EscalateToT2 { segment_id, .. } => assert_eq!(segment_id, seg_id),
    }

    // Advisory computation still cannot manufacture persisted truth.
    let seg_final = db.get_segment_by_id(seg_id).unwrap().unwrap();
    assert!(seg_final.verdict.is_none());
    assert!(!seg_final.escalated);
}
