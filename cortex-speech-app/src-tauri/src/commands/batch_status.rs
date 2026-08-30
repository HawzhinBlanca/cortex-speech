//! Durable batch status discovery, response-loss reconciliation, and renderer acknowledgment.

use super::ingest::{canonical_batch_operation_id, invalid_batch_operation_id_error};
use super::*;
use crate::ipc_contract::{BatchRunStatusResponseV1, BatchRunStatusV1, CommandErrorV1, SuggestedActionV1};

#[tauri::command]
#[specta::specta]
pub async fn get_batch_run_status(
    operation_id: String,
    app: tauri::AppHandle,
) -> Result<BatchRunStatusResponseV1, CommandErrorV1> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        get_batch_run_status_blocking(operation_id, &state)
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "Durable batch-status worker stopped unexpectedly");
        CommandErrorV1::new(
            "BATCH_STATUS_WORKER_FAILED",
            "Batch state could not be checked. Retry; if it continues, open Health.",
            true,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    })?
}

pub(super) fn get_batch_run_status_blocking(
    operation_id: String,
    state: &AppState,
) -> Result<BatchRunStatusResponseV1, CommandErrorV1> {
    RATE_LIMITER.check("get_batch_run_status").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Batch recovery is busy. Wait a moment, then retry.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    let operation_id = canonical_batch_operation_id(&operation_id).map_err(|_| invalid_batch_operation_id_error())?;
    match state.batch_store().status(&operation_id) {
        Ok(Some(status)) => publishable_durable_batch_status(state, status).map_err(|error| {
            tracing::error!(%operation_id, %error, "Durable batch status failed public-contract validation");
            CommandErrorV1::new(
                "BATCH_EVIDENCE_INVALID",
                "Batch state could not be verified. Retry; if it continues, open Health.",
                false,
            )
            .suggested(SuggestedActionV1::OpenHealth)
        }),
        Ok(None) => {
            let (status, operation, outcome) = state.batch_run_admission(&operation_id);
            match status {
                crate::BatchRunAdmission::Rejected => Ok(BatchRunStatusResponseV1 {
                    operation_id,
                    operation: operation.map(Into::into),
                    status: BatchRunStatusV1::Rejected,
                    total: outcome.as_ref().map(|value| value.total),
                    outcome: outcome.map(Into::into),
                }),
                crate::BatchRunAdmission::Unknown => Ok(BatchRunStatusResponseV1 {
                    operation_id,
                    operation: None,
                    status: BatchRunStatusV1::Unknown,
                    total: None,
                    outcome: None,
                }),
                crate::BatchRunAdmission::Running => {
                    if let Some((operation, total)) = state.starting_batch_run(&operation_id) {
                        return Ok(BatchRunStatusResponseV1 {
                            operation_id,
                            operation: Some(operation.into()),
                            status: BatchRunStatusV1::Starting,
                            total: Some(total),
                            outcome: None,
                        });
                    }
                    // The first database read can observe `None` immediately before streamed
                    // admission publishes the journal and advances the tracker out of `Starting`.
                    // Re-read the exact durable operation once before treating that legal state
                    // transition as missing authority.
                    if let Some(status) = state.batch_store().status(&operation_id).map_err(|error| {
                        tracing::error!(%operation_id, %error, "Durable batch status re-read failed after start handoff");
                        CommandErrorV1::new(
                            "BATCH_EVIDENCE_INVALID",
                            "Batch state could not be verified. Retry; if it continues, open Health.",
                            true,
                        )
                        .suggested(SuggestedActionV1::OpenHealth)
                    })? {
                        return publishable_durable_batch_status(state, status).map_err(|error| {
                            tracing::error!(%operation_id, %error, "Durable batch status re-read failed public-contract validation");
                            CommandErrorV1::new(
                                "BATCH_EVIDENCE_INVALID",
                                "Batch state could not be verified. Retry; if it continues, open Health.",
                                false,
                            )
                            .suggested(SuggestedActionV1::OpenHealth)
                        });
                    }
                    tracing::error!(%operation_id, ?status, "Process-local batch state has no durable journal authority");
                    Err(CommandErrorV1::new(
                        "BATCH_EVIDENCE_INVALID",
                        "Batch state could not be verified. New batch work is blocked until Health is checked.",
                        false,
                    )
                    .suggested(SuggestedActionV1::OpenHealth))
                }
                crate::BatchRunAdmission::Settled => {
                    tracing::error!(%operation_id, ?status, "Settled process-local batch state has no durable journal authority");
                    Err(CommandErrorV1::new(
                        "BATCH_EVIDENCE_INVALID",
                        "Batch state could not be verified. New batch work is blocked until Health is checked.",
                        false,
                    )
                    .suggested(SuggestedActionV1::OpenHealth))
                }
            }
        }
        Err(error) => {
            tracing::error!(%operation_id, %error, "Durable batch status could not be read");
            Err(CommandErrorV1::new(
                "BATCH_EVIDENCE_INVALID",
                "Batch state could not be verified. Retry; if it continues, open Health.",
                true,
            )
            .suggested(SuggestedActionV1::OpenHealth))
        }
    }
}

