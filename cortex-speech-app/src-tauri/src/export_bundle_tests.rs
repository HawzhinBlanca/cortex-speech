//! Unit tests for `export_bundle.rs`, split out via `#[path]` (Week-4 decomposition) to keep
//! export_bundle.rs under the 3-4k-line target. Included from export_bundle.rs as
//! `#[cfg(test)] #[path = "export_bundle_tests.rs"] mod tests;`; `super::*` still resolves to the parent
//! module. Tests are UNCHANGED — only relocated (dedented one level; rustfmt canonicalization applied).

use super::*;
use crate::db::{SegmentHypothesis, SourceTranscriptRecord, SpeechSegment};
use tempfile::TempDir;

/// Declare full redistribution rights on a source recording.
///
/// A PRODUCTION bundle is the artifact that leaves the machine, so it refuses clips whose rights are
/// undeclared (audit 2026-08-05 #3). Every production-success fixture therefore has to say what it is
/// permitted to do, exactly as a real operator must. Deliberately NOT folded into the shared segment
/// helpers: rights are what is under test in `production_export_blocks_clips_without_declared_
/// redistribution_rights`, and granting them by default in a helper would silence that gate for every
/// test at once.
fn declare_redistribution_rights(db: &Database, audio_path: &str) {
    db.set_recording_rights(
        audio_path,
        &crate::db::RecordingRights {
            license: Some("CC-BY-4.0".into()),
            consent_basis: Some("explicit_written_consent".into()),
            permitted_use: Some("train,redistribute".into()),
            attribution: Some("test speaker".into()),
            source: Some("unit-test fixture".into()),
            revoked_at: None,
        },
    )
    .unwrap();
}

fn json_string_values_contain(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(text) => text.contains(needle),
        serde_json::Value::Array(items) => items.iter().any(|item| json_string_values_contain(item, needle)),
        serde_json::Value::Object(map) => map.values().any(|item| json_string_values_contain(item, needle)),
        _ => false,
    }
}

fn assert_json_strings_do_not_contain(value: &serde_json::Value, needle: &str, artifact: &str) {
    assert!(!json_string_values_contain(value, needle), "{artifact} leaked private path fragment: {needle}");
}

fn insert_machine_silver_segment_with_coverage(db: &Database, tmp: &TempDir, segment_id: &str) -> (String, String) {
    let audio = tmp.path().join(format!("{segment_id}.wav"));
    std::fs::write(&audio, b"audio").unwrap();
    let audio_path = audio.to_string_lossy().to_string();
    let evidence_json = serde_json::json!({
        "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
        "selectedModelId": "omniasr-wsl-7b",
        "selectedTranscript": "reference candidate",
        "shouldCommit": true
    })
    .to_string();
    let segment = SpeechSegment {
        id: segment_id.to_string(),
        created_at: None,
        audio_path: audio_path.clone(),
        raw_transcript: "reference candidate".into(),
        normalized_transcript: None,
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1200,
        speaker_id: Some("spk1".into()),
        verified: false,
        confidence: Some(0.95),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        verdict: Some("jury_accept".into()),
        verdict_transcript: Some("reference candidate".into()),
        rationale: Some("multi-reference consensus committed agent text".into()),
        evidence_json: Some(evidence_json.clone()),
        agent_confidence: Some(0.92),
        escalated: false,
        human_decision: None,
        corrected_at: None,
        is_gold: false,
        alignment_quality: None,
        ..SpeechSegment::default()
    };
    db.insert_segment(&segment).unwrap();
    db.write_segment_verdict(
        &segment.id,
        "jury_accept",
        Some("reference candidate"),
        Some("multi-reference consensus committed agent text"),
        Some(evidence_json.as_str()),
        Some(0.92),
        false,
    )
    .unwrap();
    for model_id in ["omniasr-wsl-7b", "omniasr-ctc-300m"] {
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: model_id.to_string(),
            transcript: "reference candidate".to_string(),
            confidence: Some(0.95),
        })
        .unwrap();
    }
    (audio_path, segment.id)
}

fn insert_current_identity_source_reference(db: &Database, audio_path: &str, model_id: &str) {
    let identity = crate::pipeline::source_audio_identity(Path::new(audio_path)).unwrap();
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: audio_path.to_string(),
        model_id: model_id.to_string(),
        audio_content_hash: Some(identity.content_hash),
        audio_size_bytes: Some(identity.size_bytes),
        transcript_path: format!("source_transcripts\\{}__{model_id}.txt", sanitize_bundle_filename(model_id)),
        transcript_text: "whole file reference transcript".to_string(),
        created_at: None,
    })
    .unwrap();
}

fn record_ready_agentic_promotion_report(
    db: &Database,
    audio_path: &str,
    segment_id: &str,
    run_id: &str,
) -> crate::runs::AgentImportReport {
    crate::runs::record_agent_import_report_with_options(
        db,
        "file",
        &[audio_path.to_string()],
        &[segment_id.to_string()],
        Some(&serde_json::json!({
            "referenceCommitted": 1,
            "humanInbox": 0,
        })),
        None,
        crate::runs::AgentImportReportOptions {
            agent_run_id: Some(run_id.to_string()),
            agentic_readiness: Some(serde_json::json!({
                "status": "ready",
                "ready": true,
                "sourceReferenceModels": ["gemini-2.5-pro", "gemini-2.5-flash"],
                "availableHypothesisModels": ["omniasr-wsl-7b", "omniasr-ctc-300m"],
                "requiredHypothesisModels": 2,
                "checks": [{
                    "id": "hypothesis_coverage",
                    "label": "Multi-model hypothesis coverage",
                    "status": "ready",
                    "detail": "Two hypothesis models are ready"
                }]
            })),
            ..crate::runs::AgentImportReportOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn production_export_blocks_on_validation_errors() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    db.insert_segment(&SpeechSegment {
        id: "missing-audio".into(),
        created_at: None,
        audio_path: "C:\\definitely\\missing\\audio.wav".into(),
        raw_transcript: "test".into(),
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
    })
    .unwrap();
    let settings = AppSettings::default();
    let models = ModelManager::new(TempDir::new().unwrap().path().join("models"));
    let out = TempDir::new().unwrap();
    let err = export_dataset_bundle(&db, &models, out.path(), &settings, true, 0).unwrap_err();
    assert!(err.to_string().contains("Production export blocked"));
}

#[test]
fn production_export_blocks_when_warnings_exceed_threshold() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("sample.wav");
    std::fs::write(&audio, b"audio").unwrap();
    db.insert_segment(&SpeechSegment {
        id: "empty-transcript".into(),
        created_at: None,
        audio_path: audio.to_string_lossy().to_string(),
        raw_transcript: "".into(),
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
    })
    .unwrap();
    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");

    let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, 0).unwrap_err();

    assert!(err.to_string().contains("Production export blocked"));
    assert!(!out.join("manifest.json").exists(), "blocked production export must not write bundle files");
}

#[test]
fn production_export_blocks_when_no_segments_are_training_ready() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("sample.wav");
    std::fs::write(&audio, b"audio").unwrap();
    db.insert_segment(&SpeechSegment {
        id: "review-only".into(),
        created_at: None,
        audio_path: audio.to_string_lossy().to_string(),
        raw_transcript: "needs review".into(),
        normalized_transcript: None,
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1000,
        speaker_id: None,
        verified: false,
        confidence: Some(0.60),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: None,
        signal_anomaly_score: None,
        ..SpeechSegment::default()
    })
    .unwrap();
    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");

    let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap_err();

    assert!(err.to_string().contains("no training-ready segments"));
    assert!(!out.join("manifest.json").exists(), "blocked production export must not write bundle files");
}

