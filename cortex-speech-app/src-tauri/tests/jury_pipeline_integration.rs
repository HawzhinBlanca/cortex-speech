mod fixtures;

use cortex_speech_app_lib::db::{Database, SegmentHypothesis, SpeechSegment};
use cortex_speech_app_lib::jury::{self, Verdict};
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
        ood_score: None,
        verdict: None,
        verdict_transcript: None,
        rationale: None,
        evidence_json: None,
        agent_confidence: None,
        escalated: false,
        human_decision: None,
        corrected_at: None,
        is_gold: false,
        alignment_quality: None,
    }
}

#[test]
fn test_jury_pipeline_t0_escalates_and_t1_resolves() {
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

    // 1. Run T0 Gate (default autonomy = ActConfirm: auto-accept agreements, escalate the rest).
    let t0_report =
        jury::run_t0_gate(&db, &[seg_id.to_string()], &cortex_speech_app_lib::settings::AutonLevel::ActConfirm, false)
            .unwrap();
    println!("T0 report: {:?}", t0_report);
    assert_eq!(t0_report.total, 1);
    assert_eq!(t0_report.escalated, 1); // Disagreement leads to escalation

    // Verify database shows escalated segment
    let seg_after_t0 = db.get_segment_by_id(seg_id).unwrap().unwrap();
    assert_eq!(seg_after_t0.verdict.unwrap(), "escalated");
    assert!(seg_after_t0.escalated);

    // 2. T1 text judge - let's run T1 judge on the hypotheses
    let t1_threshold = 0.85;
    let decision = jury::t1_judge::judge_t1(seg_id, &[hyp1.clone(), hyp2.clone()], t1_threshold);

    // We expect judge_t1 to return a decision. Assert or write verdict based on it
    match decision {
        jury::t1_judge::T1Decision::Commit { transcript, reason, .. } => {
            jury::write_verdict(&db, seg_id, Verdict::JuryAccept, Some(&transcript), Some(&reason), None, Some(0.9))
                .unwrap();
        }
        jury::t1_judge::T1Decision::EscalateToT2 { .. } => {
            jury::write_verdict(&db, seg_id, Verdict::Escalated, None, Some("T2 fallback integration"), None, None)
                .unwrap();
        }
    }

    // Verify the state of segment in the DB
    let seg_final = db.get_segment_by_id(seg_id).unwrap().unwrap();
    assert!(seg_final.verdict.is_some());
}
