//! Agentic-ops + engine-control IPC commands — slice 10 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use agentic::*;`),
//! so `lib.rs`'s invoke_handler still names `commands::run_consensus_refinery` and the frontend invokes
//! are untouched. Same functions, only relocated.
//!
//! Agent import/stage reports, the escalation queue + rate trend, the IRT consensus refinery, and the
//! 7B engine status/readiness/start controls. The WSL status probes + consensus compute run via
//! run_blocking so polling never freezes the UI (per the Week-1 responsiveness audit).

use super::{
    build_agentic_readiness, external_provider_status, run_blocking, validate, AgenticReadiness, EngineStatusV1,
    RATE_LIMITER, STRICT_RATE_LIMITER,
};
use crate::ipc_contract::{
    AgentImportReportV1, AgentStageEventV1, AgenticReadinessV1, CommandErrorV1, SuggestedActionV1,
};
use crate::AppState;
use tauri::State;

fn engine_rate_limited(message: &str) -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", message, true).suggested(SuggestedActionV1::Retry)
}

fn public_engine_error(
    code: &str,
    message: &str,
    retryable: bool,
    action: SuggestedActionV1,
    _private_detail: &str,
) -> CommandErrorV1 {
    CommandErrorV1::new(code, message, retryable).suggested(action)
}

fn public_engine_block_reason(_private_detail: &str) -> String {
    "Champion engine startup is blocked. Open Models or Health for recovery options.".to_string()
}

fn public_engine_probe_failure_reason(_private_detail: &str) -> String {
    "Champion engine is offline or did not answer the health probe.".to_string()
}

fn agent_history_rate_limited() -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", "Import history is busy. Retry in a moment.", true)
        .suggested(SuggestedActionV1::Retry)
}

fn invalid_agent_run_id() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_AGENT_RUN_ID", "The import run identity is invalid.", false)
}

fn agent_history_read_failed(kind: &'static str, private_detail: &str) -> CommandErrorV1 {
    tracing::warn!(history_kind = kind, error = private_detail, "Could not read private agent import history");
    CommandErrorV1::new(
        "AGENT_HISTORY_READ_FAILED",
        "Import history could not be read. Retry; if it continues, open Health.",
        true,
    )
    .suggested(SuggestedActionV1::Retry)
}

fn agent_readiness_failed(private_detail: &str) -> CommandErrorV1 {
    tracing::warn!(error = private_detail, "Agentic readiness probe failed");
    CommandErrorV1::new(
        "AGENTIC_READINESS_FAILED",
        "Import readiness could not be checked. Retry; if it continues, open Health.",
        true,
    )
    .suggested(SuggestedActionV1::Retry)
}

