//! Model registry, database maintenance, WSL refinement and acoustic-analysis commands.

use super::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatusV1 {
    /// True only when the warm server reports the exact id + deployment SHA selected by registry.
    pub ready: bool,
    pub port: u16,
    pub identity_matches: bool,
    pub expected_model_version_id: Option<String>,
    pub expected_deployment_sha256: Option<String>,
    pub loaded_model_version_id: Option<String>,
    pub loaded_deployment_sha256: Option<String>,
    pub reason: Option<String>,
}

/// Versioned renderer-safe registry row. The durable checkpoint path remains backend-only; the UI
/// needs the content identity and provenance, never a local filesystem location.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelVersionSummaryV1 {
    pub id: String,
    pub family: String,
    pub model_card_name: Option<String>,
    pub checkpoint_sha256: String,
    pub source: String,
    pub license: String,
    pub status: String,
}

impl From<crate::registry::ModelVersion> for ModelVersionSummaryV1 {
    fn from(version: crate::registry::ModelVersion) -> Self {
        Self {
            id: version.id,
            family: version.family,
            model_card_name: version.model_card_name,
            checkpoint_sha256: version.checkpoint_sha256,
            source: version.source,
            license: version.license,
            status: version.status,
        }
    }
}

fn model_registry_rate_limited_error() -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new("RATE_LIMITED", "The model registry is busy. Retry in a moment.", true)
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
}

fn public_model_registry_error(_private_detail: &str) -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(
        "MODEL_REGISTRY_READ_FAILED",
        "The model registry could not be read. Open Models or Health for recovery options.",
        false,
    )
    .suggested(crate::ipc_contract::SuggestedActionV1::OpenModels)
}

const MODEL_MUTATION_RESTORE_BLOCKED: &str = "MODEL_MUTATION_RESTORE_BLOCKED";

fn invalid_model_import_error(field: &str) -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(
        "INVALID_MODEL_IMPORT",
        "The model import request is invalid. Check the highlighted model field.",
        false,
    )
    .suggested(crate::ipc_contract::SuggestedActionV1::OpenModels)
    .detail("field", field)
}

fn validate_model_identifier(value: &str, field: &str) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    validate::validate_identifier(value).map_err(|_| invalid_model_import_error(field))
}

fn validate_deployment_request(
    manifest_path: &str,
    expected_deployment_sha256: &str,
    expected_model_id: &str,
    license: &str,
) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    validate_model_identifier(expected_model_id, "expectedModelId")?;
    validate_model_identifier(license, "license")?;
    if expected_deployment_sha256.len() != 64
        || !expected_deployment_sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_model_import_error("expectedDeploymentSha256"));
    }
    if manifest_path.trim().is_empty() || manifest_path.len() > 4096 || manifest_path.chars().any(char::is_control) {
        return Err(invalid_model_import_error("manifestPath"));
    }
    Ok(())
}

fn public_model_write_error(code: &str, message: &str, private_detail: &str) -> crate::ipc_contract::CommandErrorV1 {
    if private_detail == MODEL_MUTATION_RESTORE_BLOCKED {
        return crate::ipc_contract::CommandErrorV1::new(
            "RESTORE_IN_PROGRESS",
            "Model changes are unavailable while database recovery is in progress. Retry afterward.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry);
    }
    crate::ipc_contract::CommandErrorV1::new(code, message, false)
        .suggested(crate::ipc_contract::SuggestedActionV1::OpenModels)
}

/// The model registry, newest-first within each family — what a registry panel lists.
#[tauri::command]
#[specta::specta]
pub fn list_model_versions(
    state: State<'_, AppState>,
) -> Result<Vec<ModelVersionSummaryV1>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("list_model_versions").map_err(|_| model_registry_rate_limited_error())?;
    let db = state.lock_db();
    crate::registry::list_model_versions(&db)
        .map(|versions| {
            versions
                .into_iter()
                .filter(|version| version.family == crate::deployment::OMNIASR_7B_FAMILY)
                .map(ModelVersionSummaryV1::from)
                .collect()
        })
        .map_err(|error| public_model_registry_error(&error.to_string()))
}

#[cfg(test)]
mod typed_model_registry_ipc_tests {
    use super::*;

    #[test]
    fn public_registry_rows_are_camel_case_and_failures_scrub_backend_details() {
        let row = ModelVersionSummaryV1 {
            id: "candidate-1".into(),
            family: "omniasr-7b".into(),
            model_card_name: Some("owner-card".into()),
            checkpoint_sha256: "a".repeat(64),
            source: "owner-finetune".into(),
            license: "Apache-2.0".into(),
            status: "candidate".into(),
        };
        let wire = serde_json::to_value(row).expect("serialize public registry row");
        assert_eq!(wire["modelCardName"], "owner-card");
        assert_eq!(wire["checkpointSha256"], "a".repeat(64));
        assert!(wire.get("model_card_name").is_none());

        let error = public_model_registry_error(r"SQL D:\private\registry.db token=secret");
        let wire = serde_json::to_string(&error).expect("serialize public registry error");
        assert!(wire.contains("MODEL_REGISTRY_READ_FAILED"));
        assert!(wire.contains("openModels"));
        for forbidden in ["SQL", "D:\\", "private", "token", "secret"] {
            assert!(!wire.contains(forbidden));
        }
    }

    #[test]
    fn model_write_validation_and_failures_are_closed_typed_and_renderer_safe() {
        let invalid = validate_deployment_request("", "NOT-A-SHA", "bad id", "")
            .expect_err("invalid deployment request must refuse before worker admission");
        let invalid = serde_json::to_value(invalid).expect("serialize validation refusal");
        assert_eq!(invalid["code"], "INVALID_MODEL_IMPORT");
        assert_eq!(invalid["details"]["field"], "expectedModelId");

        let restore = public_model_write_error(
            "MODEL_DEPLOYMENT_IMPORT_FAILED",
            "The deployment could not be verified and registered. Open Models for recovery options.",
            MODEL_MUTATION_RESTORE_BLOCKED,
        );
        assert_eq!(restore.code, "RESTORE_IN_PROGRESS");
        assert!(restore.retryable);

        let hostile = public_model_write_error(
            "MODEL_DEPLOYMENT_IMPORT_FAILED",
            "The deployment could not be verified and registered. Open Models for recovery options.",
            r"manifest D:\private\deployment.json token=secret SQL mismatch",
        );
        let wire = serde_json::to_string(&hostile).expect("serialize model write failure");
        assert!(wire.contains("MODEL_DEPLOYMENT_IMPORT_FAILED"));
        for forbidden in ["deployment.json", "D:\\", "private", "token", "secret", "SQL", "mismatch"] {
            assert!(!wire.contains(forbidden));
        }
    }
}

