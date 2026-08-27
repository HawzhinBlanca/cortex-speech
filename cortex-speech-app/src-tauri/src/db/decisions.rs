use super::*;

impl Database {
    /// Read a column that an older schema may lack (the jury cols were added by Migration v11 and
    /// alignment_quality by v12). A genuinely ABSENT column (`InvalidColumnIndex`, i.e. a row read
    /// through a pre-migration schema) yields `None`, and a SQL `NULL` yields `None` — both the
    /// intended defaults. But a type-mismatch / decode fault PROPAGATES instead of being masked:
    /// silently defaulting one of these on a decode error would misreport a genuinely gold /
    /// human-reviewed segment as `is_gold = false` / `human_decision = None`, which the
    /// human-protection guards key on — the exact silent-corruption the honesty rule forbids. This
    /// mirrors the strict `?` handling of columns 0-16 and the fail-closed `is_gold` read in
    /// `record_model_correction`.
    pub(super) fn optional_col<T: rusqlite::types::FromSql>(
        row: &rusqlite::Row,
        idx: usize,
    ) -> rusqlite::Result<Option<T>> {
        match row.get::<_, Option<T>>(idx) {
            Ok(value) => Ok(value),
            Err(rusqlite::Error::InvalidColumnIndex(_)) => Ok(None),
            Err(other) => Err(other),
        }
    }

    pub(super) fn map_row(row: &rusqlite::Row) -> rusqlite::Result<SpeechSegment> {
        Ok(SpeechSegment {
            id: row.get(0)?,
            created_at: row.get(1)?,
            audio_path: row.get(2)?,
            raw_transcript: row.get(3)?,
            normalized_transcript: row.get(4)?,
            annotated_transcript: row.get(5)?,
            alignment_json: row.get(6)?,
            duration_ms: row.get(7)?,
            speaker_id: row.get(8)?,
            verified: row.get::<_, i32>(9)? != 0,
            confidence: row.get(10)?,
            ctc_score: row.get(11)?,
            clipping_ratio: row.get(12)?,
            rms_db: row.get(13)?,
            snr_db: row.get(14)?,
            split: row.get(15)?,
            signal_anomaly_score: row.get(16)?,
            // Jury fields — added by Migration v11. Default ONLY when the column is genuinely absent
            // (old schema) or NULL; a decode error propagates so it can't silently strip provenance.
            verdict: Self::optional_col(row, 17)?,
            verdict_transcript: Self::optional_col(row, 18)?,
            rationale: Self::optional_col(row, 19)?,
            evidence_json: Self::optional_col(row, 20)?,
            agreement_score: Self::optional_col(row, 21)?,
            escalated: Self::optional_col::<i32>(row, 22)?.unwrap_or(0) != 0,
            human_decision: Self::optional_col(row, 23)?,
            corrected_at: Self::optional_col(row, 24)?,
            is_gold: Self::optional_col::<i32>(row, 25)?.unwrap_or(0) != 0,
            // Alignment quality — added by Migration v12; same fail-closed treatment.
            alignment_quality: Self::optional_col(row, 26)?,
            model_version_id: Self::optional_col(row, 27)?,
            confidence_source: Self::optional_col(row, 28)?,
            cloud_call: Self::optional_col::<i32>(row, 29)?.unwrap_or(0) != 0,
            decoder_config_hash: Self::optional_col(row, 30)?,
            normalizer_version: Self::optional_col(row, 31)?,
            // Per-segment processing provenance — Migration v41; nullable 0/1 -> Option<bool>, where
            // None (absent/NULL, i.e. a legacy pre-v41 row) stays "not recorded" rather than a fake false.
            denoised: Self::optional_col::<i32>(row, 32)?.map(|v| v != 0),
            diarized: Self::optional_col::<i32>(row, 33)?.map(|v| v != 0),
            // VAD backend — Migration v42; nullable TEXT. None (absent/NULL) stays "not recorded".
            vad_backend: Self::optional_col(row, 34)?,
            // Reviewer attribution — Migration v43; nullable TEXT. None = not attributed (legacy row,
            // undecided row, or a desktop decision), never a fabricated "owner".
            reviewed_by: Self::optional_col(row, 35)?,
            // Speaker-change score — Migration v47; nullable REAL. None (absent/NULL) means NOT
            // MEASURED and must never be read as "measured, one speaker".
            speaker_change_score: Self::optional_col(row, 36)?,
        })
    }

    // ── Jury DB helpers ───────────────────────────────────────────────────────

