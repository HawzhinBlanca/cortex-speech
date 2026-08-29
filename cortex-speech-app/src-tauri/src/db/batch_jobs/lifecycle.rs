//! Schema-68 durable batch terminalization and interrupted-process recovery.

use super::*;

impl Database {
    /// Terminalize a running operation from its item evidence. Failed/cancelled stops abandon every
    /// still-pending item in the same commit; success is accepted only when nothing remains pending.
    /// If a cancellation or panic arrives after the final pending item already committed, the durable
    /// ledger has no work left to cancel or abandon. In that exact race the all-applied/skipped ledger
    /// canonicalizes to success instead of leaving an otherwise complete header stuck in `running`.
    pub fn finish_batch_job_v1(
        &self,
        operation_id: &str,
        intent: BatchTerminalIntentV1,
        executor: &BatchExecutorIdentityV1,
    ) -> AppResult<BatchJobStatusV1> {
        self.require_batch_schema_v1()?;
        validate_operation_uuid(operation_id)?;
        let (requested_target, requested_code, abandoned_code) = match &intent {
            BatchTerminalIntentV1::Succeeded => (BatchJobLifecycleV1::Succeeded, None, None),
            BatchTerminalIntentV1::Failed { code } => {
                validate_result_code(code)?;
                (BatchJobLifecycleV1::Failed, Some(code.as_str()), Some(code.as_str()))
            }
            BatchTerminalIntentV1::Cancelled { code } => {
                validate_result_code(code)?;
                (BatchJobLifecycleV1::Cancelled, Some(code.as_str()), Some(code.as_str()))
            }
        };
        self.conn.execute("SAVEPOINT batch_v1_finish", [])?;
        let result = (|| -> AppResult<()> {
            self.reserve_batch_writer()?;
            let header = self
                .read_batch_header_v1(operation_id)?
                .ok_or_else(|| AppError::Validation("batch operation does not exist".into()))?;
            Self::require_batch_executor_v1(&header, executor)?;
            if header.state.is_terminal() {
                if header.state == requested_target && header.error_code.as_deref() == requested_code {
                    return Ok(());
                }
                // Idempotent replay of the same too-late cancellation/panic must return the already
                // committed canonical success. All-positive terminal item evidence is the proof that
                // the requested stop had no remaining effect to prevent; no failed or abandoned item
                // can pass `status_from_header_v1` for a succeeded header.
                if header.state == BatchJobLifecycleV1::Succeeded
                    && matches!(requested_target, BatchJobLifecycleV1::Failed | BatchJobLifecycleV1::Cancelled)
                {
                    self.status_from_header_v1(header)?;
                    return Ok(());
                }
                return Err(batch_evidence_error("terminal batch cannot be rewritten to a different outcome"));
            }
            if header.state != BatchJobLifecycleV1::Running {
                return Err(batch_evidence_error("only a running batch can be normally terminalized"));
            }
            if let Some(abandoned_code) = abandoned_code {
                self.conn.execute(
                    "UPDATE batch_job_items_v1
                        SET state='abandoned',result_code=?2,
                            terminal_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                      WHERE job_id=?1 AND state='pending'",
                    params![operation_id, abandoned_code],
                )?;
            } else {
                let pending: i64 = self.conn.query_row(
                    "SELECT count(*) FROM batch_job_items_v1 WHERE job_id=?1 AND state='pending'",
                    [operation_id],
                    |row| row.get(0),
                )?;
                if pending != 0 {
                    return Err(AppError::Validation(format!(
                        "cannot succeed batch {operation_id}: {pending} item(s) remain pending"
                    )));
                }
            }
            let counts = self.batch_item_counts_v1(operation_id)?;
            if requested_target == BatchJobLifecycleV1::Cancelled && counts.failed != 0 {
                return Err(AppError::Validation(
                    "BATCH_TERMINAL_CONFLICT: a durable item failure must remain a failed batch, not be relabelled cancelled"
                        .into(),
                ));
            }
            let (target, code) = if counts.failed == 0 && counts.abandoned == 0 {
                // A stop signal can win immediately after the last effect commits but before the
                // worker publishes its header. With no negative or pending item evidence, success is
                // the only lifecycle state accepted by the schema-68 authority and by startup
                // recovery. Canonicalize here so the live process reaches that same truth immediately.
                (BatchJobLifecycleV1::Succeeded, None)
            } else {
                (requested_target, requested_code)
            };
            let changed = self.conn.execute(
                "UPDATE jobs
                    SET state=?2,completed=total,progress=1.0,error_code=?3,error_detail=NULL,
                        finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                        updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE id=?1 AND state='running' AND total=?4",
                params![operation_id, target.as_str(), code, counts.terminal()],
            )?;
            if changed != 1 {
                return Err(batch_evidence_error("batch header rejected its terminal item evidence"));
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("batch_v1_finish")?;
                self.track_write()?;
                self.get_batch_job_status_v1(operation_id)?.ok_or_else(|| {
                    batch_evidence_error("terminalized batch disappeared immediately after durable commit")
                })
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("batch_v1_finish");
                Err(error)
            }
        }
    }

    /// Startup recovery for the sole interrupted operation. A fully-settled running header is
    /// completed from its durable items; otherwise every pending item is abandoned and the parent
    /// hard-fails. A committed queued header is likewise made running and failed atomically so it can
    /// never block all future work forever.
    pub fn recover_active_batch_job_v1(&self) -> AppResult<Option<BatchJobStatusV1>> {
        let Some(active) = self.active_batch_job_v1()? else { return Ok(None) };
        self.validate_batch_job_authority_v1()?;
        let operation_id = active.operation_id;
        self.conn.execute("SAVEPOINT batch_v1_recover", [])?;
        let result = (|| -> AppResult<()> {
            self.reserve_batch_writer()?;
            let header = self
                .read_batch_header_v1(&operation_id)?
                .ok_or_else(|| batch_evidence_error("active batch vanished during recovery"))?;
            if header.state == BatchJobLifecycleV1::Queued {
                self.conn.execute(
                    "UPDATE jobs
                        SET state='running',started_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                      WHERE id=?1 AND state='queued'",
                    [&operation_id],
                )?;
            }
            self.conn.execute(
                "UPDATE batch_job_items_v1
                    SET state='abandoned',result_code='PROCESS_INTERRUPTED',
                        terminal_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE job_id=?1 AND state='pending'",
                [&operation_id],
            )?;
            let counts = self.batch_item_counts_v1(&operation_id)?;
            let first_failure_code = if counts.failed == 0 {
                None
            } else {
                Some(
                    self.conn
                        .query_row(
                            "SELECT result_code FROM batch_job_items_v1
                              WHERE job_id=?1 AND state='failed' ORDER BY ordinal LIMIT 1",
                            [&operation_id],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(|error| {
                            batch_evidence_error(format!(
                                "interrupted batch failure code could not be recovered: {error}"
                            ))
                        })?,
                )
            };
            let (state, error_code) = if counts.failed + counts.abandoned == 0 {
                ("succeeded", None)
            } else {
                ("failed", first_failure_code.as_deref().or(Some("PROCESS_INTERRUPTED")))
            };
            let changed = self.conn.execute(
                "UPDATE jobs
                    SET state=?2,completed=total,progress=1.0,error_code=?3,error_detail=NULL,
                        finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                        updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE id=?1 AND state='running' AND total=?4",
                params![operation_id, state, error_code, counts.terminal()],
            )?;
            if changed != 1 {
                return Err(batch_evidence_error("interrupted batch could not be reconciled from item evidence"));
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("batch_v1_recover")?;
                self.track_write()?;
                self.get_batch_job_status_v1(&operation_id)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("batch_v1_recover");
                Err(error)
            }
        }
    }
}