#[test]
fn production_export_blocks_machine_ready_rows_without_hypothesis_coverage() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("weak-machine.wav");
    std::fs::write(&audio, b"audio").unwrap();
    let audio_path = audio.to_string_lossy().to_string();
    let segment = SpeechSegment {
        id: "weak-machine-seg-1".into(),
        created_at: None,
        audio_path,
        raw_transcript: "reference candidate".into(),
        normalized_transcript: None,
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1200,
        speaker_id: Some("spk1".into()),
        verified: false,
        confidence: Some(0.95),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        verdict: Some("jury_accept".into()),
        verdict_transcript: Some("reference candidate".into()),
        rationale: Some("legacy source-reference commit".into()),
        evidence_json: Some(
            serde_json::json!({
                "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
                "selectedModelId": "omniasr-wsl-7b",
                "shouldCommit": true
            })
            .to_string(),
        ),
        agent_confidence: Some(0.92),
        escalated: false,
        human_decision: None,
        corrected_at: None,
        is_gold: false,
        alignment_quality: None,
        ..SpeechSegment::default()
    };
    db.insert_segment(&segment).unwrap();
    let weak_evidence = serde_json::json!({
        "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
        "selectedModelId": "omniasr-wsl-7b",
        "selectedTranscript": "reference candidate",
        "shouldCommit": true
    })
    .to_string();
    db.write_segment_verdict(
        &segment.id,
        "jury_accept",
        Some("reference candidate"),
        Some("legacy source-reference commit"),
        Some(weak_evidence.as_str()),
        Some(0.92),
        false,
    )
    .unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "omniasr-wsl-7b".to_string(),
        transcript: "reference candidate".to_string(),
        confidence: Some(0.95),
    })
    .unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap_err();

    let err_text = err.to_string();
    assert!(err_text.contains("missing multi-model hypothesis coverage"), "{err_text}");
    assert!(err_text.contains("weak-machine-seg-1"), "{err_text}");
    assert!(!out.join("manifest.json").exists(), "blocked production export must not write bundle files");
}

#[test]
fn production_export_allows_human_gold_without_hypothesis_coverage() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("human-gold.wav");
    std::fs::write(&audio, b"audio").unwrap();
    db.insert_segment(&SpeechSegment {
        id: "human-gold-seg-1".into(),
        created_at: None,
        audio_path: audio.to_string_lossy().to_string(),
        raw_transcript: "human checked text".into(),
        normalized_transcript: None,
        annotated_transcript: Some("human checked text".into()),
        alignment_json: None,
        duration_ms: 1200,
        speaker_id: Some("spk1".into()),
        verified: true,
        confidence: Some(0.95),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        ..SpeechSegment::default()
    })
    .unwrap();
    declare_redistribution_rights(&db, &audio.to_string_lossy());

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let result = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap();

    assert!(Path::new(&result.manifest_path).exists());
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["trainingReadySegments"].as_u64(), Some(1));
    assert_eq!(manifest["trainingGradeSummary"]["goldSegments"].as_u64(), Some(1));
}

#[test]
fn production_export_blocks_machine_ready_rows_without_ready_agentic_promotion_report() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let (_audio_path, segment_id) = insert_machine_silver_segment_with_coverage(&db, &tmp, "machine-no-agent-report");

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap_err();

    let err_text = err.to_string();
    assert!(err_text.contains("ready agentic promotion report"), "{err_text}");
    assert!(err_text.contains("missing_agent_report"), "{err_text}");
    assert!(err_text.contains(segment_id.as_str()), "{err_text}");
    assert!(!out.join("manifest.json").exists(), "blocked production export must not write bundle files");
}

#[test]
fn production_export_blocks_machine_ready_rows_not_covered_by_latest_agentic_promotion_report() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let (_uncovered_audio_path, uncovered_segment_id) =
        insert_machine_silver_segment_with_coverage(&db, &tmp, "machine-uncovered-by-report");
    let (covered_audio_path, covered_segment_id) =
        insert_machine_silver_segment_with_coverage(&db, &tmp, "machine-covered-by-report");
    crate::runs::record_agent_import_report_with_options(
        &db,
        "file",
        std::slice::from_ref(&covered_audio_path),
        std::slice::from_ref(&covered_segment_id),
        Some(&serde_json::json!({
            "referenceCommitted": 1,
            "humanInbox": 0,
        })),
        None,
        crate::runs::AgentImportReportOptions {
            agent_run_id: Some("run-stale-agentic-production".to_string()),
            agentic_readiness: Some(serde_json::json!({
                "status": "ready",
                "ready": true,
                "sourceReferenceModels": ["gemini-2.5-pro", "gemini-2.5-flash"],
                "availableHypothesisModels": ["omniasr-wsl-7b", "omniasr-ctc-300m"],
                "requiredHypothesisModels": 2,
                "checks": [{
                    "id": "hypothesis_coverage",
                    "label": "Multi-model hypothesis coverage",
                    "status": "ready",
                    "detail": "Two hypothesis models are ready"
                }]
            })),
            ..crate::runs::AgentImportReportOptions::default()
        },
    )
    .unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap_err();

    let err_text = err.to_string();
    assert!(err_text.contains("not covered by the latest ready agentic promotion report"), "{err_text}");
    assert!(err_text.contains(uncovered_segment_id.as_str()), "{err_text}");
    assert!(!err_text.contains(covered_segment_id.as_str()), "{err_text}");
    assert!(!out.join("manifest.json").exists(), "blocked production export must not write bundle files");
}

/// The gate itself: a clip good enough to publish, with nothing said about whether it MAY be.
#[test]
fn production_export_blocks_clips_without_declared_redistribution_rights() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let (audio_path, segment_id) = insert_machine_silver_segment_with_coverage(&db, &tmp, "rights-undeclared");
    for model_id in ["gemini-2.5-pro", "gemini-2.5-flash"] {
        insert_current_identity_source_reference(&db, &audio_path, model_id);
    }
    record_ready_agentic_promotion_report(&db, &audio_path, &segment_id, "run-rights-undeclared");
    // Every OTHER production gate is satisfied — deliberately. If this test only proved that a
    // half-finished clip is blocked it would prove nothing about rights.

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap_err();

    let err_text = err.to_string();
    assert!(err_text.contains("no declared redistribution rights"), "{err_text}");
    assert!(err_text.contains(segment_id.as_str()), "must name the offending clip: {err_text}");
    assert!(!out.join("manifest.json").exists(), "blocked production export must not write bundle files");
}

/// A licence is not consent to republish a voice, and `train` is not `redistribute`. Both are the
/// plausible near-misses an operator actually produces, so both are pinned.
#[test]
fn production_export_blocks_rights_that_stop_short_of_redistribution() {
    for (label, rights) in [
        (
            "licence and consent but permitted_use omits redistribution",
            crate::db::RecordingRights {
                license: Some("CC-BY-4.0".into()),
                consent_basis: Some("explicit_written_consent".into()),
                permitted_use: Some("train".into()),
                attribution: None,
                source: None,
                revoked_at: None,
            },
        ),
        (
            "permitted_use names redistribution but no consent basis is recorded",
            crate::db::RecordingRights {
                license: Some("CC-BY-4.0".into()),
                consent_basis: None,
                permitted_use: Some("redistribute".into()),
                attribution: None,
                source: None,
                revoked_at: None,
            },
        ),
    ] {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let tmp = TempDir::new().unwrap();
        let (audio_path, segment_id) = insert_machine_silver_segment_with_coverage(&db, &tmp, "rights-partial");
        for model_id in ["gemini-2.5-pro", "gemini-2.5-flash"] {
            insert_current_identity_source_reference(&db, &audio_path, model_id);
        }
        record_ready_agentic_promotion_report(&db, &audio_path, &segment_id, "run-rights-partial");
        db.set_recording_rights(&audio_path, &rights).unwrap();

        let models = ModelManager::new(tmp.path().join("models"));
        let out = tmp.path().join("bundle");
        let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap_err();
        assert!(err.to_string().contains("no declared redistribution rights"), "{label}: {err}");
    }
}

