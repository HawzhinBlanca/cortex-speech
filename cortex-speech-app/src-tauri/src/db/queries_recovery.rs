use super::*;

impl Database {
    pub fn get_segments(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        let mut query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments");
        if let Some(v) = verified {
            query.push_str(&format!(" WHERE verified = {}", if v { 1 } else { 0 }));
        }
        // `, id ASC` is a deterministic tiebreaker: created_at has 1s resolution, so a chunked file's
        // batch-inserted segments tie, and without a unique secondary key SQLite's tie order is
        // undefined — making JSON/JSONL/CSV/Parquet exports non-byte-reproducible across plan/VACUUM.
        query.push_str(" ORDER BY created_at DESC, id ASC");

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    /// Stream every segment through a callback, one row at a time, without materialising the corpus.
    ///
    /// External review 2026-08-06 P1.3, for the corpus-wide STATISTICS in `commands/dataset_analytics.rs`.
    /// Those are whole-corpus by definition — a training-grade breakdown that skipped rows would be
    /// wrong, not slow — so the fix is not a WHERE clause and it is emphatically NOT reimplementing the
    /// grading rule in SQL: `training_grade_for_segment` is the same function the EXPORT gates on, and a
    /// second SQL copy of it would let the dashboard and the export disagree about what is training-ready.
    /// That is the P1.2 drift bug wearing a different hat.
    ///
    /// So the row is what gets bounded, not the rule. Each caller folds into a small accumulator
    /// (counters, or a `(f64, f64)` per eligible clip) while the full record — transcript, alignment
    /// JSON, evidence JSON — lives only for the duration of one callback.
    ///
    /// Same ORDER BY as `get_segments`, so a fold that is order-sensitive sees exactly what the
    /// collect-then-iterate version saw.
    /// `verified` filters exactly like [`get_segments`], so a streaming caller sees the same rows in the
    /// same order as the collecting one it replaced.
    pub fn for_each_segment(&self, verified: Option<bool>, mut f: impl FnMut(SpeechSegment)) -> AppResult<()> {
        let where_sql = match verified {
            Some(true) => " WHERE verified = 1",
            Some(false) => " WHERE verified = 0",
            None => "",
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments{where_sql} ORDER BY created_at DESC, id ASC"
        ))?;
        let rows = stmt.query_map([], Self::map_row)?;
        for row in rows {
            f(row?);
        }
        Ok(())
    }

    /// The IDs of every pending (unverified) clip, in the same order `get_segments(Some(false))` returns.
    ///
    /// External review 2026-08-06 P1.3, for the couch-review queue. That queue must walk EVERY pending
    /// row — its `heldByOthers` / `skippedByYou` / `pendingTotal` counts depend on in-memory lease state,
    /// so no SQL aggregate can produce them — but it only needs the ID of a row it is merely COUNTING.
    /// The full record (transcript, alignment JSON, evidence JSON) is needed for the <= QUEUE_BATCH
    /// clips it actually hands out, and those are hydrated with `get_segments_by_ids`.
    ///
    /// Ordering is byte-identical to `get_segments` on purpose: this replaced a whole-row read, and a
    /// different ORDER BY would silently change WHICH clips a reviewer is handed.
    /// Ids the review queue may hand out.
    ///
    /// A clip whose draft is still a PLACEHOLDER is excluded. `api_decision` already refuses to
    /// verify `[...]` text, so serving one could only ever waste the reviewer's time — but the real
    /// cost is worse than that: if they type the transcript themselves instead, the clip is finished
    /// without the champion ever drafting it, so it permanently has no baseline and no CER can be
    /// measured against it. What the queue SERVES now agrees with what the decide path ACCEPTS.
    ///
    /// This matters because placeholders exist for a whole import: segments are created carrying
    /// `[Pending WSL 7B ASR]` and filled in afterwards at ~8.5 clips/minute. Normally the app is
    /// closed for imports and nobody can reach them; an INTERRUPTED import leaves them behind, which
    /// is exactly what happened on 2026-08-14 (36 orphaned placeholder rows).
    ///
    /// Matched in SQL by `placeholder_or_empty_transcript_sql`, which mirrors
    /// `quality::is_placeholder_transcript` — the authority on what a placeholder IS — so the two
    /// cannot disagree. It used to be an inline `[%]`-only test, which served the `n/a` and `null`
    /// drafts the authority rejects to a PAID reviewer (2026-08-25 audit M11).
    ///
    /// OLDEST FIRST, deliberately (2026-08-14). This was newest-first, which quietly buries the work
    /// the owner actually wants finished: after importing 27 hours of new podcast audio the queue
    /// would have served the NEW material first and left the 537 clips of the original corpus behind
    /// 6,823 newer ones. A review queue is FIFO — an import must go to the BACK of the line, so
    /// adding more audio can never delay finishing what is already in progress. (The desktop library
    /// view keeps newest-first; that is a browsing order, not a work order.)
    /// A clip whose AUDIO FILE IS GONE is never served either (2026-08-15). The reviewer cannot listen,
    /// so the only verdicts they can give are guesses about text they never heard — and this is a
    /// VERBATIM corpus, where an unheard "looks good" is worse than no decision at all.
    ///
    /// MEASURED that day: three staging folders under `SoraniVoice_PC_` had ceased to exist, taking the
    /// audio for 1,031 clips (7% of the library) with them. 536 of those were still pending, and because
    /// this queue is oldest-first they sat at the very HEAD of it — so every reviewer who opened a link
    /// was handed unplayable clips first and reported "the audio does not play". Nothing detected it:
    /// the rows are perfectly well-formed, and every gate in the sweep reads the database, not the disk.
    ///
    /// Existence is memoised PER DISTINCT PATH, not per row: segments are chunks of a recording, so a
    /// few hundred files back the whole library and the check costs a few hundred `stat` calls rather
    /// than one per pending row.
    pub fn pending_segment_ids(&self) -> AppResult<Vec<String>> {
        self.pending_segment_ids_for(None)
    }

