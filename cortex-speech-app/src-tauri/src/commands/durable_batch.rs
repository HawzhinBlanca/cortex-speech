//! Durable schema-68 batch outcome, process-settlement, and worker-lifetime authority.

use super::*;

fn public_durable_batch_error_code(code: Option<&str>) -> String {
    match code {
        Some(
            code @ ("CHAMPION_UNAVAILABLE"
            | "CHAMPION_IDENTITY_MISMATCH"
            | "MODEL_IDENTITY_CHANGED"
            | "TRANSCRIPTION_SOURCE_CHANGED"
            | "AUDIO_DECODE_FAILED"
            | "BATCH_SEGMENT_MISSING"
            | "BATCH_TRANSCRIPT_WRITE_FAILED"
            | "BATCH_NORMALIZATION_FAILED"
            | "BATCH_REFINEMENT_FAILED"
            | "BATCH_JURY_FAILED"
            | "BATCH_TRANSCRIPTION_FAILED"
            | "BATCH_WORKER_START_FAILED"
            | "BATCH_WORKER_PANICKED"
            | "PROCESS_INTERRUPTED"),
        ) => code.to_string(),
        _ => "BATCH_EVIDENCE_INVALID".to_string(),
    }
}

pub(crate) fn durable_batch_outcome(
    status: &crate::db::BatchJobStatusV1,
) -> crate::error::AppResult<Option<BatchRunOutcome>> {
    use crate::db::BatchJobLifecycleV1;
    let disposition = match status.state {
        BatchJobLifecycleV1::Queued | BatchJobLifecycleV1::Running => return Ok(None),
        BatchJobLifecycleV1::Succeeded => BatchRunDisposition::Completed,
        BatchJobLifecycleV1::Cancelled => BatchRunDisposition::Cancelled,
        BatchJobLifecycleV1::Failed if status.error_code.as_deref() == Some("BATCH_WORKER_PANICKED") => {
            BatchRunDisposition::Panicked
        }
        BatchJobLifecycleV1::Failed => BatchRunDisposition::Halted,
    };
    let invalid = |field: &str, value: i64| {
        crate::error::AppError::Other(format!(
            "E_BATCH_EVIDENCE_INVALID: durable {field} count {value} is outside the public contract"
        ))
    };
    let count = |field: &str, value: i64| u32::try_from(value).map_err(|_| invalid(field, value));
    let total = usize::try_from(status.total).map_err(|_| invalid("total", status.total))?;
    let succeeded = count("applied", status.counts.applied)?;
    let failed = count("failed", status.counts.failed)?;
    let skipped = count("skipped", status.counts.skipped)?;
    let abandoned = count("abandoned", status.counts.abandoned)?;
    let accounted = u64::from(succeeded) + u64::from(failed) + u64::from(skipped) + u64::from(abandoned);
    if accounted != total as u64 {
        return Err(crate::error::AppError::Other(format!(
            "E_BATCH_EVIDENCE_INVALID: terminal item counts {accounted} do not equal total {}",
            status.total
        )));
    }
    Ok(Some(BatchRunOutcome {
        disposition,
        total,
        succeeded,
        failed,
        skipped,
        abandoned,
        cancelled: status.state == BatchJobLifecycleV1::Cancelled,
        error_code: match status.state {
            BatchJobLifecycleV1::Failed => Some(public_durable_batch_error_code(status.error_code.as_deref())),
            BatchJobLifecycleV1::Queued
            | BatchJobLifecycleV1::Running
            | BatchJobLifecycleV1::Succeeded
            | BatchJobLifecycleV1::Cancelled => None,
        },
    }))
}