/// The gate is scoped to PUBLICATION. Local dataset preparation must keep working on undeclared
/// clips, or this change would silently redefine what the owner's everyday export command does.
#[test]
fn local_export_still_works_without_declared_rights() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let (audio_path, _segment_id) = insert_machine_silver_segment_with_coverage(&db, &tmp, "rights-local-only");
    for model_id in ["gemini-2.5-pro", "gemini-2.5-flash"] {
        insert_current_identity_source_reference(&db, &audio_path, model_id);
    }

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let result = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();
    assert!(Path::new(&result.manifest_path).exists(), "a local bundle must not need publication rights");
}

#[test]
fn production_export_allows_machine_ready_rows_with_ready_agentic_promotion_report() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let (audio_path, segment_id) = insert_machine_silver_segment_with_coverage(&db, &tmp, "machine-ready-agent-report");
    for model_id in ["gemini-2.5-pro", "gemini-2.5-flash"] {
        insert_current_identity_source_reference(&db, &audio_path, model_id);
    }
    let agent_report =
        record_ready_agentic_promotion_report(&db, &audio_path, &segment_id, "run-ready-agentic-production");
    declare_redistribution_rights(&db, &audio_path);

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let result = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap();

    assert!(Path::new(&result.manifest_path).exists());
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["trainingReadySegments"].as_u64(), Some(1));
    assert_eq!(manifest["trainingGradeSummary"]["silverSegments"].as_u64(), Some(1));
    assert_eq!(manifest["agentPromotionReadiness"]["reportId"].as_str(), Some(agent_report.id.as_str()));
    assert_eq!(manifest["agentPromotionReadiness"]["status"].as_str(), Some("ready"));
    assert_eq!(manifest["agentPromotionReadiness"]["segmentIds"][0].as_str(), Some(segment_id.as_str()));
    assert_eq!(manifest["agentPromotionReadiness"]["agenticReadiness"]["status"].as_str(), Some("ready"));
    assert_eq!(manifest["agentPromotionReadiness"]["agenticReadiness"]["ready"].as_bool(), Some(true));
    let source_reference_manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("source_reference_manifest.json")).unwrap()).unwrap();
    assert_eq!(source_reference_manifest["schemaVersion"].as_u64(), Some(4));
    assert_eq!(source_reference_manifest["sourceReferenceCount"].as_u64(), Some(2));
    assert!(source_reference_manifest["references"]
        .as_array()
        .unwrap()
        .iter()
        .all(|reference| reference["audioIdentityVerified"].as_bool() == Some(true)));
}

#[test]
fn bundle_reexport_removes_orphan_source_reference_of_a_dropped_clip() {
    // HOLDOUT-CONTAMINATION / manifest-vs-disk (hunt-117): source_transcripts/*.txt use content-hashed
    // variable names, and export_dataset_bundle writes into a REUSED output_dir without clearing it. A clip
    // present in an earlier export but DROPPED from a later one (its segment now human-rejected, or
    // registered as a gold holdout — both filtered out of `segments`) would otherwise leave its old
    // reference .txt on disk: re-hashed into SHA256SUMS by the whole-tree walk, absent from
    // source_reference_manifest.json, and (worst case) its HUMAN reference transcript — the WER/CER answer
    // key — left inside the "holdout-free" bundle. A re-export must not leave that orphan.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let (_audio_keep, _id_keep) = insert_machine_silver_segment_with_coverage(&db, &tmp, "keep-seg");
    let (audio_drop, id_drop) = insert_machine_silver_segment_with_coverage(&db, &tmp, "drop-seg");
    insert_current_identity_source_reference(&db, &audio_drop, "gemini-2.5-pro");

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let source_dir = out.join("source_transcripts");

    // Export 1: the clip is present, so its source-reference .txt is written.
    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();
    let orphans_before: Vec<std::path::PathBuf> =
        std::fs::read_dir(&source_dir).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(orphans_before.len(), 1, "run 1 must write the clip's source-reference txt, got {orphans_before:?}");

    // The clip is dropped from the next bundle (human-rejected -> filtered out of `segments`).
    db.record_human_decision(&id_drop, "reject", None, None).unwrap();

    // Export 2 into the SAME dir.
    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    // The orphan .txt must be gone — not left behind to be re-hashed into SHA256SUMS.
    let orphans_after: Vec<std::path::PathBuf> =
        std::fs::read_dir(&source_dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    assert!(
        orphans_after.is_empty(),
        "re-export must remove the dropped clip's orphan source-reference txt, found {orphans_after:?}"
    );
    for p in &orphans_before {
        assert!(!p.exists(), "orphan source-reference file must be deleted on re-export: {p:?}");
    }
    // SHA256SUMS must not list a source_transcripts orphan (manifest-vs-disk consistency).
    let sums = std::fs::read_to_string(out.join("SHA256SUMS")).unwrap();
    assert!(!sums.contains("source_transcripts"), "no orphan source-reference file in the integrity manifest:\n{sums}");
}

#[test]
fn bundle_excludes_holdout_gold_from_all_artifacts() {
    // Round-22 #1 (bundle completion): the holdout gold clip's HUMAN REFERENCE transcript and its
    // per-segment detail must NOT ship in the bundle SIDECARS (source_transcripts/*.txt,
    // source_reference_manifest.json, training_grade_details.json, manifest counts) — not just the
    // tabular dataset files. A non-production bundle exercises the sidecars without the agentic
    // gates. The holdout is registered by PATH so no audio file needs to exist.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();

    let mk = |id: &str, path: &str, transcript: &str| SpeechSegment {
        id: id.into(),
        created_at: None,
        audio_path: path.into(),
        raw_transcript: transcript.into(),
        normalized_transcript: None,
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1200,
        speaker_id: None,
        verified: true,
        confidence: Some(0.95),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
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
        ..SpeechSegment::default()
    };
    db.insert_segment(&mk("keep-seg", "/data/keep.wav", "KEEPMARKERTEXT")).unwrap();
    db.insert_segment(&mk("hold-seg", "/data/holdout.wav", "HOLDOUTMARKERTEXT")).unwrap();
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: "/data/holdout.wav".to_string(),
        model_id: "gemini-2.5-pro".to_string(),
        audio_content_hash: None,
        audio_size_bytes: None,
        transcript_path: "source_transcripts/holdout__gemini.txt".to_string(),
        transcript_text: "HOLDOUTREFERENCETEXT".to_string(),
        created_at: None,
    })
    .unwrap();
    crate::eval::import_gold_segments(
        &db,
        vec![crate::eval::GoldSegmentInput {
            audio_path: "/data/holdout.wav".into(),
            reference: "HOLDOUTREFERENCETEXT".into(),
            is_holdout: true,
        }],
    )
    .unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    let mut blob = String::new();
    for name in [
        "training_grade_details.json",
        "source_reference_manifest.json",
        "manifest.json",
        "dataset.csv",
        "dataset.jsonl",
    ] {
        blob.push_str(&std::fs::read_to_string(out.join(name)).unwrap_or_default());
    }
    if let Ok(rd) = std::fs::read_dir(out.join("source_transcripts")) {
        for entry in rd.flatten() {
            blob.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    assert!(blob.contains("KEEPMARKERTEXT"), "the non-holdout segment must still be in the bundle");
    assert!(!blob.contains("HOLDOUTMARKERTEXT"), "holdout transcript must NOT leak into any bundle artifact");
    assert!(!blob.contains("HOLDOUTREFERENCETEXT"), "holdout source-reference must NOT leak into the bundle");
}

#[test]
fn bundle_manifest_count_excludes_placeholders_matching_the_shipped_data_files() {
    // A not-yet-transcribed PLACEHOLDER row ("[Pending WSL 7B ASR]") is dropped from the tabular data
    // files by export::export_dataset, so the bundle manifest/card counts must exclude it too — otherwise
    // segmentCount claims more rows than dataset.{json,jsonl,csv,parquet} actually ship (a dishonest,
    // inflated number that also disagrees with dataset.json's own embedded total).
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();

    let mk = |id: &str, path: &str, raw: &str, verified: bool| SpeechSegment {
        id: id.into(),
        audio_path: path.into(),
        raw_transcript: raw.into(),
        duration_ms: 1000,
        verified,
        ..SpeechSegment::default()
    };
    db.insert_segment(&mk("real", "/data/real.wav", "دەقی ڕاست", true)).unwrap();
    db.insert_segment(&mk("pending", "/data/pending.wav", "[Pending WSL 7B ASR]", false)).unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["segmentCount"].as_u64(),
        Some(1),
        "manifest must count only the 1 real segment, not the placeholder: {manifest}"
    );

    // The shipped data files ship exactly the real row and never the placeholder string.
    let jsonl = std::fs::read_to_string(out.join("dataset.jsonl")).unwrap();
    assert!(jsonl.contains("دەقی ڕاست"), "the real segment ships in the data files");
    assert!(!jsonl.contains("[Pending WSL 7B ASR]"), "the placeholder must never ship in the data files");

    // The human-readable card's Segments line agrees with the manifest (both exclude the placeholder).
    let card = std::fs::read_to_string(out.join("dataset_card.md")).unwrap();
    assert!(
        card.contains("Segments: 1"),
        "the card count must match the shipped rows, not include the placeholder: {card}"
    );
}