/// Bounded (~5s) health check of the champion 7B engine, for the UI status pill. Cheap + side-effect
/// free (a TCP probe), so the frontend can poll it.
#[tauri::command]
#[specta::specta]
pub async fn get_champion_engine_status(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<EngineStatusV1, CommandErrorV1> {
    RATE_LIMITER
        .check("get_champion_engine_status")
        .map_err(|_| engine_rate_limited("The champion health check is busy. Retry in a moment."))?;
    let port = crate::pipeline::wsl_7b_port();
    let expected = crate::registry::champion_identity(&state.lock_db(), crate::deployment::OMNIASR_7B_FAMILY).map_err(
        |error| {
            public_engine_error(
                "CHAMPION_REGISTRY_UNAVAILABLE",
                "Champion identity could not be read. Open Health for recovery options.",
                false,
                SuggestedActionV1::OpenHealth,
                &error.to_string(),
            )
        },
    )?;
    let Some(expected) = expected else {
        return Ok(EngineStatusV1 {
            ready: false,
            port,
            identity_matches: false,
            expected_model_version_id: None,
            expected_deployment_sha256: None,
            loaded_model_version_id: None,
            loaded_deployment_sha256: None,
            reason: Some("No OmniASR-7B champion is registered; bootstrap the measured incumbent first.".into()),
        });
    };
    if let Some(reason) = crate::engine_runtime::champion_operational_block_reason() {
        return Ok(EngineStatusV1 {
            ready: false,
            port,
            identity_matches: false,
            expected_model_version_id: Some(expected.model_version_id),
            expected_deployment_sha256: Some(expected.deployment_sha256),
            loaded_model_version_id: None,
            loaded_deployment_sha256: None,
            reason: Some(public_engine_block_reason(reason)),
        });
    }
    let expected_for_probe = expected.clone();
    let result =
        run_blocking(move || crate::engine_runtime::query_loaded_champion(&app, std::time::Duration::from_secs(3)))
            .await;
    Ok(match result {
        Ok(loaded) => {
            let identity_matches = loaded.matches(&expected_for_probe);
            EngineStatusV1 {
                ready: identity_matches,
                port,
                identity_matches,
                expected_model_version_id: Some(expected.model_version_id),
                expected_deployment_sha256: Some(expected.deployment_sha256),
                loaded_model_version_id: Some(loaded.model_version_id),
                loaded_deployment_sha256: Some(loaded.deployment_sha256),
                reason: (!identity_matches).then_some(
                    "The port answers, but the loaded model identity does not match the registry champion.".into(),
                ),
            }
        }
        Err(error) => EngineStatusV1 {
            ready: false,
            port,
            identity_matches: false,
            expected_model_version_id: Some(expected.model_version_id),
            expected_deployment_sha256: Some(expected.deployment_sha256),
            loaded_model_version_id: None,
            loaded_deployment_sha256: None,
            reason: Some(public_engine_probe_failure_reason(&error)),
        },
    })
}

/// Start the champion 7B server (WSL) FROM THE APP so the owner never hand-runs a terminal. Spawns
/// the committed start script DETACHED and returns immediately; the UI then polls
/// get_champion_engine_status until ready (warm-up loads ~30 GB, 1-5 min). The script path comes from
/// CORTEX_7B_START_SCRIPT (the desktop launcher sets it); without it we return an actionable error
/// rather than guess a path.
#[tauri::command]
#[specta::specta]
pub async fn start_champion_engine(app: tauri::AppHandle) -> Result<(), CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("start_champion_engine")
        .map_err(|_| engine_rate_limited("Champion startup is already busy. Retry in a moment."))?;
    // `restart_current_champion` tree-kills the held child and spawns a new wsl.exe that loads ~30 GB.
    // As a SYNC command that ran inline, that whole body executed on the UI thread and froze the
    // window (test_ui_thread_blocking_audit.py). Same async + run_blocking shape as
    // `get_champion_engine_status` above.
    run_blocking(move || crate::engine_runtime::restart_current_champion(&app)).await.map_err(|error| {
        public_engine_error(
            "CHAMPION_START_FAILED",
            "Champion startup failed. Open Models or Health for recovery options.",
            true,
            SuggestedActionV1::OpenModels,
            &error,
        )
    })
}

#[cfg(test)]
mod typed_engine_ipc_tests {
    use super::*;

    #[test]
    fn public_engine_status_and_start_failures_never_forward_private_probe_details() {
        let hostile = r"WSL SQL token=secret D:\private\cortex_7b_server.py";
        let blocked = public_engine_block_reason(hostile);
        let offline = public_engine_probe_failure_reason(hostile);
        let start = public_engine_error(
            "CHAMPION_START_FAILED",
            "Champion startup failed. Open Models or Health for recovery options.",
            true,
            SuggestedActionV1::OpenModels,
            hostile,
        );
        let wire = format!("{blocked} {offline} {}", serde_json::to_string(&start).unwrap());
        assert!(wire.contains("CHAMPION_START_FAILED"));
        assert!(wire.contains("openModels"));
        for forbidden in ["SQL", "D:\\", "private", "token", "secret", "cortex_7b_server.py"] {
            assert!(!wire.contains(forbidden));
        }
    }

