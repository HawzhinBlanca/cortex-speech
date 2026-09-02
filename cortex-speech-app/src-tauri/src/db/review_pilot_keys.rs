use super::*;

impl Database {
    /// Return this reviewer's complete durable hidden-key set for one exact controlled-pilot policy.
    pub fn review_pilot_hidden_keys(
        &self,
        policy_sha256: &str,
        after_review_event_id: i64,
        reviewer: &str,
    ) -> AppResult<Vec<String>> {
        let reviewer = validate_review_pilot_hidden_namespace(policy_sha256, after_review_event_id, reviewer)?;
        let mut statement = self.conn.prepare(
            "SELECT segment_id
               FROM review_pilot_hidden_keys
              WHERE policy_sha256 = ?1 AND after_review_event_id = ?2
                AND reviewer = ?3 COLLATE NOCASE
              ORDER BY segment_id",
        )?;
        let keys = statement
            .query_map(params![policy_sha256, after_review_event_id, reviewer], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys)
    }

    /// Atomically bind candidate hidden keys to one reviewer for the lifetime of an exact pilot.
    ///
    /// `BEGIN IMMEDIATE` takes the cross-connection write reservation before either quota read.
    /// Therefore two process-local database handles cannot both consume the final per-reviewer or
    /// global slot.  The migration trigger independently enforces both ceilings for every SQL writer.
    pub fn reserve_review_pilot_hidden_keys(
        &self,
        policy_sha256: &str,
        after_review_event_id: i64,
        reviewer: &str,
        segment_ids: &[String],
        quota: usize,
    ) -> AppResult<Vec<String>> {
        let reviewer = validate_review_pilot_hidden_namespace(policy_sha256, after_review_event_id, reviewer)?;
        let required_quota = usize::try_from(crate::review_pilot::REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER)
            .map_err(|_| AppError::Other("controlled-review hidden-key quota is not representable".into()))?;
        if quota != required_quota {
            return Err(AppError::Validation(format!(
                "controlled-review hidden-key quota must be exactly {required_quota}"
            )));
        }
        let global_quota = usize::try_from(crate::review_pilot::REVIEW_PILOT_TOTAL_HIDDEN_QC)
            .map_err(|_| AppError::Other("controlled-review global hidden-key quota is not representable".into()))?;
        let mut candidates = std::collections::BTreeSet::new();
        for segment_id in segment_ids {
            validate_review_pilot_hidden_segment_id(segment_id)?;
            candidates.insert(segment_id.clone());
            if candidates.len() > required_quota {
                return Err(AppError::Validation(format!(
                    "controlled-review hidden-key reservation exceeds reviewer quota {required_quota}"
                )));
            }
        }

        // Assignment is irreplaceable paid-review state. FULL makes the committed reservation as
        // power-loss durable as the human verdict paths; every exit restores the connection default.
        self.conn.execute_batch("PRAGMA synchronous=FULL;")?;
        let reservation = (|| -> AppResult<(Vec<String>, bool)> {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            let current_max: i64 =
                tx.query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))?;
            if after_review_event_id > current_max {
                return Err(AppError::Validation(
                    "controlled-review baseline is ahead of durable review history".into(),
                ));
            }
            let conflicting_policy: Option<String> = tx
                .query_row(
                    "SELECT policy_sha256 FROM review_pilot_hidden_keys
                      WHERE after_review_event_id = ?1 AND policy_sha256 <> ?2
                      LIMIT 1",
                    params![after_review_event_id, policy_sha256],
                    |row| row.get(0),
                )
                .optional()?;
            if conflicting_policy.is_some() {
                return Err(AppError::Validation(
                    "controlled-review baseline is already bound to another policy identity".into(),
                ));
            }

            let mut statement = tx.prepare(
                "SELECT segment_id
                   FROM review_pilot_hidden_keys
                  WHERE policy_sha256 = ?1 AND after_review_event_id = ?2
                    AND reviewer = ?3 COLLATE NOCASE
                  ORDER BY segment_id",
            )?;
            let existing = statement
                .query_map(params![policy_sha256, after_review_event_id, reviewer], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            if existing.len() > required_quota {
                return Err(AppError::Other("controlled-review hidden-key history exceeds the reviewer quota".into()));
            }

            // Schema-58/session-file deployments may already have completed checks when v59 first
            // opens.  Their post-baseline couch_spot_check events are durable, unambiguous evidence
            // that those keys consumed this pilot's lifetime budget.  Backfill them in this same
            // reservation transaction before considering any fresh session candidates; otherwise a
            // lost session could mint two replacements after the reviewer had already been paid.
            let mut statement = tx.prepare(
                "SELECT DISTINCT segment_id
                   FROM review_events
                  WHERE id > ?1 AND reviewer = ?2 COLLATE NOCASE
                    AND source = 'couch_spot_check'
                  ORDER BY segment_id",
            )?;
            let completed = statement
                .query_map(params![after_review_event_id, reviewer], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            for segment_id in &completed {
                validate_review_pilot_hidden_segment_id(segment_id)?;
            }
            let mut complete: std::collections::BTreeSet<String> = existing.iter().cloned().collect();
            complete.extend(candidates.iter().cloned());
            complete.extend(completed);
            if complete.len() > required_quota {
                return Err(AppError::Validation(format!(
                    "controlled-review hidden-key reservation exceeds reviewer quota {required_quota}"
                )));
            }

            // A hidden quality check must be blind. The append-only event log—not reviewed_by on
            // the mutable segment row—is the durable evidence that this reviewer encountered a clip.
            // Allow only this pilot's own post-baseline hidden result (or its legacy skip path) when
            // rehydrating a completed reservation; any ordinary or older encounter makes the key
            // ineligible forever for this reviewer. Validate inside the same IMMEDIATE transaction
            // so a concurrent review cannot race candidate selection and key reservation.
            for segment_id in &complete {
                let exposed: bool = tx.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM review_events
                          WHERE segment_id = ?1
                            AND reviewer = ?2 COLLATE NOCASE
                            AND NOT (
                                id > ?3 AND (
                                    source = 'couch_spot_check'
                                    OR (source = 'couch' AND action = 'skip')
                                )
                            )
                     )",
                    params![segment_id, reviewer, after_review_event_id],
                    |row| row.get(0),
                )?;
                if exposed {
                    return Err(AppError::Validation(format!(
                        "controlled-review hidden key {segment_id} was already seen by {reviewer}"
                    )));
                }
            }

            let global_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM review_pilot_hidden_keys
                  WHERE policy_sha256 = ?1 AND after_review_event_id = ?2",
                params![policy_sha256, after_review_event_id],
                |row| row.get(0),
            )?;
            let global_count = usize::try_from(global_count)
                .map_err(|_| AppError::Other("controlled-review global hidden-key count is invalid".into()))?;
            let additional = complete.len().saturating_sub(existing.len());
            if global_count.saturating_add(additional) > global_quota {
                return Err(AppError::Validation(format!(
                    "controlled-review hidden-key reservation exceeds global quota {global_quota}"
                )));
            }

