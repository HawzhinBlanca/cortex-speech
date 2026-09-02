//! Exact schema-68 batch undo/redo token construction and atomic inverse application.

use super::*;

impl Database {
    /// Build a self-describing undo token for every applied item in a terminal batch. Failed and
    /// cancelled operations remain undoable when they committed a prefix before the hard stop.
    pub fn batch_job_history_token_v1(&self, operation_id: &str) -> AppResult<Option<BatchHistoryTokenV1>> {
        self.require_batch_schema_v1()?;
        validate_operation_uuid(operation_id)?;
        // History is latency-sensitive and needs only this operation's immutable proof. Startup and
        // restore still call the global validator over every historical batch.
        let status = validate_one_batch_job_authority_on(&self.conn, operation_id)?;
        if !status.state.is_terminal() {
            return Err(AppError::Validation("batch history is unavailable until durable terminalization".into()));
        }
        let mut tokens = Vec::with_capacity(status.counts.applied as usize);
        for ordinal in 0..status.total {
            let item = self
                .read_batch_item_v1(operation_id, ordinal)?
                .ok_or_else(|| batch_evidence_error(format!("job {operation_id} is missing ordinal {ordinal}")))?;
            Self::decode_before_projection_v1(&item)?;
            if item.state != BatchItemStateV1::Applied {
                continue;
            }
            let after = Self::decode_after_projection_v1(&item)?
                .ok_or_else(|| batch_evidence_error("applied history item has no after projection"))?;
            let current = Self::read_batch_projection_on(&self.conn, &item.segment_id)?.ok_or_else(|| {
                AppError::Validation(format!("batch history segment '{}' no longer exists", item.segment_id))
            })?;
            let (_, current_sha256, _) = projection_authority(&current)?;
            if current.review_revision != after.review_revision
                || item.after_projection_sha256.as_deref() != Some(current_sha256.as_str())
            {
                return Err(AppError::Validation(format!(
                    "BATCH_HISTORY_CONFLICT: segment '{}' no longer matches the applied journal endpoint",
                    item.segment_id
                )));
            }
            tokens.push(BatchHistoryItemTokenV1 {
                ordinal,
                segment_id: item.segment_id,
                expected_projection_sha256: current_sha256,
                expected_review_revision: current.review_revision,
            });
        }
        if tokens.is_empty() {
            return Ok(None);
        }
        Ok(Some(BatchHistoryTokenV1 {
            operation_id: operation_id.to_string(),
            kind: status.kind,
            expected_side: BatchHistorySideV1::After,
            items: tokens,
        }))
    }

    pub fn batch_execution_history_token_v1(
        &self,
        operation_id: &str,
    ) -> AppResult<Option<BatchExecutionHistoryTokenV1>> {
        self.batch_job_history_token_v1(operation_id)
    }

