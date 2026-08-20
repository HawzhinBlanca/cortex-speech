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
    build_agentic_readiness, external_provider_status, run_blocking, AgenticReadiness, EngineStatus, RATE_LIMITER,
    STRICT_RATE_LIMITER,
};
use crate::AppState;
use tauri::State;

/// Bounded (~5s) health check of the champion 7B engine, for the UI status pill. Cheap + side-effect
/// free (a TCP probe), so the frontend can poll it.
#[tauri::command]
pub async fn get_champion_engine_status(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<EngineStatus, String> {
    let port = crate::pipeline::wsl_7b_port();
    let expected =
        crate::registry::champion_identity(&state.lock_db(), crate::deployment::OMNIASR_7B_FAMILY).ok().flatten();
    let Some(expected) = expected else {
        return Ok(EngineStatus {
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
        return Ok(EngineStatus {
            ready: false,
            port,
            identity_matches: false,
            expected_model_version_id: Some(expected.model_version_id),
            expected_deployment_sha256: Some(expected.deployment_sha256),
            loaded_model_version_id: None,
            loaded_deployment_sha256: None,
            reason: Some(reason.to_string()),
        });
    }
    let expected_for_probe = expected.clone();
    let result =
        run_blocking(move || crate::engine_runtime::query_loaded_champion(&app, std::time::Duration::from_secs(3)))
            .await;
    Ok(match result {
        Ok(loaded) => {
            let identity_matches = loaded.matches(&expected_for_probe);
            EngineStatus {
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
        Err(error) => EngineStatus {
            ready: false,
            port,
            identity_matches: false,
            expected_model_version_id: Some(expected.model_version_id),
            expected_deployment_sha256: Some(expected.deployment_sha256),
            loaded_model_version_id: None,
            loaded_deployment_sha256: None,
            reason: Some(error),
        },
    })
}

/// Start the champion 7B server (WSL) FROM THE APP so the owner never hand-runs a terminal. Spawns
/// the committed start script DETACHED and returns immediately; the UI then polls
/// get_champion_engine_status until ready (warm-up loads ~30 GB, 1-5 min). The script path comes from
/// CORTEX_7B_START_SCRIPT (the desktop launcher sets it); without it we return an actionable error
/// rather than guess a path.
#[tauri::command]
pub async fn start_champion_engine(app: tauri::AppHandle) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("start_champion_engine")?;
    // `restart_current_champion` tree-kills the held child and spawns a new wsl.exe that loads ~30 GB.
    // As a SYNC command that ran inline, that whole body executed on the UI thread and froze the
    // window (test_ui_thread_blocking_audit.py). Same async + run_blocking shape as
    // `get_champion_engine_status` above.
    run_blocking(move || crate::engine_runtime::restart_current_champion(&app)).await
}

#[tauri::command]
pub fn list_agent_import_reports(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::runs::AgentImportReport>, String> {
    RATE_LIMITER.check("list_agent_import_reports")?;
    let db = state.lock_db();
    crate::runs::list_agent_import_reports(&db, limit).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_agent_stage_events(
    run_id: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::runs::AgentStageEvent>, String> {
    RATE_LIMITER.check("list_agent_stage_events")?;
    let db = state.lock_db();
    crate::runs::list_agent_stage_events(&db, run_id.as_deref(), limit).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn check_agentic_readiness(state: State<'_, AppState>) -> Result<AgenticReadiness, String> {
    RATE_LIMITER.check("check_agentic_readiness")?;
    // Grab the two cheap, lock-guarded inputs on the caller thread (a settings clone + a bounded
    // model-file stat), then run the SLOW part off the UI thread: external_provider_status shells out
    // to `wsl --status` (bounded at ~10s but that still froze the readiness poll). No lock is held
    // across the await — both guards are released before run_blocking.
    let settings = state.lock_settings().clone();
    let model_status = {
        let model_manager = state.lock_model_manager();
        model_manager.status()
    };
    run_blocking(move || {
        let external_provider = external_provider_status(&settings);
        Ok(build_agentic_readiness(&settings, &model_status, &external_provider))
    })
    .await
}

#[tauri::command]
pub fn get_escalation_queue(state: State<'_, AppState>, limit: usize) -> Result<Vec<crate::db::SpeechSegment>, String> {
    RATE_LIMITER.check("get_escalation_queue")?;
    let db = state.lock_db();
    db.get_escalation_queue(limit).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_escalation_rate_trend(state: State<'_, AppState>) -> Result<Vec<crate::jury::EscalationTrendPoint>, String> {
    RATE_LIMITER.check("get_escalation_rate_trend")?;
    let db = state.lock_db();
    crate::jury::get_escalation_rate_trend(&db).map_err(|e| e.to_string())
}
