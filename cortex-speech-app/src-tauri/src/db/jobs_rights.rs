use super::*;

impl Database {
    // ── P3.2: import journal (resume a directory import interrupted by a crash) ──────────────────

    /// Open a new import job (status 'running'). Also prunes old finished jobs so the journal stays
    /// small. Production callers fail closed if this durable recovery boundary cannot be written.
    pub fn begin_import_job(&self, dir: &str, total_files: usize) -> AppResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        // SAVEPOINT (write-path audit, Week 2): reap + INSERT + retention are one invariant — a failure
        // after the reap used to leave prior crashes marked 'abandoned' WITHOUT the new running job that
        // justified abandoning them (the resume prompt would then find nothing to offer).
        self.conn.execute("SAVEPOINT import_job_begin", [])?;
        let result: AppResult<()> = (|| {
            // Reap stale crashes first. Imports are single-flight (try_start_import guards the only call
            // site), so when a NEW import begins any lingering 'running' job is a PRIOR crash the user did
            // not resume — the startup resume prompt already had its chance before this new import started.
            // Marking them 'abandoned' keeps exactly one 'running' job (the active one), so:
            //   * find_interrupted_import_job stays unambiguous — no spurious "resume?" for an old crash
            //     after the user already resumed a newer one, and
            //   * 'running' rows can't accumulate unboundedly across repeated crashes (abandoned rows are
            //     status != 'running', so the retention prune below reaps them + CASCADE clears their files).
            self.conn.execute(
                "UPDATE import_jobs SET status = 'abandoned', updated_at = datetime('now') WHERE status = 'running'",
                [],
            )?;
            self.conn.execute(
                "INSERT INTO import_jobs (id, dir, total_files, status) VALUES (?1, ?2, ?3, 'running')",
                params![id, dir, total_files as i64],
            )?;
            // Retention: keep the newest 50 FINISHED jobs (running jobs are always kept — they may be crashes).
            self.conn.execute(
                "DELETE FROM import_jobs WHERE status != 'running' AND id NOT IN (
                     SELECT id FROM import_jobs WHERE status != 'running'
                     ORDER BY datetime(created_at) DESC, id DESC LIMIT 50
                 )",
                [],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("import_job_begin")?;
                Ok(id)
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("import_job_begin");
                Err(e)
            }
        }
    }

    /// Atomically replace one crashed import journal with the journal that will own its resume.
    ///
    /// The old implementation deleted the crashed journal in the command handler, then created the
    /// successor only after the background worker entered `import_directory_with_agent_run_id`. A
    /// process kill in that gap left already-published segments with no durable resume authority. This
    /// transaction keeps the old job visible until the successor row *and* its completed-file set are
    /// committed. SQLite rolls the whole handoff back on any failure or process death, so observers see
    /// either the exact old journal or the exact successor, never zero (or two) running journals.
    pub fn handoff_import_job_for_resume(&self, prior_job_id: &str) -> AppResult<String> {
        crate::validation::input::validate_identifier(prior_job_id).map_err(AppError::Validation)?;
        let successor_id = uuid::Uuid::new_v4().to_string();
        self.conn.execute("SAVEPOINT import_job_resume_handoff", [])?;
        let result: AppResult<()> = (|| {
            let prior_exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM import_jobs WHERE id = ?1 AND status = 'running')",
                params![prior_job_id],
                |row| row.get(0),
            )?;
            if !prior_exists {
                return Err(AppError::Validation(format!(
                    "Interrupted import journal '{prior_job_id}' is no longer the active resumable job"
                )));
            }

            // Retire every stale running row in the same transaction. Single-flight import admission
            // means `prior_job_id` should be the only one; handling legacy duplicates here restores the
            // stronger invariant instead of selecting one nondeterministically.
            self.conn.execute(
                "UPDATE import_jobs SET status = 'abandoned', updated_at = datetime('now') WHERE status = 'running'",
                [],
            )?;
            let inserted = self.conn.execute(
                "INSERT INTO import_jobs (id, dir, total_files, status)
                 SELECT ?1, dir, total_files, 'running' FROM import_jobs WHERE id = ?2",
                params![successor_id, prior_job_id],
            )?;
            if inserted != 1 {
                return Err(AppError::Other(format!(
                    "Interrupted import journal '{prior_job_id}' disappeared during resume handoff"
                )));
            }
            self.conn.execute(
                "INSERT INTO import_job_files (job_id, path)
                 SELECT ?1, path FROM import_job_files WHERE job_id = ?2",
                params![successor_id, prior_job_id],
            )?;

            let running: i64 =
                self.conn
                    .query_row("SELECT COUNT(*) FROM import_jobs WHERE status = 'running'", [], |row| row.get(0))?;
            if running != 1 {
                return Err(AppError::Other(format!(
                    "Resume journal handoff produced {running} running jobs instead of exactly one"
                )));
            }

            // Same bounded retention contract as `begin_import_job`, after the successor has copied
            // the old file set. The old row may now be pruned without erasing resume progress.
            self.conn.execute(
                "DELETE FROM import_jobs WHERE status != 'running' AND id NOT IN (
                     SELECT id FROM import_jobs WHERE status != 'running'
                     ORDER BY datetime(created_at) DESC, id DESC LIMIT 50
                 )",
                [],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("import_job_resume_handoff")?;
                Ok(successor_id)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("import_job_resume_handoff");
                Err(error)
            }
        }
    }

    /// Admit the already-claimed resume journal at worker entry before any audio can be decoded or
    /// segment row can publish. The compare-and-update proves the worker still owns the exact running
    /// journal and refreshes its total for the directory as it exists on this attempt.
    pub fn continue_import_job(&self, job_id: &str, dir: &str, total_files: usize) -> AppResult<()> {
        crate::validation::input::validate_identifier(job_id).map_err(AppError::Validation)?;
        let updated = self.conn.execute(
            "UPDATE import_jobs
                SET total_files = ?3, updated_at = datetime('now')
              WHERE id = ?1 AND dir = ?2 AND status = 'running'",
            params![job_id, dir, total_files as i64],
        )?;
        if updated != 1 {
            return Err(AppError::Validation(format!(
                "Resume worker does not own running import journal '{job_id}' for '{dir}'"
            )));
        }
        Ok(())
    }

    /// Record that `path` finished processing in job `job_id` (idempotent).
    pub fn mark_import_file_done(&self, job_id: &str, path: &str) -> AppResult<()> {
        self.conn
            .execute("INSERT OR IGNORE INTO import_job_files (job_id, path) VALUES (?1, ?2)", params![job_id, path])?;
        self.conn.execute("UPDATE import_jobs SET updated_at = datetime('now') WHERE id = ?1", params![job_id])?;
        Ok(())
    }

    /// Mark a job finished (a clean end): it is no longer an interruption to resume.
    pub fn complete_import_job(&self, job_id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE import_jobs SET status = 'completed', updated_at = datetime('now') WHERE id = ?1",
            params![job_id],
        )?;
        Ok(())
    }

    /// Discard an interrupted job (the user chose not to resume). Deletes both tables explicitly so it
    /// works whether or not the foreign-keys pragma is enabling CASCADE.
    pub fn discard_import_job(&self, job_id: &str) -> AppResult<()> {
        // SAVEPOINT: the two deletes are one invariant (same pattern as begin_import_job). As two
        // auto-commit statements, a failure between them deleted the job's per-file progress journal
        // while leaving the job row alive and 'running' — the startup resume prompt would then offer a
        // job with an EMPTY completed-files list, and resuming would re-import files whose segments
        // already exist, duplicating them.
        self.conn.execute("SAVEPOINT discard_import_job", [])?;
        let result: AppResult<()> = (|| {
            self.conn.execute("DELETE FROM import_job_files WHERE job_id = ?1", params![job_id])?;
            self.conn.execute("DELETE FROM import_jobs WHERE id = ?1", params![job_id])?;
            Ok(())
        })();
        match result {
            Ok(()) => self.release_savepoint("discard_import_job"),
            Err(e) => {
                self.cleanup_savepoint_after_error("discard_import_job");
                Err(e)
            }
        }
    }

    /// The most recent still-'running' job — a crash never calls `complete_import_job`, so it stays
    /// running. Intended to be queried at STARTUP (when no import is active, a running job IS a crash).
    pub fn find_interrupted_import_job(&self) -> AppResult<Option<ImportJob>> {
        let head = self.conn.query_row(
            "SELECT id, dir, total_files, created_at FROM import_jobs
             WHERE status = 'running' ORDER BY datetime(created_at) DESC, id DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)),
        );
        let (id, dir, total_files, created_at) = match head {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut stmt = self.conn.prepare("SELECT path FROM import_job_files WHERE job_id = ?1")?;
        let completed_paths: Vec<String> = stmt.query_map(params![id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(Some(ImportJob { id, dir, total_files: total_files as usize, completed_paths, created_at }))
    }

    // ── Durable jobs (migration v37 + crate::jobs::JobState) — the persistent Job Supervisor. ──

    /// Build a `Job` from a `(id, kind, state_str, progress, completed, total, error_code)` row tuple,
    /// erroring if the persisted state is outside the lifecycle vocabulary (the CHECK constraint makes
    /// that impossible, but a corrupt DB shouldn't silently coerce to a wrong state).
    pub(super) fn job_from_row(row: JobRow) -> AppResult<crate::jobs::Job> {
        let (id, kind, state_str, progress, completed, total, error_code) = row;
        let state = crate::jobs::JobState::parse(&state_str)
            .ok_or_else(|| AppError::Other(format!("job {id} has an unknown state {state_str:?} in the database")))?;
        Ok(crate::jobs::Job { id, kind, state, progress, completed, total, error_code })
    }

    const JOB_COLS: &str = "id, kind, state, progress, completed, total, error_code";

    pub(super) fn read_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
    }

    pub(super) fn read_payload_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::jobs::PayloadJob> {
        let state_token: String = r.get(2)?;
        let id: String = r.get(0)?;
        let state = crate::jobs::JobState::parse(&state_token).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                format!("job {id} has unknown state {state_token:?}").into(),
            )
        })?;
        Ok(crate::jobs::PayloadJob {
            id,
            kind: r.get(1)?,
            state,
            idempotency_key: r.get(3)?,
            error_code: r.get(4)?,
            payload_json: r.get(5)?,
        })
    }

    const PAYLOAD_JOB_COLS: &str = "id, kind, state, idempotency_key, error_code, payload_json";

    /// Fetch a job by id, or `None` if it doesn't exist.
    pub fn get_job(&self, id: &str) -> AppResult<Option<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs WHERE id = ?1", Self::JOB_COLS);
        match self.conn.query_row(&sql, params![id], Self::read_job_row) {
            Ok(row) => Ok(Some(Self::job_from_row(row)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub(super) fn get_job_by_idempotency_key(&self, key: &str) -> AppResult<Option<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs WHERE idempotency_key = ?1", Self::JOB_COLS);
        match self.conn.query_row(&sql, params![key], Self::read_job_row) {
            Ok(row) => Ok(Some(Self::job_from_row(row)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Create a queued job, or return the existing one when `idempotency_key` is already present — so a
    /// re-issued identical request (retry, double-click) resumes the same job instead of duplicating work.
    pub fn create_or_get_job(
        &self,
        id: &str,
        kind: &str,
        idempotency_key: Option<&str>,
        total: Option<i64>,
    ) -> AppResult<crate::jobs::Job> {
        // Check-then-insert with nothing holding the gap is only dedup for the caller that wins: the
        // loser used to get a raw UNIQUE-constraint error instead of the job the winner created —
        // exactly the retry this function exists to absorb. One savepoint (the sibling
        // `begin_running_payload_job`'s idiom) plus insert-then-select-on-conflict closes it.
        self.conn.execute("SAVEPOINT create_or_get_job", [])?;
        let result: AppResult<crate::jobs::Job> = (|| {
            self.conn.execute(
                "INSERT INTO jobs (id, kind, idempotency_key, total, state) VALUES (?1, ?2, ?3, ?4, 'queued')
                 ON CONFLICT DO NOTHING",
                params![id, kind, idempotency_key, total],
            )?;
            if let Some(key) = idempotency_key {
                if let Some(existing) = self.get_job_by_idempotency_key(key)? {
                    return Ok(existing);
                }
            }
            self.get_job(id)?.ok_or_else(|| AppError::Other(format!("job {id} vanished immediately after insert")))
        })();
        match result {
            Ok(job) => {
                self.release_savepoint("create_or_get_job")?;
                Ok(job)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("create_or_get_job");
                Err(error)
            }
        }
    }

    /// Fetch the durable payload for a payload-owning job kind. A NULL payload is an integrity error:
    /// callers of this API have declared that their recovery contract depends on the journal bytes.
    pub fn get_payload_job(&self, id: &str) -> AppResult<Option<crate::jobs::PayloadJob>> {
        let sql = format!("SELECT {} FROM jobs WHERE id = ?1", Self::PAYLOAD_JOB_COLS);
        match self.conn.query_row(&sql, params![id], Self::read_payload_job_row) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically publish a job as `running` with its recovery payload before any external mutation.
    /// An idempotent retry returns the existing row; the owning coordinator must compare its exact
    /// contract bytes before resuming. No queued-without-payload crash window exists.
    pub fn begin_running_payload_job(
        &self,
        id: &str,
        kind: &str,
        idempotency_key: &str,
        payload_json: &str,
    ) -> AppResult<crate::jobs::BegunPayloadJob> {
        self.conn.execute("SAVEPOINT begin_running_payload_job", [])?;
        let result: AppResult<crate::jobs::BegunPayloadJob> = (|| {
            let by_key_sql = format!("SELECT {} FROM jobs WHERE idempotency_key = ?1", Self::PAYLOAD_JOB_COLS);
            let existing = match self.conn.query_row(&by_key_sql, params![idempotency_key], Self::read_payload_job_row)
            {
                Ok(row) => Some(row),
                Err(rusqlite::Error::QueryReturnedNoRows) => self.get_payload_job(id)?,
                Err(e) => return Err(e.into()),
            };
            if let Some(job) = existing {
                return Ok(crate::jobs::BegunPayloadJob { job, created: false });
            }

            self.conn.execute(
                "INSERT INTO jobs
                    (id, kind, state, idempotency_key, payload_json, started_at)
                 VALUES (?1, ?2, 'running', ?3, ?4, datetime('now'))",
                params![id, kind, idempotency_key, payload_json],
            )?;
            let job = self
                .get_payload_job(id)?
                .ok_or_else(|| AppError::Other(format!("payload job {id} vanished immediately after insert")))?;
            Ok(crate::jobs::BegunPayloadJob { job, created: true })
        })();
        match result {
            Ok(value) => {
                self.release_savepoint("begin_running_payload_job")?;
                Ok(value)
            }
            Err(error) => {
                self.cleanup_savepoint_after_error("begin_running_payload_job");
                Err(error)
            }
        }
    }

    /// Compare-and-swap a running job's recovery payload. Re-running an external side effect after a
    /// crash is safe only when phase advancement cannot silently overwrite another recovery worker.
    pub fn compare_and_swap_running_job_payload(
        &self,
        id: &str,
        kind: &str,
        expected_payload_json: &str,
        next_payload_json: &str,
    ) -> AppResult<()> {
        let changed = self.conn.execute(
            "UPDATE jobs SET payload_json = ?4, updated_at = datetime('now')
             WHERE id = ?1 AND kind = ?2 AND state = 'running' AND payload_json = ?3",
            params![id, kind, expected_payload_json, next_payload_json],
        )?;
        if changed != 1 {
            return Err(AppError::Validation(format!(
                "running payload job {id} changed concurrently or is no longer resumable"
            )));
        }
        Ok(())
    }

    /// Atomically persist a final payload and terminal state. The expected payload comparison makes a
    /// stale recovery worker unable to claim success (or failure) over a newer phase.
    pub fn finish_running_payload_job(
        &self,
        id: &str,
        kind: &str,
        expected_payload_json: &str,
        final_payload_json: &str,
        state: crate::jobs::JobState,
        error_code: Option<&str>,
    ) -> AppResult<()> {
        if !matches!(state, crate::jobs::JobState::Succeeded | crate::jobs::JobState::Failed) {
            return Err(AppError::Validation(format!(
                "payload job {id} can only finish succeeded or failed, not {state}"
            )));
        }
        let changed = self.conn.execute(
            "UPDATE jobs SET state = ?5, error_code = ?6, payload_json = ?4,
                 progress = CASE WHEN ?5 = 'succeeded' THEN 1.0 ELSE progress END,
                 finished_at = datetime('now'), updated_at = datetime('now')
             WHERE id = ?1 AND kind = ?2 AND state = 'running' AND payload_json = ?3",
            params![id, kind, expected_payload_json, final_payload_json, state.as_str(), error_code],
        )?;
        if changed != 1 {
            return Err(AppError::Validation(format!(
                "running payload job {id} changed concurrently or is no longer finishable"
            )));
        }
        Ok(())
    }

    /// Startup recovery inventory for one durable kind. Unlike the generic orphan reaper, this
    /// exposes the exact payloads so the owner can complete or roll back each state machine.
    pub fn list_running_payload_jobs(&self, kind: &str) -> AppResult<Vec<crate::jobs::PayloadJob>> {
        let sql = format!(
            "SELECT {} FROM jobs WHERE kind = ?1 AND state = 'running'
             ORDER BY datetime(created_at), id",
            Self::PAYLOAD_JOB_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![kind], Self::read_payload_job_row)?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Move a job to `to`, enforcing the `JobState` lifecycle (an illegal edge — e.g. completing twice,
    /// or resurrecting a cancelled job — is rejected, not silently written). Stamps `started_at` on the
    /// first entry to `running` and `finished_at` on any terminal state.
    pub fn transition_job(&self, id: &str, to: crate::jobs::JobState, error_code: Option<&str>) -> AppResult<()> {
        let current = self.get_job(id)?.ok_or_else(|| AppError::Other(format!("job {id} not found")))?;
        if !current.state.can_transition_to(to) {
            return Err(AppError::Validation(format!(
                "illegal job transition {} -> {} for job {id}",
                current.state, to
            )));
        }
        let finished = to.is_terminal() as i64;
        // Compare-and-swap (write-path audit, Week 2): the lifecycle check above is read-then-write, and
        // a concurrent transition on ANOTHER connection could land between the read and this UPDATE —
        // the old unconditional WHERE would then apply an edge validated against a stale state (e.g.
        // resurrecting a just-cancelled job). Conditioning on the state we validated makes the racing
        // writer's edge a 0-row miss, surfaced as an honest error instead of a silent double-write.
        let affected = self.conn.execute(
            "UPDATE jobs SET
                 state = ?2,
                 error_code = ?3,
                 started_at = CASE WHEN ?2 = 'running' AND started_at IS NULL THEN datetime('now') ELSE started_at END,
                 finished_at = CASE WHEN ?4 = 1 THEN datetime('now') ELSE finished_at END,
                 updated_at = datetime('now')
             WHERE id = ?1 AND state = ?5",
            params![id, to.as_str(), error_code, finished, current.state.as_str()],
        )?;
        if affected == 0 {
            let now_state = self.get_job(id)?.map(|j| j.state.to_string()).unwrap_or_else(|| "<gone>".to_string());
            return Err(AppError::Validation(format!(
                "job {id} was transitioned concurrently ({} -> {now_state}); {} -> {to} rejected",
                current.state, current.state
            )));
        }
        Ok(())
    }

    /// Update a running job's progress. `progress` is clamped to 0.0..=1.0 to respect the CHECK constraint.
    pub fn update_job_progress(&self, id: &str, completed: i64, progress: f64) -> AppResult<()> {
        let progress = progress.clamp(0.0, 1.0);
        self.conn.execute(
            "UPDATE jobs SET completed = ?2, progress = ?3, updated_at = datetime('now') WHERE id = ?1",
            params![id, completed, progress],
        )?;
        Ok(())
    }

    /// At STARTUP, any job still `running` is a crash residue (a clean run always reaches a terminal
    /// state). Mark them failed with a stable `INTERRUPTED` code so the UI can honestly show "interrupted"
    /// instead of a ghost that never finishes. Returns how many were reaped.
    // ponytail: generic recovery = fail+INTERRUPTED; a resumable job kind can re-create from its own
    // durable state on the next run. Add per-kind auto-resume only when a kind actually needs it.
    pub fn mark_orphaned_running_jobs_failed(&self) -> AppResult<usize> {
        let n = self.conn.execute(
            "UPDATE jobs SET state = 'failed', error_code = COALESCE(error_code, 'INTERRUPTED'),
                 finished_at = datetime('now'), updated_at = datetime('now')
             WHERE state = 'running' AND kind <> 'model_promotion'",
            [],
        )?;
        Ok(n)
    }

    /// The most recent jobs (newest first), for a UI activity surface.
    pub fn list_recent_jobs(&self, limit: i64) -> AppResult<Vec<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs ORDER BY datetime(created_at) DESC, id DESC LIMIT ?1", Self::JOB_COLS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<JobRow> = stmt.query_map(params![limit], Self::read_job_row)?.collect::<Result<_, _>>()?;
        rows.into_iter().map(Self::job_from_row).collect()
    }

    /// Bracket `work` as a durable job: record a queued→running lifecycle, run it, then mark
    /// succeeded, or failed with the stable `error_code` on error (the original error still propagates).
    /// A crash mid-`work` leaves a `running` row that `mark_orphaned_running_jobs_failed` reaps at the
    /// next startup — that is the whole point of routing a long op through here. `job_id` is caller-
    /// supplied (a fresh uuid) so the id is known before `work` starts and survives a crash.
    pub fn run_tracked<T>(
        &self,
        job_id: &str,
        kind: &str,
        error_code: &str,
        work: impl FnOnce(&Database) -> AppResult<T>,
    ) -> AppResult<T> {
        self.create_or_get_job(job_id, kind, None, None)?;
        self.transition_job(job_id, crate::jobs::JobState::Running, None)?;
        // Once `work` returns, the op's real outcome is DECIDED (the export file is on disk, or not).
        // The terminal stamp is a best-effort RECORD of that — it must never change what the caller sees.
        // If a stamp write fails, the row lingers `running` and is reaped as INTERRUPTED at the next
        // startup: a cosmetic history wart, never a false result to the user or data loss.
        match work(self) {
            Ok(v) => {
                if let Err(e) = self.transition_job(job_id, crate::jobs::JobState::Succeeded, None) {
                    tracing::warn!("job {job_id} ({kind}) succeeded but recording success failed: {e}");
                }
                Ok(v)
            }
            Err(e) => {
                let _ = self.transition_job(job_id, crate::jobs::JobState::Failed, Some(error_code));
                Err(e)
            }
        }
    }

    pub fn insert_hypothesis(&self, hyp: &SegmentHypothesis) -> AppResult<()> {
        // NFC-canonicalize the vote at this single chokepoint so EVERY engine's hypothesis (local
        // 300M/1B/WSL-7B and cloud Scribe) is stored in the same normalization form. The jury scores
        // agreement by exact surface word-equality (diff/phonetic.rs); without this, two engines that
        // emit the same Sorani text in different forms (NFD vs NFC) would be scored as disagreeing and
        // a real consensus would be spuriously escalated. Matches the NFC enforced on speech_segments.
        let transcript = to_nfc(&hyp.transcript);
        self.conn.execute(
            "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(segment_id, model_id) DO UPDATE SET
                transcript=excluded.transcript,
                confidence=excluded.confidence,
                created_at=datetime('now')",
            params![hyp.segment_id, hyp.model_id, transcript, hyp.confidence],
        )?;
        Ok(())
    }

    /// Atomically make one model the segment's sole machine hypothesis. Champion transcription uses
    /// this after a successful 7B write so votes left by an older 300M/1B/MMS/Scribe run cannot remain
    /// active evidence. DELETE + INSERT must be one savepoint: a failed insert may never leave a good
    /// segment with no provenance merely because cleanup ran first.
    pub fn replace_hypotheses_with(&self, hyp: &SegmentHypothesis) -> AppResult<()> {
        let transcript = to_nfc(&hyp.transcript);
        self.conn.execute("SAVEPOINT replace_hypotheses", [])?;
        let result = (|| -> AppResult<()> {
            self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id = ?1", params![hyp.segment_id])?;
            self.conn.execute(
                "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hyp.segment_id, hyp.model_id, transcript, hyp.confidence],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => self.release_savepoint("replace_hypotheses"),
            Err(error) => {
                self.cleanup_savepoint_after_error("replace_hypotheses");
                Err(error)
            }
        }
    }

    /// The rights attached to the recording this segment came from (migration v49, audit #6).
    ///
    /// Read as its own row lookup rather than as fields on [`SpeechSegment`]: that struct is already
    /// wide, and every past widening broke every destructuring insert site. The export gate that needs
    /// this already runs a per-segment query for hypotheses, so this costs the same shape it already
    /// pays.
    pub fn rights_for_segment(&self, segment_id: &str) -> AppResult<RecordingRights> {
        let rights = self.conn.query_row(
            "SELECT rights_license, rights_consent_basis, rights_permitted_use,
                    rights_attribution, rights_source, rights_revoked_at
             FROM speech_segments WHERE id = ?1",
            params![segment_id],
            |r| {
                Ok(RecordingRights {
                    license: r.get(0)?,
                    consent_basis: r.get(1)?,
                    permitted_use: r.get(2)?,
                    attribution: r.get(3)?,
                    source: r.get(4)?,
                    revoked_at: r.get(5)?,
                })
            },
        );
        match rights {
            Ok(v) => Ok(v),
            // A missing row is UNKNOWN rights, never "permitted": the default must fail closed.
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(RecordingRights::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Declare rights for a RECORDING — every segment cut from that source file, in one statement.
    ///
    /// Storage is per row (see migration v49) but the unit of consent is the recording, so the API
    /// takes an audio path. Returns the number of segments updated so a caller can report what it
    /// actually covered rather than what it attempted.
    ///
    /// Deliberately does NOT clear `rights_revoked_at`: re-declaring a licence must not silently
    /// resurrect a withdrawn recording. Un-revoking is a separate, explicit act.
    pub fn set_recording_rights(&self, audio_path: &str, rights: &RecordingRights) -> AppResult<usize> {
        Ok(self.conn.execute(
            "UPDATE speech_segments
                SET rights_license = ?2, rights_consent_basis = ?3, rights_permitted_use = ?4,
                    rights_attribution = ?5, rights_source = ?6, updated_at = datetime('now')
              WHERE audio_path = ?1",
            params![
                audio_path,
                rights.license,
                rights.consent_basis,
                rights.permitted_use,
                rights.attribution,
                rights.source,
            ],
        )?)
    }

    /// Record a withdrawal of consent for a recording. Stamps every segment cut from it.
    ///
    /// This is the revocation lineage: once stamped, `rights_disposition` returns `Revoked` and every
    /// export path — including plain local export — must drop the row. A withdrawal that only blocks
    /// future publishing is not a withdrawal.
    /// Persist BOTH identity tiers for every segment of one source recording (v50 spectral + v51 hash).
    ///
    /// Keyed on `audio_path` like `set_recording_rights`: all VAD chunks of one recording share it, and
    /// the identity being recorded is the RECORDING's, not the chunk's.
    ///
    /// Both columns in ONE statement deliberately: a caller that could write the spectral value without
    /// the content hash would re-create the pre-v51 state on the next restart, where a rehydrated entry
    /// has a bucket key but nothing able to prove identity.
    ///
    /// `spectral as i64` is a bit-cast, not a numeric conversion — SQLite integers are i64 and the
    /// value is a u64, so the top bit round-trips only because both directions cast rather than
    /// convert. `load_audio_identities` casts back the same way.
    pub(super) fn ensure_audio_identity_compatible(&self, audio_path: &str, identity: &AudioIdentity) -> AppResult<()> {
        let alias_key = audio_path.replace('/', "\\").to_lowercase();
        let conflict: Option<(String, Option<i64>, String)> = self
            .conn
            .query_row(
                "SELECT id, audio_fingerprint, audio_content_hash
                   FROM speech_segments
                  WHERE LOWER(REPLACE(audio_path, '/', CHAR(92))) = ?1
                    AND audio_content_hash IS NOT NULL
                    AND (audio_content_hash <> ?2
                         OR (audio_fingerprint IS NOT NULL AND audio_fingerprint <> ?3))
                  ORDER BY rowid
                  LIMIT 1",
                params![alias_key, identity.content, identity.spectral as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((segment_id, prior_spectral, prior_content)) = conflict {
            return Err(AppError::Validation(format!(
                "SOURCE_IDENTITY_DRIFT: source path aliases already belong to segment '{segment_id}' with recording identity {prior_content}/{prior_spectral:?}; refusing to bind replacement bytes"
            )));
        }
        Ok(())
    }

    /// Bind identity to exactly the segments published by one source operation.
    ///
    /// The compatibility check includes Windows case/separator aliases. It runs after the containing
    /// publication has obtained the SQLite writer lock, so a changed file at the same logical path
    /// rolls the new rows back instead of rebinding any older machine or human-owned segment.
    pub(super) fn set_audio_identity_for_segments(
        &self,
        audio_path: &str,
        segment_ids: &[String],
        identity: &AudioIdentity,
    ) -> AppResult<usize> {
        if segment_ids.is_empty() {
            return Err(AppError::Validation("No source-operation segments supplied for audio identity".into()));
        }
        self.ensure_audio_identity_compatible(audio_path, identity)?;

        let mut seen = HashSet::with_capacity(segment_ids.len());
        let mut updated = 0usize;
        for segment_id in segment_ids {
            if !seen.insert(segment_id.as_str()) {
                return Err(AppError::Validation(format!(
                    "Duplicate segment '{segment_id}' in one audio-identity publication"
                )));
            }
            let current: Option<(String, Option<i64>, Option<String>)> = self
                .conn
                .query_row(
                    "SELECT audio_path, audio_fingerprint, audio_content_hash
                       FROM speech_segments WHERE id = ?1",
                    [segment_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((stored_path, stored_spectral, stored_content)) = current else {
                return Err(AppError::Validation(format!("Audio-identity publication lost segment '{segment_id}'")));
            };
            if stored_path != audio_path {
                return Err(AppError::Validation(format!(
                    "Audio-identity publication segment '{segment_id}' belongs to another source path"
                )));
            }
            if stored_spectral == Some(identity.spectral as i64)
                && stored_content.as_deref() == Some(identity.content.as_str())
            {
                continue;
            }
            let changed = self.conn.execute(
                "UPDATE speech_segments
                    SET audio_fingerprint = ?2, audio_content_hash = ?3
                  WHERE id = ?1 AND audio_path = ?4",
                params![segment_id, identity.spectral as i64, identity.content, audio_path],
            )?;
            if changed != 1 {
                return Err(AppError::Validation(format!(
                    "Audio-identity publication could not bind exact segment '{segment_id}'"
                )));
            }
            updated += 1;
        }
        Ok(updated)
    }

    /// Legacy path-wide identity backfill. Existing non-null identity is immutable: changed bytes at
    /// the same logical Windows path must never rewrite earlier segment authority. New import
    /// publication uses `set_audio_identity_for_segments` instead of this compatibility API.
    pub fn set_audio_identity(&self, audio_path: &str, identity: &AudioIdentity) -> AppResult<usize> {
        self.ensure_audio_identity_compatible(audio_path, identity)?;
        Ok(self.conn.execute(
            "UPDATE speech_segments SET audio_fingerprint = ?2, audio_content_hash = ?3
              WHERE audio_path = ?1
                AND (audio_content_hash IS NULL OR audio_content_hash = ?3)",
            params![audio_path, identity.spectral as i64, identity.content],
        )?)
    }

    /// Every stored recording identity, for rehydrating the in-memory dedup map at startup.
    ///
    /// DISTINCT because a recording is many chunks sharing one path and one identity; without it a
    /// 144-chunk file would return 144 identical rows. Rows whose spectral value was never computed
    /// (every row predating v50, until the backfill runs) are skipped by the WHERE, not defaulted to 0 —
    /// a zero value is the degenerate silent-window bucket `register` deliberately refuses to store.
    ///
    /// `content` is `None` for v50-era rows: they have a bucket key but were never hashed. The dedup map
    /// keeps them and never lets them reject, because a value that cannot distinguish content must not
    /// be allowed to discard a legitimate recording. `backfill_fingerprints` closes that gap.
    pub fn load_audio_identities(&self) -> AppResult<Vec<StoredAudioIdentity>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT audio_fingerprint, audio_content_hash, audio_path FROM speech_segments
             WHERE audio_fingerprint IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(StoredAudioIdentity {
                    spectral: r.get::<_, i64>(0)? as u64,
                    content: r.get::<_, Option<String>>(1)?,
                    audio_path: r.get::<_, String>(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Load the complete cross-run dedup inventory from one stable SQLite read snapshot.
    ///
    /// The legacy [`load_audio_identities`](Self::load_audio_identities) API intentionally omits rows
    /// whose spectral bucket is NULL. That is useful to callers which can tolerate legacy rows, but it
    /// is not enough for the production import admission gate: an omitted row is exactly an existing
    /// recording which the in-memory index cannot reject on a later import. Count every distinct
    /// recording with either identity tier missing or unusable, then load the usable rows in the SAME
    /// transaction so startup cannot certify one database generation and rehydrate another. A zero
    /// spectral key is unusable because `AudioFingerprint::rehydrate` deliberately skips it; a hash
    /// outside the canonical 64-character lowercase-hex form can never match a newly decoded recording.
    pub fn load_audio_identity_inventory(&self) -> AppResult<(Vec<StoredAudioIdentity>, usize)> {
        let tx = self.conn.unchecked_transaction()?;
        let incomplete_recordings = tx.query_row(
            "SELECT COUNT(*) FROM (
                 SELECT audio_path
                   FROM speech_segments
                  GROUP BY audio_path
                 HAVING SUM(
                     CASE WHEN audio_fingerprint IS NULL
                                OR audio_fingerprint = 0
                                OR audio_content_hash IS NULL
                                OR LENGTH(audio_content_hash) <> 64
                                OR audio_content_hash GLOB '*[^0-9a-f]*'
                          THEN 1 ELSE 0 END
                 ) > 0
             )",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let identities = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT audio_fingerprint, audio_content_hash, audio_path
                   FROM speech_segments
                  WHERE audio_fingerprint IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(StoredAudioIdentity {
                    spectral: row.get::<_, i64>(0)? as u64,
                    content: row.get::<_, Option<String>>(1)?,
                    audio_path: row.get::<_, String>(2)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok((identities, incomplete_recordings.max(0) as usize))
    }

    pub fn revoke_recording(&self, audio_path: &str) -> AppResult<usize> {
        Ok(self.conn.execute(
            "UPDATE speech_segments
                SET rights_revoked_at = COALESCE(rights_revoked_at, datetime('now')),
                    updated_at = datetime('now')
              WHERE audio_path = ?1",
            params![audio_path],
        )?)
    }

    /// Every distinct source recording plus its rights, for the operator view.
    pub fn list_recording_rights(&self) -> AppResult<Vec<(String, usize, RecordingRights)>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, COUNT(*), rights_license, rights_consent_basis, rights_permitted_use,
                    rights_attribution, rights_source, rights_revoked_at
             FROM speech_segments GROUP BY audio_path ORDER BY audio_path",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as usize,
                    RecordingRights {
                        license: r.get(2)?,
                        consent_basis: r.get(3)?,
                        permitted_use: r.get(4)?,
                        attribution: r.get(5)?,
                        source: r.get(6)?,
                        revoked_at: r.get(7)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_hypotheses_for_segment(&self, segment_id: &str) -> AppResult<Vec<SegmentHypothesis>> {
        let mut stmt = self.conn.prepare(
            "SELECT segment_id, model_id, transcript, confidence
             FROM segment_hypotheses WHERE segment_id = ?1
             ORDER BY created_at DESC, model_id ASC",
        )?;
        let rows = stmt.query_map(params![segment_id], |row| {
            Ok(SegmentHypothesis {
                segment_id: row.get(0)?,
                model_id: row.get(1)?,
                transcript: row.get(2)?,
                confidence: row.get(3)?,
            })
        })?;
        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    pub fn delete_hypotheses_for_segment(&self, segment_id: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id = ?1", params![segment_id])?;
        Ok(())
    }

    /// Seal a dataset snapshot: an IMMUTABLE record of exactly which rows a training pack contained.
    ///
    /// `id` is the manifest's content hash, so the same rows always produce the same snapshot id and
    /// a different selection can never masquerade as the same one. INSERT OR IGNORE is the
    /// immutability: re-exporting identical data is a no-op, and an existing snapshot is never
    /// rewritten — a training run that cites a snapshot id must be able to trust that what it cites
    /// has not changed underneath it.
    ///
    /// Returns true when this call sealed a NEW snapshot, false when the id already existed.
    pub fn seal_dataset_snapshot(&self, id: &str, name: &str, config_json: &str) -> AppResult<bool> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO dataset_runs (id, name, status, config_json, completed_at)
             VALUES (?1, ?2, 'sealed', ?3, datetime('now'))",
            params![id, name, config_json],
        )?;
        self.track_write()?;
        Ok(changed > 0)
    }

    /// A sealed snapshot's stored record, or `None` if that id was never sealed.
    pub fn dataset_snapshot(&self, id: &str) -> AppResult<Option<(String, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT name, status, config_json FROM dataset_runs WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?))),
            None => Ok(None),
        }
    }

    /// Record that `record.audio_path` was processed before import (see [`SourceAudioProvenance`]).
    pub fn upsert_source_audio_provenance(&self, record: &SourceAudioProvenance) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO source_audio_provenance
                (audio_path, processing, separator_model, timeline_preserved, manifest_path)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(audio_path) DO UPDATE SET
                processing=excluded.processing,
                separator_model=excluded.separator_model,
                timeline_preserved=excluded.timeline_preserved,
                manifest_path=excluded.manifest_path,
                recorded_at=datetime('now')",
            params![
                record.audio_path,
                record.processing,
                record.separator_model,
                record.timeline_preserved as i32,
                record.manifest_path
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// What was done to this recording before import, or `None` if nothing ever said.
    ///
    /// `None` is NOT a certificate of originality. It means unclaimed — a recording imported before
    /// this table existed reads the same as one that was never processed, which is why the export
    /// wording below states what is known rather than asserting the audio is raw.
    pub fn source_audio_provenance(&self, audio_path: &str) -> AppResult<Option<SourceAudioProvenance>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, processing, separator_model, timeline_preserved, manifest_path
             FROM source_audio_provenance WHERE audio_path = ?1",
        )?;
        let mut rows = stmt.query(params![audio_path])?;
        match rows.next()? {
            Some(row) => Ok(Some(SourceAudioProvenance {
                audio_path: row.get(0)?,
                processing: row.get(1)?,
                separator_model: row.get(2)?,
                timeline_preserved: row.get::<_, i32>(3)? != 0,
                manifest_path: row.get(4)?,
            })),
            None => Ok(None),
        }
    }

    /// Every declaration at once, keyed by source path — one query for a whole export instead of one
    /// per clip (a 550 h corpus is ~250k clips over a few thousand recordings).
    pub fn source_audio_provenance_map(&self) -> AppResult<HashMap<String, SourceAudioProvenance>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, processing, separator_model, timeline_preserved, manifest_path
             FROM source_audio_provenance",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SourceAudioProvenance {
                audio_path: row.get(0)?,
                processing: row.get(1)?,
                separator_model: row.get(2)?,
                timeline_preserved: row.get::<_, i32>(3)? != 0,
                manifest_path: row.get(4)?,
            })
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let record = row?;
            map.insert(record.audio_path.clone(), record);
        }
        Ok(map)
    }

    pub fn upsert_source_transcript(&self, record: &SourceTranscriptRecord) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO source_transcripts
                (audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(audio_path, model_id) DO UPDATE SET
                audio_content_hash=excluded.audio_content_hash,
                audio_size_bytes=excluded.audio_size_bytes,
                transcript_path=excluded.transcript_path,
                transcript_text=excluded.transcript_text,
                updated_at=datetime('now')",
            params![
                record.audio_path,
                record.model_id,
                record.audio_content_hash,
                record.audio_size_bytes,
                record.transcript_path,
                record.transcript_text
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn get_source_transcript(&self, audio_path: &str, model_id: &str) -> AppResult<Option<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1 AND model_id = ?2",
        )?;
        let mut rows = stmt.query(params![audio_path, model_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_latest_source_transcript_for_audio(
        &self,
        audio_path: &str,
    ) -> AppResult<Option<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1
             ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![audio_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_source_transcripts_for_audio(&self, audio_path: &str) -> AppResult<Vec<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1
             -- model_id breaks the tie, and the tie is the COMMON case: both timestamps have
             -- one-second granularity, and two reference transcripts for the same clip are written
             -- back to back. Without it SQLite is free to return them in any order, which made
             -- everything downstream of this list per-run nondeterministic — including the
             -- `multi-reference-consensus:a+b` provenance string that gets persisted and exported.
             ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC, model_id ASC",
        )?;
        let rows = stmt.query_map(params![audio_path], |row| {
            Ok(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// P3.2 (resume fix): segment IDs previously imported from a given source audio file. Used on
    /// import-resume to fold already-imported files back into the post-import jury batch — the jury
    /// runs once at the end keyed on the freshly-imported ids, so without this the files persisted
    /// before a crash would never be adjudicated (they are skipped from re-processing on resume).
    /// Every source file under `dir_prefix` that already has segments in the library.
    ///
    /// The set a re-run of a large directory import passes as `resume_completed`. Without it a
    /// re-run is a FRESH import: `AudioFingerprint::new()` starts empty, so the cross-session
    /// duplicate check cannot see the earlier run, and every already-imported file is processed
    /// again and persisted a SECOND time under the same `audio_path` — the 2026-08-14 shape, where
    /// one folder re-import silently doubled 494 already-reviewed clips.
    ///
    /// Deliberately "has ANY rows", not "has good rows": the importer re-checks each candidate for
    /// placeholder/empty drafts itself (`staged_incomplete`) and re-does the ones that were left
    /// mid-stage. Answering that question here as well would put the same rule in two places.
    ///
    /// Prefix-matched in Rust rather than with SQL `LIKE`: a Windows path is full of `\`, and `_`
    /// is a LIKE wildcard that `lamo_000056.wav` contains twice, so the pattern would need escaping
    /// on two axes at once to avoid silently matching more paths than were asked for.
    pub fn audio_paths_with_segments_under(&self, dir_prefix: &str) -> AppResult<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT audio_path FROM speech_segments")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let prefix = dir_prefix.to_lowercase().replace('/', "\\");
        let mut out = Vec::new();
        for row in rows {
            let path = row?;
            if path.to_lowercase().replace('/', "\\").starts_with(&prefix) {
                out.push(path);
            }
        }
        Ok(out)
    }

    pub fn segment_ids_for_audio_path(&self, audio_path: &str) -> AppResult<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT id FROM speech_segments WHERE audio_path = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![audio_path], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_all_hypotheses(&self) -> AppResult<Vec<SegmentHypothesis>> {
        let mut stmt =
            self.conn.prepare("SELECT segment_id, model_id, transcript, confidence FROM segment_hypotheses")?;
        let rows = stmt.query_map([], |row| {
            Ok(SegmentHypothesis {
                segment_id: row.get(0)?,
                model_id: row.get(1)?,
                transcript: row.get(2)?,
                confidence: row.get(3)?,
            })
        })?;
        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Load the persisted per-model IRT abilities (F7). Empty when learning has never run, in which
    /// case the consensus falls back to the hardcoded heuristic priors (identical to the old behavior).
    pub fn load_model_abilities(&self) -> AppResult<std::collections::HashMap<String, f64>> {
        let mut stmt = self.conn.prepare("SELECT model_id, ability FROM model_abilities")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (id, ability) = r?;
            map.insert(id, ability);
        }
        Ok(map)
    }

    /// Upsert the EM-fitted per-model IRT abilities (F7). Only finite abilities are stored.
    pub fn save_model_abilities(&self, abilities: &std::collections::HashMap<String, f64>) -> AppResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO model_abilities (model_id, ability, updated_at) VALUES (?1, ?2, datetime('now'))
                 ON CONFLICT(model_id) DO UPDATE SET ability = excluded.ability, updated_at = excluded.updated_at",
            )?;
            for (model_id, ability) in abilities {
                if ability.is_finite() {
                    stmt.execute(rusqlite::params![model_id, ability])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The row's database-owned monotonic review revision, encoded as text to preserve the existing
    /// Couch Review JSON wire shape (`rowVersion` was historically a timestamp string). Unlike
    /// `updated_at`, this changes for every update even when several writes land in the same second.
    pub fn segment_row_stamp(&self, segment_id: &str) -> AppResult<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT CAST(review_revision AS TEXT) FROM speech_segments WHERE id = ?1",
                params![segment_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn segment_review_revision(&self, segment_id: &str) -> AppResult<Option<i64>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row("SELECT review_revision FROM speech_segments WHERE id = ?1", params![segment_id], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Returns the number of rows actually changed — human-reviewed rows are skipped by the guard,
    /// so this can be less than `updates.len()`; callers must report THIS, not the attempted count.
    pub fn update_segment_consensus_batch(&self, updates: &[(String, String, String, f64)]) -> AppResult<usize> {
        self.conn.execute("SAVEPOINT consensus_batch", [])?;
        let result: AppResult<usize> = (|| {
            let mut stmt = self.conn.prepare(
                // Guard: never overwrite a human-reviewed/edited segment with machine consensus —
                // mirrors update_asr_transcript_if_unreviewed and merge_dataset_json. Without this,
                // running the consensus refinery silently discards human corrections.
                // `confidence_source` is restamped WITH the confidence it now describes: the stored
                // number becomes an IRT-consensus score, and leaving the decoder's tag (e.g.
                // "real_posterior") on it is a provenance lie — conformal.rs branches on that exact
                // token when counting real-posterior calibration coverage.
                "UPDATE speech_segments
                 SET raw_transcript = ?2,
                     normalized_transcript = ?3,
                     confidence = ?4,
                     confidence_source = 'irt_consensus',
                     updated_at = datetime('now')
                 WHERE id = ?1
                   AND verified = 0
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            )?;
            let mut changed = 0usize;
            for (seg_id, cons, norm, conf) in updates {
                // NFC-canonicalize the consensus transcript + its normalization before they hit the
                // FTS-indexed columns — same guard as the other write paths, so machine consensus
                // doesn't store a decomposed form that search can't match.
                changed += stmt.execute(params![seg_id, to_nfc(cons), to_nfc(norm), conf])?;
            }
            Ok(changed)
        })();
        match result {
            Ok(changed) => {
                self.release_savepoint("consensus_batch")?;
                self.track_write()?;
                Ok(changed)
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("consensus_batch");
                Err(e)
            }
        }
    }

    pub fn update_ctc_score(&self, id: &str, score: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET ctc_score = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, score],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_signal_anomaly_score(&self, id: &str, score: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET signal_anomaly_score = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, score],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_segment_split(&self, id: &str, split: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET split = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, split],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_quality_metrics(&self, id: &str, clipping: f64, rms: f64, snr: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET clipping_ratio = ?2, rms_db = ?3, snr_db = ?4, updated_at = datetime('now') WHERE id = ?1",
            params![id, clipping, rms, snr],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub(super) fn track_write(&self) -> AppResult<()> {
        // Placeholder for write-tracking if needed by external observers
        Ok(())
    }

    /// Persist word timings AND their honest quality marker in ONE atomic statement.
    /// `alignment_json` is metadata (chunk window + per-word timings), NOT FTS-indexed transcript
    /// text, so no NFC canonicalization is needed. `quality`: "ctc_forced" | "energy_heuristic".
    ///
    /// These two columns must never be written as separate statements: quality.rs raises the
    /// `energy_heuristic_alignment` review-risk reason only when the marker is PRESENT, so timings
    /// that land without their marker read as trustworthy alignment. The old two-statement pair had
    /// exactly that window — and the background aligner swallowed the second write's error outright
    /// (`let _ =`), silently laundering heuristic timestamps whenever the quality stamp failed.
    pub fn update_segment_alignment(&self, segment_id: &str, alignment_json: &str, quality: &str) -> AppResult<()> {
        crate::validation::input::validate_alignment_json(alignment_json).map_err(AppError::Validation)?;
        self.conn.execute(
            "UPDATE speech_segments
             SET alignment_json = ?2, alignment_quality = ?3, updated_at = datetime('now')
             WHERE id = ?1",
            params![segment_id, alignment_json, quality],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Persist alignment output only if the canonical alignment read before slow inference is still
    /// current. SQLite `IS` is deliberately used instead of `=` so `None` compares NULL-safely. A
    /// concurrent boundary/timing edit returns `Ok(false)` and is never overwritten.
    pub fn update_segment_alignment_if_unchanged(
        &self,
        segment_id: &str,
        expected_alignment: Option<&str>,
        alignment_json: &str,
        quality: &str,
    ) -> AppResult<bool> {
        crate::validation::input::validate_alignment_json(alignment_json).map_err(AppError::Validation)?;
        let changed = self.conn.execute(
            "UPDATE speech_segments
             SET alignment_json = ?3, alignment_quality = ?4, updated_at = datetime('now')
             WHERE id = ?1 AND alignment_json IS ?2",
            params![segment_id, expected_alignment, alignment_json, quality],
        )?;
        if changed > 0 {
            self.track_write()?;
        }
        Ok(changed > 0)
    }

    /// Record the within-clip speaker-change measurement (Migration v47).
    ///
    /// Writes ONE column. Nothing here may touch the transcript, the verdict or the human decision:
    /// this is a measurement about the audio, and the tool that produces it runs over the whole
    /// library at once — a wider write would put every reviewed row in its blast radius.
    ///
    /// `updated_at` is deliberately NOT bumped. It means "when did this row's CONTENT last change",
    /// and re-running a measurement changes nothing a reviewer or an export would read differently.
    pub fn set_speaker_change_score(&self, segment_id: &str, score: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET speaker_change_score = ?2 WHERE id = ?1",
            params![segment_id, score],
        )?;
        self.track_write()?;
        Ok(())
    }
}