fn durable_batch_status_response(
    status: crate::db::BatchJobStatusV1,
) -> crate::error::AppResult<crate::ipc_contract::BatchRunStatusResponseV1> {
    let outcome = durable_batch_outcome(&status)?.map(Into::into);
    let public_status = if outcome.is_some() {
        crate::ipc_contract::BatchRunStatusV1::Settled
    } else {
        crate::ipc_contract::BatchRunStatusV1::Running
    };
    let total = usize::try_from(status.total).map_err(|_| {
        crate::error::AppError::Other(format!(
            "E_BATCH_EVIDENCE_INVALID: durable total {} is outside the public contract",
            status.total
        ))
    })?;
    Ok(crate::ipc_contract::BatchRunStatusResponseV1 {
        operation_id: status.operation_id,
        operation: Some(status.kind.into()),
        status: public_status,
        total: Some(total),
        outcome,
    })
}

/// Bind durable status publication to the exact process-local settlement boundary. The database
/// header terminalizes before the worker can publish exact current-session Undo authority and close
/// its physical process gate. A renderer poll in that interval must therefore continue to observe a
/// non-terminal run, and ACK must remain impossible. Terminal journals from an earlier process have
/// no tracker entry and remain database-authoritative after restart.
pub(crate) fn publishable_durable_batch_status(
    state: &AppState,
    status: crate::db::BatchJobStatusV1,
) -> crate::error::AppResult<crate::ipc_contract::BatchRunStatusResponseV1> {
    let operation_id = status.operation_id.clone();
    let durable_operation: crate::ipc_contract::BatchOperationV1 = status.kind.into();
    let mut response = durable_batch_status_response(status)?;
    let (admission, tracked_operation, tracked_outcome) = state.batch_run_admission(&operation_id);
    let tracked_operation = tracked_operation.map(crate::ipc_contract::BatchOperationV1::from);

    match admission {
        crate::BatchRunAdmission::Running => {
            if tracked_operation != Some(durable_operation) {
                return Err(crate::error::AppError::Other(
                    "E_BATCH_EVIDENCE_INVALID: active process identity disagrees with durable batch kind".into(),
                ));
            }
            // This includes the small but critical terminal-header/final-history window. Even if the
            // guard has already retained the exact outcome in RAM, publication waits until it closes
            // the process gate and moves the tracker to Settled.
            response.status = crate::ipc_contract::BatchRunStatusV1::Running;
            response.outcome = None;
            Ok(response)
        }
        crate::BatchRunAdmission::Settled => {
            if tracked_operation != Some(durable_operation) {
                return Err(crate::error::AppError::Other(
                    "E_BATCH_EVIDENCE_INVALID: settled process identity disagrees with durable batch truth".into(),
                ));
            }
            if response.status != crate::ipc_contract::BatchRunStatusV1::Settled {
                // The database snapshot can be read immediately before the worker commits its
                // terminal header, while the tracker read below occurs immediately after guard
                // settlement. Re-read the exact immutable identity once; process settlement proves
                // that a terminal durable header must now exist, but never substitute RAM outcome
                // for that refreshed database authority.
                let refreshed = state.batch_store().status(&operation_id)?.ok_or_else(|| {
                    crate::error::AppError::Other(
                        "E_BATCH_EVIDENCE_INVALID: settled process identity lost its durable batch journal".into(),
                    )
                })?;
                let refreshed_operation: crate::ipc_contract::BatchOperationV1 = refreshed.kind.into();
                if refreshed_operation != durable_operation {
                    return Err(crate::error::AppError::Other(
                        "E_BATCH_EVIDENCE_INVALID: refreshed durable batch kind disagrees with settled process identity"
                            .into(),
                    ));
                }
                response = durable_batch_status_response(refreshed)?;
            }
            if response.status != crate::ipc_contract::BatchRunStatusV1::Settled {
                return Err(crate::error::AppError::Other(
                    "E_BATCH_EVIDENCE_INVALID: settled process identity has no terminal durable batch truth".into(),
                ));
            }
            let tracked_outcome = tracked_outcome.map(crate::ipc_contract::BatchRunOutcomeV1::from);
            if tracked_outcome != response.outcome {
                return Err(crate::error::AppError::Other(
                    "E_BATCH_EVIDENCE_INVALID: settled process outcome disagrees with durable batch truth".into(),
                ));
            }
            Ok(response)
        }
        crate::BatchRunAdmission::Rejected | crate::BatchRunAdmission::Unknown => {
            if response.status != crate::ipc_contract::BatchRunStatusV1::Settled {
                return Err(crate::error::AppError::Other(
                    "E_BATCH_EVIDENCE_INVALID: live durable batch has no process-local execution authority".into(),
                ));
            }
            Ok(response)
        }
    }
}

