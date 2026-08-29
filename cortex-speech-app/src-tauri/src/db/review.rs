use super::*;

impl Database {
    pub(super) fn compensation_audio_identity_tx(
        tx: &rusqlite::Transaction<'_>,
        segment_id: &str,
    ) -> AppResult<(String, &'static str, i64)> {
        let (content_hash, alignment_json, duration_ms): (Option<String>, Option<String>, i64) = tx.query_row(
            "SELECT audio_content_hash, alignment_json, duration_ms FROM speech_segments WHERE id = ?1",
            params![segment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if duration_ms <= 0 {
            return Err(AppError::Validation(format!(
                "review compensation refused: segment {segment_id} has non-positive duration {duration_ms}"
            )));
        }
        if let Some(hash) = content_hash {
            if !is_canonical_audio_content_hash(&hash) {
                return Err(AppError::Validation(format!(
                    "review compensation refused: segment {segment_id} has no canonical PCM content hash"
                )));
            }
            if let Some((start, end)) = canonical_source_span(alignment_json.as_deref()) {
                if !source_span_matches_duration(start, end, duration_ms) {
                    return Err(AppError::Validation(format!(
                        "review compensation refused: segment {segment_id} source span disagrees with decoded duration"
                    )));
                }
                return Ok((
                    format!("audio-segment-v1:{hash}:{start}:{end}"),
                    "audio_content_hash+source_span",
                    duration_ms,
                ));
            }
        }
        Err(AppError::Validation(format!(
            "review compensation refused: segment {segment_id} lacks a canonical PCM content hash and valid source span"
        )))
    }

    pub(super) fn verify_review_pay_policy_tx(tx: &rusqlite::Transaction<'_>) -> AppResult<i64> {
        let row: (i64, i64, i64, i64, i64, i64) = tx.query_row(
            "SELECT effective_after_event_id, base_rate_micro_iqd_per_hour,
                    edit_basis_points, accept_basis_points, reject_basis_points, skip_basis_points
               FROM review_compensation_policies WHERE policy_version = ?1",
            params![REVIEW_PAY_POLICY_VERSION],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )?;
        let expected = (
            REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR,
            REVIEW_PAY_EDIT_BPS,
            REVIEW_PAY_ACCEPT_BPS,
            REVIEW_PAY_REJECT_BPS,
            REVIEW_PAY_SKIP_BPS,
        );
        if (row.1, row.2, row.3, row.4, row.5) != expected {
            return Err(AppError::Other(format!(
                "review compensation policy row disagrees with certified binary {}",
                REVIEW_PAY_POLICY_VERSION
            )));
        }
        Ok(row.0)
    }

    /// Look up the immutable receipt for a client operation. Used before and after a write attempt:
    /// before, to acknowledge a lost-response replay; after a UNIQUE race, to distinguish the same
    /// request (safe success) from UUID reuse with a different payload (hard conflict).
    pub fn review_operation(&self, operation_id: &str) -> AppResult<Option<ReviewOperationReceipt>> {
        use rusqlite::OptionalExtension;

        let parsed = uuid::Uuid::parse_str(operation_id)
            .map_err(|_| AppError::Validation("review operation id must be a canonical UUID".into()))?;
        if parsed.hyphenated().to_string() != operation_id {
            return Err(AppError::Validation("review operation id must be a lowercase hyphenated UUID".into()));
        }
        Ok(self
            .conn
            .query_row(
                "SELECT operation_id, operation_payload_hash, id, segment_id, reviewer, action,
                        compensation_action
                   FROM review_events WHERE operation_id = ?1",
                params![operation_id],
                |row| {
                    Ok(ReviewOperationReceipt {
                        operation_id: row.get(0)?,
                        operation_payload_hash: row.get(1)?,
                        review_event_id: row.get(2)?,
                        segment_id: row.get(3)?,
                        reviewer: row.get(4)?,
                        action: row.get(5)?,
                        compensation_action: row.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn human_decision_effect_for_operation(&self, operation_id: &str) -> AppResult<Option<(i64, String)>> {
        validate_operation_uuid(operation_id)?;
        Ok(self
            .conn
            .query_row(
                "SELECT e.id, e.segment_id
                   FROM human_decision_effect_events e
                   JOIN review_events r ON r.id = e.review_event_id
                  WHERE r.operation_id = ?1",
                params![operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Durable bodyless-phone Undo target. Select the latest Couch decision for this reviewer even
    /// when it is already reversed: a lost Undo response or process restart must replay that same
    /// idempotent inverse, never fall through and retract an older decision.
    pub(crate) fn latest_phone_human_decision_effect(
        &self,
        reviewer: &str,
    ) -> AppResult<Option<(i64, String, String)>> {
        let reviewer = reviewer.trim();
        if reviewer.is_empty() {
            return Err(AppError::Validation("phone undo reviewer must not be blank".into()));
        }
        Ok(self
            .conn
            .query_row(
                "SELECT effect.id, event.operation_id, effect.segment_id
                   FROM human_decision_effect_events effect
                   JOIN review_events event ON event.id = effect.review_event_id
                  WHERE effect.source = 'couch'
                    AND effect.reviewer = ?1 COLLATE NOCASE
                    AND event.source = 'couch'
                  ORDER BY effect.id DESC
                  LIMIT 1",
                params![reviewer],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    /// Latest active desktop decision that can be offered as restart-safe Undo authority.
    ///
    /// A durable reversal is also a process-crash barrier: once one Undo lands, older decisions are
    /// not offered after restart until a newer desktop decision exists. This prevents a lost Undo
    /// response followed by a crash from turning the next Backspace into an accidental second Undo.
    pub(crate) fn desktop_review_undo_availability(&self) -> AppResult<DesktopReviewUndoAvailability> {
        Self::desktop_review_undo_availability_on(&self.conn)
    }

    /// Full v69 ledger proof for startup and restore admission. Interactive availability and Undo
    /// use the trigger-maintained journal tail in O(1); scanning the complete immutable source and
    /// journal sets belongs at these lifecycle boundaries, not on every renderer refresh.
    pub(crate) fn validate_desktop_review_action_journal(&self) -> AppResult<()> {
        Self::validate_desktop_review_action_journal_on(&self.conn)
    }

    pub(super) fn desktop_review_undo_availability_on(conn: &Connection) -> AppResult<DesktopReviewUndoAvailability> {
        let latest_action: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT action_kind,effect_event_id
                   FROM desktop_review_action_events_v1
                  ORDER BY id DESC
                  LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((action_kind, effect_event_id)) = latest_action else {
            return Ok(DesktopReviewUndoAvailability::NoHistory);
        };
        let blocked = match action_kind.as_str() {
            "legacy_barrier" => Some(DesktopReviewUndoBlockReason::LegacyHistory),
            "decision_undo" => Some(DesktopReviewUndoBlockReason::LatestDecisionUndone),
            "flag_undo" => Some(DesktopReviewUndoBlockReason::LatestFlagUndone),
            "decision" | "flag" => None,
            _ => {
                return Err(AppError::Other("desktop review action journal contains an invalid action kind".into()));
            }
        };
        if let Some(reason) = blocked {
            return Ok(DesktopReviewUndoAvailability::Blocked(reason));
        }
        let effect_event_id = effect_event_id
            .ok_or_else(|| AppError::Other("desktop review action journal is missing its effect identity".into()))?;
        if action_kind == "flag" {
            let effect = conn
                .query_row(
                    "SELECT effect.id, effect.segment_id, effect.operation_id,
                            effect.prior_revision, effect.flag_revision, effect.flag_rationale
                       FROM review_flag_effect_events effect
                       JOIN speech_segments segment ON segment.id=effect.segment_id
                       LEFT JOIN review_flag_effect_reversals reversal
                         ON reversal.flag_effect_event_id=effect.id
                      WHERE effect.id=?1
                        AND reversal.flag_effect_event_id IS NULL
                        AND segment.review_revision >= effect.flag_revision
                        AND segment.verdict = 'escalated'
                        AND segment.rationale IS effect.flag_rationale
                        AND segment.escalated = 1
                        AND (segment.human_decision IS NULL OR segment.human_decision = '')
                        AND NOT EXISTS (
                            SELECT 1
                              FROM review_flag_effect_events newer
                             WHERE newer.segment_id=effect.segment_id
                               AND newer.flag_revision > effect.flag_revision
                        )
                        AND NOT EXISTS (
                            SELECT 1
                              FROM human_decision_effect_events newer
                             WHERE newer.segment_id=effect.segment_id
                               AND newer.decision_revision > effect.flag_revision
                        )
                      LIMIT 1",
                    [effect_event_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    },
                )
                .optional()?;
            return Ok(match effect {
                Some((effect_event_id, segment_id, flag_operation_id, prior_revision, flag_revision, rationale)) => {
                    let flag_kind = match crate::quality::technical_unusable_reason_from_rationale(Some(&rationale)) {
                        Some(reason) => DesktopReviewFlagKind::TechnicalUnusable(reason.to_string()),
                        None if rationale.starts_with(crate::quality::TECHNICAL_UNUSABLE_RATIONALE_PREFIX) => {
                            return Err(AppError::Other(
                                "desktop review flag contains a malformed reserved technical rationale".into(),
                            ));
                        }
                        None => DesktopReviewFlagKind::Generic,
                    };
                    DesktopReviewUndoAvailability::Available(DesktopReviewUndoAuthority::Flag(
                        DesktopReviewFlagUndoAuthority {
                            effect_event_id,
                            flag_payload_hash: desktop_review_flag_payload_hash(
                                &segment_id,
                                prior_revision,
                                &rationale,
                            ),
                            segment_id,
                            flag_operation_id,
                            prior_revision,
                            flag_revision,
                            flag_kind,
                        },
                    ))
                }
                None => DesktopReviewUndoAvailability::Blocked(DesktopReviewUndoBlockReason::FlagShadowed),
            });
        }
        let authority = conn
            .query_row(
                "SELECT effect.id, effect.segment_id, effect.action,
                        effect.operation_id, effect.operation_payload_hash
                   FROM human_decision_effect_events effect
                   JOIN speech_segments segment ON segment.id=effect.segment_id
                   LEFT JOIN human_decision_effect_reversals reversal
                     ON reversal.effect_event_id=effect.id
                  WHERE effect.source = 'desktop'
                    AND effect.reviewer IS NULL
                    AND effect.id=?1
                    AND reversal.effect_event_id IS NULL
                    AND segment.review_revision >= effect.decision_revision
                    AND segment.human_decision = effect.action
                    AND segment.verdict = CASE effect.action
                        WHEN 'accept' THEN 'human_accept'
                        WHEN 'edit' THEN 'human_edit'
                        WHEN 'reject' THEN 'human_reject'
                    END
                    AND segment.escalated = 0
                    AND segment.reviewed_by IS effect.reviewer
                    AND segment.verified = effect.decision_verified
                    AND segment.annotated_transcript IS effect.decision_annotated_transcript
                    AND segment.verdict_transcript IS CASE
                        WHEN effect.action='reject' THEN effect.prior_verdict_transcript
                        ELSE effect.decision_transcript
                    END
                    AND segment.corrected_at = effect.decision_corrected_at
                    AND segment.rationale IS effect.decision_rationale
                    AND NOT EXISTS (
                        SELECT 1
                          FROM human_decision_effect_events newer
                         WHERE newer.segment_id = effect.segment_id
                           AND newer.decision_revision > effect.decision_revision
                    )
                    AND NOT EXISTS (
                        SELECT 1
                          FROM review_flag_effect_events flag
                         WHERE flag.segment_id = effect.segment_id
                           AND flag.flag_revision > effect.decision_revision
                    )
                  LIMIT 1",
                [effect_event_id],
                |row| {
                    Ok(DesktopHumanDecisionUndoAuthority {
                        effect_event_id: row.get(0)?,
                        segment_id: row.get(1)?,
                        action: row.get(2)?,
                        decision_operation_id: row.get(3)?,
                        decision_payload_hash: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(match authority {
            Some(authority) => {
                DesktopReviewUndoAvailability::Available(DesktopReviewUndoAuthority::Decision(authority))
            }
            None => DesktopReviewUndoAvailability::Blocked(DesktopReviewUndoBlockReason::DecisionShadowed),
        })
    }

    /// Prove that the v69 total-order journal is neither missing, duplicating nor inventing a
    /// desktop review action. Pre-v69 rows are enumerated in the sealed legacy baseline; every
    /// action created after that boundary must have exactly one trigger-authored journal row.
    pub(super) fn validate_desktop_review_action_journal_on(conn: &Connection) -> AppResult<()> {
        let invalid: bool = conn.query_row(
            "WITH source_actions(action_kind,effect_event_id) AS (
                 SELECT 'decision',id FROM human_decision_effect_events
                  WHERE source='desktop' AND reviewer IS NULL
                 UNION ALL
                 SELECT 'decision_undo',reversal.effect_event_id
                   FROM human_decision_effect_reversals reversal
                   JOIN human_decision_effect_events effect ON effect.id=reversal.effect_event_id
                  WHERE effect.source='desktop' AND effect.reviewer IS NULL
                 UNION ALL SELECT 'flag',id FROM review_flag_effect_events
                 UNION ALL SELECT 'flag_undo',flag_effect_event_id FROM review_flag_effect_reversals
             ),
             recorded_actions(action_kind,effect_event_id) AS (
                 SELECT source_kind,effect_event_id FROM desktop_review_legacy_actions_v1
                 UNION ALL
                 SELECT action_kind,effect_event_id FROM desktop_review_action_events_v1
                  WHERE action_kind<>'legacy_barrier'
             )
             SELECT
                 EXISTS(SELECT action_kind,effect_event_id FROM source_actions
                        EXCEPT SELECT action_kind,effect_event_id FROM recorded_actions)
                 OR EXISTS(SELECT action_kind,effect_event_id FROM recorded_actions
                           EXCEPT SELECT action_kind,effect_event_id FROM source_actions)
                 OR EXISTS(SELECT 1 FROM recorded_actions
                            GROUP BY action_kind,effect_event_id HAVING COUNT(*)<>1)
                 OR ((SELECT COUNT(*) FROM desktop_review_action_events_v1
                       WHERE action_kind='legacy_barrier') <>
                     CASE WHEN EXISTS(SELECT 1 FROM desktop_review_legacy_actions_v1) THEN 1 ELSE 0 END)
                 OR EXISTS(
                     SELECT 1
                       FROM desktop_review_action_events_v1 action
                      WHERE action.action_kind<>'legacy_barrier'
                        AND action.id < (
                            SELECT barrier.id
                              FROM desktop_review_action_events_v1 barrier
                             WHERE barrier.action_kind='legacy_barrier'
                        )
                 )
                 OR EXISTS(
                     SELECT 1 FROM desktop_review_action_events_v1 inverse
                      WHERE inverse.action_kind='decision_undo'
                        AND NOT EXISTS (
                            SELECT 1 FROM desktop_review_legacy_actions_v1 legacy
                             WHERE legacy.source_kind='decision'
                               AND legacy.effect_event_id=inverse.effect_event_id
                        )
                        AND NOT EXISTS (
                            SELECT 1 FROM desktop_review_action_events_v1 original
                             WHERE original.action_kind='decision'
                               AND original.effect_event_id=inverse.effect_event_id
                               AND original.id<inverse.id
                        )
                 )
                 OR EXISTS(
                     SELECT 1 FROM desktop_review_action_events_v1 inverse
                      WHERE inverse.action_kind='flag_undo'
                        AND NOT EXISTS (
                            SELECT 1 FROM desktop_review_legacy_actions_v1 legacy
                             WHERE legacy.source_kind='flag'
                               AND legacy.effect_event_id=inverse.effect_event_id
                        )
                        AND NOT EXISTS (
                            SELECT 1 FROM desktop_review_action_events_v1 original
                             WHERE original.action_kind='flag'
                               AND original.effect_event_id=inverse.effect_event_id
                               AND original.id<inverse.id
                        )
                 )",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            Err(AppError::Other(
                "desktop review action journal does not exactly match immutable decision and flag authority".into(),
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn require_latest_desktop_review_action_on(
        conn: &Connection,
        expected_kind: &str,
        expected_effect_event_id: i64,
    ) -> AppResult<()> {
        let latest: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT action_kind,effect_event_id
                   FROM desktop_review_action_events_v1
                  ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if latest
            .as_ref()
            .is_some_and(|(kind, effect_id)| kind == expected_kind && *effect_id == Some(expected_effect_event_id))
        {
            Ok(())
        } else {
            Err(AppError::Validation("review Undo target is no longer the globally current desktop action".into()))
        }
    }

    pub(super) fn desktop_human_decision_replay_on(
        conn: &Connection,
        operation_id: &str,
        operation_payload_hash: &str,
        segment_id: &str,
    ) -> AppResult<Option<HumanDecisionCommit>> {
        let existing: Option<DesktopReplayEffect> = conn
            .query_row(
                "SELECT id, segment_id, source, reviewer, operation_payload_hash, action,
                        decision_transcript, decision_annotated_transcript, decision_verified,
                        decision_corrected_at, decision_rationale, requested_action, requested_transcript,
                        requested_timestamp_ms, prior_revision, decision_revision,
                        prior_verdict_transcript, desktop_review_contract_version,
                        playback_authority_session_id
                   FROM human_decision_effect_events
                  WHERE operation_id = ?1",
                params![operation_id],
                |row| {
                    Ok(DesktopReplayEffect {
                        id: row.get(0)?,
                        segment_id: row.get(1)?,
                        source: row.get(2)?,
                        reviewer: row.get(3)?,
                        operation_payload_hash: row.get(4)?,
                        action: row.get(5)?,
                        decision_transcript: row.get(6)?,
                        decision_annotated_transcript: row.get(7)?,
                        decision_verified: row.get::<_, i32>(8)? != 0,
                        decision_corrected_at: row.get(9)?,
                        decision_rationale: row.get(10)?,
                        requested_action: row.get(11)?,
                        requested_transcript: row.get(12)?,
                        requested_timestamp_ms: row.get(13)?,
                        prior_revision: row.get(14)?,
                        decision_revision: row.get(15)?,
                        prior_verdict_transcript: row.get(16)?,
                        desktop_review_contract_version: row.get(17)?,
                        playback_authority_session_id: row.get(18)?,
                    })
                },
            )
            .optional()?;
        let Some(effect) = existing else {
            return Ok(None);
        };
        let legacy_payload_hash = desktop_decision_payload_hash(
            &effect.segment_id,
            &effect.requested_action,
            effect.requested_transcript.as_deref(),
            Some(effect.requested_timestamp_ms),
        );
        let typed_payload_hash = effect.playback_authority_session_id.as_deref().map(|authority_id| {
            desktop_review_v1_payload_hash(
                &effect.segment_id,
                effect.prior_revision,
                &effect.requested_action,
                effect.requested_transcript.as_deref(),
                authority_id,
            )
        });
        let stored_payload_is_canonical = match effect.desktop_review_contract_version {
            Some(1) => typed_payload_hash.as_deref() == Some(effect.operation_payload_hash.as_str()),
            // Historical desktop-v1 effects predate exact authority persistence. Keep them readable
            // and replayable only by their historical digest; every new typed effect is version 1.
            None => {
                legacy_payload_hash == effect.operation_payload_hash
                    || legacy_desktop_review_v1_payload_hash(
                        &effect.segment_id,
                        effect.prior_revision,
                        &effect.requested_action,
                        effect.requested_transcript.as_deref(),
                    ) == effect.operation_payload_hash
            }
            Some(_) => false,
        };
        if effect.segment_id != segment_id
            || effect.operation_payload_hash != operation_payload_hash
            || !stored_payload_is_canonical
        {
            return Err(AppError::Validation(
                "desktop decision operation UUID was already used for a different canonical payload".into(),
            ));
        }
        if effect.source != "desktop" || effect.reviewer.is_some() {
            return Err(AppError::Other("desktop decision operation is bound to a non-desktop effect".into()));
        }
        let Some((segment, current_revision, _)) = Self::decision_snapshot_on(conn, segment_id)? else {
            return Err(AppError::Validation(
                "desktop decision operation committed, but its segment no longer exists".into(),
            ));
        };
        let expected_verdict = human_verdict_for_decision(&effect.action)?;
        let expected_verdict_transcript = if effect.action == "reject" {
            effect.prior_verdict_transcript.as_deref()
        } else {
            effect.decision_transcript.as_deref()
        };
        let later_review_mutation: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM human_decision_effect_reversals reversal
                  WHERE reversal.effect_event_id = ?1
                 UNION ALL
                 SELECT 1
                   FROM human_decision_effect_events newer
                  WHERE newer.segment_id = ?2
                    AND newer.decision_revision > ?3
                 UNION ALL
                 SELECT 1
                   FROM review_flag_effect_events flag
                  WHERE flag.segment_id = ?2
                    AND flag.flag_revision > ?3
             )",
            params![effect.id, segment_id, effect.decision_revision],
            |row| row.get(0),
        )?;
        if current_revision < effect.decision_revision
            || later_review_mutation
            || segment.human_decision.as_deref() != Some(effect.action.as_str())
            || segment.verdict.as_deref() != Some(expected_verdict)
            || segment.escalated
            || segment.reviewed_by.is_some()
            || segment.verified != effect.decision_verified
            || segment.annotated_transcript.as_deref() != effect.decision_annotated_transcript.as_deref()
            || segment.verdict_transcript.as_deref() != expected_verdict_transcript
            || segment.corrected_at.as_deref() != Some(effect.decision_corrected_at.as_str())
            || segment.rationale != effect.decision_rationale
        {
            return Err(AppError::Validation(
                "desktop decision operation committed, but its exact post-state is no longer current".into(),
            ));
        }
        Ok(Some(HumanDecisionCommit {
            effect_event_id: effect.id,
            segment_id: effect.segment_id,
            effective_action: effect.action,
            prior_revision: effect.prior_revision,
            decided_revision: effect.decision_revision,
            segment,
        }))
    }

    /// Lost-response preflight for the desktop IPC. It deliberately runs before playback/revision
    /// lookup: a committed decision advanced the row, so fresh evidence for the old revision no
    /// longer exists. The same check is repeated under BEGIN IMMEDIATE in the writer for races.
    #[cfg(test)]
    pub(crate) fn replay_desktop_human_decision(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        operation_id: &str,
    ) -> AppResult<Option<HumanDecisionCommit>> {
        validate_operation_uuid(operation_id)?;
        human_verdict_for_decision(decision)?;
        if !timestamp_ms.is_some_and(|timestamp| timestamp > 0) {
            return Err(AppError::Validation(
                "replayable desktop decisions require a positive request timestamp".into(),
            ));
        }
        let operation_payload_hash =
            desktop_decision_payload_hash(segment_id, decision, corrected_transcript, timestamp_ms);
        Self::desktop_human_decision_replay_on(&self.conn, operation_id, &operation_payload_hash, segment_id)
    }

    /// Lost-response preflight for the typed review IPC. The base revision, rather than a client
    /// clock, is part of the immutable request identity.
    pub(crate) fn replay_desktop_review_v1_and_clear_draft(
        &self,
        segment_id: &str,
        base_revision: i64,
        decision: &str,
        corrected_transcript: Option<&str>,
        playback_receipt_id: &str,
        operation_id: &str,
    ) -> AppResult<Option<HumanDecisionCommit>> {
        validate_operation_uuid(operation_id)?;
        if base_revision < 0 {
            return Err(AppError::Validation("human decision revision must be non-negative".into()));
        }
        human_verdict_for_decision(decision)?;
        let operation_payload_hash = desktop_review_v1_payload_hash(
            segment_id,
            base_revision,
            decision,
            corrected_transcript,
            playback_receipt_id,
        );
        let (commit, cleared_draft) = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            let Some(commit) =
                Self::desktop_human_decision_replay_on(&tx, operation_id, &operation_payload_hash, segment_id)?
            else {
                tx.rollback()?;
                return Ok((None, false));
            };
            let cleared_draft = tx.execute(
                "DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2",
                params![segment_id, base_revision],
            )? > 0;
            tx.commit()?;
            Ok((Some(commit), cleared_draft))
        })?;
        if cleared_draft {
            self.track_write()?;
        }
        Ok(commit)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn append_review_compensation_tx(
        tx: &rusqlite::Transaction<'_>,
        review_event_id: i64,
        segment_id: &str,
        reviewer: &str,
        source: &str,
        compensation_action: &str,
        effective_decision: &str,
        decision_revision: Option<i64>,
    ) -> AppResult<()> {
        let cutoff = Self::verify_review_pay_policy_tx(tx)?;
        if review_event_id <= cutoff {
            return Err(AppError::Other(format!(
                "new review event {review_event_id} did not fall after policy cutoff {cutoff}"
            )));
        }
        let basis_points = review_pay_basis_points(compensation_action)?;
        let (audio_work_id, identity_kind, duration_ms) = Self::compensation_audio_identity_tx(tx, segment_id)?;
        let reviewer_key = reviewer.trim().to_lowercase();
        if reviewer_key.is_empty() {
            return Err(AppError::Validation("review compensation requires a named reviewer".into()));
        }
        let canonical_work_id = format!("reviewer-work-v1:{}:{reviewer_key}:{audio_work_id}", reviewer_key.len());
        let (prior_entitlement, prior_corrected_ms): (i64, i64) = tx.query_row(
            "SELECT COALESCE(SUM(delta_micro_iqd), 0),
                    COALESCE(SUM(delta_corrected_ms), 0)
               FROM review_compensation_ledger
              WHERE policy_version = ?1 AND canonical_work_id = ?2",
            params![REVIEW_PAY_POLICY_VERSION, canonical_work_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if prior_corrected_ms < 0 {
            return Err(AppError::Other(format!(
                "review compensation corrected-audio balance is negative for {canonical_work_id}"
            )));
        }
        let entitlement = review_pay_entitlement_micro_iqd(duration_ms, basis_points)?;
        // Skip is an explicit no-verdict. It must neither mint money nor retract a previous valid
        // entitlement if a strange legacy/replay path reaches the same work identity.
        let delta = if compensation_action == "skip" {
            0
        } else {
            entitlement
                .checked_sub(prior_entitlement)
                .ok_or_else(|| AppError::Other("review compensation adjustment overflow".into()))?
        };
        // Correction time is its own signed entitlement, not inferred from a money balance. A skip
        // leaves the active state untouched; accept/reject clear correction entitlement; edit owns
        // the exact duration snapshot used by this ledger entry.
        let corrected_entitlement_ms = match compensation_action {
            "edit" => duration_ms,
            "accept" | "reject" => 0,
            "skip" => prior_corrected_ms,
            other => return Err(AppError::Validation(format!("unsupported corrected-audio action {other:?}"))),
        };
        let delta_corrected_ms = corrected_entitlement_ms
            .checked_sub(prior_corrected_ms)
            .ok_or_else(|| AppError::Other("corrected-audio adjustment overflow".into()))?;
        let entry_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO review_compensation_ledger
                (entry_id, entry_key, policy_version, review_event_id, canonical_work_id,
                 canonical_identity_kind, reviewer, segment_id, source, compensation_action,
                 effective_decision, decision_revision, duration_ms, rate_basis_points,
                 entitlement_micro_iqd, delta_micro_iqd, corrected_entitlement_ms,
                 delta_corrected_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18)",
            params![
                entry_id,
                format!("review-event:{review_event_id}"),
                REVIEW_PAY_POLICY_VERSION,
                review_event_id,
                canonical_work_id,
                identity_kind,
                reviewer,
                segment_id,
                source,
                compensation_action,
                effective_decision,
                decision_revision,
                duration_ms,
                basis_points,
                entitlement,
                delta,
                corrected_entitlement_ms,
                delta_corrected_ms,
            ],
        )?;
        Ok(())
    }

    /// Append the exact signed inverse of the ledger row bound to one phone decision effect.
    /// Nothing is re-derived from the current segment: the immutable original entry is the authority.
    pub(super) fn append_review_compensation_reversal_for_effect_tx(
        tx: &rusqlite::Transaction<'_>,
        effect_event_id: i64,
        operation_id: &str,
    ) -> AppResult<()> {
        Self::verify_review_pay_policy_tx(tx)?;
        let original: (String, String, String, String, String, String, i64, i64, i64, i64) = tx.query_row(
            "SELECT l.entry_id, l.policy_version, l.canonical_work_id,
                    l.canonical_identity_kind, l.reviewer, l.segment_id,
                    l.decision_revision, l.duration_ms, l.delta_micro_iqd,
                    l.delta_corrected_ms
               FROM human_decision_effect_events e
               JOIN review_compensation_ledger l ON l.review_event_id = e.review_event_id
              WHERE e.id = ?1 AND e.review_event_id IS NOT NULL
                AND l.reverses_entry_id IS NULL",
            params![effect_event_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )?;
        if original.1 != REVIEW_PAY_POLICY_VERSION || original.3 != "audio_content_hash+source_span" || original.7 <= 0
        {
            return Err(AppError::Validation(
                "phone undo refused: original compensation identity is not canonical policy evidence".into(),
            ));
        }
        let latest_unreversed_entry: Option<String> = tx
            .query_row(
                "SELECT candidate.entry_id
                   FROM review_compensation_ledger candidate
                  WHERE candidate.policy_version = ?1
                    AND candidate.canonical_work_id = ?2
                    AND candidate.review_event_id IS NOT NULL
                    AND candidate.reverses_entry_id IS NULL
                    AND NOT EXISTS (
                         SELECT 1 FROM review_compensation_ledger reversal
                          WHERE reversal.reverses_entry_id = candidate.entry_id
                    )
                  ORDER BY candidate.id DESC
                  LIMIT 1",
                params![original.1, original.2],
                |row| row.get(0),
            )
            .optional()?;
        if latest_unreversed_entry.as_deref() != Some(original.0.as_str()) {
            return Err(AppError::Validation(
                "phone undo refused: a newer active entitlement mutation owns this canonical audio work".into(),
            ));
        }
        let current_corrected_ms: i64 = tx.query_row(
            "SELECT COALESCE(SUM(delta_corrected_ms), 0)
               FROM review_compensation_ledger
              WHERE policy_version = ?1 AND canonical_work_id = ?2",
            params![original.1, original.2],
            |row| row.get(0),
        )?;
        let reversal_delta =
            original.8.checked_neg().ok_or_else(|| AppError::Other("review reversal overflow".into()))?;
        let reversal_corrected_delta =
            original.9.checked_neg().ok_or_else(|| AppError::Other("corrected-audio reversal overflow".into()))?;
        let corrected_entitlement_ms = current_corrected_ms
            .checked_add(reversal_corrected_delta)
            .ok_or_else(|| AppError::Other("corrected-audio reversal balance overflow".into()))?;
        if corrected_entitlement_ms < 0 {
            return Err(AppError::Other("corrected-audio reversal would produce a negative entitlement".into()));
        }
        tx.execute(
            "INSERT INTO review_compensation_ledger
                (entry_id, entry_key, policy_version, canonical_work_id, canonical_identity_kind,
                 reviewer, segment_id, source, compensation_action, effective_decision,
                 decision_revision, duration_ms, rate_basis_points, entitlement_micro_iqd,
                 delta_micro_iqd, corrected_entitlement_ms, delta_corrected_ms,
                 reverses_entry_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'couch_undo', 'undo', 'undo',
                     ?8, ?9, 0, 0, ?10, ?11, ?12, ?13)",
            params![
                uuid::Uuid::new_v4().to_string(),
                format!("undo:{operation_id}"),
                original.1,
                original.2,
                original.3,
                original.4,
                original.5,
                original.6,
                original.7,
                reversal_delta,
                corrected_entitlement_ms,
                reversal_corrected_delta,
                original.0,
            ],
        )?;
        Ok(())
    }

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

    /// Record how a reviewer answered one spot check. The FIRST answer is immutable: a network retry
    /// cannot inflate the score, and a reviewer cannot improve a failed hidden check by submitting a
    /// different answer later. `ON CONFLICT DO NOTHING` still makes a lost-response replay idempotent.
    ///
    /// The score, audit event, and compensation consequence share one transaction. Grading never
    /// alters the corpus row, but losing the pay event after accepting a score would still lose real
    /// reviewer work and make a dropped-response retry impossible to reconcile.
    ///
    /// TEST-ONLY, like its `record_spot_check_with_operation` sibling: it passes `operation: None`,
    /// which clears `enforce_production_proof` inside `record_spot_check_inner` — a PAID ledger write
    /// with no operation identity and no playback proof behind it. Nothing in the shipped app calls
    /// it (couch goes through `record_pilot_spot_check_with_operation_request`), so it is compiled out
    /// of the production binary rather than left there as a proof-free way to move money.
    #[cfg(test)]
    pub(crate) fn record_spot_check(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        submitted: &str,
        expected: &str,
    ) -> AppResult<()> {
        self.record_spot_check_inner(
            segment_id, reviewer, action, submitted, expected, action, submitted, None, None, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub(crate) fn record_spot_check_with_operation(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        submitted: &str,
        expected: &str,
        playback: Option<&PlaybackDecisionProof>,
        operation_id: &str,
        operation_payload_hash: &str,
    ) -> AppResult<()> {
        validate_review_operation_identity(operation_id, operation_payload_hash)?;
        self.record_spot_check_inner(
            segment_id,
            reviewer,
            action,
            submitted,
            expected,
            action,
            submitted,
            Some((operation_id, operation_payload_hash)),
            None,
            playback,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_spot_check_with_operation_request(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        submitted: &str,
        expected: &str,
        requested_action: &str,
        requested_transcript: &str,
        playback: Option<&PlaybackDecisionProof>,
        operation_id: &str,
        operation_payload_hash: &str,
    ) -> AppResult<()> {
        validate_review_operation_identity(operation_id, operation_payload_hash)?;
        self.record_spot_check_inner(
            segment_id,
            reviewer,
            action,
            submitted,
            expected,
            requested_action,
            requested_transcript,
            Some((operation_id, operation_payload_hash)),
            None,
            playback,
        )
    }

    /// Pilot-scoped spot-check write.  The exact durable reservation is authorized inside the same
    /// transaction and before the first insert, so a stale/repaired session cannot manufacture paid
    /// hidden-check results outside its policy namespace.
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub(crate) fn record_pilot_spot_check_with_operation(
        &self,
        policy_sha256: &str,
        after_review_event_id: i64,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        submitted: &str,
        expected: &str,
        playback: Option<&PlaybackDecisionProof>,
        operation_id: &str,
        operation_payload_hash: &str,
    ) -> AppResult<()> {
        let reviewer = validate_review_pilot_hidden_namespace(policy_sha256, after_review_event_id, reviewer)?;
        validate_review_pilot_hidden_segment_id(segment_id)?;
        validate_review_operation_identity(operation_id, operation_payload_hash)?;
        self.record_spot_check_inner(
            segment_id,
            &reviewer,
            action,
            submitted,
            expected,
            action,
            submitted,
            Some((operation_id, operation_payload_hash)),
            Some((policy_sha256, after_review_event_id)),
            playback,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_pilot_spot_check_with_operation_request(
        &self,
        policy_sha256: &str,
        after_review_event_id: i64,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        submitted: &str,
        expected: &str,
        requested_action: &str,
        requested_transcript: &str,
        playback: Option<&PlaybackDecisionProof>,
        operation_id: &str,
        operation_payload_hash: &str,
    ) -> AppResult<()> {
        let reviewer = validate_review_pilot_hidden_namespace(policy_sha256, after_review_event_id, reviewer)?;
        validate_review_pilot_hidden_segment_id(segment_id)?;
        validate_review_operation_identity(operation_id, operation_payload_hash)?;
        self.record_spot_check_inner(
            segment_id,
            &reviewer,
            action,
            submitted,
            expected,
            requested_action,
            requested_transcript,
            Some((operation_id, operation_payload_hash)),
            Some((policy_sha256, after_review_event_id)),
            playback,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_spot_check_inner(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        submitted: &str,
        expected: &str,
        requested_action: &str,
        requested_transcript: &str,
        operation: Option<(&str, &str)>,
        pilot_namespace: Option<(&str, i64)>,
        playback: Option<&PlaybackDecisionProof>,
    ) -> AppResult<()> {
        let submitted_nfc = to_nfc(submitted.trim());
        let expected_nfc = to_nfc(expected.trim());
        let requested_action = requested_action.trim();
        let requested_transcript_nfc = to_nfc(requested_transcript.trim());
        if !matches!(requested_action, "accept" | "edit" | "reject" | "bad" | "skip") {
            return Err(AppError::Validation("hidden-check request action is outside the client vocabulary".into()));
        }
        let enforce_production_proof = operation.is_some();
        let generated_operation = operation.is_none().then(|| {
            (
                uuid::Uuid::new_v4().to_string(),
                review_operation_payload_hash(segment_id, requested_action, &requested_transcript_nfc, reviewer),
            )
        });
        let operation = operation.or_else(|| {
            generated_operation
                .as_ref()
                .map(|(operation_id, payload_hash)| (operation_id.as_str(), payload_hash.as_str()))
        });
        if let Some((_, payload_hash)) = operation {
            let expected_payload_hash =
                review_operation_payload_hash(segment_id, requested_action, &requested_transcript_nfc, reviewer);
            if payload_hash != expected_payload_hash {
                return Err(AppError::Validation(
                    "hidden-check operation payload hash does not match its exact submitted request".into(),
                ));
            }
        }
        // A hidden check has a known-valid human answer. "Noticed" therefore means the reviewer
        // actually recovered that answer (under the same normalized text key used by the learning
        // paths), not merely that they changed *something*. A blanket reject or arbitrary garbage
        // used to score as attentive and sort a reject-spammer to the top of the trust report.
        let noticed = action != "reject" && learning_text_key(&submitted_nfc) == learning_text_key(&expected_nfc);
        let cer = crate::wer::compute_cer(&expected_nfc, &submitted_nfc);
        self.conn.execute_batch("PRAGMA synchronous=FULL;")?;
        let write = (|| -> AppResult<bool> {
            // Paid hidden-check writes use BEGIN IMMEDIATE: once the in-transaction evidence check
            // passes, no second connection can swap the audio, advance the revision, or remove its
            // receipt before score + event + compensation commit.
            let tx = if enforce_production_proof {
                rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?
            } else {
                self.conn.unchecked_transaction()?
            };
            if let Some((operation_id, _)) = operation {
                Self::require_canonical_operation_namespace_on(&tx, operation_id)?;
            }
            // Resolve the stored reviewer spelling while proving the exact policy+baseline grant.
            // This also prevents a case-only re-pair from bypassing spot_checks' binary reviewer PK.
            let effective_reviewer = if let Some((policy_sha256, after_review_event_id)) = pilot_namespace {
                tx.query_row(
                    "SELECT reviewer FROM review_pilot_hidden_keys
                      WHERE policy_sha256 = ?1 AND after_review_event_id = ?2
                        AND reviewer = ?3 COLLATE NOCASE AND segment_id = ?4",
                    params![policy_sha256, after_review_event_id, reviewer, segment_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    AppError::Validation(
                        "controlled-review pilot hidden-check result has no durable reservation".into(),
                    )
                })?
            } else {
                reviewer.to_string()
            };
            // A committed first answer is immutable. A replay writes and earns nothing, so it needs
            // no fresh playback/key proof; acknowledging it before those checks keeps outbox retries
            // idempotent even if the owner has since changed the answer row.
            let already_recorded: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM spot_checks WHERE segment_id = ?1 AND reviewer = ?2
                 )",
                params![segment_id, effective_reviewer],
                |row| row.get(0),
            )?;
            if already_recorded {
                tx.commit()?;
                return Ok(false);
            }
            if enforce_production_proof && action != "skip" && playback.is_none() {
                tx.rollback()?;
                return Err(AppError::Validation(
                    "E_NO_PLAYBACK_EVIDENCE: a hidden judgement must be bound to verified canonical-media traversal for that clip".into(),
                ));
            }
            if let Some(proof) = playback {
                let sufficient = if let Some(authority_id) = proof.authority_session_id.as_deref() {
                    has_sufficient_desktop_playback_evidence_v4_on(
                        &tx,
                        segment_id,
                        proof.segment_revision,
                        &proof.audio_content_hash,
                        proof.source_start_ms,
                        proof.source_end_ms,
                        Some(reviewer),
                        authority_id,
                    )?
                } else {
                    has_sufficient_playback_evidence_on(
                        &tx,
                        segment_id,
                        proof.segment_revision,
                        &proof.audio_content_hash,
                        proof.source_start_ms,
                        proof.source_end_ms,
                        Some(reviewer),
                    )?
                };
                if !sufficient {
                    tx.rollback()?;
                    return Err(AppError::Validation(format!(
                        "{PLAYBACK_EVIDENCE_CHANGED}: clip identity or playback proof changed while the hidden check was being saved"
                    )));
                }
                if let Some(authority_id) = proof.authority_session_id.as_deref() {
                    let (operation_id, _) = operation.ok_or_else(|| {
                        AppError::Validation("policy-4 hidden check has no exact operation identity".into())
                    })?;
                    consume_couch_playback_authority_on(
                        &tx,
                        authority_id,
                        "spot_check",
                        operation_id,
                        reviewer,
                        segment_id,
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|duration| duration.as_millis() as i64)
                            .unwrap_or(1)
                            .max(1),
                    )?;
                }
            }
            // The answer key was obtained before this write transaction. An owner correction or
            // reject racing the submit must not grade/pay the reviewer against that stale text.
            // BEGIN IMMEDIATE above freezes this canonical key until the result commits.
            if enforce_production_proof {
                let current_expected = current_hidden_answer_key_on(&tx, segment_id)?;
                let expected_matches =
                    current_expected.as_deref().is_some_and(|value| to_nfc(value.trim()) == expected_nfc);
                if !expected_matches {
                    tx.rollback()?;
                    return Err(AppError::Validation(format!(
                        "{HIDDEN_ANSWER_KEY_CHANGED}: hidden-check answer changed while the result was being saved"
                    )));
                }
            }
            let changed = tx.execute(
                "INSERT INTO spot_checks
                     (segment_id, reviewer, action, submitted_transcript, expected_transcript, noticed, cer)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(segment_id, reviewer) DO NOTHING",
                params![segment_id, effective_reviewer, action, submitted_nfc, expected_nfc, noticed as i32, cer],
            )?;
            if changed > 0 {
                let (served_raw, served_revision): (String, i64) = tx.query_row(
                    "SELECT raw_transcript, review_revision
                       FROM speech_segments WHERE id = ?1",
                    params![segment_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let served_transcript = to_nfc(served_raw.trim());
                if served_transcript.is_empty() {
                    return Err(AppError::Validation(
                        "hidden-check write refused: the server-owned served transcript is blank".into(),
                    ));
                }
                if playback.is_some_and(|proof| proof.segment_revision != served_revision) {
                    return Err(AppError::Validation(format!(
                        "{PLAYBACK_EVIDENCE_CHANGED}: hidden-check served revision changed before commit"
                    )));
                }
                let timestamp_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0);
                tx.execute(
                    "INSERT INTO review_events
                        (segment_id, reviewer, action, compensation_action, source, timestamp_ms,
                         duration_ms, operation_id, operation_payload_hash, requested_action,
                         requested_transcript, served_transcript, served_revision, app_git_sha,
                         playback_guard_version)
                     VALUES (?1, ?2, ?3, ?3, 'couch_spot_check', ?4,
                             (SELECT duration_ms FROM speech_segments WHERE id = ?1), ?5, ?6, ?7, ?8,
                              ?9, ?10, ?11, ?12)",
                    params![
                        segment_id,
                        effective_reviewer,
                        action,
                        timestamp_ms,
                        operation.map(|value| value.0),
                        operation.map(|value| value.1),
                        requested_action,
                        requested_transcript_nfc,
                        served_transcript,
                        served_revision,
                        crate::GIT_SHA,
                        if playback.and_then(|proof| proof.authority_session_id.as_ref()).is_some() {
                            "interval-authority-v4"
                        } else {
                            "content-hash-raw-counter-v3"
                        },
                    ],
                )?;
                let event_id = tx.last_insert_rowid();
                Self::append_review_compensation_tx(
                    &tx,
                    event_id,
                    segment_id,
                    &effective_reviewer,
                    "couch_spot_check",
                    action,
                    action,
                    Some(served_revision),
                )?;
            }
            tx.commit()?;
            Ok(changed > 0)
        })();
        let reset = self.conn.execute_batch("PRAGMA synchronous=NORMAL;");
        let changed = match write {
            Ok(changed) => {
                reset?;
                changed
            }
            Err(error) => {
                let _ = reset;
                return Err(error);
            }
        };
        if changed {
            self.track_write()?;
        }
        Ok(())
    }

    /// Has this reviewer already supplied the immutable first answer for this hidden check?
    /// Used only to acknowledge outbox retries without appending another generic review event.
    pub fn has_spot_check_result(&self, segment_id: &str, reviewer: &str) -> AppResult<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM spot_checks WHERE segment_id = ?1 AND reviewer = ?2
             )",
            params![segment_id, reviewer],
            |row| row.get(0),
        )?)
    }

    /// Append one non-corpus review act (currently skip) and its compensation consequence atomically.
    pub fn record_review_event(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        source: &str,
        timestamp_ms: i64,
    ) -> AppResult<()> {
        self.record_review_event_inner(segment_id, reviewer, action, source, timestamp_ms, None, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_review_event_with_operation(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        source: &str,
        timestamp_ms: i64,
        operation_id: &str,
        operation_payload_hash: &str,
    ) -> AppResult<()> {
        self.record_review_event_with_operation_limit(
            segment_id,
            reviewer,
            action,
            source,
            timestamp_ms,
            operation_id,
            operation_payload_hash,
            action,
            "",
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_review_event_with_operation_limit(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        source: &str,
        timestamp_ms: i64,
        operation_id: &str,
        operation_payload_hash: &str,
        requested_action: &str,
        requested_transcript: &str,
        action_limit: Option<&ReviewDecisionLimit>,
    ) -> AppResult<()> {
        validate_review_operation_identity(operation_id, operation_payload_hash)?;
        let requested_action = requested_action.trim();
        let requested_transcript = to_nfc(requested_transcript.trim());
        if requested_action != "skip" {
            return Err(AppError::Validation("skip audit request snapshot must remain skip".into()));
        }
        let expected_payload_hash =
            review_operation_payload_hash(segment_id, requested_action, &requested_transcript, reviewer);
        if operation_payload_hash != expected_payload_hash {
            return Err(AppError::Validation(
                "skip operation payload hash does not match its exact submitted request".into(),
            ));
        }
        self.record_review_event_inner(
            segment_id,
            reviewer,
            action,
            source,
            timestamp_ms,
            Some((operation_id, operation_payload_hash)),
            Some((requested_action, requested_transcript.as_str())),
            action_limit,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_review_event_inner(
        &self,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        source: &str,
        timestamp_ms: i64,
        operation: Option<(&str, &str)>,
        request: Option<(&str, &str)>,
        action_limit: Option<&ReviewDecisionLimit>,
    ) -> AppResult<()> {
        if action != "skip" {
            return Err(AppError::Validation(
                "record_review_event is restricted to the zero-credit skip audit path".into(),
            ));
        }
        if action_limit.is_some() && source != "couch" {
            return Err(AppError::Validation("controlled-review limits are valid only for Couch actions".into()));
        }
        if timestamp_ms <= 0 {
            return Err(AppError::Validation("review event timestamp must be positive".into()));
        }
        let generated_operation = operation.is_none().then(|| {
            (uuid::Uuid::new_v4().to_string(), review_operation_payload_hash(segment_id, action, "", reviewer))
        });
        let operation = operation.or_else(|| {
            generated_operation
                .as_ref()
                .map(|(operation_id, payload_hash)| (operation_id.as_str(), payload_hash.as_str()))
        });
        let (requested_action, requested_transcript) = request.unwrap_or((action, ""));
        self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            if let Some((operation_id, _)) = operation {
                Self::require_canonical_operation_namespace_on(&tx, operation_id)?;
            }
            if let Some(limit) = action_limit {
                enforce_review_action_limit_on(&tx, reviewer, limit)?;
            }
            let (served_draft, served_revision): (String, i64) = tx.query_row(
                "SELECT COALESCE(NULLIF(TRIM(annotated_transcript), ''), raw_transcript),
                        review_revision
                   FROM speech_segments WHERE id = ?1",
                params![segment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let served_transcript = to_nfc(served_draft.trim());
            if served_transcript.is_empty() {
                return Err(AppError::Validation(
                    "skip audit refused: the server-owned served transcript is blank".into(),
                ));
            }
            tx.execute(
                "INSERT INTO review_events
                    (segment_id, reviewer, action, compensation_action, source, timestamp_ms,
                     duration_ms, operation_id, operation_payload_hash, requested_action,
                     requested_transcript, served_transcript, served_revision, app_git_sha,
                     playback_guard_version)
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5,
                         (SELECT duration_ms FROM speech_segments WHERE id = ?1), ?6, ?7, ?8,
                         ?9, ?10, ?11, ?12, 'content-hash-raw-counter-v3')",
                params![
                    segment_id,
                    reviewer,
                    action,
                    source,
                    timestamp_ms,
                    operation.map(|value| value.0),
                    operation.map(|value| value.1),
                    requested_action,
                    requested_transcript,
                    served_transcript,
                    served_revision,
                    crate::GIT_SHA,
                ],
            )?;
            let event_id = tx.last_insert_rowid();
            Self::append_review_compensation_tx(
                &tx,
                event_id,
                segment_id,
                reviewer,
                source,
                action,
                action,
                Some(served_revision),
            )?;
            tx.commit()?;
            Ok(())
        })?;
        self.track_write()?;
        Ok(())
    }

    /// Highest durable audit-event identity currently present. A pilot policy records this value
    /// before links are opened; a value in the future is refused at Couch startup rather than
    /// silently granting free decisions until the sequence catches up.
    pub fn max_review_event_id(&self) -> AppResult<i64> {
        Ok(self.conn.query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))?)
    }

    /// Read the controlled-pilot counter with the same event predicate the write transaction uses.
    /// Unauthorized names and already-overrun history are errors, never ignored rows.
    pub fn review_decision_progress(&self, limit: &ReviewDecisionLimit) -> AppResult<ReviewDecisionProgress> {
        review_decision_progress_on(&self.conn, limit)
    }

    /// Per-reviewer throughput from the audit trail, busiest first.
    ///
    /// The median is computed **within each reviewer's own stream**, which is the entire reason this
    /// does not reuse `stats.rs::compute_review_timing`: that one orders `decision_log` GLOBALLY, so
    /// with several people reviewing at once it would measure the gap between two DIFFERENT humans'
    /// decisions and report it as one person's pace. Correct for a single reviewer, meaningless for a
    /// team — so the existing metric is left exactly as it is and this one is partitioned by design.
    ///
    /// Gaps longer than [`REVIEW_SESSION_GAP_MS`] are dropped: a reviewer who closes the page and
    /// returns tomorrow did not spend fourteen hours on one clip.
    ///
    /// Counts DECISIONS only. `review_events` is a general audit trail and now also carries `skip` —
    /// a reviewer saying "I cannot judge this one", which writes nothing to the corpus. Counting those
    /// would credit somebody for work they explicitly did not do and inflate the one number that says
    /// how fast the corpus is really being reviewed. A WHITELIST, not `action <> 'skip'`: the next
    /// non-decision event added to this trail must not silently re-open the hole.
    /// Total AUDIO this reviewer has judged, in ms — activity progress only, never money.
    ///
    /// DISTINCT segment_id, so a network retry or a re-decision of the same clip cannot inflate it —
    /// the same reason `ReviewerThroughput::clips` counts distinct. Scoped to ONE reviewer rather
    /// than reusing `reviewer_throughput`, which walks every event of every reviewer: this runs on
    /// each queue fetch from a phone, so it stays a single aggregate.
    pub fn reviewed_audio_ms(&self, reviewer: &str) -> AppResult<i64> {
        // Full activity and payable credit are deliberately separate. Accept/edit/reject all mean the
        // reviewer judged the clip, so all count here once. The versioned compensation ledger applies
        // 10%/100%/10%; `skip` is neither activity nor pay.
        // LEFT JOIN + the event's own duration snapshot (v56): an INNER JOIN silently shrank this
        // total whenever the owner deleted a reviewed clip — real judged activity vanishing from the
        // progress history. The event's snapshot wins; the live row backfills events
        // that predate v56; an event whose clip is gone AND predates the snapshot stays 0 rather
        // than invented.
        // COLLATE NOCASE like every money and limit path: the ledger pays one reviewer whose name was
        // typed with different casing as ONE person, so an activity total that split them would
        // disagree with the balance shown beside it on the same screen.
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(d), 0)
               FROM (SELECT MAX(COALESCE(e.duration_ms, s.duration_ms, 0)) AS d
                       FROM review_events e
                       LEFT JOIN speech_segments s ON s.id = e.segment_id
                      WHERE e.reviewer = ?1 COLLATE NOCASE AND e.action IN ('accept', 'edit', 'reject')
                      GROUP BY e.segment_id)",
            params![reviewer],
            |row| row.get(0),
        )?)
    }

    /// Exact money projection under the active immutable policy. Legacy events are intentionally
    /// reported, not repriced: they do not preserve the semantic action needed for the new schedule.
    pub fn review_compensation_summary(&self, reviewer: &str) -> AppResult<ReviewCompensationSummary> {
        let (cutoff, base_rate, edit, accept, reject, skip): (i64, i64, i64, i64, i64, i64) = self.conn.query_row(
            "SELECT effective_after_event_id, base_rate_micro_iqd_per_hour,
                        edit_basis_points, accept_basis_points, reject_basis_points, skip_basis_points
                   FROM review_compensation_policies WHERE policy_version = ?1",
            params![REVIEW_PAY_POLICY_VERSION],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )?;
        if (base_rate, edit, accept, reject, skip)
            != (
                REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR,
                REVIEW_PAY_EDIT_BPS,
                REVIEW_PAY_ACCEPT_BPS,
                REVIEW_PAY_REJECT_BPS,
                REVIEW_PAY_SKIP_BPS,
            )
        {
            return Err(AppError::Other(format!(
                "review compensation policy row disagrees with certified binary {}",
                REVIEW_PAY_POLICY_VERSION
            )));
        }
        let earned_micro_iqd: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(delta_micro_iqd), 0)
               FROM review_compensation_ledger
              WHERE policy_version = ?1 AND reviewer = ?2 COLLATE NOCASE",
            params![REVIEW_PAY_POLICY_VERSION, reviewer],
            |row| row.get(0),
        )?;
        let legacy_events_pending_reconciliation: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM review_events
              WHERE id <= ?1 AND reviewer = ?2 COLLATE NOCASE
                AND action IN ('accept','edit','reject')",
            params![cutoff, reviewer],
            |row| row.get(0),
        )?;
        let fallback_identity_entries: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM review_compensation_ledger
              WHERE policy_version = ?1 AND reviewer = ?2 COLLATE NOCASE
                AND canonical_identity_kind = 'segment_id_fallback'",
            params![REVIEW_PAY_POLICY_VERSION, reviewer],
            |row| row.get(0),
        )?;
        let settled_micro_iqd: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(allocated_micro_iqd), 0)
               FROM review_compensation_settlements
              WHERE policy_version = ?1 AND reviewer = ?2 COLLATE NOCASE",
            params![REVIEW_PAY_POLICY_VERSION, reviewer],
            |row| row.get(0),
        )?;
        let outstanding_micro_iqd = earned_micro_iqd
            .checked_sub(settled_micro_iqd)
            .ok_or_else(|| AppError::Other("review compensation outstanding balance overflow".into()))?;
        // Correction time is a first-class signed projection. Keeping it separate from money means
        // duration repairs, skip audit entries, and later rate changes cannot make an active edit
        // disappear merely because its monetary balance no longer equals a freshly recomputed value.
        let (corrected_audio_ms, minimum_work_balance): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(corrected_ms), 0), COALESCE(MIN(corrected_ms), 0)
               FROM (
                    SELECT canonical_work_id, SUM(delta_corrected_ms) AS corrected_ms
                      FROM review_compensation_ledger
                     WHERE policy_version = ?1 AND reviewer = ?2 COLLATE NOCASE
                     GROUP BY canonical_work_id
               )",
            params![REVIEW_PAY_POLICY_VERSION, reviewer],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if minimum_work_balance < 0 || corrected_audio_ms < 0 {
            return Err(AppError::Other("review compensation corrected-audio ledger has a negative balance".into()));
        }
        Ok(ReviewCompensationSummary {
            policy_version: REVIEW_PAY_POLICY_VERSION.to_string(),
            earned_micro_iqd,
            corrected_audio_ms,
            settled_micro_iqd,
            outstanding_micro_iqd,
            legacy_events_pending_reconciliation: usize::try_from(legacy_events_pending_reconciliation)
                .map_err(|_| AppError::Other("legacy review-event count exceeds usize".into()))?,
            fallback_identity_entries: usize::try_from(fallback_identity_entries)
                .map_err(|_| AppError::Other("fallback identity count exceeds usize".into()))?,
        })
    }

    /// Allocate the next contiguous ledger interval to one immutable external payout reference.
    ///
    /// This does not send money. It is the durable exactly-once boundary an owner records only after
    /// the corresponding external payout/adjustment exists. The database trigger independently
    /// recomputes the range and amount, so even a future caller bug cannot overlap or forge it.
    pub fn record_review_compensation_settlement(
        &self,
        reviewer: &str,
        through_ledger_id_inclusive: i64,
        payout_reference: &str,
    ) -> AppResult<ReviewCompensationSettlement> {
        use rusqlite::OptionalExtension;

        let reviewer = reviewer.trim();
        let payout_reference = payout_reference.trim();
        if reviewer.is_empty() || payout_reference.is_empty() {
            return Err(AppError::Validation(
                "review compensation settlement requires reviewer and payout reference".into(),
            ));
        }
        // `with_full_sync` rather than a raw PRAGMA pair: the three explicit early returns lowered
        // `synchronous` again, but every `?` between them (policy check, the four queries, the INSERT,
        // the commit) returned with the SHARED connection still pinned at FULL for the rest of the
        // process — one failed settlement silently taxing every later write with an extra fsync.
        // BEGIN IMMEDIATE like the other money writers: the boundary is read and then extended, so a
        // DEFERRED transaction could have a second connection allocate the same ledger range in the
        // window between the MAX() read and the INSERT.
        self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            Self::verify_review_pay_policy_tx(&tx)?;
            // A payout provider can succeed while its HTTP response is lost. Retrying the same durable
            // reference must return the original allocation, not fail the now-advanced boundary or mint
            // another settlement. Reusing a reference for different parameters is a hard error.
            let existing: Option<ReviewCompensationSettlement> = tx
                .query_row(
                    "SELECT settlement_id, policy_version, reviewer, from_ledger_id_exclusive,
                            through_ledger_id_inclusive, allocated_micro_iqd, payout_reference
                       FROM review_compensation_settlements WHERE payout_reference = ?1",
                    params![payout_reference],
                    |row| {
                        Ok(ReviewCompensationSettlement {
                            settlement_id: row.get(0)?,
                            policy_version: row.get(1)?,
                            reviewer: row.get(2)?,
                            from_ledger_id_exclusive: row.get(3)?,
                            through_ledger_id_inclusive: row.get(4)?,
                            allocated_micro_iqd: row.get(5)?,
                            payout_reference: row.get(6)?,
                        })
                    },
                )
                .optional()?;
            if let Some(existing) = existing {
                tx.rollback()?;
                if existing.policy_version == REVIEW_PAY_POLICY_VERSION
                    && existing.reviewer.eq_ignore_ascii_case(reviewer)
                    && existing.through_ledger_id_inclusive == through_ledger_id_inclusive
                {
                    return Ok(existing);
                }
                return Err(AppError::Validation(format!(
                    "payout reference {payout_reference:?} is already bound to a different settlement"
                )));
            }
            let from_ledger_id_exclusive: i64 = tx.query_row(
                "SELECT COALESCE(MAX(through_ledger_id_inclusive), 0)
                   FROM review_compensation_settlements
                  WHERE policy_version = ?1 AND reviewer = ?2 COLLATE NOCASE",
                params![REVIEW_PAY_POLICY_VERSION, reviewer],
                |row| row.get(0),
            )?;
            if through_ledger_id_inclusive <= from_ledger_id_exclusive {
                tx.rollback()?;
                return Err(AppError::Validation(format!(
                    "settlement boundary {through_ledger_id_inclusive} must exceed prior boundary {from_ledger_id_exclusive}"
                )));
            }
            let (entry_count, allocated_micro_iqd): (i64, i64) = tx.query_row(
                "SELECT COUNT(*), COALESCE(SUM(delta_micro_iqd), 0)
                   FROM review_compensation_ledger
                  WHERE policy_version = ?1 AND reviewer = ?2 COLLATE NOCASE
                    AND id > ?3 AND id <= ?4",
                params![REVIEW_PAY_POLICY_VERSION, reviewer, from_ledger_id_exclusive, through_ledger_id_inclusive],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if entry_count == 0 {
                tx.rollback()?;
                return Err(AppError::Validation(
                    "settlement range contains no ledger entries for this reviewer".into(),
                ));
            }
            let settlement_id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO review_compensation_settlements
                    (settlement_id, policy_version, reviewer, from_ledger_id_exclusive,
                     through_ledger_id_inclusive, allocated_micro_iqd, payout_reference)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    settlement_id,
                    REVIEW_PAY_POLICY_VERSION,
                    reviewer,
                    from_ledger_id_exclusive,
                    through_ledger_id_inclusive,
                    allocated_micro_iqd,
                    payout_reference,
                ],
            )?;
            tx.commit()?;
            self.track_write()?;
            Ok(ReviewCompensationSettlement {
                settlement_id,
                policy_version: REVIEW_PAY_POLICY_VERSION.to_string(),
                reviewer: reviewer.to_string(),
                from_ledger_id_exclusive,
                through_ledger_id_inclusive,
                allocated_micro_iqd,
                payout_reference: payout_reference.to_string(),
            })
        })
    }

    pub fn reviewer_throughput(&self) -> AppResult<Vec<ReviewerThroughput>> {
        // Grouped by the CASE-FOLDED name, and ordered the same way, like every money and limit path.
        // Keyed on the raw string, "Sara" and "sara" became two reviewers with half the clips each
        // while the ledger paid them as one — and the ORDER BY has to fold too, or the interleaved
        // timestamps of the two spellings would break the per-session gap windowing below.
        let mut stmt = self.conn.prepare(
            "SELECT reviewer, segment_id, timestamp_ms FROM review_events
             WHERE action IN ('accept', 'edit', 'reject')
             ORDER BY reviewer COLLATE NOCASE ASC, timestamp_ms ASC",
        )?;
        let rows =
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?;

        // folded key -> (display spelling, distinct segments, timestamps)
        type ReviewerRuns = (String, std::collections::BTreeSet<String>, Vec<i64>);
        let mut by_reviewer: std::collections::BTreeMap<String, ReviewerRuns> = std::collections::BTreeMap::new();
        for row in rows {
            let (reviewer, segment, ts) = row?;
            let entry = by_reviewer
                .entry(reviewer.to_ascii_lowercase())
                .or_insert_with(|| (reviewer.clone(), std::collections::BTreeSet::new(), Vec::new()));
            entry.1.insert(segment);
            entry.2.push(ts);
        }

        let mut out: Vec<ReviewerThroughput> = by_reviewer
            .into_values()
            .map(|(reviewer, segments, stamps)| {
                let mut deltas: Vec<i64> =
                    stamps.windows(2).map(|w| w[1] - w[0]).filter(|&d| d > 0 && d <= REVIEW_SESSION_GAP_MS).collect();
                deltas.sort_unstable();
                let median_seconds = if deltas.is_empty() {
                    None
                } else {
                    let mid = deltas.len() / 2;
                    let ms = if deltas.len() % 2 == 1 {
                        deltas[mid] as f64
                    } else {
                        (deltas[mid - 1] + deltas[mid]) as f64 / 2.0
                    };
                    Some(ms / 1000.0)
                };
                ReviewerThroughput { reviewer, clips: segments.len(), median_seconds, samples: deltas.len() }
            })
            .collect();
        out.sort_by(|a, b| b.clips.cmp(&a.clips).then_with(|| a.reviewer.cmp(&b.reviewer)));
        Ok(out)
    }

    /// Build the two-rater agreement sample from clips more than one reviewer has answered.
    ///
    /// INTER-ANNOTATOR AGREEMENT NEEDS DOUBLE-ASSIGNMENT, AND SPOT CHECKS ALREADY PROVIDE IT. Leasing
    /// exists to stop two reviewers colliding on the same pending clip — but spot checks are
    /// deliberately NOT leased, because measuring two people independently is the point. So the
    /// overlap an agreement study requires is already there as a side effect, and `spot_checks` is
    /// already one row per (clip, reviewer): a per-decision table in all but name.
    ///
    /// The labels compared are the ACTIONS (accept / edit / reject) — the categorical judgement kappa
    /// is defined over. Comparing free transcripts instead would measure typing, not agreement.
    ///
    /// Returns `None` when no clip has yet been answered by two different people; a kappa computed
    /// from nothing would be a number with no evidence under it.
    pub fn agreement_sample(&self) -> AppResult<Option<AgreementExport>> {
        let mut stmt =
            self.conn.prepare("SELECT segment_id, reviewer, action FROM spot_checks ORDER BY segment_id ASC")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?;
        // segment -> (reviewer -> action), BTreeMap so the emitted TSV is byte-identical run to run.
        // Keyed on the CASE-FOLDED name like every money and limit path: with the raw string, one
        // person spelled two ways rated "both sides" of a clip and kappa measured them against
        // themselves. `reviewers` maps that folded key back to a display spelling for the report.
        let mut by_segment: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> =
            std::collections::BTreeMap::new();
        let mut reviewers: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for row in rows {
            let (segment, reviewer, action) = row?;
            let key = reviewer.to_ascii_lowercase();
            reviewers.entry(key.clone()).or_insert(reviewer);
            by_segment.entry(segment).or_default().insert(key, action);
        }

        // The pair sharing the most clips. Ties break on the (sorted) names so the choice is
        // deterministic — a report that silently picked a different pair on each run would make two
        // kappa numbers incomparable for no visible reason.
        let names: Vec<&String> = reviewers.keys().collect();
        let mut best: Option<(usize, &String, &String)> = None;
        for (ai, a) in names.iter().enumerate() {
            for b in names.iter().skip(ai + 1) {
                let shared =
                    by_segment.values().filter(|m| m.contains_key(a.as_str()) && m.contains_key(b.as_str())).count();
                // Written out rather than via `is_none_or`, which is stable only since Rust 1.82 while
                // this crate's MSRV is 1.81 (clippy::incompatible_msrv catches it).
                let better = match best {
                    None => true,
                    Some((most, _, _)) => shared > most,
                };
                if shared > 0 && better {
                    best = Some((shared, a, b));
                }
            }
        }
        let Some((items, a, b)) = best else {
            return Ok(None);
        };

        // Report the human-readable spelling, never the folded grouping key.
        let (rater_a, rater_b) = (reviewers[a].clone(), reviewers[b].clone());
        let mut tsv = format!("{rater_a}\t{rater_b}\n");
        for actions in by_segment.values() {
            if let (Some(la), Some(lb)) = (actions.get(a.as_str()), actions.get(b.as_str())) {
                tsv.push_str(&format!("{la}\t{lb}\n"));
            }
        }
        let other_reviewers: Vec<String> =
            reviewers.iter().filter(|(key, _)| *key != a && *key != b).map(|(_, name)| name.clone()).collect();
        Ok(Some(AgreementExport {
            rater_a,
            rater_b,
            items,
            tsv,
            path: String::new(), // filled in by the command that writes it
            other_reviewers,
        }))
    }

    /// Per-reviewer spot-check scores, worst `noticed` rate first — the order that puts a reviewer who
    /// may not be listening at the top of the list rather than buried under the diligent ones.
    pub fn spot_check_report(&self) -> AppResult<Vec<SpotCheckScore>> {
        let mut stmt = self.conn.prepare(
            "SELECT reviewer, COUNT(*), SUM(noticed), AVG(cer)
             FROM spot_checks GROUP BY reviewer ORDER BY (CAST(SUM(noticed) AS REAL) / COUNT(*)) ASC, reviewer ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let checks: i64 = row.get(1)?;
            let noticed: i64 = row.get(2)?;
            Ok(SpotCheckScore {
                reviewer: row.get(0)?,
                checks: checks as usize,
                noticed: noticed as usize,
                mean_cer: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
