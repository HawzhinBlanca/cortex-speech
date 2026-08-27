//! Shipped model-management IPC. The production ASR is the separately provisioned WSL7B champion;
//! this surface manages only its supporting Silero/CAM++/denoiser artifacts. Optional ASR models
//! remain available solely to explicit offline diagnostic tools.

use super::{emit_or_log, run_blocking, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::ipc_contract::{CommandErrorV1, SuggestedActionV1};
use crate::models::ModelStatusEntryV1;
use crate::AppState;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadSummaryV1 {
    pub downloaded: u32,
    pub failed: u32,
    pub total: usize,
    pub skipped: usize,
}

fn model_rate_limited(message: &str) -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", message, true).suggested(SuggestedActionV1::Retry)
}

fn public_model_failure(code: &str, message: &str, _private_detail: &str) -> CommandErrorV1 {
    CommandErrorV1::new(code, message, true).suggested(SuggestedActionV1::OpenModels)
}

#[tauri::command]
#[specta::specta]
pub fn models_status(state: State<'_, AppState>) -> Result<Vec<ModelStatusEntryV1>, CommandErrorV1> {
    RATE_LIMITER
        .check("models_status")
        .map_err(|_| model_rate_limited("The support-model status is busy. Retry in a moment."))?;
    let mm = state.lock_model_manager();
    Ok(mm.production_status())
}

#[tauri::command]
#[specta::specta]
pub async fn models_download_all(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ModelDownloadSummaryV1, CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("models_download_all")
        .map_err(|_| model_rate_limited("Support-model download is already busy. Retry in a moment."))?;
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
            return Ok(ModelDownloadSummaryV1 { downloaded: 0, failed: 0, total: 0, skipped });
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

        Ok(ModelDownloadSummaryV1 { downloaded: succeeded, failed, total, skipped })
    })
    .await
    .map_err(|error| {
        public_model_failure(
            "MODEL_DOWNLOAD_FAILED",
            "Support-model download could not complete. Open Models and retry.",
            &error,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_download_contract_is_typed_camel_case_and_scrubs_private_failures() {
        let summary = ModelDownloadSummaryV1 { downloaded: 2, failed: 1, total: 3, skipped: 1 };
        let wire = serde_json::to_value(summary).expect("serialize download summary");
        assert_eq!(wire["downloaded"], 2);
        assert_eq!(wire["skipped"], 1);

        let hostile = public_model_failure(
            "MODEL_DOWNLOAD_FAILED",
            "Support-model download could not complete. Open Models and retry.",
            r"token=secret SQL D:\private\models\support.onnx",
        );
        let wire = serde_json::to_string(&hostile).expect("serialize public model failure");
        assert!(wire.contains("MODEL_DOWNLOAD_FAILED"));
        assert!(wire.contains("openModels"));
        for forbidden in ["secret", "SQL", "D:\\", "private", "support.onnx"] {
            assert!(!wire.contains(forbidden));
        }
    }
}
