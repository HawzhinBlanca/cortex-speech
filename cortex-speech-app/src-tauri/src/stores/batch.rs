//! Durable batch execution boundary.
//!
//! The lease deliberately owns one process-wide mutation admission for its entire lifetime.  A
//! database mutex serializes statements, but it does not by itself prevent a restore from replacing
//! the database while inference is running between statements.  Commands receive this capability
//! instead of raw SQL access and must move it into the worker they successfully spawn.

use crate::database_runtime::{
    begin_mutation_at_restore_generation, DatabaseRuntime, MutationGuard, RestoreGeneration,
};
use crate::db::{
    BatchChampionDraftV1, BatchExecutionHistoryTokenV1, BatchExecutorIdentityV1, BatchItemCommitOutcomeV1,
    BatchJobKindV1, BatchJobStatusV1, BatchPendingItemV1, BatchTerminalIntentV1,
};
use crate::error::{AppError, AppResult};

#[derive(Clone)]
pub(crate) struct BatchStore {
    runtime: DatabaseRuntime,
}

pub(crate) struct BatchAdmissionV1<'a> {
    pub operation_id: &'a str,
    pub kind: BatchJobKindV1,
    pub segment_ids: &'a [String],
    pub config_sha256: &'a str,
    pub executor: BatchExecutorIdentityV1,
    pub cancel: &'a std::sync::atomic::AtomicBool,
    pub restore_generation: RestoreGeneration,
}

impl BatchStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    fn lock_after_mutation(
        &self,
        operation: &str,
        mutation: &MutationGuard<'_>,
    ) -> std::sync::MutexGuard<'_, crate::db::Database> {
        self.runtime.lock_after_mutation(mutation).unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during durable batch work");
            poisoned.into_inner()
        })
    }

    /// Acquire restore exclusion first, then durably admit the exact request.  Returning a lease
    /// transfers both authorities together; an admitted job can never exist without an owner that
    /// will either terminalize it or hard-fail it from `Drop`.
    pub(crate) fn admit(&self, request: BatchAdmissionV1<'_>) -> AppResult<(BatchExecutionLease, BatchJobStatusV1)> {
        let mutation = begin_mutation_at_restore_generation(request.restore_generation).map_err(AppError::Other)?;
        let status = self.lock_after_mutation("admit_batch_job_v1", &mutation).admit_batch_job_v1_cancellable(
            request.operation_id,
            request.kind,
            request.segment_ids,
            request.config_sha256,
            &request.executor,
            request.cancel,
        )?;
        let lease = BatchExecutionLease {
            store: self.clone(),
            operation_id: request.operation_id.to_string(),
            executor: request.executor,
            terminalized: false,
            drop_failure_code: "BATCH_WORKER_START_FAILED",
            _mutation: mutation,
        };
        Ok((lease, status))
    }

    pub(crate) fn status(&self, operation_id: &str) -> AppResult<Option<BatchJobStatusV1>> {
        self.runtime.open_read()?.get_batch_job_status_v1(operation_id)
    }

    pub(crate) fn active(&self) -> AppResult<Option<BatchJobStatusV1>> {
        self.runtime.open_read()?.active_batch_job_v1()
    }
}

#[must_use = "an admitted batch lease must remain alive through durable terminalization"]
pub(crate) struct BatchExecutionLease {
    store: BatchStore,
    operation_id: String,
    executor: BatchExecutorIdentityV1,
    terminalized: bool,
    drop_failure_code: &'static str,
    _mutation: MutationGuard<'static>,
}

impl BatchExecutionLease {
    /// Called as the first instruction inside the spawned closure. If the OS rejects the spawn, the
    /// captured lease is dropped without reaching this method and records START_FAILED instead.
    pub(crate) fn mark_worker_entered(&mut self) {
        self.drop_failure_code = "BATCH_WORKER_PANICKED";
    }

    pub(crate) fn pending_page(&self, after_ordinal: Option<i64>) -> AppResult<Vec<BatchPendingItemV1>> {
        self.store
            .lock_after_mutation("pending_batch_item_page_v1", &self._mutation)
            .pending_batch_item_page_v1(&self.operation_id, after_ordinal)
    }

    pub(crate) fn commit_normalization(
        &self,
        ordinal: i64,
        normalized_transcript: &str,
        normalizer_version: &str,
    ) -> AppResult<BatchItemCommitOutcomeV1> {
        self.store.lock_after_mutation("commit_batch_normalization_v1", &self._mutation).commit_batch_normalization_v1(
            &self.operation_id,
            ordinal,
            normalized_transcript,
            normalizer_version,
            &self.executor,
        )
    }

    pub(crate) fn commit_champion_draft(
        &self,
        ordinal: i64,
        draft: &BatchChampionDraftV1,
    ) -> AppResult<BatchItemCommitOutcomeV1> {
        self.store
            .lock_after_mutation("commit_batch_champion_draft_v1", &self._mutation)
            .commit_batch_champion_draft_v1(&self.operation_id, ordinal, draft, &self.executor)
    }

    pub(crate) fn finish(&mut self, intent: BatchTerminalIntentV1) -> AppResult<BatchJobStatusV1> {
        let status = self.store.lock_after_mutation("finish_batch_job_v1", &self._mutation).finish_batch_job_v1(
            &self.operation_id,
            intent,
            &self.executor,
        )?;
        self.terminalized = true;
        Ok(status)
    }

    /// Create the bounded current-session undo authority only after the parent job is durably
    /// terminal.  Failed/halted jobs with an applied prefix remain undoable.
    pub(crate) fn history_token(&self) -> AppResult<Option<BatchExecutionHistoryTokenV1>> {
        self.store
            .lock_after_mutation("batch_execution_history_token_v1", &self._mutation)
            .batch_execution_history_token_v1(&self.operation_id)
    }
}

impl Drop for BatchExecutionLease {
    fn drop(&mut self) {
        if self.terminalized {
            return;
        }
        let intent = BatchTerminalIntentV1::Failed { code: self.drop_failure_code.to_string() };
        match self.store.lock_after_mutation("drop_batch_execution_lease", &self._mutation).finish_batch_job_v1(
            &self.operation_id,
            intent,
            &self.executor,
        ) {
            Ok(_) => self.terminalized = true,
            Err(error) => tracing::error!(
                operation_id = %self.operation_id,
                %error,
                "admitted batch lease dropped before durable terminalization"
            ),
        }
    }
}