/// The sole durable operation eligible for renderer remount adoption. This includes a just-settled
/// process-local run until the renderer explicitly acknowledges presenting its exact durable
/// outcome, closing the terminalization-between-discovery-calls race without replaying old results
/// after a full process restart.
#[tauri::command]
#[specta::specta]
pub async fn get_active_batch_run(app: tauri::AppHandle) -> Result<Option<BatchRunStatusResponseV1>, CommandErrorV1> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        get_active_batch_run_blocking(&state)
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "Active batch-status worker stopped unexpectedly");
        CommandErrorV1::new(
            "BATCH_STATUS_WORKER_FAILED",
            "Active batch state could not be checked. Retry; if it continues, open Health.",
            true,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    })?
}

fn get_active_batch_run_blocking(state: &AppState) -> Result<Option<BatchRunStatusResponseV1>, CommandErrorV1> {
    RATE_LIMITER.check("get_active_batch_run").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Batch recovery is busy. Wait a moment, then retry.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    match state.batch_store().active() {
        Ok(Some(status)) => publishable_durable_batch_status(state, status).map(Some).map_err(|error| {
            tracing::error!(%error, "Active durable batch failed public-contract validation");
            CommandErrorV1::new(
                "BATCH_EVIDENCE_INVALID",
                "Active batch state could not be verified. New batch work remains blocked.",
                false,
            )
            .suggested(SuggestedActionV1::OpenHealth)
        }),
        Ok(None) => {
            let Some((operation_id, _operation)) = state.adoptable_batch_run_identity() else {
                return Ok(None);
            };
            match state.batch_store().status(&operation_id) {
                Ok(Some(status)) => publishable_durable_batch_status(state, status).map(Some).map_err(|error| {
                    tracing::error!(%operation_id, %error, "Adoptable batch failed public-contract validation");
                    CommandErrorV1::new(
                        "BATCH_EVIDENCE_INVALID",
                        "Adoptable batch state could not be verified. New batch work remains blocked.",
                        false,
                    )
                    .suggested(SuggestedActionV1::OpenHealth)
                }),
                Ok(None) => {
                    if let Some((operation, total)) = state.starting_batch_run(&operation_id) {
                        return Ok(Some(BatchRunStatusResponseV1 {
                            operation_id,
                            operation: Some(operation.into()),
                            status: BatchRunStatusV1::Starting,
                            total: Some(total),
                            outcome: None,
                        }));
                    }
                    // As in exact status lookup, the journal can publish between the active read and
                    // the Starting-phase check. Retry that durable identity once before diagnosing
                    // missing evidence.
                    if let Some(status) = state.batch_store().status(&operation_id).map_err(|error| {
                        tracing::error!(%operation_id, %error, "Adoptable batch status re-read failed after start handoff");
                        CommandErrorV1::new(
                            "BATCH_EVIDENCE_INVALID",
                            "Adoptable batch state could not be verified. New batch work remains blocked.",
                            false,
                        )
                        .suggested(SuggestedActionV1::OpenHealth)
                    })? {
                        return publishable_durable_batch_status(state, status).map(Some).map_err(|error| {
                            tracing::error!(%operation_id, %error, "Adoptable batch status re-read failed public-contract validation");
                            CommandErrorV1::new(
                                "BATCH_EVIDENCE_INVALID",
                                "Adoptable batch state could not be verified. New batch work remains blocked.",
                                false,
                            )
                            .suggested(SuggestedActionV1::OpenHealth)
                        });
                    }
                    tracing::error!(%operation_id, "Process-local adoptable batch has no durable journal");
                    Err(CommandErrorV1::new(
                        "BATCH_EVIDENCE_INVALID",
                        "Adoptable batch state has no durable authority. New batch work remains blocked.",
                        false,
                    )
                    .suggested(SuggestedActionV1::OpenHealth))
                }
                Err(error) => {
                    tracing::error!(%operation_id, %error, "Adoptable durable batch could not be read");
                    Err(CommandErrorV1::new(
                        "BATCH_EVIDENCE_INVALID",
                        "Adoptable batch state could not be verified. New batch work remains blocked.",
                        false,
                    )
                    .suggested(SuggestedActionV1::OpenHealth))
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, "Active durable batch authority could not be read");
            Err(CommandErrorV1::new(
                "BATCH_EVIDENCE_INVALID",
                "Active batch state could not be verified. New batch work remains blocked.",
                false,
            )
            .suggested(SuggestedActionV1::OpenHealth))
        }
    }
}

/// Acknowledge only an exact terminal result that this process retained for renderer recovery. The
/// call is idempotent, and a lost response leaves the result eligible for safe replay.
#[tauri::command]
#[specta::specta]
pub async fn acknowledge_batch_run(operation_id: String, app: tauri::AppHandle) -> Result<bool, CommandErrorV1> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        acknowledge_batch_run_blocking(operation_id, &state)
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "Batch-acknowledgment worker stopped unexpectedly");
        CommandErrorV1::new(
            "BATCH_STATUS_WORKER_FAILED",
            "The batch result could not be acknowledged. Retry in a moment.",
            true,
        )
        .suggested(SuggestedActionV1::Retry)
    })?
}

