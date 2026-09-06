use super::*;

struct PrivateRestoreSourceBundle {
    root: PathBuf,
    database: PathBuf,
}

/// One source move whose exact decoded-audio authority has been proven before publication.
///
/// The lease is intentionally retained until SQLite commits. On the supported Windows workstation
/// it prevents the candidate file or any parent directory from being replaced between the decoded
/// PCM hash and the path update that makes those bytes authoritative for every segment.
struct RelinkAudioPlan {
    old_path: String,
    new_path: String,
    expected_pcm_hash: String,
    segment_count: usize,
    _candidate_lease: crate::media::ImportMediaSourceLease,
}

impl Drop for PrivateRestoreSourceBundle {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("could not remove private restore-source bundle {}: {error}", self.root.display());
            }
        }
    }
}

fn sqlite_sidecar_path(database: &Path, suffix: &str) -> PathBuf {
    let mut name = database.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn restore_source_file_identity(path: &Path) -> AppResult<Option<(u64, [u8; 32])>> {
    use std::io::Read as _;

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::Other(format!("restore source {} must be a regular non-symlink file", path.display())));
    }
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(Some((metadata.len(), hash.finalize().into())))
}

fn capture_private_restore_source(source: &Path) -> AppResult<PrivateRestoreSourceBundle> {
    let file_name = source
        .file_name()
        .ok_or_else(|| AppError::Other(format!("restore source {} has no file name", source.display())))?;
    let root = std::env::temp_dir().join(format!("cortex-restore-source-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let bundle = PrivateRestoreSourceBundle { database: root.join(file_name), root };

    // SHM is a rebuildable index, not authority. A rollback journal, however, may require a write to
    // recover and cannot be interpreted safely by the read-only source path, so fail closed instead of
    // guessing whether it is hot. Main + WAL are copied and identity-checked as one stable set.
    let rollback_journal = sqlite_sidecar_path(source, "-journal");
    if restore_source_file_identity(&rollback_journal)?.is_some() {
        return Err(AppError::Other(format!(
            "restore source {} has a rollback journal; close its writer cleanly and create a stable backup before restoring",
            source.display()
        )));
    }
    let source_files = [source.to_path_buf(), sqlite_sidecar_path(source, "-wal")];
    let before = source_files.iter().map(|path| restore_source_file_identity(path)).collect::<AppResult<Vec<_>>>()?;
    if before[0].is_none() {
        return Err(AppError::Other(format!("restore source {} does not exist", source.display())));
    }
    for (index, source_path) in source_files.iter().enumerate() {
        let Some((expected_len, expected_hash)) = before[index] else { continue };
        let destination =
            if index == 0 { bundle.database.clone() } else { sqlite_sidecar_path(&bundle.database, "-wal") };
        std::fs::copy(source_path, &destination)?;
        let copied = restore_source_file_identity(&destination)?.ok_or_else(|| {
            AppError::Other(format!("private restore source copy {} disappeared", destination.display()))
        })?;
        if copied != (expected_len, expected_hash) {
            return Err(AppError::Other(format!(
                "private restore source copy {} does not match {}",
                destination.display(),
                source_path.display()
            )));
        }
    }
    let after = source_files.iter().map(|path| restore_source_file_identity(path)).collect::<AppResult<Vec<_>>>()?;
    if after != before || restore_source_file_identity(&rollback_journal)?.is_some() {
        return Err(AppError::Other(
            "restore source SQLite main/WAL generation changed during private capture; retry after its writer stops"
                .to_string(),
        ));
    }
    Ok(bundle)
}

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
                    format!(
                        "({created_expr} > COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))"
                    )
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
                    format!(
                        "(verified < ?{v1} OR (verified = ?{v2} AND ({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))))"
                    )
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
                    format!(
                        "(escalated < ?{e1} OR (escalated = ?{e2} AND ({poor_expr} > ?{p1} OR ({poor_expr} = ?{p2} AND (COALESCE(agreement_score, 0.5) > ?{a1} OR (COALESCE(agreement_score, 0.5) = ?{a2} AND ({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))))))))"
                    )
                }
                _ => {
                    let t1 = bind(Value::Text(key.created_at.clone()));
                    let t2 = bind(Value::Text(key.created_at.clone()));
                    let id = bind(Value::Text(key.id.clone()));
                    format!(
                        "({created_expr} < COALESCE(datetime(?{t1}), '') OR ({created_expr} = COALESCE(datetime(?{t2}), '') AND id > ?{id}))"
                    )
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
        // `file:lamo_016604` searches file names only; otherwise transcript content, plus any token
        // that is unmistakably a file name (ASCII with a digit and a `_`/`-`/`.`, e.g. `lamo_016604`).
        let (file_only, body) = match text.trim().strip_prefix("file:") {
            Some(rest) => (true, rest.trim()),
            None => (false, text.trim()),
        };
        let match_query = to_fts5_match(&normalize_search_query(body));
        // Whitespace-only / empty input is an empty result, not an FTS5 `MATCH ""` error.
        if match_query.is_empty() {
            return Ok(Vec::new());
        }
        // Round-23 #7: the segments_fts table also indexes `audio_path`, so a bare `MATCH ?` matches the
        // query against the FILE PATH too — a token that appears only in a folder/file name returned
        // false-positive segments whose transcript did not contain it. Restrict the match to the
        // transcript columns with an FTS5 column filter so only transcript content is searched.
        // Owner 2026-09-06 ("search didn't find it"): a file NAME is still the one thing a person can
        // read off a report, so file-shaped tokens additionally match the `audio_path` column — never
        // plain words such as a folder name, which is what Round-23 #7 forbade.
        let scoped_query = format!("{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})");
        let file_tokens: Vec<String> = body
            .split_whitespace()
            .filter(|token| crate::db::looks_like_file_token(token))
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect();
        let path_query = if file_only {
            Some(format!("{{audio_path}} : ({match_query})"))
        } else if file_tokens.is_empty() {
            None
        } else {
            Some(format!("{{audio_path}} : ({})", file_tokens.join(" ")))
        };
        // Bind exactly the MATCH strings the clause names: rusqlite refuses a spare parameter.
        let (clause, bindings): (&str, Vec<String>) = match (path_query, file_only) {
            (Some(path), true) => ("id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?1)", vec![path]),
            (Some(path), false) => (
                "(id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?1)
                  OR id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?2))",
                vec![scoped_query, path],
            ),
            (None, _) => ("id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?1)", vec![scoped_query]),
        };
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS}
             FROM speech_segments
             WHERE {clause}
             ORDER BY created_at DESC, id ASC"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bindings.iter()), Self::map_row)?;
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

    /// Atomically rename exactly the speaker inventory the caller observed and return the minimal
    /// inverse for history. The operation rechecks source/target counts after acquiring SQLite's
    /// writer lock, so even a same-count membership swap cannot produce mismatched undo evidence.
    pub fn rename_speaker_with_inventory(
        &self,
        old_id: Option<&str>,
        new_id: &str,
        expected_source_count: usize,
        expected_target_count: usize,
    ) -> AppResult<Option<Vec<SpeakerAssignmentChange>>> {
        self.conn.execute("SAVEPOINT speaker_rename", [])?;
        let result: AppResult<Vec<SpeakerAssignmentChange>> = (|| {
            let (source_count, target_count) = self.speaker_counts(old_id, new_id)?;
            if source_count != expected_source_count || target_count != expected_target_count {
                return Err(AppError::Validation("speaker inventory changed before rename".into()));
            }
            let source_segments = self.get_segments_by_speaker_id(old_id)?;
            let source_ids: Vec<String> = source_segments.into_iter().map(|segment| segment.id).collect();
            let changes = self.assign_speaker_batch_atomic(&source_ids, Some(new_id))?;
            let (source_after, target_after) = self.speaker_counts(old_id, new_id)?;
            if source_after != 0 || target_after != expected_source_count + expected_target_count {
                return Err(AppError::Validation("speaker inventory changed during rename".into()));
            }
            Ok(changes)
        })();

        match result {
            Ok(changes) => {
                self.release_savepoint("speaker_rename")?;
                Ok(Some(changes))
            }
            Err(AppError::Validation(_)) => {
                self.cleanup_savepoint_after_error("speaker_rename");
                Ok(None)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("speaker_rename");
                Err(error)
            }
        }
    }

    pub fn speaker_counts(&self, old_id: Option<&str>, new_id: &str) -> AppResult<(usize, usize)> {
        let source_count = self.conn.query_row(
            "SELECT COUNT(*) FROM speech_segments
             WHERE ((?1 IS NULL AND speaker_id IS NULL) OR (?1 IS NOT NULL AND speaker_id = ?1))",
            params![old_id],
            |row| row.get::<_, i64>(0),
        )?;
        let target_count = self.conn.query_row(
            "SELECT COUNT(*) FROM speech_segments WHERE speaker_id = ?1",
            params![new_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok((source_count as usize, target_count as usize))
    }

    pub fn get_segments_by_speaker_id(&self, speaker_id: Option<&str>) -> AppResult<Vec<SpeechSegment>> {
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments
             WHERE ((?1 IS NULL AND speaker_id IS NULL) OR (?1 IS NOT NULL AND speaker_id = ?1))
             ORDER BY id ASC"
        );
        let mut statement = self.conn.prepare(&query)?;
        let rows = statement.query_map(params![speaker_id], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
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
    ///
    /// The source copy is deliberately WAL-aware. `immutable=1` is correct only after a caller has
    /// independently proved that the main file is the whole SQLite authority; a user-selected bare
    /// backup can still have committed rows in its `-wal`, and ignoring them would validate and
    /// publish an older, merely healthy generation.
    pub(crate) fn stage_restore_source<P: AsRef<Path>>(src: P) -> AppResult<Self> {
        Self::stage_restore_source_with_original_evidence(src).map(|(staged, _, _)| staged)
    }

    /// Stage one WAL-consistent source generation while retaining the policy evidence that existed
    /// *before* migrations ran. A restore may legitimately migrate an old database to this binary's
    /// current schema, but that must never make an old policy-bearing snapshot look as though it had
    /// durable hidden-key authority when it was created.
    ///
    /// The returned tuple is `(staged_database, original_schema_version,
    /// original_max_review_event_id)`. Both evidence values and the final staged database come from
    /// the same private SQLite snapshot, so no second open can mix generations.
    pub(crate) fn stage_restore_source_with_original_evidence<P: AsRef<Path>>(src: P) -> AppResult<(Self, i64, i64)> {
        let private_source = capture_private_restore_source(src.as_ref())?;
        let staged = Self::open_detached_read_snapshot(private_source.database.to_string_lossy().as_ref())?;
        // Validate the complete, description-bound history before overwriting one live page. A lone
        // high version row is not evidence that its 57 predecessors ran, and restore must not discover
        // that only after the healthy live database has already been replaced.
        let original_schema_version = crate::migrations::validate_applied_history(&staged.conn)?;
        // review_events was introduced by migration 45. Older, otherwise valid databases have no
        // table to query; zero is the only possible event authority for them.
        let original_max_review_event_id = if original_schema_version >= 45 {
            staged.conn.query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))?
        } else {
            0
        };

        // Restore is two-phase. The WAL-consistent private copy above is writable, so FTS5 integrity
        // checks and migrations cannot mutate the source. Only after that exact generation reaches
        // HEAD and passes every check may it be copied over the live connection.
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
        staged.validate_desktop_review_action_journal().map_err(|error| {
            AppError::Other(format!("staged restore has an invalid desktop review action journal: {error}"))
        })?;

        Ok((staged, original_schema_version, original_max_review_event_id))
    }

    /// Compute a deterministic semantic digest of the complete logical SQLite generation visible to
    /// this connection. The digest binds ordered schema objects plus every persistent table as a
    /// multiset of type-preserving row hashes. It is therefore independent of page layout and
    /// scan/insertion order while still binding NULL/integer/real/text/blob values and hidden row
    /// identity whenever SQLite exposes one of its canonical aliases. The only value-level
    /// canonicalization is a closed list of structurally validated wall-clock fields injected by
    /// immutable historical migrations; those instants are replay time, not source authority.
    ///
    /// Callers that start from a file path must first use `open_detached_read_snapshot`; that copies a
    /// single WAL-consistent snapshot and prevents a main-file-only digest from blessing stale pages.
    pub(crate) fn restore_generation_sha256(&self) -> AppResult<String> {
        fn frame(hash: &mut Sha256, bytes: &[u8]) {
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }

        fn hash_value(hash: &mut Sha256, value: rusqlite::types::ValueRef<'_>) {
            match value {
                rusqlite::types::ValueRef::Null => hash.update([0]),
                rusqlite::types::ValueRef::Integer(value) => {
                    hash.update([1]);
                    hash.update(value.to_be_bytes());
                }
                rusqlite::types::ValueRef::Real(value) => {
                    hash.update([2]);
                    hash.update(value.to_bits().to_be_bytes());
                }
                rusqlite::types::ValueRef::Text(value) => {
                    hash.update([3]);
                    frame(hash, value);
                }
                rusqlite::types::ValueRef::Blob(value) => {
                    hash.update([4]);
                    frame(hash, value);
                }
            }
        }

        fn quote_identifier(identifier: &str) -> String {
            format!("\"{}\"", identifier.replace('"', "\"\""))
        }

        fn is_migration_clock_value(table: &str, column: &str) -> bool {
            matches!(
                (table, column),
                ("schema_migrations", "applied_at")
                    | ("review_compensation_policies", "created_at")
                    | ("review_effect_state", "created_at")
                    | ("orphan_segment_hypotheses_archive_v58", "archived_at")
                    | ("orphan_loop0_shadow_log_archive_v58", "archived_at")
            )
        }

        fn hash_migration_clock(
            hash: &mut Sha256,
            table: &str,
            column: &str,
            value: rusqlite::types::ValueRef<'_>,
        ) -> AppResult<()> {
            let rusqlite::types::ValueRef::Text(timestamp) = value else {
                return Err(AppError::Other(format!(
                    "restore generation {table}.{column} is not a text migration timestamp"
                )));
            };
            // SQLite's datetime('now') is `YYYY-MM-DD HH:MM:SS`. Permit the ISO separator and a
            // bounded fractional/timezone suffix for old supported databases, while rejecting an
            // empty/control-filled value that could conceal corruption behind canonicalization.
            let structurally_timestamped = (19..=40).contains(&timestamp.len())
                && timestamp.iter().all(u8::is_ascii)
                && timestamp.iter().all(|byte| !byte.is_ascii_control())
                && timestamp.get(0..4).is_some_and(|part| part.iter().all(u8::is_ascii_digit))
                && timestamp.get(4) == Some(&b'-')
                && timestamp.get(5..7).is_some_and(|part| part.iter().all(u8::is_ascii_digit))
                && timestamp.get(7) == Some(&b'-')
                && timestamp.get(8..10).is_some_and(|part| part.iter().all(u8::is_ascii_digit))
                && matches!(timestamp.get(10), Some(b' ' | b'T'))
                && timestamp.get(11..13).is_some_and(|part| part.iter().all(u8::is_ascii_digit))
                && timestamp.get(13) == Some(&b':')
                && timestamp.get(14..16).is_some_and(|part| part.iter().all(u8::is_ascii_digit))
                && timestamp.get(16) == Some(&b':')
                && timestamp.get(17..19).is_some_and(|part| part.iter().all(u8::is_ascii_digit));
            if !structurally_timestamped {
                return Err(AppError::Other(format!(
                    "restore generation {table}.{column} is not a structurally valid migration timestamp"
                )));
            }
            // These narrowly enumerated values are injected by `datetime('now')` while replaying immutable
            // historical migrations. Their wall-clock instant is neither source data nor runtime
            // authority, and hashing it would make the same old snapshot acquire a different target
            // digest on every recovery attempt. Bind a distinct typed sentinel instead; the table,
            // column, row identity and every authority-bearing field remain hashed normally.
            hash.update([0x7f]);
            Ok(())
        }

        let mut generation = Sha256::new();
        frame(&mut generation, b"cortex-sqlite-logical-generation-v1");

        // `rootpage` is intentionally excluded: it is allocation history, not logical authority.
        // SQL text and all object identities remain exact, including implicit index rows with NULL SQL.
        let mut schema = self.conn.prepare(
            "SELECT type, name, tbl_name, sql
               FROM main.sqlite_schema
              ORDER BY type COLLATE BINARY, name COLLATE BINARY, tbl_name COLLATE BINARY,
                       COALESCE(sql, '') COLLATE BINARY",
        )?;
        let mut schema_rows = schema.query([])?;
        while let Some(row) = schema_rows.next()? {
            generation.update([0x10]);
            for index in 0..4 {
                hash_value(&mut generation, row.get_ref(index)?);
            }
        }
        drop(schema_rows);
        drop(schema);

        let mut table_list = self.conn.prepare(
            "SELECT name, type, wr, strict
               FROM pragma_table_list
              WHERE schema = 'main'
                AND name <> 'sqlite_schema'
                AND type IN ('table', 'shadow', 'virtual')
              ORDER BY name COLLATE BINARY, type COLLATE BINARY",
        )?;
        let tables = table_list
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(table_list);

        for (table_name, table_type, without_rowid, strict) in tables {
            generation.update([0x20]);
            frame(&mut generation, table_name.as_bytes());
            frame(&mut generation, table_type.as_bytes());
            generation.update(without_rowid.to_be_bytes());
            generation.update(strict.to_be_bytes());

            let mut column_statement = self.conn.prepare(
                "SELECT name
                   FROM pragma_table_xinfo(?1)
                  WHERE hidden = 0
                  ORDER BY cid",
            )?;
            let columns = column_statement
                .query_map([&table_name], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(column_statement);
            for column in &columns {
                frame(&mut generation, column.as_bytes());
            }

            // A declared column can shadow a rowid alias. Select the first unshadowed canonical alias;
            // if all three are declared (or this is WITHOUT ROWID), the hidden identity is not exposed.
            let declared = columns.iter().map(|name| name.to_ascii_lowercase()).collect::<HashSet<_>>();
            let rowid_alias = (without_rowid == 0)
                .then(|| ["rowid", "_rowid_", "oid"].into_iter().find(|alias| !declared.contains(*alias)))
                .flatten();
            generation.update([u8::from(rowid_alias.is_some())]);

            let mut projections = Vec::with_capacity(columns.len() + usize::from(rowid_alias.is_some()));
            if let Some(alias) = rowid_alias {
                projections.push(quote_identifier(alias));
            }
            projections.extend(columns.iter().map(|column| quote_identifier(column)));
            if projections.is_empty() {
                return Err(AppError::Other(format!(
                    "persistent table {table_name:?} exposes neither columns nor row identity"
                )));
            }
            let query = format!("SELECT {} FROM {}", projections.join(", "), quote_identifier(&table_name));
            let mut statement = self.conn.prepare(&query)?;
            let mut rows = statement.query([])?;
            let mut row_hashes = Vec::<[u8; 32]>::new();
            while let Some(row) = rows.next()? {
                let mut row_hash = Sha256::new();
                frame(&mut row_hash, b"cortex-sqlite-logical-row-v1");
                for index in 0..projections.len() {
                    let column_index = index.checked_sub(usize::from(rowid_alias.is_some()));
                    if let Some(column_index) = column_index.filter(|index| {
                        columns.get(*index).is_some_and(|column| is_migration_clock_value(&table_name, column))
                    }) {
                        hash_migration_clock(&mut row_hash, &table_name, &columns[column_index], row.get_ref(index)?)?;
                    } else {
                        hash_value(&mut row_hash, row.get_ref(index)?);
                    }
                }
                row_hashes.push(row_hash.finalize().into());
            }
            row_hashes.sort_unstable();
            generation.update((row_hashes.len() as u64).to_be_bytes());
            for row_hash in row_hashes {
                generation.update(row_hash);
            }
        }

        let digest = generation.finalize();
        let encoded = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(encoded)
    }

    pub(crate) fn require_restore_generation_sha256(&self, expected: &str) -> AppResult<()> {
        let actual = self.restore_generation_sha256()?;
        if actual != expected {
            return Err(AppError::Other(format!(
                "published SQLite generation digest mismatch: expected {expected}, found {actual}"
            )));
        }
        Ok(())
    }

    /// Publish a source already proven by [`Self::stage_restore_source`]. SQLite's backup API holds
    /// the destination transaction until completion, so a copy error rolls the destination back
    /// instead of exposing a partially-restored database. Publication is forced through SQLite FULL
    /// durability and a verified TRUNCATE checkpoint; success therefore means no authoritative frame
    /// remains only in the live WAL.
    pub(crate) fn commit_staged_restore(&mut self, staged: &Database) -> AppResult<()> {
        self.conn.execute_batch("PRAGMA synchronous=FULL;")?;
        let result = (|| {
            {
                let backup = backup::Backup::new(&staged.conn, &mut self.conn)?;
                backup.run_to_completion(Self::BACKUP_PAGES_PER_STEP, Self::BACKUP_STEP_PAUSE, None)?;
            }
            let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
                self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
            let is_memory_database = self.path == ":memory:";
            let checkpoint_complete = busy == 0
                && ((log_frames == 0 && checkpointed_frames == 0)
                    || (is_memory_database && log_frames == -1 && checkpointed_frames == -1));
            if !checkpoint_complete {
                return Err(AppError::Other(format!(
                    "restore publication could not prove an empty authoritative WAL after FULL checkpoint \
                     (busy={busy}, log={log_frames}, checkpointed={checkpointed_frames})"
                )));
            }
            if !is_memory_database {
                crate::atomic_file::fsync_parent_dir_strict(Path::new(&self.path)).map_err(|error| {
                    AppError::Other(format!(
                        "restore SQLite generation was checkpointed but its directory metadata is not durably synchronized: {error}"
                    ))
                })?;
            }
            Ok(())
        })();
        let reset = self.conn.execute_batch("PRAGMA synchronous=NORMAL;");
        match result {
            Ok(()) => {
                // Publication is already durable. Failing to relax the connection is conservative and
                // must not manufacture an ambiguous failure response after the generation committed.
                if let Err(error) = reset {
                    tracing::error!(
                        "restore publication committed at FULL durability but synchronous=NORMAL could not be restored: {error}"
                    );
                }
                Ok(())
            }
            Err(error) => {
                if let Err(reset_error) = reset {
                    tracing::warn!("failed to restore SQLite synchronous=NORMAL after restore error: {reset_error}");
                }
                Err(error)
            }
        }
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
        let mut missing_paths: Vec<String> = paths.into_iter().filter(|p| !Path::new(p).is_file()).collect();
        missing_paths.sort();
        Ok(AudioHealth { total_files, missing_files: missing_paths.len(), missing_paths })
    }

    /// Relink missing source audio by basename only after proving exact decoded-PCM identity.
    ///
    /// The filename locates a candidate; it is never identity. Every segment at the missing path must
    /// carry the same canonical decoded-PCM BLAKE3, and the sealed candidate must decode to that exact
    /// hash. All segment, processing-provenance, and whole-file-reference path keys then move in one
    /// FULL-synchronous transaction. Any identity ambiguity or write failure leaves every key at the
    /// old path.
    pub fn relink_audio(&self, search_dir: &Path) -> AppResult<RelinkResult> {
        self.relink_audio_with_hook(search_dir, |_| Ok(()))
    }

    #[cfg(test)]
    pub(crate) fn relink_audio_with_test_hook(
        &self,
        search_dir: &Path,
        after_table_update: impl FnMut(&'static str) -> AppResult<()>,
    ) -> AppResult<RelinkResult> {
        self.relink_audio_with_hook(search_dir, after_table_update)
    }

    fn relink_audio_with_hook(
        &self,
        search_dir: &Path,
        mut after_table_update: impl FnMut(&'static str) -> AppResult<()>,
    ) -> AppResult<RelinkResult> {
        if !search_dir.is_absolute() {
            return Err(AppError::Validation("Audio relink requires an absolute owner-selected folder".to_string()));
        }
        let missing = self.audio_health()?.missing_paths;
        // Windows filenames are case-insensitive. Count a normalized UTF-8 basename so two stored
        // spellings cannot both claim the same candidate directory entry.
        let mut basename_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for old in &missing {
            if let Some(name) = Path::new(old).file_name() {
                *basename_counts.entry(name.to_string_lossy().to_lowercase()).or_insert(0) += 1;
            }
        }

        // Prove every candidate before opening a transaction. This makes a multi-source relink
        // all-or-none even when a later candidate has missing, conflicting, or wrong identity.
        let mut plans = Vec::new();
        for old in &missing {
            let Some(name) = Path::new(old).file_name() else { continue };
            let basename_key = name.to_string_lossy().to_lowercase();
            if basename_counts.get(&basename_key).copied().unwrap_or(0) > 1 {
                tracing::warn!(
                    "relink: '{}' shares its filename with another missing source — skipped (ambiguous, would risk the wrong audio)",
                    old
                );
                continue;
            }
            let candidate = search_dir.join(name);
            if candidate.is_file() {
                let new_path = candidate.to_str().ok_or_else(|| {
                    AppError::Validation("Audio relink candidate path is not valid Unicode".to_string())
                })?;
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

                let mut statement = self.conn.prepare(
                    "SELECT audio_content_hash
                       FROM speech_segments
                      WHERE audio_path = ?1
                      ORDER BY id",
                )?;
                let stored_hashes = statement
                    .query_map(params![old], |row| row.get::<_, Option<String>>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                if stored_hashes.is_empty() {
                    return Err(AppError::Validation(format!(
                        "Audio relink source '{old}' no longer has any segments"
                    )));
                }
                let mut expected_pcm_hash: Option<&str> = None;
                for stored_hash in &stored_hashes {
                    let stored_hash = stored_hash.as_deref().ok_or_else(|| {
                        AppError::Validation(format!(
                            "Audio relink source '{old}' has a segment without canonical decoded-PCM identity"
                        ))
                    })?;
                    if !is_canonical_audio_content_hash(stored_hash) {
                        return Err(AppError::Validation(format!(
                            "Audio relink source '{old}' has a non-canonical decoded-PCM identity"
                        )));
                    }
                    if expected_pcm_hash.is_some_and(|expected| expected != stored_hash) {
                        return Err(AppError::Validation(format!(
                            "Audio relink source '{old}' has conflicting decoded-PCM identities"
                        )));
                    }
                    expected_pcm_hash = Some(stored_hash);
                }

                let expected_pcm_hash = expected_pcm_hash
                    .ok_or_else(|| AppError::Validation(format!("Audio relink source '{old}' has no PCM identity")))?
                    .to_string();
                let candidate_lease = crate::media::seal_import_source(&candidate).map_err(|error| {
                    AppError::Validation(format!(
                        "Cannot seal audio relink candidate '{}': {error}",
                        candidate.display()
                    ))
                })?;
                let candidate_hash = crate::export_bundle::current_canonical_pcm_blake3(candidate_lease.source_path())?;
                if candidate_hash != expected_pcm_hash {
                    return Err(AppError::Validation(format!(
                        "Audio relink candidate '{}' is different audio from the missing source '{old}'",
                        candidate.display()
                    )));
                }
                plans.push(RelinkAudioPlan {
                    old_path: old.clone(),
                    new_path: new_path.to_string(),
                    expected_pcm_hash,
                    segment_count: stored_hashes.len(),
                    _candidate_lease: candidate_lease,
                });
            }
        }

        if plans.is_empty() {
            return Ok(RelinkResult { relinked: 0, still_missing: missing.len() });
        }

        self.with_full_sync(|| {
            let transaction =
                rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            for plan in &plans {
                // BEGIN IMMEDIATE holds the database write reservation while target ownership and the
                // complete segment set are re-proved. A concurrent insert, identity drift, or new
                // owner therefore cannot slip between these checks and publication.
                let target_owners: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM speech_segments WHERE audio_path = ?1",
                    params![plan.new_path],
                    |row| row.get(0),
                )?;
                if target_owners != 0 {
                    return Err(AppError::Validation(format!(
                        "Audio relink target acquired another owner before publication for '{}'",
                        plan.old_path
                    )));
                }
                let updated_segments = transaction.execute(
                    "UPDATE speech_segments
                        SET audio_path = ?2, updated_at = datetime('now')
                      WHERE audio_path = ?1
                        AND audio_content_hash = ?3",
                    params![plan.old_path, plan.new_path, plan.expected_pcm_hash],
                )?;
                if updated_segments != plan.segment_count {
                    return Err(AppError::Validation(format!(
                        "Audio relink authority changed before publication for '{}'",
                        plan.old_path
                    )));
                }
                after_table_update("speech_segments")?;

                transaction.execute(
                    "UPDATE source_audio_provenance SET audio_path = ?2 WHERE audio_path = ?1",
                    params![plan.old_path, plan.new_path],
                )?;
                after_table_update("source_audio_provenance")?;

                transaction.execute(
                    "UPDATE source_transcripts SET audio_path = ?2 WHERE audio_path = ?1",
                    params![plan.old_path, plan.new_path],
                )?;
                after_table_update("source_transcripts")?;
            }
            transaction.commit()?;
            Ok(RelinkResult { relinked: plans.len(), still_missing: missing.len().saturating_sub(plans.len()) })
        })
    }
}

#[cfg(test)]
mod restore_generation_tests {
    use super::*;

    fn append_desktop_decision(database: &Database, segment_id: &str) {
        database
            .insert_segment(&SpeechSegment {
                id: segment_id.into(),
                audio_path: format!("/{segment_id}.wav"),
                raw_transcript: format!("served {segment_id}"),
                duration_ms: 1_000,
                ..SpeechSegment::default()
            })
            .unwrap();
        database
            .record_human_decision(segment_id, "accept", Some(&format!("served {segment_id}")), Some(1_000))
            .unwrap();
    }

    fn logical_fixture(reverse: bool) -> Database {
        let database = Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        database
            .connection()
            .execute_batch(
                "CREATE TABLE digest_probe (
                    id INTEGER PRIMARY KEY,
                    nullable_value,
                    integer_value INTEGER NOT NULL,
                    real_value REAL NOT NULL,
                    text_value TEXT NOT NULL,
                    blob_value BLOB NOT NULL
                 );",
            )
            .unwrap();
        let ids = if reverse { [2_i64, 1_i64] } else { [1_i64, 2_i64] };
        for id in ids {
            database
                .connection()
                .execute(
                    "INSERT INTO digest_probe
                         (id, nullable_value, integer_value, real_value, text_value, blob_value)
                     VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, id * -17, id as f64 + 0.25, format!("value-{id}"), vec![0, id as u8, 255]],
                )
                .unwrap();
        }
        database
    }

    #[test]
    fn logical_generation_digest_ignores_page_and_scan_history_but_binds_values_and_schema() {
        let first = logical_fixture(false);
        let second = logical_fixture(true);
        let expected = first.restore_generation_sha256().unwrap();
        assert_eq!(expected.len(), 64);
        assert_eq!(second.restore_generation_sha256().unwrap(), expected);

        second.connection().execute("UPDATE digest_probe SET text_value = 'changed' WHERE id = 2", []).unwrap();
        assert_ne!(second.restore_generation_sha256().unwrap(), expected, "one typed value must change authority");
        second.connection().execute("UPDATE digest_probe SET text_value = 'value-2' WHERE id = 2", []).unwrap();
        assert_eq!(second.restore_generation_sha256().unwrap(), expected);

        second.connection().execute("CREATE INDEX digest_probe_text ON digest_probe(text_value)", []).unwrap();
        assert_ne!(second.restore_generation_sha256().unwrap(), expected, "schema drift must change authority");
    }

    #[test]
    fn staged_restore_includes_a_small_committed_wal_generation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wal-source.db");
        let source = Database::open(path.to_string_lossy().as_ref()).unwrap();
        source.initialize().unwrap();
        source.wal_checkpoint().unwrap();
        source.connection().execute_batch("PRAGMA wal_autocheckpoint=0;").unwrap();
        source
            .insert_segment(&SpeechSegment {
                id: "wal-authority".into(),
                audio_path: "wal-authority.wav".into(),
                raw_transcript: "committed-only-in-wal".into(),
                duration_ms: 1_000,
                ..SpeechSegment::default()
            })
            .unwrap();

        let wal_path = path.with_file_name(format!("{}-wal", path.file_name().unwrap().to_string_lossy()));
        assert!(std::fs::metadata(&wal_path).unwrap().len() > 32, "fixture must retain committed WAL frames");
        let main_before = std::fs::read(&path).unwrap();
        let wal_before = std::fs::read(&wal_path).unwrap();
        let expected = source.restore_generation_sha256().unwrap();
        let staged = Database::stage_restore_source(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), main_before, "staging must not mutate the source main file");
        assert_eq!(std::fs::read(&wal_path).unwrap(), wal_before, "staging must not mutate the source WAL");
        assert_eq!(staged.restore_generation_sha256().unwrap(), expected);
        assert_eq!(staged.get_segment_by_id("wal-authority").unwrap().unwrap().raw_transcript, "committed-only-in-wal");
    }

    #[test]
    fn staged_restore_retains_original_policy_schema_evidence_before_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old-policy-source.db");
        let source = Database::open(path.to_string_lossy().as_ref()).unwrap();
        source.initialize().unwrap();
        let current = crate::migrations::max_supported_version();
        assert!(current >= 59);
        let reverted = crate::migrations::rollback(&source, usize::try_from(current - 58).unwrap()).unwrap();
        assert_eq!(reverted.len(), usize::try_from(current - 58).unwrap());
        assert_eq!(crate::migrations::validate_applied_history(source.connection()).unwrap(), 58);

        let (staged, original_schema, original_max_review_event_id) =
            Database::stage_restore_source_with_original_evidence(&path).unwrap();
        assert_eq!(original_schema, 58, "staging migration must not rewrite creation-time policy evidence");
        assert_eq!(original_max_review_event_id, 0);
        assert_eq!(
            crate::migrations::validate_applied_history(staged.connection()).unwrap(),
            current,
            "the publishable copy must still migrate fully"
        );
    }

    #[test]
    fn desktop_action_journal_round_trips_through_staging_publication_and_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("journal-source.db");
        let source = Database::open(source_path.to_string_lossy().as_ref()).unwrap();
        source.initialize().unwrap();
        source.connection().execute_batch("PRAGMA wal_autocheckpoint=0;").unwrap();
        append_desktop_decision(&source, "journal-round-trip");
        let expected_availability = source.desktop_review_undo_availability().unwrap();

        let staged = Database::stage_restore_source(&source_path).unwrap();
        assert_eq!(staged.desktop_review_undo_availability().unwrap(), expected_availability);
        let expected_generation = staged.restore_generation_sha256().unwrap();

        let live_path = directory.path().join("journal-live.db");
        let mut live = Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.commit_staged_restore(&staged).unwrap();
        live.require_restore_generation_sha256(&expected_generation).unwrap();
        drop(live);

        let reopened = Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        reopened.initialize().unwrap();
        assert_eq!(reopened.desktop_review_undo_availability().unwrap(), expected_availability);
        reopened.require_restore_generation_sha256(&expected_generation).unwrap();
    }

    #[test]
    fn staged_restore_refuses_structurally_healthy_missing_or_invented_desktop_journal_rows() {
        for corruption in ["missing", "invented"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("journal-{corruption}.db"));
            let source = Database::open(path.to_string_lossy().as_ref()).unwrap();
            source.initialize().unwrap();
            append_desktop_decision(&source, &format!("journal-{corruption}"));

            match corruption {
                "missing" => {
                    source
                        .connection()
                        .execute_batch(
                            "DROP TRIGGER desktop_review_action_events_v1_immutable_delete;
                             DELETE FROM desktop_review_action_events_v1 WHERE action_kind='decision';
                             CREATE TRIGGER desktop_review_action_events_v1_immutable_delete
                             BEFORE DELETE ON desktop_review_action_events_v1
                             BEGIN SELECT RAISE(ABORT,'desktop review action journal is append-only'); END;",
                        )
                        .unwrap();
                }
                "invented" => {
                    source
                        .connection()
                        .execute_batch(
                            "DROP TRIGGER desktop_review_action_events_v1_validate_insert;
                             INSERT INTO desktop_review_action_events_v1(action_kind,effect_event_id)
                             VALUES('flag',9223372036854770000);
                             CREATE TRIGGER desktop_review_action_events_v1_validate_insert
                             BEFORE INSERT ON desktop_review_action_events_v1
                             WHEN (NEW.action_kind<>'legacy_barrier' AND EXISTS (
                                       SELECT 1 FROM desktop_review_legacy_actions_v1 legacy
                                        WHERE legacy.source_kind=NEW.action_kind
                                          AND legacy.effect_event_id=NEW.effect_event_id
                                   ))
                                OR (NEW.action_kind='decision' AND NOT EXISTS (
                                       SELECT 1 FROM human_decision_effect_events effect
                                        WHERE effect.id=NEW.effect_event_id
                                          AND effect.source='desktop' AND effect.reviewer IS NULL
                                   ))
                                OR (NEW.action_kind='decision_undo' AND NOT EXISTS (
                                       SELECT 1
                                         FROM human_decision_effect_reversals reversal
                                         JOIN human_decision_effect_events effect
                                           ON effect.id=reversal.effect_event_id
                                        WHERE reversal.effect_event_id=NEW.effect_event_id
                                          AND effect.source='desktop' AND effect.reviewer IS NULL
                                   ))
                                OR (NEW.action_kind='flag' AND NOT EXISTS (
                                       SELECT 1 FROM review_flag_effect_events flag
                                        WHERE flag.id=NEW.effect_event_id
                                   ))
                                OR (NEW.action_kind='flag_undo' AND NOT EXISTS (
                                       SELECT 1 FROM review_flag_effect_reversals reversal
                                        WHERE reversal.flag_effect_event_id=NEW.effect_event_id
                                   ))
                             BEGIN SELECT RAISE(ABORT,'desktop review action journal requires its exact effect'); END;",
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(source.integrity_check().unwrap(), "ok", "{corruption} fixture must remain SQLite-healthy");
            let error = match Database::stage_restore_source(&path) {
                Ok(_) => panic!("{corruption} journal corruption unexpectedly passed staged restore"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("desktop review action journal"), "{corruption}: {error}");
            drop(source);
            let reopened = Database::open(path.to_string_lossy().as_ref()).unwrap();
            let startup_error = reopened.initialize().expect_err("normal startup must reject the corrupt journal");
            assert!(
                startup_error.to_string().contains("desktop review action journal"),
                "{corruption} startup: {startup_error}"
            );
        }
    }

    #[test]
    fn staged_restore_and_startup_refuse_a_legacy_barrier_after_post_boundary_actions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal-late-barrier.db");
        let source = Database::open(path.to_string_lossy().as_ref()).unwrap();
        source.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&source, 2).unwrap(), vec![70, 69]);
        append_desktop_decision(&source, "legacy-before-barrier");
        assert_eq!(crate::migrations::run_migrations(&source).unwrap(), vec![69, 70]);
        append_desktop_decision(&source, "post-boundary-after-barrier");
        source
            .connection()
            .execute_batch(
                "DROP TRIGGER desktop_review_action_events_v1_immutable_update;
                 UPDATE desktop_review_action_events_v1
                    SET id=(SELECT MAX(id)+1 FROM desktop_review_action_events_v1)
                  WHERE action_kind='legacy_barrier';
                 CREATE TRIGGER desktop_review_action_events_v1_immutable_update
                 BEFORE UPDATE ON desktop_review_action_events_v1
                 BEGIN SELECT RAISE(ABORT,'desktop review action journal is append-only'); END;",
            )
            .unwrap();
        assert_eq!(source.integrity_check().unwrap(), "ok");
        let staged_error = match Database::stage_restore_source(&path) {
            Ok(_) => panic!("a late legacy barrier unexpectedly passed staged restore"),
            Err(error) => error,
        };
        assert!(staged_error.to_string().contains("desktop review action journal"), "{staged_error}");

        drop(source);
        let reopened = Database::open(path.to_string_lossy().as_ref()).unwrap();
        let startup_error = reopened.initialize().expect_err("normal startup must refuse the late legacy barrier");
        assert!(startup_error.to_string().contains("desktop review action journal"), "{startup_error}");
    }

    #[test]
    fn restore_publication_is_full_synced_checkpointed_and_exact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.db");
        let mut live = Database::open(path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();

        let staged = logical_fixture(true);
        let expected = staged.restore_generation_sha256().unwrap();
        live.commit_staged_restore(&staged).unwrap();
        live.require_restore_generation_sha256(&expected).unwrap();
        let checkpoint: (i64, i64, i64) = live
            .connection()
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        assert_eq!(checkpoint, (0, 0, 0), "no authoritative WAL frame may remain after publication");
        let synchronous: i64 = live.connection().query_row("PRAGMA synchronous", [], |row| row.get(0)).unwrap();
        assert_eq!(synchronous, 1, "ordinary operation returns to NORMAL only after the FULL barrier");
    }
}