/// Owns both the durable execution lease and the process-local liveness gate from the instant a
/// journal is admitted, including the fallible OS-spawn boundary. Drop ordering is intentional:
/// durable terminal truth and exact undo authority must both exist before the process-local gate is
/// reopened or a physical-settlement event tells the renderer to query final status.
enum DurableBatchGuardOwner<R: tauri::Runtime = tauri::Wry> {
    App(tauri::AppHandle<R>),
    #[cfg(test)]
    Test(std::sync::Arc<AppState>),
}

/// Generic over the runtime (default: the desktop runtime) so a test can drive a real worker
/// through a mock app handle; production never names the parameter.
pub(crate) struct DurableBatchWorkerGuard<R: tauri::Runtime = tauri::Wry> {
    owner: DurableBatchGuardOwner<R>,
    operation_id: String,
    operation: crate::BatchOperation,
    lease: Option<crate::stores::BatchExecutionLease>,
    worker_entered: bool,
    durably_terminalized: bool,
    terminal_status: Option<crate::db::BatchJobStatusV1>,
    history_recorded: bool,
}

impl<R: tauri::Runtime> DurableBatchWorkerGuard<R> {
    pub(crate) fn new(
        app: tauri::AppHandle<R>,
        operation_id: String,
        operation: crate::BatchOperation,
        lease: crate::stores::BatchExecutionLease,
    ) -> Self {
        Self {
            owner: DurableBatchGuardOwner::App(app),
            operation_id,
            operation,
            lease: Some(lease),
            worker_entered: false,
            durably_terminalized: false,
            terminal_status: None,
            history_recorded: false,
        }
    }

    fn with_state<T>(
        &self,
        operation: impl FnOnce(&AppState) -> crate::error::AppResult<T>,
    ) -> crate::error::AppResult<T> {
        match &self.owner {
            DurableBatchGuardOwner::App(app) => {
                let state = app.try_state::<AppState>().ok_or_else(|| {
                    crate::error::AppError::Other("application state unavailable for durable batch guard".into())
                })?;
                operation(&state)
            }
            #[cfg(test)]
            DurableBatchGuardOwner::Test(state) => operation(state),
        }
    }

    /// This must be the first instruction executed by the spawned closure. Before it, any unwind or
    /// OS spawn refusal is a start failure; after it, an unwind is a worker panic.
    pub(crate) fn mark_worker_entered(&mut self) {
        self.worker_entered = true;
        if let Some(lease) = self.lease.as_mut() {
            lease.mark_worker_entered();
        }
    }

    pub(crate) fn lease(&self) -> crate::error::AppResult<&crate::stores::BatchExecutionLease> {
        self.lease.as_ref().ok_or_else(|| crate::error::AppError::Other("durable batch lease is unavailable".into()))
    }

    pub(crate) fn finish(
        &mut self,
        intent: crate::db::BatchTerminalIntentV1,
    ) -> crate::error::AppResult<crate::db::BatchJobStatusV1> {
        let lease = self
            .lease
            .as_mut()
            .ok_or_else(|| crate::error::AppError::Other("durable batch lease is unavailable".into()))?;
        let status = lease.finish(intent)?;
        self.durably_terminalized = true;
        self.terminal_status = Some(status.clone());
        self.record_history_token()?;
        Ok(status)
    }

