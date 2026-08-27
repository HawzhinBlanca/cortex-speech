//! Exact, fail-closed persistence for desktop Undo/Redo endpoints.
//!
//! This sibling owns only machine-history projection changes. Review truth and source identity are
//! compared but never authored, while multi-row inverses hold one writer reservation and savepoint.

use super::*;

impl Database {
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

    /// Restore exactly the columns owned by one batch-transcription endpoint. Review/source fields
    /// are never named, and a later human decision makes the inverse ineligible.
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

    /// Apply one batch-transcription history endpoint as a compare-and-set, all-or-nothing action.
    /// Both endpoint vectors are server snapshots; renderer data never enters this boundary.
    pub(crate) fn apply_batch_transcription_history(
        &self,
        previous_segments: &[SpeechSegment],
        current_segments: &[SpeechSegment],
        forward: bool,
    ) -> AppResult<()> {
        let mut unique_ids = HashSet::with_capacity(previous_segments.len());
        if previous_segments.is_empty()
            || previous_segments.len() != current_segments.len()
            || previous_segments.iter().any(|segment| !unique_ids.insert(segment.id.as_str()))
        {
            return Err(AppError::Validation(
                "batch transcription history requires equal non-empty endpoints with unique segment ids".into(),
            ));
        }
        let current_by_id: HashMap<&str, &SpeechSegment> =
            current_segments.iter().map(|segment| (segment.id.as_str(), segment)).collect();
        if current_by_id.len() != current_segments.len() {
            return Err(AppError::Validation(
                "batch transcription history current endpoints contain duplicate segment ids".into(),
            ));
        }
        for previous in previous_segments {
            let current = current_by_id.get(previous.id.as_str()).ok_or_else(|| {
                AppError::Validation(format!(
                    "batch transcription history has no current endpoint for segment {}",
                    previous.id
                ))
            })?;
            if !history_source_identity_matches(previous, current)
                || !review_owned_projection_matches(previous, current)
            {
                return Err(AppError::Validation(format!(
                    "batch transcription history endpoints changed protected truth for segment {}",
                    previous.id
                )));
            }
        }

        self.conn.execute("SAVEPOINT history_batch_transcription", [])?;
        let result: AppResult<()> = (|| {
            self.conn.execute("UPDATE speech_segments SET id = id WHERE 0", [])?;
            for previous in previous_segments {
                let current = current_by_id[previous.id.as_str()];
                let (expected, desired) = if forward { (previous, current) } else { (current, previous) };
                let actual = self.get_segment_by_id(&expected.id)?.ok_or_else(|| {
                    AppError::Validation(format!(
                        "Cannot apply batch transcription history for segment {}: it no longer exists",
                        expected.id
                    ))
                })?;
                if !history_source_identity_matches(&actual, expected)
                    || !review_owned_projection_matches(&actual, expected)
                    || !batch_transcription_projection_matches(&actual, expected)
                {
                    return Err(AppError::Validation(format!(
                        "Cannot apply stale batch transcription history for segment {}",
                        expected.id
                    )));
                }
                self.restore_batch_transcription_snapshot(desired)?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.release_savepoint("history_batch_transcription")?;
                self.track_write()?;
                Ok(())
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("history_batch_transcription");
                Err(error)
            }
        }
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
