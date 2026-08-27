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
    build_agentic_readiness, external_provider_status, run_blocking, AgenticReadiness, EngineStatusV1, RATE_LIMITER,
    STRICT_RATE_LIMITER,
};
use crate::ipc_contract::{CommandErrorV1, SuggestedActionV1};
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
    // The Inbox is a SERVING PATH: it plays these clips, mints playback receipts for them, and
    // records accept/edit/reject against them. So the voice focus governs it exactly as it governs
    // the review page and every phone queue — found by review 2026-08-20, when narrowing the review
    // page still left the Inbox handing out the guest clips the focus exists to skip.
    let focus = {
        let dir = state.lock_data_dir().clone();
        crate::voice_focus::resolve(dir.as_deref())?
    };
    let db = state.lock_db();
    db.get_escalation_queue(limit, focus.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_escalation_rate_trend(state: State<'_, AppState>) -> Result<Vec<crate::jury::EscalationTrendPoint>, String> {
    RATE_LIMITER.check("get_escalation_rate_trend")?;
    let db = state.lock_db();
    crate::jury::get_escalation_rate_trend(&db).map_err(|e| e.to_string())
}