    fn terminal_status_for_drop(&mut self) -> crate::error::AppResult<crate::db::BatchJobStatusV1> {
        if let Some(status) = self.terminal_status.clone() {
            return Ok(status);
        }

        if !self.durably_terminalized {
            let failure_code = if self.worker_entered { "BATCH_WORKER_PANICKED" } else { "BATCH_WORKER_START_FAILED" };
            if let Some(lease) = self.lease.as_mut() {
                match lease.finish(crate::db::BatchTerminalIntentV1::Failed { code: failure_code.into() }) {
                    Ok(status) => {
                        self.durably_terminalized = true;
                        self.terminal_status = Some(status.clone());
                        return Ok(status);
                    }
                    Err(error) => tracing::error!(
                        operation_id = %self.operation_id,
                        worker_entered = self.worker_entered,
                        %error,
                        "Batch guard could not terminalize directly; checking durable status before process settlement"
                    ),
                }
            }
        }

        let status = self.with_state(|state| state.batch_store().status(&self.operation_id))?.ok_or_else(|| {
            crate::error::AppError::Other(
                "E_BATCH_EVIDENCE_INVALID: admitted batch journal disappeared before settlement".into(),
            )
        })?;
        if durable_batch_outcome(&status)?.is_none() {
            return Err(crate::error::AppError::Other(
                "E_BATCH_EVIDENCE_INVALID: admitted batch journal remained non-terminal after guard drop".into(),
            ));
        }
        self.terminal_status = Some(status.clone());
        Ok(status)
    }

    /// Store exactly the durable outcome if the worker's normal terminal path did not already do
    /// so. Returning `true` means this guard still owns a live process gate and may settle it.
    fn prepare_process_settlement(&self, status: &crate::db::BatchJobStatusV1) -> crate::error::AppResult<bool> {
        let exact_outcome = durable_batch_outcome(status)?.ok_or_else(|| {
            crate::error::AppError::Other(
                "E_BATCH_EVIDENCE_INVALID: process settlement requires a terminal durable batch status".into(),
            )
        })?;
        self.with_state(|state| {
            let (admission, tracked_operation, tracked_outcome) = state.batch_run_admission(&self.operation_id);
            if tracked_operation != Some(self.operation) {
                return Err(crate::error::AppError::Other(
                    "E_BATCH_EVIDENCE_INVALID: durable outcome does not match the process-local batch identity".into(),
                ));
            }
            if let Some(tracked_outcome) = tracked_outcome {
                if tracked_outcome != exact_outcome {
                    return Err(crate::error::AppError::Other(
                        "E_BATCH_EVIDENCE_INVALID: process-local batch outcome disagrees with durable truth".into(),
                    ));
                }
            } else {
                if admission != crate::BatchRunAdmission::Running {
                    return Err(crate::error::AppError::Other(
                        "E_BATCH_EVIDENCE_INVALID: terminal durable batch has no writable process-local outcome slot"
                            .into(),
                    ));
                }
                if !state.record_batch_outcome(&self.operation_id, self.operation, exact_outcome.clone()) {
                    let (_, retry_operation, retry_outcome) = state.batch_run_admission(&self.operation_id);
                    if retry_operation != Some(self.operation) || retry_outcome.as_ref() != Some(&exact_outcome) {
                        return Err(crate::error::AppError::Other(
                            "E_BATCH_EVIDENCE_INVALID: exact durable outcome could not be retained before settlement"
                                .into(),
                        ));
                    }
                }
            }
            Ok(admission == crate::BatchRunAdmission::Running)
        })
    }

    fn record_history_token(&mut self) -> crate::error::AppResult<()> {
        if self.history_recorded {
            return Ok(());
        }
        let token = self
            .lease
            .as_ref()
            .ok_or_else(|| crate::error::AppError::Other("durable batch lease is unavailable".into()))?
            .history_token()?;
        if let Some(token) = token {
            self.with_state(|state| state.lock_history().record_batch_token(token).map(|_| ()))?;
        }
        self.history_recorded = true;
        Ok(())
    }
}

#[cfg(test)]
impl DurableBatchWorkerGuard {
    pub(crate) fn new_for_test(
        state: std::sync::Arc<AppState>,
        operation_id: String,
        operation: crate::BatchOperation,
        lease: crate::stores::BatchExecutionLease,
    ) -> Self {
        Self {
            owner: DurableBatchGuardOwner::Test(state),
            operation_id,
            operation,
            lease: Some(lease),
            worker_entered: false,
            durably_terminalized: false,
            terminal_status: None,
            history_recorded: false,
        }
    }
}