pub(super) fn acknowledge_batch_run_blocking(operation_id: String, state: &AppState) -> Result<bool, CommandErrorV1> {
    RATE_LIMITER.check("acknowledge_batch_run").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Batch acknowledgment is busy. Wait a moment, then retry.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    let operation_id = canonical_batch_operation_id(&operation_id).map_err(|_| invalid_batch_operation_id_error())?;
    let status = state.batch_store().status(&operation_id).map_err(|error| {
        tracing::error!(%operation_id, %error, "Acknowledged batch authority could not be read");
        CommandErrorV1::new("BATCH_EVIDENCE_INVALID", "The batch result could not be verified.", false)
            .suggested(SuggestedActionV1::OpenHealth)
    })?;
    let Some(status) = status else {
        return Err(CommandErrorV1::new("BATCH_RUN_UNKNOWN", "The batch result is no longer available.", false));
    };
    let response = publishable_durable_batch_status(state, status).map_err(|error| {
        tracing::error!(%operation_id, %error, "Acknowledged batch failed public-contract validation");
        CommandErrorV1::new("BATCH_EVIDENCE_INVALID", "The batch result could not be verified.", false)
            .suggested(SuggestedActionV1::OpenHealth)
    })?;
    if response.status != BatchRunStatusV1::Settled || response.outcome.is_none() {
        return Err(CommandErrorV1::new("BATCH_RUN_NOT_SETTLED", "The batch is still running.", true)
            .suggested(SuggestedActionV1::Retry));
    }
    if !state.acknowledge_batch_run_renderer(&operation_id) {
        return Err(CommandErrorV1::new(
            "BATCH_RUN_NOT_ADOPTABLE",
            "This batch result is not awaiting renderer acknowledgment.",
            false,
        ));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc_contract::BatchOperationV1;

    #[test]
    fn batch_run_status_wire_shape_is_kind_bound_and_exact() {
        let operation_id = "00000000-0000-4000-8000-000000000001";
        for (status, expected) in [
            (BatchRunStatusV1::Starting, "starting"),
            (BatchRunStatusV1::Running, "running"),
            (BatchRunStatusV1::Settled, "settled"),
            (BatchRunStatusV1::Rejected, "rejected"),
            (BatchRunStatusV1::Unknown, "unknown"),
        ] {
            let wire = serde_json::to_value(BatchRunStatusResponseV1 {
                operation_id: operation_id.to_string(),
                operation: Some(BatchOperationV1::Transcribe),
                status,
                total: Some(2),
                outcome: None,
            })
            .expect("serialize batch status DTO");
            assert_eq!(wire["operationId"], operation_id);
            assert_eq!(wire["operation"], "transcribe");
            assert_eq!(wire["status"], expected);
        }

        let unknown = serde_json::to_value(BatchRunStatusResponseV1 {
            operation_id: operation_id.to_string(),
            operation: None,
            status: BatchRunStatusV1::Unknown,
            total: None,
            outcome: None,
        })
        .expect("serialize unknown batch status DTO");
        assert!(unknown["operation"].is_null());
    }
}