            let mut changed = false;
            for segment_id in complete.iter().filter(|segment_id| !existing.contains(segment_id)) {
                tx.execute(
                    "INSERT INTO review_pilot_hidden_keys
                        (policy_sha256, after_review_event_id, reviewer, segment_id)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![policy_sha256, after_review_event_id, reviewer, segment_id],
                )?;
                changed = true;
            }
            let complete = complete.into_iter().collect();
            tx.commit()?;
            Ok((complete, changed))
        })();
        let reset = self.conn.execute_batch("PRAGMA synchronous=NORMAL;");
        let (complete, changed) = match reservation {
            Ok(value) => {
                reset?;
                value
            }
            Err(error) => {
                let _ = reset;
                return Err(error);
            }
        };
        if changed {
            self.track_write()?;
        }
        Ok(complete)
    }

    /// Whether an exact durable pilot key has either an immutable answer or a later Couch skip.
    pub fn review_pilot_hidden_key_resolved(
        &self,
        policy_sha256: &str,
        after_review_event_id: i64,
        reviewer: &str,
        segment_id: &str,
    ) -> AppResult<bool> {
        let reviewer = validate_review_pilot_hidden_namespace(policy_sha256, after_review_event_id, reviewer)?;
        validate_review_pilot_hidden_segment_id(segment_id)?;
        Ok(self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM review_pilot_hidden_keys key
                  WHERE key.policy_sha256 = ?1
                    AND key.after_review_event_id = ?2
                    AND key.reviewer = ?3 COLLATE NOCASE
                    AND key.segment_id = ?4
                    AND (
                        EXISTS (
                            SELECT 1 FROM spot_checks result
                             WHERE result.segment_id = key.segment_id
                               AND result.reviewer = key.reviewer COLLATE NOCASE
                        )
                        OR EXISTS (
                            SELECT 1 FROM effective_review_events_v60 event
                             WHERE event.review_event_id > key.after_review_event_id
                               AND event.segment_id = key.segment_id
                               AND event.reviewer = key.reviewer COLLATE NOCASE
                               AND event.source = 'couch'
                               AND event.action = 'skip'
                        )
                    )
             )",
            params![policy_sha256, after_review_event_id, reviewer, segment_id],
            |row| row.get(0),
        )?)
    }
}
