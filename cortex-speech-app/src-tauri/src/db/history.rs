//! Exact, fail-closed persistence for desktop Undo/Redo endpoints.
//!
//! This sibling owns only machine-history projection changes. Review truth and source identity are
//! compared but never authored, while multi-row inverses hold one writer reservation and savepoint.

use super::*;

/// Retranscription may replace transcript-dependent `words` while the four source-span fields remain
/// immutable. Generic history keeps requiring byte-identical alignment JSON; this narrower comparator
/// is used only by the bound champion endpoint that validates the exact current JSON separately.
fn transcription_source_identity_matches(left: &SpeechSegment, right: &SpeechSegment) -> bool {
    if left.id != right.id || left.audio_path != right.audio_path || left.duration_ms != right.duration_ms {
        return false;
    }
    if left.alignment_json == right.alignment_json {
        return true;
    }
    let full_file_meta = |segment: &SpeechSegment| {
        if segment.duration_ms <= 0 {
            return None;
        }
        match segment.alignment_json.as_deref() {
            Some(json) => crate::chunking::SegmentSourceMeta::from_alignment_json(json),
            // Historical whole-file rows legitimately use NULL alignment. Adding word timings now
            // promotes that implicit span to an explicit 0..duration, one-chunk identity so future
            // transcription never sees a present-but-offset-less (and therefore unsafe) JSON blob.
            None => Some(crate::chunking::SegmentSourceMeta {
                source_start_ms: 0,
                source_end_ms: segment.duration_ms,
                chunk_index: 0,
                chunk_count: 1,
            }),
        }
    };
    let left_meta = full_file_meta(left);
    let right_meta = full_file_meta(right);
    left_meta.is_some() && left_meta == right_meta
}