#[test]
fn production_export_blocks_source_reference_without_audio_identity() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let (audio_path, segment_id) =
        insert_machine_silver_segment_with_coverage(&db, &tmp, "machine-legacy-source-reference");
    for model_id in ["gemini-2.5-pro", "gemini-2.5-flash"] {
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: model_id.to_string(),
            audio_content_hash: None,
            audio_size_bytes: None,
            transcript_path: format!("source_transcripts\\machine-legacy-source-reference__{model_id}.txt"),
            transcript_text: "whole file reference transcript".to_string(),
            created_at: None,
        })
        .unwrap();
    }
    record_ready_agentic_promotion_report(&db, &audio_path, &segment_id, "run-legacy-source-reference-production");

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap_err();

    let err_text = err.to_string();
    assert!(err_text.contains("source-reference transcripts have missing or stale audio identity"), "{err_text}");
    assert!(err_text.contains("missing_stored_audio_identity"), "{err_text}");
    assert!(err_text.contains("gemini-2.5-pro"), "{err_text}");
    assert!(!out.join("manifest.json").exists(), "blocked production export must not write bundle files");
}

#[test]
fn production_export_blocks_stale_source_reference_audio_identity() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let (audio_path, segment_id) =
        insert_machine_silver_segment_with_coverage(&db, &tmp, "machine-stale-source-reference");
    for model_id in ["gemini-2.5-pro", "gemini-2.5-flash"] {
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: model_id.to_string(),
            audio_content_hash: Some("stale-audio-hash".to_string()),
            audio_size_bytes: Some(1),
            transcript_path: format!("source_transcripts\\machine-stale-source-reference__{model_id}.txt"),
            transcript_text: "whole file reference transcript".to_string(),
            created_at: None,
        })
        .unwrap();
    }
    record_ready_agentic_promotion_report(&db, &audio_path, &segment_id, "run-stale-source-reference-production");

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let err = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), true, usize::MAX).unwrap_err();

    let err_text = err.to_string();
    assert!(err_text.contains("source-reference transcripts have missing or stale audio identity"), "{err_text}");
    assert!(err_text.contains("stored_audio_identity_mismatch"), "{err_text}");
    assert!(err_text.contains("gemini-2.5-flash"), "{err_text}");
    assert!(!out.join("manifest.json").exists(), "blocked production export must not write bundle files");
}

#[test]
fn draft_export_writes_complete_release_bundle() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("sample.wav");
    std::fs::write(&audio, b"audio").unwrap();
    db.insert_segment(&SpeechSegment {
        id: "seg-1".into(),
        created_at: None,
        audio_path: audio.to_string_lossy().to_string(),
        raw_transcript: "دەنگ".into(),
        normalized_transcript: None,
        annotated_transcript: Some("دەنگ".into()),
        alignment_json: None,
        duration_ms: 1000,
        speaker_id: Some("spk1".into()),
        verified: true,
        confidence: Some(0.95),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        ..SpeechSegment::default()
    })
    .unwrap();
    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let result = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    let expected_files = [
        "manifest.json",
        "dataset_card.md",
        "dataset.json",
        "dataset.jsonl",
        "dataset.csv",
        "dataset.parquet",
        "validation_report.json",
        "quality_report.json",
        "training_grade_summary.json",
        "training_grade_details.json",
        "source_reference_manifest.json",
        "learning_manifest.json",
        "long_file_dossiers.json",
        "agent_provenance.json",
        "model_manifest.json",
        // True-10 audit 2026-07-09: integrity sums over every bundle file (clips included).
        "SHA256SUMS",
    ];
    let actual_files = result.files.iter().map(String::as_str).collect::<std::collections::BTreeSet<_>>();
    let expected_set = expected_files.into_iter().collect::<std::collections::BTreeSet<_>>();

    assert_eq!(actual_files, expected_set);
    assert!(Path::new(&result.manifest_path).exists());
    for file in expected_files {
        let path = out.join(file);
        assert!(path.exists(), "missing bundle file: {file}");
        assert!(path.metadata().unwrap().len() > 0, "bundle file is empty: {file}");
    }

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    let manifest_files = manifest["files"]
        .as_array()
        .expect("manifest files")
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();
    for file in [
        "dataset.json",
        "dataset.jsonl",
        "dataset.csv",
        "dataset.parquet",
        "validation_report.json",
        "quality_report.json",
        "training_grade_summary.json",
        "training_grade_details.json",
        "source_reference_manifest.json",
        "learning_manifest.json",
        "long_file_dossiers.json",
        "agent_provenance.json",
        "model_manifest.json",
    ] {
        assert!(manifest_files.contains(&file), "manifest file list is missing {file}");
    }
    assert_eq!(manifest["trainingReadySegments"].as_u64(), Some(1));
    assert_eq!(manifest["trainingGradeSummary"]["goldSegments"].as_u64(), Some(1));
    let grade_summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("training_grade_summary.json")).unwrap()).unwrap();
    assert_eq!(grade_summary["trainingReadySegments"].as_u64(), Some(1));
    let grade_details: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("training_grade_details.json")).unwrap()).unwrap();
    assert_eq!(grade_details["segmentCount"].as_u64(), Some(1));
    assert_eq!(grade_details["trainingReadySegmentCount"].as_u64(), Some(1));
    assert_eq!(grade_details["segments"][0]["segmentId"].as_str(), Some("seg-1"));
    assert_eq!(grade_details["segments"][0]["grade"].as_str(), Some("gold"));
    assert_eq!(grade_details["segments"][0]["trainingReady"].as_bool(), Some(true));
    assert_eq!(grade_details["segments"][0]["transcriptSource"].as_str(), Some("human_verified"));
    let source_reference_manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("source_reference_manifest.json")).unwrap()).unwrap();
    assert_eq!(source_reference_manifest["sourceReferenceCount"].as_u64(), Some(0));
    let learning_manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("learning_manifest.json")).unwrap()).unwrap();
    assert_eq!(learning_manifest["pairCount"].as_u64(), Some(0));
    assert!(learning_manifest["preferencesPath"].is_null());
    assert!(!out.join("learning_preferences.jsonl").exists());
    let agent_provenance: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("agent_provenance.json")).unwrap()).unwrap();
    assert_eq!(agent_provenance["agentImportReportCount"].as_u64(), Some(0));
    assert_eq!(agent_provenance["sourceReferenceCount"].as_u64(), Some(0));
}