/// Import an externally fine-tuned checkpoint into the registry as a gated candidate. The SHA is
/// computed server-side from the file; the caller never supplies it. Promotion is a separate,
/// gated step (not exposed yet — it must run through the eval gate), so this can only ever add a
/// candidate, never crown a champion.
#[tauri::command]
#[specta::specta]
pub async fn import_model_checkpoint(
    id: String,
    checkpoint_path: String,
    source: String,
    license: String,
    model_card_name: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER.check("import_model_checkpoint").map_err(|_| model_registry_rate_limited_error())?;
    validate_model_identifier(&id, "id")?;
    validate_model_identifier(&source, "source")?;
    validate_model_identifier(&license, "license")?;
    if let Some(ref card) = model_card_name {
        validate::validate_text(card, 256, "model_card_name")
            .map_err(|_| invalid_model_import_error("modelCardName"))?;
    }
    let checkpoint_path =
        validate::validate_file_path(&checkpoint_path).map_err(|_| invalid_model_import_error("checkpointPath"))?;
    let database = state.db_runtime();
    run_blocking(move || {
        // Own the restore fence in the worker itself. Cancelling the async IPC must not detach the
        // multi-GB hash from the generation it will eventually mutate.
        let mutation = database.begin_mutation().map_err(|_| MODEL_MUTATION_RESTORE_BLOCKED.to_string())?;
        // Hash the (potentially multi-GB) checkpoint off the main thread AND before taking the DB lock
        // — holding the global db mutex across the full-file SHA-256 would starve every UI DB poll.
        let sha = crate::registry::hash_checkpoint(&checkpoint_path).map_err(|e| e.to_string())?;
        let db = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
        crate::registry::register_checkpoint(
            &db,
            &id,
            crate::deployment::OMNIASR_7B_FAMILY,
            &checkpoint_path,
            &source,
            &license,
            model_card_name,
            sha,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|error| {
        public_model_write_error(
            "MODEL_CHECKPOINT_IMPORT_FAILED",
            "The checkpoint could not be verified and registered. Open Models for recovery options.",
            &error,
        )
    })
}

/// Import a content-addressed OmniASR-7B deployment. Identity comes from the verified manifest,
/// never from renderer-supplied model/card fields, and all four behavior-determining components are
/// hashed before the DB lock is acquired.
#[tauri::command]
#[specta::specta]
pub async fn import_model_deployment(
    manifest_path: String,
    expected_deployment_sha256: String,
    expected_model_id: String,
    source: String,
    license: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ModelVersionSummaryV1, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER.check("import_model_deployment").map_err(|_| model_registry_rate_limited_error())?;
    validate_model_identifier(&source, "source")?;
    validate_deployment_request(&manifest_path, &expected_deployment_sha256, &expected_model_id, &license)?;
    let database = state.db_runtime();
    run_blocking(move || {
        // Manifest verification can take ten minutes. It and the final registry write are one
        // generation-bound mutation, and the guard must outlive cancellation of the async caller.
        let mutation = database.begin_mutation().map_err(|_| MODEL_MUTATION_RESTORE_BLOCKED.to_string())?;
        let verified = if manifest_path.starts_with('/') {
            let server = crate::engine_runtime::server_script_path(&app)
                .ok_or_else(|| "bundled cortex_7b_server.py verifier could not be resolved".to_string())?;
            crate::deployment::verify_deployment_manifest_wsl(
                &server,
                &manifest_path,
                &expected_deployment_sha256,
                &expected_model_id,
                std::time::Duration::from_secs(10 * 60),
            )
            .map_err(|error| error.to_string())?
        } else {
            let local = validate::validate_file_path(&manifest_path)?;
            let local = crate::deployment::verify_deployment_manifest(
                std::path::Path::new(&local),
                Some(&expected_deployment_sha256),
            )
            .map_err(|error| error.to_string())?;
            if local.manifest.model_id != expected_model_id {
                return Err(format!(
                    "deployment manifest model id '{}' does not match expectedModelId '{}'",
                    local.manifest.model_id, expected_model_id
                ));
            }
            local.record()
        };
        let db = database.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::registry::register_verified_deployment_record(&db, &verified, &source, &license)
            .map(ModelVersionSummaryV1::from)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| {
        public_model_write_error(
            "MODEL_DEPLOYMENT_IMPORT_FAILED",
            "The deployment could not be verified and registered. Open Models for recovery options.",
            &error,
        )
    })
}

/// One-time admission of the historically measured incumbent. This is deliberately a different
/// command from challenger import: the registry family must be completely empty and the verified
/// composite must match every owner-measured legacy pin. It cannot be reused for a future model.
#[tauri::command]
#[specta::specta]
pub async fn bootstrap_legacy_champion(
    manifest_path: String,
    expected_deployment_sha256: String,
    expected_model_id: String,
    license: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ModelVersionSummaryV1, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER.check("bootstrap_legacy_champion").map_err(|_| model_registry_rate_limited_error())?;
    validate_deployment_request(&manifest_path, &expected_deployment_sha256, &expected_model_id, &license)?;
    let database = state.db_runtime();
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| {
        crate::ipc_contract::CommandErrorV1::new(
            "MODEL_STATE_UNAVAILABLE",
            "The application data location is unavailable. Open Health for recovery options.",
            false,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::OpenHealth)
    })?;
    run_blocking(move || {
        // Champion publication spans external verification, a registry transaction, and an atomic
        // pointer update. Never allow a restore to split those across database generations.
        let mutation = database.begin_mutation().map_err(|_| MODEL_MUTATION_RESTORE_BLOCKED.to_string())?;
        let verified = if manifest_path.starts_with('/') {
            let server = crate::engine_runtime::server_script_path(&app)
                .ok_or_else(|| "bundled cortex_7b_server.py verifier could not be resolved".to_string())?;
            crate::deployment::verify_deployment_manifest_wsl(
                &server,
                &manifest_path,
                &expected_deployment_sha256,
                &expected_model_id,
                std::time::Duration::from_secs(10 * 60),
            )
            .map_err(|error| error.to_string())?
        } else {
            let local = validate::validate_file_path(&manifest_path)?;
            let local = crate::deployment::verify_deployment_manifest(
                std::path::Path::new(&local),
                Some(&expected_deployment_sha256),
            )
            .map_err(|error| error.to_string())?;
            if local.manifest.model_id != expected_model_id {
                return Err("legacy deployment model id does not match expectedModelId".into());
            }
            local.record()
        };
        let db = database.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| poisoned.into_inner());
        let model = crate::registry::bootstrap_verified_legacy_deployment(&db, &verified, &license)
            .map_err(|error| error.to_string())?;
        crate::registry::sync_champion_pointer(&db, &data_dir).map_err(|error| error.to_string())?;
        Ok(ModelVersionSummaryV1::from(model))
    })
    .await
    .map_err(|error| {
        public_model_write_error(
            "LEGACY_CHAMPION_BOOTSTRAP_FAILED",
            "The pinned legacy champion could not be admitted. Open Models for recovery options.",
            &error,
        )
    })
}

/// Complete, non-lossy speaker inventory for the management panel. SQL NULL stays distinct from a
/// literal `unknown` id, and backend failures are reduced to a stable renderer-safe contract.
#[tauri::command]
#[specta::specta]
pub fn get_speaker_inventory_v1(
    state: State<'_, AppState>,
) -> Result<Vec<crate::ipc_contract::SpeakerInventoryItemV1>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_speaker_inventory_v1").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "RATE_LIMITED",
            "Too many speaker inventory requests. Retry in a moment.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?;
    state
        .segment_queries()
        .speaker_inventory()
        .map(|items| {
            items
                .into_iter()
                .map(|item| crate::ipc_contract::SpeakerInventoryItemV1 {
                    speaker_id: item.speaker_id,
                    segment_count: item.segment_count,
                    total_duration_seconds: item.total_duration_seconds,
                })
                .collect()
        })
        .map_err(|error| {
            let normalized = error.to_string().to_ascii_lowercase();
            if normalized.contains("database is locked") || normalized.contains("database is busy") {
                crate::ipc_contract::CommandErrorV1::new(
                    "DATABASE_BUSY",
                    "The workspace is busy. Retry loading speakers.",
                    true,
                )
                .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
            } else {
                crate::ipc_contract::CommandErrorV1::new(
                    "SPEAKER_INVENTORY_FAILED",
                    "The speaker inventory could not be loaded. Open Health for recovery options.",
                    false,
                )
                .suggested(crate::ipc_contract::SuggestedActionV1::OpenHealth)
            }
        })
}

