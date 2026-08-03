//! Headless import → export → validate path for real Tauri binary integration tests.
//!
//! Modes (env):
//! - Directory fixture: `CORTEX_INTEGRATION_TEST=1` + `CORTEX_INTEGRATION_FIXTURE=<dir>`
//! - Audiobook pipeline: `CORTEX_AUDIOBOOK_PIPELINE=1` + `CORTEX_AUDIOBOOK_MP3=<file>`
//!   Optional: `CORTEX_EXPORT_DIR` (default Desktop\cortex-audiobook-dataset)

use crate::export;
use crate::export_audio::{AudioExportFormat, AudioExportOptions};
use crate::pipeline::PipelineEvent;
use crate::settings::ExportFormat;
use crate::validation;
use crate::AppState;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::Manager;

pub fn run(app: &tauri::AppHandle) -> Result<(), String> {
    if std::env::var("CORTEX_AUDIOBOOK_PIPELINE").ok().as_deref() == Some("1") {
        return run_audiobook_pipeline(app);
    }

    let fixture =
        std::env::var("CORTEX_INTEGRATION_FIXTURE").map_err(|_| "CORTEX_INTEGRATION_FIXTURE not set".to_string())?;
    let fixture_path = Path::new(&fixture);
    if !fixture_path.is_dir() {
        return Err(format!("Fixture directory missing: {fixture}"));
    }

    {
        let state = app.state::<AppState>();
        let pipeline = state.lock_pipeline();
        pipeline.import_directory(fixture_path, None, |_| {}).map_err(|e| e.to_string())?;
    }

    let state = app.state::<AppState>();
    let db = state.lock_db();
    let segments = db.get_segments(None).map_err(|e| e.to_string())?;
    let count = segments.len();
    if count == 0 {
        return Err("integration import produced zero segments".into());
    }

    // Into the app data dir, not the TEMP root: the caller owns that dir and disposes of it, so the
    // export goes with it. Written to the TEMP root it outlived every run — 58 stale files measured.
    let export_path = crate::get_app_data_dir().join(format!("integration-export-{count}.json"));
    export::export_dataset(&db, &export_path, &ExportFormat::Json).map_err(|e| e.to_string())?;
    if !export_path.exists() {
        return Err("export file was not created".into());
    }

    let report = validation::validate_dataset(&db).map_err(|e| e.to_string())?;
    if report.total_segments == 0 {
        return Err("validation report has zero segments".into());
    }

    println!(
        "CORTEX_INTEGRATION_OK segments={count} export={} validation_errors={}",
        export_path.display(),
        report.errors.len()
    );
    Ok(())
}

