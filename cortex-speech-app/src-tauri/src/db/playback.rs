use super::*;

impl Database {
    /// The stored content fingerprint of a segment's audio, if one has been computed.
    ///
    /// Read as a single column rather than widening `SpeechSegment`: that struct's field order is
    /// load-bearing for SEGMENT_SELECT_COLUMNS' index-based map_row, and adding to it has broken
    /// every destructuring site twice before.
    /// The clip's length as the SERVER knows it — the denominator of every coverage ratio.
    ///
    /// A page reporting "I played 100ms of a 100ms clip" scores 1.0 against its own claim. The
    /// length has to come from here or the guard compares the client's claim with the client's claim.
    /// Does this recording still carry any machine PLACEHOLDER (`[...]`-shaped) or EMPTY drafts?
    ///
    /// Resume uses it to tell an ADOPTABLE finished file from a crash-interrupted stage: rows are
    /// committed before the champion pass fills them, so "rows exist" alone proved nothing — the
    /// 2026-08-14 incident left 36 placeholder rows that every later resume would have adopted as a
    /// completed file (found again by the 2026-08-20 external review). Same
    /// `placeholder_or_empty_transcript_sql` the review-queue exclusion uses, so the two cannot
    /// disagree about what a placeholder is.
    pub fn audio_path_has_placeholder_rows(&self, audio_path: &str) -> AppResult<bool> {
        let n: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM speech_segments
                 WHERE audio_path = ?1
                   AND {}",
                placeholder_or_empty_transcript_sql("raw_transcript")
            ),
            params![audio_path],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn segment_clip_duration_ms(&self, segment_id: &str) -> AppResult<Option<i64>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT NULLIF(COALESCE(duration_ms, 0), 0) FROM speech_segments WHERE id = ?1",
                [segment_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn segment_audio_content_hash(&self, segment_id: &str) -> AppResult<Option<String>> {
        use rusqlite::OptionalExtension;
        let value = self
            .conn
            .query_row(
                "SELECT NULLIF(TRIM(COALESCE(audio_content_hash, '')), '') FROM speech_segments WHERE id = ?1",
                [segment_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        match value {
            Some(content_hash) if is_canonical_audio_content_hash(&content_hash) => Ok(Some(content_hash)),
            Some(_) => {
                Err(AppError::Validation(format!("segment {segment_id} has a non-canonical audio content hash")))
            }
            None => Ok(None),
        }
    }

    /// Resolve the one canonical decoded-PCM identity recorded for an imported source.
    ///
    /// Every segment cut from one recording must carry the same source-level content hash.  Media
    /// playback uses this stricter source query before it copies bytes into the WebView cache; a
    /// missing or conflicting identity cannot be turned into a playback capability.  Keeping this
    /// query behind `Database` also prevents the media/IPC layers from escaping a raw connection.
    pub(crate) fn source_audio_content_hash(&self, audio_path: &str) -> AppResult<Option<String>> {
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT TRIM(audio_content_hash)
               FROM speech_segments
              WHERE audio_path=?1
                AND audio_content_hash IS NOT NULL
                AND TRIM(audio_content_hash)<>''
              ORDER BY TRIM(audio_content_hash)",
        )?;
        let hashes =
            statement.query_map([audio_path], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        match hashes.as_slice() {
            [] => Ok(None),
            [hash] if is_canonical_audio_content_hash(hash) => Ok(Some(hash.clone())),
            [..] if hashes.len() > 1 => Err(AppError::Validation(format!(
                "imported source {audio_path} has conflicting canonical audio identities"
            ))),
            _ => Err(AppError::Validation(format!(
                "imported source {audio_path} has a non-canonical audio content hash"
            ))),
        }
    }

    pub(crate) fn segment_source_span(&self, segment_id: &str) -> AppResult<Option<(i64, i64)>> {
        use rusqlite::OptionalExtension;
        let alignment = self
            .conn
            .query_row("SELECT alignment_json FROM speech_segments WHERE id = ?1", [segment_id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten();
        Ok(canonical_source_span(alignment.as_deref()))
    }

    /// Refuse a gold-minting verdict unless the authorized renderer recorded enough canonical-media
    /// traversal for THIS exact clip identity.
    ///
    /// The enforcement point is here, not in a renderer: a decision surface can be reloaded, scripted
    /// or replayed offline, so "the button was disabled" is a usability property, never a guarantee.
    /// The backend guarantee is deliberately narrower: it proves bounded media traversal, not human
    /// attention, audibility, comprehension, or truthfulness.
    ///
    /// `reject` is included deliberately. Marking a clip bad is a judgement about the AUDIO, and a
    /// reviewer who never traversed it cannot make it — a wrongly rejected clip is silently dropped from
    /// the corpus, which is the most expensive mistake available and the hardest to notice later.
    /// `skip` never reaches here: it writes no verdict at all.
    pub fn require_playback_evidence(
        &self,
        segment_id: &str,
        revision: i64,
        content_hash: &str,
        reviewer: Option<&str>,
    ) -> AppResult<()> {
        if self.has_sufficient_playback_evidence(segment_id, revision, content_hash, reviewer)? {
            return Ok(());
        }
        Err(AppError::Validation(format!(
            "E_NO_PLAYBACK_EVIDENCE: no receipt records at least {:.0}% canonical-media traversal for segment {segment_id} at revision {revision}. Reload the clip and play it before deciding.",
            MIN_PLAYBACK_COVERAGE * 100.0
        )))
    }

    /// Issue a short-lived desktop playback authority only after the live media grant is proven to
    /// resolve to this segment's imported source.  The returned UUID is intentionally not evidence by
    /// itself: it can authorize a receipt only after [`Self::finalize_desktop_playback_session_v1`]
    /// stores a plausible exact interval union against the same immutable clip identity. This is an
    /// integrity/replay boundary for the signed desktop renderer, not a claim that software can prove
    /// human attention or defeat a fully compromised renderer.
    // Every argument is a separately validated authority coordinate. Collapsing them into an
    // unvalidated bag would make cross-source/revision mix-ups easier at this database boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_desktop_playback_session_v1(
        &self,
        segment_id: &str,
        expected_revision: i64,
        media_grant_id: &str,
        client_attempt_id: &str,
        grant_source_path: &Path,
        grant_audio_content_hash: &str,
        reviewer: Option<&str>,
    ) -> AppResult<DesktopPlaybackSession> {
        if expected_revision < 0 {
            return Err(AppError::Validation("desktop playback expected revision must be non-negative".into()));
        }
        uuid::Uuid::parse_str(media_grant_id)
            .map_err(|_| AppError::Validation("desktop playback media-grant identity is invalid".into()))?;
        let parsed_attempt = uuid::Uuid::parse_str(client_attempt_id)
            .map_err(|_| AppError::Validation("desktop playback client-attempt identity is invalid".into()))?;
        if parsed_attempt.hyphenated().to_string() != client_attempt_id {
            return Err(AppError::Validation(
                "desktop playback client-attempt identity must be a lowercase hyphenated UUID".into(),
            ));
        }
        let grant_source_path_sha256 = canonical_grant_source_path_sha256(grant_source_path)?;
        let playback_receipt_id = uuid::Uuid::new_v4().to_string();
        let (issued_at_ms, issued_active_100ns) = self.playback_clock_now()?;
        let expires_at_ms = issued_at_ms
            .checked_add(DESKTOP_PLAYBACK_SESSION_TTL_MS)
            .ok_or_else(|| AppError::Other("desktop playback session expiry overflowed".into()))?;
        let active_session_ids = self.active_playback_session_ids(issued_active_100ns);

        let (session, wrote, reclaimed_session_ids) = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            Self::prune_abandoned_playback_sessions_on(&tx, &active_session_ids)?;
            let row: Option<DesktopPlaybackSegmentSnapshot> = tx
                .query_row(
                    "SELECT audio_path, COALESCE(review_revision,0),
                            NULLIF(TRIM(COALESCE(audio_content_hash,'')),''),
                            COALESCE(duration_ms,0), alignment_json
                       FROM speech_segments WHERE id=?1",
                    [segment_id],
                    |row| {
                        Ok(DesktopPlaybackSegmentSnapshot {
                            audio_path: row.get(0)?,
                            review_revision: row.get(1)?,
                            audio_content_hash: row.get(2)?,
                            duration_ms: row.get(3)?,
                            alignment_json: row.get(4)?,
                        })
                    },
                )
                .optional()?;
            let Some(row) = row else {
                return Err(AppError::Validation(format!(
                    "cannot start desktop playback for unknown segment {segment_id}"
                )));
            };
            let DesktopPlaybackSegmentSnapshot {
                audio_path,
                review_revision: segment_revision,
                audio_content_hash,
                duration_ms: clip_duration_ms,
                alignment_json,
            } = row;
            if segment_revision != expected_revision {
                return Err(AppError::Validation(format!(
                    "E_PLAYBACK_REVISION_CHANGED: clip revision is {segment_revision}, not requested revision {expected_revision}"
                )));
            }
            let segment_source_hash = canonical_grant_source_path_sha256(Path::new(&audio_path))?;
            if segment_source_hash != grant_source_path_sha256 {
                return Err(AppError::Validation(
                    "desktop playback media grant belongs to a different imported source".into(),
                ));
            }
            let audio_content_hash = audio_content_hash.ok_or_else(|| {
                AppError::Validation(format!(
                    "cannot start desktop playback for segment {segment_id} without a server-derived audio content hash"
                ))
            })?;
            if !is_canonical_audio_content_hash(&audio_content_hash) {
                return Err(AppError::Validation(format!(
                    "cannot start desktop playback for segment {segment_id} with a non-canonical audio content hash"
                )));
            }
            if audio_content_hash != grant_audio_content_hash {
                return Err(AppError::Validation(
                    "desktop playback media grant carries different audio bytes than this imported source".into(),
                ));
            }
            let (source_start_ms, source_end_ms) = canonical_source_span(alignment_json.as_deref()).ok_or_else(|| {
                AppError::Validation(format!(
                    "cannot start desktop playback for segment {segment_id} without a canonical source span"
                ))
            })?;
            if !source_span_matches_duration(source_start_ms, source_end_ms, clip_duration_ms) {
                return Err(AppError::Validation(format!(
                    "cannot start desktop playback for segment {segment_id} whose source span disagrees with decoded duration"
                )));
            }
            type ExistingAttempt = (String, String, String, String, i64, String, Option<String>, i64, i64, i64, i64);
            let existing: Option<ExistingAttempt> = tx
                .query_row(
                    "SELECT playback_receipt_id, media_grant_id, grant_source_path_sha256,
                            segment_id, segment_revision, audio_content_hash, reviewer,
                            clip_duration_ms, source_start_ms, source_end_ms, expires_at_ms
                       FROM desktop_playback_sessions_v4 WHERE client_attempt_id=?1",
                    [client_attempt_id],
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
                            row.get(10)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((
                existing_receipt_id,
                existing_grant_id,
                existing_source_hash,
                existing_segment_id,
                existing_revision,
                existing_content_hash,
                existing_reviewer,
                existing_duration_ms,
                existing_start_ms,
                existing_end_ms,
                existing_expires_at_ms,
            )) = existing
            {
                if existing_grant_id != media_grant_id
                    || existing_source_hash != grant_source_path_sha256
                    || existing_segment_id != segment_id
                    || existing_revision != segment_revision
                    || existing_content_hash != audio_content_hash
                    || existing_reviewer.as_deref() != reviewer
                    || existing_duration_ms != clip_duration_ms
                    || existing_start_ms != source_start_ms
                    || existing_end_ms != source_end_ms
                {
                    return Err(AppError::Validation(
                        "desktop playback client-attempt UUID was already used for a different exact request".into(),
                    ));
                }
                tx.rollback()?;
                return Ok((
                    DesktopPlaybackSession {
                        playback_receipt_id: existing_receipt_id,
                        segment_id: existing_segment_id,
                        segment_revision: existing_revision,
                        clip_duration_ms: existing_duration_ms,
                        expires_at_ms: existing_expires_at_ms,
                    },
                    false,
                    Vec::new(),
                ));
            }

            // Renderer teardown normally cancels the superseded authority. If that best-effort IPC
            // was lost, reclaim only the oldest never-finalized rows needed to admit this request.
            // This prevents ordinary browsing from turning the 64/2 abuse bounds into a 30-minute
            // workstation lockout while preserving every immutable receipt and consumed effect.
            let reclaimed_session_ids = Self::playback_sessions_to_reclaim_on(&tx, segment_id)?;

            let (live_total, live_for_segment): (i64, i64) = tx.query_row(
                "SELECT
                     (SELECT COUNT(*) FROM desktop_playback_sessions_v4 session
                       WHERE NOT EXISTS (SELECT 1 FROM playback_receipts receipt
                                          WHERE receipt.authority_session_id=session.playback_receipt_id)),
                     (SELECT COUNT(*) FROM desktop_playback_sessions_v4 session
                       WHERE session.segment_id=?1
                         AND NOT EXISTS (SELECT 1 FROM playback_receipts receipt
                                          WHERE receipt.authority_session_id=session.playback_receipt_id))",
                [segment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if live_total >= MAX_LIVE_DESKTOP_PLAYBACK_SESSIONS
                || live_for_segment >= MAX_LIVE_DESKTOP_PLAYBACK_SESSIONS_PER_SEGMENT
            {
                return Err(AppError::Validation(
                    "E_PLAYBACK_SESSION_LIMIT: too many live playback attempts; finish or reload an existing clip"
                        .into(),
                ));
            }
            tx.execute(
                "INSERT INTO desktop_playback_sessions_v4
                    (playback_receipt_id, media_grant_id, client_attempt_id, surface,
                     session_binding_sha256, grant_source_path_sha256,
                     segment_id, segment_revision, audio_content_hash, reviewer,
                     clip_duration_ms, source_start_ms, source_end_ms, issued_at_ms, expires_at_ms)
                 VALUES (?1,?2,?3,'desktop',NULL,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    playback_receipt_id,
                    media_grant_id,
                    client_attempt_id,
                    grant_source_path_sha256,
                    segment_id,
                    segment_revision,
                    audio_content_hash,
                    reviewer,
                    clip_duration_ms,
                    source_start_ms,
                    source_end_ms,
                    issued_at_ms,
                    expires_at_ms,
                ],
            )?;
            tx.commit()?;
            Ok((
                DesktopPlaybackSession {
                    playback_receipt_id: playback_receipt_id.clone(),
                    segment_id: segment_id.to_string(),
                    segment_revision,
                    clip_duration_ms,
                    expires_at_ms,
                },
                true,
                reclaimed_session_ids,
            ))
        })?;
        if !reclaimed_session_ids.is_empty() {
            let mut live_sessions = self.lock_playback_live_sessions();
            for reclaimed in reclaimed_session_ids {
                live_sessions.remove(&reclaimed);
            }
        }
        if wrote {
            self.lock_playback_live_sessions().insert(playback_receipt_id, issued_active_100ns);
            self.track_write()?;
        }
        Ok(session)
    }

    /// Convert one server-issued desktop playback session into immutable policy-4 evidence.  This is
    /// the only production writer for policy 4. It refuses instant counter inflation by capping unique
    /// media time at the application's maximum 2x rate relative to Windows active time (wall-clock
    /// changes and workstation suspend contribute nothing), and stores every canonical interval so the
    /// receipt's scalar can be independently re-derived.
    pub fn finalize_desktop_playback_session_v1(
        &self,
        playback_receipt_id: &str,
        media_grant_id: &str,
        grant_source_path: &Path,
        grant_audio_content_hash: &str,
        intervals: &[DesktopPlaybackInterval],
    ) -> AppResult<DesktopPlaybackReceipt> {
        uuid::Uuid::parse_str(playback_receipt_id)
            .map_err(|_| AppError::Validation("desktop playback receipt identity is invalid".into()))?;
        uuid::Uuid::parse_str(media_grant_id)
            .map_err(|_| AppError::Validation("desktop playback media-grant identity is invalid".into()))?;
        let grant_source_path_sha256 = canonical_grant_source_path_sha256(grant_source_path)?;
        let (observed_at_ms, observed_active_100ns) = self.playback_clock_now()?;
        let live_elapsed_ms = self.live_playback_elapsed_ms(playback_receipt_id, observed_active_100ns)?;

        let (receipt, wrote) = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            type SessionRow = (String, String, String, i64, String, Option<String>, i64, i64, i64, i64, i64);
            let session: Option<SessionRow> = tx
                .query_row(
                    "SELECT media_grant_id, grant_source_path_sha256, segment_id, segment_revision,
                            audio_content_hash, reviewer, clip_duration_ms, source_start_ms,
                            source_end_ms, issued_at_ms, expires_at_ms
                       FROM desktop_playback_sessions_v4 WHERE playback_receipt_id=?1",
                    [playback_receipt_id],
                    |row| {
                        Ok((
                            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?,
                            row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?,
                        ))
                    },
                )
                .optional()?;
            let Some((
                stored_grant_id,
                stored_source_hash,
                segment_id,
                segment_revision,
                audio_content_hash,
                reviewer,
                clip_duration_ms,
                source_start_ms,
                source_end_ms,
                issued_at_ms,
                _expires_at_ms,
            )) = session
            else {
                return Err(AppError::Validation("desktop playback session is missing or was never issued".into()));
            };
            if stored_grant_id != media_grant_id
                || stored_source_hash != grant_source_path_sha256
                || audio_content_hash != grant_audio_content_hash
            {
                return Err(AppError::Validation(
                    "desktop playback session no longer matches its live immutable media grant".into(),
                ));
            }

            let (unique_played_ms, interval_union_sha256) =
                validate_desktop_playback_intervals(intervals, clip_duration_ms)?;
            let coverage_ratio = (unique_played_ms as f64 / clip_duration_ms as f64).min(1.0);

            let replay: Option<(i64, i64, f64, String)> = tx
                .query_row(
                    "SELECT segment_revision, played_ms, coverage_ratio, interval_union_sha256
                       FROM playback_receipts
                      WHERE authority_session_id=?1 AND policy_version=?2",
                    params![playback_receipt_id, DESKTOP_PLAYBACK_POLICY_VERSION],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            if let Some((stored_revision, stored_played_ms, stored_coverage, stored_union_sha256)) = replay {
                if stored_revision != segment_revision
                    || stored_played_ms != unique_played_ms
                    || stored_union_sha256 != interval_union_sha256
                {
                    return Err(AppError::Validation(
                        "desktop playback receipt replay carries a different interval union".into(),
                    ));
                }
                tx.rollback()?;
                return Ok((
                    DesktopPlaybackReceipt {
                        playback_receipt_id: playback_receipt_id.to_string(),
                        segment_id,
                        segment_revision,
                        unique_played_ms,
                        clip_duration_ms,
                        coverage_ratio: stored_coverage,
                    },
                    false,
                ));
            }

            let required_unique_played_ms = clip_duration_ms
                .checked_mul(MIN_PLAYBACK_COVERAGE_NUMERATOR)
                .and_then(|value| value.checked_add(MIN_PLAYBACK_COVERAGE_DENOMINATOR - 1))
                .map(|value| value / MIN_PLAYBACK_COVERAGE_DENOMINATOR)
                .ok_or_else(|| AppError::Validation("desktop playback coverage threshold overflowed".into()))?;
            if unique_played_ms < required_unique_played_ms {
                return Err(AppError::Validation(format!(
                    "E_PLAYBACK_COVERAGE_INSUFFICIENT: {unique_played_ms} ms is below the required {required_unique_played_ms} ms for this exact server clip duration"
                )));
            }

            let elapsed_ms = live_elapsed_ms.ok_or_else(|| {
                AppError::Validation("desktop playback session has no live active-time authority; reload the clip".into())
            })?;
            if elapsed_ms > DESKTOP_PLAYBACK_SESSION_TTL_MS {
                return Err(AppError::Validation("desktop playback session expired; reload the clip".into()));
            }
            // Active time is minted in this process from QueryUnbiasedInterruptTimePrecise. The former
            // 250 ms grace was doubled with playback rate and let a 400 ms clip mint 85% immediately.
            // Exact lost-response replay is handled above and needs no grace or process-local clock.
            let maximum_plausible_ms = elapsed_ms
                .checked_mul(DESKTOP_PLAYBACK_MAX_RATE)
                .unwrap_or(i64::MAX)
                .min(clip_duration_ms);
            if unique_played_ms > maximum_plausible_ms {
                return Err(AppError::Validation(format!(
                    "E_PLAYBACK_TIME_IMPLAUSIBLE: {unique_played_ms} ms of unique media cannot be traversed in {elapsed_ms} ms at the app's maximum playback rate"
                )));
            }

            let current_audio_path: Option<String> = tx
                .query_row(
                    "SELECT segment.audio_path FROM speech_segments segment
                      WHERE segment.id=?1
                        AND COALESCE(segment.review_revision,0)=?2
                        AND segment.audio_content_hash=?3
                        AND segment.duration_ms=?4
                        AND json_valid(segment.alignment_json)
                        AND json_extract(segment.alignment_json,'$.source_start_ms')=?5
                        AND json_extract(segment.alignment_json,'$.source_end_ms')=?6",
                params![
                    segment_id,
                    segment_revision,
                    audio_content_hash,
                    clip_duration_ms,
                    source_start_ms,
                    source_end_ms,
                ],
                |row| row.get(0),
                )
                .optional()?;
            let current_source_matches = current_audio_path
                .as_deref()
                .map(Path::new)
                .map(canonical_grant_source_path_sha256)
                .transpose()?
                .is_some_and(|current| current == stored_source_hash);
            if !current_source_matches {
                return Err(AppError::Validation(format!(
                    "{PLAYBACK_EVIDENCE_CHANGED}: clip source, identity, or revision changed after the desktop playback session began"
                )));
            }

            for (ordinal, interval) in intervals.iter().enumerate() {
                tx.execute(
                    "INSERT INTO desktop_playback_intervals_v4
                        (playback_receipt_id, ordinal, start_ms, end_ms, observed_at_ms)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![playback_receipt_id, ordinal as i64, interval.start_ms, interval.end_ms, observed_at_ms],
                )?;
            }
            tx.execute(
                "INSERT INTO playback_receipts
                    (segment_id, segment_revision, audio_fingerprint, reviewer, session_id,
                     started_at_ms, played_ms, clip_duration_ms, coverage_ratio, policy_version,
                     source_start_ms, source_end_ms, authority_session_id, interval_union_sha256)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    segment_id,
                    segment_revision,
                    audio_content_hash,
                    reviewer,
                    playback_receipt_id,
                    issued_at_ms,
                    unique_played_ms,
                    clip_duration_ms,
                    coverage_ratio,
                    DESKTOP_PLAYBACK_POLICY_VERSION,
                    source_start_ms,
                    source_end_ms,
                    playback_receipt_id,
                    interval_union_sha256,
                ],
            )?;
            tx.commit()?;
            Ok((
                DesktopPlaybackReceipt {
                    playback_receipt_id: playback_receipt_id.to_string(),
                    segment_id,
                    segment_revision,
                    unique_played_ms,
                    clip_duration_ms,
                    coverage_ratio,
                },
                true,
            ))
        })?;
        if wrote {
            self.lock_playback_live_sessions().remove(playback_receipt_id);
            self.track_write()?;
        }
        Ok(receipt)
    }

    /// Recover an already-finalized exact receipt without depending on its now-ephemeral media grant.
    /// This path cannot mint evidence: it returns `Some` only when the immutable policy-4 row already
    /// exists and the caller presents the identical canonical interval union. It is used after a lost
    /// response or workstation suspend, when the 30-minute cache lease may legitimately have expired.
    pub fn replay_finalized_desktop_playback_receipt_v1(
        &self,
        playback_receipt_id: &str,
        media_grant_id: &str,
        intervals: &[DesktopPlaybackInterval],
    ) -> AppResult<Option<DesktopPlaybackReceipt>> {
        use rusqlite::OptionalExtension;
        if uuid::Uuid::parse_str(playback_receipt_id).is_err() {
            return Err(AppError::Validation("desktop playback receipt identity is invalid".into()));
        }
        if uuid::Uuid::parse_str(media_grant_id).is_err() {
            return Err(AppError::Validation("desktop playback media-grant identity is invalid".into()));
        }
        type ReplayRow = (String, String, i64, i64, i64, f64, String);
        let replay: Option<ReplayRow> = self
            .conn
            .query_row(
                "SELECT session.media_grant_id, session.segment_id, session.segment_revision, receipt.played_ms,
                        session.clip_duration_ms, receipt.coverage_ratio,
                        receipt.interval_union_sha256
                   FROM desktop_playback_sessions_v4 session
                   JOIN playback_receipts receipt
                     ON receipt.authority_session_id=session.playback_receipt_id
                    AND receipt.policy_version=?2
                    AND receipt.segment_id=session.segment_id
                    AND receipt.segment_revision=session.segment_revision
                    AND receipt.clip_duration_ms=session.clip_duration_ms
                  WHERE session.playback_receipt_id=?1",
                params![playback_receipt_id, DESKTOP_PLAYBACK_POLICY_VERSION],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .optional()?;
        let Some((
            stored_grant_id,
            segment_id,
            segment_revision,
            stored_played_ms,
            clip_duration_ms,
            coverage_ratio,
            stored_hash,
        )) = replay
        else {
            return Ok(None);
        };
        if stored_grant_id != media_grant_id {
            return Err(AppError::Validation(
                "desktop playback receipt replay belongs to a different media grant".into(),
            ));
        }
        let (unique_played_ms, interval_union_sha256) =
            validate_desktop_playback_intervals(intervals, clip_duration_ms)?;
        if unique_played_ms != stored_played_ms || interval_union_sha256 != stored_hash {
            return Err(AppError::Validation(
                "desktop playback receipt replay carries a different interval union".into(),
            ));
        }
        Ok(Some(DesktopPlaybackReceipt {
            playback_receipt_id: playback_receipt_id.to_string(),
            segment_id,
            segment_revision,
            unique_played_ms,
            clip_duration_ms,
            coverage_ratio,
        }))
    }

    /// Finalize one authenticated Couch attempt into the same immutable policy-4 interval authority
    /// used by the desktop. The browser supplies only canonical clip-relative intervals; reviewer,
    /// cookie-session binding, clip revision/hash/span/duration and source path all originate in the
    /// server-issued in-memory attempt and are re-resolved here under BEGIN IMMEDIATE.
    pub(crate) fn finalize_couch_playback_attempt_v1(
        &self,
        attempt: &CouchPlaybackAttemptAuthority,
        intervals: &[DesktopPlaybackInterval],
        server_monotonic_elapsed_ms: i64,
    ) -> AppResult<DesktopPlaybackReceipt> {
        for (value, label) in [
            (&attempt.playback_receipt_id, "Couch playback receipt"),
            (&attempt.media_grant_id, "Couch playback media grant"),
            (&attempt.client_attempt_id, "Couch playback client attempt"),
        ] {
            let parsed = uuid::Uuid::parse_str(value)
                .map_err(|_| AppError::Validation(format!("{label} identity is invalid")))?;
            if parsed.hyphenated().to_string() != *value {
                return Err(AppError::Validation(format!("{label} identity must be a lowercase hyphenated UUID")));
            }
        }
        if attempt.session_binding_sha256.len() != 64
            || !attempt
                .session_binding_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || attempt.reviewer.trim().is_empty()
            || attempt.segment_revision < 0
            || !is_canonical_audio_content_hash(&attempt.audio_content_hash)
            || !source_span_matches_duration(attempt.source_start_ms, attempt.source_end_ms, attempt.clip_duration_ms)
            || attempt.issued_at_ms <= 0
            || attempt.expires_at_ms <= attempt.issued_at_ms
            || server_monotonic_elapsed_ms < 0
        {
            return Err(AppError::Validation("Couch playback attempt authority is malformed".into()));
        }
        let grant_source_path_sha256 = canonical_grant_source_path_sha256(&attempt.source_path)?;
        let source_lease = crate::media::verify_current_source_lease(&attempt.source_path, &attempt.audio_content_hash)
            .map_err(|error| AppError::Validation(format!("{PLAYBACK_EVIDENCE_CHANGED}: {error}")))?;
        let (unique_played_ms, interval_union_sha256) =
            validate_desktop_playback_intervals(intervals, attempt.clip_duration_ms)?;
        if unique_played_ms
            .checked_mul(MIN_PLAYBACK_COVERAGE_DENOMINATOR)
            .ok_or_else(|| AppError::Validation("Couch playback coverage overflowed".into()))?
            < attempt
                .clip_duration_ms
                .checked_mul(MIN_PLAYBACK_COVERAGE_NUMERATOR)
                .ok_or_else(|| AppError::Validation("Couch playback duration overflowed".into()))?
        {
            return Err(AppError::Validation(format!(
                "E_NO_PLAYBACK_EVIDENCE: renderer-reported playback traversal covers less than {:.0}% of this clip",
                MIN_PLAYBACK_COVERAGE * 100.0
            )));
        }
        let maximum_plausible_ms = server_monotonic_elapsed_ms
            .checked_mul(DESKTOP_PLAYBACK_MAX_RATE)
            .unwrap_or(i64::MAX)
            .min(attempt.clip_duration_ms);
        if unique_played_ms > maximum_plausible_ms {
            return Err(AppError::Validation(format!(
                "E_PLAYBACK_TIME_IMPLAUSIBLE: {unique_played_ms} ms of renderer traversal cannot occur in {server_monotonic_elapsed_ms} ms at the maximum playback rate"
            )));
        }
        let observed_at_ms = playback_server_now_ms()?.max(1);
        let coverage_ratio = (unique_played_ms as f64 / attempt.clip_duration_ms as f64).min(1.0);

        let (receipt, wrote) = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            type ReplayRow = (String, String, String, String, String, i64, i64, f64, String);
            let replay: Option<ReplayRow> = tx
                .query_row(
                    "SELECT session.media_grant_id, session.client_attempt_id,
                            session.session_binding_sha256, session.reviewer, session.segment_id,
                            session.segment_revision, receipt.played_ms, receipt.coverage_ratio,
                            receipt.interval_union_sha256
                       FROM desktop_playback_sessions_v4 session
                       JOIN playback_receipts receipt
                         ON receipt.authority_session_id=session.playback_receipt_id
                        AND receipt.policy_version=?2
                      WHERE session.playback_receipt_id=?1 AND session.surface='couch'",
                    params![attempt.playback_receipt_id, DESKTOP_PLAYBACK_POLICY_VERSION],
                    |row| {
                        Ok((
                            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                            row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((
                media_grant_id,
                client_attempt_id,
                session_binding_sha256,
                reviewer,
                segment_id,
                segment_revision,
                stored_played_ms,
                stored_coverage_ratio,
                stored_interval_hash,
            )) = replay
            {
                if media_grant_id != attempt.media_grant_id
                    || client_attempt_id != attempt.client_attempt_id
                    || session_binding_sha256 != attempt.session_binding_sha256
                    || !reviewer.eq_ignore_ascii_case(&attempt.reviewer)
                    || segment_id != attempt.segment_id
                    || segment_revision != attempt.segment_revision
                    || stored_played_ms != unique_played_ms
                    || stored_interval_hash != interval_union_sha256
                {
                    return Err(AppError::Validation(
                        "Couch playback finalization replay changed its exact authority or interval union".into(),
                    ));
                }
                tx.rollback()?;
                return Ok((
                    DesktopPlaybackReceipt {
                        playback_receipt_id: attempt.playback_receipt_id.clone(),
                        segment_id,
                        segment_revision,
                        unique_played_ms,
                        clip_duration_ms: attempt.clip_duration_ms,
                        coverage_ratio: stored_coverage_ratio,
                    },
                    false,
                ));
            }

            let conflicting_attempt: Option<(String, String, String, i64)> = tx
                .query_row(
                    "SELECT playback_receipt_id, session_binding_sha256, segment_id, segment_revision
                       FROM desktop_playback_sessions_v4 WHERE client_attempt_id=?1",
                    [attempt.client_attempt_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            if conflicting_attempt.is_some() {
                return Err(AppError::Validation(
                    "Couch playback client-attempt UUID was already bound to a different request".into(),
                ));
            }

            let current: Option<(String, i64, String, i64, Option<String>)> = tx
                .query_row(
                    "SELECT audio_path, COALESCE(review_revision,0), audio_content_hash,
                            duration_ms, alignment_json
                       FROM speech_segments WHERE id=?1",
                    [attempt.segment_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                )
                .optional()?;
            let Some((audio_path, revision, content_hash, duration_ms, alignment_json)) = current else {
                return Err(AppError::Validation(format!(
                    "{PLAYBACK_EVIDENCE_CHANGED}: Couch playback segment disappeared before finalization"
                )));
            };
            let current_span = canonical_source_span(alignment_json.as_deref());
            if revision != attempt.segment_revision
                || content_hash != attempt.audio_content_hash
                || duration_ms != attempt.clip_duration_ms
                || current_span != Some((attempt.source_start_ms, attempt.source_end_ms))
                || canonical_grant_source_path_sha256(Path::new(&audio_path))? != grant_source_path_sha256
                || source_lease.source_path != std::fs::canonicalize(Path::new(&audio_path))?
            {
                return Err(AppError::Validation(format!(
                    "{PLAYBACK_EVIDENCE_CHANGED}: clip source, identity, revision, duration, or span changed before Couch playback finalization"
                )));
            }

            tx.execute(
                "INSERT INTO desktop_playback_sessions_v4
                    (playback_receipt_id,media_grant_id,client_attempt_id,surface,
                     session_binding_sha256,grant_source_path_sha256,segment_id,segment_revision,
                     audio_content_hash,reviewer,clip_duration_ms,source_start_ms,source_end_ms,
                     issued_at_ms,expires_at_ms)
                 VALUES (?1,?2,?3,'couch',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    attempt.playback_receipt_id,
                    attempt.media_grant_id,
                    attempt.client_attempt_id,
                    attempt.session_binding_sha256,
                    grant_source_path_sha256,
                    attempt.segment_id,
                    attempt.segment_revision,
                    attempt.audio_content_hash,
                    attempt.reviewer,
                    attempt.clip_duration_ms,
                    attempt.source_start_ms,
                    attempt.source_end_ms,
                    attempt.issued_at_ms,
                    attempt.expires_at_ms,
                ],
            )?;
            for (ordinal, interval) in intervals.iter().enumerate() {
                tx.execute(
                    "INSERT INTO desktop_playback_intervals_v4
                        (playback_receipt_id,ordinal,start_ms,end_ms,observed_at_ms)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        attempt.playback_receipt_id,
                        ordinal as i64,
                        interval.start_ms,
                        interval.end_ms,
                        observed_at_ms,
                    ],
                )?;
            }
            tx.execute(
                "INSERT INTO playback_receipts
                    (segment_id,segment_revision,audio_fingerprint,reviewer,session_id,
                     started_at_ms,played_ms,clip_duration_ms,coverage_ratio,policy_version,
                     source_start_ms,source_end_ms,authority_session_id,interval_union_sha256)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    attempt.segment_id,
                    attempt.segment_revision,
                    attempt.audio_content_hash,
                    attempt.reviewer,
                    attempt.playback_receipt_id,
                    attempt.issued_at_ms,
                    unique_played_ms,
                    attempt.clip_duration_ms,
                    coverage_ratio,
                    DESKTOP_PLAYBACK_POLICY_VERSION,
                    attempt.source_start_ms,
                    attempt.source_end_ms,
                    attempt.playback_receipt_id,
                    interval_union_sha256,
                ],
            )?;
            tx.commit()?;
            Ok((
                DesktopPlaybackReceipt {
                    playback_receipt_id: attempt.playback_receipt_id.clone(),
                    segment_id: attempt.segment_id.clone(),
                    segment_revision: attempt.segment_revision,
                    unique_played_ms,
                    clip_duration_ms: attempt.clip_duration_ms,
                    coverage_ratio,
                },
                true,
            ))
        })?;
        if wrote {
            self.track_write()?;
        }
        Ok(receipt)
    }

    /// Idempotent response-loss recovery for a finalized Couch receipt. This cannot mint evidence:
    /// every authority coordinate and the interval-union digest must already exist durably.
    pub(crate) fn replay_finalized_couch_playback_receipt_v1(
        &self,
        playback_receipt_id: &str,
        client_attempt_id: &str,
        session_binding_sha256: &str,
        reviewer: &str,
        intervals: &[DesktopPlaybackInterval],
    ) -> AppResult<Option<DesktopPlaybackReceipt>> {
        if uuid::Uuid::parse_str(playback_receipt_id).is_err() || uuid::Uuid::parse_str(client_attempt_id).is_err() {
            return Err(AppError::Validation("Couch playback replay identity is invalid".into()));
        }
        type Replay = (String, i64, i64, i64, f64, String);
        let replay: Option<Replay> = self
            .conn
            .query_row(
                "SELECT session.segment_id,session.segment_revision,session.clip_duration_ms,
                        receipt.played_ms,receipt.coverage_ratio,receipt.interval_union_sha256
                   FROM desktop_playback_sessions_v4 session
                   JOIN playback_receipts receipt
                     ON receipt.authority_session_id=session.playback_receipt_id
                    AND receipt.policy_version=?6
                  WHERE session.playback_receipt_id=?1
                    AND session.client_attempt_id=?2
                    AND session.surface=?5
                    AND session.session_binding_sha256=?3
                    AND session.reviewer=?4 COLLATE NOCASE",
                params![
                    playback_receipt_id,
                    client_attempt_id,
                    session_binding_sha256,
                    reviewer,
                    "couch",
                    DESKTOP_PLAYBACK_POLICY_VERSION,
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()?;
        let Some((segment_id, segment_revision, clip_duration_ms, stored_played_ms, coverage_ratio, stored_hash)) =
            replay
        else {
            return Ok(None);
        };
        let (unique_played_ms, supplied_hash) = validate_desktop_playback_intervals(intervals, clip_duration_ms)?;
        if unique_played_ms != stored_played_ms || supplied_hash != stored_hash {
            return Err(AppError::Validation(
                "Couch playback receipt replay carries a different interval union".into(),
            ));
        }
        Ok(Some(DesktopPlaybackReceipt {
            playback_receipt_id: playback_receipt_id.to_string(),
            segment_id,
            segment_revision,
            unique_played_ms,
            clip_duration_ms,
            coverage_ratio,
        }))
    }

    pub(crate) fn couch_playback_proof_v4(
        &self,
        segment_id: &str,
        revision: i64,
        content_hash: &str,
        reviewer: &str,
        session_binding_sha256: &str,
        playback_receipt_id: &str,
    ) -> AppResult<Option<PlaybackDecisionProof>> {
        if uuid::Uuid::parse_str(playback_receipt_id).is_err()
            || !is_canonical_audio_content_hash(content_hash)
            || session_binding_sha256.len() != 64
        {
            return Ok(None);
        }
        let binding: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT segment.audio_path,session.grant_source_path_sha256
                   FROM desktop_playback_sessions_v4 session
                   JOIN speech_segments segment ON segment.id=session.segment_id
                  WHERE session.playback_receipt_id=?1
                    AND session.surface='couch'
                    AND session.session_binding_sha256=?2
                    AND session.reviewer=?3 COLLATE NOCASE
                    AND session.segment_id=?4
                    AND session.segment_revision=?5
                    AND session.audio_content_hash=?6",
                params![playback_receipt_id, session_binding_sha256, reviewer, segment_id, revision, content_hash,],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((audio_path, issued_source_hash)) = binding else {
            return Ok(None);
        };
        if canonical_grant_source_path_sha256(Path::new(&audio_path))? != issued_source_hash {
            return Ok(None);
        }
        let source_lease = crate::media::verify_current_source_lease(Path::new(&audio_path), content_hash)
            .map_err(|error| AppError::Validation(format!("{PLAYBACK_EVIDENCE_CHANGED}: {error}")))?;
        let Some((source_start_ms, source_end_ms)) = self.segment_source_span(segment_id)? else {
            return Ok(None);
        };
        let sufficient = has_sufficient_desktop_playback_evidence_v4_on(
            &self.conn,
            segment_id,
            revision,
            content_hash,
            source_start_ms,
            source_end_ms,
            Some(reviewer),
            playback_receipt_id,
        )?;
        Ok(sufficient.then_some(PlaybackDecisionProof {
            segment_revision: revision,
            audio_content_hash: content_hash.to_string(),
            source_start_ms,
            source_end_ms,
            authority_session_id: Some(playback_receipt_id.to_string()),
            source_lease: Some(source_lease),
        }))
    }

    pub(crate) fn desktop_playback_proof_v4(
        &self,
        segment_id: &str,
        revision: i64,
        content_hash: &str,
        playback_receipt_id: &str,
        source_lease: Option<crate::media::VerifiedMediaSourceLease>,
    ) -> AppResult<Option<PlaybackDecisionProof>> {
        if uuid::Uuid::parse_str(playback_receipt_id).is_err() || !is_canonical_audio_content_hash(content_hash) {
            return Ok(None);
        }
        let source_binding: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT segment.audio_path, session.grant_source_path_sha256
                   FROM desktop_playback_sessions_v4 session
                   JOIN speech_segments segment ON segment.id=session.segment_id
                  WHERE session.playback_receipt_id=?1 AND segment.id=?2",
                params![playback_receipt_id, segment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((current_audio_path, issued_source_hash)) = source_binding else {
            return Ok(None);
        };
        if canonical_grant_source_path_sha256(Path::new(&current_audio_path))? != issued_source_hash {
            return Ok(None);
        }
        let source_lease = match source_lease {
            Some(lease)
                if lease.audio_content_hash == content_hash
                    && canonical_grant_source_path_sha256(&lease.source_path)? == issued_source_hash =>
            {
                lease
            }
            Some(_) => return Ok(None),
            None => crate::media::verify_current_source_lease(Path::new(&current_audio_path), content_hash)
                .map_err(|error| AppError::Validation(format!("{PLAYBACK_EVIDENCE_CHANGED}: {error}")))?,
        };
        let Some((source_start_ms, source_end_ms)) = self.segment_source_span(segment_id)? else {
            return Ok(None);
        };
        let sufficient = has_sufficient_desktop_playback_evidence_v4_on(
            &self.conn,
            segment_id,
            revision,
            content_hash,
            source_start_ms,
            source_end_ms,
            None,
            playback_receipt_id,
        )?;
        Ok(sufficient.then_some(PlaybackDecisionProof {
            segment_revision: revision,
            audio_content_hash: content_hash.to_string(),
            source_start_ms,
            source_end_ms,
            authority_session_id: Some(playback_receipt_id.to_string()),
            source_lease: Some(source_lease),
        }))
    }

    pub(crate) fn desktop_playback_media_grant_id(&self, playback_receipt_id: &str) -> AppResult<Option<String>> {
        if uuid::Uuid::parse_str(playback_receipt_id).is_err() {
            return Ok(None);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT session.media_grant_id
                   FROM desktop_playback_sessions_v4 session
                   JOIN playback_receipts receipt
                     ON receipt.authority_session_id=session.playback_receipt_id
                    AND receipt.policy_version=?2
                  WHERE session.playback_receipt_id=?1",
                params![playback_receipt_id, DESKTOP_PLAYBACK_POLICY_VERSION],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(crate) fn desktop_playback_recovery_source_identity(
        &self,
        segment_id: &str,
        revision: i64,
        playback_receipt_id: &str,
    ) -> AppResult<Option<(PathBuf, String)>> {
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT segment.audio_path, segment.audio_content_hash, session.grant_source_path_sha256
                   FROM desktop_playback_sessions_v4 session
                   JOIN playback_receipts receipt
                     ON receipt.authority_session_id=session.playback_receipt_id
                    AND receipt.policy_version=?4
                   JOIN speech_segments segment ON segment.id=session.segment_id
                  WHERE session.playback_receipt_id=?1
                    AND segment.id=?2
                    AND COALESCE(segment.review_revision,0)=?3
                    AND segment.audio_content_hash=session.audio_content_hash
                    AND receipt.segment_revision=?3",
                params![playback_receipt_id, segment_id, revision, DESKTOP_PLAYBACK_POLICY_VERSION],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((audio_path, audio_content_hash, issued_source_hash)) = row else {
            return Ok(None);
        };
        let path = PathBuf::from(audio_path);
        if canonical_grant_source_path_sha256(&path)? != issued_source_hash {
            return Ok(None);
        }
        Ok(Some((path, audio_content_hash)))
    }

    /// Record an untrusted client observation without making the caller invent server-owned identity.
    #[cfg(test)]
    pub(crate) fn record_playback_observation(&self, observation: PlaybackReceiptObservation) -> AppResult<()> {
        self.record_playback_receipt(&PlaybackReceipt {
            segment_id: observation.segment_id,
            segment_revision: 0,
            audio_content_hash: String::new(),
            reviewer: observation.reviewer,
            session_id: observation.session_id,
            started_at_ms: observation.started_at_ms,
            played_ms: observation.played_ms,
            clip_duration_ms: observation.claimed_clip_duration_ms,
            source_start_ms: None,
            source_end_ms: None,
        })
    }

    /// A renderer-originated record of canonical media traversal for this exact clip revision.
    ///
    /// `played_ms` is cumulative MEDIA time advanced, never wall-clock and never a `play()` call —
    /// download, metadata load and an autoplay attempt do not establish media traversal. Even a valid
    /// receipt does not prove human attention or comprehension.
    ///
    /// EVERY identity field is resolved HERE, from the row — revision, decoded-PCM content hash, and the
    /// coverage denominator alike. The struct's values for those three are treated as claims and
    /// overwritten. Found by the 2026-08-19 hunt: the phone path resolved all three at its call
    /// site, the desktop resolved two and passed the renderer's clipDurationMs through — and both
    /// desktop surfaces report the WHOLE source file's length (403 of 414 clips share one
    /// recording), so an honest 10s listen scored ~0.004 and was refused, while a shrunk claim
    /// minted 1.0. Per-caller resolution is exactly how one caller gets it wrong; this is the one
    /// door, so no caller can.
    pub fn record_playback_receipt(&self, receipt: &PlaybackReceipt) -> AppResult<()> {
        use rusqlite::OptionalExtension;
        validate_playback_receipt_nonnegative_fields(receipt)?;
        let row: Option<(i64, Option<String>, i64, Option<String>)> = self
            .conn
            .query_row(
                "SELECT COALESCE(review_revision, 0),
                        NULLIF(TRIM(COALESCE(audio_content_hash, '')), ''),
                        COALESCE(duration_ms, 0), alignment_json
                 FROM speech_segments WHERE id = ?1",
                [&receipt.segment_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((revision, content_hash, duration_ms, alignment_json)) = row else {
            return Err(AppError::Validation(format!(
                "cannot mint a listening receipt for unknown segment {}",
                receipt.segment_id
            )));
        };
        let content_hash = content_hash.ok_or_else(|| {
            AppError::Validation(format!(
                "cannot mint a listening receipt for segment {} without a server-derived audio content hash",
                receipt.segment_id
            ))
        })?;
        if !is_canonical_audio_content_hash(&content_hash) {
            return Err(AppError::Validation(format!(
                "cannot mint a listening receipt for segment {} with a non-canonical audio content hash",
                receipt.segment_id
            )));
        }
        if duration_ms <= 0 {
            return Err(AppError::Validation(format!(
                "cannot mint a listening receipt for segment {} without a positive server clip duration",
                receipt.segment_id
            )));
        }
        let (source_start_ms, source_end_ms) = canonical_source_span(alignment_json.as_deref()).ok_or_else(|| {
            AppError::Validation(format!(
                "cannot mint policy-3 listening evidence for segment {} without a canonical server source span",
                receipt.segment_id
            ))
        })?;
        if !source_span_matches_duration(source_start_ms, source_end_ms, duration_ms) {
            return Err(AppError::Validation(format!(
                "cannot mint policy-3 listening evidence for segment {} whose source span disagrees with decoded duration",
                receipt.segment_id
            )));
        }
        let resolved = PlaybackReceipt {
            segment_revision: revision,
            audio_content_hash: content_hash,
            clip_duration_ms: duration_ms,
            source_start_ms: Some(source_start_ms),
            source_end_ms: Some(source_end_ms),
            ..receipt.clone()
        };
        self.record_playback_receipt_raw(&resolved)
    }

    /// Mint a receipt ONLY IF the row is still at `expected_revision` — one atomic statement.
    ///
    /// The 2026-08-20 hunt found the front door's own resolution re-opening the hole the serve/decide
    /// fence had just closed: the fence verifies the serve against the current revision, then
    /// [`Self::record_playback_receipt`] re-queries the row, so a write landing in between rebinds
    /// the receipt to a revision (and content hash) the reviewer never heard. Check-and-insert as one
    /// `INSERT … SELECT … WHERE review_revision = ?` closes that: SQLite executes it atomically, so
    /// either the receipt is minted against exactly the verified revision or nothing is written.
    ///
    /// Returns `false` (and writes NOTHING) when the row moved or vanished — the caller treats that
    /// as the fence firing late, not as an error.
    pub fn record_playback_receipt_if_at_revision(
        &self,
        receipt: &PlaybackReceipt,
        expected_revision: i64,
    ) -> AppResult<bool> {
        validate_playback_receipt_nonnegative_fields(receipt)?;
        if expected_revision < 0 {
            return Err(AppError::Validation(
                "cannot record playback evidence at a negative expected segment revision".into(),
            ));
        }
        let changed = self.conn.execute(
            "INSERT INTO playback_receipts (segment_id, segment_revision, audio_fingerprint, reviewer,
                                            session_id, started_at_ms, played_ms, clip_duration_ms,
                                            coverage_ratio, policy_version, source_start_ms, source_end_ms)
             SELECT s.id, ?2, s.audio_content_hash,
                     ?3, ?4, ?5, ?6, s.duration_ms,
                     MIN(1.0, CAST(?6 AS REAL) / CAST(s.duration_ms AS REAL)),
                     ?7,
                     json_extract(s.alignment_json, '$.source_start_ms'),
                     json_extract(s.alignment_json, '$.source_end_ms')
             FROM speech_segments s
             WHERE s.id = ?1 AND COALESCE(s.review_revision, 0) = ?2
               AND typeof(s.duration_ms) = 'integer' AND s.duration_ms > 0
               AND typeof(s.audio_content_hash) = 'text'
               AND length(s.audio_content_hash) = 64
               AND s.audio_content_hash NOT GLOB '*[^0-9a-f]*'
               AND json_valid(s.alignment_json)
               AND typeof(json_extract(s.alignment_json, '$.source_start_ms')) = 'integer'
               AND typeof(json_extract(s.alignment_json, '$.source_end_ms')) = 'integer'
               AND json_extract(s.alignment_json, '$.source_start_ms') >= 0
               AND json_extract(s.alignment_json, '$.source_end_ms')
                   > json_extract(s.alignment_json, '$.source_start_ms')
               AND ABS(
                    (json_extract(s.alignment_json, '$.source_end_ms')
                     - json_extract(s.alignment_json, '$.source_start_ms'))
                    - s.duration_ms
               ) <= ?8",
            params![
                receipt.segment_id,
                expected_revision,
                receipt.reviewer,
                receipt.session_id,
                receipt.started_at_ms,
                receipt.played_ms,
                PLAYBACK_POLICY_VERSION,
                MAX_SOURCE_SPAN_DURATION_DELTA_MS,
            ],
        )?;
        Ok(changed == 1)
    }

    /// The raw writer: stores exactly what it is given, resolving nothing.
    ///
    /// For tests that must fabricate divergent worlds (a receipt at a dead revision, audio bytes
    /// that changed after the listen) and for the undo path carrying a reviewer's own evidence
    /// forward. Production surfaces go through [`Self::record_playback_receipt`]; minting a receipt
    /// from unresolved client claims through this is the exact bug the front door closed.
    pub(crate) fn record_playback_receipt_raw(&self, receipt: &PlaybackReceipt) -> AppResult<()> {
        if !is_canonical_audio_content_hash(&receipt.audio_content_hash) {
            return Err(AppError::Validation(
                "cannot record policy-3 playback evidence without a canonical server-derived decoded-PCM BLAKE3 hash"
                    .into(),
            ));
        }
        validate_playback_receipt_nonnegative_fields(receipt)?;
        if receipt.clip_duration_ms <= 0 {
            return Err(AppError::Validation(
                "cannot record policy-3 playback evidence without a positive clip duration".into(),
            ));
        }
        let (source_start_ms, source_end_ms) = match (receipt.source_start_ms, receipt.source_end_ms) {
            (Some(start), Some(end)) if start >= 0 && end > start => (start, end),
            _ => {
                return Err(AppError::Validation(
                    "cannot record policy-3 playback evidence without a canonical source span".into(),
                ));
            }
        };
        if !source_span_matches_duration(source_start_ms, source_end_ms, receipt.clip_duration_ms) {
            return Err(AppError::Validation(
                "cannot record policy-3 playback evidence whose source span disagrees with decoded clip duration"
                    .into(),
            ));
        }
        let coverage = (receipt.played_ms as f64 / receipt.clip_duration_ms as f64).min(1.0);
        self.conn.execute(
            "INSERT INTO playback_receipts
                (segment_id, segment_revision, audio_fingerprint, reviewer, session_id,
                 started_at_ms, played_ms, clip_duration_ms, coverage_ratio, policy_version,
                 source_start_ms, source_end_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                receipt.segment_id,
                receipt.segment_revision,
                receipt.audio_content_hash,
                receipt.reviewer,
                receipt.session_id,
                receipt.started_at_ms,
                receipt.played_ms,
                receipt.clip_duration_ms,
                coverage,
                PLAYBACK_POLICY_VERSION,
                source_start_ms,
                source_end_ms,
            ],
        )?;
        Ok(())
    }

    /// Is there sufficient renderer-reported canonical-media traversal for the CURRENT clip identity?
    ///
    /// Deliberately strict about identity, not just quantity:
    ///   * the receipt must name this segment AND the revision being decided — a correction changes
    ///     the text under judgement, so the previous listen does not carry over;
    ///   * it must name the decoded-PCM CONTENT HASH now on file, so a receipt cannot be replayed against a
    ///     different clip or survive the audio being swapped underneath it;
    ///   * coverage is cumulative media time, so paused, seeked or replayed traversal remains exact
    ///     and a download does not count at all.
    ///
    /// `reviewer` is part of the evidence's identity: renderer evidence is not transferable.
    ///
    /// Found by the hunt: matching on segment+revision+content-hash alone let reviewer A's full
    /// traversal evidence authorize reviewer B's blind verdict (a clip A encountered and skipped goes to B's queue
    /// with A's receipt still valid for it). `None` matches only anonymous (desktop-minted)
    /// receipts, never a named phone receipt, and vice versa: SQL `IS` treats NULL as its own
    /// identity.
    pub fn has_sufficient_playback_evidence(
        &self,
        segment_id: &str,
        revision: i64,
        content_hash: &str,
        reviewer: Option<&str>,
    ) -> AppResult<bool> {
        let Some((source_start_ms, source_end_ms)) = self.segment_source_span(segment_id)? else {
            return Ok(false);
        };
        has_sufficient_playback_evidence_on(
            &self.conn,
            segment_id,
            revision,
            content_hash,
            source_start_ms,
            source_end_ms,
            reviewer,
        )
    }
}