#[test]
fn training_grade_details_records_hypothesis_coverage_evidence() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("coverage.wav");
    std::fs::write(&audio, b"audio").unwrap();
    let segment = SpeechSegment {
        id: "coverage-seg-1".into(),
        created_at: None,
        audio_path: audio.to_string_lossy().to_string(),
        raw_transcript: "coverage transcript".into(),
        normalized_transcript: None,
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1400,
        speaker_id: Some("spk1".into()),
        verified: false,
        confidence: Some(0.91),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        ..SpeechSegment::default()
    };
    db.insert_segment(&segment).unwrap();
    for (model_id, transcript, confidence) in [
        ("omniasr-ctc-300m", "coverage transcript", Some(0.91)),
        ("omniasr-ctc-1b", "[ASR unavailable: model missing]", Some(0.0)),
        ("asr", "coverage transcript", Some(0.91)),
    ] {
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: model_id.to_string(),
            transcript: transcript.to_string(),
            confidence,
        })
        .unwrap();
    }

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    let grade_details: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("training_grade_details.json")).unwrap()).unwrap();
    let coverage = &grade_details["segments"][0]["hypothesisCoverage"];
    assert_eq!(coverage["minimumNonEmptyModelCount"].as_u64(), Some(2));
    assert_eq!(coverage["nonEmptyModelCount"].as_u64(), Some(1));
    assert_eq!(coverage["passesMinimum"].as_bool(), Some(false));
    assert_eq!(
        coverage["nonEmptyModels"].as_array().unwrap(),
        &vec![serde_json::Value::String("omniasr-ctc-300m".into())]
    );
    let ignored = coverage["ignoredModels"].as_array().unwrap();
    assert!(ignored.contains(&serde_json::Value::String("asr".into())));
    assert!(ignored.contains(&serde_json::Value::String("omniasr-ctc-1b".into())));
}

