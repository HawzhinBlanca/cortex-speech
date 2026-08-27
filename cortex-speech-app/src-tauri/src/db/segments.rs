use super::*;

impl Database {
    /// Generic ASR/source insert-upsert.
    ///
    /// At schema 60 this boundary accepts only a neutral review projection and its SQL never names a
    /// review-owned column.  Human truth must be created by an atomic effect writer.  The historical
    /// pre-v60 shape remains solely for migration compatibility.
    pub fn insert_segment(&self, seg: &SpeechSegment) -> AppResult<()> {
        validate_segment(seg)?;
        if crate::migrations::get_current_version(self)? >= 60 {
            let review_fields = imported_review_owned_fields(seg);
            if !review_fields.is_empty() {
                return Err(AppError::Validation(format!(
                    "generic segment insert/upsert cannot author review-owned field(s) {} at schema v60",
                    review_fields.join(", ")
                )));
            }
            self.upsert_machine_segment_row(seg)?;
            #[cfg(test)]
            self.materialize_couch_fixture_audio_identity(seg)?;
            self.track_write()?;
            return Ok(());
        }
        let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, audio_path, raw_transcript, normalized_transcript,
                 annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score, alignment_quality,
                 model_version_id, confidence_source, cloud_call, decoder_config_hash, normalizer_version, denoised, diarized, vad_backend)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, COALESCE(?18, 'unknown@pre-registry'), COALESCE(?19, 'unknown'), ?20, ?21, ?22, ?23, ?24, ?25)
             ON CONFLICT(id) DO UPDATE SET
                audio_path=excluded.audio_path,
                raw_transcript=excluded.raw_transcript,
                normalized_transcript=excluded.normalized_transcript,
                annotated_transcript=excluded.annotated_transcript,
                alignment_json=excluded.alignment_json,
                duration_ms=excluded.duration_ms,
                speaker_id=excluded.speaker_id,
                verified=excluded.verified,
                confidence=excluded.confidence,
                ctc_score=excluded.ctc_score,
                clipping_ratio=excluded.clipping_ratio,
                rms_db=excluded.rms_db,
                snr_db=excluded.snr_db,
                split=excluded.split,
                signal_anomaly_score=excluded.signal_anomaly_score,
                alignment_quality=excluded.alignment_quality,
                model_version_id=excluded.model_version_id,
                confidence_source=excluded.confidence_source,
                cloud_call=excluded.cloud_call,
                decoder_config_hash=excluded.decoder_config_hash,
                normalizer_version=excluded.normalizer_version,
                denoised=excluded.denoised,
                diarized=excluded.diarized,
                vad_backend=excluded.vad_backend,
                updated_at=datetime('now')",
            params![
                seg.id, seg.audio_path, raw_nfc,
                normalized_nfc, annotated_nfc,
                seg.alignment_json, seg.duration_ms, seg.speaker_id,
                seg.verified as i32, seg.confidence, seg.ctc_score,
                seg.clipping_ratio, seg.rms_db, seg.snr_db, seg.split,
                seg.signal_anomaly_score, seg.alignment_quality,
                seg.model_version_id,
                seg.confidence_source,
                seg.cloud_call as i32,
                seg.decoder_config_hash,
                seg.normalizer_version,
                seg.denoised.map(|b| b as i32),
                seg.diarized.map(|b| b as i32),
                seg.vad_backend,
            ],
        )?;
        #[cfg(test)]
        self.materialize_couch_fixture_audio_identity(seg)?;
        self.track_write()?;
        Ok(())
    }

    /// Couch endpoint tests create real WAVs but bypass the importer that normally owns canonical
    /// decoded-PCM identity. Only databases carrying that suite's TEMP trigger enter this branch;
    /// production builds do not compile it, and unrelated database tests retain their exact fixtures.
    #[cfg(test)]
    pub(super) fn materialize_couch_fixture_audio_identity(&self, segment: &SpeechSegment) -> AppResult<()> {
        let is_couch_fixture: i64 = self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_temp_master
                  WHERE type='trigger' AND name='fixture_audio_content_hash'
             )",
            [],
            |row| row.get(0),
        )?;
        if is_couch_fixture == 0 || !Path::new(&segment.audio_path).is_file() {
            return Ok(());
        }
        let content_hash = crate::export_bundle::current_canonical_pcm_blake3(Path::new(&segment.audio_path))?;
        self.conn.execute(
            "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1 AND audio_content_hash IS NOT ?2",
            params![segment.id, content_hash],
        )?;
        Ok(())
    }

    /// Schema-v60 machine/source upsert. Review-owned columns are absent from both INSERT and UPDATE.
    pub(super) fn upsert_machine_segment_row(&self, segment: &SpeechSegment) -> AppResult<()> {
        let raw_nfc = to_nfc(&segment.raw_transcript);
        let normalized_nfc = segment.normalized_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, created_at, audio_path, raw_transcript, normalized_transcript,
                 alignment_json, duration_ms, speaker_id, confidence, ctc_score,
                 clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                 alignment_quality, model_version_id, confidence_source, cloud_call,
                 decoder_config_hash, normalizer_version, denoised, diarized, vad_backend,
                 speaker_change_score, updated_at)
             VALUES (?1, COALESCE(?2, datetime('now')), ?3, ?4, ?5, ?6, ?7, ?8,
                 ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 COALESCE(?17, 'unknown@pre-registry'), COALESCE(?18, 'unknown'),
                 ?19, ?20, ?21, ?22, ?23, ?24, ?25, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                 audio_path=excluded.audio_path,
                 raw_transcript=excluded.raw_transcript,
                 normalized_transcript=excluded.normalized_transcript,
                 alignment_json=excluded.alignment_json,
                 duration_ms=excluded.duration_ms,
                 speaker_id=excluded.speaker_id,
                 confidence=excluded.confidence,
                 ctc_score=excluded.ctc_score,
                 clipping_ratio=excluded.clipping_ratio,
                 rms_db=excluded.rms_db,
                 snr_db=excluded.snr_db,
                 split=excluded.split,
                 signal_anomaly_score=excluded.signal_anomaly_score,
                 alignment_quality=excluded.alignment_quality,
                 model_version_id=excluded.model_version_id,
                 confidence_source=excluded.confidence_source,
                 cloud_call=excluded.cloud_call,
                 decoder_config_hash=excluded.decoder_config_hash,
                 normalizer_version=excluded.normalizer_version,
                 denoised=excluded.denoised,
                 diarized=excluded.diarized,
                 vad_backend=excluded.vad_backend,
                 speaker_change_score=COALESCE(excluded.speaker_change_score, speech_segments.speaker_change_score),
                 updated_at=datetime('now')",
            params![
                segment.id,
                segment.created_at,
                segment.audio_path,
                raw_nfc,
                normalized_nfc,
                segment.alignment_json,
                segment.duration_ms,
                segment.speaker_id,
                segment.confidence,
                segment.ctc_score,
                segment.clipping_ratio,
                segment.rms_db,
                segment.snr_db,
                segment.split,
                segment.signal_anomaly_score,
                segment.alignment_quality,
                segment.model_version_id,
                segment.confidence_source,
                segment.cloud_call as i32,
                segment.decoder_config_hash,
                segment.normalizer_version,
                segment.denoised.map(|value| value as i32),
                segment.diarized.map(|value| value as i32),
                segment.vad_backend,
                segment.speaker_change_score,
            ],
        )?;
        Ok(())
    }

    /// Schema-v60 generic/editor insert boundary. New rows may carry machine/source metadata only;
    /// human truth must originate in the atomic review-effect writers.
    pub(crate) fn insert_machine_segment_snapshot(&self, segment: &SpeechSegment) -> AppResult<()> {
        validate_segment(segment)?;
        let review_fields = imported_review_owned_fields(segment);
        if !review_fields.is_empty() {
            return Err(AppError::Validation(format!(
                "generic segment insert cannot author review-owned field(s) {} at schema v60",
                review_fields.join(", ")
            )));
        }
        let raw_nfc = to_nfc(&segment.raw_transcript);
        let normalized_nfc = segment.normalized_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, created_at, audio_path, raw_transcript, normalized_transcript,
                 alignment_json, duration_ms, speaker_id, confidence, ctc_score,
                 clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                 alignment_quality, model_version_id, confidence_source, cloud_call,
                 decoder_config_hash, normalizer_version, denoised, diarized, vad_backend,
                 speaker_change_score, updated_at)
             VALUES (?1, COALESCE(?2, datetime('now')), ?3, ?4, ?5, ?6, ?7, ?8,
                 ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 COALESCE(?17, 'unknown@pre-registry'), COALESCE(?18, 'unknown'),
                 ?19, ?20, ?21, ?22, ?23, ?24, ?25, datetime('now'))",
            params![
                segment.id,
                segment.created_at,
                segment.audio_path,
                raw_nfc,
                normalized_nfc,
                segment.alignment_json,
                segment.duration_ms,
                segment.speaker_id,
                segment.confidence,
                segment.ctc_score,
                segment.clipping_ratio,
                segment.rms_db,
                segment.snr_db,
                segment.split,
                segment.signal_anomaly_score,
                segment.alignment_quality,
                segment.model_version_id,
                segment.confidence_source,
                segment.cloud_call as i32,
                segment.decoder_config_hash,
                segment.normalizer_version,
                segment.denoised.map(|value| value as i32),
                segment.diarized.map(|value| value as i32),
                segment.vad_backend,
                segment.speaker_change_score,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Persist a generic v60 whole-row editor request through an explicit machine/source allowlist.
    /// Review-owned columns are compared but never named in the UPDATE.
    pub(crate) fn persist_machine_segment_snapshot(
        &self,
        expected: &SpeechSegment,
        desired: &SpeechSegment,
    ) -> AppResult<()> {
        validate_segment(desired)?;
        if expected.id != desired.id {
            return Err(AppError::Validation("generic segment update endpoints must identify the same segment".into()));
        }
        if !review_owned_projection_matches(expected, desired) {
            return Err(AppError::Validation(format!(
                "generic segment update for {} attempted to mutate review-owned truth",
                expected.id
            )));
        }
        let current = self.get_segment_by_id(&expected.id)?.ok_or_else(|| {
            AppError::Validation(format!("Cannot update segment {}: it no longer exists", expected.id))
        })?;
        if !review_owned_projection_matches(&current, expected)
            || !history_source_identity_matches(&current, expected)
            || !history_machine_projection_matches(&current, expected)
        {
            return Err(AppError::Validation(format!(
                "Cannot apply stale generic update for segment {}: its stored state changed",
                expected.id
            )));
        }

        let raw_nfc = to_nfc(&desired.raw_transcript);
        let normalized_nfc = desired.normalized_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "UPDATE speech_segments SET
                audio_path=?2, raw_transcript=?3, normalized_transcript=?4,
                alignment_json=?5, duration_ms=?6, speaker_id=?7,
                confidence=?8, ctc_score=?9, clipping_ratio=?10, rms_db=?11, snr_db=?12,
                split=?13, signal_anomaly_score=?14, alignment_quality=?15,
                model_version_id=COALESCE(?16, 'unknown@pre-registry'),
                confidence_source=COALESCE(?17, 'unknown'), cloud_call=?18,
                decoder_config_hash=?19, normalizer_version=?20, denoised=?21,
                diarized=?22, vad_backend=?23, speaker_change_score=?24,
                updated_at=datetime('now')
             WHERE id=?1",
            params![
                desired.id,
                desired.audio_path,
                raw_nfc,
                normalized_nfc,
                desired.alignment_json,
                desired.duration_ms,
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

    /// Undo the fields owned by batch transcription. The command intentionally carries only the
    /// pre-batch row, so deterministic redo is unsupported; its inverse is correspondingly limited
    /// to the exact ASR columns the batch writer owns. Review/source fields are never named.
    pub(crate) fn restore_batch_transcription_snapshot(&self, previous: &SpeechSegment) -> AppResult<()> {
        if crate::migrations::get_current_version(self)? < 60 {
            return self.insert_segment(previous);
        }
        let Some(current) = self.get_segment_by_id(&previous.id)? else {
            return Ok(());
        };
        if !history_source_identity_matches(&current, previous) {
            return Err(AppError::Validation(format!(
                "Cannot undo batch transcription for segment {}: its protected source identity changed",
                previous.id
            )));
        }
        // A human who decided AFTER the batch now owns this row's text. The undo names only ASR
        // columns, so it would put the pre-batch machine draft back UNDERNEATH a live human verdict —
        // the reviewer's accept/edit would then stand against text they never saw, and the couch's
        // served/approved key would no longer match the row. Refuse instead: the batch it is undoing
        // was itself superseded by a decision, so there is nothing safe left to restore.
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

    /// Legacy lossless full-row insertion. Schema 60 turns this generic boundary into a machine/source
    /// upsert: it rejects any review-owned projection and never names review columns. Reviewed deletion
    /// is itself forbidden, so an effect-backed row can never need whole-row resurrection.
    pub fn insert_segment_full(&self, seg: &SpeechSegment) -> AppResult<()> {
        if crate::migrations::get_current_version(self)? >= 60 {
            validate_segment(seg)?;
            let review_fields = imported_review_owned_fields(seg);
            if !review_fields.is_empty() {
                return Err(AppError::Validation(format!(
                    "generic full segment insert/upsert cannot author review-owned field(s) {} at schema v60",
                    review_fields.join(", ")
                )));
            }
            self.upsert_machine_segment_row(seg)?;
            self.track_write()?;
            return Ok(());
        }
        self.insert_segment_full_unchecked(seg)
    }

    pub(super) fn insert_segment_full_unchecked(&self, seg: &SpeechSegment) -> AppResult<()> {
        validate_segment(seg)?;
        let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
        // NFC-normalize the jury verdict transcript too, so a restored row stays byte-consistent with
        // the rest of the (already NFC) transcript columns.
        let verdict_transcript_nfc = seg.verdict_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, created_at, audio_path, raw_transcript, normalized_transcript,
                 annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence,
                 ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                 verdict, verdict_transcript, rationale, evidence_json, agreement_score, escalated,
                 human_decision, corrected_at, is_gold, alignment_quality, model_version_id,
                 confidence_source, cloud_call, decoder_config_hash, normalizer_version, denoised, diarized, vad_backend,
                 reviewed_by, speaker_change_score, updated_at)
             VALUES (?1, COALESCE(?2, datetime('now')), ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                 COALESCE(?28, 'unknown@pre-registry'), COALESCE(?29, 'unknown'), ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                created_at=excluded.created_at,
                audio_path=excluded.audio_path,
                raw_transcript=excluded.raw_transcript,
                normalized_transcript=excluded.normalized_transcript,
                annotated_transcript=excluded.annotated_transcript,
                alignment_json=excluded.alignment_json,
                duration_ms=excluded.duration_ms,
                speaker_id=excluded.speaker_id,
                verified=excluded.verified,
                confidence=excluded.confidence,
                ctc_score=excluded.ctc_score,
                clipping_ratio=excluded.clipping_ratio,
                rms_db=excluded.rms_db,
                snr_db=excluded.snr_db,
                split=excluded.split,
                signal_anomaly_score=excluded.signal_anomaly_score,
                verdict=excluded.verdict,
                verdict_transcript=excluded.verdict_transcript,
                rationale=excluded.rationale,
                evidence_json=excluded.evidence_json,
                agreement_score=excluded.agreement_score,
                escalated=excluded.escalated,
                human_decision=excluded.human_decision,
                corrected_at=excluded.corrected_at,
                is_gold=excluded.is_gold,
                alignment_quality=excluded.alignment_quality,
                model_version_id=excluded.model_version_id,
                confidence_source=excluded.confidence_source,
                cloud_call=excluded.cloud_call,
                decoder_config_hash=excluded.decoder_config_hash,
                normalizer_version=excluded.normalizer_version,
                denoised=excluded.denoised,
                diarized=excluded.diarized,
                vad_backend=excluded.vad_backend,
                reviewed_by=excluded.reviewed_by,
                speaker_change_score=excluded.speaker_change_score,
                updated_at=datetime('now')",
            params![
                seg.id,
                seg.created_at,
                seg.audio_path,
                raw_nfc,
                normalized_nfc,
                annotated_nfc,
                seg.alignment_json,
                seg.duration_ms,
                seg.speaker_id,
                seg.verified as i32,
                seg.confidence,
                seg.ctc_score,
                seg.clipping_ratio,
                seg.rms_db,
                seg.snr_db,
                seg.split,
                seg.signal_anomaly_score,
                seg.verdict,
                verdict_transcript_nfc,
                seg.rationale,
                seg.evidence_json,
                seg.agreement_score,
                seg.escalated as i32,
                seg.human_decision,
                seg.corrected_at,
                seg.is_gold as i32,
                seg.alignment_quality,
                seg.model_version_id,
                seg.confidence_source,
                seg.cloud_call as i32,
                seg.decoder_config_hash,
                seg.normalizer_version,
                seg.denoised.map(|b| b as i32),
                seg.diarized.map(|b| b as i32),
                seg.vad_backend,
                seg.reviewed_by,
                // Restoring a deleted clip must bring its measurement back with it. A fresh INSERT
                // takes the schema default for anything omitted, so leaving this out would silently
                // un-flag a two-speaker clip the moment it was deleted and undone.
                seg.speaker_change_score,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Insert an exact historical row for a unit-test fixture.
    ///
    /// Schema v60 deliberately forbids generic production writers from authoring review-owned
    /// columns. A number of export, migration, and statistics tests still need to construct legacy
    /// rows so their read-side behaviour can be verified. Keeping that capability behind
    /// `cfg(test)` preserves those fixtures without weakening the production boundary or teaching
    /// tests to disable triggers with ad-hoc SQL.
    #[cfg(test)]
    pub(crate) fn insert_legacy_segment_fixture(&self, seg: &SpeechSegment) -> AppResult<()> {
        self.insert_segment_full_unchecked(seg)
    }

    /// Legacy batch verification cannot create schema-v60 human truth: every production review must
    /// have an immutable decision effect (or belong to the frozen pre-v60 authority snapshot).
    pub fn update_verified(&self, _id: &str, _verified: bool) -> AppResult<bool> {
        Err(AppError::Validation(
            "legacy batch verify/unverify is disabled; use the review decision flow so human truth has an immutable effect"
                .into(),
        ))
    }

    /// Test-only fixture writer for legacy/evaluation scenarios that intentionally model an unbound
    /// verified bit. Production code must never call this schema-v60 bypass.
    #[cfg(test)]
    pub fn update_verified_for_test(&self, id: &str, verified: bool) -> AppResult<bool> {
        let rows = self.conn.execute(
            "UPDATE speech_segments SET verified = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, verified as i32],
        )?;
        Ok(rows > 0)
    }

    /// Targeted single-column update: sets `speaker_id` without touching any other field.
    /// Pass `None` to clear the speaker assignment.
    /// Returns true if the row was found and updated.
    pub fn update_speaker_id(&self, id: &str, speaker_id: Option<&str>) -> AppResult<bool> {
        let rows = self.conn.execute(
            "UPDATE speech_segments SET speaker_id = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, speaker_id],
        )?;
        Ok(rows > 0)
    }

    /// Targeted single-column update: sets `normalized_transcript` (the normalized ASR draft) without
    /// touching the human's answer (annotated_transcript / verdict) or any other field. Returns true
    /// if the row was found and updated. Used by batch_normalize instead of a read-modify-write +
    /// whole-row insert_segment upsert, which could clobber a concurrent write between the re-read and
    /// the write (the anti-clobber discipline the sibling batch updates already follow).
    pub fn update_normalized_transcript(&self, id: &str, normalized: &str) -> AppResult<bool> {
        let rows = self.conn.execute(
            "UPDATE speech_segments SET normalized_transcript = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, normalized],
        )?;
        Ok(rows > 0)
    }

    pub fn insert_segments_batch(&self, segments: &[SpeechSegment]) -> AppResult<()> {
        let schema_v60 = crate::migrations::get_current_version(self)? >= 60;
        if schema_v60 {
            // Validate the complete batch before the savepoint or first INSERT. One forged row must
            // reject the payload without partially importing the preceding neutral rows.
            for segment in segments {
                validate_segment(segment)?;
                let review_fields = imported_review_owned_fields(segment);
                if !review_fields.is_empty() {
                    return Err(AppError::Validation(format!(
                        "generic segment batch cannot author review-owned field(s) {} at schema v60",
                        review_fields.join(", ")
                    )));
                }
            }
        }
        // Use a SAVEPOINT on the shared connection — avoids opening a second
        // file handle that could race with other writers under WAL mode.
        self.conn.execute("SAVEPOINT batch_insert", [])?;
        let result: AppResult<()> = (|| {
            let mut legacy_stmt = (!schema_v60)
                .then(|| {
                    self.conn.prepare(
                "INSERT INTO speech_segments
                    (id, audio_path, raw_transcript, normalized_transcript,
                     annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                     model_version_id, confidence_source, cloud_call, decoder_config_hash, normalizer_version, denoised, diarized, vad_backend)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, COALESCE(?17, 'unknown@pre-registry'), COALESCE(?18, 'unknown'), ?19, ?20, ?21, ?22, ?23, ?24)
                 ON CONFLICT(id) DO UPDATE SET
                    audio_path=excluded.audio_path,
                    raw_transcript=excluded.raw_transcript,
                    normalized_transcript=excluded.normalized_transcript,
                    annotated_transcript=excluded.annotated_transcript,
                    alignment_json=excluded.alignment_json,
                    duration_ms=excluded.duration_ms,
                    speaker_id=excluded.speaker_id,
                    verified=excluded.verified,
                    confidence=excluded.confidence,
                    ctc_score=excluded.ctc_score,
                    clipping_ratio=excluded.clipping_ratio,
                    rms_db=excluded.rms_db,
                    snr_db=excluded.snr_db,
                    split=excluded.split,
                    signal_anomaly_score=excluded.signal_anomaly_score,
                    model_version_id=excluded.model_version_id,
                    confidence_source=excluded.confidence_source,
                    cloud_call=excluded.cloud_call,
                    decoder_config_hash=excluded.decoder_config_hash,
                    normalizer_version=excluded.normalizer_version,
                    denoised=excluded.denoised,
                    diarized=excluded.diarized,
                    vad_backend=excluded.vad_backend,
                     updated_at=datetime('now')",
                    )
                })
                .transpose()?;
            let mut machine_stmt = schema_v60
                .then(|| {
                    self.conn.prepare(
                        "INSERT INTO speech_segments
                            (id, created_at, audio_path, raw_transcript, normalized_transcript,
                             alignment_json, duration_ms, speaker_id, confidence, ctc_score,
                             clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                             alignment_quality, model_version_id, confidence_source, cloud_call,
                             decoder_config_hash, normalizer_version, denoised, diarized, vad_backend,
                             speaker_change_score, updated_at)
                         VALUES (?1, COALESCE(?2, datetime('now')), ?3, ?4, ?5, ?6, ?7, ?8,
                             ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                             COALESCE(?17, 'unknown@pre-registry'), COALESCE(?18, 'unknown'),
                             ?19, ?20, ?21, ?22, ?23, ?24, ?25, datetime('now'))
                         ON CONFLICT(id) DO UPDATE SET
                             audio_path=excluded.audio_path,
                             raw_transcript=excluded.raw_transcript,
                             normalized_transcript=excluded.normalized_transcript,
                             alignment_json=excluded.alignment_json,
                             duration_ms=excluded.duration_ms,
                             speaker_id=excluded.speaker_id,
                             confidence=excluded.confidence,
                             ctc_score=excluded.ctc_score,
                             clipping_ratio=excluded.clipping_ratio,
                             rms_db=excluded.rms_db,
                             snr_db=excluded.snr_db,
                             split=excluded.split,
                             signal_anomaly_score=excluded.signal_anomaly_score,
                             alignment_quality=excluded.alignment_quality,
                             model_version_id=excluded.model_version_id,
                             confidence_source=excluded.confidence_source,
                             cloud_call=excluded.cloud_call,
                             decoder_config_hash=excluded.decoder_config_hash,
                             normalizer_version=excluded.normalizer_version,
                             denoised=excluded.denoised,
                             diarized=excluded.diarized,
                             vad_backend=excluded.vad_backend,
                             speaker_change_score=COALESCE(
                                 excluded.speaker_change_score,
                                 speech_segments.speaker_change_score
                             ),
                             updated_at=datetime('now')",
                    )
                })
                .transpose()?;
            for seg in segments {
                validate_segment(seg)?;
                let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
                if let Some(stmt) = machine_stmt.as_mut() {
                    stmt.execute(params![
                        seg.id,
                        seg.created_at,
                        seg.audio_path,
                        raw_nfc,
                        normalized_nfc,
                        seg.alignment_json,
                        seg.duration_ms,
                        seg.speaker_id,
                        seg.confidence,
                        seg.ctc_score,
                        seg.clipping_ratio,
                        seg.rms_db,
                        seg.snr_db,
                        seg.split,
                        seg.signal_anomaly_score,
                        seg.alignment_quality,
                        seg.model_version_id,
                        seg.confidence_source,
                        seg.cloud_call as i32,
                        seg.decoder_config_hash,
                        seg.normalizer_version,
                        seg.denoised.map(|b| b as i32),
                        seg.diarized.map(|b| b as i32),
                        seg.vad_backend,
                        seg.speaker_change_score,
                    ])?;
                } else if let Some(stmt) = legacy_stmt.as_mut() {
                    stmt.execute(params![
                        seg.id,
                        seg.audio_path,
                        raw_nfc,
                        normalized_nfc,
                        annotated_nfc,
                        seg.alignment_json,
                        seg.duration_ms,
                        seg.speaker_id,
                        seg.verified as i32,
                        seg.confidence,
                        seg.ctc_score,
                        seg.clipping_ratio,
                        seg.rms_db,
                        seg.snr_db,
                        seg.split,
                        seg.signal_anomaly_score,
                        seg.model_version_id,
                        seg.confidence_source,
                        seg.cloud_call as i32,
                        seg.decoder_config_hash,
                        seg.normalizer_version,
                        seg.denoised.map(|b| b as i32),
                        seg.diarized.map(|b| b as i32),
                        seg.vad_backend,
                    ])?;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("batch_insert")?;
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("batch_insert");
                Err(e)
            }
        }
    }

    /// Publish one non-champion import file and bind recording identity only to the rows from this
    /// source operation. Older rows that happen to spell the same path are never rewritten.
    pub(crate) fn insert_segments_with_audio_identity_batch(
        &self,
        segments: &[SpeechSegment],
        identity: &AudioIdentity,
    ) -> AppResult<()> {
        if segments.is_empty() {
            return Err(AppError::Validation("No import segments to publish".into()));
        }
        if identity.spectral == 0 {
            return Err(AppError::Validation(
                "Import recording identity has an unusable zero spectral fingerprint".into(),
            ));
        }
        let audio_path = segments[0].audio_path.as_str();
        if segments.iter().any(|segment| segment.audio_path != audio_path) {
            return Err(AppError::Validation(
                "One import identity publication may contain segments from only one source file".into(),
            ));
        }
        let segment_ids: Vec<String> = segments.iter().map(|segment| segment.id.clone()).collect();

        self.conn.execute("SAVEPOINT import_identity_publish", [])?;
        let result = (|| -> AppResult<()> {
            // The insert obtains SQLite's writer lock before the compatibility check. No independent
            // importer can change the source identity between that check and the scoped update.
            self.insert_segments_batch(segments)?;
            self.set_audio_identity_for_segments(audio_path, &segment_ids, identity)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("import_identity_publish")?;
                self.track_write()?;
                Ok(())
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("import_identity_publish");
                Err(error)
            }
        }
    }

    /// Publish one fully champion-drafted import file as a single durable unit.
    ///
    /// Import inference happens before this boundary. Consequently no placeholder or partially
    /// drafted segment may enter the canonical table, and the segment rows, their sole champion
    /// hypotheses, and the recording identity either all commit or all roll back together.
    pub(crate) fn insert_champion_segments_batch(
        &self,
        segments: &[SpeechSegment],
        deployment_sha256: &str,
        identity: Option<&AudioIdentity>,
    ) -> AppResult<()> {
        if segments.is_empty() {
            return Err(AppError::Validation("No champion-drafted segments to publish".into()));
        }
        if deployment_sha256.len() != 64
            || !deployment_sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AppError::Validation(
                "Champion deployment identity must be a canonical lowercase SHA-256".into(),
            ));
        }

        let audio_path = segments[0].audio_path.as_str();
        for segment in segments {
            if segment.audio_path != audio_path {
                return Err(AppError::Validation(
                    "One champion import publication may contain segments from only one source file".into(),
                ));
            }
            if segment.raw_transcript.trim().is_empty()
                || crate::quality::is_placeholder_transcript(&segment.raw_transcript)
            {
                return Err(AppError::Validation(format!(
                    "Champion import segment '{}' has no usable finalized transcript",
                    segment.id
                )));
            }
            let model_id = segment.model_version_id.as_deref().ok_or_else(|| {
                AppError::Validation(format!("Champion import segment '{}' is missing its model identity", segment.id))
            })?;
            let identity_is_current: bool = self.conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM model_versions
                    WHERE id = ?1 AND family = 'omniasr-7b' AND status = 'champion'
                      AND checkpoint_sha256 = ?2
                )",
                params![model_id, deployment_sha256],
                |row| row.get(0),
            )?;
            if !identity_is_current {
                return Err(AppError::Validation(format!(
                    "MODEL_IDENTITY_CHANGED: refusing import transcript from model '{model_id}' deployment '{deployment_sha256}' because it is not the current registry champion"
                )));
            }
        }

        self.conn.execute("SAVEPOINT champion_import_publish", [])?;
        let result = (|| -> AppResult<()> {
            self.insert_segments_batch(segments)?;
            let mut delete = self.conn.prepare("DELETE FROM segment_hypotheses WHERE segment_id = ?1")?;
            let mut insert = self.conn.prepare(
                "INSERT INTO segment_hypotheses
                    (segment_id, model_id, transcript, confidence, model_version_id)
                 VALUES (?1, ?2, ?3, ?4, ?2)",
            )?;
            for segment in segments {
                let model_id = segment.model_version_id.as_deref().ok_or_else(|| {
                    AppError::Validation(format!(
                        "Champion import segment '{}' lost its model identity before publication",
                        segment.id
                    ))
                })?;
                delete.execute([segment.id.as_str()])?;
                insert.execute(params![segment.id, model_id, to_nfc(&segment.raw_transcript), segment.confidence])?;
            }
            drop(insert);
            drop(delete);
            if let Some(identity) = identity.filter(|identity| identity.spectral != 0) {
                let segment_ids: Vec<String> = segments.iter().map(|segment| segment.id.clone()).collect();
                self.set_audio_identity_for_segments(audio_path, &segment_ids, identity)?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.release_savepoint("champion_import_publish")?;
                self.track_write()?;
                Ok(())
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("champion_import_publish");
                Err(error)
            }
        }
    }

    pub fn merge_dataset_json(&self, json_content: &str) -> AppResult<(usize, usize)> {
        let external_segments: Vec<SpeechSegment> = serde_json::from_str(json_content)?;
        let schema_v60 = crate::migrations::get_current_version(self)? >= 60;

        // Validate the entire renderer-owned payload before opening the savepoint or touching even one
        // row. At v60+, pasted JSON may carry machine/source metadata only; review truth must be born
        // through the server-owned effect finalizers. Pre-v60 keeps its historical lossless-import
        // behavior solely for migration compatibility.
        if schema_v60 {
            for segment in &external_segments {
                let fields = imported_review_owned_fields(segment);
                if !fields.is_empty() {
                    return Err(AppError::Validation(format!(
                        "Dataset merge refused atomically: segment '{}' supplies review-owned field(s) {}; use the review decision/flag flow so human truth has immutable authority",
                        segment.id,
                        fields.join(", ")
                    )));
                }
            }
        }
        let mut updated = 0;
        let mut created = 0;

        self.conn.execute("SAVEPOINT merge_json", [])?;
        let result: AppResult<()> = (|| {
            let mut check_stmt = self.conn.prepare("SELECT id FROM speech_segments WHERE id = ?1")?;
            let update_sql = if schema_v60 {
                // Do not even name review-owned columns in the SET list. The complete guard also keeps
                // machine transcript/metadata replacement away from every current or frozen reviewed
                // row; pasted data must never mutate the baseline of an existing review chain.
                "UPDATE speech_segments SET
                    audio_path=?2, raw_transcript=?3, normalized_transcript=?4,
                    alignment_json=?5, duration_ms=?6, speaker_id=?7,
                    confidence=?8, ctc_score=?9, clipping_ratio=?10, rms_db=?11,
                    snr_db=?12, split=?13, signal_anomaly_score=?14,
                    model_version_id=COALESCE(?15, 'unknown@pre-registry'),
                    confidence_source=COALESCE(?16, 'unknown'), cloud_call=?17,
                    decoder_config_hash=?18, normalizer_version=?19,
                    updated_at=datetime('now')
                 WHERE id=?1
                   AND verified = 0 AND escalated = 0 AND is_gold = 0
                   AND annotated_transcript IS NULL
                   AND human_decision IS NULL AND verdict IS NULL
                   AND verdict_transcript IS NULL AND rationale IS NULL
                   AND evidence_json IS NULL AND agreement_score IS NULL
                   AND corrected_at IS NULL AND reviewed_by IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM legacy_reviewed_segments_v60 legacy
                       WHERE legacy.id = speech_segments.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM human_decision_effect_events effect
                       WHERE effect.segment_id = speech_segments.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM review_flag_effect_events flag
                       WHERE flag.segment_id = speech_segments.id
                   )"
            } else {
                "UPDATE speech_segments SET
                    audio_path=?2, raw_transcript=?3, normalized_transcript=?4,
                    annotated_transcript=?5, alignment_json=?6, duration_ms=?7,
                    speaker_id=?8, verified=?9, confidence=?10, ctc_score=?11,
                    clipping_ratio=?12, rms_db=?13, snr_db=?14, split=?15, signal_anomaly_score=?16,
                    model_version_id=COALESCE(?17, 'unknown@pre-registry'),
                    confidence_source=COALESCE(?18, 'unknown'), cloud_call=?19,
                    decoder_config_hash=?20, normalizer_version=?21,
                    updated_at=datetime('now')
                 WHERE id=?1
                   AND verified = 0
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))"
            };
            let mut update_stmt = self.conn.prepare(update_sql)?;

            let mut insert_machine_stmt = if schema_v60 {
                Some(self.conn.prepare(
                    "INSERT INTO speech_segments
                        (id, created_at, audio_path, raw_transcript, normalized_transcript,
                         alignment_json, duration_ms, speaker_id, confidence, ctc_score,
                         clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                         alignment_quality, model_version_id, confidence_source, cloud_call,
                         decoder_config_hash, normalizer_version, denoised, diarized, vad_backend,
                         speaker_change_score, updated_at)
                     VALUES (?1, COALESCE(?2, datetime('now')), ?3, ?4, ?5, ?6, ?7, ?8,
                         ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         COALESCE(?17, 'unknown@pre-registry'), COALESCE(?18, 'unknown'),
                         ?19, ?20, ?21, ?22, ?23, ?24, ?25, datetime('now'))",
                )?)
            } else {
                None
            };

            for seg in &external_segments {
                validate_segment(seg)?;
                let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
                let exists = check_stmt.exists(params![seg.id])?;
                if exists {
                    // Count only rows the guard actually changed — a human-reviewed row matches 0
                    // rows here (the UPDATE skips it), so it must not be reported as "updated".
                    let changed = if schema_v60 {
                        update_stmt.execute(params![
                            seg.id,
                            seg.audio_path,
                            raw_nfc,
                            normalized_nfc,
                            seg.alignment_json,
                            seg.duration_ms,
                            seg.speaker_id,
                            seg.confidence,
                            seg.ctc_score,
                            seg.clipping_ratio,
                            seg.rms_db,
                            seg.snr_db,
                            seg.split,
                            seg.signal_anomaly_score,
                            seg.model_version_id,
                            seg.confidence_source,
                            seg.cloud_call as i32,
                            seg.decoder_config_hash,
                            seg.normalizer_version,
                        ])?
                    } else {
                        update_stmt.execute(params![
                            seg.id,
                            seg.audio_path,
                            raw_nfc,
                            normalized_nfc,
                            annotated_nfc,
                            seg.alignment_json,
                            seg.duration_ms,
                            seg.speaker_id,
                            seg.verified as i32,
                            seg.confidence,
                            seg.ctc_score,
                            seg.clipping_ratio,
                            seg.rms_db,
                            seg.snr_db,
                            seg.split,
                            seg.signal_anomaly_score,
                            seg.model_version_id,
                            seg.confidence_source,
                            seg.cloud_call as i32,
                            seg.decoder_config_hash,
                            seg.normalizer_version,
                        ])?
                    };
                    if changed > 0 {
                        updated += 1;
                    }
                } else {
                    if let Some(stmt) = insert_machine_stmt.as_mut() {
                        // Schema-v60 imports create machine/source rows only. Review fields take their
                        // schema defaults and cannot be supplied by renderer JSON.
                        stmt.execute(params![
                            seg.id,
                            seg.created_at,
                            seg.audio_path,
                            raw_nfc,
                            normalized_nfc,
                            seg.alignment_json,
                            seg.duration_ms,
                            seg.speaker_id,
                            seg.confidence,
                            seg.ctc_score,
                            seg.clipping_ratio,
                            seg.rms_db,
                            seg.snr_db,
                            seg.split,
                            seg.signal_anomaly_score,
                            seg.alignment_quality,
                            seg.model_version_id,
                            seg.confidence_source,
                            seg.cloud_call as i32,
                            seg.decoder_config_hash,
                            seg.normalizer_version,
                            seg.denoised.map(|value| value as i32),
                            seg.diarized.map(|value| value as i32),
                            seg.vad_backend,
                            seg.speaker_change_score,
                        ])?;
                    } else {
                        // Historical pre-v60 compatibility: before atomic effect authority existed,
                        // dataset merge was the supported lossless reviewed-row import.
                        self.insert_segment_full(seg)?;
                    }
                    created += 1;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("merge_json")?;
                self.track_write()?;
                Ok((created, updated))
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("merge_json");
                Err(e)
            }
        }
    }

    /// Safely update the ASR transcript for a segment only if a human has NOT
    /// already reviewed it. This is the correct API for the WSL 7B branch to
    /// persist refined transcripts without overwriting user edits.
    #[allow(clippy::too_many_arguments)]
    pub fn update_asr_transcript_if_unreviewed(
        &self,
        segment_id: &str,
        raw_transcript: &str,
        normalized_transcript: Option<&str>,
        confidence: Option<f64>,
        confidence_source: Option<&str>,
        model_version_id: Option<&str>,
        cloud_call: bool,
    ) -> AppResult<bool> {
        refuse_blank_asr_persist(segment_id, raw_transcript)?;
        // NFC-canonicalize before writing the FTS-indexed columns, exactly like insert_segment /
        // update_segment. The WSL 7B branch feeds raw ASR output here, which can arrive decomposed;
        // storing a non-NFC form fragments the search index so the text can't be found.
        let raw_nfc = to_nfc(raw_transcript);
        let normalized_nfc = normalized_transcript.map(to_nfc);
        let rows_changed = self.conn.execute(
            "UPDATE speech_segments
             SET raw_transcript        = ?2,
                 normalized_transcript = ?3,
                 confidence            = ?4,
                 confidence_source     = COALESCE(?5, 'unknown'),
                 model_version_id      = COALESCE(?6, 'unknown@pre-registry'),
                 cloud_call            = ?7,
                 updated_at            = datetime('now')
             WHERE id = ?1
               -- verified = 0: a human who clicked \"Verify\"/\"Verify selected\" (batch_verify ->
               -- update_verified) sets ONLY `verified`, leaving human_decision/verdict NULL. Without this
               -- clause the background WSL-7B refinement loop (which snapshots empty-transcript targets at
               -- start) would reach a segment the human re-transcribed + verified mid-run and silently
               -- overwrite its raw/normalized transcript with unapproved 7B text, while the row stays
               -- verified=1 and still exports as human-verified GOLD. Mirrors the sibling
               -- update_batch_transcription_if_unreviewed's guard for the identical race.
               AND verified = 0
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            params![
                segment_id,
                raw_nfc,
                normalized_nfc,
                confidence,
                confidence_source,
                model_version_id,
                cloud_call as i32,
            ],
        )?;
        self.track_write()?;
        Ok(rows_changed > 0)
    }

    /// Atomically commit a champion transcript and make its hypothesis the segment's sole vote.
    ///
    /// The champion runs outside SQLite, so a reviewer may verify or decide the segment while inference
    /// is in flight. The guarded update is the compare-and-swap boundary: `Ok(false)` means the row still
    /// exists but became human-owned before this commit. In that case its transcript and all existing
    /// hypotheses remain untouched. A missing segment is an error rather than being misreported as a
    /// review race.
    ///
    /// Transcript/provenance replacement and stale-hypothesis cleanup share one savepoint. If deleting
    /// or inserting the sole champion hypothesis fails, the transcript update is rolled back too.
    #[cfg(test)]
    pub fn commit_champion_transcript_if_unreviewed(
        &self,
        champion: &SegmentHypothesis,
        expected_deployment_sha256: Option<&str>,
        normalized_transcript: Option<&str>,
        confidence_source: Option<&str>,
        cloud_call: bool,
    ) -> AppResult<bool> {
        self.commit_champion_transcript_inner(
            champion,
            expected_deployment_sha256,
            normalized_transcript,
            confidence_source,
            cloud_call,
            None,
        )
    }

    /// Commit a champion result only while the exact source/revision selected before inference is
    /// still authoritative. Production re-transcription callers must keep the matching decoded-PCM
    /// source lease alive until this method returns.
    pub(crate) fn commit_bound_champion_transcript_if_unreviewed(
        &self,
        champion: &SegmentHypothesis,
        expected_deployment_sha256: Option<&str>,
        normalized_transcript: Option<&str>,
        confidence_source: Option<&str>,
        cloud_call: bool,
        expected_source: &ChampionTranscriptionSourceSnapshot,
    ) -> AppResult<bool> {
        self.commit_champion_transcript_inner(
            champion,
            expected_deployment_sha256,
            normalized_transcript,
            confidence_source,
            cloud_call,
            Some(expected_source),
        )
    }

    pub(super) fn commit_champion_transcript_inner(
        &self,
        champion: &SegmentHypothesis,
        expected_deployment_sha256: Option<&str>,
        normalized_transcript: Option<&str>,
        confidence_source: Option<&str>,
        cloud_call: bool,
        expected_source: Option<&ChampionTranscriptionSourceSnapshot>,
    ) -> AppResult<bool> {
        crate::validation::input::validate_identifier(&champion.segment_id).map_err(AppError::Validation)?;
        crate::validation::input::validate_identifier(&champion.model_id).map_err(AppError::Validation)?;
        crate::validation::input::validate_text(&champion.transcript, 100_000, "Champion transcript")
            .map_err(AppError::Validation)?;
        if let Some(sha) = expected_deployment_sha256 {
            if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return Err(AppError::Validation(
                    "Champion deployment identity must be a canonical lowercase SHA-256".into(),
                ));
            }
        }
        if let Some(normalized) = normalized_transcript {
            crate::validation::input::validate_text(normalized, 100_000, "Normalized transcript")
                .map_err(AppError::Validation)?;
        }

        let transcript_nfc = to_nfc(&champion.transcript);
        let normalized_nfc = normalized_transcript.map(to_nfc);
        self.conn.execute("SAVEPOINT champion_commit", [])?;
        let result = (|| -> AppResult<bool> {
            if let Some(expected_sha) = expected_deployment_sha256 {
                let identity_is_current: bool = self.conn.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM model_versions
                        WHERE id = ?1 AND family = 'omniasr-7b' AND status = 'champion'
                          AND checkpoint_sha256 = ?2
                    )",
                    params![champion.model_id, expected_sha],
                    |row| row.get(0),
                )?;
                if !identity_is_current {
                    return Err(AppError::Validation(format!(
                        "MODEL_IDENTITY_CHANGED: refusing transcript from model '{}' deployment '{}' because it is not the current registry champion",
                        champion.model_id, expected_sha
                    )));
                }
            }
            let rows_changed = if let Some(source) = expected_source {
                if source.segment.id != champion.segment_id {
                    return Err(AppError::Validation(
                        "E_TRANSCRIPTION_SOURCE_MISMATCH: champion segment id does not match its bound source snapshot"
                            .into(),
                    ));
                }
                self.conn.execute(
                    "UPDATE speech_segments
                     SET raw_transcript        = ?2,
                         normalized_transcript = ?3,
                         confidence            = ?4,
                         confidence_source     = COALESCE(?5, 'unknown'),
                         model_version_id      = ?6,
                         cloud_call            = ?7,
                         updated_at            = datetime('now')
                     WHERE id = ?1
                       AND verified = 0
                       AND (human_decision IS NULL OR human_decision = '')
                       AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))
                       AND review_revision = ?8
                       AND audio_path = ?9
                       AND alignment_json IS ?10
                       AND duration_ms = ?11
                       AND audio_content_hash IS ?12",
                    params![
                        champion.segment_id,
                        transcript_nfc,
                        normalized_nfc,
                        champion.confidence,
                        confidence_source,
                        champion.model_id,
                        cloud_call as i32,
                        source.review_revision,
                        source.segment.audio_path,
                        source.segment.alignment_json,
                        source.segment.duration_ms,
                        source.audio_content_hash,
                    ],
                )?
            } else {
                self.conn.execute(
                    "UPDATE speech_segments
                     SET raw_transcript        = ?2,
                         normalized_transcript = ?3,
                         confidence            = ?4,
                         confidence_source     = COALESCE(?5, 'unknown'),
                         model_version_id      = ?6,
                         cloud_call            = ?7,
                         updated_at            = datetime('now')
                     WHERE id = ?1
                       AND verified = 0
                       AND (human_decision IS NULL OR human_decision = '')
                       AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
                    params![
                        champion.segment_id,
                        transcript_nfc,
                        normalized_nfc,
                        champion.confidence,
                        confidence_source,
                        champion.model_id,
                        cloud_call as i32,
                    ],
                )?
            };

            if rows_changed == 0 {
                let current = Self::decision_snapshot_on(&self.conn, &champion.segment_id)?;
                let Some((current_segment, current_revision, current_audio_content_hash)) = current else {
                    return Err(AppError::Validation(format!(
                        "Cannot commit champion transcript: segment '{}' does not exist",
                        champion.segment_id
                    )));
                };
                if let Some(source) = expected_source {
                    let source_unchanged = current_revision == source.review_revision
                        && current_segment.audio_path == source.segment.audio_path
                        && current_segment.alignment_json == source.segment.alignment_json
                        && current_segment.duration_ms == source.segment.duration_ms
                        && current_audio_content_hash == source.audio_content_hash;
                    if !source_unchanged {
                        return Err(AppError::Validation(format!(
                            "E_TRANSCRIPTION_SOURCE_CHANGED: segment '{}' no longer names the source/revision selected before inference; no transcript or hypothesis was written",
                            champion.segment_id
                        )));
                    }
                }
                return Ok(false);
            }

            self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id = ?1", params![champion.segment_id])?;
            self.conn.execute(
                "INSERT INTO segment_hypotheses
                    (segment_id, model_id, transcript, confidence, model_version_id)
                 VALUES (?1, ?2, ?3, ?4, ?2)",
                params![champion.segment_id, champion.model_id, transcript_nfc, champion.confidence],
            )?;
            Ok(true)
        })();

        match result {
            Ok(committed) => {
                self.release_savepoint("champion_commit")?;
                if committed {
                    self.track_write()?;
                }
                Ok(committed)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("champion_commit");
                Err(error)
            }
        }
    }

    /// Persist a batch (re)transcription result WITHOUT clobbering concurrent human work.
    ///
    /// Batch transcription runs in a background thread off a snapshot taken at batch start; a human can
    /// verify or edit a target segment while the batch is in flight. Writing the whole stale snapshot
    /// back (the old `insert_segment` path) reverted the human's `verified` flag and overwrote their
    /// edited annotation — a silent lost update. This targeted write instead:
    ///   • updates ONLY the ASR-derived columns (raw / normalized / confidence),
    ///   • NEVER touches `annotated_transcript` — that field is human-only, by law. This function
    ///     used to seed it with the machine draft via `COALESCE(annotated_transcript, seed)`, and
    ///     because every serving path ranks annotated first on presence alone, that first machine
    ///     seed outranked every later champion re-draft FOREVER: the 2026-08-12 incident, where 348
    ///     review clips served a stale machine paraphrase while fresh champion text sat invisible.
    ///     Pinned by scripts/test_machine_never_writes_annotated_policy.py.
    ///   • never touches `verified`, and
    ///   • skips any row a human has verified or reviewed since the batch began.
    /// Returns Ok(true) if the row was updated, Ok(false) if it was skipped as human-owned.
    #[allow(clippy::too_many_arguments)]
    pub fn update_batch_transcription_if_unreviewed(
        &self,
        segment_id: &str,
        raw_transcript: &str,
        normalized_transcript: Option<&str>,
        confidence: Option<f64>,
        confidence_source: Option<&str>,
        model_version_id: Option<&str>,
        cloud_call: bool,
    ) -> AppResult<bool> {
        refuse_blank_asr_persist(segment_id, raw_transcript)?;
        let raw_nfc = to_nfc(raw_transcript);
        let normalized_nfc = normalized_transcript.map(to_nfc);
        let rows_changed = self.conn.execute(
            "UPDATE speech_segments
             SET raw_transcript        = ?2,
                 normalized_transcript = ?3,
                 confidence            = ?4,
                 confidence_source     = COALESCE(?5, 'unknown'),
                 model_version_id      = COALESCE(?6, 'unknown@pre-registry'),
                 cloud_call            = ?7,
                 updated_at            = datetime('now')
             WHERE id = ?1
               AND verified = 0
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            params![
                segment_id,
                raw_nfc,
                normalized_nfc,
                confidence,
                confidence_source,
                model_version_id,
                cloud_call as i32,
            ],
        )?;
        self.track_write()?;
        Ok(rows_changed > 0)
    }

    /// Fold ONE segment's LOOP-0 shadow evidence into the durable archive BEFORE it is deleted, so the
    /// C5 over-trigger gate isn't survivor-biased by the owner's normal cleanup (review a bad clip, then
    /// delete it — exactly the rows most likely to be over-triggers). Uses the same correlation as
    /// `intelligence_report`. Must run while the segment + its shadow rows still exist (before DELETE).
    pub(super) fn archive_loop0_evidence_for(&self, id: &str) -> AppResult<()> {
        // Per-SEGMENT semantics (true-10 audit 2026-07-09): a segment re-processed N times holds N
        // shadow rows, but the C5 gate reasons about distinct events — one clip, one human decision,
        // at most one over-trigger. Fold MAX(memory_fired) per segment (this fn archives exactly one
        // segment), matching intelligence_report's DISTINCT-segment live counts.
        self.conn.execute(
            "UPDATE loop0_evidence_archive SET
                 total_observations = total_observations
                     + COALESCE((SELECT COUNT(DISTINCT segment_id) FROM loop0_shadow_log WHERE segment_id = ?1), 0),
                 would_fire = would_fire
                     + COALESCE((SELECT MAX(memory_fired) FROM loop0_shadow_log WHERE segment_id = ?1), 0),
                 fired_human_accepted = fired_human_accepted + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('accept','human_accept') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0),
                 fired_human_edited = fired_human_edited + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('edit','human_edit') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0),
                 fired_human_rejected = fired_human_rejected + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('reject','human_reject') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0)
             WHERE id = 1",
            params![id],
        )?;
        Ok(())
    }

    /// v34 twin of [`Self::archive_loop0_evidence_for`], for the C4 auto-accept-precision denominator:
    /// decision_verdicts CASCADE-deletes with its segment, so the owner's normal cleanup (review a bad
    /// clip, then delete it) removed exactly the T0_ACCEPT rows whose humans CONTRADICTED the machine —
    /// the precision gating any autonomy increase could only drift optimistic (true-10 audit
    /// 2026-07-09). Must run while the segment + its verdict row still exist (before DELETE).
    pub(super) fn archive_c4_evidence_for(&self, id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE c4_evidence_archive SET
                 t0_accepts = t0_accepts + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts WHERE segment_id = ?1 AND auto_accept_verdict = 'T0_ACCEPT'), 0),
                 t1_escalations = t1_escalations + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts WHERE segment_id = ?1 AND auto_accept_verdict = 'T1_ESCALATE'), 0),
                 t0_human_confirmed = t0_human_confirmed + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id
                     WHERE dv.segment_id = ?1 AND dv.auto_accept_verdict = 'T0_ACCEPT'
                       AND s.human_decision IN ('accept','human_accept')), 0),
                 t0_human_contradicted = t0_human_contradicted + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id
                     WHERE dv.segment_id = ?1 AND dv.auto_accept_verdict = 'T0_ACCEPT'
                       AND s.human_decision IN ('edit','human_edit','reject','human_reject')), 0)
             WHERE id = 1",
            params![id],
        )?;
        Ok(())
    }

    pub fn delete_segment(&self, id: &str) -> AppResult<()> {
        self.conn.execute("SAVEPOINT del_seg", [])?;
        let result: AppResult<()> = (|| {
            self.archive_loop0_evidence_for(id)?;
            self.archive_c4_evidence_for(id)?;
            self.conn
                .execute("DELETE FROM speech_segments WHERE id = ?1", params![id])
                .map_err(map_segment_delete_error)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("del_seg")?;
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("del_seg");
                Err(e)
            }
        }
    }

    pub fn delete_segments_batch(&self, ids: &[String]) -> AppResult<()> {
        let mut unique_ids = HashSet::with_capacity(ids.len());
        if ids.iter().any(|id| !unique_ids.insert(id.as_str())) {
            return Err(AppError::Validation(
                "batch segment deletion refuses duplicate ids before evidence archival".into(),
            ));
        }
        self.conn.execute("SAVEPOINT batch_delete", [])?;
        let result: AppResult<()> = (|| {
            // Archive each segment's shadow + C4 evidence FIRST (while its rows still exist), then delete.
            for id in ids {
                self.archive_loop0_evidence_for(id)?;
                self.archive_c4_evidence_for(id)?;
            }
            let mut stmt = self.conn.prepare("DELETE FROM speech_segments WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id]).map_err(map_segment_delete_error)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("batch_delete")?;
                // Keep the FTS5 index clean after bulk deletions.
                if let Err(error) = self.conn.execute("INSERT INTO segments_fts(segments_fts) VALUES('optimize')", []) {
                    tracing::warn!("Failed to optimize segments FTS index after batch delete: {error}");
                }
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("batch_delete");
                Err(e)
            }
        }
    }

    pub fn get_segment_by_id(&self, id: &str) -> AppResult<Option<SpeechSegment>> {
        let query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE id = ?1");
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// Read the complete row and its database-owned review revision in ONE SQLite statement.
    /// Couch Review must never pair a row snapshot from one instant with a revision fetched by a
    /// second statement after a concurrent writer has already changed it.
    pub fn get_segment_by_id_with_revision(&self, id: &str) -> AppResult<Option<(SpeechSegment, i64)>> {
        let query = format!("SELECT {SEGMENT_SELECT_COLUMNS}, review_revision FROM speech_segments WHERE id = ?1");
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some((Self::map_row(row)?, row.get(37)?)))
        } else {
            Ok(None)
        }
    }

    /// Read every identity used by a bound champion transcription in one statement. A separate
    /// segment read followed by an audio-hash read can manufacture a snapshot that never existed
    /// when another writer changes the row between those statements.
    pub(crate) fn champion_transcription_source_snapshot(
        &self,
        id: &str,
    ) -> AppResult<Option<ChampionTranscriptionSourceSnapshot>> {
        let Some((segment, review_revision, audio_content_hash)) = Self::decision_snapshot_on(&self.conn, id)? else {
            return Ok(None);
        };
        Ok(Some(ChampionTranscriptionSourceSnapshot { segment, review_revision, audio_content_hash }))
    }

    pub(super) fn decision_snapshot_on(
        conn: &Connection,
        id: &str,
    ) -> AppResult<Option<(SpeechSegment, i64, Option<String>)>> {
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS}, review_revision, audio_content_hash
               FROM speech_segments WHERE id = ?1"
        );
        let mut stmt = conn.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some((Self::map_row(row)?, row.get(37)?, row.get(38)?)))
        } else {
            Ok(None)
        }
    }

    /// One-statement source/revision snapshot used before a technical audio failure probe. The final
    /// write rechecks all three identities under `BEGIN IMMEDIATE`; callers never pair a segment row
    /// from one instant with an audio epoch from another.
    pub(crate) fn technical_unusable_source_snapshot(
        &self,
        id: &str,
    ) -> AppResult<Option<TechnicalUnusableSourceSnapshot>> {
        let Some((segment, review_revision, audio_content_hash)) = Self::decision_snapshot_on(&self.conn, id)? else {
            return Ok(None);
        };
        let source_path_sha256 = technical_unusable_source_path_sha256(Path::new(&segment.audio_path))?;
        Ok(Some(TechnicalUnusableSourceSnapshot { segment, review_revision, source_path_sha256, audio_content_hash }))
    }

    /// A review flag owns only verdict/rationale/escalation.  Before it snapshots those fields, the
    /// human-owned fields it leaves untouched must already have a durable origin: either the exact
    /// immutable pre-v60 reviewed-row snapshot, or the canonical empty/non-human baseline.  Without
    /// this check a trigger-disabled/imported row could set `verified` or an annotation first and
    /// then use a perfectly valid flag effect to make that unbound text look review-authorized.
    pub(super) fn flag_human_baseline_is_authorized_on(conn: &Connection, segment: &SpeechSegment) -> AppResult<bool> {
        let exact_legacy_human_baseline: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM legacy_reviewed_segments_v60 legacy
                   JOIN speech_segments current
                     ON current.rowid = legacy.original_rowid
                    AND current.id = legacy.id
                  WHERE current.id = ?1
                    AND current.review_revision >= legacy.review_revision
                    AND current.human_decision IS legacy.human_decision
                    AND current.verdict_transcript IS legacy.verdict_transcript
                    AND current.annotated_transcript IS legacy.annotated_transcript
                    AND current.verified IS legacy.verified
                    AND current.reviewed_by IS legacy.reviewed_by
                    AND current.corrected_at IS legacy.corrected_at
                    AND current.is_gold IS legacy.is_gold
             )",
            [&segment.id],
            |row| row.get(0),
        )?;
        if exact_legacy_human_baseline {
            return Ok(true);
        }

        Ok(!segment.verified
            && segment.annotated_transcript.is_none()
            && segment.human_decision.as_deref().map_or(true, |value| value.trim().is_empty())
            && segment.reviewed_by.as_deref().map_or(true, |value| value.trim().is_empty())
            && segment.corrected_at.as_deref().map_or(true, |value| value.trim().is_empty())
            && !segment.is_gold)
    }

    pub(super) fn load_correction_memories_on(conn: &Connection) -> AppResult<Vec<crate::corrections::MemoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT wrong_token, human_token, slot_key, phonetic_key, confidence, hit_count
               FROM effective_correction_memory_v60
              ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::corrections::MemoryEntry {
                wrong_token: row.get(0)?,
                human_token: row.get(1)?,
                slot_key: row.get(2)?,
                phonetic_key: row.get(3)?,
                confidence: row.get(4)?,
                hit_count: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Look up a segment by its `audio_path` column using the `idx_segments_audio_path` index.
    /// Used by the media registry to verify playback access without a full table scan.
    /// Returns `Ok(Some(...))` when found, `Ok(None)` when no segment matches the path.
    pub fn get_segment_by_audio_path(&self, audio_path: &str) -> AppResult<Option<SpeechSegment>> {
        let query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE audio_path = ?1 LIMIT 1");
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![audio_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// Resolve one exact source/audio-alignment pair without exposing the raw connection to callers.
    pub(crate) fn get_segment_id_by_audio_alignment(
        &self,
        audio_path: &str,
        alignment_json: &str,
    ) -> AppResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM speech_segments WHERE audio_path = ?1 AND alignment_json = ?2",
                params![audio_path, alignment_json],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Segments usable as SPOT CHECKS: a trusted human answer already exists, and the raw ASR draft
    /// DIFFERS from it (Migration v44, docs/REMOTE_REVIEW_PLAN.md §2.1).
    ///
    /// "Trusted" must exclude a PEER'S FRESH GUESS. Without that, every clip a reviewer had just
    /// corrected became an answer key the moment they saved it, and the next reviewer was graded
    /// against it and marked wrong for merely disagreeing. (Found by the soak test: clients reported
    /// 61 successes against 60 clips, the extra one being a just-decided clip re-served as a check.)
    ///
    /// That was first expressed as `is_gold = 1`, which was correct in intent and inert in fact:
    /// **nothing in this application ever sets `is_gold`.** Every write of it lives inside
    /// `#[cfg(test)]`, and the migrations only ever declare it `DEFAULT 0` — the `gold_segments`
    /// table is a different thing entirely (the frozen eval set). So the candidate set was
    /// unconditionally empty in every real installation and the whole spot-check mechanism could
    /// never fire, silently, while the UI simply showed nothing.
    ///
    /// `reviewed_by IS NULL` is the trust signal that is actually populated. It is set
    /// UNCONDITIONALLY by `record_human_decision_by` to the name of whoever authored the row's
    /// current decision, and the desktop path passes `None` while every phone decision passes a
    /// reviewer name — so NULL means "verified here, by the owner, not by a remote reviewer", which
    /// is exactly the distinction the gold flag was reaching for. `is_gold = 1` is still honoured so
    /// that anything which does mark gold in future keeps working.
    ///
    /// The cost is honest and bounded: spot-check volume is capped by how much owner-verified work
    /// exists, so a small library yields few checks. `SpotCheckScore::checks` reports the real number
    /// rather than hiding it, and no conclusion should be drawn from a handful.
    ///
    /// The difference is the whole mechanism. Served with its raw draft, such a clip is a trap that a
    /// reviewer who actually listens will correct and a reviewer who taps "accept" will not — with no
    /// synthetic or planted data anywhere: these are real clips a human already answered.
    ///
    /// Ordered by id so the selection is deterministic; a queue that reshuffled its traps every poll
    /// would grade two reviewers on different material and make the scores incomparable.
    ///
    /// Excludes what THIS reviewer has already been scored on, and that exclusion is what makes the
    /// number mean anything. Without it every batch drew the same first-N-by-id, so a reviewer met the
    /// identical traps over and over: after the first batch they are answering from memory, not from
    /// listening. Worse, `record_spot_check` upserts on (segment_id, reviewer) — so the later,
    /// memorised attempt OVERWRITES the one honest measurement, and the score drifts upward the longer
    /// someone works. It also meant only the first few candidates were ever used no matter how many
    /// existed. Per-reviewer, not global: two reviewers meeting the same clip independently is the
    /// point (it is what `agreement_sample` reads).
    ///
    /// When a reviewer exhausts the pool they simply stop being measured, which is the honest outcome:
    /// `SpotCheckScore::checks` reports how many they actually answered, and re-testing on answers they
    /// already know would be a bigger number meaning less.
    ///
    /// `exclude` is the caller's set of "do not serve this reviewer this clip again" ids — in practice
    /// the clips they SKIPPED (R4.4). The SQL exclusion above cannot cover those: it lists clips with a
    /// row in `spot_checks`, i.e. ones the reviewer was SCORED on, and a skip is the absence of an
    /// answer and writes no score. Without this the one clip somebody said they could not judge was
    /// re-inserted into every subsequent batch forever, and the skip button did nothing for it.
    ///
    /// Filtered here rather than by the caller dropping candidates afterwards, and the difference
    /// matters: this still returns `limit` candidates, just DIFFERENT ones. Skipping a check must cost
    /// you that clip, never your place in the measurement — otherwise the honest exit doubles as a way
    /// to never be tested again.
    pub fn list_spot_check_candidates(
        &self,
        limit: usize,
        reviewer: &str,
        exclude: &std::collections::HashSet<String>,
        allowed_dialects: Option<&[String]>,
        focus: Option<&std::collections::HashSet<String>>,
    ) -> AppResult<Vec<(SpeechSegment, String)>> {
        // `reviewed_by IS NULL` keeps OWNER-desktop decisions (which pass no annotator name) as
        // keys while excluding a phone peer's fresh correction — grading one reviewer against
        // another's guess would mark them wrong for disagreeing. Machine-text keys are impossible
        // a layer down: `human_verified_text` (gold-provenance law, 2026-08-12) returns None
        // unless a REAL human decision produced the text, so flag-only verifies and reject rows
        // can never become answer keys regardless of what this WHERE admits.
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments
             WHERE verified = 1 AND raw_transcript <> '' AND (is_gold = 1 OR reviewed_by IS NULL)
               AND id NOT IN (SELECT segment_id FROM spot_checks WHERE reviewer = ?1)
               AND id NOT IN (
                   SELECT segment_id FROM review_events WHERE reviewer = ?1 COLLATE NOCASE
               )
             ORDER BY id ASC"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([reviewer], Self::map_row)?;
        let mut out = Vec::new();
        for row in rows {
            // Checked BEFORE the push, not after. Testing it afterwards makes `limit == 0` return ONE
            // candidate — an off-by-one that silently hands a spot check to a caller that asked for
            // none. Found by a fail-before revert that failed to fail.
            if out.len() >= limit {
                break;
            }
            let seg = row?;
            if exclude.contains(&seg.id) {
                continue; // they already declined this one; find them a different key
            }
            // A voice focus constrains the entire paid queue, including its hidden quality checks.
            // Serving an out-of-focus check and then correctly rejecting it at the decision boundary
            // would strand honest work; filtering before `limit` preserves the requested check count.
            if focus.is_some_and(|ids| !ids.contains(&seg.id)) {
                continue;
            }
            let Some(expected) = crate::quality::human_verified_text(&seg) else {
                continue; // a machine verdict is not an answer key
            };
            // The reviewer has to LISTEN to a check — that is the entire point of it — so a key whose
            // audio file has gone is not a key, it is a broken clip in the middle of their batch.
            //
            // MEASURED 2026-08-15: every answer key came from the original corpus, whose audio had been
            // deleted. `pending_segment_ids` had already been taught to skip unplayable WORK, but spot
            // checks are injected here, on a separate path, so they bypassed that filter entirely and
            // `/api/audio` answered 500. With SPOT_CHECK_EVERY = 8 that put a dead clip roughly every
            // eighth item — which is exactly what the reviewers reported: the first few clips play,
            // then one errors.
            if !std::path::Path::new(&seg.audio_path).is_file() {
                continue;
            }
            // Spot checks are injected AFTER the queue's own dialect filter, on this separate path —
            // so without this they were the one way a reviewer could still be handed a dialect they
            // do not speak, and the worst possible one: a check is SCORED, so an honest reviewer
            // fails a test they had no way to pass and reads as a blind-accepter. The queue and the
            // measurement have to agree about who may judge what.
            if !crate::dialect::reviewer_may_judge(allowed_dialects, &seg.audio_path) {
                continue;
            }
            // Only a clip whose raw draft is WRONG can distinguish listening from tapping.
            if learning_text_key(expected) == learning_text_key(&seg.raw_transcript) {
                continue;
            }
            let expected = expected.to_string();
            out.push((seg, expected));
        }
        Ok(out)
    }
}
