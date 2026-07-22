use crate::db::Database;
use crate::error::AppResult;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetRunConfig {
    pub model_version: String,
    pub vad_threshold: f32,
    pub min_segment_duration_ms: u32,
    pub max_segment_duration_ms: u32,
    pub denoising: bool,
    pub diarization: bool,
    pub normalization: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStageEvent {
    pub id: i64,
    pub run_id: String,
    pub source: String,
    pub stage: String,
    pub status: String,
    pub file: String,
    pub detail: String,
    pub current: usize,
    pub total: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSourceReferenceSummary {
    pub audio_path: String,
    pub model_id: String,
    pub audio_content_hash: Option<String>,
    pub audio_size_bytes: Option<i64>,
    pub transcript_path: String,
    pub text_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentSourceReferenceCoverage {
    pub audio_path: String,
    pub required_models: Vec<String>,
    pub present_models: Vec<String>,
    pub missing_models: Vec<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentLongFileDossier {
    pub audio_path: String,
    pub chunk_count: usize,
    pub total_duration_ms: i64,
    pub source_references: Vec<AgentSourceReferenceSummary>,
    pub source_reference_coverage: AgentSourceReferenceCoverage,
    pub hypothesis_model_counts: BTreeMap<String, usize>,
    pub verdict_counts: BTreeMap<String, usize>,
    pub training_ready_segments: usize,
    pub escalated_segments: Vec<String>,
    pub promotion_status: String,
    pub promotion_blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHypothesisCoverageBlocker {
    pub segment_id: String,
    pub grade: String,
    pub training_ready: bool,
    pub coverage: crate::quality::HypothesisCoverageReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOrchestrationStage {
    pub stage: String,
    pub status: String,
    pub summary: String,
    pub blocker_count: usize,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentImportSummary {
    pub total_segments: usize,
    #[serde(default)]
    pub agentic_readiness: Option<serde_json::Value>,
    pub source_references: Vec<AgentSourceReferenceSummary>,
    #[serde(default)]
    pub source_reference_required: bool,
    #[serde(default)]
    pub required_source_reference_models: Vec<String>,
    pub source_reference_models: Vec<String>,
    #[serde(default)]
    pub source_reference_coverage: Vec<AgentSourceReferenceCoverage>,
    #[serde(default)]
    pub long_file_dossiers: Vec<AgentLongFileDossier>,
    pub hypothesis_models: Vec<String>,
    #[serde(default)]
    pub hypothesis_model_counts: BTreeMap<String, usize>,
    pub verdict_counts: BTreeMap<String, usize>,
    pub escalated_segments: Vec<String>,
    pub training_grade_summary: crate::quality::TrainingGradeSummary,
    #[serde(default)]
    pub training_grade_reason_counts: BTreeMap<String, usize>,
    #[serde(default)]
    pub hypothesis_coverage_blockers: Vec<AgentHypothesisCoverageBlocker>,
    #[serde(default)]
    pub orchestration_stages: Vec<AgentOrchestrationStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentImportReport {
    pub id: String,
    pub agent_run_id: Option<String>,
    pub source: String,
    pub status: String,
    pub audio_paths: Vec<String>,
    pub segment_ids: Vec<String>,
    pub summary: AgentImportSummary,
    pub jury_report: Option<serde_json::Value>,
    pub error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentImportReportBody {
    #[serde(default)]
    agent_run_id: Option<String>,
    summary: AgentImportSummary,
    jury_report: Option<serde_json::Value>,
    error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentImportReportOptions {
    pub agent_run_id: Option<String>,
    pub agentic_readiness: Option<serde_json::Value>,
    pub source_reference_required: bool,
    pub required_source_reference_models: Vec<String>,
}

impl AgentImportReportOptions {
    pub fn from_settings(settings: &crate::settings::AppSettings) -> Self {
        Self {
            agent_run_id: None,
            agentic_readiness: None,
            source_reference_required: settings.jury_cloud_opt_in,
            required_source_reference_models: settings.source_reference_models(),
        }
    }
}

/// Build the run's provenance config. `denoising_active` is whether the denoiser model is actually
/// available — round-23 #3: the denoiser is a silent pass-through when its (optional) model is absent,
/// so the run must record whether denoising was ACTUALLY applied, not merely requested in settings.
/// Recording the requested flag while the audio passed through un-denoised is a provenance lie.
pub fn config_from_settings(settings: &crate::settings::AppSettings, denoising_active: bool) -> DatasetRunConfig {
    DatasetRunConfig {
        model_version: format!("{:?}", settings.asr_model_size),
        vad_threshold: settings.vad_threshold,
        min_segment_duration_ms: settings.min_segment_duration_ms,
        max_segment_duration_ms: settings.max_segment_duration_ms,
        denoising: settings.enable_denoising && denoising_active,
        diarization: settings.enable_diarization,
        normalization: settings.auto_normalize,
    }
}

pub fn record_agent_import_report(
    db: &Database,
    source: &str,
    audio_paths: &[String],
    segment_ids: &[String],
    jury_report: Option<&serde_json::Value>,
    error: Option<&str>,
) -> AppResult<AgentImportReport> {
    record_agent_import_report_with_options(
        db,
        source,
        audio_paths,
        segment_ids,
        jury_report,
        error,
        AgentImportReportOptions::default(),
    )
}

pub fn record_agent_import_report_with_options(
    db: &Database,
    source: &str,
    audio_paths: &[String],
    segment_ids: &[String],
    jury_report: Option<&serde_json::Value>,
    error: Option<&str>,
    options: AgentImportReportOptions,
) -> AppResult<AgentImportReport> {
    let id = uuid::Uuid::new_v4().to_string();
    let status = if error.is_some() { "failed" } else { "completed" }.to_string();
    let audio_paths = normalized_audio_paths(db, audio_paths, segment_ids)?;
    let segment_ids = segment_ids.to_vec();
    let summary = build_agent_import_summary(db, &audio_paths, &segment_ids, &options)?;
    let body = AgentImportReportBody {
        agent_run_id: options.agent_run_id.clone(),
        summary,
        jury_report: jury_report.cloned(),
        error: error.map(str::to_string),
    };
    let audio_paths_json = serde_json::to_string(&audio_paths)?;
    let segment_ids_json = serde_json::to_string(&segment_ids)?;
    let report_json = serde_json::to_string(&body)?;

    db.connection().execute(
        "INSERT INTO agent_import_reports
            (id, source, status, audio_paths_json, segment_ids_json, report_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, source, status, audio_paths_json, segment_ids_json, report_json],
    )?;

    // P3.8: retention — this table appends one (often large `report_json`) row per import and had no
    // DELETE, so it grew without bound over months of daily use. Keep the newest N; prune older.
    prune_agent_import_reports(db, MAX_AGENT_IMPORT_REPORTS)?;

    get_agent_import_report(db, &id)?
        .ok_or_else(|| crate::error::AppError::Other("Agent import report was not persisted".into()))
}

/// Newest agent-import reports to retain; older ones are pruned after each insert (P3.8).
const MAX_AGENT_IMPORT_REPORTS: usize = 500;

/// Delete all but the newest `keep` agent-import reports (by created_at, id-tiebroken like the list
/// query). Returns rows deleted. Extracted so retention is unit-testable with a small `keep`.
pub(crate) fn prune_agent_import_reports(db: &Database, keep: usize) -> AppResult<usize> {
    let deleted = db.connection().execute(
        "DELETE FROM agent_import_reports
         WHERE id NOT IN (
             SELECT id FROM agent_import_reports ORDER BY datetime(created_at) DESC, id DESC LIMIT ?1
         )",
        params![keep as i64],
    )?;
    Ok(deleted)
}

pub fn list_agent_import_reports(db: &Database, limit: Option<usize>) -> AppResult<Vec<AgentImportReport>> {
    let limit = limit.unwrap_or(25).clamp(1, 200);
    let mut stmt = db.connection().prepare(
        "SELECT id, source, status, audio_paths_json, segment_ids_json, report_json, created_at
         FROM agent_import_reports
         -- `, id DESC` tiebreaker: created_at is 1s-resolution, so same-second reports tie and LIMIT 1
         -- would otherwise pick an arbitrary one — flipping the HF-export SILVER readiness allow-list.
         ORDER BY datetime(created_at) DESC, id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], map_agent_import_report)?;
    let mut reports = Vec::new();
    for row in rows {
        reports.push(row?);
    }
    Ok(reports)
}

pub fn get_agent_import_report(db: &Database, id: &str) -> AppResult<Option<AgentImportReport>> {
    let mut stmt = db.connection().prepare(
        "SELECT id, source, status, audio_paths_json, segment_ids_json, report_json, created_at
         FROM agent_import_reports
         WHERE id = ?1",
    )?;
    let mut rows = stmt.query(params![id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(map_agent_import_report(row)?))
    } else {
        Ok(None)
    }
}

fn normalized_audio_paths(db: &Database, audio_paths: &[String], segment_ids: &[String]) -> AppResult<Vec<String>> {
    let mut paths: BTreeSet<String> = audio_paths.iter().filter(|path| !path.trim().is_empty()).cloned().collect();
    if paths.is_empty() && !segment_ids.is_empty() {
        for segment in db.get_segments_by_ids(segment_ids)? {
            if !segment.audio_path.trim().is_empty() {
                paths.insert(segment.audio_path);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn build_agent_import_summary(
    db: &Database,
    audio_paths: &[String],
    segment_ids: &[String],
    options: &AgentImportReportOptions,
) -> AppResult<AgentImportSummary> {
    let segments = db.get_segments_by_ids(segment_ids)?;
    let mut source_references = Vec::new();
    let mut source_reference_models = BTreeSet::new();
    for audio_path in audio_paths {
        for reference in db.get_source_transcripts_for_audio(audio_path)? {
            source_reference_models.insert(reference.model_id.clone());
            source_references.push(AgentSourceReferenceSummary {
                audio_path: reference.audio_path,
                model_id: reference.model_id,
                audio_content_hash: reference.audio_content_hash,
                audio_size_bytes: reference.audio_size_bytes,
                transcript_path: reference.transcript_path,
                text_chars: reference.transcript_text.chars().count(),
            });
        }
    }

    source_references.sort_by(|a, b| a.audio_path.cmp(&b.audio_path).then_with(|| a.model_id.cmp(&b.model_id)));
    let required_source_reference_models =
        normalize_required_source_reference_models(&options.required_source_reference_models);
    let source_reference_required = options.source_reference_required || !source_references.is_empty();
    let source_reference_coverage = build_source_reference_coverage(
        audio_paths,
        &source_references,
        source_reference_required,
        &required_source_reference_models,
    );

    let mut hypothesis_models = BTreeSet::new();
    let mut hypothesis_model_counts = BTreeMap::new();
    let mut hypotheses_by_segment = BTreeMap::new();
    for segment_id in segment_ids {
        let hypotheses = db.get_hypotheses_for_segment(segment_id)?;
        for hypothesis in &hypotheses {
            // Count only NON-EMPTY hypotheses, matching the per-file dossier (build_long_file_dossiers)
            // and the coverage report's non_empty_models. An empty/whitespace hypothesis (e.g. a WSL 7B
            // pass that returned no text, still written as a SegmentHypothesis) is not real model output;
            // counting it here overstated multi-model coverage and DISAGREED with the dossier's own
            // per-model counts for the exact same data.
            if !hypothesis.transcript.trim().is_empty() {
                hypothesis_models.insert(hypothesis.model_id.clone());
                *hypothesis_model_counts.entry(hypothesis.model_id.clone()).or_insert(0) += 1;
            }
        }
        hypotheses_by_segment.insert(segment_id.clone(), hypotheses);
    }

    let mut verdict_counts = BTreeMap::new();
    let mut escalated_segments = Vec::new();
    let mut training_grade_reason_counts = BTreeMap::new();
    let mut hypothesis_coverage_blockers = Vec::new();
    for segment in &segments {
        let verdict = segment
            .verdict
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unprocessed")
            .to_string();
        *verdict_counts.entry(verdict).or_insert(0) += 1;
        if segment.escalated {
            escalated_segments.push(segment.id.clone());
        }
        let grade = crate::quality::training_grade_for_segment(segment);
        let coverage = crate::quality::hypothesis_coverage_for_model_outputs(
            hypotheses_by_segment.get(&segment.id).map(Vec::as_slice).unwrap_or(&[]),
        );
        if !coverage.passes_minimum {
            hypothesis_coverage_blockers.push(AgentHypothesisCoverageBlocker {
                segment_id: segment.id.clone(),
                grade: grade.grade.clone(),
                training_ready: grade.training_ready,
                coverage,
            });
        }
        for reason in grade.reasons {
            *training_grade_reason_counts.entry(reason).or_insert(0) += 1;
        }
    }
    escalated_segments.sort();
    hypothesis_coverage_blockers.sort_by(|a, b| a.segment_id.cmp(&b.segment_id));
    let training_grade_summary = crate::quality::training_grade_summary(&segments);
    let long_file_dossiers = build_long_file_dossiers(
        audio_paths,
        &segments,
        &source_references,
        &source_reference_coverage,
        &hypotheses_by_segment,
        &hypothesis_coverage_blockers,
    );
    let orchestration_stages = build_orchestration_stages(
        audio_paths,
        SourceReferenceStageInput {
            references: &source_references,
            required: source_reference_required,
            coverage: &source_reference_coverage,
        },
        segments.len(),
        &verdict_counts,
        &escalated_segments,
        &training_grade_summary,
        &hypothesis_coverage_blockers,
    );

    Ok(AgentImportSummary {
        total_segments: segments.len(),
        agentic_readiness: options.agentic_readiness.clone(),
        source_references,
        source_reference_required,
        required_source_reference_models,
        source_reference_models: source_reference_models.into_iter().collect(),
        source_reference_coverage,
        long_file_dossiers,
        hypothesis_models: hypothesis_models.into_iter().collect(),
        hypothesis_model_counts,
        verdict_counts,
        escalated_segments,
        training_grade_summary,
        training_grade_reason_counts,
        hypothesis_coverage_blockers,
        orchestration_stages,
    })
}

fn normalize_required_source_reference_models(models: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    models
        .iter()
        .filter_map(|model| {
            let trimmed = model.trim();
            if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

fn build_source_reference_coverage(
    audio_paths: &[String],
    source_references: &[AgentSourceReferenceSummary],
    source_reference_required: bool,
    required_source_reference_models: &[String],
) -> Vec<AgentSourceReferenceCoverage> {
    let required_models = required_source_reference_models.to_vec();
    audio_paths
        .iter()
        .map(|audio_path| {
            let mut present = BTreeSet::new();
            for reference in source_references {
                if reference.audio_path == *audio_path && reference.text_chars > 0 {
                    present.insert(reference.model_id.clone());
                }
            }
            let present_models = present.into_iter().collect::<Vec<_>>();
            let missing_models = if required_models.is_empty() {
                if source_reference_required && present_models.is_empty() {
                    vec!["source_reference_transcript".to_string()]
                } else {
                    Vec::new()
                }
            } else {
                required_models
                    .iter()
                    .filter(|model| !present_models.iter().any(|present| present == *model))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let complete = missing_models.is_empty() && (!source_reference_required || !present_models.is_empty());
            AgentSourceReferenceCoverage {
                audio_path: audio_path.clone(),
                required_models: required_models.clone(),
                present_models,
                missing_models,
                complete,
            }
        })
        .collect()
}

fn build_long_file_dossiers(
    audio_paths: &[String],
    segments: &[crate::db::SpeechSegment],
    source_references: &[AgentSourceReferenceSummary],
    source_reference_coverage: &[AgentSourceReferenceCoverage],
    hypotheses_by_segment: &BTreeMap<String, Vec<crate::db::SegmentHypothesis>>,
    hypothesis_coverage_blockers: &[AgentHypothesisCoverageBlocker],
) -> Vec<AgentLongFileDossier> {
    let empty_coverage = AgentSourceReferenceCoverage::default();
    audio_paths
        .iter()
        .map(|audio_path| {
            let source_segments =
                segments.iter().filter(|segment| segment.audio_path == *audio_path).collect::<Vec<_>>();
            let segment_ids = source_segments.iter().map(|segment| segment.id.as_str()).collect::<BTreeSet<_>>();
            let chunk_count = source_segments.len();
            let total_duration_ms = source_segments.iter().map(|segment| segment.duration_ms).sum();
            let source_references = source_references
                .iter()
                .filter(|reference| reference.audio_path == *audio_path)
                .cloned()
                .collect::<Vec<_>>();
            let source_reference_coverage = source_reference_coverage
                .iter()
                .find(|coverage| coverage.audio_path == *audio_path)
                .cloned()
                .unwrap_or_else(|| AgentSourceReferenceCoverage {
                    audio_path: audio_path.clone(),
                    ..empty_coverage.clone()
                });

            let mut hypothesis_model_counts = BTreeMap::new();
            for segment_id in &segment_ids {
                if let Some(hypotheses) = hypotheses_by_segment.get(*segment_id) {
                    for hypothesis in hypotheses {
                        if !hypothesis.transcript.trim().is_empty() {
                            *hypothesis_model_counts.entry(hypothesis.model_id.clone()).or_insert(0) += 1;
                        }
                    }
                }
            }

            let mut verdict_counts = BTreeMap::new();
            let mut training_ready_segments = 0usize;
            let mut escalated_segments = Vec::new();
            for segment in &source_segments {
                let verdict = segment
                    .verdict
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("unprocessed")
                    .to_string();
                *verdict_counts.entry(verdict).or_insert(0) += 1;
                if segment.escalated {
                    escalated_segments.push(segment.id.clone());
                }
                if crate::quality::training_grade_for_segment(segment).training_ready {
                    training_ready_segments += 1;
                }
            }
            escalated_segments.sort();

            let mut promotion_blockers = Vec::new();
            if chunk_count == 0 {
                promotion_blockers.push("no_speech_chunks".to_string());
            }
            if !source_reference_coverage.complete {
                if source_reference_coverage.missing_models.is_empty() {
                    promotion_blockers.push("source_reference_incomplete".to_string());
                } else {
                    promotion_blockers.push(format!(
                        "missing_source_reference_models:{}",
                        source_reference_coverage.missing_models.join(",")
                    ));
                }
            }
            for blocker in hypothesis_coverage_blockers {
                if blocker.training_ready && segment_ids.contains(blocker.segment_id.as_str()) {
                    promotion_blockers.push(format!("missing_hypothesis_coverage:{}", blocker.segment_id));
                }
            }
            if training_ready_segments == 0 {
                promotion_blockers.push("no_training_ready_segments".to_string());
            }

            promotion_blockers.sort();
            promotion_blockers.dedup();
            let promotion_status = if chunk_count == 0
                || training_ready_segments == 0
                || !source_reference_coverage.complete
                || promotion_blockers.iter().any(|blocker| blocker.starts_with("missing_hypothesis_coverage:"))
            {
                "blocked"
            } else if !escalated_segments.is_empty() {
                "needs_review"
            } else {
                "ready"
            }
            .to_string();

            AgentLongFileDossier {
                audio_path: audio_path.clone(),
                chunk_count,
                total_duration_ms,
                source_references,
                source_reference_coverage,
                hypothesis_model_counts,
                verdict_counts,
                training_ready_segments,
                escalated_segments,
                promotion_status,
                promotion_blockers,
            }
        })
        .collect()
}

fn stage(stage: &str, status: &str, summary: impl Into<String>, blockers: Vec<String>) -> AgentOrchestrationStage {
    AgentOrchestrationStage {
        stage: stage.to_string(),
        status: status.to_string(),
        summary: summary.into(),
        blocker_count: blockers.len(),
        blockers,
    }
}

struct SourceReferenceStageInput<'a> {
    references: &'a [AgentSourceReferenceSummary],
    required: bool,
    coverage: &'a [AgentSourceReferenceCoverage],
}

fn build_orchestration_stages(
    audio_paths: &[String],
    source_reference: SourceReferenceStageInput<'_>,
    total_segments: usize,
    verdict_counts: &BTreeMap<String, usize>,
    escalated_segments: &[String],
    training_grade_summary: &crate::quality::TrainingGradeSummary,
    hypothesis_coverage_blockers: &[AgentHypothesisCoverageBlocker],
) -> Vec<AgentOrchestrationStage> {
    let mut stages = Vec::new();

    let source_reference_blockers = source_reference
        .coverage
        .iter()
        .filter(|coverage| !coverage.complete)
        .map(|coverage| {
            if coverage.missing_models.is_empty() {
                coverage.audio_path.clone()
            } else {
                format!("{} missing {}", coverage.audio_path, coverage.missing_models.join(", "))
            }
        })
        .collect::<Vec<_>>();
    if source_reference.references.is_empty() && !source_reference.required {
        stages.push(stage(
            "source_reference",
            "skipped",
            "No whole-file source reference transcripts were recorded for this run.",
            Vec::new(),
        ));
    } else if source_reference_blockers.is_empty() && !source_reference.coverage.is_empty() {
        stages.push(stage(
            "source_reference",
            "ready",
            format!(
                "{} whole-file source reference transcript(s) cover {} source audio file(s).",
                source_reference.references.len(),
                source_reference.coverage.len()
            ),
            Vec::new(),
        ));
    } else {
        stages.push(stage(
            "source_reference",
            "blocked",
            format!(
                "{} source audio file(s) are missing whole-file reference transcripts.",
                source_reference_blockers.len()
            ),
            source_reference_blockers,
        ));
    }

    if total_segments == 0 {
        stages.push(stage(
            "audio_chunking",
            "blocked",
            "No speech segments were produced from the imported audio.",
            audio_paths.to_vec(),
        ));
    } else {
        stages.push(stage(
            "audio_chunking",
            "ready",
            format!("{total_segments} speech segment(s) produced and persisted."),
            Vec::new(),
        ));
    }

    let hypothesis_blockers =
        hypothesis_coverage_blockers.iter().map(|blocker| blocker.segment_id.clone()).collect::<Vec<_>>();
    if hypothesis_blockers.is_empty() {
        stages.push(stage(
            "multi_model_hypotheses",
            "ready",
            "Every imported segment has the required non-empty multi-model hypothesis coverage.",
            Vec::new(),
        ));
    } else {
        stages.push(stage(
            "multi_model_hypotheses",
            "blocked",
            format!(
                "{} segment(s) lack enough non-empty model hypotheses for automatic promotion.",
                hypothesis_blockers.len()
            ),
            hypothesis_blockers,
        ));
    }

    let unprocessed = verdict_counts.get("unprocessed").copied().unwrap_or(0);
    if unprocessed > 0 {
        stages.push(stage(
            "jury_adjudication",
            "blocked",
            format!("{unprocessed} segment(s) have not been processed by the jury pipeline."),
            vec!["unprocessed".to_string()],
        ));
    } else if !escalated_segments.is_empty() {
        stages.push(stage(
            "jury_adjudication",
            "needs_review",
            format!("{} segment(s) are in the human review queue.", escalated_segments.len()),
            escalated_segments.to_vec(),
        ));
    } else {
        stages.push(stage(
            "jury_adjudication",
            "ready",
            "All imported segments have a non-escalated machine or human verdict.",
            Vec::new(),
        ));
    }

    let training_ready_coverage_blockers = hypothesis_coverage_blockers
        .iter()
        .filter(|blocker| blocker.training_ready)
        .map(|blocker| blocker.segment_id.clone())
        .collect::<Vec<_>>();
    if training_grade_summary.training_ready_segments == 0 {
        stages.push(stage(
            "dataset_promotion",
            "blocked",
            "No imported segment is currently training-ready.",
            Vec::new(),
        ));
    } else if !training_ready_coverage_blockers.is_empty() {
        stages.push(stage(
            "dataset_promotion",
            "blocked",
            format!(
                "{} training-ready machine segment(s) still lack multi-model coverage.",
                training_ready_coverage_blockers.len()
            ),
            training_ready_coverage_blockers,
        ));
    } else if !escalated_segments.is_empty() {
        stages.push(stage(
            "dataset_promotion",
            "needs_review",
            format!(
                "{} segment(s) are training-ready, but review queue items remain.",
                training_grade_summary.training_ready_segments
            ),
            escalated_segments.to_vec(),
        ));
    } else {
        stages.push(stage(
            "dataset_promotion",
            "ready",
            format!(
                "{} segment(s) are training-ready with no promotion blockers.",
                training_grade_summary.training_ready_segments
            ),
            Vec::new(),
        ));
    }

    stages
}

#[allow(clippy::too_many_arguments)]
pub fn record_agent_stage_event(
    db: &Database,
    run_id: &str,
    source: &str,
    stage: &str,
    status: &str,
    file: &str,
    detail: &str,
    current: usize,
    total: usize,
) -> AppResult<()> {
    db.connection().execute(
        "INSERT INTO agent_stage_events
            (run_id, source, stage, status, file, detail, current, total)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![run_id, source, stage, status, file, detail, current as i64, total as i64],
    )?;
    Ok(())
}

pub fn list_agent_stage_events(
    db: &Database,
    run_id: Option<&str>,
    limit: Option<usize>,
) -> AppResult<Vec<AgentStageEvent>> {
    let limit = limit.unwrap_or(50).clamp(1, 500) as i64;
    let mut events = if let Some(run_id) = run_id.filter(|value| !value.trim().is_empty()) {
        let mut stmt = db.connection().prepare(
            "SELECT id, run_id, source, stage, status, file, detail, current, total, created_at
             FROM agent_stage_events
             WHERE run_id = ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![run_id, limit], map_agent_stage_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        events
    } else {
        let mut stmt = db.connection().prepare(
            "SELECT id, run_id, source, stage, status, file, detail, current, total, created_at
             FROM agent_stage_events
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], map_agent_stage_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        events
    };
    events.reverse();
    Ok(events)
}

fn map_agent_stage_event(row: &rusqlite::Row) -> rusqlite::Result<AgentStageEvent> {
    let current: i64 = row.get(7)?;
    let total: i64 = row.get(8)?;
    Ok(AgentStageEvent {
        id: row.get(0)?,
        run_id: row.get(1)?,
        source: row.get(2)?,
        stage: row.get(3)?,
        status: row.get(4)?,
        file: row.get(5)?,
        detail: row.get(6)?,
        current: current.max(0) as usize,
        total: total.max(0) as usize,
        created_at: row.get(9)?,
    })
}

fn map_agent_import_report(row: &rusqlite::Row) -> rusqlite::Result<AgentImportReport> {
    let audio_paths_json: String = row.get(3)?;
    let segment_ids_json: String = row.get(4)?;
    let report_json: String = row.get(5)?;
    let audio_paths = serde_json::from_str(&audio_paths_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e)))?;
    let segment_ids = serde_json::from_str(&segment_ids_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?;
    let body: AgentImportReportBody = serde_json::from_str(&report_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e)))?;
    Ok(AgentImportReport {
        id: row.get(0)?,
        agent_run_id: body.agent_run_id,
        source: row.get(1)?,
        status: row.get(2)?,
        audio_paths,
        segment_ids,
        summary: body.summary,
        jury_report: body.jury_report,
        error: body.error,
        created_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{SegmentHypothesis, SourceTranscriptRecord, SpeechSegment};

    #[test]
    fn run_config_records_denoising_only_when_actually_applied() {
        // Round-23 #3: denoising requested but the model absent -> the run config must record
        // denoising=false (the audio passed through un-denoised), never the requested flag.
        let on = crate::settings::AppSettings { enable_denoising: true, ..Default::default() };
        assert!(!config_from_settings(&on, false).denoising, "requested but inactive -> recorded false");
        assert!(config_from_settings(&on, true).denoising, "requested and active -> recorded true");
        let off = crate::settings::AppSettings { enable_denoising: false, ..Default::default() };
        assert!(!config_from_settings(&off, true).denoising, "not requested -> false regardless of model");
    }

    #[test]
    fn prune_agent_import_reports_keeps_newest() {
        // P3.8: retention — keep the newest N, delete older, so the table can't grow unbounded.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        for (id, at) in [("r1", "2020-01-01 00:00:01"), ("r2", "2020-01-01 00:00:02"), ("r3", "2020-01-01 00:00:03")] {
            db.connection()
                .execute(
                    "INSERT INTO agent_import_reports (id, source, status, audio_paths_json, segment_ids_json, report_json, created_at)
                     VALUES (?1, 'file', 'ok', '[]', '[]', '{}', ?2)",
                    params![id, at],
                )
                .unwrap();
        }
        assert_eq!(prune_agent_import_reports(&db, 2).unwrap(), 1, "one oldest report pruned");
        let ids: Vec<String> = db
            .connection()
            .prepare("SELECT id FROM agent_import_reports ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(ids, vec!["r2".to_string(), "r3".to_string()], "newest two kept, r1 pruned");
    }

    #[test]
    fn records_and_filters_agent_stage_events_by_run() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        record_agent_stage_event(
            &db,
            "run-a",
            "file",
            "source_reference",
            "running",
            "long.wav",
            "Building whole-file reference transcript",
            0,
            4,
        )
        .unwrap();
        record_agent_stage_event(
            &db,
            "run-a",
            "file",
            "jury_adjudication",
            "completed",
            "long.wav",
            "Reference commits: 2; review queue: 0",
            4,
            4,
        )
        .unwrap();
        record_agent_stage_event(
            &db,
            "run-b",
            "directory",
            "source_reference",
            "blocked",
            "other.wav",
            "Gemini API key is required",
            0,
            1,
        )
        .unwrap();

        let run_a = list_agent_stage_events(&db, Some("run-a"), Some(10)).unwrap();
        assert_eq!(run_a.len(), 2);
        assert_eq!(run_a[0].stage, "source_reference");
        assert_eq!(run_a[1].stage, "jury_adjudication");
        assert_eq!(run_a[1].status, "completed");
        assert_eq!(run_a[1].current, 4);

        let latest = list_agent_stage_events(&db, None, Some(2)).unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].run_id, "run-a");
        assert_eq!(latest[1].run_id, "run-b");
        assert_eq!(latest[1].status, "blocked");
    }

    #[test]
    fn records_agent_import_report_with_multi_agent_evidence_summary() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let audio_path = "C:\\audio\\long.wav".to_string();
        let segment = SpeechSegment {
            id: "seg-report".to_string(),
            audio_path: audio_path.clone(),
            raw_transcript: "raw candidate".to_string(),
            duration_ms: 2000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).unwrap();
        let evidence_json = serde_json::json!({
            "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
            "selectedTranscript": "final candidate",
            "shouldCommit": true
        })
        .to_string();
        db.write_segment_verdict(
            &segment.id,
            "jury_accept",
            Some("final candidate"),
            Some("multi-reference consensus committed agent text"),
            Some(&evidence_json),
            Some(0.91),
            false,
        )
        .unwrap();
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: "omniasr-ctc-300m".to_string(),
            transcript: "raw candidate".to_string(),
            confidence: Some(0.8),
        })
        .unwrap();
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: "omniasr-wsl-7b".to_string(),
            transcript: "final candidate".to_string(),
            confidence: Some(0.91),
        })
        .unwrap();
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: Some("source-audio-hash-v1".to_string()),
            audio_size_bytes: Some(123456),
            transcript_path: "source_transcripts\\long__gemini_2_5_pro.txt".to_string(),
            transcript_text: "whole file reference".to_string(),
            created_at: None,
        })
        .unwrap();

        let jury_report = serde_json::json!({
            "totalInput": 1,
            "referenceCommitted": 1,
            "humanInbox": 0
        });
        let report = record_agent_import_report(
            &db,
            "file",
            std::slice::from_ref(&audio_path),
            std::slice::from_ref(&segment.id),
            Some(&jury_report),
            None,
        )
        .unwrap();

        assert_eq!(report.status, "completed");
        assert_eq!(report.agent_run_id, None);
        assert_eq!(report.summary.total_segments, 1);
        assert_eq!(report.summary.source_reference_models, vec!["gemini-2.5-pro".to_string()]);
        assert_eq!(
            report.summary.hypothesis_models,
            vec!["omniasr-ctc-300m".to_string(), "omniasr-wsl-7b".to_string()]
        );
        assert_eq!(report.summary.hypothesis_model_counts.get("omniasr-ctc-300m"), Some(&1));
        assert_eq!(report.summary.hypothesis_model_counts.get("omniasr-wsl-7b"), Some(&1));
        assert_eq!(report.summary.verdict_counts.get("jury_accept"), Some(&1));
        assert_eq!(report.summary.training_grade_summary.training_ready_segments, 1);
        assert_eq!(report.summary.training_grade_reason_counts.get("high_confidence_jury_accept"), Some(&1));
        assert_eq!(report.summary.training_grade_reason_counts.get("multi_agent_evidence_verified"), Some(&1));
        assert!(report.summary.hypothesis_coverage_blockers.is_empty());
        assert_eq!(report.summary.source_references[0].audio_content_hash.as_deref(), Some("source-audio-hash-v1"));
        assert_eq!(report.summary.source_references[0].audio_size_bytes, Some(123456));
        assert_eq!(report.summary.long_file_dossiers.len(), 1);
        let dossier = &report.summary.long_file_dossiers[0];
        assert_eq!(dossier.audio_path, audio_path);
        assert_eq!(dossier.chunk_count, 1);
        assert_eq!(dossier.total_duration_ms, 2000);
        assert_eq!(dossier.source_references.len(), 1);
        assert_eq!(dossier.source_references[0].audio_content_hash.as_deref(), Some("source-audio-hash-v1"));
        assert_eq!(dossier.source_references[0].audio_size_bytes, Some(123456));
        assert!(dossier.source_reference_coverage.complete);
        assert_eq!(dossier.hypothesis_model_counts.get("omniasr-ctc-300m"), Some(&1));
        assert_eq!(dossier.hypothesis_model_counts.get("omniasr-wsl-7b"), Some(&1));
        assert_eq!(dossier.verdict_counts.get("jury_accept"), Some(&1));
        assert_eq!(dossier.training_ready_segments, 1);
        assert_eq!(dossier.promotion_status, "ready");
        assert!(dossier.promotion_blockers.is_empty());
        let stages = &report.summary.orchestration_stages;
        assert!(stages.iter().any(|stage| stage.stage == "source_reference" && stage.status == "ready"));
        assert!(stages.iter().any(|stage| stage.stage == "multi_model_hypotheses" && stage.status == "ready"));
        assert!(stages.iter().any(|stage| stage.stage == "dataset_promotion" && stage.status == "ready"));
        assert_eq!(report.jury_report.as_ref().unwrap()["referenceCommitted"], 1);

        let listed = list_agent_import_reports(&db, Some(10)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, report.id);
    }

    #[test]
    fn empty_hypothesis_is_excluded_from_both_the_summary_and_dossier_model_counts() {
        // An empty/whitespace hypothesis (e.g. a WSL 7B pass that returned no text but was still written
        // as a SegmentHypothesis) is not real model output. The top-level summary once counted it while
        // the per-file dossier (and coverage) did not — two displayed per-model counts disagreeing, with
        // the top-level one overstating multi-model coverage. Both must now exclude it and agree.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let audio_path = "C:\\audio\\empty-hyp.wav".to_string();
        let segment = SpeechSegment {
            id: "seg-empty-hyp".to_string(),
            audio_path: audio_path.clone(),
            raw_transcript: "real candidate".to_string(),
            duration_ms: 1500,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).unwrap();
        // One REAL hypothesis and one EMPTY-transcript hypothesis for the same segment.
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: "omniasr-ctc-300m".to_string(),
            transcript: "real candidate".to_string(),
            confidence: Some(0.8),
        })
        .unwrap();
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: "omniasr-wsl-7b".to_string(),
            transcript: "   ".to_string(), // empty / whitespace-only: not real output
            confidence: None,
        })
        .unwrap();

        let report = record_agent_import_report(
            &db,
            "file",
            std::slice::from_ref(&audio_path),
            std::slice::from_ref(&segment.id),
            None,
            None,
        )
        .unwrap();

        // The real model is counted; the empty-hypothesis model is excluded — at BOTH levels, in agreement.
        assert_eq!(report.summary.hypothesis_model_counts.get("omniasr-ctc-300m"), Some(&1));
        assert_eq!(
            report.summary.hypothesis_model_counts.get("omniasr-wsl-7b"),
            None,
            "an empty hypothesis must NOT be counted at the top level (it overstates coverage)"
        );
        assert_eq!(
            report.summary.hypothesis_models,
            vec!["omniasr-ctc-300m".to_string()],
            "the empty-hypothesis model must not appear as a contributing model"
        );
        let dossier = &report.summary.long_file_dossiers[0];
        assert_eq!(dossier.hypothesis_model_counts.get("omniasr-ctc-300m"), Some(&1));
        assert_eq!(dossier.hypothesis_model_counts.get("omniasr-wsl-7b"), None);
        assert_eq!(
            report.summary.hypothesis_model_counts, dossier.hypothesis_model_counts,
            "the summary and dossier per-model counts must agree"
        );
    }

    #[test]
    fn records_agent_import_report_source_reference_model_coverage_blockers() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let audio_path = "C:\\audio\\partial-reference.wav".to_string();
        let segment = SpeechSegment {
            id: "seg-partial-reference".to_string(),
            audio_path: audio_path.clone(),
            raw_transcript: "raw candidate".to_string(),
            duration_ms: 2000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).unwrap();
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: None,
            audio_size_bytes: None,
            transcript_path: "source_transcripts\\partial.gemini-2.5-pro.whole_file_reference.txt".to_string(),
            transcript_text: "whole file reference".to_string(),
            created_at: None,
        })
        .unwrap();

        let readiness_snapshot = serde_json::json!({
            "status": "degraded",
            "ready": false,
            "sourceReferenceModels": ["gemini-2.5-pro", "gemini-2.5-flash"],
            "availableHypothesisModels": ["omniasr-wsl-7b"],
            "requiredHypothesisModels": 2,
            "checks": [{
                "id": "hypothesis_coverage",
                "label": "Multi-model hypothesis coverage",
                "status": "blocked",
                "detail": "Only one hypothesis model is ready"
            }]
        });
        let report = record_agent_import_report_with_options(
            &db,
            "file",
            std::slice::from_ref(&audio_path),
            std::slice::from_ref(&segment.id),
            None,
            None,
            AgentImportReportOptions {
                agent_run_id: Some("run-source-coverage".to_string()),
                agentic_readiness: Some(readiness_snapshot.clone()),
                source_reference_required: true,
                required_source_reference_models: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
            },
        )
        .unwrap();

        assert_eq!(report.agent_run_id.as_deref(), Some("run-source-coverage"));
        let reloaded = get_agent_import_report(&db, &report.id).unwrap().expect("reloaded report");
        assert_eq!(reloaded.agent_run_id.as_deref(), Some("run-source-coverage"));
        assert_eq!(report.summary.agentic_readiness.as_ref(), Some(&readiness_snapshot));
        assert_eq!(reloaded.summary.agentic_readiness.as_ref(), Some(&readiness_snapshot));
        assert!(report.summary.source_reference_required);
        assert_eq!(
            report.summary.required_source_reference_models,
            vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()]
        );
        assert_eq!(report.summary.source_reference_coverage.len(), 1);
        let coverage = &report.summary.source_reference_coverage[0];
        assert!(!coverage.complete);
        assert_eq!(coverage.present_models, vec!["gemini-2.5-pro".to_string()]);
        assert_eq!(coverage.missing_models, vec!["gemini-2.5-flash".to_string()]);
        let dossier = &report.summary.long_file_dossiers[0];
        assert_eq!(dossier.audio_path, audio_path);
        assert_eq!(dossier.chunk_count, 1);
        assert_eq!(dossier.source_references.len(), 1);
        assert_eq!(dossier.source_reference_coverage.missing_models, vec!["gemini-2.5-flash".to_string()]);
        assert_eq!(dossier.promotion_status, "blocked");
        assert!(dossier.promotion_blockers.contains(&"missing_source_reference_models:gemini-2.5-flash".to_string()));
        let stage = report.summary.orchestration_stages.iter().find(|stage| stage.stage == "source_reference").unwrap();
        assert_eq!(stage.status, "blocked");
        assert_eq!(stage.blocker_count, 1);
        assert!(stage.blockers[0].contains("gemini-2.5-flash"));
    }

    #[test]
    fn records_agent_import_report_hypothesis_coverage_blockers() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let audio_path = "C:\\audio\\weak.wav".to_string();
        let segment = SpeechSegment {
            id: "seg-weak-coverage".to_string(),
            audio_path: audio_path.clone(),
            raw_transcript: "raw candidate".to_string(),
            duration_ms: 2000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).unwrap();
        let evidence_json = serde_json::json!({
            "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
            "selectedTranscript": "final candidate",
            "shouldCommit": true
        })
        .to_string();
        db.write_segment_verdict(
            &segment.id,
            "jury_accept",
            Some("final candidate"),
            Some("multi-reference consensus committed agent text"),
            Some(&evidence_json),
            Some(0.91),
            false,
        )
        .unwrap();
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: "omniasr-wsl-7b".to_string(),
            transcript: "final candidate".to_string(),
            confidence: Some(0.91),
        })
        .unwrap();

        let report = record_agent_import_report(
            &db,
            "file",
            std::slice::from_ref(&audio_path),
            std::slice::from_ref(&segment.id),
            None,
            None,
        )
        .unwrap();

        assert_eq!(report.summary.training_grade_summary.training_ready_segments, 1);
        assert_eq!(report.summary.hypothesis_coverage_blockers.len(), 1);
        let blocker = &report.summary.hypothesis_coverage_blockers[0];
        assert_eq!(blocker.segment_id, "seg-weak-coverage");
        assert!(blocker.training_ready);
        assert_eq!(blocker.grade, crate::quality::TRAINING_GRADE_SILVER);
        assert!(!blocker.coverage.passes_minimum);
        assert_eq!(blocker.coverage.non_empty_models, vec!["omniasr-wsl-7b".to_string()]);
        let stages = &report.summary.orchestration_stages;
        let hypothesis_stage = stages.iter().find(|stage| stage.stage == "multi_model_hypotheses").unwrap();
        assert_eq!(hypothesis_stage.status, "blocked");
        assert_eq!(hypothesis_stage.blockers, vec!["seg-weak-coverage".to_string()]);
        let promotion_stage = stages.iter().find(|stage| stage.stage == "dataset_promotion").unwrap();
        assert_eq!(promotion_stage.status, "blocked");
        assert_eq!(promotion_stage.blockers, vec!["seg-weak-coverage".to_string()]);
    }
}
