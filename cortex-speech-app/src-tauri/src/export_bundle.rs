use crate::atomic_file::{remove_file_on_error, replace_file};
use crate::db::{Database, SourceTranscriptRecord};
use crate::error::{AppError, AppResult};
use crate::models::ModelManager;
use crate::settings::{AppSettings, ExportFormat};
use crate::{export, quality, validation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockingValidationIssues {
    pub blocked: bool,
    pub error_count: usize,
    pub warning_count: usize,
    pub warning_threshold: usize,
    pub errors: Vec<validation::ValidationIssue>,
    pub warnings: Vec<validation::ValidationIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleExportResult {
    pub output_dir: String,
    pub production: bool,
    pub manifest_path: String,
    pub files: Vec<String>,
    pub validation: BlockingValidationIssues,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentPromotionReadiness {
    report_id: Option<String>,
    report_created_at: Option<String>,
    status: String,
    summary: String,
    blocker_count: usize,
    blockers: Vec<String>,
    segment_ids: Vec<String>,
    stage: Option<crate::runs::AgentOrchestrationStage>,
    agentic_readiness: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrainingGradeDetails {
    generated_at: String,
    schema_version: u32,
    segment_count: usize,
    training_ready_segment_count: usize,
    reason_counts: BTreeMap<String, usize>,
    segments: Vec<TrainingGradeSegmentDetail>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrainingGradeSegmentDetail {
    segment_id: String,
    audio_path: String,
    speaker_id: Option<String>,
    split: Option<String>,
    duration_ms: i64,
    transcript: String,
    transcript_source: String,
    grade: String,
    training_ready: bool,
    reasons: Vec<String>,
    verdict: Option<String>,
    agent_confidence: Option<f64>,
    escalated: bool,
    hypothesis_count: usize,
    hypothesis_models: Vec<String>,
    hypothesis_model_counts: BTreeMap<String, usize>,
    hypothesis_coverage: quality::HypothesisCoverageReport,
    evidence: SegmentEvidenceDetail,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentEvidenceDetail {
    has_evidence_json: bool,
    source_reference: Option<SourceReferenceEvidenceDetail>,
    t2_listener: bool,
    parse_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceReferenceEvidenceDetail {
    reference_model_id: Option<String>,
    selected_model_id: Option<String>,
    selected_score: Option<f64>,
    confidence: Option<f64>,
    margin: Option<f64>,
    should_commit: Option<bool>,
    reference_agreement_count: usize,
    reference_agreement: Vec<SourceReferenceAgreementDetail>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceReferenceAgreementDetail {
    reference_model_id: Option<String>,
    selected_model_id: Option<String>,
    selected_score: Option<f64>,
    confidence: Option<f64>,
    margin: Option<f64>,
    should_commit: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceReferenceBundleManifest {
    generated_at: String,
    schema_version: u32,
    source_reference_count: usize,
    references: Vec<BundledSourceReference>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundledSourceReference {
    audio_path: String,
    model_id: String,
    audio_content_hash: Option<String>,
    audio_size_bytes: Option<i64>,
    audio_identity_verified: bool,
    audio_identity_issue: Option<String>,
    original_transcript_path: String,
    bundle_path: String,
    created_at: Option<String>,
    transcript_chars: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LearningBundleManifest {
    generated_at: String,
    schema_version: u32,
    pair_count: usize,
    format: String,
    preferences_path: Option<String>,
    empty_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct SourceReferenceBundleRecord {
    record: SourceTranscriptRecord,
    audio_identity_verified: bool,
    audio_identity_issue: Option<String>,
}

pub fn blocking_issues(
    db: &Database,
    settings: &AppSettings,
    warning_threshold: usize,
) -> AppResult<BlockingValidationIssues> {
    let report = validation::validate_dataset_with_settings(db, settings)?;
    let error_count = report.errors.len();
    let warning_count = report.warnings.len();
    Ok(BlockingValidationIssues {
        blocked: error_count > 0 || warning_count > warning_threshold,
        error_count,
        warning_count,
        warning_threshold,
        errors: report.errors,
        warnings: report.warnings,
    })
}

fn list_stage_events_for_reports(
    db: &Database,
    reports: &[crate::runs::AgentImportReport],
    limit: usize,
) -> AppResult<Vec<crate::runs::AgentStageEvent>> {
    let mut run_ids = Vec::new();
    for report in reports {
        let Some(run_id) = report.agent_run_id.as_deref().map(str::trim).filter(|run_id| !run_id.is_empty()) else {
            continue;
        };
        if !run_ids.iter().any(|existing: &String| existing == run_id) {
            run_ids.push(run_id.to_string());
        }
    }

    if run_ids.is_empty() {
        return crate::runs::list_agent_stage_events(db, None, Some(limit));
    }

    let mut events = Vec::new();
    for run_id in run_ids {
        let remaining = limit.saturating_sub(events.len());
        if remaining == 0 {
            break;
        }
        events.extend(crate::runs::list_agent_stage_events(db, Some(run_id.as_str()), Some(remaining))?);
    }
    events.sort_by_key(|event| event.id);
    events.truncate(limit);
    Ok(events)
}

pub fn export_dataset_bundle(
    db: &Database,
    model_manager: &ModelManager,
    output_dir: &Path,
    settings: &AppSettings,
    production: bool,
    warning_threshold: usize,
) -> AppResult<BundleExportResult> {
    let validation_report = validation::validate_dataset_with_settings(db, settings)?;
    let validation_gate = BlockingValidationIssues {
        blocked: !validation_report.errors.is_empty() || validation_report.warnings.len() > warning_threshold,
        error_count: validation_report.errors.len(),
        warning_count: validation_report.warnings.len(),
        warning_threshold,
        errors: validation_report.errors.clone(),
        warnings: validation_report.warnings.clone(),
    };

    if production && validation_gate.blocked {
        return Err(AppError::Validation(format!(
            "Production export blocked by {} validation errors and {} warnings (threshold {})",
            validation_gate.error_count, validation_gate.warning_count, warning_threshold
        )));
    }

    // Round-22 #1 (bundle completion): drop held-out gold eval segments at the SOURCE so EVERY bundle
    // artifact derived from `segments` is holdout-free — not just the tabular data files (which route
    // through export::export_dataset and filter on their own). The training-grade summary/details and the
    // source-reference text artifacts below iterate this same list, so without filtering here the holdout
    // clip's HUMAN REFERENCE transcript (the WER/CER answer key) still leaks verbatim via
    // source_transcripts/*.txt + source_reference_manifest.json + training_grade_details.json, and the
    // manifest counts still include it — re-contaminating the exact eval set the promotion gate measures
    // against.
    let segments = export::exclude_holdout_segments(db, db.get_segments(None)?)?;
    // Drop human-REJECTED clips ("mark bad") from the whole bundle the same way the plain export and the
    // training path do — a discarded draft must never be published or counted as verified/training-ready.
    let segments: Vec<crate::db::SpeechSegment> =
        segments.into_iter().filter(|s| !quality::is_human_rejected(s)).collect();
    // Also drop not-yet-transcribed PLACEHOLDER rows ("[Pending WSL 7B ASR]" / "[ASR unavailable…]"),
    // matching export::export_dataset's third filter (export.rs) that writes the tabular data files. Without
    // this, the manifest/card headline counts (segmentCount, totalDurationMs, trainingGradeSummary) and the
    // composition table were derived from a list that INCLUDES placeholders while the shipped
    // dataset.{json,jsonl,csv,parquet} EXCLUDE them — an inflated count that disagrees with the bundle's own
    // data files (and with dataset.json's embedded total_segments), a dishonest number the honesty law forbids.
    let segments: Vec<crate::db::SpeechSegment> =
        segments.into_iter().filter(|s| !quality::is_effective_placeholder(s)).collect();
    let training_grade_summary = quality::training_grade_summary(&segments);
    let training_ready_machine_segment_ids = training_ready_machine_segment_ids(&segments);
    let source_reference_records = collect_source_reference_bundle_records(db, &segments)?;
    let agent_import_report_limit = 200usize;
    let agent_stage_event_limit = 500usize;
    let agent_import_reports = crate::runs::list_agent_import_reports(db, Some(agent_import_report_limit))?;
    let agent_promotion_readiness = build_agent_promotion_readiness(&agent_import_reports);
    if production && training_grade_summary.training_ready_segments == 0 {
        return Err(AppError::Validation(
            "Production export blocked: no training-ready segments. Review or correct segments until at least one row grades gold or silver.".into(),
        ));
    }
    if production {
        let machine_coverage_blockers = training_ready_machine_segments_missing_hypothesis_coverage(db, &segments)?;
        if !machine_coverage_blockers.is_empty() {
            return Err(AppError::Validation(format!(
                "Production export blocked: training-ready machine segments are missing multi-model hypothesis coverage: {}",
                segment_id_preview(&machine_coverage_blockers)
            )));
        }
        if let Some(error) = production_agentic_machine_readiness_blocker(
            &agent_promotion_readiness,
            &training_ready_machine_segment_ids,
        ) {
            return Err(AppError::Validation(error));
        }
        let source_reference_identity_blockers = source_reference_export_identity_blockers(&source_reference_records);
        if !source_reference_identity_blockers.is_empty() {
            return Err(AppError::Validation(format!(
                "Production export blocked: source-reference transcripts have missing or stale audio identity: {}",
                source_reference_identity_blockers.join("; ")
            )));
        }
    }

    std::fs::create_dir_all(output_dir)?;

    let quality_report = quality::compute_quality_with_settings(db, settings)?;
    let model_status = model_manager.status();
    let (model_meta, model_meta_load_error) = load_model_metadata_for_manifest(model_manager);

    let data_files = [
        ("dataset.json", ExportFormat::Json),
        ("dataset.jsonl", ExportFormat::Jsonl),
        ("dataset.csv", ExportFormat::Csv),
        ("dataset.parquet", ExportFormat::Parquet),
    ];

    let mut files = Vec::new();
    for (filename, format) in data_files {
        let path = output_dir.join(filename);
        export::export_dataset(db, &path, &format)?;
        files.push(filename.to_string());
    }

    write_json(&output_dir.join("validation_report.json"), &validation_report)?;
    files.push("validation_report.json".into());
    write_json(&output_dir.join("quality_report.json"), &quality_report)?;
    files.push("quality_report.json".into());
    write_json(&output_dir.join("training_grade_summary.json"), &training_grade_summary)?;
    files.push("training_grade_summary.json".into());
    let training_grade_details = build_training_grade_details(db, &segments)?;
    write_json(&output_dir.join("training_grade_details.json"), &training_grade_details)?;
    files.push("training_grade_details.json".into());
    let (source_reference_manifest, source_reference_files) =
        write_source_reference_artifacts(output_dir, source_reference_records)?;
    write_json(&output_dir.join("source_reference_manifest.json"), &source_reference_manifest)?;
    files.push("source_reference_manifest.json".into());
    files.extend(source_reference_files);
    let (learning_manifest, learning_files) = write_learning_artifacts(db, output_dir)?;
    write_json(&output_dir.join("learning_manifest.json"), &learning_manifest)?;
    files.push("learning_manifest.json".into());
    files.extend(learning_files);
    let agent_stage_events = list_stage_events_for_reports(db, &agent_import_reports, agent_stage_event_limit)?;
    let agent_import_report_count = agent_import_reports.len();
    let agent_stage_event_count = agent_stage_events.len();
    let source_reference_count =
        agent_import_reports.iter().map(|report| report.summary.source_references.len()).sum::<usize>();
    let long_file_dossiers = agent_import_reports
        .iter()
        .flat_map(|report| report.summary.long_file_dossiers.iter().cloned())
        .collect::<Vec<_>>();
    let long_file_dossier_count = long_file_dossiers.len();
    let long_file_dossiers_value = sanitized_bundle_value(&long_file_dossiers)?;
    let agent_import_reports_value = sanitized_bundle_value(&agent_import_reports)?;
    let agent_stage_events_value = sanitized_bundle_value(&agent_stage_events)?;
    write_json(
        &output_dir.join("long_file_dossiers.json"),
        &serde_json::json!({
            "generatedAt": chrono::Utc::now().to_rfc3339(),
            "schemaVersion": 2,
            "longFileDossiers": long_file_dossiers_value,
            "longFileDossierCount": long_file_dossier_count,
        }),
    )?;
    files.push("long_file_dossiers.json".into());
    write_json(
        &output_dir.join("agent_provenance.json"),
        &serde_json::json!({
            "generatedAt": chrono::Utc::now().to_rfc3339(),
            "schemaVersion": 2,
            "agentImportReportLimit": agent_import_report_limit,
            "agentImportReports": agent_import_reports_value,
            "agentImportReportCount": agent_import_report_count,
            "agentStageEventLimit": agent_stage_event_limit,
            "agentStageEvents": agent_stage_events_value,
            "agentStageEventCount": agent_stage_event_count,
            "sourceReferenceCount": source_reference_count,
            "longFileDossierCount": long_file_dossier_count,
            "agentPromotionReadiness": &agent_promotion_readiness,
        }),
    )?;
    files.push("agent_provenance.json".into());
    write_json(
        &output_dir.join("model_manifest.json"),
        &serde_json::json!({
            "generatedAt": chrono::Utc::now().to_rfc3339(),
            "models": model_status,
            "installed": model_meta,
            "installedMetadataLoadError": model_meta_load_error,
        }),
    )?;
    files.push("model_manifest.json".into());

    let total_duration_ms: i64 = segments.iter().map(|s| s.duration_ms).sum();
    let verified_segments = segments.iter().filter(|s| s.verified).count();
    let training_ready_segments = training_grade_summary.training_ready_segments;
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "app": "cortex-speech-app",
        "appVersion": env!("CARGO_PKG_VERSION"),
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "production": production,
        "language": settings.language,
        "segmentCount": segments.len(),
        "verifiedSegments": verified_segments,
        "trainingReadySegments": training_ready_segments,
        "trainingGradeSummary": training_grade_summary.clone(),
        "agentPromotionReadiness": &agent_promotion_readiness,
        "agentImportReportCount": agent_import_report_count,
        "agentStageEventCount": agent_stage_event_count,
        "longFileDossierCount": long_file_dossier_count,
        "totalDurationMs": total_duration_ms,
        "runConfig": crate::runs::config_from_settings(settings, model_manager.denoiser_present()),
        "validation": {
            "blocked": validation_gate.blocked,
            "errors": validation_gate.error_count,
            "warnings": validation_gate.warning_count,
            "warningThreshold": warning_threshold
        },
        "files": files,
    });
    write_json(&output_dir.join("manifest.json"), &manifest)?;

    let card = format!(
        "# Cortex Kurdish Speech Dataset\n\n\
        Generated: {}\n\n\
        Language: {}\n\n\
        Segments: {}\n\n\
        Verified segments: {}\n\n\
        Training-ready segments: {}\n\n\
        Total duration (ms): {}\n\n\
        Validation errors: {}\n\n\
        Validation warnings: {}\n\n\
        Production export: {}\n\n\
        This bundle was generated by Cortex and includes manifest, validation, quality, training-grade detail, source-reference transcripts, self-learning preferences when available, agent provenance with persisted stage timeline, model provenance, and tabular dataset files.\n{}",
        manifest["exportedAt"].as_str().unwrap_or("unknown"),
        settings.language,
        segments.len(),
        verified_segments,
        training_ready_segments,
        total_duration_ms,
        validation_gate.error_count,
        validation_gate.warning_count,
        production,
        // Per-speaker composition + dominant-speaker flag in the HUMAN-readable card too — it lived
        // only in dataset.json, so the stated skew warning never reached a card reader (true-10
        // audit 2026-07-09).
        export::composition_markdown(&export::compute_composition(&segments))
    );
    write_text(&output_dir.join("dataset_card.md"), &card)?;

    // Integrity over every bundle file (clips + manifests + card): manifest consistency alone leaves
    // corrupted bytes undetectable (true-10 audit 2026-07-09). Written after everything else so the
    // sums cover the full bundle; excludes itself by construction.
    export::write_sha256sums(output_dir)?;

    let mut final_files = vec!["manifest.json".to_string(), "dataset_card.md".to_string(), "SHA256SUMS".to_string()];
    final_files.extend(files);

    Ok(BundleExportResult {
        output_dir: output_dir.to_string_lossy().to_string(),
        production,
        manifest_path: output_dir.join("manifest.json").to_string_lossy().to_string(),
        files: final_files,
        validation: validation_gate,
    })
}

fn build_agent_promotion_readiness(reports: &[crate::runs::AgentImportReport]) -> AgentPromotionReadiness {
    let Some(report) = reports.first() else {
        return AgentPromotionReadiness {
            report_id: None,
            report_created_at: None,
            status: "missing_agent_report".to_string(),
            summary: "No agent import report is available for promotion readiness.".to_string(),
            blocker_count: 0,
            blockers: Vec::new(),
            segment_ids: Vec::new(),
            stage: None,
            agentic_readiness: None,
        };
    };

    let Some(stage) = report.summary.orchestration_stages.iter().find(|stage| stage.stage == "dataset_promotion")
    else {
        return AgentPromotionReadiness {
            report_id: Some(report.id.clone()),
            report_created_at: Some(report.created_at.clone()),
            status: "missing_dataset_promotion_stage".to_string(),
            summary: "The latest agent import report has no dataset promotion stage.".to_string(),
            blocker_count: 0,
            blockers: Vec::new(),
            segment_ids: report.segment_ids.clone(),
            stage: None,
            agentic_readiness: report.summary.agentic_readiness.clone(),
        };
    };

    AgentPromotionReadiness {
        report_id: Some(report.id.clone()),
        report_created_at: Some(report.created_at.clone()),
        status: stage.status.clone(),
        summary: stage.summary.clone(),
        blocker_count: stage.blocker_count,
        blockers: stage.blockers.clone(),
        segment_ids: report.segment_ids.clone(),
        stage: Some(stage.clone()),
        agentic_readiness: report.summary.agentic_readiness.clone(),
    }
}

fn write_learning_artifacts(db: &Database, output_dir: &Path) -> AppResult<(LearningBundleManifest, Vec<String>)> {
    let dpo_export = crate::jury::learning::build_dpo_dataset(db)?;
    let mut files = Vec::new();
    let preferences_path = if dpo_export.pair_count > 0 {
        let path = "learning_preferences.jsonl".to_string();
        write_text(&output_dir.join(&path), &dpo_export.jsonl)?;
        files.push(path.clone());
        Some(path)
    } else {
        None
    };

    let empty_reason = if dpo_export.pair_count == 0 {
        Some("No non-gold human edit preference pairs are available yet.".to_string())
    } else {
        None
    };

    Ok((
        LearningBundleManifest {
            generated_at: chrono::Utc::now().to_rfc3339(),
            schema_version: 1,
            pair_count: dpo_export.pair_count,
            format: "jsonl:dpo-preference-pairs".to_string(),
            preferences_path,
            empty_reason,
        },
        files,
    ))
}

fn collect_source_reference_bundle_records(
    db: &Database,
    segments: &[crate::db::SpeechSegment],
) -> AppResult<Vec<SourceReferenceBundleRecord>> {
    let mut records_by_key = BTreeMap::<(String, String), SourceTranscriptRecord>::new();
    for segment in segments {
        for reference in db.get_source_transcripts_for_audio(&segment.audio_path)? {
            records_by_key.entry((reference.audio_path.clone(), reference.model_id.clone())).or_insert(reference);
        }
    }

    Ok(records_by_key.into_values().map(source_reference_bundle_record).collect())
}

fn source_reference_bundle_record(record: SourceTranscriptRecord) -> SourceReferenceBundleRecord {
    let audio_identity_issue = source_reference_audio_identity_issue(&record);
    let audio_identity_verified = audio_identity_issue.is_none();
    SourceReferenceBundleRecord { record, audio_identity_verified, audio_identity_issue }
}

fn source_reference_audio_identity_issue(record: &SourceTranscriptRecord) -> Option<String> {
    let Some(stored_hash) = record.audio_content_hash.as_deref().map(str::trim).filter(|value| !value.is_empty())
    else {
        return Some("missing_stored_audio_identity".to_string());
    };
    let Some(stored_size) = record.audio_size_bytes else {
        return Some("missing_stored_audio_identity".to_string());
    };

    let current_identity = match crate::pipeline::source_audio_identity(Path::new(&record.audio_path)) {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(
                "Cannot verify bundled source-reference audio identity for {} with {}: {}",
                record.audio_path,
                record.model_id,
                error
            );
            return Some("current_audio_identity_unavailable".to_string());
        }
    };

    if stored_hash == current_identity.content_hash && stored_size == current_identity.size_bytes {
        None
    } else {
        Some("stored_audio_identity_mismatch".to_string())
    }
}

fn source_reference_export_identity_blockers(records: &[SourceReferenceBundleRecord]) -> Vec<String> {
    records
        .iter()
        .filter(|record| !record.audio_identity_verified)
        .map(|record| {
            format!(
                "{} [{}]: {}",
                record.record.audio_path,
                record.record.model_id,
                record.audio_identity_issue.as_deref().unwrap_or("unverified_audio_identity")
            )
        })
        .collect()
}

fn write_source_reference_artifacts(
    output_dir: &Path,
    records: Vec<SourceReferenceBundleRecord>,
) -> AppResult<(SourceReferenceBundleManifest, Vec<String>)> {
    let source_dir = output_dir.join("source_transcripts");
    let mut bundled_references = Vec::with_capacity(records.len());
    let mut files = Vec::with_capacity(records.len());
    if !records.is_empty() {
        std::fs::create_dir_all(&source_dir)?;
    }

    for bundled_record in records {
        let record = bundled_record.record;
        let filename = source_reference_bundle_filename(&record);
        let bundle_path = Path::new("source_transcripts").join(filename);
        let bundle_path_string = bundle_path.to_string_lossy().replace('\\', "/");
        write_text(&output_dir.join(&bundle_path), record.transcript_text.trim())?;
        files.push(bundle_path_string.clone());
        bundled_references.push(BundledSourceReference {
            audio_path: bundle_path_label(&record.audio_path),
            model_id: record.model_id,
            audio_content_hash: record.audio_content_hash,
            audio_size_bytes: record.audio_size_bytes,
            audio_identity_verified: bundled_record.audio_identity_verified,
            audio_identity_issue: bundled_record.audio_identity_issue,
            original_transcript_path: bundle_path_label(&record.transcript_path),
            bundle_path: bundle_path_string,
            created_at: record.created_at,
            transcript_chars: record.transcript_text.trim().chars().count(),
        });
    }

    Ok((
        SourceReferenceBundleManifest {
            generated_at: chrono::Utc::now().to_rfc3339(),
            schema_version: 4,
            source_reference_count: bundled_references.len(),
            references: bundled_references,
        },
        files,
    ))
}

fn source_reference_bundle_filename(record: &SourceTranscriptRecord) -> String {
    let stem = Path::new(&record.audio_path).file_stem().and_then(|name| name.to_str()).unwrap_or("audio");
    let mut hasher = blake3::Hasher::new();
    hasher.update(record.audio_path.as_bytes());
    hasher.update(b"\0");
    hasher.update(record.model_id.as_bytes());
    let hash = hasher.finalize().to_hex().to_string();
    format!(
        "{}.{}.{}.whole_file_reference.txt",
        sanitize_bundle_filename(stem),
        sanitize_bundle_filename(&record.model_id),
        &hash[..12]
    )
}

fn build_training_grade_details(
    db: &Database,
    segments: &[crate::db::SpeechSegment],
) -> AppResult<TrainingGradeDetails> {
    let mut details = Vec::with_capacity(segments.len());
    let mut reason_counts = BTreeMap::new();
    let mut training_ready_segment_count = 0usize;

    for segment in segments {
        let grade = quality::training_grade_for_segment(segment);
        if grade.training_ready {
            training_ready_segment_count += 1;
        }
        for reason in &grade.reasons {
            *reason_counts.entry(reason.clone()).or_insert(0) += 1;
        }

        let hypotheses = db.get_hypotheses_for_segment(&segment.id)?;
        let mut hypothesis_model_counts = BTreeMap::new();
        for hypothesis in &hypotheses {
            *hypothesis_model_counts.entry(hypothesis.model_id.clone()).or_insert(0) += 1;
        }
        let hypothesis_models = hypothesis_model_counts.keys().cloned().collect::<Vec<_>>();
        let hypothesis_coverage = quality::hypothesis_coverage_for_model_outputs(&hypotheses);

        details.push(TrainingGradeSegmentDetail {
            segment_id: segment.id.clone(),
            audio_path: bundle_path_label(&segment.audio_path),
            speaker_id: segment.speaker_id.clone(),
            split: segment.split.clone(),
            duration_ms: segment.duration_ms,
            transcript: grade.transcript,
            transcript_source: grade.transcript_source,
            grade: grade.grade,
            training_ready: grade.training_ready,
            reasons: grade.reasons,
            verdict: segment.verdict.clone(),
            agent_confidence: segment.agent_confidence,
            escalated: segment.escalated,
            hypothesis_count: hypotheses.len(),
            hypothesis_models,
            hypothesis_model_counts,
            hypothesis_coverage,
            evidence: segment_evidence_detail(segment.evidence_json.as_deref()),
        });
    }

    Ok(TrainingGradeDetails {
        generated_at: chrono::Utc::now().to_rfc3339(),
        schema_version: 2,
        segment_count: segments.len(),
        training_ready_segment_count,
        reason_counts,
        segments: details,
    })
}

fn training_ready_machine_segments_missing_hypothesis_coverage(
    db: &Database,
    segments: &[crate::db::SpeechSegment],
) -> AppResult<Vec<String>> {
    let mut blocked = Vec::new();
    for segment in segments {
        let grade = quality::training_grade_for_segment(segment);
        if !grade.training_ready || grade.grade != quality::TRAINING_GRADE_SILVER {
            continue;
        }

        let hypotheses = db.get_hypotheses_for_segment(&segment.id)?;
        if !quality::hypothesis_coverage_for_model_outputs(&hypotheses).passes_minimum {
            blocked.push(segment.id.clone());
        }
    }
    blocked.sort();
    Ok(blocked)
}

fn training_ready_machine_segment_ids(segments: &[crate::db::SpeechSegment]) -> Vec<String> {
    let mut ids = segments
        .iter()
        .filter_map(|segment| {
            let grade = quality::training_grade_for_segment(segment);
            if grade.training_ready && grade.grade == quality::TRAINING_GRADE_SILVER {
                Some(segment.id.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn agentic_readiness_status(readiness: Option<&serde_json::Value>) -> Option<&str> {
    readiness.and_then(|value| value.get("status")).and_then(serde_json::Value::as_str)
}

fn agentic_readiness_ready(readiness: Option<&serde_json::Value>) -> bool {
    readiness.and_then(|value| value.get("ready")).and_then(serde_json::Value::as_bool) == Some(true)
        && agentic_readiness_status(readiness) == Some("ready")
}

fn production_agentic_machine_readiness_blocker(
    readiness: &AgentPromotionReadiness,
    training_ready_machine_segment_ids: &[String],
) -> Option<String> {
    if training_ready_machine_segment_ids.is_empty() {
        return None;
    }

    let agentic_status = agentic_readiness_status(readiness.agentic_readiness.as_ref()).unwrap_or("missing");
    if readiness.status != "ready" || !agentic_readiness_ready(readiness.agentic_readiness.as_ref()) {
        return Some(format!(
            "Production export blocked: training-ready machine segments require a ready agentic promotion report; latest promotion status is {}, agentic readiness is {}: {}",
            readiness.status,
            agentic_status,
            segment_id_preview(training_ready_machine_segment_ids)
        ));
    }

    let covered_segments = readiness.segment_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let uncovered_segments = training_ready_machine_segment_ids
        .iter()
        .filter(|segment_id| !covered_segments.contains(segment_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !uncovered_segments.is_empty() {
        return Some(format!(
            "Production export blocked: training-ready machine segments are not covered by the latest ready agentic promotion report: {}",
            segment_id_preview(&uncovered_segments)
        ));
    }

    None
}

fn segment_id_preview(segment_ids: &[String]) -> String {
    const MAX_PREVIEW: usize = 12;
    let mut preview = segment_ids.iter().take(MAX_PREVIEW).cloned().collect::<Vec<_>>();
    if segment_ids.len() > MAX_PREVIEW {
        preview.push(format!("... and {} more", segment_ids.len() - MAX_PREVIEW));
    }
    preview.join(", ")
}

fn segment_evidence_detail(evidence_json: Option<&str>) -> SegmentEvidenceDetail {
    let Some(evidence) = evidence_json.map(str::trim).filter(|value| !value.is_empty()) else {
        return SegmentEvidenceDetail {
            has_evidence_json: false,
            source_reference: None,
            t2_listener: false,
            parse_error: None,
        };
    };

    match serde_json::from_str::<serde_json::Value>(evidence) {
        Ok(value) => SegmentEvidenceDetail {
            has_evidence_json: true,
            source_reference: source_reference_evidence_detail(&value),
            t2_listener: contains_key_recursive(&value, "t2Evidence"),
            parse_error: None,
        },
        Err(error) => SegmentEvidenceDetail {
            has_evidence_json: true,
            source_reference: None,
            t2_listener: false,
            parse_error: Some(error.to_string()),
        },
    }
}

fn source_reference_evidence_detail(value: &serde_json::Value) -> Option<SourceReferenceEvidenceDetail> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(selection) = map.get("referenceSelection") {
                if let Some(detail) = source_reference_evidence_detail(selection) {
                    return Some(detail);
                }
            }

            let has_reference_fields = map.contains_key("referenceModelId")
                || map.contains_key("shouldCommit")
                || map.contains_key("selectedModelId");
            if has_reference_fields {
                let reference_agreement = map
                    .get("referenceAgreement")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| items.iter().map(source_reference_agreement_detail).collect::<Vec<_>>())
                    .unwrap_or_default();
                return Some(SourceReferenceEvidenceDetail {
                    reference_model_id: json_string(value, "referenceModelId"),
                    selected_model_id: json_string(value, "selectedModelId"),
                    selected_score: json_f64(value, "selectedScore"),
                    confidence: json_f64(value, "confidence"),
                    margin: json_f64(value, "margin"),
                    should_commit: map.get("shouldCommit").and_then(serde_json::Value::as_bool),
                    reference_agreement_count: reference_agreement.len(),
                    reference_agreement,
                });
            }

            map.values().find_map(source_reference_evidence_detail)
        }
        serde_json::Value::Array(items) => items.iter().find_map(source_reference_evidence_detail),
        _ => None,
    }
}

fn source_reference_agreement_detail(value: &serde_json::Value) -> SourceReferenceAgreementDetail {
    SourceReferenceAgreementDetail {
        reference_model_id: json_string(value, "referenceModelId"),
        selected_model_id: json_string(value, "selectedModelId"),
        selected_score: json_f64(value, "selectedScore"),
        confidence: json_f64(value, "confidence"),
        margin: json_f64(value, "margin"),
        should_commit: value.get("shouldCommit").and_then(serde_json::Value::as_bool),
    }
}

fn contains_key_recursive(value: &serde_json::Value, key: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key(key) || map.values().any(|value| contains_key_recursive(value, key))
        }
        serde_json::Value::Array(items) => items.iter().any(|value| contains_key_recursive(value, key)),
        _ => false,
    }
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn json_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(serde_json::Value::as_f64)
}

fn sanitize_bundle_filename(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "artifact".to_string()
    } else {
        trimmed.to_string()
    }
}

fn bundle_path_label(path: &str) -> String {
    let trimmed = path.trim();
    let label =
        trimmed.rsplit(['\\', '/']).next().map(str::trim).filter(|value| !value.is_empty()).unwrap_or("artifact");
    label.to_string()
}

fn sanitize_bundle_path_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                match key.as_str() {
                    "audioPath" | "transcriptPath" | "originalTranscriptPath" | "file" => {
                        if let serde_json::Value::String(path) = item {
                            *path = bundle_path_label(path);
                        }
                    }
                    "audioPaths" => {
                        if let serde_json::Value::Array(items) = item {
                            for entry in items {
                                if let serde_json::Value::String(path) = entry {
                                    *path = bundle_path_label(path);
                                } else {
                                    sanitize_bundle_path_fields(entry);
                                }
                            }
                        } else {
                            sanitize_bundle_path_fields(item);
                        }
                    }
                    _ => sanitize_bundle_path_fields(item),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sanitize_bundle_path_fields(item);
            }
        }
        _ => {}
    }
}

fn sanitized_bundle_value<T: Serialize + ?Sized>(value: &T) -> AppResult<serde_json::Value> {
    let mut value = serde_json::to_value(value)?;
    sanitize_bundle_path_fields(&mut value);
    Ok(value)
}

fn load_model_metadata_for_manifest(model_manager: &ModelManager) -> (Vec<crate::models::ModelMeta>, Option<String>) {
    match model_manager.load_meta() {
        Ok(model_meta) => (model_meta, None),
        Err(error) => {
            tracing::warn!("Failed to load model metadata for export bundle: {error}");
            (Vec::new(), Some(error))
        }
    }
}

fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> AppResult<()> {
    let json = serde_json::to_string_pretty(value)?;
    write_text(path, &json)?;
    Ok(())
}

fn write_text(path: &Path, text: &str) -> AppResult<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|ext| ext.to_str()).unwrap_or("tmp")));
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            std::fs::write(&tmp, text)?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

#[cfg(test)]
#[path = "export_bundle_tests.rs"]
mod tests;
