//! Model-download IPC commands — slice 2 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES are unchanged: `commands.rs` re-exports this module
//! (`pub use model_download::*;`), so `lib.rs`'s invoke_handler still names `commands::models_download`
//! and the frontend's `invoke('models_download')` is untouched. These are the same functions that
//! lived in `commands.rs`, only relocated. (The module is named `model_download`, not `models`, to
//! avoid shadowing the top-level `crate::models` that `commands.rs` still references.)
//!
//! `models_download` / `models_download_all` are `async` + `run_blocking`: a multi-hundred-MB HTTP
//! fetch would otherwise freeze the UI thread. Progress is streamed via `emit_or_log`.

use super::{emit_or_log, run_blocking, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn models_status(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    RATE_LIMITER.check("models_status")?;
    let mm = state.lock_model_manager();
    Ok(mm.status())
}

#[tauri::command]
pub async fn models_download(filename: String, state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("models_download")?;
    let model = crate::models::MODELS
        .iter()
        .find(|m| m.filename == filename)
        .ok_or_else(|| format!("Unknown model filename: {filename}"))?;
    // Clone the manager (just a models_dir PathBuf) and DROP the AppState lock before the
    // multi-hundred-MB blocking download, so the model panel's status poll and the readiness /
    // acoustic-score checks aren't starved on lock_model_manager() for the whole download. The
    // download itself runs on the blocking pool (run_blocking) so it never freezes the UI thread.
    // `model` is a &'static ModelInfo (crate::models::MODELS is a const &'static slice), so it moves into
    // the task freely.
    let mm = state.lock_model_manager().clone();
    run_blocking(move || {
        mm.download_model(model, |progress| {
            tracing::debug!("Download {} progress: {:.0}%", model.name, progress * 100.0);
        })
    })
    .await
}

#[tauri::command]
pub async fn models_download_all(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("models_download_all")?;
    // Clone the manager and DROP the AppState lock before the long download loop — otherwise every
    // missing model is fetched (hundreds of MB each) with lock_model_manager() held the whole time,
    // starving the model panel's own progress poll and the readiness/score checks. The whole loop
    // (the mm queries + the blocking downloads + progress emits) runs on the blocking pool via
    // run_blocking so it never freezes the UI thread; `missing` borrows `mm` only INSIDE the task.
    let mm = state.lock_model_manager().clone();
    run_blocking(move || {
        let all_missing_count = mm.missing_models().len();
        let missing = mm.downloadable_missing_models();
        let total = missing.len();
        let skipped = all_missing_count.saturating_sub(total);

        if total == 0 {
            return Ok(serde_json::json!({"downloaded": 0, "failed": 0, "total": 0, "skipped": skipped}));
        }

        emit_or_log(
            &app,
            "model-download-progress",
            serde_json::json!({
                "type": "started", "total": total
            }),
        );

        let mut succeeded = 0u32;
        let mut failed = 0u32;

        for (i, model) in missing.iter().enumerate() {
            let name = model.name.to_string();
            let filename = model.filename.to_string();
            match mm.download_model(model, |progress| {
                emit_or_log(
                    &app,
                    "model-download-progress",
                    serde_json::json!({
                        "type": "progress", "current": i + 1, "total": total,
                        "filename": filename, "progress": progress, "status": format!("Downloading {}", name)
                    }),
                );
            }) {
                Ok(_) => succeeded += 1,
                Err(e) => {
                    tracing::error!("Failed to download {}: {e}", model.name);
                    failed += 1;
                }
            }
        }

        emit_or_log(
            &app,
            "model-download-progress",
            serde_json::json!({
                "type": "completed", "total": total, "succeeded": succeeded, "failed": failed
            }),
        );

        Ok(serde_json::json!({
            "downloaded": succeeded, "failed": failed, "total": total, "skipped": skipped
        }))
    })
    .await
}
