//! Shipped model-management IPC. The production ASR is the separately provisioned WSL7B champion;
//! this surface manages only its supporting Silero/CAM++/denoiser artifacts. Optional ASR models
//! remain available solely to explicit offline diagnostic tools.

use super::{emit_or_log, run_blocking, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn models_status(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    RATE_LIMITER.check("models_status")?;
    let mm = state.lock_model_manager();
    Ok(mm.production_status())
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
        let all_missing_count = mm.missing_production_models().len();
        let missing = mm.downloadable_missing_production_models();
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
