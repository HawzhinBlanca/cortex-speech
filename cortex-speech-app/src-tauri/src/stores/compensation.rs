//! Reviewer-compensation query and settlement boundary for the owner's desktop.
//!
//! Written 2026-08-30 because owner canon has REQUIRED immutable, contiguous-range settlements
//! since 2026-08-21 ("External payout references allocate contiguous ledger ranges through
//! immutable `review_compensation_settlements` rows") while the recording function had ZERO
//! production callers — the table held 0 rows against ≈6.34e9 micro-IQD outstanding, so paying a
//! reviewer was an out-of-band act with no ledger anchor: the phone kept showing the full balance
//! forever and a dispute had nothing to point at. This store is the missing caller.

use crate::database_runtime::{DatabaseRuntime, MutationGuard};
use crate::db::{ReviewCompensationSettlement, ReviewCompensationSummary};
use crate::error::{AppError, AppResult};
use std::sync::MutexGuard;

/// One payee's complete money picture plus the exact ledger boundary a settlement may claim.
pub(crate) struct ReviewerCompensationOverview {
    pub reviewer: String,
    pub summary: ReviewCompensationSummary,
    /// Inclusive ledger boundary as of this read. A settlement records THIS value, so credits that
    /// land between the owner reading the screen and clicking "record" stay outstanding instead of
    /// being silently swept into a payout that never covered them.
    pub max_ledger_id: i64,
}

#[derive(Clone)]
pub(crate) struct CompensationStore {
    runtime: DatabaseRuntime,
}

impl CompensationStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    fn begin_mutation(&self) -> AppResult<MutationGuard<'_>> {
        self.runtime.begin_mutation().map_err(AppError::Other)
    }

    fn lock_after_mutation(
        &self,
        operation: &str,
        mutation: &MutationGuard<'_>,
    ) -> MutexGuard<'_, crate::db::Database> {
        self.runtime.lock_after_mutation(mutation).unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during a compensation write");
            poisoned.into_inner()
        })
    }

    /// Every ledger payee with their exact earned/settled/outstanding totals under the active
    /// policy, from one restore-gated read snapshot.
    pub(crate) fn overview(&self) -> AppResult<Vec<ReviewerCompensationOverview>> {
        let database = self.runtime.open_read()?;
        let mut rows = Vec::new();
        for (reviewer, max_ledger_id) in database.review_compensation_reviewers()? {
            let summary = database.review_compensation_summary(&reviewer)?;
            rows.push(ReviewerCompensationOverview { reviewer, summary, max_ledger_id });
        }
        Ok(rows)
    }

    /// Record one immutable external payout against an exact inclusive ledger boundary. All money
    /// semantics (idempotent replay by reference, contiguous-range allocation, policy re-proof,
    /// FULL-sync durability) live in `record_review_compensation_settlement`; this store adds only
    /// the serialized single-writer boundary every Cortex mutation goes through.
    pub(crate) fn settle(
        &self,
        reviewer: &str,
        through_ledger_id_inclusive: i64,
        payout_reference: &str,
    ) -> AppResult<ReviewCompensationSettlement> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("record_compensation_settlement", &mutation).record_review_compensation_settlement(
            reviewer,
            through_ledger_id_inclusive,
            payout_reference,
        )
    }
}