    /// M2.3 / P1.3: record what LOOP-0 WOULD have done for a segment WITHOUT mutating it. `memory_fired`
    /// is true when a correction memory would have changed the finalized transcript. One row per shadow
    /// observation; the C5 over-trigger decision joins these to the human's later decision at analysis
    /// time (an over-trigger is a would-fire the human subsequently contradicts).
    pub fn record_loop0_shadow(&self, segment_id: &str, memory_fired: bool) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO loop0_shadow_log (segment_id, memory_fired) VALUES (?1, ?2)",
            params![segment_id, memory_fired],
        )?;
        Ok(())
    }

    /// True-10 audit: the READ side of the intelligence instrumentation. loop0_shadow_log and
    /// decision_verdicts were write-only — the C5 (LOOP-0 go-live) and C4 (auto-accept precision)
    /// decisions were impossible to make in-app. This joins both against the humans' subsequent
    /// decisions:
    ///
    /// * LOOP-0 shadow: `fired_but_human_accepted_original` is the OVER-TRIGGER count (the memory
    ///   would have changed text a human then confirmed was already right) — C5 requires this to be
    ///   0 before `loop0_firing_enabled` may ever be turned on. `fired_and_human_edited` is
    ///   inconclusive-positive (the text did need changing; whether the memory's change matched the
    ///   human's is not knowable from the flag alone).
    /// * C4: of the machine's T0 auto-accepts that a human later reviewed, how many did the human
    ///   confirm vs contradict (edit/reject) — the honest precision behind any autonomy increase.
    pub fn intelligence_report(&self) -> AppResult<serde_json::Value> {
        // Live counts over surviving segments PLUS the durable archive of segments already deleted
        // (migration v33), so the C5 over-trigger gate is not survivor-biased by ordinary cleanup.
        // Per-SEGMENT counts (true-10 audit 2026-07-09): shadow_log holds one row per OBSERVATION and
        // re-processed segments accumulate several, but C5 reasons about distinct events — one clip,
        // one human decision, at most one over-trigger. Aggregate per segment first (MAX(memory_fired)
        // = "ever would have fired for this clip"), then count segments; the v33/v34 archives fold the
        // same per-segment semantics at delete time. (Archive rows accumulated before this change may
        // carry per-observation counts — a conservative overstatement for the C5 "must be 0" gate.)
        let loop0 = self.conn.query_row(
            "WITH per_seg AS (
                 SELECT l.segment_id, MAX(l.memory_fired) AS fired, s.human_decision AS hd
                 FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id
                 GROUP BY l.segment_id
             )
             SELECT COUNT(*) + COALESCE((SELECT total_observations FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(fired), 0)
                        + COALESCE((SELECT would_fire FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('accept','human_accept') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_accepted FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('edit','human_edit') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_edited FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('reject','human_reject') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_rejected FROM loop0_evidence_archive WHERE id = 1), 0)
             FROM per_seg",
            [],
            |row| {
                Ok(serde_json::json!({
                    "totalObservations": row.get::<_, i64>(0)?,
                    "wouldFire": row.get::<_, i64>(1)?,
                    "firedButHumanAcceptedOriginal": row.get::<_, i64>(2)?,
                    "firedAndHumanEdited": row.get::<_, i64>(3)?,
                    "firedAndHumanRejected": row.get::<_, i64>(4)?,
                }))
            },
        )?;
        // Live counts PLUS the v34 durable archive — deleting a reviewed clip must not shrink
        // t0HumanContradicted (the C4 precision could only drift optimistic; same class as v33/C5).
        let c4 = self.conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_accepts FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T1_ESCALATE' THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t1_escalations FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' AND s.human_decision IN ('accept','human_accept') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_human_confirmed FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' AND s.human_decision IN ('edit','human_edit','reject','human_reject') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_human_contradicted FROM c4_evidence_archive WHERE id = 1), 0)
             FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id",
            [],
            |row| {
                Ok(serde_json::json!({
                    "t0Accepts": row.get::<_, i64>(0)?,
                    "t1Escalations": row.get::<_, i64>(1)?,
                    "t0HumanConfirmed": row.get::<_, i64>(2)?,
                    "t0HumanContradicted": row.get::<_, i64>(3)?,
                }))
            },
        )?;
        // C3 honesty (true-10 audit 2026-07-09): the T0 auto-accept gate needs a Hoeffding-certified
        // per-SNR-bucket calibration set, and at the shipped constants that means ~thousands of
        // perfectly-transcribed verified clips PER BUCKET — previously invisible, so the user just
        // experienced "the jury escalates everything" with no stated reason or distance. Surface the
        // per-bucket progress: verified-with-reference counts vs the minimum needed at ZERO CER
        // (a hard lower bound — real data needs more). The gate itself is deliberately unchanged.
        let mut bucket_counts = [0i64; crate::quality::conformal::N_SNR_BUCKETS];
        {
            let mut stmt = self.conn.prepare(
                // Exclude human-REJECTED clips: "mark bad" sets verified=1 (to leave the review queue) with
                // human_decision='reject'/verdict='human_reject' while keeping annotated_transcript, so
                // without this guard a discarded clip counts as a "verified-with-reference" calibration
                // sample — overstating C3 progress toward T0 auto-accept. Matches quality::is_human_rejected,
                // which every export/gate path uses to drop these rows.
                "SELECT snr_db FROM speech_segments
                 WHERE verified = 1 AND annotated_transcript IS NOT NULL AND TRIM(annotated_transcript) != ''
                   AND NOT (COALESCE(human_decision,'') IN ('reject','human_reject') OR COALESCE(verdict,'') = 'human_reject')",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, Option<f64>>(0))?;
            for snr in rows {
                bucket_counts[crate::quality::conformal::snr_bucket(snr?)] += 1;
            }
        }
        // The T0 gate's shipped constants (jury/mod.rs): target 5% CER at 90% joint confidence,
        // Bonferroni-split across the buckets.
        let target_error = 0.05;
        let per_bucket_delta = (1.0 - 0.90) / crate::quality::conformal::N_SNR_BUCKETS as f64;
        let min_needed = crate::quality::conformal::min_calibration_n(target_error, per_bucket_delta);
        let bucket_labels = ["<5 dB (very noisy)", "5-15 dB", "15-25 dB", ">25 dB (clean)", "unknown SNR"];
        let calibration: Vec<serde_json::Value> = (0..crate::quality::conformal::N_SNR_BUCKETS)
            .map(|b| {
                serde_json::json!({
                    "bucket": bucket_labels[b],
                    "verifiedWithReference": bucket_counts[b],
                    "minNeededAtZeroCer": min_needed,
                })
            })
            .collect();
        let conformal_progress = serde_json::json!({
            "targetErrorCer": target_error,
            "perBucketDelta": per_bucket_delta,
            "minNeededAtZeroCer": min_needed,
            "buckets": calibration,
        });
        Ok(serde_json::json!({
            "loop0Shadow": loop0,
            "autoAcceptPrecision": c4,
            "conformalCalibration": conformal_progress,
        }))
    }

    /// M2.2 / P1.2: classify a MACHINE verdict as T0 (auto-resolved, no human needed) or T1
    /// (escalated to a human) and record it in decision_verdicts — the denominator/index for the C4
    /// auto-accept-precision measurement. Human verdicts (`human_*`) and any unknown string record
    /// nothing: they are not machine auto-accept decisions. The raw verdict stays on
    /// speech_segments.verdict, so a C4 query can still recover auto_accept-vs-jury_accept-vs-jury_edit.
    /// Call ONLY after the verdict UPDATE affected the row (segment was not already human-decided), so a
    /// stale/late machine verdict never plants a phantom T0/T1 over a human's decision.
    pub(super) fn record_decision_verdict_on(
        conn: &Connection,
        segment_id: &str,
        verdict: &str,
        escalated: bool,
    ) -> AppResult<()> {
        let auto_accept_verdict = if escalated || verdict == "escalated" {
            "T1_ESCALATE"
        } else if matches!(verdict, "auto_accept" | "jury_accept" | "jury_edit") {
            "T0_ACCEPT"
        } else {
            return Ok(());
        };
        conn.execute(
            "INSERT INTO decision_verdicts (segment_id, auto_accept_verdict, verdict_computed_at)
             VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(segment_id) DO UPDATE SET
                 auto_accept_verdict=excluded.auto_accept_verdict,
                 verdict_computed_at=excluded.verdict_computed_at",
            params![segment_id, auto_accept_verdict],
        )?;
        Ok(())
    }

    /// Direct decision-log fixture authority for pre-v60 migration tests. Production schema-v60
    /// jury writes are disabled before either segment state or classification metrics can mutate.
    #[cfg(test)]
    pub(crate) fn record_decision_verdict(&self, segment_id: &str, verdict: &str, escalated: bool) -> AppResult<()> {
        Self::record_decision_verdict_on(&self.conn, segment_id, verdict, escalated)
    }

    /// Persist a legacy machine-jury verdict only before schema v60.
    ///
    /// The first paid-review release deliberately freezes all automated jury writers at v60. This
    /// database boundary is shared by T0, T1, T2, and the direct jury command paths so none can
    /// create review-looking truth without the evidence-backed human decision protocol.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_segment_verdict(
        &self,
        segment_id: &str,
        verdict: &str,
        transcript: Option<&str>,
        rationale: Option<&str>,
        evidence_json: Option<&str>,
        agreement_score: Option<f64>,
        escalated: bool,
    ) -> AppResult<bool> {
        if crate::migrations::get_current_version(self)? >= 60 {
            return Err(AppError::Validation(
                "machine jury writes are disabled at schema v60; paid review truth must use the evidence-backed human decision flow"
                    .into(),
            ));
        }
        self.write_segment_verdict_legacy(
            segment_id,
            verdict,
            transcript,
            rationale,
            evidence_json,
            agreement_score,
            escalated,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn write_segment_verdict_legacy(
        &self,
        segment_id: &str,
        verdict: &str,
        transcript: Option<&str>,
        rationale: Option<&str>,
        evidence_json: Option<&str>,
        agreement_score: Option<f64>,
        escalated: bool,
    ) -> AppResult<bool> {
        if segment_id.trim().is_empty() || segment_id != segment_id.trim() {
            return Err(AppError::Validation("machine jury segment id must be canonical and nonblank".into()));
        }
        if !matches!(verdict, "auto_accept" | "jury_accept" | "jury_edit" | "escalated") {
            return Err(AppError::Validation(format!(
                "machine jury verdict '{verdict}' is not allowed; human truth requires the atomic review decision flow"
            )));
        }
        if escalated != (verdict == "escalated") {
            return Err(AppError::Validation(
                "machine jury escalation flag must exactly match verdict='escalated'".into(),
            ));
        }
        if let Some(transcript) = transcript {
            let canonical = to_nfc(transcript.trim());
            if canonical.is_empty() || canonical != transcript {
                return Err(AppError::Validation("machine jury transcript must be nonblank, trimmed NFC text".into()));
            }
        }
        if verdict == "escalated" && transcript.is_some() {
            return Err(AppError::Validation(
                "machine escalation cannot author a review transcript; use the atomic human decision flow".into(),
            ));
        }
        if verdict != "escalated" && transcript.is_none() {
            return Err(AppError::Validation(
                "machine accept/edit verdict requires an exact machine transcript".into(),
            ));
        }
        if let Some(rationale) = rationale {
            let canonical = to_nfc(rationale.trim());
            if canonical.is_empty() || canonical != rationale {
                return Err(AppError::Validation(
                    "machine jury rationale must be nonblank, trimmed NFC text when present".into(),
                ));
            }
        }
        if let Some(evidence) = evidence_json {
            serde_json::from_str::<serde_json::Value>(evidence)
                .map_err(|error| AppError::Validation(format!("machine jury evidence must be valid JSON: {error}")))?;
        }
        if agreement_score.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
            return Err(AppError::Validation(
                "machine jury agreement score must be finite and between zero and one".into(),
            ));
        }
        self.conn.execute("SAVEPOINT verdict_write", [])?;
        let result: AppResult<bool> = (|| {
            let affected = self.conn.execute(
                "UPDATE speech_segments
                 SET verdict              = ?2,
                     verdict_transcript   = ?3,
                     -- v48: the SAME text, kept where no human path can overwrite it. `verdict_transcript`
                     -- is whichever verdict is current and `record_human_decision_by` replaces it with the
                     -- reviewer's correction, which is why the label-quality lift compared the human's
                     -- answer with itself on every decided row. Written ONLY by the machine-verdict
                     -- writers (here and jury::write_verdict), never by the human-decision path, so
                     -- the machine's own output survives the human's.
                     jury_transcript      = ?3,
                     rationale            = ?4,
                     evidence_json        = ?5,
                     agreement_score     = COALESCE(?6, agreement_score),
                     escalated            = ?7,
                     updated_at           = datetime('now')
                 WHERE id = ?1
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept', 'human_edit', 'human_reject'))",
                params![segment_id, verdict, transcript, rationale, evidence_json, agreement_score, escalated as i32],
            )?;
            if affected == 0 {
                // Either the row is gone or a human already decided it — in both cases the machine verdict
                // correctly does not apply. Logged (not an error) so the no-op is visible without masking it.
                tracing::debug!(
                    "write_segment_verdict({segment_id}, {verdict}): no-op — segment is human-decided or missing"
                );
            } else {
                // M2.2/P1.2: record the T0/T1 classification for the C4 denominator (no-op for human/unknown).
                Self::record_decision_verdict_on(&self.conn, segment_id, verdict, escalated)?;
            }
            Ok(affected > 0)
        })();
        match result {
            Ok(wrote) => {
                self.release_savepoint("verdict_write")?;
                if wrote {
                    self.track_write()?;
                }
                Ok(wrote)
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("verdict_write");
                Err(e)
            }
        }
    }

    /// Explicit pre-v60 machine-state fixture authority. This bypass is compiled only into unit
    /// tests that exercise frozen legacy state; registered application routes cannot call it.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_legacy_machine_verdict_for_test(
        &self,
        segment_id: &str,
        verdict: &str,
        transcript: Option<&str>,
        rationale: Option<&str>,
        evidence_json: Option<&str>,
        agreement_score: Option<f64>,
        escalated: bool,
    ) -> AppResult<bool> {
        self.write_segment_verdict_legacy(
            segment_id,
            verdict,
            transcript,
            rationale,
            evidence_json,
            agreement_score,
            escalated,
        )
    }

    /// Fully RE-OPEN a segment whose human decision is being undone. record_human_decision OVERWRITES
    /// the prior machine verdict with the human one, so the pre-decision verdict is gone — the honest
    /// reset is "un-adjudicated": clear the human decision AND the verdict it set, and return the segment
    /// to the review queue (escalated = 1). Clearing only human_decision (the old behavior) left a stale
    /// verdict = 'human_*' so the "undone" segment still looked decided on reload AND the machine
    /// verdict-write guard (write_segment_verdict / jury::write_verdict) would refuse to re-adjudicate it.
    pub fn clear_human_decision(&self, segment_id: &str) -> AppResult<()> {
        let _ = segment_id;
        Err(AppError::Validation(
            "clear_human_decision is disabled: undo requires an immutable decision effect id and operation UUID".into(),
        ))
    }

    /// Reverse exactly one active human-decision effect. The immutable database snapshot is the
    /// only restore authority; the caller supplies only the effect identity, actor, and idempotency
    /// UUID. A stale row is a conflict with no mutation.
    pub fn undo_human_decision(
        &self,
        effect_event_id: i64,
        actor: Option<&str>,
        operation_id: &str,
    ) -> AppResult<HumanDecisionUndoOutcome> {
        if effect_event_id <= 0 {
            return Err(AppError::Validation("human decision effect id must be positive".into()));
        }
        validate_operation_uuid(operation_id)?;
        let outcome = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            let effect = tx
                .query_row(
                    "SELECT review_event_id, segment_id, reviewer, source, action,
                            decision_transcript, decision_annotated_transcript, decision_verified,
                            decision_corrected_at, decision_rationale, prior_revision,
                            decision_revision, prior_verified, prior_annotated_transcript, prior_verdict,
                            prior_verdict_transcript, prior_rationale, prior_escalated, prior_human_decision,
                            prior_corrected_at, prior_reviewed_by
                       FROM human_decision_effect_events WHERE id = ?1",
                    params![effect_event_id],
                    |row| {
                        Ok(DecisionEffectSnapshot {
                            review_event_id: row.get(0)?,
                            segment_id: row.get(1)?,
                            reviewer: row.get(2)?,
                            source: row.get(3)?,
                            action: row.get(4)?,
                            decision_transcript: row.get(5)?,
                            decision_annotated_transcript: row.get(6)?,
                            decision_verified: row.get::<_, i32>(7)? != 0,
                            decision_corrected_at: row.get(8)?,
                            decision_rationale: row.get(9)?,
                            prior_revision: row.get(10)?,
                            decision_revision: row.get(11)?,
                            prior_verified: row.get::<_, i32>(12)? != 0,
                            prior_annotated_transcript: row.get(13)?,
                            prior_verdict: row.get(14)?,
                            prior_verdict_transcript: row.get(15)?,
                            prior_rationale: row.get(16)?,
                            prior_escalated: row.get::<_, i32>(17)? != 0,
                            prior_human_decision: row.get(18)?,
                            prior_corrected_at: row.get(19)?,
                            prior_reviewed_by: row.get(20)?,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| AppError::Validation("unknown human decision effect id".into()))?;
            let actor_ok = match (effect.source.as_str(), effect.reviewer.as_deref(), actor.map(str::trim)) {
                ("desktop", None, None) => true,
                ("couch", Some(owner), Some(candidate)) => owner.eq_ignore_ascii_case(candidate),
                _ => false,
            };
            if !actor_ok {
                return Err(AppError::Validation("human decision undo actor does not own this effect".into()));
            }
            if effect.decision_rationale != effect.prior_rationale {
                return Err(AppError::Other(
                    "human decision effect does not preserve its server-owned rationale snapshot".into(),
                ));
            }
            let current = Self::decision_snapshot_on(&tx, &effect.segment_id)?
                .ok_or_else(|| AppError::Validation("cannot undo a decision whose segment no longer exists".into()))?;
            let prior_reversal: Option<String> = tx
                .query_row(
                    "SELECT operation_id FROM human_decision_effect_reversals WHERE effect_event_id = ?1",
                    params![effect_event_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(prior_operation) = prior_reversal {
                tx.rollback()?;
                return Ok(if prior_operation == operation_id {
                    HumanDecisionUndoOutcome::AlreadyApplied { restored_revision: current.1, segment: current.0 }
                } else {
                    HumanDecisionUndoOutcome::Conflict { segment: current.0 }
                });
            }
            let expected_verdict = human_verdict_for_decision(&effect.action)?;
            let expected_verdict_transcript = if effect.action == "reject" {
                effect.prior_verdict_transcript.as_deref()
            } else {
                effect.decision_transcript.as_deref()
            };
            let later_review_mutation: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1
                       FROM human_decision_effect_events newer
                      WHERE newer.segment_id = ?1
                        AND newer.decision_revision > ?2
                     UNION ALL
                     SELECT 1
                       FROM review_flag_effect_events flag
                      WHERE flag.segment_id = ?1
                        AND flag.flag_revision > ?2
                 )",
                params![effect.segment_id, effect.decision_revision],
                |row| row.get(0),
            )?;
            if current.1 < effect.decision_revision || later_review_mutation {
                tx.rollback()?;
                return Ok(HumanDecisionUndoOutcome::Conflict { segment: current.0 });
            }
            let changed = tx.execute(
                "UPDATE speech_segments
                    SET verified = ?2,
                        annotated_transcript = ?3,
                        verdict = ?4,
                        verdict_transcript = ?5,
                        escalated = ?6,
                        human_decision = ?7,
                        corrected_at = ?8,
                        reviewed_by = ?9,
                        updated_at = datetime('now')
                  WHERE id = ?1
                    AND review_revision = ?10
                    AND human_decision = ?11
                    AND verdict = ?12
                    AND escalated = 0
                    AND reviewed_by IS ?13
                    AND verified = ?14
                    AND annotated_transcript IS ?15
                    AND verdict_transcript IS ?16
                    AND corrected_at = ?17
                    AND rationale IS ?18",
                params![
                    effect.segment_id,
                    effect.prior_verified as i32,
                    effect.prior_annotated_transcript,
                    effect.prior_verdict,
                    effect.prior_verdict_transcript,
                    effect.prior_escalated as i32,
                    effect.prior_human_decision,
                    effect.prior_corrected_at,
                    effect.prior_reviewed_by,
                    current.1,
                    effect.action,
                    expected_verdict,
                    effect.reviewer,
                    effect.decision_verified as i32,
                    effect.decision_annotated_transcript,
                    expected_verdict_transcript,
                    effect.decision_corrected_at,
                    effect.decision_rationale,
                ],
            )?;
            if changed == 0 {
                tx.rollback()?;
                return Ok(HumanDecisionUndoOutcome::Conflict { segment: current.0 });
            }
            let restored_revision: i64 = tx.query_row(
                "SELECT review_revision FROM speech_segments WHERE id = ?1",
                params![effect.segment_id],
                |row| row.get(0),
            )?;
            if restored_revision != current.1 + 1 {
                return Err(AppError::Other("human decision undo did not advance exactly one revision".into()));
            }
            if effect.review_event_id.is_some() {
                Self::append_review_compensation_reversal_for_effect_tx(&tx, effect_event_id, operation_id)?;
            }
            tx.execute(
                "INSERT INTO human_decision_effect_reversals (effect_event_id, operation_id)
                 VALUES (?1, ?2)",
                params![effect_event_id, operation_id],
            )?;
            tx.execute(
                "INSERT INTO playback_receipts
                    (segment_id, segment_revision, audio_fingerprint, reviewer, session_id,
                     started_at_ms, played_ms, clip_duration_ms, coverage_ratio, policy_version,
                     source_start_ms, source_end_ms)
                 SELECT p.segment_id, ?4, p.audio_fingerprint, p.reviewer, p.session_id,
                        p.started_at_ms, p.played_ms, p.clip_duration_ms,
                        MIN(1.0, CAST(p.played_ms AS REAL) / CAST(p.clip_duration_ms AS REAL)),
                        p.policy_version, p.source_start_ms, p.source_end_ms
                   FROM playback_receipts p
                   JOIN speech_segments s ON s.id = p.segment_id
                  WHERE p.segment_id = ?1 AND p.reviewer IS ?2
                    AND p.segment_revision = ?3 AND p.policy_version = ?5
                    AND length(s.audio_content_hash) = 64
                    AND s.audio_content_hash NOT GLOB '*[^0-9a-f]*'
                    AND p.audio_fingerprint = s.audio_content_hash
                    AND json_valid(s.alignment_json)
                    AND typeof(json_extract(s.alignment_json, '$.source_start_ms')) = 'integer'
                    AND typeof(json_extract(s.alignment_json, '$.source_end_ms')) = 'integer'
                    AND json_extract(s.alignment_json, '$.source_start_ms') >= 0
                    AND json_extract(s.alignment_json, '$.source_end_ms')
                        > json_extract(s.alignment_json, '$.source_start_ms')
                    AND typeof(p.source_start_ms) = 'integer'
                    AND typeof(p.source_end_ms) = 'integer'
                    AND p.source_start_ms = json_extract(s.alignment_json, '$.source_start_ms')
                    AND p.source_end_ms = json_extract(s.alignment_json, '$.source_end_ms')
                    AND ABS((p.source_end_ms - p.source_start_ms) - s.duration_ms) <= ?6
                    AND p.clip_duration_ms = s.duration_ms
                    AND p.clip_duration_ms > 0 AND p.played_ms >= 0 AND p.started_at_ms >= 0
                    AND NOT EXISTS (
                        SELECT 1 FROM playback_receipts newer
                         WHERE newer.segment_id = ?1 AND newer.reviewer IS ?2
                           AND newer.segment_revision = ?4 AND newer.policy_version = ?5
                    )
                  ORDER BY MIN(1.0, CAST(p.played_ms AS REAL) / CAST(p.clip_duration_ms AS REAL)) DESC,
                           p.id DESC
                  LIMIT 1",
                params![
                    effect.segment_id,
                    effect.reviewer,
                    effect.prior_revision,
                    restored_revision,
                    PLAYBACK_POLICY_VERSION,
                    MAX_SOURCE_SPAN_DURATION_DELTA_MS,
                ],
            )?;
            let segment = Self::decision_snapshot_on(&tx, &effect.segment_id)?
                .ok_or_else(|| AppError::Other("segment disappeared inside human decision undo".into()))?
                .0;
            tx.commit()?;
            Ok(HumanDecisionUndoOutcome::Applied { restored_revision, segment })
        })?;
        if matches!(&outcome, HumanDecisionUndoOutcome::Applied { .. }) {
            self.track_write()?;
        }
        Ok(outcome)
    }
    pub fn record_review_flag(
        &self,
        segment_id: &str,
        rationale: &str,
        operation_id: &str,
    ) -> AppResult<HumanFlagCommit> {
        let rationale = to_nfc(rationale.trim());
        if rationale.is_empty() {
            return Err(AppError::Validation("review flag rationale must not be blank".into()));
        }
        if rationale.starts_with(crate::quality::TECHNICAL_UNUSABLE_RATIONALE_PREFIX) {
            return Err(AppError::Validation(
                "technical-unusable rationale namespace is reserved for mark_segment_unusable_v1".into(),
            ));
        }
        validate_operation_uuid(operation_id)?;
        let commit = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            let replay = tx
                .query_row(
                    "SELECT id, segment_id, prior_revision, flag_revision, flag_rationale
                       FROM review_flag_effect_events
                      WHERE operation_id = ?1",
                    params![operation_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((effect_event_id, replay_segment_id, prior_revision, flag_revision, replay_rationale)) = replay {
                if replay_segment_id != segment_id || replay_rationale != rationale {
                    return Err(AppError::Validation(
                        "review flag operation UUID was already used for a different request".into(),
                    ));
                }
                let current = Self::decision_snapshot_on(&tx, segment_id)?
                    .ok_or_else(|| AppError::Validation("flag replay segment no longer exists".into()))?;
                let reversed: bool = tx.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM review_flag_effect_reversals
                          WHERE flag_effect_event_id = ?1
                     )",
                    params![effect_event_id],
                    |row| row.get(0),
                )?;
                let later_review_mutation: bool = tx.query_row(
                    "SELECT EXISTS(
                         SELECT 1
                           FROM review_flag_effect_events newer
                          WHERE newer.segment_id = ?1
                            AND newer.flag_revision > ?2
                         UNION ALL
                         SELECT 1
                           FROM human_decision_effect_events decision
                          WHERE decision.segment_id = ?1
                            AND decision.decision_revision > ?2
                     )",
                    params![segment_id, flag_revision],
                    |row| row.get(0),
                )?;
                if reversed
                    || later_review_mutation
                    || current.1 < flag_revision
                    || current.0.verdict.as_deref() != Some("escalated")
                    || current.0.rationale.as_deref() != Some(rationale.as_str())
                    || !current.0.escalated
                    || current
                        .0
                        .human_decision
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                {
                    return Err(AppError::Validation(
                        "review flag operation was committed, but its exact post-state is no longer current".into(),
                    ));
                }
                tx.rollback()?;
                return Ok(HumanFlagCommit {
                    effect_event_id,
                    segment_id: replay_segment_id,
                    prior_revision,
                    flag_revision,
                    segment: current.0,
                });
            }
            let (prior, prior_revision, _) = Self::decision_snapshot_on(&tx, segment_id)?
                .ok_or_else(|| AppError::Validation("cannot flag an unknown segment".into()))?;
            if crate::quality::is_technically_unusable(&prior) {
                return Err(AppError::Validation(
                    "cannot replace a durable technical-unusable marker with a generic review flag; undo its exact effect first"
                        .into(),
                ));
            }
            if prior.human_decision.as_deref().is_some_and(|value| !value.trim().is_empty()) {
                return Err(AppError::Validation("cannot flag a segment that already has a human decision".into()));
            }
            if !Self::flag_human_baseline_is_authorized_on(&tx, &prior)? {
                return Err(AppError::Validation(
                    "review flag refused: the segment carries human review fields with no immutable legacy or decision-effect authority"
                        .into(),
                ));
            }
            let changed = tx.execute(
                "UPDATE speech_segments
                    SET verdict = 'escalated', rationale = ?2, escalated = 1,
                        updated_at = datetime('now')
                  WHERE id = ?1 AND review_revision = ?3
                    AND (human_decision IS NULL OR human_decision = '')",
                params![segment_id, rationale, prior_revision],
            )?;
            if changed != 1 {
                return Err(AppError::Validation("segment changed while its review flag was being saved".into()));
            }
            let flag_revision: i64 = tx.query_row(
                "SELECT review_revision FROM speech_segments WHERE id = ?1",
                params![segment_id],
                |row| row.get(0),
            )?;
            if flag_revision != prior_revision + 1 {
                return Err(AppError::Other("review flag did not advance exactly one revision".into()));
            }
            tx.execute(
                "INSERT INTO review_flag_effect_events
                    (operation_id, segment_id, prior_revision, flag_revision, prior_verdict,
                     prior_rationale, flag_rationale, prior_escalated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    operation_id,
                    segment_id,
                    prior_revision,
                    flag_revision,
                    prior.verdict,
                    prior.rationale,
                    rationale,
                    prior.escalated as i32,
                ],
            )?;
            let effect_event_id = tx.last_insert_rowid();
            let segment = Self::decision_snapshot_on(&tx, segment_id)?
                .ok_or_else(|| AppError::Other("segment disappeared inside review flag transaction".into()))?
                .0;
            tx.commit()?;
            Ok(HumanFlagCommit {
                effect_event_id,
                segment_id: segment_id.to_string(),
                prior_revision,
                flag_revision,
                segment,
            })
        })?;
        self.track_write()?;
        Ok(commit)
    }

    pub(super) fn replay_technical_unusable_on(
        conn: &Connection,
        segment_id: &str,
        base_revision: i64,
        reason: &str,
        operation_id: &str,
    ) -> AppResult<Option<HumanFlagCommit>> {
        let replay = conn
            .query_row(
                "SELECT id, segment_id, prior_revision, flag_revision, flag_rationale
                   FROM review_flag_effect_events
                  WHERE operation_id = ?1",
                params![operation_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((effect_event_id, replay_segment_id, prior_revision, flag_revision, replay_rationale)) = replay else {
            return Ok(None);
        };
        if replay_segment_id != segment_id
            || prior_revision != base_revision
            || crate::quality::technical_unusable_reason_from_rationale(Some(&replay_rationale)) != Some(reason)
        {
            return Err(AppError::Validation(
                "technical-unusable operation UUID was already used for a different request".into(),
            ));
        }
        let current = Self::decision_snapshot_on(conn, segment_id)?
            .ok_or_else(|| AppError::Validation("technical-unusable replay segment no longer exists".into()))?;
        let reversed: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM review_flag_effect_reversals
                  WHERE flag_effect_event_id = ?1
             )",
            params![effect_event_id],
            |row| row.get(0),
        )?;
        let later_review_mutation: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM review_flag_effect_events newer
                  WHERE newer.segment_id = ?1
                    AND newer.flag_revision > ?2
                 UNION ALL
                 SELECT 1
                   FROM human_decision_effect_events decision
                  WHERE decision.segment_id = ?1
                    AND decision.decision_revision > ?2
             )",
            params![segment_id, flag_revision],
            |row| row.get(0),
        )?;
        if reversed
            || later_review_mutation
            || current.1 < flag_revision
            || current.0.verdict.as_deref() != Some("escalated")
            || current.0.rationale.as_deref() != Some(replay_rationale.as_str())
            || !current.0.escalated
            || current.0.human_decision.as_deref().is_some_and(|value| !value.trim().is_empty())
        {
            return Err(AppError::Validation(
                "technical-unusable operation was committed, but its exact post-state is no longer current".into(),
            ));
        }
        // A draft autosave can race just behind the first committed action, or be retried by a
        // renderer that lost the success response. Re-delete only the original revision's stale
        // draft; a draft for the committed/newer revision is never touched.
        conn.execute(
            "DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2",
            params![segment_id, base_revision],
        )?;
        Ok(Some(HumanFlagCommit {
            effect_event_id,
            segment_id: replay_segment_id,
            prior_revision,
            flag_revision,
            segment: current.0,
        }))
    }

    /// Resolve an exact response-loss retry before probing the mutable source again. A file may have
    /// been repaired after the original durable failure; that must not make the same operation UUID
    /// ambiguous or create a second effect.
    pub(crate) fn replay_segment_technically_unusable(
        &self,
        segment_id: &str,
        base_revision: i64,
        reason: &str,
        operation_id: &str,
    ) -> AppResult<Option<HumanFlagCommit>> {
        if base_revision < 0 {
            return Err(AppError::Validation("technical-unusable base revision must be non-negative".into()));
        }
        if !crate::quality::is_supported_technical_unusable_reason(reason) {
            return Err(AppError::Validation(
                "technical-unusable reason is not one of the supported structured codes".into(),
            ));
        }
        validate_operation_uuid(operation_id)?;
        let replay = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            let replay = Self::replay_technical_unusable_on(&tx, segment_id, base_revision, reason, operation_id)?;
            if replay.is_some() {
                tx.commit()?;
            } else {
                tx.rollback()?;
            }
            Ok(replay)
        })?;
        if replay.is_some() {
            self.track_write()?;
        }
        Ok(replay)
    }

    /// Durably classify a clip as technically unusable without creating human transcript truth.
    ///
    /// The existing immutable review-flag effect graph is sufficient authority: its prior snapshot,
    /// operation UUID, revision edge and exact inverse make this action auditable and reversible. A
    /// reserved structured rationale distinguishes the technical fact from a free-form human flag.
    /// No playback receipt, human decision, compensation entry, transcript or verification field is
    /// read or written here.
    pub(crate) fn mark_segment_technically_unusable_after_verified_failure(
        &self,
        segment_id: &str,
        base_revision: i64,
        reason: &str,
        expected_source_path_sha256: &str,
        expected_audio_content_hash: Option<&str>,
        operation_id: &str,
    ) -> AppResult<HumanFlagCommit> {
        if base_revision < 0 {
            return Err(AppError::Validation("technical-unusable base revision must be non-negative".into()));
        }
        // Absence is not a leaseable filesystem object. The supported desktop store rejects this
        // before probing; keep the persistence boundary fail-closed as well so a future internal
        // caller cannot reintroduce the negative-entry TOCTOU by bypassing that store.
        if reason == "missingFile" {
            return Err(AppError::Validation("E_TECHNICAL_UNUSABLE_MISSING_FILE_UNLEASEABLE".into()));
        }
        if crate::quality::canonical_technical_unusable_rationale(
            reason,
            expected_source_path_sha256,
            expected_audio_content_hash,
            base_revision,
        )
        .is_none()
        {
            return Err(AppError::Validation("technical-unusable source identity or reason is not canonical".into()));
        }
        validate_operation_uuid(operation_id)?;

        let commit = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            if let Some(replay) = Self::replay_technical_unusable_on(
                &tx,
                segment_id,
                base_revision,
                reason,
                operation_id,
            )? {
                tx.commit()?;
                return Ok(replay);
            }

            let (prior, current_revision, current_audio_content_hash) = Self::decision_snapshot_on(&tx, segment_id)?
                .ok_or_else(|| {
                AppError::Validation("E_TECHNICAL_UNUSABLE_SEGMENT_NOT_FOUND".into())
            })?;
            if current_revision != base_revision {
                return Err(AppError::Validation(format!(
                    "E_STALE_TECHNICAL_UNUSABLE_REVISION:{current_revision}"
                )));
            }
            let current_source_path_sha256 =
                technical_unusable_source_path_sha256(Path::new(&prior.audio_path))?;
            if current_source_path_sha256 != expected_source_path_sha256
                || current_audio_content_hash.as_deref() != expected_audio_content_hash
            {
                return Err(AppError::Validation("E_TECHNICAL_UNUSABLE_SOURCE_CHANGED".into()));
            }
            let rationale = crate::quality::canonical_technical_unusable_rationale(
                reason,
                &current_source_path_sha256,
                current_audio_content_hash.as_deref(),
                current_revision,
            )
            .ok_or_else(|| AppError::Other("technical-unusable source snapshot was not canonical".into()))?;
            if prior.human_decision.as_deref().is_some_and(|value| !value.trim().is_empty()) {
                return Err(AppError::Validation("E_TECHNICAL_UNUSABLE_ALREADY_HUMAN_REVIEWED".into()));
            }
            if crate::quality::is_technically_unusable(&prior) {
                return Err(AppError::Validation(
                    "segment already has a different active technical-unusable effect".into(),
                ));
            }
            if !Self::flag_human_baseline_is_authorized_on(&tx, &prior)? {
                return Err(AppError::Validation(
                    "technical-unusable mark refused: the segment carries human review fields with no immutable authority"
                        .into(),
                ));
            }

            let changed = tx.execute(
                "UPDATE speech_segments
                    SET verdict = 'escalated', rationale = ?2, escalated = 1,
                        updated_at = datetime('now')
                  WHERE id = ?1 AND review_revision = ?3
                    AND (human_decision IS NULL OR human_decision = '')",
                params![segment_id, rationale, base_revision],
            )?;
            if changed != 1 {
                let current_revision = tx
                    .query_row(
                        "SELECT review_revision FROM speech_segments WHERE id = ?1",
                        params![segment_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                return Err(match current_revision {
                    Some(revision) => {
                        AppError::Validation(format!("E_STALE_TECHNICAL_UNUSABLE_REVISION:{revision}"))
                    }
                    None => AppError::Validation("E_TECHNICAL_UNUSABLE_SEGMENT_NOT_FOUND".into()),
                });
            }
            let flag_revision: i64 = tx.query_row(
                "SELECT review_revision FROM speech_segments WHERE id = ?1",
                params![segment_id],
                |row| row.get(0),
            )?;
            if flag_revision != base_revision + 1 {
                return Err(AppError::Other(
                    "technical-unusable mark did not advance exactly one revision".into(),
                ));
            }
            tx.execute(
                "INSERT INTO review_flag_effect_events
                    (operation_id, segment_id, prior_revision, flag_revision, prior_verdict,
                     prior_rationale, flag_rationale, prior_escalated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    operation_id,
                    segment_id,
                    base_revision,
                    flag_revision,
                    prior.verdict,
                    prior.rationale,
                    rationale,
                    prior.escalated as i32,
                ],
            )?;
            let effect_event_id = tx.last_insert_rowid();

            // A successful queue-terminal action clears only the draft bound to the exact state the
            // renderer marked. It cannot erase a draft for a later revision, and a failed transaction
            // leaves the draft intact.
            tx.execute(
                "DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2",
                params![segment_id, base_revision],
            )?;

            let segment = Self::decision_snapshot_on(&tx, segment_id)?
                .ok_or_else(|| AppError::Other("segment disappeared inside technical-unusable transaction".into()))?
                .0;
            tx.commit()?;
            Ok(HumanFlagCommit {
                effect_event_id,
                segment_id: segment_id.to_string(),
                prior_revision: base_revision,
                flag_revision,
                segment,
            })
        })?;
        self.track_write()?;
        Ok(commit)
    }

    pub fn undo_review_flag(&self, effect_event_id: i64, operation_id: &str) -> AppResult<HumanFlagUndoOutcome> {
        if effect_event_id <= 0 {
            return Err(AppError::Validation("review flag effect id must be positive".into()));
        }
        validate_operation_uuid(operation_id)?;
        let outcome = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            let effect = tx
                .query_row(
                    "SELECT segment_id, flag_revision, prior_verdict,
                            prior_rationale, flag_rationale, prior_escalated
                       FROM review_flag_effect_events WHERE id = ?1",
                    params![effect_event_id],
                    |row| {
                        Ok(FlagEffectSnapshot {
                            segment_id: row.get(0)?,
                            flag_revision: row.get(1)?,
                            prior_verdict: row.get(2)?,
                            prior_rationale: row.get(3)?,
                            flag_rationale: row.get(4)?,
                            prior_escalated: row.get::<_, i32>(5)? != 0,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| AppError::Validation("unknown review flag effect id".into()))?;
            let current = Self::decision_snapshot_on(&tx, &effect.segment_id)?
                .ok_or_else(|| AppError::Validation("cannot undo a flag whose segment no longer exists".into()))?;
            let prior_reversal: Option<String> = tx
                .query_row(
                    "SELECT operation_id FROM review_flag_effect_reversals WHERE flag_effect_event_id = ?1",
                    params![effect_event_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(prior_operation) = prior_reversal {
                tx.rollback()?;
                return Ok(if prior_operation == operation_id {
                    HumanFlagUndoOutcome::AlreadyApplied { segment: current.0 }
                } else {
                    HumanFlagUndoOutcome::Conflict { segment: current.0 }
                });
            }
            let later_review_mutation: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1
                       FROM review_flag_effect_events newer
                      WHERE newer.segment_id = ?1
                        AND newer.flag_revision > ?2
                     UNION ALL
                     SELECT 1
                       FROM human_decision_effect_events decision
                      WHERE decision.segment_id = ?1
                        AND decision.decision_revision > ?2
                 )",
                params![effect.segment_id, effect.flag_revision],
                |row| row.get(0),
            )?;
            if current.1 < effect.flag_revision || later_review_mutation {
                tx.rollback()?;
                return Ok(HumanFlagUndoOutcome::Conflict { segment: current.0 });
            }
            let changed = tx.execute(
                "UPDATE speech_segments
                    SET verdict = ?2, rationale = ?3, escalated = ?4,
                        updated_at = datetime('now')
                  WHERE id = ?1 AND review_revision = ?5
                    AND verdict = 'escalated' AND escalated = 1
                    AND rationale = ?6
                    AND (human_decision IS NULL OR human_decision = '')",
                params![
                    effect.segment_id,
                    effect.prior_verdict,
                    effect.prior_rationale,
                    effect.prior_escalated as i32,
                    current.1,
                    effect.flag_rationale,
                ],
            )?;
            if changed == 0 {
                tx.rollback()?;
                return Ok(HumanFlagUndoOutcome::Conflict { segment: current.0 });
            }
            let restored_revision: i64 = tx.query_row(
                "SELECT review_revision FROM speech_segments WHERE id = ?1",
                params![effect.segment_id],
                |row| row.get(0),
            )?;
            if current.1 + 1 != restored_revision {
                return Err(AppError::Other("review flag undo did not advance exactly one revision".into()));
            }
            tx.execute(
                "INSERT INTO review_flag_effect_reversals (flag_effect_event_id, operation_id)
                 VALUES (?1, ?2)",
                params![effect_event_id, operation_id],
            )?;
            let segment = Self::decision_snapshot_on(&tx, &effect.segment_id)?
                .ok_or_else(|| AppError::Other("segment disappeared inside review flag undo".into()))?
                .0;
            tx.commit()?;
            Ok(HumanFlagUndoOutcome::Applied { restored_revision, segment })
        })?;
        if matches!(&outcome, HumanFlagUndoOutcome::Applied { .. }) {
            self.track_write()?;
        }
        Ok(outcome)
    }
    /// Reverse a UI `flag()` escalation (the review-inbox Undo path): clear the `escalated` flag and the
    /// machine `'escalated'` verdict + rationale that flag wrote, WITHOUT touching a human_decision (flag
    /// never sets one). This is the exact inverse of flag — unlike `clear_human_decision`, which
    /// deliberately SETS escalated=1 to reopen a human-decided row for re-adjudication. Guarded to a
    /// still-undecided row so it can never stomp a human decision made after the flag; idempotent. Every
    /// SET expression references the row's PRE-UPDATE values (SQLite semantics), so both CASEs see the
    /// original verdict.
    pub fn clear_escalation(&self, segment_id: &str) -> AppResult<()> {
        let _ = segment_id;
        Err(AppError::Validation(
            "clear_escalation is disabled: undo requires an immutable review-flag effect id and operation UUID".into(),
        ))
    }

    /// Capture a MODEL correction (the jury auto-correcting OmniASR) as a provenance-tagged PSEUDO
    /// example: `source='model'`, `verified_by_human=0`. Unlike a human edit, this is NOT trusted
    /// training data — it is a candidate for human review / a future gated pseudo-label pass, and is
    /// excluded from the DPO export and few-shot context until a human signs off. Training directly
    /// on model-generated corrections causes model collapse (Shumailov et al., Nature 2024), so this
    /// path only RECORDS; it never promotes a label into the trainable pool.
    ///
    /// No-ops when the corrected text equals the wrong text (not a real correction) or the segment is
    /// gold/holdout (quarantined at capture). Best-effort: returns Ok even when it records nothing.
    pub fn record_model_correction(
        &self,
        segment_id: &str,
        wrong_transcript: &str,
        corrected_transcript: &str,
        corrector_model_id: &str,
    ) -> AppResult<()> {
        let wrong = wrong_transcript.trim();
        let fix = corrected_transcript.trim();
        if fix.is_empty() || wrong == fix {
            return Ok(()); // not a correction
        }
        // Quarantine gold at capture time (holdout exclusion is also applied at every export). Distinguish
        // "no such segment" (genuinely not gold -> 0) from a TRANSIENT read error (e.g. SQLITE_BUSY after
        // the busy_timeout, under a long adjudication on the other connection): the latter must NOT
        // fail-OPEN the quarantine by defaulting to 0 and writing a model pseudo-label onto a gold row —
        // propagate it so the best-effort caller simply skips this capture.
        let is_gold: i64 =
            match self
                .conn
                .query_row("SELECT is_gold FROM speech_segments WHERE id = ?1", params![segment_id], |r| r.get(0))
            {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => 0,
                Err(e) => return Err(e.into()),
            };
        if is_gold != 0 {
            return Ok(());
        }
        let example_id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO agent_examples
                 (id, segment_id, wrong_transcript, human_fix, source, verified_by_human, corrector_model_id)
             VALUES (?1, ?2, ?3, ?4, 'model', 0, ?5)",
            params![example_id, segment_id, wrong, fix, corrector_model_id],
        )?;
        Ok(())
    }

    /// Record a human decision (accept/edit/reject) and optionally store a
    /// corrected transcript.  Gold segments are updated but never written to
    /// agent_examples. M2.1: Also logs decision timing to decision_log table.
    #[cfg(test)]
    pub fn record_human_decision(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
    ) -> AppResult<()> {
        self.record_human_decision_by(segment_id, decision, corrected_transcript, timestamp_ms, None)
    }

    /// Legacy desktop decision entry point. Desktop effects are deliberately anonymous; named reviewers
    /// must use the phone writer, which binds their identity to the review event, compensation row, and
    /// immutable effect. Supplying `annotator` therefore fails closed instead of creating an effect whose
    /// post-state cannot be authenticated during exact Undo.
    #[cfg(test)]
    pub fn record_human_decision_by(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        annotator: Option<&str>,
    ) -> AppResult<()> {
        self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            timestamp_ms,
            annotator,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
        )?;
        Ok(())
    }

    /// Phone review is a complete adjudication, so the transcript/verdict, attribution, learning
    /// side-effects, annotation, and `verified` flag must share one commit. Keeping finalization in
    /// this transaction prevents an interrupted second write from leaving a decided-but-pending row.
    #[cfg(test)]
    pub fn record_phone_human_decision_by(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        annotator: &str,
    ) -> AppResult<()> {
        let expected_revision = self
            .segment_review_revision(segment_id)?
            .ok_or_else(|| AppError::Other(format!("segment {segment_id} no longer exists")))?;
        match self.record_phone_human_decision_by_at_revision(
            segment_id,
            decision,
            corrected_transcript,
            annotator,
            expected_revision,
        )? {
            Some(_) => Ok(()),
            None => Err(AppError::Other(
                "segment changed while the phone decision was being recorded; reload and retry".into(),
            )),
        }
    }

    /// Atomically record a phone decision only if the row is still the exact revision that was
    /// served/read. `Ok(None)` is a normal compare-and-swap miss: no row or learning side effect was
    /// written. `Ok(Some(revision))` returns the post-decision revision for a safe undo token.
    #[cfg(test)]
    pub fn record_phone_human_decision_by_at_revision(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        annotator: &str,
        expected_revision: i64,
    ) -> AppResult<Option<i64>> {
        self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            None,
            Some(annotator),
            Some(expected_revision),
            true,
            // The phone's pay-bearing audit row commits WITH the decision (2026-08-20 hunt).
            Some("couch"),
            None,
            None,
            None,
            None,
            None,
        )
        .map(|commit| commit.map(|value| value.decided_revision))
    }

    /// Production phone write with a client-authored idempotency identity committed in the same
    /// transaction as verdict, event, and compensation. HTTP retries must use this variant.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn record_phone_human_decision_by_at_revision_with_operation(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        annotator: &str,
        expected_revision: i64,
        operation_id: &str,
        operation_payload_hash: &str,
    ) -> AppResult<Option<i64>> {
        validate_review_operation_identity(operation_id, operation_payload_hash)?;
        self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            None,
            Some(annotator),
            Some(expected_revision),
            true,
            Some("couch"),
            Some((operation_id, operation_payload_hash)),
            Some((decision, corrected_transcript.unwrap_or_default())),
            None,
            None,
            None,
        )
        .map(|commit| commit.map(|value| value.decided_revision))
    }

    /// The controlled-pilot production variant. The limit is checked under an IMMEDIATE SQLite
    /// transaction before the segment, audit event, or compensation ledger can change.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_phone_human_decision_by_at_revision_with_operation_limit(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        annotator: &str,
        expected_revision: i64,
        playback: &PlaybackDecisionProof,
        operation_id: &str,
        operation_payload_hash: &str,
        requested_action: &str,
        requested_transcript: &str,
        decision_limit: Option<&ReviewDecisionLimit>,
    ) -> AppResult<Option<HumanDecisionCommit>> {
        validate_review_operation_identity(operation_id, operation_payload_hash)?;
        if playback.segment_revision != expected_revision {
            return Err(AppError::Validation(
                "playback proof revision does not match the served decision revision".into(),
            ));
        }
        self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            None,
            Some(annotator),
            Some(expected_revision),
            true,
            Some("couch"),
            Some((operation_id, operation_payload_hash)),
            Some((requested_action, requested_transcript)),
            decision_limit,
            Some(playback),
            None,
        )
    }

    /// Record a DESKTOP adjudication complete: decision, transcript and `verified` in
    /// one commit, so an interrupted second write can never leave a decided-but-pending row.
    /// The desktop boundary is anonymous: `annotator = Some(_)` is rejected; named work must use
    /// the attributed Couch writer so its effect and compensation identities remain exact.
    ///
    /// The phone has had this since the finalization transaction was written; the desktop reached
    /// `verified` through a separate `update_segment_fields` call that ReviewInbox never made.
    pub fn finalize_human_review(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        annotator: Option<&str>,
    ) -> AppResult<()> {
        self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            timestamp_ms,
            annotator,
            None,
            true,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .map(|_| ())
    }

    /// Finalize a desktop adjudication only while the exact anonymous playback proof remains valid.
    /// The useful preflight error is produced by the command layer; this is the authority check,
    /// repeated under the same IMMEDIATE transaction and revision CAS as the decision itself.
    #[cfg(test)]
    pub(crate) fn finalize_human_review_with_playback(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        playback: &PlaybackDecisionProof,
        operation_id: &str,
    ) -> AppResult<HumanDecisionCommit> {
        validate_operation_uuid(operation_id)?;
        let operation_payload_hash =
            desktop_decision_payload_hash(segment_id, decision, corrected_transcript, timestamp_ms);
        match self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            timestamp_ms,
            None,
            Some(playback.segment_revision),
            true,
            None,
            Some((operation_id, &operation_payload_hash)),
            None,
            None,
            Some(playback),
            None,
        )? {
            Some(commit) => Ok(commit),
            None => Err(AppError::Validation(format!(
                "{PLAYBACK_EVIDENCE_CHANGED}: clip identity, revision, or playback proof changed while the desktop decision was being saved"
            ))),
        }
    }

    /// Typed desktop review commit. The supplied revision is checked again inside the same
    /// `BEGIN IMMEDIATE` transaction as playback proof, human truth, effect identity and undo state.
    pub(crate) fn finalize_desktop_review_v1_with_playback(
        &self,
        segment_id: &str,
        base_revision: i64,
        decision: &str,
        corrected_transcript: Option<&str>,
        playback: &PlaybackDecisionProof,
        operation_id: &str,
    ) -> AppResult<HumanDecisionCommit> {
        validate_operation_uuid(operation_id)?;
        let playback_authority_session_id = playback.authority_session_id.as_deref().ok_or_else(|| {
            AppError::Validation("typed desktop review requires an exact policy-4 playback authority".into())
        })?;
        let operation_payload_hash = desktop_review_v1_payload_hash(
            segment_id,
            base_revision,
            decision,
            corrected_transcript,
            playback_authority_session_id,
        );
        match self.record_human_decision_by_with_finalize(
            segment_id,
            decision,
            corrected_transcript,
            None,
            None,
            Some(base_revision),
            true,
            None,
            Some((operation_id, &operation_payload_hash)),
            None,
            None,
            Some(playback),
            Some(base_revision),
        )? {
            Some(commit) => Ok(commit),
            None => Err(AppError::Validation(format!(
                "{PLAYBACK_EVIDENCE_CHANGED}: clip identity, revision, or playback proof changed while the desktop decision was being saved"
            ))),
        }
    }

    /// Finish a row written by an older release that committed the decision but not phone
    /// finalization. This intentionally does not replay learning/audit side effects.
    pub fn finalize_phone_human_decision(&self, segment_id: &str, corrected_transcript: Option<&str>) -> AppResult<()> {
        let expected_revision = self
            .segment_review_revision(segment_id)?
            .ok_or_else(|| AppError::Other(format!("segment {segment_id} no longer exists")))?;
        match self.finalize_phone_human_decision_at_revision(segment_id, corrected_transcript, expected_revision)? {
            Some(_) => Ok(()),
            None => Err(AppError::Other(
                "segment changed while the interrupted phone decision was being finalized; reload and retry".into(),
            )),
        }
    }

    pub fn finalize_phone_human_decision_at_revision(
        &self,
        segment_id: &str,
        corrected_transcript: Option<&str>,
        expected_revision: i64,
    ) -> AppResult<Option<i64>> {
        if crate::migrations::get_current_version(self)? >= 60 {
            return Err(AppError::Validation(
                "legacy interrupted phone decision requires the offline repair lane; schema-v60 runtime cannot mutate human truth without an immutable effect"
                    .into(),
            ));
        }
        let corrected = corrected_transcript.map(|text| to_nfc(text.trim())).filter(|text| !text.is_empty());
        let changed = self.conn.execute(
            "UPDATE speech_segments
             SET annotated_transcript = COALESCE(?2, annotated_transcript),
                 verified = 1,
                 updated_at = datetime('now')
             WHERE id = ?1 AND human_decision IS NOT NULL AND review_revision = ?3",
            params![segment_id, corrected, expected_revision],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let revision = self
            .segment_review_revision(segment_id)?
            .ok_or_else(|| AppError::Other(format!("segment {segment_id} disappeared after finalization")))?;
        self.track_write()?;
        Ok(Some(revision))
    }

    /// Collapse runs of whitespace, exactly as `check_review_serving_provenance._norm` does.
    /// A differing space is not a differing transcript, and the two sides of this invariant must
    /// agree byte-for-byte or the gate fails on rows the backend believed it had classified.
    pub(super) fn provenance_norm(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// What a human decision REALLY is, decided from the stored bytes rather than the client's word.
    ///
    /// `accept` asserts a specific, checkable thing: an ASR engine produced this exact text and a
    /// human approved it unchanged. That is true on a first review and routinely FALSE on a
    /// re-review, where the displayed text is a previous human's correction. Recording the latter as
    /// `accept` launders human authorship into machine provenance — the row then claims an engine
    /// wrote words no engine ever emitted, and `check_review_serving_provenance` rejects it.
    ///
    /// So: an accept whose approved text matches one of the segment's own hypotheses stays an
    /// accept; one that matches none is reclassified `edit`, which is what it actually is — a human
    /// affirming human-authored text. The text is never invented, only carried forward.
    ///
    /// Returns the effective decision and, when it had to resolve the approved text itself, that text.
    pub(super) fn authoritative_decision_on(
        conn: &Connection,
        segment_id: &str,
        requested: &str,
        approved: Option<&str>,
    ) -> AppResult<(String, Option<String>)> {
        use rusqlite::OptionalExtension;
        if requested != "accept" {
            return Ok((requested.to_string(), None));
        }
        // Accept-what-you-SEE passes the displayed text; when it does not, the approved text is
        // whatever the export would ship for this row today.
        let (approved_text, resolved): (String, Option<String>) = match approved {
            Some(text) if !text.trim().is_empty() => (text.to_string(), None),
            _ => {
                let shipped: Option<String> = conn
                    .query_row(
                        "SELECT COALESCE(NULLIF(TRIM(verdict_transcript), ''),                                  NULLIF(TRIM(annotated_transcript), ''),                                  raw_transcript)                          FROM speech_segments WHERE id = ?1",
                        [segment_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                match shipped.map(|text| to_nfc(text.trim())).filter(|text| !text.is_empty()) {
                    Some(text) => (text.clone(), Some(text)),
                    // Nothing to verify against; leave the caller's decision untouched rather than
                    // invent a classification.
                    None => return Ok((requested.to_string(), None)),
                }
            }
        };

        let wanted = Self::provenance_norm(&approved_text);
        let mut stmt = conn.prepare("SELECT transcript FROM segment_hypotheses WHERE segment_id = ?1")?;
        let mut rows = stmt.query([segment_id])?;
        let mut saw_any_hypothesis = false;
        while let Some(row) = rows.next()? {
            let hypothesis: String = row.get(0)?;
            saw_any_hypothesis = true;
            if Self::provenance_norm(&hypothesis) == wanted {
                return Ok(("accept".to_string(), resolved));
            }
        }
        // Only a CONTRADICTION justifies reclassifying. With no hypotheses on file there is nothing
        // to contradict, and calling the decision an "edit" would launder in the other direction —
        // dressing a genuine ASR accept as human authorship because provenance was never recorded.
        // That is a data-completeness problem, and `check_review_serving_provenance` reports it as
        // one ("no ASR hypothesis on file to justify the accept"); it is not this function's to hide.
        if !saw_any_hypothesis {
            return Ok((requested.to_string(), resolved));
        }
        tracing::info!(
            "segment {segment_id}: accept reclassified as edit — the approved text matches none of              this segment's ASR hypotheses, so it is human-authored, not an engine transcript"
        );
        Ok(("edit".to_string(), Some(approved_text)))
    }

    /// Re-derive the payable phone action from the exact text the Couch surface serves.
    ///
    /// The request's button/action is not financial provenance. A client can send `edit` for an NFC
    /// or whitespace-only rewrite; both are a no-op under the same key that suppresses degenerate
    /// learning pairs. Classifying again in the database boundary keeps every direct/replayed phone
    /// call on the 10% accept rate unless the retained words materially changed.
    pub(super) fn phone_compensation_action_on(
        conn: &Connection,
        segment_id: &str,
        requested: &str,
        submitted: Option<&str>,
    ) -> AppResult<String> {
        match requested {
            "accept" | "edit" => {
                let served: String = conn.query_row(
                    "SELECT COALESCE(NULLIF(TRIM(annotated_transcript), ''), raw_transcript)
                       FROM speech_segments WHERE id = ?1",
                    [segment_id],
                    |row| row.get(0),
                )?;
                let approved = submitted.unwrap_or(&served);
                let served_key = learning_text_key(&to_nfc(served.trim()));
                let approved_key = learning_text_key(&to_nfc(approved.trim()));
                Ok(if served_key == approved_key { "accept" } else { "edit" }.to_string())
            }
            "reject" => Ok("reject".to_string()),
            other => Err(AppError::Validation(format!("unsupported phone compensation action {other:?}"))),
        }
    }
}