    #[test]
    fn public_agent_history_and_readiness_errors_never_forward_private_diagnostics() {
        let hostile = r"SQL failed at D:\private\cortex-speech.db token=secret";
        let errors = [agent_history_read_failed("reports", hostile), agent_readiness_failed(hostile)];
        let wire = serde_json::to_string(&errors).expect("serialize public errors");
        assert!(wire.contains("AGENT_HISTORY_READ_FAILED"));
        assert!(wire.contains("AGENTIC_READINESS_FAILED"));
        for forbidden in ["SQL", "D:\\", "private", "cortex-speech.db", "token", "secret"] {
            assert!(!wire.contains(forbidden), "public command error leaked {forbidden}: {wire}");
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn list_agent_import_reports(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<AgentImportReportV1>, CommandErrorV1> {
    RATE_LIMITER.check("list_agent_import_reports").map_err(|_| agent_history_rate_limited())?;
    let db = state.lock_db();
    let reports = crate::runs::list_agent_import_reports(&db, limit)
        .map_err(|error| agent_history_read_failed("import_reports", &error.to_string()))?;
    Ok(reports.iter().map(AgentImportReportV1::from).collect())
}

#[tauri::command]
#[specta::specta]
pub fn get_agent_import_report_by_run_id(
    run_id: String,
    state: State<'_, AppState>,
) -> Result<Option<AgentImportReportV1>, CommandErrorV1> {
    RATE_LIMITER.check("get_agent_import_report_by_run_id").map_err(|_| agent_history_rate_limited())?;
    validate::validate_identifier(&run_id).map_err(|_| invalid_agent_run_id())?;
    let canonical = uuid::Uuid::parse_str(&run_id).map(|id| id.to_string()).map_err(|_| invalid_agent_run_id())?;
    if canonical != run_id {
        return Err(invalid_agent_run_id());
    }
    let db = state.lock_db();
    crate::runs::get_agent_import_report_by_run_id(&db, &run_id)
        .map(|report| report.as_ref().map(AgentImportReportV1::from))
        .map_err(|error| agent_history_read_failed("import_report_by_run", &error.to_string()))
}

#[tauri::command]
#[specta::specta]
pub fn list_agent_stage_events(
    run_id: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<AgentStageEventV1>, CommandErrorV1> {
    RATE_LIMITER.check("list_agent_stage_events").map_err(|_| agent_history_rate_limited())?;
    if let Some(run_id) = run_id.as_deref().filter(|value| !value.trim().is_empty()) {
        validate::validate_identifier(run_id).map_err(|_| invalid_agent_run_id())?;
    }
    let db = state.lock_db();
    let events = crate::runs::list_agent_stage_events(&db, run_id.as_deref(), limit)
        .map_err(|error| agent_history_read_failed("stage_events", &error.to_string()))?;
    Ok(events.iter().map(AgentStageEventV1::from).collect())
}

#[tauri::command]
#[specta::specta]
pub async fn check_agentic_readiness(state: State<'_, AppState>) -> Result<AgenticReadinessV1, CommandErrorV1> {
    RATE_LIMITER.check("check_agentic_readiness").map_err(|_| agent_history_rate_limited())?;
    // Grab the two cheap, lock-guarded inputs on the caller thread (a settings clone + a bounded
    // model-file stat), then run the SLOW part off the UI thread: external_provider_status shells out
    // to `wsl --status` (bounded at ~10s but that still froze the readiness poll). No lock is held
    // across the await — both guards are released before run_blocking.
    let settings = state.lock_settings().clone();
    let model_status = {
        let model_manager = state.lock_model_manager();
        model_manager.status()
    };
    let readiness: AgenticReadiness = run_blocking(move || {
        let external_provider = external_provider_status(&settings);
        Ok(build_agentic_readiness(&settings, &model_status, &external_provider))
    })
    .await
    .map_err(|error| agent_readiness_failed(&error))?;
    Ok(AgenticReadinessV1::from(&readiness))
}

#[tauri::command]
#[specta::specta]
pub fn get_escalation_queue(
    state: State<'_, AppState>,
    limit: usize,
) -> Result<Vec<crate::db::SpeechSegment>, CommandErrorV1> {
    RATE_LIMITER
        .check("get_escalation_queue")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("get_escalation_queue"))?;
    // The Inbox is a SERVING PATH: it plays these clips, mints playback receipts for them, and
    // records accept/edit/reject against them. So the voice focus governs it exactly as it governs
    // the review page and every phone queue — found by review 2026-08-20, when narrowing the review
    // page still left the Inbox handing out the guest clips the focus exists to skip.
    let focus = {
        let dir = state.lock_data_dir().clone();
        crate::voice_focus::resolve(dir.as_deref()).map_err(|error| {
            tracing::warn!("Owner escalation voice-focus resolution failed: {error}");
            crate::ipc_contract::public_owner_analysis_error(
                crate::ipc_contract::OwnerAnalysisOperationV1::EscalationQueue,
                &error,
            )
        })?
    };
    let db = state.lock_db();
    db.get_escalation_queue(limit, focus.as_deref()).map_err(|error| {
        tracing::warn!("Owner escalation queue read failed: {error}");
        crate::ipc_contract::public_owner_analysis_error(
            crate::ipc_contract::OwnerAnalysisOperationV1::EscalationQueue,
            &error.to_string(),
        )
    })
}

#[tauri::command]
#[specta::specta]
pub fn get_escalation_rate_trend(
    state: State<'_, AppState>,
) -> Result<Vec<crate::ipc_contract::EscalationTrendPointV1>, CommandErrorV1> {
    RATE_LIMITER
        .check("get_escalation_rate_trend")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("get_escalation_rate_trend"))?;
    let db = state.lock_db();
    crate::jury::get_escalation_rate_trend(&db).map(|points| points.into_iter().map(Into::into).collect()).map_err(
        |error| {
            tracing::warn!("Owner escalation trend read failed: {error}");
            crate::ipc_contract::public_owner_analysis_error(
                crate::ipc_contract::OwnerAnalysisOperationV1::EscalationTrend,
                &error.to_string(),
            )
        },
    )
}