impl<R: tauri::Runtime> Drop for DurableBatchWorkerGuard<R> {
    fn drop(&mut self) {
        let process_settlement_ready = self
            .terminal_status_for_drop()
            .and_then(|status| {
                // A panic after an applied prefix must publish its exact current-session undo token
                // before another batch can start. A no-effect terminal returns `None` and succeeds.
                self.record_history_token()?;
                self.prepare_process_settlement(&status)
            })
            .map_err(|error| {
                tracing::error!(
                    operation_id = %self.operation_id,
                    worker_entered = self.worker_entered,
                    %error,
                    "Batch process gate retained because durable settlement proof is incomplete"
                );
            })
            .ok()
            .unwrap_or(false);

        // Explicitly release durable restore exclusion before announcing physical settlement.
        drop(self.lease.take());
        if !process_settlement_ready {
            return;
        }
        let finished = self
            .with_state(|state| Ok(state.finish_batch_for_run(&self.operation_id, self.operation)))
            .unwrap_or(false);
        if !finished {
            return;
        }
        match &self.owner {
            DurableBatchGuardOwner::App(app) => emit_or_log(
                app,
                "batch-worker-settled",
                serde_json::json!({
                    "operationId": self.operation_id,
                    "operation": match self.operation {
                        crate::BatchOperation::Transcribe => "transcribe",
                        crate::BatchOperation::Normalize => "normalize",
                    }
                }),
            ),
            #[cfg(test)]
            DurableBatchGuardOwner::Test(_) => {}
        }
    }
}

#[cfg(test)]
mod durable_batch_contract_tests {
    use super::*;
    use crate::db::{BatchItemCountsV1, BatchJobKindV1, BatchJobLifecycleV1, BatchJobStatusV1};
    use tauri::Manager;

    fn status(
        operation_id: &str,
        kind: BatchJobKindV1,
        state: BatchJobLifecycleV1,
        total: i64,
        counts: BatchItemCountsV1,
        error_code: Option<&str>,
    ) -> BatchJobStatusV1 {
        BatchJobStatusV1 {
            operation_id: operation_id.to_string(),
            kind,
            state,
            total,
            completed: counts.applied + counts.skipped + counts.failed + counts.abandoned,
            progress: 0.0,
            counts,
            request_sha256: "1".repeat(64),
            config_sha256: "2".repeat(64),
            error_code: error_code.map(str::to_string),
            started_at: Some("2026-09-04T00:00:00Z".to_string()),
            finished_at: None,
        }
    }

    fn counts(applied: i64, failed: i64, skipped: i64, abandoned: i64) -> BatchItemCountsV1 {
        BatchItemCountsV1 { pending: 0, applied, skipped, failed, abandoned }
    }

    fn outcome(total: usize, succeeded: u32, abandoned: u32) -> BatchRunOutcome {
        BatchRunOutcome {
            disposition: BatchRunDisposition::Completed,
            total,
            succeeded,
            failed: 0,
            skipped: 0,
            abandoned,
            cancelled: false,
            error_code: None,
        }
    }