#[test]
fn draft_export_preserves_agent_import_provenance() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("agent-source.wav");
    std::fs::write(&audio, b"audio").unwrap();
    let audio_path = audio.to_string_lossy().to_string();
    let segment = SpeechSegment {
        id: "agent-seg-1".into(),
        created_at: None,
        audio_path: audio_path.clone(),
        raw_transcript: "raw candidate".into(),
        normalized_transcript: None,
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1000,
        speaker_id: Some("spk1".into()),
        verified: false,
        confidence: Some(0.95),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        verdict: Some("jury_accept".into()),
        verdict_transcript: Some("reference candidate".into()),
        rationale: Some("multi-reference consensus committed agent text".into()),
        evidence_json: Some(
            serde_json::json!({
                "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
                "selectedTranscript": "reference candidate",
                "shouldCommit": true
            })
            .to_string(),
        ),
        agent_confidence: Some(0.92),
        escalated: false,
        human_decision: None,
        corrected_at: None,
        is_gold: false,
        alignment_quality: None,
        ..SpeechSegment::default()
    };
    db.insert_segment(&segment).unwrap();
    let evidence_json = serde_json::json!({
        "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
        "selectedModelId": "omniasr-wsl-7b",
        "selectedTranscript": "reference candidate",
        "selectedScore": 0.91,
        "confidence": 0.92,
        "margin": 0.18,
        "shouldCommit": true
    })
    .to_string();
    db.write_segment_verdict(
        &segment.id,
        "jury_accept",
        Some("reference candidate"),
        Some("multi-reference consensus committed agent text"),
        Some(evidence_json.as_str()),
        Some(0.92),
        false,
    )
    .unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "omniasr-wsl-7b".to_string(),
        transcript: "reference candidate".to_string(),
        confidence: Some(0.95),
    })
    .unwrap();
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: audio_path.clone(),
        model_id: "gemini-2.5-pro".to_string(),
        audio_content_hash: Some("bundle-source-audio-hash-v1".to_string()),
        audio_size_bytes: Some(654321),
        transcript_path: "source_transcripts\\agent-source__gemini_2_5_pro.txt".to_string(),
        transcript_text: "whole file reference transcript".to_string(),
        created_at: None,
    })
    .unwrap();
    let agent_report = crate::runs::record_agent_import_report_with_options(
        &db,
        "file",
        std::slice::from_ref(&audio_path),
        std::slice::from_ref(&segment.id),
        Some(&serde_json::json!({
            "referenceCommitted": 1,
            "humanInbox": 0,
        })),
        None,
        crate::runs::AgentImportReportOptions {
            agent_run_id: Some("run-agent-1".to_string()),
            agentic_readiness: Some(serde_json::json!({
                "status": "ready",
                "ready": true,
                "sourceReferenceModels": ["gemini-2.5-pro", "gemini-2.5-flash"],
                "availableHypothesisModels": ["omniasr-wsl-7b", "omniasr-ctc-300m"],
                "requiredHypothesisModels": 2,
                "checks": [{
                    "id": "hypothesis_coverage",
                    "label": "Multi-model hypothesis coverage",
                    "status": "ready",
                    "detail": "Two hypothesis models are ready"
                }]
            })),
            ..crate::runs::AgentImportReportOptions::default()
        },
    )
    .unwrap();
    crate::runs::record_agent_stage_event(
        &db,
        "run-agent-1",
        "file",
        "source_reference",
        "completed",
        "agent-source.wav",
        "Recorded Gemini whole-file source references",
        1,
        3,
    )
    .unwrap();
    crate::runs::record_agent_stage_event(
        &db,
        "run-agent-1",
        "file",
        "multi_model_hypotheses",
        "completed",
        "agent-source.wav",
        "Collected OmniASR WSL 7B and secondary hypotheses",
        2,
        3,
    )
    .unwrap();
    crate::runs::record_agent_stage_event(
        &db,
        "run-agent-1",
        "file",
        "jury_adjudication",
        "blocked",
        "agent-source.wav",
        "Dataset promotion still needs multi-model coverage",
        3,
        3,
    )
    .unwrap();
    crate::runs::record_agent_stage_event(
        &db,
        "unrelated-run",
        "file",
        "source_reference",
        "completed",
        "unrelated.wav",
        "This event belongs to a different run and must not be bundled",
        1,
        1,
    )
    .unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    let agent_provenance: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("agent_provenance.json")).unwrap()).unwrap();
    assert_eq!(agent_provenance["schemaVersion"].as_u64(), Some(2));
    assert_eq!(agent_provenance["agentImportReportCount"].as_u64(), Some(1));
    assert_eq!(agent_provenance["agentStageEventLimit"].as_u64(), Some(500));
    assert_eq!(agent_provenance["agentStageEventCount"].as_u64(), Some(3));
    assert_eq!(agent_provenance["agentImportReports"][0]["agentRunId"].as_str(), Some("run-agent-1"));
    assert_eq!(agent_provenance["agentImportReports"][0]["audioPaths"][0].as_str(), Some("agent-source.wav"));
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["agenticReadiness"]["status"].as_str(),
        Some("ready")
    );
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["agenticReadiness"]["availableHypothesisModels"][0]
            .as_str(),
        Some("omniasr-wsl-7b")
    );
    assert_eq!(agent_provenance["sourceReferenceCount"].as_u64(), Some(1));
    assert_eq!(agent_provenance["longFileDossierCount"].as_u64(), Some(1));
    assert_eq!(agent_provenance["agentPromotionReadiness"]["reportId"].as_str(), Some(agent_report.id.as_str()));
    assert_eq!(agent_provenance["agentPromotionReadiness"]["status"].as_str(), Some("blocked"));
    assert_eq!(agent_provenance["agentPromotionReadiness"]["agenticReadiness"]["status"].as_str(), Some("ready"));
    assert_eq!(agent_provenance["agentPromotionReadiness"]["stage"]["stage"].as_str(), Some("dataset_promotion"));
    assert_eq!(agent_provenance["agentPromotionReadiness"]["blockers"][0].as_str(), Some("agent-seg-1"));
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["hypothesisModelCounts"]["omniasr-wsl-7b"].as_u64(),
        Some(1)
    );
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["sourceReferences"][0]["modelId"].as_str(),
        Some("gemini-2.5-pro")
    );
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["sourceReferences"][0]["audioPath"].as_str(),
        Some("agent-source.wav")
    );
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["sourceReferences"][0]["transcriptPath"].as_str(),
        Some("agent-source__gemini_2_5_pro.txt")
    );
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["sourceReferences"][0]["audioContentHash"].as_str(),
        Some("bundle-source-audio-hash-v1")
    );
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["sourceReferences"][0]["audioSizeBytes"].as_u64(),
        Some(654321)
    );
    assert_eq!(agent_provenance["agentImportReports"][0]["juryReport"]["referenceCommitted"].as_u64(), Some(1));
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["longFileDossiers"][0]["audioPath"].as_str(),
        Some("agent-source.wav")
    );
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["longFileDossiers"][0]["chunkCount"].as_u64(),
        Some(1)
    );
    assert_eq!(
        agent_provenance["agentImportReports"][0]["summary"]["longFileDossiers"][0]["sourceReferenceCoverage"]
            ["audioPath"]
            .as_str(),
        Some("agent-source.wav")
    );
    let stage_events = agent_provenance["agentStageEvents"].as_array().unwrap();
    assert!(stage_events.iter().all(|event| event["runId"].as_str() == Some("run-agent-1")));
    assert_eq!(agent_provenance["agentStageEvents"][0]["runId"].as_str(), Some("run-agent-1"));
    assert_eq!(agent_provenance["agentStageEvents"][0]["stage"].as_str(), Some("source_reference"));
    assert_eq!(agent_provenance["agentStageEvents"][1]["stage"].as_str(), Some("multi_model_hypotheses"));
    assert_eq!(agent_provenance["agentStageEvents"][2]["stage"].as_str(), Some("jury_adjudication"));
    assert_eq!(agent_provenance["agentStageEvents"][2]["status"].as_str(), Some("blocked"));
    assert_eq!(agent_provenance["agentStageEvents"][2]["current"].as_u64(), Some(3));

    let grade_details: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("training_grade_details.json")).unwrap()).unwrap();
    assert_eq!(grade_details["schemaVersion"].as_u64(), Some(2));
    let detail = &grade_details["segments"][0];
    assert_eq!(detail["segmentId"].as_str(), Some("agent-seg-1"));
    assert_eq!(detail["audioPath"].as_str(), Some("agent-source.wav"));
    assert_eq!(detail["grade"].as_str(), Some("silver"));
    assert_eq!(detail["trainingReady"].as_bool(), Some(true));
    assert_eq!(detail["transcriptSource"].as_str(), Some("jury_verdict"));
    assert_eq!(detail["hypothesisModelCounts"]["omniasr-wsl-7b"].as_u64(), Some(1));
    assert_eq!(
        detail["evidence"]["sourceReference"]["referenceModelId"].as_str(),
        Some("multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash")
    );
    assert_eq!(detail["evidence"]["sourceReference"]["shouldCommit"].as_bool(), Some(true));

    let source_reference_manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("source_reference_manifest.json")).unwrap()).unwrap();
    assert_eq!(source_reference_manifest["sourceReferenceCount"].as_u64(), Some(1));
    assert_eq!(source_reference_manifest["schemaVersion"].as_u64(), Some(4));
    assert_eq!(source_reference_manifest["references"][0]["modelId"].as_str(), Some("gemini-2.5-pro"));
    assert_eq!(source_reference_manifest["references"][0]["audioPath"].as_str(), Some("agent-source.wav"));
    assert_eq!(
        source_reference_manifest["references"][0]["audioContentHash"].as_str(),
        Some("bundle-source-audio-hash-v1")
    );
    assert_eq!(source_reference_manifest["references"][0]["audioSizeBytes"].as_u64(), Some(654321));
    assert_eq!(source_reference_manifest["references"][0]["audioIdentityVerified"].as_bool(), Some(false));
    assert_eq!(
        source_reference_manifest["references"][0]["audioIdentityIssue"].as_str(),
        Some("stored_audio_identity_mismatch")
    );
    assert_eq!(
        source_reference_manifest["references"][0]["originalTranscriptPath"].as_str(),
        Some("agent-source__gemini_2_5_pro.txt")
    );
    let bundled_reference_path =
        source_reference_manifest["references"][0]["bundlePath"].as_str().expect("bundle path");
    assert!(bundled_reference_path.starts_with("source_transcripts/"));
    assert!(bundled_reference_path.ends_with(".whole_file_reference.txt"));
    assert_eq!(std::fs::read_to_string(out.join(bundled_reference_path)).unwrap(), "whole file reference transcript");

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["agentPromotionReadiness"]["reportId"].as_str(), Some(agent_report.id.as_str()));
    assert_eq!(manifest["agentPromotionReadiness"]["status"].as_str(), Some("blocked"));
    assert_eq!(manifest["agentPromotionReadiness"]["agenticReadiness"]["ready"].as_bool(), Some(true));
    assert_eq!(manifest["agentImportReportCount"].as_u64(), Some(1));
    assert_eq!(manifest["agentStageEventCount"].as_u64(), Some(3));
    assert_eq!(manifest["longFileDossierCount"].as_u64(), Some(1));
    assert_eq!(
        manifest["agentPromotionReadiness"]["summary"].as_str(),
        agent_provenance["agentPromotionReadiness"]["summary"].as_str()
    );
    let long_file_dossiers: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("long_file_dossiers.json")).unwrap()).unwrap();
    assert_eq!(long_file_dossiers["schemaVersion"].as_u64(), Some(2));
    assert_eq!(long_file_dossiers["longFileDossierCount"].as_u64(), Some(1));
    assert_eq!(long_file_dossiers["longFileDossiers"][0]["audioPath"].as_str(), Some("agent-source.wav"));
    assert_eq!(
        long_file_dossiers["longFileDossiers"][0]["sourceReferenceCoverage"]["audioPath"].as_str(),
        Some("agent-source.wav")
    );
    assert_eq!(
        long_file_dossiers["longFileDossiers"][0]["sourceReferences"][0]["modelId"].as_str(),
        Some("gemini-2.5-pro")
    );
    assert_eq!(
        long_file_dossiers["longFileDossiers"][0]["sourceReferences"][0]["audioPath"].as_str(),
        Some("agent-source.wav")
    );
    assert_eq!(
        long_file_dossiers["longFileDossiers"][0]["sourceReferences"][0]["transcriptPath"].as_str(),
        Some("agent-source__gemini_2_5_pro.txt")
    );
    assert_eq!(
        long_file_dossiers["longFileDossiers"][0]["sourceReferences"][0]["audioContentHash"].as_str(),
        Some("bundle-source-audio-hash-v1")
    );
    assert_eq!(long_file_dossiers["longFileDossiers"][0]["hypothesisModelCounts"]["omniasr-wsl-7b"].as_u64(), Some(1));
    let manifest_files = manifest["files"]
        .as_array()
        .expect("manifest files")
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();
    assert!(manifest_files.contains(&"source_reference_manifest.json"));
    assert!(manifest_files.contains(&"long_file_dossiers.json"));
    assert!(manifest_files.contains(&bundled_reference_path));
    let private_root = tmp.path().to_string_lossy().to_string();
    for (artifact, value) in [
        ("agent_provenance.json", &agent_provenance),
        ("training_grade_details.json", &grade_details),
        ("source_reference_manifest.json", &source_reference_manifest),
        ("long_file_dossiers.json", &long_file_dossiers),
    ] {
        assert_json_strings_do_not_contain(value, &private_root, artifact);
        assert_json_strings_do_not_contain(value, &audio_path, artifact);
    }
}

