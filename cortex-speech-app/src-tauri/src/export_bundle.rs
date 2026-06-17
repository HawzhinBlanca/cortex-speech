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

    let segments = db.get_segments(None)?;
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
        "runConfig": crate::runs::config_from_settings(settings),
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
        This bundle was generated by Cortex and includes manifest, validation, quality, training-grade detail, source-reference transcripts, self-learning preferences when available, agent provenance with persisted stage timeline, model provenance, and tabular dataset files.\n",
        manifest["exportedAt"].as_str().unwrap_or("unknown"),
        settings.language,
        segments.len(),
        verified_segments,
        training_ready_segments,
        total_duration_ms,
        validation_gate.error_count,
        validation_gate.warning_count,
        production
    );
    write_text(&output_dir.join("dataset_card.md"), &card)?;

    let mut final_files = vec!["manifest.json".to_string(), "dataset_card.md".to_string()];
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
mod tests {
    use super::*;
    use crate::db::{SegmentHypothesis, SourceTranscriptRecord, SpeechSegment};
    use tempfile::TempDir;

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
            ood_score: None,
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
            ood_score: None,
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
            ood_score: None,
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
            ood_score: None,
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
            ood_score: None,
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
            ood_score: None,
            ..SpeechSegment::default()
        })
        .unwrap();

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
        let (_audio_path, segment_id) =
            insert_machine_silver_segment_with_coverage(&db, &tmp, "machine-no-agent-report");

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

    #[test]
    fn production_export_allows_machine_ready_rows_with_ready_agentic_promotion_report() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let tmp = TempDir::new().unwrap();
        let (audio_path, segment_id) =
            insert_machine_silver_segment_with_coverage(&db, &tmp, "machine-ready-agent-report");
        for model_id in ["gemini-2.5-pro", "gemini-2.5-flash"] {
            insert_current_identity_source_reference(&db, &audio_path, model_id);
        }
        let agent_report =
            record_ready_agentic_promotion_report(&db, &audio_path, &segment_id, "run-ready-agentic-production");

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
            serde_json::from_str(&std::fs::read_to_string(out.join("source_reference_manifest.json")).unwrap())
                .unwrap();
        assert_eq!(source_reference_manifest["schemaVersion"].as_u64(), Some(4));
        assert_eq!(source_reference_manifest["sourceReferenceCount"].as_u64(), Some(2));
        assert!(source_reference_manifest["references"]
            .as_array()
            .unwrap()
            .iter()
            .all(|reference| reference["audioIdentityVerified"].as_bool() == Some(true)));
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
            ood_score: None,
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
            serde_json::from_str(&std::fs::read_to_string(out.join("source_reference_manifest.json")).unwrap())
                .unwrap();
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
            ood_score: None,
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
            ood_score: None,
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
            serde_json::from_str(&std::fs::read_to_string(out.join("source_reference_manifest.json")).unwrap())
                .unwrap();
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
        assert_eq!(
            std::fs::read_to_string(out.join(bundled_reference_path)).unwrap(),
            "whole file reference transcript"
        );

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
        assert_eq!(
            long_file_dossiers["longFileDossiers"][0]["hypothesisModelCounts"]["omniasr-wsl-7b"].as_u64(),
            Some(1)
        );
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
            ood_score: None,
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
            ood_score: None,
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
            ood_score: None,
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
}