    /// As above, but only clips in dialects this reviewer can actually judge. `None` = unrestricted.
    ///
    /// Owner instruction 2026-08-16: a reviewer who does not speak Hawleri must not be handed Hawleri.
    /// Judging a dialect you do not speak produces confident WRONG verdicts, and downstream those are
    /// indistinguishable from good ones — so this is a corpus-integrity filter, not a convenience.
    pub fn pending_segment_ids_for(&self, allowed_dialects: Option<&[String]>) -> AppResult<Vec<String>> {
        self.pending_segment_ids_focused(allowed_dialects, None)
    }

    /// `pending_segment_ids_for`, additionally narrowed to a voice-focus set when one is active.
    ///
    /// The focus is an ALLOW-LIST of segment ids (see `voice_focus.rs`): `None` is the full queue,
    /// `Some(set)` serves only clips in it. Applied after the dialect fence, never instead of it — a
    /// reviewer restricted to Sorani stays restricted even when the focus set spans both dialects.
    pub fn pending_segment_ids_focused(
        &self,
        allowed_dialects: Option<&[String]>,
        focus: Option<&std::collections::HashSet<String>>,
    ) -> AppResult<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, audio_path FROM speech_segments
             WHERE verified = 0
               AND NOT {}
             ORDER BY created_at ASC, id ASC",
            placeholder_or_empty_transcript_sql("raw_transcript")
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut ids = Vec::new();
        // Both checks are memoised per DISTINCT PATH: 13,797 clips come from 32 recordings, so this
        // costs a few dozen stat calls and dialect lookups rather than one per pending row.
        let mut servable: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for row in rows {
            let (id, audio_path) = row?;
            let ok = *servable.entry(audio_path.clone()).or_insert_with(|| {
                std::path::Path::new(&audio_path).is_file()
                    && crate::dialect::reviewer_may_judge(allowed_dialects, &audio_path)
            });
            if ok && focus.map_or(true, |set| set.contains(&id)) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// The backlog one background pass still has to process.
    ///
    /// External review 2026-08-06 P1.3. Each of these three passes used to call `get_segments(None)` —
    /// materialising the WHOLE library — and then `continue` past every row that was already done. The
    /// work list is a WHERE clause; reading the corpus to find it is the O(corpus) read the policy gate
    /// exists to retire.
    ///
    /// What bounds this is the BACKLOG, not a page size: these passes must process every unfinished row,
    /// so a `LIMIT` here would silently drop work rather than bound it. The bound is that a finished row
    /// is never read at all — after the first run the result is empty, where the old code still paid for
    /// the whole library every time.
    ///
    /// Ordering matches `get_segments` (created_at, then a unique tiebreak) so a capped or interrupted
    /// run resumes deterministically instead of picking an arbitrary SQLite row order.
    pub fn get_pending_segments(&self, work: PendingWork) -> AppResult<Vec<SpeechSegment>> {
        let where_sql = match work {
            // A SUPERSET of what `quality::is_placeholder_transcript` accepts, deliberately: SQL narrows
            // cheaply and Rust stays the single authority on what a placeholder IS. Widening in the
            // permissive direction can only cost an extra row that Rust then rejects; it can never drop a
            // real target. `db_tests::sql_placeholder_prefilter_is_a_superset_of_the_rust_predicate`
            // pins that relationship so the two cannot drift apart.
            PendingWork::Transcript => {
                "TRIM(COALESCE(raw_transcript, '')) = ''
                 OR TRIM(raw_transcript) LIKE '[%'
                 OR LENGTH(TRIM(raw_transcript)) <= 4"
            }
            PendingWork::CtcScore => "ctc_score IS NULL",
            PendingWork::SignalAnomaly => "signal_anomaly_score IS NULL",
        };
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments
              WHERE {where_sql}
              ORDER BY created_at ASC, audio_path ASC, id ASC"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    pub fn get_segments_page(
        &self,
        verified: Option<bool>,
        text_query: Option<&str>,
        sort: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> AppResult<SegmentsPage> {
        self.get_segments_page_focused(verified, text_query, sort, limit, cursor, None)
    }

    /// `get_segments_page`, additionally narrowed to a voice-focus allow-list when one is active.
    ///
    /// Same contract as `pending_segment_ids_focused` (the phone queue's narrowing): `None` is the
    /// full library, `Some(set)` serves only clips in it. The DESKTOP review queue reads through this
    /// — found 2026-08-20 when the owner, reviewing on desktop with a focus active, was still served
    /// the guests the focus existed to skip: the narrowing lived only on the couch path, and the
    /// desktop is a serving path too. The curate/library views keep calling the unfocused wrapper on
    /// purpose: the QUEUE narrows, the LIBRARY does not (voice_focus.rs).
    pub fn get_segments_page_focused(
        &self,
        verified: Option<bool>,
        text_query: Option<&str>,
        sort: &str,
        limit: usize,
        cursor: Option<&str>,
        focus: Option<&std::collections::HashSet<String>>,
    ) -> AppResult<SegmentsPage> {
        self.get_segments_page_scoped(SegmentPageQuery {
            verified,
            text_query,
            sort,
            limit,
            cursor,
            focus,
            escalation_only: false,
        })
    }

    /// Versioned, keyset-paginated escalation queue for the desktop review contract.
    ///
    /// Unlike the general library page, escalation rows carry their complete alignment/evidence
    /// payload because the inbox immediately authorizes clip-bounded playback from the returned row.
    /// The row and review revision still originate in one SQLite result row.
    pub fn get_escalation_review_page(
        &self,
        limit: usize,
        cursor: Option<&str>,
        focus: Option<&std::collections::HashSet<String>>,
    ) -> AppResult<SegmentsPage> {
        self.get_segments_page_scoped(SegmentPageQuery {
            verified: None,
            text_query: None,
            sort: "suspectFirst",
            limit,
            cursor,
            focus,
            escalation_only: true,
        })
    }

    pub(super) fn get_segments_page_scoped(&self, query: SegmentPageQuery<'_>) -> AppResult<SegmentsPage> {
        let SegmentPageQuery { verified, text_query, sort, limit, cursor, focus, escalation_only } = query;
        let limit = limit.clamp(1, 500);
        let sort = canonical_segment_sort(sort);
        let base_scope = segment_page_scope(verified, text_query, focus);
        let scope = if escalation_only { format!("escalation:{base_scope}") } else { base_scope };
        let decoded_cursor = cursor.map(decode_segment_cursor).transpose()?;
        if let Some(ref cursor) = decoded_cursor {
            if cursor.sort != sort || cursor.scope != scope {
                return Err(AppError::Validation(
                    "Segment page cursor does not match the active filter or sort".into(),
                ));
            }
        }
        let anchor_rowid = if let Some(ref cursor) = decoded_cursor {
            cursor.anchor_rowid
        } else {
            self.conn.query_row("SELECT COALESCE(MAX(rowid), 0) FROM speech_segments", [], |row| row.get(0))?
        };

        let mut where_parts: Vec<String> = vec!["rowid <= ?1".to_string()];
        let mut bind_values: Vec<Value> = vec![Value::Integer(anchor_rowid)];
        if let Some(v) = verified {
            bind_values.push(Value::Integer(if v { 1 } else { 0 }));
            where_parts.push(format!("verified = ?{}", bind_values.len()));
        }
        if let Some(raw_query) = text_query.map(str::trim).filter(|value| !value.is_empty()) {
            let match_query = to_fts5_match(&normalize_search_query(raw_query));
            if match_query.is_empty() {
                return Ok(SegmentsPage {
                    items: Vec::new(),
                    total: 0,
                    next_cursor: None,
                    revisions: std::collections::BTreeMap::new(),
                    focus_narrowed: focus.is_some(),
                });
            }
            let scoped_query =
                format!("{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})");
            bind_values.push(Value::Text(scoped_query));
            where_parts
                .push(format!("id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?{})", bind_values.len()));
        }
        if let Some(set) = focus {
            // One JSON-array bind, unpacked by json_each — the allow-list joins the SQL WHERE so the
            // COUNT, the keyset pages, and `total` all agree. Filtering rows after the query instead
            // would report the unfocused total and shrink every page by however many guests it held.
            let mut ids: Vec<&str> = set.iter().map(String::as_str).collect();
            ids.sort_unstable();
            let ids_json = serde_json::to_string(&ids)
                .map_err(|e| AppError::Validation(format!("Could not encode voice-focus ids: {e}")))?;
            bind_values.push(Value::Text(ids_json));
            where_parts.push(format!("id IN (SELECT value FROM json_each(?{}))", bind_values.len()));
        }
        if escalation_only {
            where_parts.push("escalated = 1".to_string());
            where_parts.push("(human_decision IS NULL OR human_decision = '')".to_string());
        }
        let total = if let Some(ref cursor) = decoded_cursor {
            cursor.total
        } else {
            let count_where_sql = format!(" WHERE {}", where_parts.join(" AND "));
            let count_sql = format!("SELECT COUNT(*) FROM speech_segments{count_where_sql}");
            let total: i64 =
                self.conn.query_row(&count_sql, rusqlite::params_from_iter(bind_values.iter()), |row| row.get(0))?;
            usize::try_from(total).map_err(|_| AppError::Validation("Segment count is out of range".into()))?
        };

        let created_expr = "COALESCE(datetime(created_at), '')";
        let confidence_expr = "COALESCE(confidence, 1.0)";
        let active_expr = "ABS(((1.0 - COALESCE(confidence, 0.5)) + (0.1 * -COALESCE(ctc_score, -5.0))) - 0.35)";
        let poor_expr = format!(
            "CASE WHEN COALESCE(snr_db, 99.0) < {} OR COALESCE(clipping_ratio, 0.0) > {} THEN 0 ELSE 1 END",
            crate::quality::POOR_AUDIO_SNR_DB,
            crate::quality::POOR_AUDIO_CLIPPING_RATIO,
        );
        let order_sql = match sort {
            "oldest" => format!("{created_expr} ASC, id ASC"),
            "duration" => "duration_ms DESC, id ASC".to_string(),
            "verified" => format!("verified DESC, {created_expr} DESC, id ASC"),
            "confidence" => format!("{confidence_expr} ASC, id ASC"),
            "activeLearning" => format!("{active_expr} ASC, id ASC"),
            "suspectFirst" => format!(
                "escalated DESC, {poor_expr} ASC, COALESCE(agreement_score, 0.5) ASC, {created_expr} DESC, id ASC"
            ),
            _ => format!("{created_expr} DESC, id ASC"),
        };

        if let Some(cursor) = decoded_cursor.as_ref() {
            let key = &cursor.last;
            let mut bind = |value: Value| {
                bind_values.push(value);
                bind_values.len()
            };
            let keyset = match sort {
                "oldest" => {
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("({created_expr} > COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))")
                }
                "duration" => {
                    let d1 = bind(Value::Integer(key.duration_ms));
                    let d2 = bind(Value::Integer(key.duration_ms));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("(duration_ms < ?{d1} OR (duration_ms = ?{d2} AND id > ?{id}))")
                }
                "verified" => {
                    let v1 = bind(Value::Integer(i64::from(key.verified)));
                    let v2 = bind(Value::Integer(i64::from(key.verified)));
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("(verified < ?{v1} OR (verified = ?{v2} AND ({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))))")
                }
                "confidence" => {
                    let c1 = bind(Value::Real(key.confidence));
                    let c2 = bind(Value::Real(key.confidence));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("({confidence_expr} > ?{c1} OR ({confidence_expr} = ?{c2} AND id > ?{id}))")
                }
                "activeLearning" => {
                    let a1 = bind(Value::Real(key.active_learning));
                    let a2 = bind(Value::Real(key.active_learning));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("({active_expr} > ?{a1} OR ({active_expr} = ?{a2} AND id > ?{id}))")
                }
                "suspectFirst" => {
                    let e1 = bind(Value::Integer(i64::from(key.escalated)));
                    let e2 = bind(Value::Integer(i64::from(key.escalated)));
                    let p1 = bind(Value::Integer(i64::from(!key.poor_audio)));
                    let p2 = bind(Value::Integer(i64::from(!key.poor_audio)));
                    let a1 = bind(Value::Real(key.agreement));
                    let a2 = bind(Value::Real(key.agreement));
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("(escalated < ?{e1} OR (escalated = ?{e2} AND ({poor_expr} > ?{p1} OR ({poor_expr} = ?{p2} AND (COALESCE(agreement_score, 0.5) > ?{a1} OR (COALESCE(agreement_score, 0.5) = ?{a2} AND ({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))))))))")
                }
                _ => {
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!("({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))")
                }
            };
            where_parts.push(keyset);
        }

        let where_sql = format!(" WHERE {}", where_parts.join(" AND "));
        bind_values.push(Value::Integer(limit as i64));
        let limit_idx = bind_values.len();
        let select_columns = if escalation_only { SEGMENT_SELECT_COLUMNS } else { SEGMENT_LIST_SELECT_COLUMNS };
        let page_sql = format!(
            "SELECT {select_columns}, review_revision FROM speech_segments{where_sql} ORDER BY {order_sql} LIMIT ?{limit_idx}"
        );
        let mut stmt = self.conn.prepare(&page_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind_values.iter()), |row| {
            Ok((Self::map_row(row)?, row.get::<_, i64>(37)?))
        })?;
        let mut items = Vec::new();
        let mut revisions = std::collections::BTreeMap::new();
        for row in rows {
            let (segment, revision) = row?;
            revisions.insert(segment.id.clone(), revision);
            items.push(segment);
        }
        let emitted = decoded_cursor.as_ref().map_or(0, |cursor| cursor.emitted) + items.len();
        let next_cursor = if emitted < total && !items.is_empty() {
            let Some(last) = items.last() else { unreachable!("positive limit with full page") };
            let active_learning =
                (((1.0 - last.confidence.unwrap_or(0.5)) + (0.1 * -last.ctc_score.unwrap_or(-5.0))) - 0.35).abs();
            let poor_audio = last.snr_db.is_some_and(|v| v < crate::quality::POOR_AUDIO_SNR_DB)
                || last.clipping_ratio.is_some_and(|v| v > crate::quality::POOR_AUDIO_CLIPPING_RATIO);
            Some(encode_segment_cursor(&SegmentPageCursor {
                version: 1,
                sort: sort.to_string(),
                scope,
                anchor_rowid,
                total,
                emitted,
                last: SegmentPageKey {
                    id: last.id.clone(),
                    created_at: last.created_at.clone().unwrap_or_default(),
                    duration_ms: last.duration_ms,
                    verified: last.verified,
                    confidence: last.confidence.unwrap_or(1.0),
                    active_learning,
                    escalated: last.escalated,
                    poor_audio,
                    agreement: last.agreement_score.unwrap_or(0.5),
                },
            })?)
        } else {
            None
        };
        Ok(SegmentsPage { items, total, next_cursor, revisions, focus_narrowed: focus.is_some() })
    }

    /// Lightweight batch scope: return only ids plus the transcript needed for the optional content
    /// gate. This lets whole-filter actions remain whole-filter actions without hydrating every row.
    pub fn get_segment_ids_for_view(
        &self,
        verified: Option<bool>,
        text_query: Option<&str>,
        transcript_state: &str,
    ) -> AppResult<Vec<String>> {
        let mut where_parts: Vec<String> = Vec::new();
        let mut bind_values: Vec<Value> = Vec::new();
        if let Some(v) = verified {
            bind_values.push(Value::Integer(i64::from(v)));
            where_parts.push(format!("verified = ?{}", bind_values.len()));
        }
        if let Some(raw_query) = text_query.map(str::trim).filter(|value| !value.is_empty()) {
            let match_query = to_fts5_match(&normalize_search_query(raw_query));
            if match_query.is_empty() {
                return Ok(Vec::new());
            }
            bind_values.push(Value::Text(format!(
                "{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})"
            )));
            where_parts
                .push(format!("id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?{})", bind_values.len()));
        }
        let where_sql =
            if where_parts.is_empty() { String::new() } else { format!(" WHERE {}", where_parts.join(" AND ")) };
        let sql = format!("SELECT id, raw_transcript FROM speech_segments{where_sql} ORDER BY id ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind_values.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut ids = Vec::new();
        for row in rows {
            let (id, transcript) = row?;
            let placeholder = crate::quality::is_placeholder_transcript(&transcript);
            if transcript_state == "any"
                || (transcript_state == "real" && !placeholder)
                || (transcript_state == "missing" && placeholder)
            {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    pub fn get_signal_anomaly_segments(&self, limit: usize) -> AppResult<Vec<SpeechSegment>> {
        let sql = format!(
            "SELECT {SEGMENT_LIST_SELECT_COLUMNS} FROM speech_segments
             WHERE signal_anomaly_score IS NOT NULL
             ORDER BY signal_anomaly_score DESC, id ASC LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([limit.clamp(1, 500) as i64], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    /// M2.5: Return segments ordered by suspect-first priority for ReviewInbox.
    /// Jury escalated segments first, then low-confidence (suspicious) segments, then chronological.
    pub fn get_segments_suspect_first(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        let mut query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments");
        if let Some(v) = verified {
            query.push_str(&format!(" WHERE verified = {}", if v { 1 } else { 0 }));
        }
        // Priority: escalated (jury doubts) first, then low agent confidence (suspicious), then chronological.
        query.push_str(&format!(" ORDER BY {}", *SUSPECT_FIRST_ORDER));

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    pub fn search_segments(&self, text: &str) -> AppResult<Vec<SpeechSegment>> {
        let match_query = to_fts5_match(&normalize_search_query(text));
        // Whitespace-only / empty input is an empty result, not an FTS5 `MATCH ""` error.
        if match_query.is_empty() {
            return Ok(Vec::new());
        }
        // Round-23 #7: the segments_fts table also indexes `audio_path`, so a bare `MATCH ?` matches the
        // query against the FILE PATH too — a token that appears only in a folder/file name returned
        // false-positive segments whose transcript did not contain it. Restrict the match to the
        // transcript columns with an FTS5 column filter so only transcript content is searched.
        let scoped_query = format!("{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})");
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS}
             FROM speech_segments
             WHERE id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?1)
             ORDER BY created_at DESC, id ASC"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![scoped_query], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    /// Batch-fetch segments by a list of IDs using a single `WHERE id IN (...)` query.
    /// Dramatically faster than N individual `get_segment_by_id` calls for delete/undo
    /// history snapshots on large selections.
    pub fn get_segments_by_ids(&self, ids: &[String]) -> AppResult<Vec<SpeechSegment>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQLite caps bound parameters per statement (SQLITE_MAX_VARIABLE_NUMBER — only 999 on older
        // builds). A large selection (delete/undo of thousands of segments) would overflow a single
        // IN(?,?,…) and fail with "too many SQL variables", so fetch in bounded chunks and re-impose
        // the global ordering afterwards (per-chunk ORDER BY doesn't compose across chunks).
        const CHUNK: usize = 500;
        let mut segments: Vec<SpeechSegment> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            // Build a parameterised placeholder list: (?1,?2,...?N)
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let query = format!(
                "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&query)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), Self::map_row)?;
            for row in rows {
                segments.push(row?);
            }
        }
        // Match the single-query contract: created_at DESC (newest first), then id ASC. None sorts
        // last under DESC, mirroring SQLite ordering NULLs after non-NULLs in a descending sort.
        segments.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(segments)
    }

    /// Couch Review variant of [`Self::get_segments_by_ids`]: each row and its revision come from the
    /// same SQLite result row. Fetching the revision afterwards could stamp an older transcript with a
    /// newer revision and let a decision against text the reviewer never saw pass its CAS fence.
    pub fn get_segments_by_ids_with_revisions(&self, ids: &[String]) -> AppResult<Vec<(SpeechSegment, i64)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        const CHUNK: usize = 500;
        let mut segments: Vec<(SpeechSegment, i64)> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let query = format!(
                "SELECT {SEGMENT_SELECT_COLUMNS}, review_revision FROM speech_segments WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&query)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((Self::map_row(row)?, row.get(37)?)))?;
            for row in rows {
                segments.push(row?);
            }
        }
        segments.sort_by(|a, b| b.0.created_at.cmp(&a.0.created_at).then_with(|| a.0.id.cmp(&b.0.id)));
        Ok(segments)
    }

    pub fn rename_speaker(&self, old_id: &str, new_id: &str) -> AppResult<usize> {
        let count = self.conn.execute(
            "UPDATE speech_segments SET speaker_id = ?2, updated_at = datetime('now') WHERE speaker_id = ?1",
            params![old_id, new_id],
        )?;
        self.track_write()?;
        Ok(count)
    }

    pub fn integrity_check(&self) -> AppResult<String> {
        let result: String = self.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(result)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// The on-disk path this connection was opened from (or `":memory:"`). Used by commands that need
    /// to open a SECOND, dedicated connection so they can release the global AppState db Mutex before a
    /// long network call (e.g. cloud jury T2) — holding it across the round-trip would freeze every
    /// other DB-touching command app-wide.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn info(&self) -> AppResult<serde_json::Value> {
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let journal_mode: String = self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let segment_count: i64 = self.conn.query_row("SELECT count(*) FROM speech_segments", [], |r| r.get(0))?;
        Ok(serde_json::json!({
            "path": self.path,
            "sizeBytes": size,
            "journalMode": journal_mode,
            "segmentCount": segment_count,
        }))
    }

    pub fn segment_count(&self) -> AppResult<i64> {
        let count: i64 = self.conn.query_row("SELECT count(*) FROM speech_segments", [], |r| r.get(0))?;
        Ok(count)
    }

    /// Pages copied per backup step, and the pause between steps. See [`Self::backup`].
    pub(super) const BACKUP_PAGES_PER_STEP: std::os::raw::c_int = 4096;
    pub(super) const BACKUP_STEP_PAUSE: std::time::Duration = std::time::Duration::from_millis(1);

    /// SQLite online backup — safe against a live database, unlike a file copy.
    ///
    /// The pacing is load-bearing and was pathological. It used to be `(5, 250ms)` — the literal
    /// rusqlite doc example — which copies 5 pages, sleeps a quarter second, and repeats. At 4 KB
    /// pages that is 80 KB/s, so the 84 MB library took ~21,600 pages / 5 × 250 ms = **18 minutes**.
    ///
    /// MEASURED 2026-08-17. Three consequences, none of them obvious from this function:
    ///   * `take_snapshot` runs SYNCHRONOUSLY on the startup path, so every launch held the review
    ///     port shut for ~16 minutes. That is the entire cold start — the watchdog's startup grace
    ///     was raised to 45 minutes twice to accommodate it, treating the symptom both times.
    ///   * The periodic snapshot timer is 10 minutes, i.e. SHORTER than one snapshot took, so a copy
    ///     was essentially always in flight against the database reviewers were writing to.
    ///   * A backup restarts from scratch when the source is written mid-copy, so the slower it is,
    ///     the likelier it is to never finish on a busy library.
    ///
    /// 4096 pages (~16 MB) per step holds the source lock for a few milliseconds at a time, which is
    /// what stepping is actually for, and finishes an 84 MB database in well under a second.
    pub fn backup<P: AsRef<Path>>(&self, dest: P) -> AppResult<()> {
        let mut dest_conn = Connection::open(dest.as_ref())?;
        let backup = backup::Backup::new(&self.conn, &mut dest_conn)?;
        backup.run_to_completion(Self::BACKUP_PAGES_PER_STEP, Self::BACKUP_STEP_PAUSE, None)?;
        Ok(())
    }

    /// Fully validate, copy, migrate, and integrity-check a restore source in isolation. The returned
    /// in-memory database is safe to publish; this phase never touches the live connection. Named
    /// restore uses the split explicitly so its durable cross-file barrier can be written immediately
    /// before (not before source validation, and not after) the live page swap.
    pub(crate) fn stage_restore_source<P: AsRef<Path>>(src: P) -> AppResult<Self> {
        let src_conn = Self::open_immutable_connection(src.as_ref())?;
        // Validate the complete, description-bound history before overwriting one live page. A lone
        // high version row is not evidence that its 57 predecessors ran, and restore must not discover
        // that only after the healthy live database has already been replaced.
        crate::migrations::validate_applied_history(&src_conn)?;

        // Restore is two-phase. First copy the source into an isolated in-memory database, run the
        // exact current startup/migration path there, and prove the resulting HEAD database healthy.
        // Only then copy it over the live connection. Previously an old-but-drifted snapshot could
        // overwrite the healthy live pages and fail its pending migration afterwards, returning Err
        // with the library already clobbered.
        let mut staged = Database::open(":memory:")?;
        {
            let backup = backup::Backup::new(&src_conn, &mut staged.conn)?;
            backup.run_to_completion(Self::BACKUP_PAGES_PER_STEP, Self::BACKUP_STEP_PAUSE, None)?;
        }
        // Run integrity on the isolated copy, not the frozen read-only source. SQLite's FTS5
        // integrity command may use temporary writes and can report "attempt to write a readonly
        // database" even when the source bytes are healthy; the copied pages are identical and this
        // remains strictly before any live mutation.
        let source_integrity: String = staged.conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if source_integrity.trim() != "ok" {
            return Err(AppError::Other(format!("snapshot database failed its integrity check: {source_integrity}")));
        }
        staged.initialize()?;

        let staged_quick: String = staged.conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if staged_quick.trim() != "ok" {
            return Err(AppError::Other(format!("staged restore failed quick_check: {staged_quick}")));
        }
        let staged_integrity: String = staged.conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if staged_integrity.trim() != "ok" {
            return Err(AppError::Other(format!("staged restore failed integrity_check: {staged_integrity}")));
        }
        let foreign_key_violations: i64 =
            staged.conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
        if foreign_key_violations != 0 {
            return Err(AppError::Other(format!(
                "staged restore has {foreign_key_violations} foreign-key violation(s)"
            )));
        }
        let staged_version = crate::migrations::validate_applied_history(&staged.conn)?;
        let expected_version = crate::migrations::max_supported_version();
        if staged_version != expected_version {
            return Err(AppError::Other(format!(
                "staged restore stopped at schema v{staged_version}; this build requires v{expected_version}"
            )));
        }

        Ok(staged)
    }

    /// Publish a source already proven by [`Self::stage_restore_source`]. SQLite's backup API holds
    /// the destination transaction until completion, so a copy error rolls the destination back
    /// instead of exposing a partially-restored database.
    pub(crate) fn commit_staged_restore(&mut self, staged: &Database) -> AppResult<()> {
        let backup = backup::Backup::new(&staged.conn, &mut self.conn)?;
        backup.run_to_completion(Self::BACKUP_PAGES_PER_STEP, Self::BACKUP_STEP_PAUSE, None)?;
        Ok(())
    }

    pub fn restore<P: AsRef<Path>>(&mut self, src: P) -> AppResult<()> {
        let staged = Self::stage_restore_source(src)?;
        self.commit_staged_restore(&staged)
    }

    pub fn vacuum(&self) -> AppResult<()> {
        // SQLite VACUUM cannot run inside a transaction — it commits any pending work and runs
        // standalone — so the VACUUM and its compensating FTS rebuild below CANNOT be wrapped in one
        // atomic statement. VACUUM renumbers speech_segments' implicit rowids, desyncing the
        // external-content FTS index (search would return unrelated rows). Rebuild it immediately.
        self.conn.execute("VACUUM", [])?;
        // If the rebuild fails the index is left stale, but only until the next launch: initialize()
        // unconditionally rebuilds segments_fts on every startup. Surface that so a rebuild failure is
        // an actionable "restart repairs search", not a cryptic error over a silently-wrong index.
        self.conn.execute("INSERT INTO segments_fts(segments_fts) VALUES('rebuild')", []).map_err(|e| {
            AppError::Other(format!(
                "VACUUM completed but rebuilding the search index failed: {e}. Search may return stale \
                 results until you restart the app, which rebuilds the index automatically."
            ))
        })?;
        Ok(())
    }

    pub fn wal_checkpoint(&self) -> AppResult<()> {
        self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    /// P3.3: audio durability — segments reference their source audio by absolute path in place, so a
    /// file the owner moves/renames over months of use silently breaks playback, re-transcription, and
    /// the jury's source-reference guard. Report which distinct audio files are now missing.
    pub fn audio_health(&self) -> AppResult<AudioHealth> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT audio_path FROM speech_segments")?;
        let paths: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
        let total_files = paths.len();
        let mut missing_paths: Vec<String> = paths.into_iter().filter(|p| !Path::new(p).exists()).collect();
        missing_paths.sort();
        Ok(AudioHealth { total_files, missing_files: missing_paths.len(), missing_paths })
    }

    /// P3.3: relink missing source audio by basename — for each missing `audio_path`, if a file with the
    /// same file name exists under `search_dir`, repoint every segment on that old path to the found one.
    /// Basename match (speech_segments store no content hash to verify against); the owner points at the
    /// folder they moved the audio to. Returns how many distinct paths were relinked + how many remain.
    ///
    /// AMBIGUITY GUARD: if two DISTINCT missing source paths share a basename (e.g. `interview.wav`
    /// imported from two different folders), a single found `interview.wav` cannot be known to be the
    /// right one for both — blindly repointing both would serve the WRONG audio for one recording on
    /// playback/re-transcription. Such colliding paths are left missing (and warned), never guessed.
    pub fn relink_audio(&self, search_dir: &Path) -> AppResult<RelinkResult> {
        let missing = self.audio_health()?.missing_paths;
        // Count distinct missing paths per basename so we can refuse ambiguous relinks.
        let mut basename_counts: std::collections::HashMap<std::ffi::OsString, usize> =
            std::collections::HashMap::new();
        for old in &missing {
            if let Some(name) = Path::new(old).file_name() {
                *basename_counts.entry(name.to_os_string()).or_insert(0) += 1;
            }
        }
        let mut relinked = 0usize;
        for old in &missing {
            let Some(name) = Path::new(old).file_name() else { continue };
            if basename_counts.get(name).copied().unwrap_or(0) > 1 {
                tracing::warn!(
                    "relink: '{}' shares its filename with another missing source — skipped (ambiguous, would risk the wrong audio)",
                    old
                );
                continue;
            }
            let candidate = search_dir.join(name);
            if candidate.is_file() {
                let new_path = candidate.to_string_lossy().to_string();
                // Second ambiguity guard: the collision check above only covers basenames shared among
                // MISSING paths. If the candidate file is already OWNED by another library entry (a
                // still-present segment whose recording happens to share the name), repointing would
                // alias this missing recording onto THAT recording's audio — transcript/audio
                // mispairing, the exact wrong-audio hazard this function refuses to guess about.
                let owned: i64 = self.conn.query_row(
                    "SELECT COUNT(*) FROM speech_segments WHERE audio_path = ?1",
                    params![new_path],
                    |r| r.get(0),
                )?;
                if owned > 0 {
                    tracing::warn!(
                        "relink: '{}' matches '{}', which another library recording already owns — skipped \
                         (ambiguous, would serve the wrong audio)",
                        old,
                        new_path
                    );
                    continue;
                }
                let n = self.conn.execute(
                    "UPDATE speech_segments SET audio_path = ?2, updated_at = datetime('now') WHERE audio_path = ?1",
                    params![old, new_path],
                )?;
                // Carry the SOURCE-KEYED tables with the move. Both are joined to segments by
                // audio_path, so relinking only `speech_segments` orphans them at the old key:
                //
                //  * `source_audio_provenance` (v54) then reports the recording as UNCLAIMED, and a
                //    training pack and dataset card would describe neural-separated, re-concatenated
                //    audio as an original field recording — precisely the lie that table exists to
                //    prevent, reintroduced by a file move. Found by adversarial verification
                //    2026-08-17.
                //  * `source_transcripts` would silently lose its whole-file reference transcripts,
                //    forcing a re-fetch that the cache was built to avoid.
                //
                // Not fatal: a relink whose provenance carry fails must still relink the audio (the
                // clips are otherwise unplayable), so the failure warns rather than aborting.
                for (table, what) in [
                    ("source_audio_provenance", "processing provenance"),
                    ("source_transcripts", "reference transcripts"),
                ] {
                    if let Err(e) = self.conn.execute(
                        &format!("UPDATE OR IGNORE {table} SET audio_path = ?2 WHERE audio_path = ?1"),
                        params![old, new_path],
                    ) {
                        tracing::warn!("relink: {what} for '{old}' could not follow the move to '{new_path}': {e}");
                    }
                }
                if n > 0 {
                    relinked += 1;
                }
            }
        }
        self.track_write()?;
        Ok(RelinkResult { relinked, still_missing: self.audio_health()?.missing_files })
    }
}