#[test]
fn draft_export_includes_self_learning_preference_artifacts() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("learning-source.wav");
    std::fs::write(&audio, b"audio").unwrap();
    let identity = crate::pipeline::source_audio_identity(&audio).unwrap();
    let audio_path = audio.to_string_lossy().to_string();
    let segment = SpeechSegment {
        id: "learning-seg-1".into(),
        created_at: None,
        audio_path: audio_path.clone(),
        raw_transcript: "raw candidate".into(),
        normalized_transcript: Some("normalized candidate".into()),
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1800,
        speaker_id: Some("spk1".into()),
        verified: false,
        confidence: Some(0.82),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        verdict: Some("jury_accept".into()),
        verdict_transcript: Some("agent wrong text".into()),
        rationale: Some("reference-aware agent selected a weak candidate".into()),
        evidence_json: Some(
            serde_json::json!({
                "referenceModelId": "gemini-2.5-pro",
                "selectedModelId": "omniasr-wsl-7b",
                "selectedTranscript": "agent wrong text",
                "shouldCommit": true
            })
            .to_string(),
        ),
        agent_confidence: Some(0.90),
        escalated: false,
        human_decision: None,
        corrected_at: None,
        is_gold: false,
        alignment_quality: None,
        ..SpeechSegment::default()
    };
    db.insert_segment(&segment).unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "omniasr-wsl-7b".to_string(),
        transcript: "agent wrong text".to_string(),
        confidence: Some(0.90),
    })
    .unwrap();
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: audio_path.clone(),
        model_id: "gemini-2.5-pro".to_string(),
        audio_content_hash: Some(identity.content_hash),
        audio_size_bytes: Some(identity.size_bytes),
        transcript_path: "source_transcripts\\learning-source__gemini_2_5_pro.txt".to_string(),
        transcript_text: "whole file reference for learning".to_string(),
        created_at: None,
    })
    .unwrap();
    db.connection()
        .execute(
            "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["learning-example-1", "learning-seg-1", "agent wrong text", "human corrected text"],
        )
        .unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    let result = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    assert!(result.files.contains(&"learning_manifest.json".to_string()));
    assert!(result.files.contains(&"learning_preferences.jsonl".to_string()));
    let learning_manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("learning_manifest.json")).unwrap()).unwrap();
    assert_eq!(learning_manifest["pairCount"].as_u64(), Some(1));
    assert_eq!(learning_manifest["preferencesPath"].as_str(), Some("learning_preferences.jsonl"));
    let jsonl = std::fs::read_to_string(out.join("learning_preferences.jsonl")).unwrap();
    let pair: serde_json::Value = serde_json::from_str(jsonl.lines().next().expect("jsonl row")).unwrap();
    assert_eq!(pair["chosen"].as_str(), Some("human corrected text"));
    assert_eq!(pair["rejected"].as_str(), Some("agent wrong text"));
    let prompt = pair["prompt"].as_str().unwrap();
    assert!(prompt.contains("omniasr-wsl-7b"));
    assert!(prompt.contains("gemini-2.5-pro"));
    assert!(prompt.contains("audio_path: learning-source.wav"));
    assert!(!prompt.contains(tmp.path().to_string_lossy().as_ref()));

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    let manifest_files = manifest["files"]
        .as_array()
        .expect("manifest files")
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();
    assert!(manifest_files.contains(&"learning_manifest.json"));
    assert!(manifest_files.contains(&"learning_preferences.jsonl"));
}

#[test]
fn re_export_into_reused_dir_removes_stale_learning_preferences_orphan() {
    // A first export with DPO pairs writes learning_preferences.jsonl; a later export into the SAME
    // directory with ZERO pairs must not leave that file behind as a stale orphan — it would be re-hashed
    // into SHA256SUMS by the whole-tree walk and re-ship withdrawn preference pairs vouched as current.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("learning-source.wav");
    std::fs::write(&audio, b"audio").unwrap();
    let identity = crate::pipeline::source_audio_identity(&audio).unwrap();
    let audio_path = audio.to_string_lossy().to_string();
    let segment = SpeechSegment {
        id: "learning-seg-1".into(),
        created_at: None,
        audio_path: audio_path.clone(),
        raw_transcript: "raw candidate".into(),
        normalized_transcript: Some("normalized candidate".into()),
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1800,
        speaker_id: Some("spk1".into()),
        verified: false,
        confidence: Some(0.82),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        verdict: Some("jury_accept".into()),
        verdict_transcript: Some("agent wrong text".into()),
        rationale: Some("reference-aware agent selected a weak candidate".into()),
        evidence_json: Some(
            serde_json::json!({
                "referenceModelId": "gemini-2.5-pro",
                "selectedModelId": "omniasr-wsl-7b",
                "selectedTranscript": "agent wrong text",
                "shouldCommit": true
            })
            .to_string(),
        ),
        agent_confidence: Some(0.90),
        escalated: false,
        human_decision: None,
        corrected_at: None,
        is_gold: false,
        alignment_quality: None,
        ..SpeechSegment::default()
    };
    db.insert_segment(&segment).unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "omniasr-wsl-7b".to_string(),
        transcript: "agent wrong text".to_string(),
        confidence: Some(0.90),
    })
    .unwrap();
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: audio_path.clone(),
        model_id: "gemini-2.5-pro".to_string(),
        audio_content_hash: Some(identity.content_hash),
        audio_size_bytes: Some(identity.size_bytes),
        transcript_path: "source_transcripts\\learning-source__gemini_2_5_pro.txt".to_string(),
        transcript_text: "whole file reference for learning".to_string(),
        created_at: None,
    })
    .unwrap();
    db.connection()
        .execute(
            "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["learning-example-1", "learning-seg-1", "agent wrong text", "human corrected text"],
        )
        .unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");

    // First export: the preference pair exists → learning_preferences.jsonl is written.
    let first = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();
    assert!(first.files.contains(&"learning_preferences.jsonl".to_string()));
    assert!(out.join("learning_preferences.jsonl").exists());

    // Withdraw the human edit so the next export has zero pairs.
    db.connection().execute("DELETE FROM agent_examples WHERE id = 'learning-example-1'", []).unwrap();

    // Second export into the SAME directory: the stale file must be gone, not re-shipped.
    let second = export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();
    assert!(!second.files.contains(&"learning_preferences.jsonl".to_string()));
    assert!(!out.join("learning_preferences.jsonl").exists(), "stale learning_preferences.jsonl orphan re-shipped");

    let learning_manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("learning_manifest.json")).unwrap()).unwrap();
    assert_eq!(learning_manifest["pairCount"].as_u64(), Some(0));
    assert!(learning_manifest["preferencesPath"].is_null());

    // SHA256SUMS must not vouch for the withdrawn file.
    let sums = std::fs::read_to_string(out.join("SHA256SUMS")).unwrap();
    assert!(!sums.contains("learning_preferences.jsonl"), "SHA256SUMS vouches a stale orphan");
}