    /// Atomically toggle every applied item between its immutable before and after journal
    /// projections. Only machine-owned fields and the exact hypothesis rows are restored; source,
    /// rights, review, and human columns are compare-only authority. Revisions advance naturally.
    pub fn apply_batch_job_history_v1(&self, token: &BatchHistoryTokenV1) -> AppResult<BatchHistoryTokenV1> {
        self.require_batch_schema_v1()?;
        validate_operation_uuid(&token.operation_id)?;
        if token.items.is_empty() || token.items.len() > MAX_BATCH_ITEMS_V1 {
            return Err(AppError::Validation("batch history token must contain at least one applied item".into()));
        }
        let mut previous_ordinal = None;
        for endpoint in &token.items {
            crate::validation::input::validate_identifier(&endpoint.segment_id).map_err(AppError::Validation)?;
            validate_sha256(&endpoint.expected_projection_sha256, "history endpoint projection hash")?;
            if endpoint.ordinal < 0 || endpoint.expected_review_revision < 0 {
                return Err(AppError::Validation("batch history endpoint identity is invalid".into()));
            }
            if previous_ordinal.is_some_and(|previous| endpoint.ordinal <= previous) {
                return Err(AppError::Validation(
                    "batch history endpoints must be unique and strictly ordered by ordinal".into(),
                ));
            }
            previous_ordinal = Some(endpoint.ordinal);
        }

        self.conn.execute("SAVEPOINT batch_v1_history", [])?;
        let result = (|| -> AppResult<BatchHistoryTokenV1> {
            self.reserve_batch_writer()?;
            let status = validate_one_batch_job_authority_on(&self.conn, &token.operation_id)?;
            let live_batch: bool = self.conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM jobs
                     WHERE kind IN ('batch_transcribe_v1','batch_normalize_v1')
                       AND state IN ('queued','running')
                 )",
                [],
                |row| row.get(0),
            )?;
            if live_batch {
                return Err(AppError::Validation(
                    "BATCH_HISTORY_CONFLICT: undo and redo are blocked while another batch is active".into(),
                ));
            }
            if !status.state.is_terminal() || status.kind != token.kind {
                return Err(AppError::Validation(
                    "BATCH_HISTORY_CONFLICT: token does not name the terminal journal operation".into(),
                ));
            }
            if status.counts.applied as usize != token.items.len() {
                return Err(AppError::Validation(
                    "BATCH_HISTORY_CONFLICT: token does not cover the exact applied item set".into(),
                ));
            }

            // Pass one proves that every endpoint is an exact inverse candidate before any write.
            // Each prepared item is dropped immediately; unlike the old Vec<HistoryAction>, peak
            // retained full-projection authority is constant rather than batch-cardinality sized.
            for endpoint in &token.items {
                drop(self.prepare_batch_history_item_v1(&token.operation_id, endpoint, token.expected_side)?);
            }

            // Pass two repeats the exact endpoint check under the same writer reservation and applies
            // one inverse at a time. Any later error rolls the enclosing savepoint back, preserving
            // atomic all-or-nothing semantics without retaining every target projection in memory.
            let mut next_items = Vec::with_capacity(token.items.len());
            for endpoint in &token.items {
                let action = self.prepare_batch_history_item_v1(&token.operation_id, endpoint, token.expected_side)?;
                let changed = match token.kind {
                    BatchJobKindV1::Normalize => self.conn.execute(
                        "UPDATE speech_segments
                            SET normalized_transcript=?3,normalizer_version=?4,
                                updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                          WHERE id=?1 AND review_revision=?2",
                        params![
                            action.segment_id,
                            action.current_revision,
                            action.target.segment.normalized_transcript,
                            action.target.segment.normalizer_version,
                        ],
                    )?,
                    BatchJobKindV1::Transcribe => self.conn.execute(
                        "UPDATE speech_segments
                            SET raw_transcript=?3,normalized_transcript=?4,confidence=?5,
                                confidence_source=?6,model_version_id=?7,cloud_call=?8,
                                decoder_config_hash=?9,normalizer_version=?10,
                                updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                          WHERE id=?1 AND review_revision=?2",
                        params![
                            action.segment_id,
                            action.current_revision,
                            action.target.segment.raw_transcript,
                            action.target.segment.normalized_transcript,
                            action.target.segment.confidence,
                            action.target.segment.confidence_source,
                            action.target.segment.model_version_id,
                            action.target.segment.cloud_call as i32,
                            action.target.segment.decoder_config_hash,
                            action.target.segment.normalizer_version,
                        ],
                    )?,
                };
                if changed != 1 {
                    return Err(AppError::Validation(format!(
                        "BATCH_HISTORY_CONFLICT: segment '{}' rejected its monotonic inverse",
                        action.segment_id
                    )));
                }
                if token.kind == BatchJobKindV1::Transcribe {
                    self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id=?1", [&action.segment_id])?;
                    let mut insert = self.conn.prepare(
                        "INSERT INTO segment_hypotheses(
                             segment_id,model_id,transcript,confidence,model_version_id,created_at)
                         VALUES(?1,?2,?3,?4,?5,?6)",
                    )?;
                    for hypothesis in &action.target.hypotheses {
                        insert.execute(params![
                            hypothesis.segment_id,
                            hypothesis.model_id,
                            hypothesis.transcript,
                            hypothesis.confidence,
                            hypothesis.model_version_id,
                            hypothesis.created_at,
                        ])?;
                    }
                    drop(insert);
                }
                let restored = Self::read_batch_projection_on(&self.conn, &action.segment_id)?
                    .ok_or_else(|| batch_evidence_error("history target vanished before after-read"))?;
                if restored.review_revision <= action.current_revision
                    || projection_semantic_sha256(&restored)? != projection_semantic_sha256(&action.target)?
                {
                    return Err(batch_evidence_error(format!(
                        "history restore for '{}' does not equal its immutable journal endpoint",
                        action.segment_id
                    )));
                }
                let (_, restored_sha256, _) = projection_authority(&restored)?;
                next_items.push(BatchHistoryItemTokenV1 {
                    ordinal: action.ordinal,
                    segment_id: action.segment_id,
                    expected_projection_sha256: restored_sha256,
                    expected_review_revision: restored.review_revision,
                });
            }
            Ok(BatchHistoryTokenV1 {
                operation_id: token.operation_id.clone(),
                kind: token.kind,
                expected_side: token.expected_side.opposite(),
                items: next_items,
            })
        })();
        match result {
            Ok(next) => {
                self.release_savepoint("batch_v1_history")?;
                self.track_write()?;
                Ok(next)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("batch_v1_history");
                Err(error)
            }
        }
    }
}