fn history_rate_limited_error() -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new("RATE_LIMITED", "Too many history actions. Retry in a moment.", true)
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
}

fn history_restore_in_progress_error() -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(
        "RESTORE_IN_PROGRESS",
        "History actions are unavailable while database recovery is in progress. Retry afterward.",
        true,
    )
    .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
}

fn public_history_error(action: &str, error: &str) -> crate::ipc_contract::CommandErrorV1 {
    let lower = error.to_ascii_lowercase();
    if lower.contains("database is locked") || lower.contains("database is busy") {
        return crate::ipc_contract::CommandErrorV1::new(
            "DATABASE_BUSY",
            "The workspace is busy. Retry this history action.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry);
    }
    let (code, message) = if action == "redo" {
        ("REDO_FAILED", "The last change could not be redone.")
    } else {
        ("UNDO_FAILED", "The last change could not be undone.")
    };
    crate::ipc_contract::CommandErrorV1::new(code, message, false)
        .suggested(crate::ipc_contract::SuggestedActionV1::OpenHealth)
}

fn history_status(history: &crate::history::HistoryManager) -> crate::ipc_contract::HistoryStatusV1 {
    crate::ipc_contract::HistoryStatusV1 {
        undo_action: history.undo_action().map(Into::into),
        redo_action: history.redo_action().map(Into::into),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn undo(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<crate::ipc_contract::HistoryMutationResultV1, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("undo").map_err(|_| history_rate_limited_error())?;
    let database = state.db_runtime();
    let history = state.history_arc_for_restore();
    let worker_app = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mutation = database.begin_mutation().map_err(|_| history_restore_in_progress_error())?;
        let database_guard = database.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| poisoned.into_inner());
        let history = history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let action = history.undo(&database_guard).map_err(|error| public_history_error("undo", &error.to_string()))?;
        let status = history_status(&history);
        drop(history);
        drop(database_guard);
        if action.is_some() {
            if let Some(app_state) = worker_app.try_state::<AppState>() {
                app_state.session_auto_save();
            }
        }
        Ok::<_, crate::ipc_contract::CommandErrorV1>(crate::ipc_contract::HistoryMutationResultV1 {
            action: action.map(Into::into),
            status,
        })
    })
    .await
    .map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "UNDO_WORKER_FAILED",
            "The Undo worker stopped unexpectedly. Retry the action.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })??;
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub async fn redo(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<crate::ipc_contract::HistoryMutationResultV1, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("redo").map_err(|_| history_rate_limited_error())?;
    let database = state.db_runtime();
    let history = state.history_arc_for_restore();
    let worker_app = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mutation = database.begin_mutation().map_err(|_| history_restore_in_progress_error())?;
        let database_guard = database.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| poisoned.into_inner());
        let history = history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let action = history.redo(&database_guard).map_err(|error| public_history_error("redo", &error.to_string()))?;
        let status = history_status(&history);
        drop(history);
        drop(database_guard);
        if action.is_some() {
            if let Some(app_state) = worker_app.try_state::<AppState>() {
                app_state.session_auto_save();
            }
        }
        Ok::<_, crate::ipc_contract::CommandErrorV1>(crate::ipc_contract::HistoryMutationResultV1 {
            action: action.map(Into::into),
            status,
        })
    })
    .await
    .map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "REDO_WORKER_FAILED",
            "The Redo worker stopped unexpectedly. Retry the action.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })??;
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub fn get_history_status_v1(
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::HistoryStatusV1, crate::ipc_contract::CommandErrorV1> {
    Ok(history_status(&state.lock_history()))
}

#[cfg(test)]
mod typed_history_ipc_tests {
    use super::*;

    #[test]
    fn history_errors_are_stable_typed_and_scrubbed() {
        let private = public_history_error("undo", r#"SQL failed at C:\private\library.db: token=secret"#);
        let json = serde_json::to_value(private).expect("serialize public history error");
        assert_eq!(json["schema"], 1);
        assert_eq!(json["code"], "UNDO_FAILED");
        assert_eq!(json["retryable"], false);
        assert_eq!(json["suggestedAction"], "openHealth");
        let wire = json.to_string();
        assert!(!wire.contains("SQL"));
        assert!(!wire.contains("private"));
        assert!(!wire.contains("secret"));

        let busy = public_history_error("redo", "database is busy");
        let busy_json = serde_json::to_value(busy).expect("serialize busy history error");
        assert_eq!(busy_json["code"], "DATABASE_BUSY");
        assert_eq!(busy_json["retryable"], true);

        let restore = serde_json::to_value(history_restore_in_progress_error()).expect("serialize restore error");
        assert_eq!(restore["code"], "RESTORE_IN_PROGRESS");
        assert_eq!(restore["retryable"], true);
        assert_eq!(busy_json["suggestedAction"], "retry");
    }
}

#[tauri::command]
pub fn db_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let db = state.lock_db();
    db.info().map_err(|e| e.to_string())
}

// Compatibility re-exports keep established command/subcommand paths stable while process-level
// connection ownership and restore admission live behind DatabaseRuntime.
pub(crate) use crate::database_runtime::{
    begin_mutation, restore_pending, RestoreReservation, RESTORE_IN_PROGRESS_MSG,
};

#[tauri::command]
#[specta::specta]
pub fn cancel_operation(state: State<'_, AppState>) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    state.cancel_current_operation();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_inference_stats() -> Result<crate::ipc_contract::InferenceStatsV1, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_inference_stats").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "RATE_LIMITED",
            "The inference diagnostics are busy. Retry in a moment.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?;
    Ok(crate::inference::get_inference_stats().into())
}

pub(super) const WSL_LOG_LINE_PREVIEW_CHARS: usize = 4096;

/// True while a batch 7B refinement run is in flight. A plain flag (not a child handle) because the
/// batch drives the per-segment warm client in a loop — there is no single long-lived child to hold.
/// Guards against a second concurrent batch starting on top of the first.
pub(crate) static WSL_REFINE_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Set by `cancel_wsl_refinement`; polled between segments by the batch loop AND in-flight by the
/// per-segment spawn so a cancel stops the run within ~50 ms. Reset to false when a new batch starts.
static WSL_REFINE_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Clears the batch flags on drop so they reset even if the worker thread panics mid-batch.
/// Resetting CANCEL here (at run END) — rather than at run start — means a new run never needs a
/// start-of-run reset that could clobber a cancel racing the claim, and a late cancel can't leak
/// into the next run.
struct WslRefineRunningGuard;
impl Drop for WslRefineRunningGuard {
    fn drop(&mut self) {
        WSL_REFINE_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
        WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(super) fn wsl_log_preview(line: &str) -> String {
    let mut chars = line.chars();
    let mut preview: String = chars.by_ref().take(WSL_LOG_LINE_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        preview.push_str(" [truncated WSL log line]");
    }
    preview
}

/// A segment needs (re)transcription by the 7B batch when it has no usable transcript yet — empty or
/// any placeholder (`[Pending …]`, `[ASR unavailable …]`, `n/a`, `null`). Uses the same predicate as
/// the rest of the app (`quality::is_placeholder_transcript`) so the batch recovers an import that
/// failed under the local CTC engine too, not just the 7B-primary "[Pending]" case. We never target
/// a segment that already has a real transcript, so the batch can't clobber good CTC output (and
/// `update_asr_transcript_if_unreviewed` additionally refuses to overwrite a human decision).
pub(super) fn segment_awaits_wsl7b(raw_transcript: &str) -> bool {
    let trimmed = raw_transcript.trim();
    trimmed.is_empty() || crate::quality::is_placeholder_transcript(trimmed)
}

/// Within-file ordering key: the chunk's source start offset (ms) parsed from `alignment_json`, or 0
/// when absent. Segments from one import share a 1-second `created_at` and are tie-broken only by a
/// random UUID, so without this the batch would process an arbitrary chunk first.
fn segment_chunk_offset_ms(segment: &crate::db::SpeechSegment) -> i64 {
    segment
        .alignment_json
        .as_deref()
        .and_then(crate::chunking::SegmentSourceMeta::from_alignment_json)
        .map(|meta| meta.source_start_ms)
        .unwrap_or(0)
}

/// Select which segments the batch 7B refinement should transcribe, honoring the panel's limits.
/// Pure (no I/O) so it is unit-testable. Drains the backlog deterministically oldest-first and, WITHIN
/// one import (segments sharing a `created_at`), in chunk order (source start offset) so `test_one`
/// and capped runs process the FIRST chunk rather than an arbitrary UUID-ordered one. `limit_files`
/// caps distinct source files; `limit_segments` caps total segments; `test_one` overrides to a single
/// segment. Returns `(segment_id, audio_path)` pairs.
pub(super) fn select_wsl_refinement_targets(
    segments: &[crate::db::SpeechSegment],
    limit_files: Option<u32>,
    limit_segments: Option<u32>,
    test_one: bool,
) -> Vec<(String, String)> {
    // Pair each pending segment with its (parsed-once) chunk offset, then sort: oldest import first,
    // same file grouped, earliest chunk first, UUID only as a final stable tiebreak.
    let mut pending: Vec<(&crate::db::SpeechSegment, i64)> = segments
        .iter()
        .filter(|s| segment_awaits_wsl7b(&s.raw_transcript))
        .map(|s| (s, segment_chunk_offset_ms(s)))
        .collect();
    pending.sort_by(|(a, a_offset), (b, b_offset)| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.audio_path.cmp(&b.audio_path))
            .then_with(|| a_offset.cmp(b_offset))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut targets: Vec<(String, String)> =
        pending.iter().map(|(s, _)| (s.id.clone(), s.audio_path.clone())).collect();

    if let Some(max_files) = limit_files.map(|n| n as usize) {
        let mut kept_files: Vec<String> = Vec::new();
        targets.retain(|(_, path)| {
            if kept_files.iter().any(|p| p == path) {
                true
            } else if kept_files.len() < max_files {
                kept_files.push(path.clone());
                true
            } else {
                false
            }
        });
    }

    if test_one {
        targets.truncate(1);
    } else if let Some(max_segments) = limit_segments.map(|n| n as usize) {
        targets.truncate(max_segments);
    }

    targets
}

/// Drain a subprocess log stream line-by-line, decoding each line LOSSILY. `BufRead::lines()` yields
/// `io::Result<String>` and returns `Err(InvalidData)` for any non-UTF-8 line, so the previous
/// `lines().map_while(Result::ok)` permanently terminated the reader on the first such line —
/// silently freezing the live WSL progress feed for the rest of a (possibly hour-long) run on a
/// distro with a non-UTF-8 locale. Reading raw bytes and decoding with `from_utf8_lossy` survives
/// any input (invalid bytes become U+FFFD) so every subsequent line still reaches the feed. The
/// trailing `\r` of a `\r\n` line is trimmed. Retained (with its regression test) as the canonical
/// subprocess-log drainer; the current per-segment warm-client batch streams progress directly, so
/// it has no caller today — kept (allow(dead_code), paired with `join_wsl_log_reader`) so the
/// subprocess log path can be restored without re-deriving the non-UTF-8 contract.
#[allow(dead_code)]
pub(super) fn drain_log_lines<R: std::io::BufRead>(reader: R, mut on_line: impl FnMut(&str)) {
    for line in reader.split(b'\n') {
        let Ok(bytes) = line else { break }; // genuine I/O error (not an encoding error): stop
        let text = String::from_utf8_lossy(&bytes);
        on_line(text.trim_end_matches('\r'));
    }
}

// Join a WSL subprocess log-reader thread, warning (never panicking) if it unwound. Paired with
// drain_log_lines for the subprocess-spawning log path; the per-segment warm-client batch supersedes
// the in-commands subprocess driver, so this currently has no caller here — kept (allow(dead_code))
// so the subprocess path can be restored without re-deriving it.
#[allow(dead_code)]
fn join_wsl_log_reader(thread: std::thread::JoinHandle<()>, stream: &str) {
    if thread.join().is_err() {
        tracing::warn!("WSL {stream} log reader thread panicked");
    }
}

#[tauri::command]
#[specta::specta]
pub fn run_wsl_refinement(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    limit_files: Option<u32>,
    limit_segments: Option<u32>,
    dry_run: bool,
    test_one: bool,
) -> Result<crate::ipc_contract::WslRefinementStartedV1, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER
        .check("run_wsl_refinement")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("run_wsl_refinement"))?;
    let result = (|| -> Result<(), String> {
        // P1.3b: don't start the 7B refinement loop (a background DB writer) while a restore is reserved.
        if restore_pending() {
            return Err(RESTORE_IN_PROGRESS_MSG.into());
        }

        // Single-run guard: claim the running flag atomically. If it was already true, a batch is in
        // flight — refuse rather than starting a second concurrent loop over the same segments.
        if WSL_REFINE_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err("WSL 7B refinement batch transcription is already running.".into());
        }
        // P1.3b (publish-then-recheck): the running flag is now PUBLISHED; re-read the reservation. This
        // closes the atomic check-then-set race with prepare_restore (which sets RESTORE_PENDING then reads
        // this flag via writers_active): either it already observed our flag (the fence refuses the restore),
        // or we observe its reservation here and roll back — the two orderings can no longer both slip.
        if restore_pending() {
            WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
            return Err(RESTORE_IN_PROGRESS_MSG.into());
        }
        // The running flag is now OURS; every early return below MUST clear it or the guard would wedge.
        // Reset CANCEL at the START of the run (standard cancellation-token pattern) rather than trusting
        // the previous run's guard to have cleared it. The guard clears CANCEL then RUNNING as two separate
        // atomic stores, so a `cancel` that read RUNNING==true just before the guard could set CANCEL=true
        // AFTER the guard cleared it — leaking a stale cancel that would make THIS fresh batch abort
        // immediately, doing zero work, with no error surfaced. Clearing it here, now that RUNNING is
        // exclusively ours, drops that leaked value. (The only residual is a cancel landing in the tiny
        // window between the claim above and this store; that is user-recoverable by clicking cancel again,
        // whereas the leak was silent and unrecoverable.)
        WSL_REFINE_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);

        // Read everything the worker needs under the locks NOW, then release them so the long per-segment
        // loop holds no AppState lock. A 7B call can take seconds; holding a lock across the loop would
        // freeze the UI's get_segments exactly like the jury-starvation bug we already fixed. The
        // poison-recovering lock_* accessors never panic.
        let setup = {
            let settings = state.lock_settings();
            let external_script = settings.external_asr_script_path();
            let auto_normalize = settings.auto_normalize;
            let verbalize_numbers = settings.verbalize_numbers;
            drop(settings);
            external_script.map(|script| (script, auto_normalize, verbalize_numbers))
        };
        let (external_script, auto_normalize, verbalize_numbers) = match setup {
            Some(values) => values,
            None => {
                WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
                return Err("External ASR provider script is not configured in Settings.".into());
            }
        };
        let db_path = state.lock_pipeline().db_path().to_string();

        // Builder::spawn returns Err on OS thread-creation failure instead of PANICKING like thread::spawn,
        // so a failed spawn can't leave WSL_REFINE_RUNNING wedged true (the RAII guard lives inside the
        // closure and would never run on a spawn panic).
        let spawned = std::thread::Builder::new().name("wsl-7b-batch".into()).spawn(move || {
        // Clears WSL_REFINE_RUNNING + WSL_REFINE_CANCEL on every exit path, including a panic.
        let _running = WslRefineRunningGuard;
        // catch_unwind so a panic in the loop still emits a terminal wsl-status — otherwise the panel
        // would stay wedged at "Processing…" forever (it only clears `running` on a wsl-status event).
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_wsl_refinement_loop(
                &app,
                &db_path,
                &external_script,
                auto_normalize,
                verbalize_numbers,
                limit_files,
                limit_segments,
                dry_run,
                test_one,
            )
        }));
        // Carry transcribed AND failed so the UI can be honest: a run with any failures is reported
        // "completed" with failed>0 (not a clean green success), and an all-failed run is "failed".
        let (status, transcribed, failed, exit_code) = match outcome {
            Ok(Ok(summary)) if summary.cancelled => {
                ("cancelled", summary.transcribed as i64, summary.failed as i64, summary.transcribed as i64)
            }
            Ok(Ok(summary)) if summary.transcribed == 0 && summary.failed > 0 => {
                ("failed", 0, summary.failed as i64, -1)
            }
            Ok(Ok(summary)) => ("completed", summary.transcribed as i64, summary.failed as i64, summary.transcribed as i64),
            Ok(Err(message)) => {
                emit_or_log(&app, "wsl-log", format!("[ERROR] {}", wsl_log_preview(&message)));
                ("failed", 0, 0, -1)
            }
            Err(_panic) => {
                emit_or_log(&app, "wsl-log", "[ERROR] WSL 7B batch worker panicked; the run was aborted.".to_string());
                ("failed", 0, 0, -1)
            }
        };
        emit_or_log(
            &app,
            "wsl-status",
            serde_json::json!({ "status": status, "transcribed": transcribed, "failed": failed, "exit_code": exit_code }),
        );
    });
        if let Err(error) = spawned {
            WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
            return Err(format!("Failed to start the WSL 7B batch worker thread: {error}"));
        }

        Ok(())
    })();
    result
        .map(|()| crate::ipc_contract::WslRefinementStartedV1 {
            status: crate::ipc_contract::WslRefinementStartStatusV1::Started,
        })
        .map_err(|error| {
            tracing::warn!("Owner WSL refinement start failed: {error}");
            crate::ipc_contract::public_wsl_refinement_error(&error)
        })
}