#[test]
fn draft_export_records_model_metadata_load_errors() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("sample.wav");
    std::fs::write(&audio, b"audio").unwrap();
    db.insert_segment(&SpeechSegment {
        id: "seg-1".into(),
        created_at: None,
        audio_path: audio.to_string_lossy().to_string(),
        raw_transcript: "test".into(),
        normalized_transcript: None,
        annotated_transcript: Some("test".into()),
        alignment_json: None,
        duration_ms: 1000,
        speaker_id: Some("spk1".into()),
        verified: true,
        confidence: Some(0.95),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        ..SpeechSegment::default()
    })
    .unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    std::fs::create_dir_all(&models.models_dir).unwrap();
    std::fs::write(models.models_dir.join("models_meta.json"), "{not valid json").unwrap();
    let out = tmp.path().join("bundle");

    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    let model_manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("model_manifest.json")).unwrap()).unwrap();
    assert_eq!(model_manifest["installed"].as_array().unwrap().len(), 0);
    assert!(
        model_manifest["installedMetadataLoadError"].as_str().unwrap().contains("Parse meta"),
        "model metadata load error should be explicit in model_manifest.json"
    );
}

#[test]
fn draft_export_replaces_bundle_metadata_atomically() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let audio = tmp.path().join("sample.wav");
    std::fs::write(&audio, b"audio").unwrap();
    db.insert_segment(&SpeechSegment {
        id: "seg-1".into(),
        created_at: None,
        audio_path: audio.to_string_lossy().to_string(),
        raw_transcript: "دەنگ".into(),
        normalized_transcript: None,
        annotated_transcript: Some("دەنگ".into()),
        alignment_json: None,
        duration_ms: 1000,
        speaker_id: Some("spk1".into()),
        verified: true,
        confidence: Some(0.95),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".into()),
        signal_anomaly_score: None,
        ..SpeechSegment::default()
    })
    .unwrap();
    let out = tmp.path().join("bundle");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("manifest.json"), "__stale_manifest__").unwrap();
    std::fs::write(out.join("dataset_card.md"), "__stale_card__").unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();

    let manifest = std::fs::read_to_string(out.join("manifest.json")).unwrap();
    let card = std::fs::read_to_string(out.join("dataset_card.md")).unwrap();
    assert!(!manifest.contains("__stale_manifest__"));
    assert!(!card.contains("__stale_card__"));
    assert!(!out.join("manifest.json.tmp").exists());
    assert!(!out.join("dataset_card.md.tmp").exists());
}

#[test]
fn provenance_counts_tally_and_unanimity() {
    // P0.4 read side: the pure helper the manifest uses. Distinct counts across all three states catch a
    // mis-tally; all_applied() must be TRUE only when every segment recorded the model as having run.
    let c = ProvenanceCounts::tally([Some(true), Some(false), None, Some(true)].into_iter());
    assert_eq!((c.applied, c.not_applied, c.not_recorded), (2, 1, 1));
    assert!(!c.all_applied(), "a mixed set is NOT unanimously applied");
    assert!(ProvenanceCounts::tally([Some(true), Some(true)].into_iter()).all_applied(), "all true -> unanimous");
    assert!(
        !ProvenanceCounts::tally([Some(true), None].into_iter()).all_applied(),
        "an unrecorded row breaks unanimity"
    );
    assert!(!ProvenanceCounts::tally(std::iter::empty()).all_applied(), "an empty export is not 'all applied'");
    assert!(!ProvenanceCounts::tally([Some(false)].into_iter()).all_applied(), "not-applied is not unanimous");
}

#[test]
fn manifest_reads_stored_per_segment_provenance_not_export_day_model_state() {
    // P0.4 read side (H3): the manifest must report the STORED denoised/diarized of the EXPORTED rows,
    // never a single flag recomputed from export-day model loadability. Mixed denoising (applied/not/
    // unrecorded) + unanimous diarization proves both the distribution and the unanimity boolean.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let tmp = TempDir::new().unwrap();
    let mk = |id: &str, denoised: Option<bool>, diarized: Option<bool>, vad: Option<&str>| {
        let audio = tmp.path().join(format!("{id}.wav"));
        std::fs::write(&audio, b"audio").unwrap();
        SpeechSegment {
            id: id.into(),
            audio_path: audio.to_string_lossy().to_string(),
            raw_transcript: "ڕستەیەکی کوردی".into(),
            duration_ms: 1000,
            denoised,
            diarized,
            vad_backend: vad.map(str::to_string),
            ..SpeechSegment::default()
        }
    };
    // denoised: applied=1, not_applied=1, not_recorded=1 (mixed) -> denoising=false.
    // diarized:  applied=3, not_applied=0, not_recorded=0 (unanimous) -> diarization=true.
    // vad_backend: silero=2, energy=1 (byBackend), notRecorded=0.
    db.insert_segments_batch(&[
        mk("s-a", Some(true), Some(true), Some("silero")),
        mk("s-b", Some(false), Some(true), Some("silero")),
        mk("s-c", None, Some(true), Some("energy")),
    ])
    .unwrap();
    // v43 reviewed_by is EARNED, not set: the import path never writes it (insert_segments_batch carries
    // no human-decision column — correctly, since a freshly imported clip has no reviewer). Attribution
    // exists only once someone actually decides a clip, so drive it through the real decision path.
    // -> Sara=2, notAttributed=1 (s-c is undecided — the same bucket a desktop or pre-v43 decision lands in).
    db.record_human_decision_by("s-a", "accept", None, None, Some("Sara")).unwrap();
    db.record_human_decision_by("s-b", "accept", None, None, Some("Sara")).unwrap();

    let models = ModelManager::new(tmp.path().join("models"));
    let out = tmp.path().join("bundle");
    // Non-production so the training-ready gate does not block; the manifest is written regardless.
    export_dataset_bundle(&db, &models, &out, &AppSettings::default(), false, usize::MAX).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();

    let prov = &manifest["processingProvenance"];
    assert_eq!(prov["total"].as_u64(), Some(3));
    assert_eq!(prov["denoised"]["applied"].as_u64(), Some(1));
    assert_eq!(prov["denoised"]["notApplied"].as_u64(), Some(1));
    assert_eq!(prov["denoised"]["notRecorded"].as_u64(), Some(1));
    assert_eq!(prov["diarized"]["applied"].as_u64(), Some(3));
    assert_eq!(prov["diarized"]["notApplied"].as_u64(), Some(0));
    assert_eq!(prov["diarized"]["notRecorded"].as_u64(), Some(0));
    // vad_backend distribution: the stored per-segment backend, honestly counted (silero=2, energy=1).
    assert_eq!(prov["vadBackend"]["byBackend"]["silero"].as_u64(), Some(2));
    assert_eq!(prov["vadBackend"]["byBackend"]["energy"].as_u64(), Some(1));
    assert_eq!(prov["vadBackend"]["notRecorded"].as_u64(), Some(0));
    // v43 reviewer attribution reaches the exported manifest — a corpus labelled by named people must
    // say WHO, and the unattributed rows must be counted separately, never folded under a reviewer.
    assert_eq!(prov["reviewedBy"]["byReviewer"]["Sara"].as_u64(), Some(2));
    assert_eq!(prov["reviewedBy"]["notAttributed"].as_u64(), Some(1));
    // The single runConfig boolean reflects STORED unanimity, not export-day loadability (no models on
    // disk here — the old code would have computed denoising/diarization from failed load probes).
    assert_eq!(manifest["runConfig"]["denoising"].as_bool(), Some(false), "mixed denoising -> false");
    assert_eq!(
        manifest["runConfig"]["diarization"].as_bool(),
        Some(true),
        "unanimous diarization -> true (from stored truth)"
    );
}