impl Database {
    /// Commit one bound champion result and capture both exact machine+hypothesis endpoints in the
    /// same outer savepoint. A caller may publish the returned pair to the in-memory Undo manager only
    /// after this method succeeds; any projection read/cap failure rolls the transcript commit back.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_bound_champion_transcript_with_history(
        &self,
        champion: &SegmentHypothesis,
        expected_deployment_sha256: Option<&str>,
        normalized_transcript: Option<&str>,
        confidence_source: Option<&str>,
        cloud_call: bool,
        decoder_config_sha256: &str,
        normalizer_version: Option<&str>,
        replacement_alignment_json: Option<&str>,
        replacement_alignment_quality: Option<&str>,
        expected_source: &ChampionTranscriptionSourceSnapshot,
    ) -> AppResult<Option<(BatchSegmentProjectionV1, BatchSegmentProjectionV1)>> {
        self.conn.execute("SAVEPOINT champion_history_commit", [])?;
        let result = (|| -> AppResult<Option<(BatchSegmentProjectionV1, BatchSegmentProjectionV1)>> {
            let before = Self::read_batch_projection_on(&self.conn, &champion.segment_id)?.ok_or_else(|| {
                AppError::Validation(format!(
                    "Cannot transcribe segment '{}': it no longer exists",
                    champion.segment_id
                ))
            })?;
            let committed = self.commit_champion_transcript_inner(
                champion,
                expected_deployment_sha256,
                normalized_transcript,
                confidence_source,
                cloud_call,
                Some(decoder_config_sha256),
                normalizer_version,
                Some(expected_source),
            )?;
            if !committed {
                return Ok(None);
            }
            let alignment_changed = self.conn.execute(
                "UPDATE speech_segments
                    SET alignment_json=?2, alignment_quality=?3, ctc_score=NULL,
                        updated_at=datetime('now')
                  WHERE id=?1",
                params![champion.segment_id, replacement_alignment_json, replacement_alignment_quality],
            )?;
            if alignment_changed != 1 {
                return Err(AppError::Other(format!(
                    "segment '{}' disappeared while invalidating its previous transcript alignment",
                    champion.segment_id
                )));
            }
            let after = Self::read_batch_projection_on(&self.conn, &champion.segment_id)?.ok_or_else(|| {
                AppError::Other(format!("segment '{}' disappeared after its champion commit", champion.segment_id))
            })?;
            if before.schema != 1
                || after.schema != 1
                || before.segment.id != after.segment.id
                // Both the speech-row update and the schema-68 hypothesis triggers advance this
                // database-owned CAS authority. Equal or decreasing revisions mean the supposedly
                // committed endpoint was not produced by the protected mutation we just executed.
                || after.review_revision <= before.review_revision
                || before.audio_content_hash != after.audio_content_hash
                || !transcription_source_identity_matches(&before.segment, &after.segment)
                || !review_owned_projection_matches(&before.segment, &after.segment)
            {
                return Err(AppError::Other(format!(
                    "champion history endpoints for segment '{}' changed protected source/review authority",
                    champion.segment_id
                )));
            }
            Ok(Some((before, after)))
        })();
        match result {
            Ok(endpoints) => {
                self.release_savepoint("champion_history_commit")?;
                Ok(endpoints)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("champion_history_commit");
                Err(error)
            }
        }
    }

    /// Apply one generic history endpoint without ever writing review-owned or source-identity
    /// columns. The two immutable command endpoints must agree on those protected projections, and
    /// the current machine/source projection must still equal the endpoint being reversed. A later
    /// human decision is deliberately allowed: its current review projection is left byte-for-byte
    /// untouched while the older machine edit is undone/redone.
    pub(crate) fn apply_history_machine_snapshot(
        &self,
        expected: &SpeechSegment,
        desired: &SpeechSegment,
    ) -> AppResult<()> {
        if crate::migrations::get_current_version(self)? < 60 {
            return self.insert_segment(desired);
        }
        if expected.id != desired.id {
            return Err(AppError::Validation("history update endpoints must identify the same segment".into()));
        }
        if !review_owned_projection_matches(expected, desired) {
            return Err(AppError::Validation(format!(
                "history update for segment {} attempted to mutate review-owned truth",
                expected.id
            )));
        }
        if !history_source_identity_matches(expected, desired) {
            return Err(AppError::Validation(format!(
                "history update for segment {} attempted to mutate protected source identity",
                expected.id
            )));
        }
        let current = self.get_segment_by_id(&expected.id)?.ok_or_else(|| {
            AppError::Validation(format!("Cannot apply history for segment {}: it no longer exists", expected.id))
        })?;
        if !history_source_identity_matches(&current, expected)
            || !history_machine_projection_matches(&current, expected)
        {
            return Err(AppError::Validation(format!(
                "Cannot apply stale history for segment {}: its machine/source state changed after the recorded edit",
                expected.id
            )));
        }

        let raw_nfc = to_nfc(&desired.raw_transcript);
        let normalized_nfc = desired.normalized_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "UPDATE speech_segments SET
                raw_transcript=?2, normalized_transcript=?3, speaker_id=?4,
                confidence=?5, ctc_score=?6, clipping_ratio=?7, rms_db=?8, snr_db=?9,
                split=?10, signal_anomaly_score=?11, alignment_quality=?12,
                model_version_id=COALESCE(?13, 'unknown@pre-registry'),
                confidence_source=COALESCE(?14, 'unknown'), cloud_call=?15,
                decoder_config_hash=?16, normalizer_version=?17, denoised=?18,
                diarized=?19, vad_backend=?20, speaker_change_score=?21,
                updated_at=datetime('now')
             WHERE id=?1",
            params![
                desired.id,
                raw_nfc,
                normalized_nfc,
                desired.speaker_id,
                desired.confidence,
                desired.ctc_score,
                desired.clipping_ratio,
                desired.rms_db,
                desired.snr_db,
                desired.split,
                desired.signal_anomaly_score,
                desired.alignment_quality,
                desired.model_version_id,
                desired.confidence_source,
                desired.cloud_call as i32,
                desired.decoder_config_hash,
                desired.normalizer_version,
                desired.denoised.map(|value| value as i32),
                desired.diarized.map(|value| value as i32),
                desired.vad_backend,
                desired.speaker_change_score,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Writer-reserved wrapper for the read/compare/write history transition above. Holding the
    /// reservation across its validation read closes the external-connection race where a row could
    /// change after comparison but before the UPDATE.
    pub(crate) fn apply_history_machine_snapshot_atomic(
        &self,
        expected: &SpeechSegment,
        desired: &SpeechSegment,
    ) -> AppResult<()> {
        self.conn.execute("SAVEPOINT history_machine_snapshot", [])?;
        let result: AppResult<()> = (|| {
            self.conn.execute("UPDATE speech_segments SET id = id WHERE 0", [])?;
            self.apply_history_machine_snapshot(expected, desired)
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("history_machine_snapshot")?;
                self.track_write()?;
                Ok(())
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("history_machine_snapshot");
                Err(error)
            }
        }
    }

    /// Exact single-clip retranscription Undo/Redo. Segment machine fields and the complete hypothesis
    /// set move together; source identity must remain unchanged. Review truth that lands later is never
    /// authored here, but any intervening machine/hypothesis edit makes the history endpoint stale and
    /// the operation changes nothing.
    pub(crate) fn apply_history_machine_projection_atomic(
        &self,
        expected: &BatchSegmentProjectionV1,
        desired: &BatchSegmentProjectionV1,
    ) -> AppResult<BatchSegmentProjectionV1> {
        if expected.schema != 1
            || desired.schema != 1
            || expected.segment.id != desired.segment.id
            || expected.review_revision < 0
            || desired.review_revision < 0
            || expected.audio_content_hash != desired.audio_content_hash
            || !transcription_source_identity_matches(&expected.segment, &desired.segment)
            || !review_owned_projection_matches(&expected.segment, &desired.segment)
        {
            return Err(AppError::Validation(
                "machine history endpoints disagree on protected source/review authority".into(),
            ));
        }
        if expected
            .hypotheses
            .iter()
            .chain(desired.hypotheses.iter())
            .any(|hypothesis| hypothesis.segment_id != expected.segment.id)
        {
            return Err(AppError::Validation("machine history contains a hypothesis for the wrong segment".into()));
        }

        self.conn.execute("SAVEPOINT history_machine_projection", [])?;
        let result = (|| -> AppResult<BatchSegmentProjectionV1> {
            // Take the writer reservation before reading the endpoint so no external connection can
            // change the row or hypothesis set between comparison and replacement.
            self.conn.execute("UPDATE speech_segments SET id=id WHERE 0", [])?;
            let current = Self::read_batch_projection_on(&self.conn, &expected.segment.id)?.ok_or_else(|| {
                AppError::Validation(format!(
                    "Cannot apply history for segment {}: it no longer exists",
                    expected.segment.id
                ))
            })?;
            if current.review_revision != expected.review_revision
                || !transcription_source_identity_matches(&current.segment, &expected.segment)
                || current.audio_content_hash != expected.audio_content_hash
                || current.segment.alignment_json != expected.segment.alignment_json
                || !history_machine_projection_matches(&current.segment, &expected.segment)
                || !review_owned_projection_matches(&current.segment, &expected.segment)
                || current.hypotheses != expected.hypotheses
            {
                return Err(AppError::Validation(format!(
                    "Cannot apply stale history for segment {}: its machine/source/hypothesis state changed after the recorded transcription",
                    expected.segment.id
                )));
            }

            // Generic machine history treats alignment JSON as protected. Give it equal source JSON
            // while it restores every other machine field, then replace the already-validated exact
            // transcript-alignment endpoint inside this same savepoint.
            let mut desired_machine = desired.segment.clone();
            desired_machine.alignment_json = expected.segment.alignment_json.clone();
            self.apply_history_machine_snapshot(&expected.segment, &desired_machine)?;
            let alignment_changed = self.conn.execute(
                "UPDATE speech_segments SET alignment_json=?2, updated_at=datetime('now') WHERE id=?1",
                params![desired.segment.id, desired.segment.alignment_json],
            )?;
            if alignment_changed != 1 {
                return Err(AppError::Other(format!(
                    "segment '{}' disappeared while applying its exact history alignment",
                    desired.segment.id
                )));
            }
            self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id=?1", [&expected.segment.id])?;
            let mut insert = self.conn.prepare(
                "INSERT INTO segment_hypotheses(
                    segment_id,model_id,transcript,confidence,model_version_id,created_at
                 ) VALUES (?1,?2,?3,?4,?5,?6)",
            )?;
            for hypothesis in &desired.hypotheses {
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

            let applied = Self::read_batch_projection_on(&self.conn, &desired.segment.id)?.ok_or_else(|| {
                AppError::Other(format!("segment '{}' disappeared while applying history", desired.segment.id))
            })?;
            if !transcription_source_identity_matches(&applied.segment, &desired.segment)
                || applied.audio_content_hash != desired.audio_content_hash
                || applied.segment.alignment_json != desired.segment.alignment_json
                || !history_machine_projection_matches(&applied.segment, &desired.segment)
                || applied.hypotheses != desired.hypotheses
                // History is an ordinary new database mutation. It restores semantic content but
                // never rewinds the CAS clock to the historical endpoint revision.
                || applied.review_revision <= current.review_revision
                || !review_owned_projection_matches(&applied.segment, &current.segment)
            {
                return Err(AppError::Other(format!(
                    "history verification failed for segment '{}'; the complete change was rolled back",
                    desired.segment.id
                )));
            }
            Ok(applied)
        })();
        match result {
            Ok(applied) => {
                self.release_savepoint("history_machine_projection")?;
                self.track_write()?;
                Ok(applied)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("history_machine_projection");
                Err(error)
            }
        }
    }

    /// Regression-only model of the retired pre-journal batch inverse. Production batch history is
    /// now applied through the schema-68 journal, so this helper must not be callable outside tests.
    #[cfg(test)]
    pub(crate) fn restore_batch_transcription_snapshot(&self, previous: &SpeechSegment) -> AppResult<()> {
        if crate::migrations::get_current_version(self)? < 60 {
            return self.insert_segment(previous);
        }
        let current = self.get_segment_by_id(&previous.id)?.ok_or_else(|| {
            AppError::Validation(format!(
                "Cannot undo batch transcription for segment {}: it no longer exists",
                previous.id
            ))
        })?;
        if !history_source_identity_matches(&current, previous) {
            return Err(AppError::Validation(format!(
                "Cannot undo batch transcription for segment {}: its protected source identity changed",
                previous.id
            )));
        }
        let human_landed_after_batch = (current.verified && !previous.verified)
            || (current.is_gold && !previous.is_gold)
            || (current.human_decision.is_some() && previous.human_decision.is_none())
            || (current.verdict.is_some() && previous.verdict.is_none())
            || (current.annotated_transcript.is_some() && previous.annotated_transcript.is_none());
        if human_landed_after_batch {
            return Err(AppError::Validation(format!(
                "Cannot undo batch transcription for segment {}: a human decision landed after the batch",
                previous.id
            )));
        }
        let raw_nfc = to_nfc(&previous.raw_transcript);
        let normalized_nfc = previous.normalized_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "UPDATE speech_segments SET
                raw_transcript=?2, normalized_transcript=?3, confidence=?4,
                confidence_source=COALESCE(?5, 'unknown'),
                model_version_id=COALESCE(?6, 'unknown@pre-registry'), cloud_call=?7,
                updated_at=datetime('now')
             WHERE id=?1",
            params![
                previous.id,
                raw_nfc,
                normalized_nfc,
                previous.confidence,
                previous.confidence_source,
                previous.model_version_id,
                previous.cloud_call as i32,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Apply the inverse or forward form of one exact deleted-segment history command. Restores
    /// require every id to still be absent. Redos require every current row to match the snapshot
    /// restored by Undo; a later edit or partial external deletion therefore deletes nothing.
    pub(crate) fn apply_deleted_segments_history(&self, segments: &[SpeechSegment], forward: bool) -> AppResult<()> {
        let mut unique_ids = HashSet::with_capacity(segments.len());
        if segments.is_empty() || segments.iter().any(|segment| !unique_ids.insert(segment.id.as_str())) {
            return Err(AppError::Validation(
                "deleted-segment history requires a non-empty set of unique segment ids".into(),
            ));
        }

        self.conn.execute("SAVEPOINT history_deleted_segments", [])?;
        let result: AppResult<()> = (|| {
            self.conn.execute("UPDATE speech_segments SET id = id WHERE 0", [])?;
            if forward {
                for expected in segments {
                    let current = self.get_segment_by_id(&expected.id)?.ok_or_else(|| {
                        AppError::Validation(format!(
                            "Cannot redo deletion for segment {}: it no longer exists",
                            expected.id
                        ))
                    })?;
                    if current.created_at != expected.created_at
                        || !review_owned_projection_matches(&current, expected)
                        || !history_source_identity_matches(&current, expected)
                        || !history_machine_projection_matches(&current, expected)
                    {
                        return Err(AppError::Validation(format!(
                            "Cannot redo stale deletion for segment {}: it changed after Undo",
                            expected.id
                        )));
                    }
                }
                let ids = segments.iter().map(|segment| segment.id.clone()).collect::<Vec<_>>();
                self.delete_segments_batch(&ids)?;
            } else {
                for segment in segments {
                    if self.get_segment_by_id(&segment.id)?.is_some() {
                        return Err(AppError::Validation(format!(
                            "Cannot undo deletion for segment {}: that id is already present",
                            segment.id
                        )));
                    }
                    self.insert_segment_full(segment)?;
                }
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.release_savepoint("history_deleted_segments")?;
                self.track_write()?;
                Ok(())
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("history_deleted_segments");
                Err(error)
            }
        }
    }
}