struct WslRefinementSummary {
    transcribed: usize,
    failed: usize,
    cancelled: bool,
}

/// The detached batch worker: drive the per-segment warm 7B client over every pending segment, write
/// each result through the human-decision-safe update, and stream progress as `wsl-log` events. No
/// AppState lock is held here — it owns its own DB connection opened from `db_path`.
#[allow(clippy::too_many_arguments)]
fn run_wsl_refinement_loop(
    app: &tauri::AppHandle,
    db_path: &str,
    external_script: &str,
    auto_normalize: bool,
    verbalize_numbers: bool,
    limit_files: Option<u32>,
    limit_segments: Option<u32>,
    dry_run: bool,
    test_one: bool,
) -> Result<WslRefinementSummary, String> {
    emit_or_log(
        app,
        "wsl-log",
        ">>> Driving the Meta OmniASR 7B warm client over pending segments (one --segment-id call each)...".to_string(),
    );

    // Worker connection (background thread): plain `open`, NOT open_with_retry — the boot-time-only
    // destructive quarantine must not be reachable from a live worker, and the DB was integrity-checked
    // at boot. `open` sets WAL + busy_timeout for contention.
    let db = crate::db::Database::open(db_path).map_err(|e| e.to_string())?;
    // P1.3: the backlog, not the library. This used to read every segment ever imported and then throw
    // away every one that already had a transcript. The SQL prefilter is a deliberate SUPERSET of
    // `segment_awaits_wsl7b`, which stays the authority below — see PendingWork::Transcript.
    let candidates = db.get_pending_segments(crate::db::PendingWork::Transcript).map_err(|e| e.to_string())?;
    let targets = select_wsl_refinement_targets(&candidates, limit_files, limit_segments, test_one);

    if targets.is_empty() {
        emit_or_log(
            app,
            "wsl-log",
            ">>> No segments are awaiting 7B transcription (every segment already has a transcript). Nothing to do."
                .to_string(),
        );
        return Ok(WslRefinementSummary { transcribed: 0, failed: 0, cancelled: false });
    }

    let total = targets.len();
    emit_or_log(app, "wsl-log", format!(">>> {total} segment(s) awaiting 7B transcription."));

    if dry_run {
        for (idx, (id, path)) in targets.iter().enumerate() {
            let file = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path.as_str());
            emit_or_log(
                app,
                "wsl-log",
                format!("[dry-run] [{}/{}] would transcribe {} ({})", idx + 1, total, id, wsl_log_preview(file)),
            );
        }
        emit_or_log(app, "wsl-log", ">>> Dry run complete — no transcripts were written.".to_string());
        return Ok(WslRefinementSummary { transcribed: 0, failed: 0, cancelled: false });
    }

    let normalizer = crate::normalizer::SoraniNormalizer::with_config(crate::normalizer::NormalizationConfig {
        normalize_numbers: auto_normalize,
        verbalize_numbers,
        normalize_hamza: true,
        remove_diacritics: false,
    });

    let mut transcribed = 0usize;
    let mut failed = 0usize;
    // Reuse one verified immutable file lease for every segment cut from the same recording. This
    // keeps source bytes frozen for the whole batch without decoding a multi-hour source once per
    // chunk.
    let mut source_leases: std::collections::HashMap<(String, String), crate::media::VerifiedMediaSourceLease> =
        std::collections::HashMap::new();
    for (idx, (id, target_path)) in targets.iter().enumerate() {
        if WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::Relaxed) {
            emit_or_log(app, "wsl-log", format!(">>> Cancelled by user after {idx}/{total} segment(s)."));
            return Ok(WslRefinementSummary { transcribed, failed, cancelled: true });
        }
        emit_or_log(app, "wsl-log", format!("[{}/{}] transcribing {}...", idx + 1, total, id));

        let source_snapshot = match db.champion_transcription_source_snapshot(id) {
            Ok(Some(snapshot)) if snapshot.segment.audio_path == *target_path => snapshot,
            Ok(Some(_)) => {
                failed += 1;
                emit_or_log(
                    app,
                    "wsl-log",
                    format!(
                        "[ERROR] [{}/{}] {} source path changed after selection; reload the batch before transcribing",
                        idx + 1,
                        total,
                        id
                    ),
                );
                continue;
            }
            Ok(None) => {
                failed += 1;
                emit_or_log(app, "wsl-log", format!("[ERROR] [{}/{}] {} no longer exists", idx + 1, total, id));
                continue;
            }
            Err(error) => {
                failed += 1;
                emit_or_log(
                    app,
                    "wsl-log",
                    format!("[ERROR] [{}/{}] {} source snapshot failed: {}", idx + 1, total, id, error),
                );
                continue;
            }
        };
        let expected_hash = match source_snapshot.audio_content_hash.as_deref() {
            Some(hash) if crate::db::is_canonical_audio_content_hash(hash) => hash.to_string(),
            _ => {
                failed += 1;
                emit_or_log(
                    app,
                    "wsl-log",
                    format!(
                        "[ERROR] [{}/{}] {} has no canonical decoded-PCM identity; repair or re-import it before transcribing",
                        idx + 1,
                        total,
                        id
                    ),
                );
                continue;
            }
        };
        let lease_key = (source_snapshot.segment.audio_path.clone(), expected_hash.clone());
        let source_lease = match source_leases.get(&lease_key) {
            Some(lease) => lease.clone(),
            None => match crate::media::verify_current_source_lease(
                std::path::Path::new(&source_snapshot.segment.audio_path),
                &expected_hash,
            ) {
                Ok(lease) => {
                    source_leases.insert(lease_key, lease.clone());
                    lease
                }
                Err(error) => {
                    failed += 1;
                    emit_or_log(
                        app,
                        "wsl-log",
                        format!(
                            "[ERROR] [{}/{}] {} source identity changed: {}",
                            idx + 1,
                            total,
                            id,
                            wsl_log_preview(&error)
                        ),
                    );
                    continue;
                }
            },
        };
        match crate::pipeline::run_wsl_segment_transcript_with_script(
            external_script,
            id,
            db_path,
            Some(&WSL_REFINE_CANCEL),
        ) {
            Ok(result) if result.raw_transcript.trim().is_empty() => {
                // A blank 7B result (silent/music/noise clip — parse_wsl_segment_result returns Ok(""))
                // must NOT overwrite an existing good transcript: update_asr_transcript_if_unreviewed
                // writes raw_transcript unconditionally (guarding only human-reviewed rows). Skip; keep the
                // current text. Neither transcribed nor failed, like the human-reviewed skip below.
                // (blank-transcript-never-overwrites-good; sibling of transcribe_segment / batch_transcribe.)
                emit_or_log(
                    app,
                    "wsl-log",
                    format!(
                        "[{}/{}] {id} produced an empty transcript (silent clip) — existing transcript kept",
                        idx + 1,
                        total
                    ),
                );
            }
            Ok(result) => {
                let raw_transcript = result.raw_transcript;
                let confidence = result.confidence;
                let normalized = if auto_normalize && !raw_transcript.is_empty() {
                    Some(normalizer.normalize(&raw_transcript))
                } else {
                    None
                };
                let normalizer_version = normalized.as_ref().map(|_| crate::normalizer::NORMALIZER_VERSION);
                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct WslRefinementCommitConfigV1<'a> {
                    schema: u8,
                    protocol: &'static str,
                    build_git_sha: &'static str,
                    model_version_id: &'a str,
                    deployment_sha256: &'a str,
                    auto_normalize: bool,
                    verbalize_numbers: bool,
                    normalizer_version: Option<&'static str>,
                }
                let decoder_config_sha256 = canonical_batch_config_sha256(&WslRefinementCommitConfigV1 {
                    schema: 1,
                    protocol: "wsl-refinement-commit-v1",
                    build_git_sha: crate::GIT_SHA,
                    model_version_id: &result.model_version_id,
                    deployment_sha256: &result.deployment_sha256,
                    auto_normalize,
                    verbalize_numbers,
                    normalizer_version,
                })?;
                let champion = crate::db::SegmentHypothesis {
                    segment_id: id.to_string(),
                    model_id: result.model_version_id,
                    transcript: raw_transcript.clone(),
                    confidence,
                };
                match db.commit_bound_champion_transcript_if_unreviewed(
                    &champion,
                    Some(&result.deployment_sha256),
                    normalized.as_deref(),
                    Some("external_provider"),
                    false,
                    &decoder_config_sha256,
                    normalizer_version,
                    &source_snapshot,
                ) {
                    Ok(true) => {
                        transcribed += 1;
                        emit_or_log(
                            app,
                            "wsl-log",
                            format!("[{}/{}] {} -> {}", idx + 1, total, id, wsl_log_preview(raw_transcript.trim())),
                        );
                    }
                    Ok(false) => emit_or_log(
                        app,
                        "wsl-log",
                        format!("[{}/{}] {} skipped (human-reviewed; transcript not overwritten)", idx + 1, total, id),
                    ),
                    Err(error) => {
                        failed += 1;
                        emit_or_log(
                            app,
                            "wsl-log",
                            format!(
                                "[ERROR] [{}/{}] {} db write failed: {}",
                                idx + 1,
                                total,
                                id,
                                wsl_log_preview(&error.to_string())
                            ),
                        );
                    }
                }
            }
            Err(error) => {
                // A cancel mid-clip surfaces here as an error from the spawn; attribute it to the
                // cancel, not to a failure, and stop the run.
                if WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::Relaxed) {
                    emit_or_log(app, "wsl-log", format!(">>> Cancelled by user during segment {}/{}.", idx + 1, total));
                    return Ok(WslRefinementSummary { transcribed, failed, cancelled: true });
                }
                failed += 1;
                emit_or_log(
                    app,
                    "wsl-log",
                    format!("[ERROR] [{}/{}] {}: {}", idx + 1, total, id, wsl_log_preview(&error.to_string())),
                );
            }
        }
        // Keep the exact imported file object immutable through the compare-and-swap commit above.
        drop(source_lease);
    }

    // A cancel that arrives during the FINAL segment passes every in-loop check (there is no next
    // iteration); re-check once here so it is honestly reported as cancelled, not completed.
    if WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::Relaxed) {
        emit_or_log(app, "wsl-log", format!(">>> Cancelled by user; {transcribed} transcribed before stopping."));
        return Ok(WslRefinementSummary { transcribed, failed, cancelled: true });
    }

    emit_or_log(
        app,
        "wsl-log",
        format!(">>> Complete! {transcribed} transcribed, {failed} failed of {total} pending."),
    );
    Ok(WslRefinementSummary { transcribed, failed, cancelled: false })
}