fn default_export_dir() -> PathBuf {
    std::env::var("CORTEX_EXPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("cortex-audiobook-dataset"))
}

#[derive(Debug, Clone)]
struct CapturedAgentStageEvent {
    stage: String,
    status: String,
    file: String,
    detail: String,
    current: usize,
    total: usize,
}

impl CapturedAgentStageEvent {
    fn record(&self, db: &crate::db::Database, run_id: &str, source: &str) -> Result<(), String> {
        crate::runs::record_agent_stage_event(
            db,
            run_id,
            source,
            &self.stage,
            &self.status,
            &self.file,
            &self.detail,
            self.current,
            self.total,
        )
        .map_err(|e| e.to_string())
    }
}

fn persist_agent_stage_events(
    db: &crate::db::Database,
    run_id: &str,
    source: &str,
    events: &[CapturedAgentStageEvent],
) -> Result<(), String> {
    for event in events {
        event.record(db, run_id, source)?;
    }
    Ok(())
}

fn audiobook_report_options(
    settings: &crate::settings::AppSettings,
    agent_run_id: &str,
    agentic_readiness: Option<serde_json::Value>,
) -> crate::runs::AgentImportReportOptions {
    let mut options = crate::runs::AgentImportReportOptions::from_settings(settings);
    options.agent_run_id = Some(agent_run_id.to_string());
    options.agentic_readiness = agentic_readiness;
    options
}

fn audiobook_agentic_readiness_snapshot(
    state: &AppState,
    settings: &crate::settings::AppSettings,
) -> serde_json::Value {
    let model_status = {
        let model_manager = state.lock_model_manager();
        model_manager.status()
    };
    let external_provider = crate::commands::external_provider_status(settings);
    crate::commands::build_agentic_readiness_snapshot(settings, &model_status, &external_provider)
}

#[allow(clippy::too_many_arguments)]
fn record_audiobook_report(
    db: &crate::db::Database,
    audio_path: &str,
    segment_ids: &[String],
    settings: &crate::settings::AppSettings,
    agent_run_id: &str,
    agentic_readiness: Option<serde_json::Value>,
    jury_report: Option<&serde_json::Value>,
    error: Option<&str>,
) -> Result<(), String> {
    crate::runs::record_agent_import_report_with_options(
        db,
        "audiobook",
        &[audio_path.to_string()],
        segment_ids,
        jury_report,
        error,
        audiobook_report_options(settings, agent_run_id, agentic_readiness),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn run_audiobook_pipeline(app: &tauri::AppHandle) -> Result<(), String> {
    let mp3 = std::env::var("CORTEX_AUDIOBOOK_MP3").map_err(|_| "CORTEX_AUDIOBOOK_MP3 not set".to_string())?;
    let mp3_path = Path::new(&mp3);
    if !mp3_path.is_file() {
        return Err(format!("Audiobook file missing: {mp3}"));
    }

    let export_dir = default_export_dir();
    std::fs::create_dir_all(&export_dir).map_err(|e| e.to_string())?;

    eprintln!("[audiobook] import: {}", mp3_path.display());
    eprintln!("[audiobook] export: {}", export_dir.display());

    let agent_run_id = uuid::Uuid::new_v4().to_string();
    let audio_label = mp3_path.file_name().and_then(|name| name.to_str()).unwrap_or("audiobook").to_string();
    let captured_stage_events = Arc::new(Mutex::new(Vec::new()));
    let import_stage_events = Arc::clone(&captured_stage_events);

    let import_result = {
        let state = app.state::<AppState>();
        let pipeline = state.lock_pipeline();
        pipeline.import_single_file_with_events(mp3_path, None, Some(&agent_run_id), move |event| match event {
            PipelineEvent::AgentStage { stage, status, file, detail, current, total } => {
                eprintln!("[audiobook] agent {stage}:{status} ({current}/{total}) {detail}");
                import_stage_events
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(CapturedAgentStageEvent { stage, status, file, detail, current, total });
            }
            PipelineEvent::Progress { current, total, status, .. } => {
                eprintln!("[audiobook] {status} ({current}/{total})");
            }
            _ => {}
        })
    };

    let captured_stage_events = captured_stage_events.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    if let Err(error) = import_result {
        let state = app.state::<AppState>();
        let settings = state.lock_settings().clone();
        let agentic_readiness = audiobook_agentic_readiness_snapshot(&state, &settings);
        let db = state.lock_db();
        let detail = format!("audiobook import failed: {error}");
        persist_agent_stage_events(&db, &agent_run_id, "audiobook", &captured_stage_events)?;
        crate::runs::record_agent_stage_event(
            &db,
            &agent_run_id,
            "audiobook",
            "agent_report",
            "failed",
            &audio_label,
            &detail,
            1,
            1,
        )
        .map_err(|e| e.to_string())?;
        record_audiobook_report(
            &db,
            &mp3,
            &[],
            &settings,
            &agent_run_id,
            Some(agentic_readiness),
            None,
            Some(&detail),
        )?;
        return Err(detail);
    }

    {
        let state = app.state::<AppState>();
        let settings = state.lock_settings().clone();
        let agentic_readiness = audiobook_agentic_readiness_snapshot(&state, &settings);
        let db = state.lock_db();
        persist_agent_stage_events(&db, &agent_run_id, "audiobook", &captured_stage_events)?;
        let segments = db.get_segments(None).map_err(|e| e.to_string())?;
        let count = segments.len();
        if count == 0 {
            let detail = "audiobook import produced zero segments".to_string();
            crate::runs::record_agent_stage_event(
                &db,
                &agent_run_id,
                "audiobook",
                "agent_report",
                "failed",
                &audio_label,
                &detail,
                1,
                1,
            )
            .map_err(|e| e.to_string())?;
            record_audiobook_report(
                &db,
                &mp3,
                &[],
                &settings,
                &agent_run_id,
                Some(agentic_readiness),
                None,
                Some(&detail),
            )?;
            return Err(detail);
        }
        let segment_ids: Vec<String> = segments.iter().map(|s| s.id.clone()).collect();
        eprintln!("[audiobook] jury: {} segments", segment_ids.len());
        crate::runs::record_agent_stage_event(
            &db,
            &agent_run_id,
            "audiobook",
            "jury_adjudication",
            "running",
            &audio_label,
            "Running multi-model jury adjudication for imported audiobook chunks",
            3,
            4,
        )
        .map_err(|e| e.to_string())?;
        let jury_report = match crate::commands::run_jury_pipeline_core(&db, &settings, segment_ids.clone()) {
            Ok(report) => {
                crate::runs::record_agent_stage_event(
                    &db,
                    &agent_run_id,
                    "audiobook",
                    "jury_adjudication",
                    "completed",
                    &audio_label,
                    "Multi-model jury adjudication completed for imported audiobook chunks",
                    3,
                    4,
                )
                .map_err(|e| e.to_string())?;
                report
            }
            Err(error) => {
                let detail = format!("audiobook jury pipeline failed: {error}");
                crate::runs::record_agent_stage_event(
                    &db,
                    &agent_run_id,
                    "audiobook",
                    "jury_adjudication",
                    "blocked",
                    &audio_label,
                    &detail,
                    3,
                    4,
                )
                .map_err(|e| e.to_string())?;
                record_audiobook_report(
                    &db,
                    &mp3,
                    &segment_ids,
                    &settings,
                    &agent_run_id,
                    Some(agentic_readiness.clone()),
                    None,
                    Some(&detail),
                )?;
                return Err(detail);
            }
        };
        record_audiobook_report(
            &db,
            &mp3,
            &segment_ids,
            &settings,
            &agent_run_id,
            Some(agentic_readiness),
            Some(&jury_report),
            None,
        )?;
        crate::runs::record_agent_stage_event(
            &db,
            &agent_run_id,
            "audiobook",
            "agent_report",
            "completed",
            &audio_label,
            "Persisted audiobook agent report with source-reference and hypothesis coverage summary",
            4,
            4,
        )
        .map_err(|e| e.to_string())?;

        let json_path = export_dir.join("dataset.json");
        let jsonl_path = export_dir.join("dataset.jsonl");
        let parquet_path = export_dir.join("dataset.parquet");
        let hf_dir = export_dir.join("huggingface");
        let audio_dir = export_dir.join("audio");
        let bundle_dir = export_dir.join("dataset_bundle");

        export::export_dataset(&db, &json_path, &ExportFormat::Json).map_err(|e| e.to_string())?;
        export::export_dataset(&db, &jsonl_path, &ExportFormat::Jsonl).map_err(|e| e.to_string())?;
        export::export_dataset(&db, &parquet_path, &ExportFormat::Parquet).map_err(|e| e.to_string())?;
        export::export_huggingface_dataset(&db, &hf_dir, &settings).map_err(|e| e.to_string())?;

        let audio_result = crate::export_audio::export_audio_segments(
            &db,
            &segment_ids,
            &AudioExportOptions {
                output_dir: audio_dir.to_string_lossy().into_owned(),
                format: AudioExportFormat::Wav,
                sample_rate: 16_000,
                include_metadata: true,
            },
        )
        .map_err(|e| e.to_string())?;

        {
            let model_manager = state.lock_model_manager();
            crate::export_bundle::export_dataset_bundle(&db, &model_manager, &bundle_dir, &settings, false, 0)
                .map_err(|e| e.to_string())?;
        }

        let report = validation::validate_dataset(&db).map_err(|e| e.to_string())?;

        println!(
            "CORTEX_AUDIOBOOK_OK segments={count} run_id={} export_dir={} json={} jsonl={} parquet={} hf={} wav={} bundle={} validation_errors={}",
            agent_run_id,
            export_dir.display(),
            json_path.display(),
            jsonl_path.display(),
            parquet_path.display(),
            hf_dir.display(),
            audio_result.succeeded,
            bundle_dir.display(),
            report.errors.len()
        );
    }
    Ok(())
}