    #[test]
    fn durable_outcome_maps_every_lifecycle_and_sanitizes_failure_codes() {
        for lifecycle in [BatchJobLifecycleV1::Queued, BatchJobLifecycleV1::Running] {
            let live = status("live", BatchJobKindV1::Normalize, lifecycle, 2, counts(0, 0, 0, 0), None);
            assert_eq!(durable_batch_outcome(&live).expect("live status is valid"), None);
        }

        let cases = [
            (BatchJobLifecycleV1::Succeeded, None, BatchRunDisposition::Completed, false, None),
            (BatchJobLifecycleV1::Cancelled, None, BatchRunDisposition::Cancelled, true, None),
            (
                BatchJobLifecycleV1::Failed,
                Some("BATCH_WORKER_PANICKED"),
                BatchRunDisposition::Panicked,
                false,
                Some("BATCH_WORKER_PANICKED"),
            ),
            (
                BatchJobLifecycleV1::Failed,
                Some("AUDIO_DECODE_FAILED"),
                BatchRunDisposition::Halted,
                false,
                Some("AUDIO_DECODE_FAILED"),
            ),
            (
                BatchJobLifecycleV1::Failed,
                Some("private sqlite detail"),
                BatchRunDisposition::Halted,
                false,
                Some("BATCH_EVIDENCE_INVALID"),
            ),
            (BatchJobLifecycleV1::Failed, None, BatchRunDisposition::Halted, false, Some("BATCH_EVIDENCE_INVALID")),
        ];
        for (lifecycle, error_code, disposition, cancelled, public_error) in cases {
            let terminal = status("terminal", BatchJobKindV1::Transcribe, lifecycle, 4, counts(1, 1, 1, 1), error_code);
            let actual = durable_batch_outcome(&terminal)
                .expect("terminal evidence is internally consistent")
                .expect("terminal state has an outcome");
            assert_eq!(actual.disposition, disposition);
            assert_eq!(
                (actual.total, actual.succeeded, actual.failed, actual.skipped, actual.abandoned),
                (4, 1, 1, 1, 1)
            );
            assert_eq!(actual.cancelled, cancelled);
            assert_eq!(actual.error_code.as_deref(), public_error);
        }

        let allowed = [
            "CHAMPION_UNAVAILABLE",
            "CHAMPION_IDENTITY_MISMATCH",
            "MODEL_IDENTITY_CHANGED",
            "TRANSCRIPTION_SOURCE_CHANGED",
            "AUDIO_DECODE_FAILED",
            "BATCH_SEGMENT_MISSING",
            "BATCH_TRANSCRIPT_WRITE_FAILED",
            "BATCH_NORMALIZATION_FAILED",
            "BATCH_REFINEMENT_FAILED",
            "BATCH_JURY_FAILED",
            "BATCH_TRANSCRIPTION_FAILED",
            "BATCH_WORKER_START_FAILED",
            "BATCH_WORKER_PANICKED",
            "PROCESS_INTERRUPTED",
        ];
        for code in allowed {
            assert_eq!(public_durable_batch_error_code(Some(code)), code);
        }
    }

    #[test]
    fn durable_outcome_rejects_negative_overflowing_and_unaccounted_counts() {
        let invalid_cases = [
            ("total", -1, counts(0, 0, 0, 0)),
            ("applied", 0, counts(-1, 0, 0, 1)),
            ("failed", 0, counts(0, -1, 0, 1)),
            ("skipped", 0, counts(0, 0, -1, 1)),
            ("abandoned", 0, counts(0, 0, 0, -1)),
            ("applied", i64::from(u32::MAX) + 1, counts(i64::from(u32::MAX) + 1, 0, 0, 0)),
            ("do not equal total", 2, counts(1, 0, 0, 0)),
        ];
        for (needle, total, item_counts) in invalid_cases {
            let malformed = status(
                "malformed",
                BatchJobKindV1::Normalize,
                BatchJobLifecycleV1::Succeeded,
                total,
                item_counts,
                None,
            );
            let error = durable_batch_outcome(&malformed).expect_err("malformed durable evidence must fail closed");
            assert!(error.to_string().contains(needle), "unexpected error for {needle}: {error}");
        }

        let negative_live_total = status(
            "negative-live-total",
            BatchJobKindV1::Normalize,
            BatchJobLifecycleV1::Running,
            -1,
            counts(0, 0, 0, 0),
            None,
        );
        assert!(
            durable_batch_status_response(negative_live_total).is_err(),
            "the public response must reject a negative total even while a batch is live"
        );
    }

