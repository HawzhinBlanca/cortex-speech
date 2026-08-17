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
use std::path::Path;
use tauri::State;

/// Bounded (~5s) health check of the champion 7B engine, for the UI status pill. Cheap + side-effect
/// free (a TCP probe), so the frontend can poll it.
#[tauri::command]
pub async fn get_champion_engine_status() -> EngineStatus {
    // The TCP probe blocks up to ~3s; run it on the blocking pool so the frontend's status-pill poll
    // never freezes the UI. A JoinError (panic in the probe) is unreachable here — degrade to
    // not-ready, which is the correct default for a health pill.
    run_blocking(move || {
        Ok(EngineStatus { ready: crate::pipeline::probe_wsl_7b_server(3), port: crate::pipeline::WSL_7B_SERVER_PORT })
    })
    .await
    .unwrap_or(EngineStatus { ready: false, port: crate::pipeline::WSL_7B_SERVER_PORT })
}

/// Start the champion 7B server (WSL) FROM THE APP so the owner never hand-runs a terminal. Spawns
/// the committed start script DETACHED and returns immediately; the UI then polls
/// get_champion_engine_status until ready (warm-up loads ~30 GB, 1-5 min). The script path comes from
/// CORTEX_7B_START_SCRIPT (the desktop launcher sets it); without it we return an actionable error
/// rather than guess a path.
#[tauri::command]
pub fn start_champion_engine() -> Result<(), String> {
    STRICT_RATE_LIMITER.check("start_champion_engine")?;
    let script = std::env::var("CORTEX_7B_START_SCRIPT").map_err(|_| {
        "Set CORTEX_7B_START_SCRIPT to the full path of scripts/start_7b_server.ps1 (the desktop \
         launcher sets it), or run that script once from a terminal."
            .to_string()
    })?;
    if !Path::new(&script).is_file() {
        return Err(format!("CORTEX_7B_START_SCRIPT does not point at a file: {script}"));
    }
    let mut cmd = std::process::Command::new("powershell");
    cmd.arg("-NoProfile").arg("-ExecutionPolicy").arg("Bypass").arg("-File").arg(&script);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // Detach: the start script waits up to 8 min for warm-up; do NOT block the command on it.
    cmd.spawn().map(|_child| ()).map_err(|e| format!("could not launch the engine start script: {e}"))
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