#[tauri::command]
#[specta::specta]
pub fn cancel_wsl_refinement() -> Result<(), crate::ipc_contract::CommandErrorV1> {
    // Only arm the cancel while a batch is actually running, so an idle cancel can't leak into and
    // immediately abort the NEXT run. Signals the batch loop (checked between segments) and the
    // in-flight per-segment spawn (which polls this same flag and kills its child) to stop; there is
    // no single child handle to kill here — each per-segment child is owned and reaped in the helper.
    if WSL_REFINE_RUNNING.load(std::sync::atomic::Ordering::SeqCst) {
        WSL_REFINE_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
pub async fn compute_acoustic_scores(state: State<'_, AppState>) -> Result<usize, String> {
    RATE_LIMITER.check("compute_acoustic_scores")?;
    let settings_gpu = {
        let s = state.lock_settings();
        s.enable_gpu
    };
    let models_dir = state.lock_model_manager().models_dir.clone();
    let database = state.db_runtime();
    run_blocking(move || {
        let mutation = database.begin_mutation()?;
        // P1.3: `WHERE ctc_score IS NULL` instead of reading the whole library and `continue`-ing past
        // every row that already has one. After the first pass this returns nothing at all.
        let segments = {
            let db = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
            db.get_pending_segments(crate::db::PendingWork::CtcScore).map_err(|e| e.to_string())?
        };

        let aligner = aligner::ForcedAligner::new(&models_dir, settings_gpu).map_err(|e| e.to_string())?;

        if !aligner.is_available() {
            return Err("MMS Forced Aligner model (mms_aligner.onnx) is not available.".to_string());
        }

        let mut count = 0;
        for seg in &segments {
            let text = seg.raw_transcript.clone();
            if text.trim().is_empty() {
                continue;
            }

            let audio_path = seg.audio_path.clone();
            if !std::path::Path::new(&audio_path).exists() {
                tracing::warn!("Skipping acoustic score for {}: audio path not found: {}", seg.id, audio_path);
                continue;
            }

            let (sample_rate, pcm) = match audio::decode_to_pcm_with_timeout(&audio_path, Duration::from_secs(30)) {
                Ok(decoded) => decoded,
                Err(error) => {
                    tracing::warn!("Skipping acoustic score for {}: decode failed: {error}", seg.id);
                    continue;
                }
            };
            let (_sr, pcm_16k) = match audio::ensure_pcm_16khz(sample_rate, pcm) {
                Ok(resampled) => resampled,
                Err(error) => {
                    tracing::warn!("Skipping acoustic score for {}: 16 kHz conversion failed: {error}", seg.id);
                    continue;
                }
            };
            // Score only THIS segment's clip, not the whole source file. Segments share the source
            // audio_path (the per-segment range lives in alignment_json), so without slicing the acoustic
            // ctc_score — which feeds the conformal jury gate — would be computed over the ENTIRE recording
            // for every segment, a systematically wrong quality signal on any multi-segment import.
            let pcm_16k = match crate::chunking::slice_pcm_by_alignment(
                &pcm_16k,
                audio::TARGET_SAMPLE_RATE,
                seg.alignment_json.as_deref(),
            ) {
                Ok((clip, _)) => clip,
                Err(error) => {
                    tracing::warn!("Skipping acoustic score for {}: clip slice failed: {error}", seg.id);
                    continue;
                }
            };
            let score = match aligner.score_consistency(&pcm_16k, audio::TARGET_SAMPLE_RATE, &text) {
                Ok(score) => score,
                Err(error) => {
                    tracing::warn!("Skipping acoustic score for {}: scoring failed: {error}", seg.id);
                    continue;
                }
            };

            let guard = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
            guard.update_ctc_score(&seg.id, score).map_err(|e| e.to_string())?;
            count += 1;
        }

        Ok(count)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn compute_signal_anomaly_scores(
    state: State<'_, AppState>,
) -> Result<usize, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER
        .check("compute_signal_anomaly_scores")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("compute_signal_anomaly_scores"))?;
    let models_dir = state.lock_model_manager().models_dir.clone();
    let database = state.db_runtime();
    let result = run_blocking(move || {
        let mutation = database.begin_mutation().map_err(|error| {
            tracing::warn!("Owner signal-analysis admission failed: {error}");
            error
        })?;
        // P1.3: `WHERE signal_anomaly_score IS NULL` — see the CTC sibling above.
        let segments = {
            let db = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
            db.get_pending_segments(crate::db::PendingWork::SignalAnomaly).map_err(|e| e.to_string())?
        };

        let detector = quality::signal_anomaly::SignalAnomalyDetector::new(&models_dir).map_err(|e| e.to_string())?;

        let mut count = 0;
        for seg in &segments {
            let audio_path = seg.audio_path.clone();
            if !std::path::Path::new(&audio_path).exists() {
                continue;
            }

            let (sample_rate, pcm) = match audio::decode_to_pcm_with_timeout(&audio_path, Duration::from_secs(30)) {
                Ok(decoded) => decoded,
                Err(error) => {
                    tracing::warn!("Skipping signal-anomaly score for {}: decode failed: {error}", seg.id);
                    continue;
                }
            };
            let (_sr, pcm_16k) = match audio::ensure_pcm_16khz(sample_rate, pcm) {
                Ok(resampled) => resampled,
                Err(error) => {
                    tracing::warn!("Skipping signal-anomaly score for {}: 16 kHz conversion failed: {error}", seg.id);
                    continue;
                }
            };
            // Score only THIS segment's clip, not the whole source file (same whole-file-vs-clip hazard as
            // the acoustic-score loop): segments share the source audio_path, with the range in alignment_json.
            let pcm_16k = match crate::chunking::slice_pcm_by_alignment(
                &pcm_16k,
                audio::TARGET_SAMPLE_RATE,
                seg.alignment_json.as_deref(),
            ) {
                Ok((clip, _)) => clip,
                Err(error) => {
                    tracing::warn!("Skipping signal-anomaly score for {}: clip slice failed: {error}", seg.id);
                    continue;
                }
            };
            let score = match detector.compute_signal_anomaly_score(&pcm_16k) {
                Ok(score) => score,
                Err(error) => {
                    tracing::warn!("Skipping signal-anomaly score for {}: scoring failed: {error}", seg.id);
                    continue;
                }
            };

            let guard = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
            guard.update_signal_anomaly_score(&seg.id, score).map_err(|e| e.to_string())?;
            count += 1;
        }

        Ok(count)
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner signal-anomaly analysis failed: {error}");
        crate::ipc_contract::public_owner_analysis_error(
            crate::ipc_contract::OwnerAnalysisOperationV1::SignalAnomaly,
            &error,
        )
    })
}

#[cfg(test)]
mod system_ops_boundary_tests {
    use super::*;

    #[test]
    fn deployment_request_validation_gates_every_field_individually() {
        // A fully well-formed request must be admitted — the refusals below prove each gate, this
        // proves none of them over-reject.
        let good_sha = "0123abcd".repeat(8); // 64 chars, digits + lowercase a-f only
        validate_deployment_request("C:/models/deployment/manifest.json", &good_sha, "omniasr-7b_v2.0", "Apache-2.0")
            .expect("well-formed deployment request must pass validation");

        let field_of = |result: Result<(), crate::ipc_contract::CommandErrorV1>| {
            let error = result.expect_err("request must be refused");
            let wire = serde_json::to_value(error).expect("serialize refusal");
            assert_eq!(wire["code"], "INVALID_MODEL_IMPORT");
            wire["details"]["field"].as_str().expect("refusal names its field").to_string()
        };

        // SHA gate: wrong length, uppercase hex, and non-hex are all refused (lowercase-only law).
        let short_sha = "0123abcd".repeat(7);
        assert_eq!(
            field_of(validate_deployment_request("C:/m.json", &short_sha, "m", "MIT")),
            "expectedDeploymentSha256"
        );
        assert_eq!(
            field_of(validate_deployment_request("C:/m.json", &"A".repeat(64), "m", "MIT")),
            "expectedDeploymentSha256"
        );
        assert_eq!(
            field_of(validate_deployment_request("C:/m.json", &"g".repeat(64), "m", "MIT")),
            "expectedDeploymentSha256"
        );

        // Manifest path gate: empty after trim, control characters, and the 4096 length cap.
        assert_eq!(field_of(validate_deployment_request("   ", &good_sha, "m", "MIT")), "manifestPath");
        assert_eq!(field_of(validate_deployment_request("mani\nfest.json", &good_sha, "m", "MIT")), "manifestPath");
        assert_eq!(field_of(validate_deployment_request(&"x".repeat(4097), &good_sha, "m", "MIT")), "manifestPath");

        // Identifier gates fire before the SHA/path checks and name their own fields.
        assert_eq!(field_of(validate_deployment_request("C:/m.json", &good_sha, "", "MIT")), "expectedModelId");
        assert_eq!(field_of(validate_deployment_request("C:/m.json", &good_sha, "m", "not a license")), "license");
    }

    #[test]
    fn model_identifier_validation_accepts_repo_ids_and_refuses_path_shapes() {
        validate_model_identifier("omniasr-7b_v2.0", "id").expect("repo-style id must pass");
        for hostile in ["", "../evil", "a/b", "id with spaces"] {
            let error = validate_model_identifier(hostile, "id").expect_err("hostile id must be refused");
            assert_eq!(error.code, "INVALID_MODEL_IMPORT");
        }
    }

    #[test]
    fn rate_limited_errors_are_retryable_with_a_retry_suggestion() {
        for error in [model_registry_rate_limited_error(), history_rate_limited_error()] {
            let wire = serde_json::to_value(error).expect("serialize rate-limit error");
            assert_eq!(wire["code"], "RATE_LIMITED");
            assert_eq!(wire["retryable"], true);
            assert_eq!(wire["suggestedAction"], "retry");
        }
    }

    #[test]
    fn public_history_error_maps_locked_to_busy_and_redo_to_redo_failed() {
        // The sibling test covers "database is busy" and the undo arm; these are the other two arms.
        let locked = public_history_error("undo", "database is locked (5)");
        assert_eq!(locked.code, "DATABASE_BUSY");
        assert!(locked.retryable);

        let redo = public_history_error("redo", "constraint violation");
        let wire = serde_json::to_value(redo).expect("serialize redo failure");
        assert_eq!(wire["code"], "REDO_FAILED");
        assert_eq!(wire["retryable"], false);
        assert_eq!(wire["suggestedAction"], "openHealth");
        assert!(!wire.to_string().contains("constraint"), "backend detail must not reach the renderer");
    }

    #[test]
    fn engine_status_wire_shape_is_camel_case() {
        let status = EngineStatusV1 {
            ready: false,
            port: 8791,
            identity_matches: false,
            expected_model_version_id: Some("champion-1".into()),
            expected_deployment_sha256: Some("e".repeat(64)),
            loaded_model_version_id: None,
            loaded_deployment_sha256: None,
            reason: Some("identity mismatch".into()),
        };
        let wire = serde_json::to_value(status).expect("serialize engine status");
        assert_eq!(wire["identityMatches"], false);
        assert_eq!(wire["expectedModelVersionId"], "champion-1");
        assert!(wire.get("identity_matches").is_none());
        assert!(wire.get("expected_model_version_id").is_none());
    }

    #[test]
    fn segment_chunk_offset_ms_reads_the_alignment_offset_or_zero() {
        let segment = |alignment_json: Option<String>| crate::db::SpeechSegment {
            id: "seg".into(),
            audio_path: "source.wav".into(),
            alignment_json,
            ..crate::db::SpeechSegment::default()
        };
        // No metadata, unparseable metadata, and metadata missing source_start_ms all order as 0.
        assert_eq!(segment_chunk_offset_ms(&segment(None)), 0);
        assert_eq!(segment_chunk_offset_ms(&segment(Some("not json".into()))), 0);
        assert_eq!(segment_chunk_offset_ms(&segment(Some(r#"{"words":[]}"#.into()))), 0);
        let meta = crate::chunking::SegmentSourceMeta {
            source_start_ms: 1234,
            source_end_ms: 5678,
            chunk_index: 1,
            chunk_count: 4,
        };
        assert_eq!(segment_chunk_offset_ms(&segment(Some(meta.to_alignment_json()))), 1234);
    }

    #[test]
    fn history_status_reflects_real_undo_and_redo_stacks() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let history = crate::history::HistoryManager::new(16);

        let empty = history_status(&history);
        assert_eq!(empty.undo_action, None);
        assert_eq!(empty.redo_action, None);
        // Wire contract: the snapshot serializes camelCase for the renderer.
        let wire = serde_json::to_value(&empty).expect("serialize history status");
        assert!(wire.get("undoAction").is_some());
        assert!(wire.get("redo_action").is_none());

        let original = crate::db::SpeechSegment {
            id: "hs-1".into(),
            audio_path: "hs-1.wav".into(),
            raw_transcript: "machine before".into(),
            duration_ms: 1_000,
            ..crate::db::SpeechSegment::default()
        };
        db.insert_segment(&original).unwrap();
        let updated = crate::db::SpeechSegment { raw_transcript: "machine after".into(), ..original.clone() };
        db.insert_segment(&updated).unwrap();
        history.push(crate::history::Command::UpdateSegment {
            segment_id: updated.id.clone(),
            previous: Box::new(original),
            current: Box::new(updated),
        });

        let recorded = history_status(&history);
        assert_eq!(recorded.undo_action, Some(crate::ipc_contract::HistoryActionV1::UpdateSegment));
        assert_eq!(recorded.redo_action, None);

        // A real undo against the real row moves the entry to the redo stack and restores the text.
        history.undo(&db).unwrap().expect("one recorded action to undo");
        assert_eq!(db.get_segment_by_id("hs-1").unwrap().unwrap().raw_transcript, "machine before");
        let undone = history_status(&history);
        assert_eq!(undone.undo_action, None);
        assert_eq!(undone.redo_action, Some(crate::ipc_contract::HistoryActionV1::UpdateSegment));
    }

    #[test]
    fn cancel_wsl_refinement_only_arms_cancel_while_running_and_guard_clears_flags() {
        // One test owns both process-global flags end to end; nothing else in the suite touches them,
        // so the arms stay deterministic without cross-test coordination.
        WSL_REFINE_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        WSL_REFINE_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);

        // Idle arm: a cancel with no batch running must NOT arm the flag (it would abort the NEXT run).
        cancel_wsl_refinement().unwrap();
        assert!(!WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::SeqCst));

        // Running arm: with a batch in flight the cancel arms the flag the loop polls.
        WSL_REFINE_RUNNING.store(true, std::sync::atomic::Ordering::SeqCst);
        cancel_wsl_refinement().unwrap();
        assert!(WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::SeqCst));

        // The RAII guard resets both flags on drop — the panic-safe exit path of the batch worker.
        drop(WslRefineRunningGuard);
        assert!(!WSL_REFINE_RUNNING.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!WSL_REFINE_CANCEL.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn get_inference_stats_serves_the_typed_snapshot_without_state() {
        // This command takes no State, so the full path is testable; a fresh rate-limiter key admits
        // the first call.
        let stats = get_inference_stats().expect("first stats call is not rate limited");
        let wire = serde_json::to_value(stats).expect("serialize inference stats");
        assert!(wire.get("vad").is_some());
        assert!(wire.get("asr").is_some());
        assert!(wire["model_load_ms"].as_f64().is_some());
        assert!(wire["vad"]["p50_ms"].as_f64().is_some());
    }
}