    #[test]
    fn durable_status_response_exposes_only_running_or_exactly_settled_truth() {
        let live = durable_batch_status_response(status(
            "response-live",
            BatchJobKindV1::Normalize,
            BatchJobLifecycleV1::Queued,
            3,
            counts(0, 0, 0, 0),
            None,
        ))
        .expect("queued response");
        assert_eq!(live.status, crate::ipc_contract::BatchRunStatusV1::Running);
        assert_eq!(live.operation, Some(crate::ipc_contract::BatchOperationV1::Normalize));
        assert_eq!(live.total, Some(3));
        assert_eq!(live.outcome, None);

        let settled = durable_batch_status_response(status(
            "response-settled",
            BatchJobKindV1::Transcribe,
            BatchJobLifecycleV1::Cancelled,
            3,
            counts(1, 0, 1, 1),
            None,
        ))
        .expect("cancelled response");
        assert_eq!(settled.status, crate::ipc_contract::BatchRunStatusV1::Settled);
        assert_eq!(settled.operation, Some(crate::ipc_contract::BatchOperationV1::Transcribe));
        assert!(settled.outcome.expect("terminal outcome").cancelled);
    }

    #[test]
    fn publication_refuses_untracked_live_truth_and_process_identity_disagreement() {
        let tmp = tempfile::tempdir().unwrap();
        let app = crate::test_support::managed_app_state(tmp.path());
        let state = app.state::<AppState>();

        let untracked = status(
            "untracked-live",
            BatchJobKindV1::Normalize,
            BatchJobLifecycleV1::Running,
            1,
            counts(0, 0, 0, 0),
            None,
        );
        assert!(publishable_durable_batch_status(&state, untracked).is_err());

        let operation_id = uuid::Uuid::from_u128(0xd001).to_string();
        state
            .try_start_batch_for_run(&operation_id, crate::BatchOperation::Normalize, 2)
            .expect("admit process-local normalize identity");
        let wrong_kind = status(
            &operation_id,
            BatchJobKindV1::Transcribe,
            BatchJobLifecycleV1::Running,
            2,
            counts(0, 0, 0, 0),
            None,
        );
        let error = publishable_durable_batch_status(&state, wrong_kind)
            .expect_err("a process/durable kind disagreement must fail closed");
        assert!(error.to_string().contains("identity disagrees"));

        let terminal_header = status(
            &operation_id,
            BatchJobKindV1::Normalize,
            BatchJobLifecycleV1::Succeeded,
            2,
            counts(2, 0, 0, 0),
            None,
        );
        let hidden = publishable_durable_batch_status(&state, terminal_header)
            .expect("a matching but physically live process remains publishable as running");
        assert_eq!(hidden.status, crate::ipc_contract::BatchRunStatusV1::Running);
        assert_eq!(hidden.outcome, None, "durable completion stays hidden until the process gate settles");
    }

    #[test]
    fn publication_requires_settled_ram_outcome_to_equal_durable_truth() {
        let tmp = tempfile::tempdir().unwrap();
        let app = crate::test_support::managed_app_state(tmp.path());
        let state = app.state::<AppState>();
        let operation_id = uuid::Uuid::from_u128(0xd002).to_string();
        state
            .try_start_batch_for_run(&operation_id, crate::BatchOperation::Normalize, 2)
            .expect("admit process-local identity");
        assert!(state.record_batch_outcome(&operation_id, crate::BatchOperation::Normalize, outcome(2, 2, 0),));
        assert!(state.finish_batch_for_run(&operation_id, crate::BatchOperation::Normalize));

        let matching = status(
            &operation_id,
            BatchJobKindV1::Normalize,
            BatchJobLifecycleV1::Succeeded,
            2,
            counts(2, 0, 0, 0),
            None,
        );
        let response = publishable_durable_batch_status(&state, matching)
            .expect("matching process and durable outcomes are publishable");
        assert_eq!(response.status, crate::ipc_contract::BatchRunStatusV1::Settled);

        let mismatched = status(
            &operation_id,
            BatchJobKindV1::Normalize,
            BatchJobLifecycleV1::Succeeded,
            2,
            counts(1, 0, 0, 1),
            None,
        );
        let error = publishable_durable_batch_status(&state, mismatched)
            .expect_err("RAM and durable outcomes must agree exactly");
        assert!(error.to_string().contains("outcome disagrees"));
    }
}
