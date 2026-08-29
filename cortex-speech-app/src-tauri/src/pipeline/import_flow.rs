//! Champion-first file import, atomic publication and resumable import orchestration.

use super::*;

impl ProcessingPipeline {
    pub fn import_directory(
        &self,
        dir_path: &Path,
        cancel: Option<CancellationToken>,
        callback: impl Fn(PipelineEvent),
    ) -> AppResult<()> {
        self.import_directory_with_agent_run_id(dir_path, cancel, None, None, None, callback)
    }

    /// `resume_completed` names already-persisted files from a crashed run; `None` keeps fresh-import behavior.
    /// `resume_job_id` names the pre-published successor journal this worker must validate and continue.
    pub fn import_directory_with_agent_run_id(
        &self,
        dir_path: &Path,
        cancel: Option<CancellationToken>,
        agent_run_id: Option<&str>,
        resume_completed: Option<&std::collections::HashSet<String>>,
        resume_job_id: Option<&str>,
        callback: impl Fn(PipelineEvent),
    ) -> AppResult<()> {
        let db = self.open_db()?;
        let audio_exts = ["wav", "mp3", "flac", "m4a", "ogg", "aac", "opus", "mp4", "mov", "wma", "webm"];
        let mut files = Vec::new();

        fn collect_audio_files(
            dir: &Path,
            exts: &[&str],
            files: &mut Vec<std::path::PathBuf>,
            depth: usize,
        ) -> std::io::Result<()> {
            if depth > 32 {
                return Ok(());
            }
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    collect_audio_files(&path, exts, files, depth + 1)?;
                } else if path.is_file() {
                    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).unwrap_or_default();
                    if exts.contains(&ext.as_str()) {
                        files.push(path);
                    }
                }
            }
            Ok(())
        }

        collect_audio_files(dir_path, &audio_exts, &mut files, 0)?;

        let source_paths: Vec<String> = files.iter().map(|path| path.to_string_lossy().to_string()).collect();
        let total = files.len();
        callback(PipelineEvent::Started { total });
        self.reset_finetuned_counters();
        callback(PipelineEvent::Phase { phase: "importing".into() });
        self.set_import_status(0, total, "");
        // An empty FRESH selection is a successful no-op, not an import generation. A resume already
        // owns a durable successor journal, however: silently reporting success would leave that row
        // running and surface the same interruption again after restart. Fail while retaining it so
        // the missing/moved source directory can be repaired without losing recovery authority.
        if total == 0 {
            if resume_job_id.is_some() {
                self.finish_import_status();
                return Err(AppError::Validation(
                    "Resume folder contains no supported audio files; the durable import journal was retained".into(),
                ));
            }
            callback(PipelineEvent::Completed { total: 0, succeeded: 0, failed: 0 });
            self.finish_import_status();
            return Ok(());
        }
        // RAII: clear import_status.running on EVERY exit path. The per-file `token.check()?` cancel
        // below and every durable-journal failure can early-return before the manual
        // finish_import_status() calls. Without this guard, either path leaves get_import_status()
        // reporting running:true forever.
        struct ImportStatusGuard<'a>(&'a ProcessingPipeline);
        impl Drop for ImportStatusGuard<'_> {
            fn drop(&mut self) {
                self.0.finish_import_status();
            }
        }
        let _status_guard = ImportStatusGuard(self);
        // P3.2: open the durable resume journal before any file can publish segment rows. A journal
        // failure is fatal: reporting a successful import without recovery evidence would make a
        // crash window silently non-resumable.
        let import_jobs = self.import_job_store()?;
        let import_writes = self.import_write_store(db.path())?;
        let dir_text = dir_path.to_string_lossy();
        let job_id = if let Some(job_id) = resume_job_id {
            if resume_completed.is_none() {
                return Err(AppError::Validation(
                    "A claimed resume journal requires resume authority; refusing to run it as a fresh import".into(),
                ));
            }
            import_jobs.continue_import(job_id, &dir_text, total).map_err(|error| {
                AppError::Other(format!(
                    "Could not admit the claimed durable resume journal before audio work: {error}"
                ))
            })?;
            job_id.to_string()
        } else {
            import_jobs.begin_import(&dir_text, total).map_err(|error| {
                AppError::Other(format!("Could not create the durable import recovery journal: {error}"))
            })?
        };
        let mut succeeded = 0;
        let failed = 0; // halt-on-first-failure (2026-08-20): a COMPLETED import has zero failures by definition
        let mut imported_ids = Vec::new();

        // A resume journal is only a hint about where the old process reached. Build the authority
        // inventory from the current database, and bind it to the exact champion plus the current
        // canonical PCM before any row can be adopted. Normalising only for lookup preserves the
        // Windows case/separator re-run fix without changing the stored source path.
        let (resume_champion_model_id, resume_paths_by_key, resume_journal_keys) = if let Some(done) = resume_completed
        {
            let champion_model_id = crate::review_pool::current_champion_7b_model_id(&db).map_err(|error| {
                AppError::Other(format!(
                    "Resume cannot establish the current OmniASR-7B champion; no prior row was adopted: {error}"
                ))
            })?;
            let mut paths_by_key: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
            for stored in db.audio_paths_with_segments_under(&dir_path.to_string_lossy()).map_err(|error| {
                AppError::Other(format!(
                    "Resume could not inventory existing source paths under {}; no prior row was adopted: {error}",
                    dir_path.display()
                ))
            })? {
                paths_by_key.entry(resume_path_key(&stored)).or_default().push(stored);
            }
            let journal_keys = done.iter().map(|path| resume_path_key(path)).collect();
            (Some(champion_model_id), paths_by_key, journal_keys)
        } else {
            (None, std::collections::HashMap::new(), std::collections::HashSet::new())
        };

        for (idx, file) in files.iter().enumerate() {
            if let Some(ref token) = cancel {
                token.check()?;
            }

            // Resume authority can delete a replaceable pre-publication stage, so its canonical-PCM
            // comparison needs the same immutable-source guarantee as a fresh import. Keep this
            // outer lease through resume inspection, any retry, publication, and durable journal
            // completion. The per-file processor deliberately takes its own compatible read lease,
            // making direct/single-file callers safe as well.
            let file_source_lease = crate::media::seal_import_source(file).map_err(|error| {
                AppError::Other(format!(
                    "Import source {} could not be held immutable through resume/publication: {error}",
                    file.display()
                ))
            })?;
            let file = file_source_lease.source_path();

            let fname = file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
            let file_path_str = file.to_string_lossy().to_string();

            // The old journal cannot make a row authoritative. Fetch every row under this logical
            // Windows path (including case/separator variants), require a complete human/champion
            // transcript on every row, and require every stored source identity to match the exact
            // canonical PCM decoded in this run. A stale journal with zero rows therefore processes
            // the file; placeholder/wrong-model/cloud/source-drift stages are rolled back and redone.
            let file_key = resume_path_key(&file_path_str);
            let journaled = resume_journal_keys.contains(&file_key);
            let mut resume_existing_ids = Vec::new();
            if resume_completed.is_some() {
                let stored_paths = resume_paths_by_key.get(&file_key).cloned().unwrap_or_default();
                if stored_paths.len() > 1 {
                    return Err(AppError::Validation(format!(
                        "Resume found multiple stored path spellings for {} ({:?}); refusing to choose one or adopt duplicate authority",
                        file.display(),
                        stored_paths
                    )));
                }
                let lookup_path = stored_paths.first().map(String::as_str).unwrap_or(file_path_str.as_str());
                resume_existing_ids = db.segment_ids_for_audio_path(lookup_path).map_err(|error| {
                    AppError::Other(format!(
                        "Resume could not determine whether {file_path_str} already has durable segments; import halted without reprocessing: {error}"
                    ))
                })?;
            }

            let mut has_authoritative_segments = false;
            if !resume_existing_ids.is_empty() {
                let segments = db.get_segments_by_ids(&resume_existing_ids).map_err(|error| {
                    AppError::Other(format!(
                        "Resume could not read the durable rows for {file_path_str}; import halted without rollback: {error}"
                    ))
                })?;
                if segments.len() != resume_existing_ids.len() {
                    return Err(AppError::Other(format!(
                        "Resume read only {}/{} durable rows for {file_path_str}; refusing partial authority",
                        segments.len(),
                        resume_existing_ids.len()
                    )));
                }
                let champion_model_id = resume_champion_model_id.as_deref().ok_or_else(|| {
                    AppError::Other("resume authority was requested without a champion identity".to_string())
                })?;
                let current_duration_ms = audio::get_duration_ms(file).map_err(|error| {
                    AppError::Other(format!(
                        "Resume could not verify the current canonical audio for {file_path_str}; existing rows were left untouched: {error}"
                    ))
                })?;
                let current_identity = self
                    .identity_for_existing_source(file, current_duration_ms, cancel.as_ref())
                    .map_err(|error| {
                        AppError::Other(format!(
                            "Resume could not verify the current canonical audio for {file_path_str}; existing rows were left untouched: {error}"
                        ))
                    })?;
                let source_identity_matches = resume_existing_ids.iter().try_fold(true, |matches, segment_id| {
                    db.segment_audio_content_hash(segment_id)
                        .map(|stored| matches && stored.as_deref() == Some(current_identity.content.as_str()))
                })?;
                has_authoritative_segments = source_identity_matches
                    && segments
                        .iter()
                        .all(|segment| resume_segment_has_authoritative_transcript(segment, champion_model_id));
            }

            if !resume_existing_ids.is_empty() && !has_authoritative_segments {
                tracing::warn!(
                    "resume: {} has {} row(s), but they do not prove current human/champion transcript and canonical-audio authority (journaled={}); discarding the replaceable stage and re-importing",
                    file_path_str,
                    resume_existing_ids.len(),
                    journaled
                );
                import_writes.rollback_segments(&resume_existing_ids)?;
                // The database is authority. Only after its complete rollback succeeds may the
                // in-memory dedup index forget this source; otherwise the old pre-publication cache
                // entry would reject the exact recovery attempt until the whole app restarted.
                self.fingerprint.forget_source(file);
            }
            if resume_should_skip_file(resume_completed.is_some(), has_authoritative_segments) {
                import_jobs.mark_import_file_done(&job_id, &file_path_str).map_err(|error| {
                    AppError::Other(format!(
                        "Could not durably journal resumed file {file_path_str}; import halted: {error}"
                    ))
                })?;
                succeeded += 1;
                // Fold the already-imported file's segments back into the jury batch. The post-import
                // jury (below) runs once at the end keyed on `imported_ids`; a crash interrupts BEFORE
                // that jury ever runs, so segments persisted pre-crash were never adjudicated. Skipping
                // them silently would leave them persisted-but-un-adjudicated (no reference commit, no
                // review routing). Adopt the ids fetched above so the end-of-run jury covers the whole
                // resumed import. Non-destructive: existing rows are adopted, never deleted (a reviewed
                // earlier import may legitimately share this audio_path).
                imported_ids.extend(resume_existing_ids);
                callback(PipelineEvent::Progress {
                    current: idx + 1,
                    total,
                    file: fname.clone(),
                    status: "Already imported — re-adjudicating (resume)".into(),
                });
                continue;
            }

            callback(PipelineEvent::Progress {
                current: idx + 1,
                total,
                file: fname.clone(),
                status: "Processing...".into(),
            });
            self.set_import_status(idx + 1, total, &fname);
            let source_reference_enabled = self.source_reference_enabled();
            if source_reference_enabled {
                callback(PipelineEvent::Phase { phase: "reference_transcribing".into() });
                callback(agent_stage(
                    "source_reference",
                    "running",
                    fname.clone(),
                    "Building whole-file reference transcript",
                    idx + 1,
                    total,
                ));
                callback(PipelineEvent::Progress {
                    current: idx + 1,
                    total,
                    file: fname.clone(),
                    status: "Building whole-file reference transcript".into(),
                });
            }

            let meta = crate::telemetry::Tracer::metadata(vec![
                ("file", fname.clone()),
                ("path", file.to_string_lossy().to_string()),
                ("index", (idx + 1).to_string()),
                ("total", total.to_string()),
            ]);
            // Thread the directory-import cancel token into per-file processing so Cancel interrupts the
            // CURRENT file (its VAD/ASR/per-segment 7B loop), not only the gap between files — a long
            // audiobook file could otherwise keep running for minutes after Cancel.
            let mut result = crate::telemetry::TRACER.record_result("pipeline.import_file", meta, || {
                self.process_single_file_with_progress(file, &db, cancel.as_ref(), |_, _| {})
            });

            if let Err(ref e) = result {
                if audio::is_transient_decode_error(e) {
                    tracing::warn!("Transient decode error for {}, retrying once: {e}", file.display());
                    std::thread::sleep(Duration::from_millis(500));
                    result = self.process_single_file_with_progress(file, &db, cancel.as_ref(), |_, _| {});
                }
            }

            match result {
                Ok(segments) => {
                    callback(PipelineEvent::Phase { phase: "transcribing".into() });
                    let segment_count = segments.len();
                    if source_reference_enabled {
                        callback(agent_stage(
                            "source_reference",
                            "completed",
                            fname.clone(),
                            "Whole-file source reference stage completed or reused",
                            idx + 1,
                            total,
                        ));
                    }
                    callback(agent_stage(
                        "audio_chunking",
                        "completed",
                        fname.clone(),
                        format!("{segment_count} speech chunk(s) persisted"),
                        segment_count,
                        segment_count.max(1),
                    ));
                    callback(multi_model_hypothesis_stage(&db, &self.settings, fname.clone(), &segments));
                    // Journal publication is part of declaring the file complete. Segment rows may
                    // already be durable if this write fails; the resume gap guard above adopts those
                    // exact rows on the next attempt instead of duplicating them.
                    import_jobs.mark_import_file_done(&job_id, &file_path_str).map_err(|error| {
                        AppError::Other(format!(
                            "Could not durably journal completed file {file_path_str}; import halted: {error}"
                        ))
                    })?;
                    succeeded += 1;
                    imported_ids.extend(segments.iter().map(|s| s.id.clone()));
                    if segments.len() > 1 {
                        tracing::info!("Imported {} annotatable segments from {}", segments.len(), file.display());
                    }
                }
                Err(e) => {
                    // HALT ON THE FIRST REAL FAILURE (owner rule 2026-08-11; wired here 2026-08-20).
                    // This arm used to count the failure and continue to the next file, ending with
                    // `Completed { failed: n }` and Ok(()) — the exact "partly-drafted dataset that
                    // looks finished" the champion law forbids, one directory level up from where
                    // batch_transcribe already halts. The resume journal makes halting cheap: every
                    // finished file is journaled, so re-running the import picks up exactly here.
                    callback(PipelineEvent::Error { file: fname.clone(), error: e.to_string() });
                    // _status_guard's Drop clears the running flag on this early return.
                    return Err(AppError::Other(format!(
                        "import HALTED at {fname} ({succeeded} file(s) completed before it): {e}. \
                         Nothing after it was attempted — fix the cause and re-import; completed files resume as done."
                    )));
                }
            }
            drop(file_source_lease);
        }

        if !imported_ids.is_empty() {
            callback(PipelineEvent::Phase { phase: "adjudicating".into() });
            callback(agent_stage(
                "jury_adjudication",
                "running",
                "post-import jury",
                format!("Adjudicating {} imported segment(s)", imported_ids.len()),
                0,
                imported_ids.len(),
            ));
            let mut report_options = crate::runs::AgentImportReportOptions::from_settings(&self.settings);
            report_options.agent_run_id = agent_run_id.map(str::to_string);
            let model_status = self.model_manager.status();
            let external_provider = crate::commands::external_provider_status(&self.settings);
            report_options.agentic_readiness = Some(crate::commands::build_agentic_readiness_snapshot(
                &self.settings,
                &model_status,
                &external_provider,
            ));
            match crate::commands::run_jury_pipeline_core(&db, &self.settings, imported_ids.clone()) {
                Ok(jury_report) => {
                    callback(agent_stage(
                        "jury_adjudication",
                        "completed",
                        "post-import jury",
                        format!(
                            "Reference commits: {}; review queue: {}",
                            jury_report["referenceCommitted"].as_u64().unwrap_or(0),
                            jury_report["humanInbox"].as_u64().unwrap_or(0)
                        ),
                        imported_ids.len(),
                        imported_ids.len(),
                    ));
                    if let Err(error) = crate::runs::record_agent_import_report_with_options(
                        &db,
                        "directory",
                        &source_paths,
                        &imported_ids,
                        Some(&jury_report),
                        None,
                        report_options,
                    ) {
                        let message = format!("Agent import report persistence failed after directory import: {error}");
                        tracing::error!("{message}");
                        callback(PipelineEvent::Error { file: "agent import report".into(), error: message.clone() });
                        self.finish_import_status();
                        return Err(AppError::Other(message));
                    }
                    callback(agent_stage(
                        "agent_report",
                        "completed",
                        "agent import report",
                        "Persisted auditable multi-agent import report",
                        imported_ids.len(),
                        imported_ids.len(),
                    ));
                }
                Err(error) => {
                    let mut message = format!("Post-import jury adjudication failed after directory import: {error}");
                    if let Err(report_error) = crate::runs::record_agent_import_report_with_options(
                        &db,
                        "directory",
                        &source_paths,
                        &imported_ids,
                        None,
                        Some(&error),
                        report_options,
                    ) {
                        message
                            .push_str(&format!("; additionally failed to persist agent import report: {report_error}"));
                    }
                    tracing::error!("{message}");
                    callback(agent_stage(
                        "jury_adjudication",
                        "blocked",
                        "post-import jury",
                        message.clone(),
                        0,
                        imported_ids.len(),
                    ));
                    callback(PipelineEvent::Error { file: "post-import jury".into(), error: message.clone() });
                    self.finish_import_status();
                    return Err(AppError::Other(message));
                }
            }
        }

        // P3.2: completion is durable evidence, not a best-effort decoration. If terminal stamping
        // fails, leave the running journal visible so recovery never mistakes the import for clean.
        import_jobs.complete_import(&job_id).map_err(|error| {
            AppError::Other(format!("Could not durably complete the import recovery journal: {error}"))
        })?;
        // F2: a fine-tuned→stock downgrade during this import must end LOUD, not log-only.
        {
            let attempts = self.finetuned_attempts.load(std::sync::atomic::Ordering::Relaxed);
            let fallbacks = self.finetuned_fallbacks.load(std::sync::atomic::Ordering::Relaxed);
            if let Some(error) = Self::finetuned_downgrade_message(attempts, fallbacks) {
                tracing::error!("finetuned downgrade on import: {error}");
                callback(PipelineEvent::Error { file: "fine-tuned engine".into(), error });
            }
        }
        callback(PipelineEvent::Completed { total, succeeded, failed });
        self.finish_import_status();
        Ok(())
    }

    /// Decode one source file and persist one or more `SpeechSegment` rows (VAD chunking for long audio).
    pub fn process_single_file(&self, path: &Path, db: &Database) -> AppResult<Vec<SpeechSegment>> {
        self.process_single_file_with_progress(path, db, None, |_, _| {})
    }

    /// Compute the one canonical whole-source identity while retaining bounded memory for long
    /// recordings. Canonical identity is always the concatenation of fixed 90-second, mono 16 kHz
    /// decode windows. It must not switch to whole-buffer resampling with review-chunk settings: a
    /// 44.1/48 kHz source resampled independently at window boundaries is byte-different from the same
    /// source resampled in one pass, so a setting change would otherwise make unchanged audio look
    /// replaced and let a cross-path duplicate acquire another identity.
    fn identity_for_existing_source(
        &self,
        path: &Path,
        duration_ms: i64,
        cancel: Option<&CancellationToken>,
    ) -> AppResult<crate::fingerprint::AudioIdentity> {
        let decode_timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);
        let mut identity = crate::fingerprint::StreamingIdentity::new();
        let mut samples_seen = false;
        audio::decode_pcm_windows_streaming(
            path.to_path_buf(),
            audio::DECODE_WINDOW_MS,
            decode_timeout.min(MAX_WINDOW_DECODE_WAIT),
            |window, _| {
                if let Some(token) = cancel {
                    token.check()?;
                }
                if !window.pcm.is_empty() {
                    let (sample_rate, pcm) = audio::ensure_pcm_16khz(window.sample_rate, window.pcm)?;
                    if !pcm.is_empty() {
                        samples_seen = true;
                        identity.push(&pcm, sample_rate);
                    }
                }
                Ok(())
            },
        )?;
        if !samples_seen {
            return Err(AppError::Validation("Existing source path now decodes to empty audio".into()));
        }
        Ok(identity.finish())
    }

    /// Decode a non-streaming import into bounded memory using the exact same fixed-window resampling
    /// protocol used by long imports and later source-identity verification. Collecting the already
    /// canonical windows avoids a second file pass and, crucially, binds ASR PCM and the persisted
    /// identity to the same bytes. Whole-file decode followed by a separate identity pass would reopen
    /// a file-mutation race between transcript input and identity publication.
    fn decode_canonical_pcm_buffered(
        &self,
        path: &Path,
        decode_timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> AppResult<(u32, Vec<i16>)> {
        let mut pcm = Vec::new();
        let mut saw_audio = false;
        audio::decode_pcm_windows_streaming(
            path.to_path_buf(),
            audio::DECODE_WINDOW_MS,
            decode_timeout.min(MAX_WINDOW_DECODE_WAIT),
            |window, _| {
                if let Some(token) = cancel {
                    token.check()?;
                }
                if window.sample_rate != audio::TARGET_SAMPLE_RATE {
                    return Err(AppError::Validation(format!(
                        "Canonical import decoder returned unexpected sample rate {}",
                        window.sample_rate
                    )));
                }
                if window.pcm.is_empty() {
                    return Ok(());
                }
                let next_len = pcm.len().checked_add(window.pcm.len()).ok_or_else(|| {
                    AppError::Validation("Canonical import PCM length overflowed the platform limit".into())
                })?;
                if next_len > MAX_PCM_SAMPLES {
                    return Err(AppError::Validation(format!(
                        "Canonical buffered import exceeded the {MAX_PCM_SAMPLES}-sample memory bound; use streaming import"
                    )));
                }
                pcm.try_reserve(window.pcm.len()).map_err(|_| {
                    AppError::Validation("Canonical import PCM could not reserve bounded memory".into())
                })?;
                pcm.extend_from_slice(&window.pcm);
                saw_audio = true;
                Ok(())
            },
        )?;
        if !saw_audio {
            return Err(AppError::Validation("Empty audio buffer".into()));
        }
        Ok((audio::TARGET_SAMPLE_RATE, pcm))
    }

    /// Return the already-published rows only when the current canonical audio and every transcript
    /// authority match exactly. Any ambiguity hard-stops without deleting or modifying existing data.
    fn adopt_existing_source_if_authoritative(
        &self,
        path: &Path,
        db: &Database,
        duration_ms: i64,
        cancel: Option<&CancellationToken>,
    ) -> AppResult<Option<Vec<SpeechSegment>>> {
        let path_text = path.to_string_lossy();
        let ids = db.segment_ids_for_audio_path(&path_text)?;
        if ids.is_empty() {
            return Ok(None);
        }
        let segments = db.get_segments_by_ids(&ids)?;
        if segments.len() != ids.len() {
            return Err(AppError::Other(format!(
                "Existing source {} returned only {}/{} durable rows; refusing partial adoption",
                path.display(),
                segments.len(),
                ids.len()
            )));
        }

        let identity = self.identity_for_existing_source(path, duration_ms, cancel)?;
        let source_matches = ids.iter().try_fold(true, |matches, segment_id| {
            db.segment_audio_content_hash(segment_id)
                .map(|stored| matches && stored.as_deref() == Some(identity.content.as_str()))
        })?;
        if !source_matches {
            return Err(AppError::Validation(format!(
                "Source {} already has durable rows, but its current audio identity does not match them; existing data was left untouched",
                path.display()
            )));
        }

        let authoritative = match crate::review_pool::current_champion_7b_model_id(db) {
            Ok(champion_model_id) => {
                segments.iter().all(|segment| resume_segment_has_authoritative_transcript(segment, &champion_model_id))
            }
            Err(error) => {
                // Human truth remains adoptable even if the model registry is temporarily unavailable.
                // Passing an impossible empty champion accepts only the helper's human-authority arm.
                if segments.iter().all(|segment| resume_segment_has_authoritative_transcript(segment, "")) {
                    true
                } else {
                    return Err(AppError::Other(format!(
                        "Source {} already has machine-authored rows, but current champion authority could not be established; existing data was left untouched: {error}",
                        path.display()
                    )));
                }
            }
        };
        if !authoritative {
            return Err(AppError::Validation(format!(
                "Source {} already has rows that do not all prove current human/champion transcript authority; use interrupted-import recovery instead of creating duplicates",
                path.display()
            )));
        }

        tracing::info!(
            source = %path.display(),
            segments = segments.len(),
            "Adopted exact authoritative import after retry; no new segment IDs were created"
        );
        Ok(Some(segments))
    }

    fn process_single_file_with_progress(
        &self,
        path: &Path,
        db: &Database,
        cancel: Option<&CancellationToken>,
        on_chunk: impl FnMut(usize, usize),
    ) -> AppResult<Vec<SpeechSegment>> {
        // Freeze the exact source object before duration probing, optional whole-file cloud
        // reference work, decoding, inference, and publication. The inner method requires a borrow
        // of the unforgeable lease, so no future direct caller can accidentally shorten this
        // lifetime and reintroduce a verify/use race.
        let source_lease = crate::media::seal_import_source(path).map_err(|error| {
            AppError::Other(format!("Import source could not be held immutable through publication: {error}"))
        })?;
        self.process_single_file_under_source_lease(source_lease.source_path(), db, cancel, on_chunk, &source_lease)
    }

    fn process_single_file_under_source_lease(
        &self,
        path: &Path,
        db: &Database,
        cancel: Option<&CancellationToken>,
        mut on_chunk: impl FnMut(usize, usize),
        _source_lease: &crate::media::ImportMediaSourceLease,
    ) -> AppResult<Vec<SpeechSegment>> {
        if let Some(token) = cancel {
            token.check()?;
        }

        let duration_ms = audio::get_duration_ms(path)?;
        if duration_ms == 0 {
            return Err(AppError::Validation("Empty audio file".into()));
        }
        if let Some(existing) = self.adopt_existing_source_if_authoritative(path, db, duration_ms, cancel)? {
            on_chunk(existing.len(), existing.len().max(1));
            return Ok(existing);
        }
        let import_writes = self.import_write_store(db.path())?;

        // Capture the cleaner's declaration before decode, but do not publish it independently.
        // Segment rows, champion evidence, recording identity and this preprocessing claim cross one
        // database boundary below; a provenance write failure hard-stops with zero rows instead of
        // silently presenting processed audio as unclaimed/raw-looking training material.
        let source_provenance = crate::source_provenance::detect(path);
        if let Some(provenance) = source_provenance.as_ref() {
            tracing::info!("source audio declared as processed before import: {}", provenance.processing);
        }

        // F2: fail fast BEFORE any decode/VAD/diarization work if the selected primary engine can't
        // actually run — never silently transcribe the whole import with the stock model.
        if self.wsl7b_primary_unresolved() {
            return Err(Self::primary_engine_unavailable_error());
        }
        // F6: when the WSL 7B is primary, confirm its warm server is up before doing any work, so a
        // down server fails in ~2 s with an actionable message instead of a ~5-minute per-segment hang.
        self.wsl_7b_server_preflight()?;

        self.ensure_source_reference_transcripts(path, db).map_err(|error| {
            AppError::Other(format!(
                "Whole-file reference transcript failed before chunking {}: {error}",
                path.display()
            ))
        })?;
        if let Some(token) = cancel {
            token.check()?;
        }

        let decode_timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);

        if chunking::should_stream_decode(duration_ms, self.settings.max_segment_duration_ms) {
            return self.process_single_file_streaming(
                path,
                db,
                decode_timeout,
                on_chunk,
                StreamingImportContext { duration_ms, cancel, source_provenance: source_provenance.as_ref() },
            );
        }

        let (sample_rate, pcm) = self.decode_canonical_pcm_buffered(path, decode_timeout, cancel)?;

        let (identity, fingerprint_reservation) = self
            .fingerprint
            .reserve_import(&pcm, sample_rate, Some(path))
            .map_err(|e| AppError::Validation(e.into()))?;
        // v50: the value used to be computed here and thrown away as `_fp`, which is why duplicate
        // detection could not survive a restart. Stamped onto the rows AFTER persist_segments below,
        // once they exist — see the set_audio_identity call there. v51: BOTH tiers travel together, so
        // the rejection rule after a restart is the same cryptographic one it is during this run.

        // The embedding service is acquired BEFORE chunk planning (it used to come after), because the
        // planner now asks it who is speaking at every candidate merge: boundaries were planned by
        // silence alone and labels attached to whole chunks afterwards, so a two-host podcast glued
        // both voices into one chunk under one confident SPEAKER_0x (owner hit this twice reviewing,
        // 2026-08-17). The judge can only REFUSE a merge — with CAM++ absent it returns None and the
        // plan is exactly the historical silence-only one.
        // Do not even construct optional Sherpa services when their setting is off. Apart from wasting
        // RAM/CPU, constructing an unused denoiser used to probe the CUDA execution provider and emit a
        // frightening "CUDA not enabled" diagnostic during a champion-only import. More importantly,
        // disabled must mean the model cannot influence chunking or audio bytes—not merely that its
        // output is ignored later.
        let mut diarization_guard = None;
        if self.settings.enable_diarization {
            let mut guard = self.lock_diarization_service();
            // Rebuild when unset OR cached-INACTIVE (see the denoiser site below): caching an inactive
            // service ignored a CAM++ model downloaded mid-session until an app restart. Cheap while absent.
            if guard.as_ref().map_or(true, |s| !s.is_available()) {
                // Per-file (round-26): resolve_root_for avoids resolved_dir()'s all-or-nothing orphan of the
                // bundled-only campp speaker model once the user downloads OmniASR into the user dir.
                let model_dir = self.model_manager.resolve_root_for(crate::models::CAMPP_MODEL);
                *guard = Some(crate::diarization::SpeakerEmbeddingService::new(&model_dir));
            }
            diarization_guard = Some(guard);
        }
        let embedding_service = diarization_guard.as_ref().and_then(|guard| guard.as_ref());

        let (chunk_ranges, vad_backend) = if let Some(service) = embedding_service {
            let judge = crate::diarization::speaker_turn_judge(service, sample_rate);
            chunking::plan_speech_chunks_with_judge(
                &pcm,
                sample_rate,
                self.settings.vad_threshold,
                self.settings.min_segment_duration_ms,
                self.settings.max_segment_duration_ms,
                Some(&judge),
            )?
        } else {
            chunking::plan_speech_chunks(
                &pcm,
                sample_rate,
                self.settings.vad_threshold,
                self.settings.min_segment_duration_ms,
                self.settings.max_segment_duration_ms,
            )?
        };

        let mut denoiser_guard = None;
        if self.settings.enable_denoising {
            let mut guard = self.lock_denoiser_service();
            // Rebuild when unset OR cached-INACTIVE: an inactive service means the model was absent when it
            // was first built, so caching that pass-through for the whole session ignored a denoiser
            // downloaded mid-session until an app restart (hunt-10 #3) — and the export's fresh-service
            // denoising flag then read `true` over un-denoised audio. The absent-path rebuild is a cheap
            // path.exists() stat; once the model appears the load runs once and is_active() latches true.
            if guard.as_ref().map_or(true, |s| !s.is_active()) {
                // Per-file (round-26): resolved_dir() is all-or-nothing, so a bundled-only or user-downloaded
                // denoiser is orphaned once OmniASR flips the root. resolve_root_for loads it from wherever it is.
                let model_dir = self.model_manager.resolve_root_for(crate::models::DENOISER_MODEL);
                *guard = Some(crate::denoiser::DenoiserService::new(&model_dir));
            }
            denoiser_guard = Some(guard);
        }
        let denoiser_service = denoiser_guard.as_ref().and_then(|guard| guard.as_ref());

        // Once per file — see the parameter's doc on build_segments_from_pcm.
        let file_hash = crate::cache::TranscriptCache::compute_hash(path).ok();
        let (segments, pcm_cache) = self.build_segments_from_pcm(
            path,
            &pcm,
            sample_rate,
            0,
            &chunk_ranges,
            vad_backend,
            cancel,
            embedding_service,
            denoiser_service,
            &mut on_chunk,
            None, // non-streaming: the whole file is one call, so diarization clusters in-place
            file_hash.as_deref(),
        )?;
        let mut prepared = segments;
        let champion_deployment = self.run_primary_wsl_pass_for_import(&mut prepared, cancel)?;
        let persisted = if let Some(deployment_sha256) = champion_deployment.as_deref() {
            // Canonical publication happens only after every champion/refiner call succeeded. Segment
            // rows, sole champion hypotheses and cross-session audio identity share one savepoint.
            import_writes.publish_champion_segments(
                &prepared,
                deployment_sha256,
                Some(&identity),
                source_provenance.as_ref(),
            )?;
            prepared
        } else if identity.spectral != 0 {
            // Compatibility publication is still one source operation: rows and identity commit
            // together, and a changed recording at the same logical path rolls the whole batch back.
            import_writes.publish_segments_with_identity(&prepared, &identity, source_provenance.as_ref())?;
            prepared
        } else {
            self.persist_segments(&import_writes, prepared, source_provenance.as_ref())?
        };
        // Rows plus source identity are now durable. Before this point every `?`, cancellation and
        // champion failure drops the reservation and makes an exact retry available in this session.
        fingerprint_reservation.commit();
        // Deferred to AFTER the 7B pass so both evaluate the real transcript, not the placeholder, and
        // so alignment does not clobber the slice offsets the pass depends on. See persist_segments.
        self.shadow_log_loop0(db, &import_writes, &persisted);
        {
            let primary_by_segment: HashMap<&str, PrimaryHypothesis<'_>> = persisted
                .iter()
                .filter_map(|segment| {
                    PrimaryHypothesis::from_segment(segment).map(|primary| (segment.id.as_str(), primary))
                })
                .collect();
            for (seg_id, f32_pcm) in pcm_cache {
                let primary = primary_by_segment.get(seg_id.as_str()).copied();
                if let Err(error) = self.populate_hypotheses_reusing_primary(db, &seg_id, &f32_pcm, primary) {
                    log_hypothesis_population_failure(&seg_id, &error);
                }
            }
        }
        // Snapshot alignment authority only after all synchronous hypothesis writes: migration v68
        // advances the segment revision for hypothesis evidence, so enqueueing earlier would make the
        // detached worker correctly reject every newly-populated row as stale.
        self.enqueue_background_alignments(&persisted, import_writes);
        Ok(persisted)
    }

    fn process_single_file_streaming(
        &self,
        path: &Path,
        db: &Database,
        decode_timeout: Duration,
        mut on_chunk: impl FnMut(usize, usize),
        context: StreamingImportContext<'_>,
    ) -> AppResult<Vec<SpeechSegment>> {
        let StreamingImportContext { duration_ms, cancel, source_provenance } = context;
        let estimated_total =
            ((duration_ms as f64 / self.settings.max_segment_duration_ms.max(1) as f64).ceil() as usize).max(1);
        let mut global_chunk = 0usize;
        let import_writes = self.import_write_store(db.path())?;
        let mut segments = Vec::new();
        let mut all_pcm_cache = Vec::new();
        let mut windows_seen = 0usize;
        // Carry the final chunk of each 90 s decode window into the next one. That chunk touches the
        // hard window edge, so re-chunking it together with the following audio lets the silence-aware
        // splitter cut on a pause instead of guillotining a word across the boundary (which made the 7B
        // re-emit the straddling word — e.g. "پێداویستە سەرەتایەکانی" was duplicated across a 180 s seam).
        let mut carry_pcm: Vec<i16> = Vec::new();
        let mut carry_base: usize = 0;
        let mut sample_rate_seen: u32 = 16_000;
        // Accumulate one speaker embedding per retained segment across ALL decode windows, so speakers
        // are clustered over the WHOLE file once (below) rather than re-clustered per 90s window.
        let mut all_embeddings: Vec<Vec<f32>> = Vec::new();
        // P1.4b (audit R4): the per-window rebuild-when-inactive below (fix #132, for a model that appears
        // mid-session) re-attempted a full GPU-then-CPU ONNX load on EVERY 90 s window for a PRESENT-but-
        // unloadable model. These flags bound the (re)build to at most ONCE per FILE — matching the
        // non-streaming sibling that builds once per file — so a corrupt/unloadable denoiser/CAM++ is not
        // reloaded per window. A NEW file (new streaming call) resets them, so a between-file download
        // still recovers (#132's intent, at file granularity).
        let mut diarization_rebuild_tried = false;
        let mut denoiser_rebuild_tried = false;
        // v51: accumulate ONE whole-recording identity across the windows. Before this, the streaming
        // path fingerprinted each window, discarded every value, and persisted nothing — so a long file
        // (the only kind that reaches this path) never participated in cross-session duplicate detection
        // at all. blake3 streams, so this costs no extra memory and yields exactly the digest the
        // non-streaming path would have computed for the same canonical PCM.
        let mut recording_identity = crate::fingerprint::StreamingIdentity::new();
        // Round-23 #5, corrected 2026-08-17: hash the source ONCE PER FILE. This used to live inside
        // build_segments_from_pcm, which the streaming path calls once per 90 s window — so a long
        // import re-read and re-hashed the entire source file for every window. Measured on the
        // library's longest source (KBHP-EP12.wav, 5,315 s / 162 MB): 60 full-file hashes instead of
        // one. The old comment claiming "once for the whole run" was only ever true of the
        // non-streaming sibling. `None` means "no cache for this run", exactly as before.
        let file_hash = crate::cache::TranscriptCache::compute_hash(path).ok();

        // Consume each decode window as it arrives instead of collecting them all first. Peak PCM
        // held is now bounded by a handful of windows (≤ 4 × 90 s ≈ 11.5 MB) instead of the whole
        // recording — 170 MB for that same KBHP-EP12, and unbounded in the file's length.
        let window_timeout = decode_timeout.min(MAX_WINDOW_DECODE_WAIT);
        let process_window = |window: audio::PcmWindow, is_last: bool| -> AppResult<()> {
            windows_seen += 1;
            if let Some(token) = cancel {
                token.check()?;
            }

            let (sample_rate, win_pcm) = if window.pcm.is_empty() {
                (sample_rate_seen, Vec::new())
            } else {
                let (sr, p) = audio::ensure_pcm_16khz(window.sample_rate, window.pcm)?;
                sample_rate_seen = sr;
                // Fingerprint only freshly-decoded audio, never the carried-over tail — pushing a carry
                // twice would change the whole-file digest and break the equality with the
                // non-streaming path.
                //
                // Do not register window identities: only the complete recording is database authority.
                // Registering here used to leave dozens of phantom entries when a later ASR window
                // failed, blocking a legitimate retry until restart.
                recording_identity.push(&p, sr);
                (sr, p)
            };

            // Prepend the previous window's carried-over tail (contiguous audio) before chunking.
            let (effective_pcm, base_sample) = if carry_pcm.is_empty() {
                let base = chunking::ms_to_samples(window.offset_ms.max(0) as u32, sample_rate);
                (win_pcm, base)
            } else {
                let mut v = std::mem::take(&mut carry_pcm);
                let base = carry_base;
                v.extend_from_slice(&win_pcm);
                (v, base)
            };
            if effective_pcm.is_empty() {
                return Ok(());
            }
            let pcm = effective_pcm;

            // Service before planning, same reorder and same reason as the non-streaming sibling: the
            // planner asks who is speaking before agreeing to a silence-approved merge. The rebuild
            // policy (at most one attempt per file) is unchanged — the block simply moved up.
            let mut diarization_guard = None;
            if self.settings.enable_diarization {
                let mut guard = self.lock_diarization_service();
                // Rebuild when unset, OR when cached-inactive AND we have not yet tried this file (P1.4b:
                // don't re-attempt an unloadable CAM++ every window — at most once per file). See the
                // non-streaming sibling site.
                if should_rebuild_streaming_service(
                    guard.is_some(),
                    guard.as_ref().is_some_and(|s| s.is_available()),
                    diarization_rebuild_tried,
                ) {
                    diarization_rebuild_tried = true;
                    // Per-file (round-26): see the sibling site — resolve_root_for avoids the all-or-nothing orphan.
                    let model_dir = self.model_manager.resolve_root_for(crate::models::CAMPP_MODEL);
                    *guard = Some(crate::diarization::SpeakerEmbeddingService::new(&model_dir));
                }
                diarization_guard = Some(guard);
            }
            let embedding_service = diarization_guard.as_ref().and_then(|guard| guard.as_ref());

            let (mut chunk_ranges, vad_backend) = if let Some(service) = embedding_service {
                let judge = crate::diarization::speaker_turn_judge(service, sample_rate);
                chunking::plan_speech_chunks_with_judge(
                    &pcm,
                    sample_rate,
                    self.settings.vad_threshold,
                    self.settings.min_segment_duration_ms,
                    self.settings.max_segment_duration_ms,
                    Some(&judge),
                )?
            } else {
                chunking::plan_speech_chunks(
                    &pcm,
                    sample_rate,
                    self.settings.vad_threshold,
                    self.settings.min_segment_duration_ms,
                    self.settings.max_segment_duration_ms,
                )?
            };

            // Hold back the boundary-touching tail of every non-final window for the next round so the
            // splitter can later cut it on a pause. Carry from the last chunk's START all the way to the
            // window END (`pcm[ls..]`), NOT just to its VAD end `le`: the samples after `le` (trailing
            // silence up to the true window boundary) are real audio, and dropping them shifted the next
            // window's base earlier by `pcm.len() - le`, drifting every later segment's source_start_ms/
            // _end_ms cumulatively (offset_ms is only consulted while the carry is empty). Carrying the
            // whole tail keeps the concatenated timeline globally contiguous. Final window emits all.
            if !is_last {
                if let Some(&(ls, _le)) = chunk_ranges.last() {
                    carry_pcm = pcm[ls..].to_vec();
                    carry_base = base_sample + ls;
                    chunk_ranges.pop();
                }
            }
            if chunk_ranges.is_empty() {
                return Ok(());
            }

            let global_ranges: Vec<(usize, usize)> =
                chunk_ranges.iter().map(|&(s, e)| (base_sample + s, base_sample + e.min(pcm.len()))).collect();

            let mut window_progress = |_: usize, _: usize| {
                global_chunk += 1;
                on_chunk(global_chunk, estimated_total.max(global_chunk));
            };

            let mut denoiser_guard = None;
            if self.settings.enable_denoising {
                let mut guard = self.lock_denoiser_service();
                // Rebuild when unset, OR when cached-inactive AND we have not yet tried this file (P1.4b:
                // don't re-attempt an unloadable GTCRN every window — at most once per file). See the
                // non-streaming sibling site.
                if should_rebuild_streaming_service(
                    guard.is_some(),
                    guard.as_ref().is_some_and(|s| s.is_active()),
                    denoiser_rebuild_tried,
                ) {
                    denoiser_rebuild_tried = true;
                    // Per-file (round-26): see the sibling site — resolve_root_for avoids the all-or-nothing orphan.
                    let model_dir = self.model_manager.resolve_root_for(crate::models::DENOISER_MODEL);
                    *guard = Some(crate::denoiser::DenoiserService::new(&model_dir));
                }
                denoiser_guard = Some(guard);
            }
            let denoiser_service = denoiser_guard.as_ref().and_then(|guard| guard.as_ref());

            let (window_segs, window_pcm_cache) = self.build_segments_from_pcm(
                path,
                &pcm,
                sample_rate,
                base_sample,
                &global_ranges,
                vad_backend,
                cancel,
                embedding_service,
                denoiser_service,
                &mut window_progress,
                Some(&mut all_embeddings), // streaming: defer clustering to the whole-file pass below
                file_hash.as_deref(),
            )?;
            segments.extend(window_segs);
            all_pcm_cache.extend(window_pcm_cache);
            Ok(())
        };

        audio::decode_pcm_windows_streaming(
            path.to_path_buf(),
            audio::DECODE_WINDOW_MS,
            window_timeout,
            process_window,
        )?;

        if windows_seen == 0 {
            return Err(AppError::Validation("Empty audio buffer".into()));
        }
        if segments.is_empty() {
            return Err(AppError::Validation("No speech chunks produced".into()));
        }

        // Reserve the ONE whole-recording identity accumulated across every decoded canonical-PCM
        // window. There are intentionally no per-window duplicate checks: only the completed identity
        // names the source that canonical publication will bind. Reservation happens before champion
        // ASR and before any segment/hypothesis row is published, so a duplicate spends decode,
        // VAD/chunk-planning and optional preprocessing work only; it cannot spend champion inference
        // or expose a partial/colliding recording.
        let identity = recording_identity.finish();
        let fingerprint_reservation = self
            .fingerprint
            .reserve_import_identity(&identity, Some(path))
            .map_err(|e| AppError::Validation(e.into()))?;

        let chunk_count = segments.len() as u32;
        for (idx, seg) in segments.iter_mut().enumerate() {
            if let Some(meta) = seg.alignment_json.as_deref().and_then(chunking::SegmentSourceMeta::from_alignment_json)
            {
                let mut meta = meta;
                meta.chunk_index = idx as u32;
                meta.chunk_count = chunk_count;
                seg.alignment_json = Some(meta.to_alignment_json());
            }
        }

        // Whole-file speaker clustering: cluster every retained segment's embedding TOGETHER so a
        // physical speaker keeps ONE SPEAKER_xx label across decode-window boundaries (per-window
        // clustering relabels the first speaker of each window as SPEAKER_00). all_embeddings is in
        // lockstep with `segments`, so labels back-fill by index; a None label keeps any
        // filename-derived speaker hint, and it is a no-op when diarization is off.
        if self.settings.enable_diarization && all_embeddings.len() == segments.len() {
            let labels = crate::diarization::cluster_embeddings(&all_embeddings, self.settings.max_speakers);
            for (seg, label) in segments.iter_mut().zip(labels) {
                if let Some(spk) = label {
                    seg.speaker_id = Some(spk);
                }
            }
        }

        let mut prepared = segments;
        let champion_deployment = self.run_primary_wsl_pass_for_import(&mut prepared, cancel)?;
        let persisted = if let Some(deployment_sha256) = champion_deployment.as_deref() {
            import_writes.publish_champion_segments(
                &prepared,
                deployment_sha256,
                Some(&identity),
                source_provenance,
            )?;
            prepared
        } else if identity.spectral != 0 {
            import_writes.publish_segments_with_identity(&prepared, &identity, source_provenance)?;
            prepared
        } else {
            self.persist_segments(&import_writes, prepared, source_provenance)?
        };
        fingerprint_reservation.commit();
        // Deferred to here so both see the real transcript and alignment doesn't clobber offsets.
        self.shadow_log_loop0(db, &import_writes, &persisted);
        {
            let primary_by_segment: HashMap<&str, PrimaryHypothesis<'_>> = persisted
                .iter()
                .filter_map(|segment| {
                    PrimaryHypothesis::from_segment(segment).map(|primary| (segment.id.as_str(), primary))
                })
                .collect();
            for (seg_id, f32_pcm) in all_pcm_cache {
                let primary = primary_by_segment.get(seg_id.as_str()).copied();
                if let Err(error) = self.populate_hypotheses_reusing_primary(db, &seg_id, &f32_pcm, primary) {
                    log_hypothesis_population_failure(&seg_id, &error);
                }
            }
        }
        self.enqueue_background_alignments(&persisted, import_writes);
        Ok(persisted)
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn build_segments_from_pcm(
        &self,
        path: &Path,
        pcm: &[i16],
        sample_rate: u32,
        global_base_sample: usize,
        chunk_ranges: &[(usize, usize)],
        vad_backend: crate::audio::VadBackend,
        cancel: Option<&CancellationToken>,
        embedding_service: Option<&crate::diarization::SpeakerEmbeddingService>,
        denoiser_service: Option<&crate::denoiser::DenoiserService>,
        on_chunk: &mut impl FnMut(usize, usize),
        // When `Some`, this is the STREAMING path: diarization clustering is DEFERRED — one embedding
        // per retained segment is appended here (in segment order) so the caller can cluster the WHOLE
        // file once. When `None`, clustering happens per-call (the non-streaming whole-file path).
        mut embedding_sink: Option<&mut Vec<Vec<f32>>>,
        // The source file's content hash, computed ONCE PER FILE by the caller and used to key the
        // per-chunk transcript cache. It used to be computed here, which the streaming path re-ran for
        // every 90 s window — O(windows × filesize) of redundant reads on exactly the long recordings
        // that path exists for. `None` = unhashable file = no cache for this run.
        file_hash: Option<&str>,
    ) -> AppResult<(Vec<SpeechSegment>, Vec<(String, Vec<f32>)>)> {
        let chunk_count = chunk_ranges.len() as u32;
        let chunk_total = chunk_ranges.len().max(1);
        let active_asr_model_size = self.selected_asr_model_size();
        let model_id = match active_asr_model_size {
            crate::settings::AsrModelSize::CTC300M => "omniasr-ctc-300m".to_string(),
            crate::settings::AsrModelSize::CTC1B => "omniasr-ctc-1b".to_string(),
            crate::settings::AsrModelSize::WSL7B => "omniasr-wsl-7b".to_string(),
        };
        let audio_path = path.to_string_lossy().to_string();
        let speaker_hint = if chunk_count > 1 && self.settings.assign_speaker_from_filename {
            path.file_stem().map(|s| s.to_string_lossy().into_owned())
        } else {
            None
        };

        let (diarization_labels, chunk_embeddings) = if let Some(embedding_service) = embedding_service {
            // chunk_ranges are in GLOBAL sample coordinates, but `pcm` is the window-local buffer in
            // the streaming path (global_base_sample > 0). Embeddings slice `pcm` directly, so rebase
            // the ranges to local coords first — exactly like the transcription slice below. Without
            // this, every chunk past the first 90s window indexes beyond pcm.len(), clamps to an empty
            // slice, and silently gets NO speaker label. No-op when global_base_sample == 0.
            let local_ranges: Vec<(usize, usize)> = chunk_ranges
                .iter()
                .map(|&(gs, ge)| {
                    (gs.saturating_sub(global_base_sample), ge.saturating_sub(global_base_sample).min(pcm.len()))
                })
                .collect();
            let embeddings =
                crate::diarization::compute_chunk_embeddings(pcm, sample_rate, &local_ranges, embedding_service);
            if embedding_sink.is_some() {
                // Streaming: defer clustering to the caller's whole-file pass; no per-window labels.
                (vec![None; chunk_ranges.len()], Some(embeddings))
            } else {
                // Non-streaming: the whole file is this one call, so cluster in place and drop the
                // embeddings (the deferred sink is unused here).
                (crate::diarization::cluster_embeddings(&embeddings, self.settings.max_speakers), None)
            }
        } else {
            (vec![None; chunk_ranges.len()], None)
        };

        let mut segments = Vec::with_capacity(chunk_ranges.len());
        let mut pcm_cache = Vec::new();

        // Round-23 #3: if the user enabled denoising but the (optional) denoiser model is absent,
        // process() is a silent pass-through — warn loudly so the un-denoised reality is visible. The
        // run config separately records denoising=false (see runs::config_from_settings) so provenance
        // is honest; this log surfaces it to the operator.
        if self.settings.enable_denoising && !denoiser_service.is_some_and(|service| service.is_active()) {
            tracing::warn!(
                "Denoising is enabled in settings but the denoiser model is not loaded — audio is NOT being denoised (download the denoiser model to enable AI cleanup)"
            );
        }

        // The retained chunk PCM below is consumed by exactly one thing: the auxiliary-hypothesis pass,
        // which this same gate turns off. Under the champion (WSL7B) config it is always off, so a long
        // import used to accumulate every chunk's f32 audio for the whole file — ~270 MB for 1.5 h —
        // and then drop all of it untouched. Keep it only when something will actually read it.
        let retain_chunk_pcm = auxiliary_hypotheses_enabled(&self.settings);

        for (chunk_index, &(global_start, global_end)) in chunk_ranges.iter().enumerate() {
            if let Some(token) = cancel {
                token.check()?;
            }
            on_chunk(chunk_index + 1, chunk_total);

            let local_start = global_start.saturating_sub(global_base_sample);
            let local_end = global_end.saturating_sub(global_base_sample).min(pcm.len());
            if local_end <= local_start {
                continue;
            }
            let chunk_pcm = &pcm[local_start..local_end];
            if audio::is_silent(chunk_pcm) {
                continue;
            }
            let quality = crate::audio_quality::analyze_audio_quality(chunk_pcm);
            let chunk_duration_ms = chunking::samples_to_ms(local_end.saturating_sub(local_start), sample_rate);
            let source_meta =
                chunking::build_source_meta(global_start, global_end, sample_rate, chunk_index as u32, chunk_count);
            // Round-22 #12: key the per-chunk cache on the SAME stored ms range the re-transcribe read
            // path uses (slice_pcm_by_alignment), NOT raw sample indices. The read side round-trips
            // sample -> ms -> sample, so a raw-sample key never matched and the cache missed every time.
            let chunk_suffix = format!("chunk_{}_{}", source_meta.source_start_ms, source_meta.source_end_ms);

            let mut f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();

            // P1-1: Normalize PCM gain to -20 dBFS RMS before denoising and ASR.
            // Prevents low-energy audio (phone calls, distant mics) from producing
            // empty or junk transcripts due to near-zero token activations.
            audio::normalize_pcm_rms(&mut f32_pcm, -20.0);

            if let Some(denoiser_service) = denoiser_service {
                let timer = crate::inference::InferenceTimer::start("denoiser");
                f32_pcm = denoiser_service.process(&f32_pcm, audio::TARGET_SAMPLE_RATE);
                timer.finish(true);
            }

            // Primary-engine override (matches transcribe()): when use_finetuned_asr is set, the
            // embedded fine-tuned MMS-CTC engine (the measured-best local Sorani engine, ~half the
            // CER of stock) is the import primary too — otherwise the flag silently did nothing on
            // import and every clip was transcribed with stock CTC. Any failure/empty output falls
            // through to the configured engine so import never breaks. Uses the raw chunk PCM, exactly
            // like transcribe()'s fine-tuned path (no extra RMS/denoise, so the two paths agree).
            // Evaluate the WSL-7B-primary routing ONCE per chunk: it decides both the placeholder branch
            // below AND whether a fine-tuned miss is a genuine STOCK downgrade (it is NOT when the 7B is the
            // primary drafter — the miss falls to the 7B champion, not stock local CTC).
            let wsl_primary = self.should_use_wsl_primary_asr();
            let finetuned_text: Option<String> = if self.finetuned_override_active() {
                // F2: every attempted chunk is counted; a fall-through TO STOCK increments the fallback
                // counter so the import completion can report the downgrade LOUDLY (a log-only
                // warn here left a whole import drafted at stock ~29.4% CER instead of the selected
                // 21.0% engine with nothing visible in the UI).
                self.finetuned_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let drafted = match Self::finetuned_model_paths() {
                    Some((onnx, vocab)) => match Self::transcribe_chunk_finetuned(&onnx, &vocab, chunk_pcm) {
                        Ok(t) if !t.trim().is_empty() => Some(t),
                        Ok(_) => {
                            tracing::warn!("fine-tuned ASR empty on import chunk; using the configured engine");
                            None
                        }
                        Err(e) => {
                            tracing::warn!("fine-tuned ASR failed on import chunk ({e}); using the configured engine");
                            None
                        }
                    },
                    None => {
                        tracing::warn!(
                            "use_finetuned_asr set but the fine-tuned model is absent; using configured engine"
                        );
                        None
                    }
                };
                // Count a fine-tuned MISS as a stock downgrade ONLY when the chunk actually falls back to
                // stock local CTC. Under the WSL-7B primary the miss falls to the 7B champion (the
                // placeholder branch below), which is NOT a stock downgrade — counting it would raise a
                // FALSE "ALL N chunk(s) were drafted by the STOCK engine … stock-grade" completion error on
                // an import the 7B actually drafted (the owner's WSL7B+use_finetuned config when the
                // fine-tuned checkpoint is absent — a direct honesty-law violation).
                if drafted.is_none() && !wsl_primary {
                    self.finetuned_fallbacks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                drafted
            } else {
                None
            };

            let (raw_transcript, confidence, confidence_source, model_version_id) = if let Some(text) = finetuned_text {
                (text, None, Some("fine_tuned_no_posterior".to_string()), Some("finetuned-mms-ckb".to_string()))
            } else if wsl_primary {
                (
                    "[Pending WSL 7B ASR]".to_string(),
                    None,
                    Some("not_run".to_string()),
                    Some("omniasr-wsl-7b".to_string()),
                )
            } else if let Some(cached) =
                file_hash.and_then(|h| self.cache.get_chunk_by_hash(h, &model_id, Some(&chunk_suffix)))
            {
                (cached.raw_transcript, None, Some("cache_replay".to_string()), Some(model_id.clone()))
            } else {
                let (text, conf, source) = self.with_asr(|asr| {
                    if asr.is_available() {
                        let timer = crate::inference::InferenceTimer::start("asr");
                        let result = asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE);
                        timer.finish(result.is_ok());
                        match result {
                            Ok((t, c, source)) => (t, c, Some(source.as_db_value().to_string())),
                            Err(e) => {
                                tracing::warn!(
                                    "ASR transcription failed for {} chunk {}: {e}",
                                    path.display(),
                                    chunk_index
                                );
                                (format!("[ASR unavailable: {e}]"), None, Some("not_available".to_string()))
                            }
                        }
                    } else {
                        tracing::warn!("ASR model not available for {} chunk {}", path.display(), chunk_index);
                        (String::new(), Some(0.0), Some("not_available".to_string()))
                    }
                });

                // Only cache a genuine transcription. Never bake a transient failure (model
                // unavailable → empty, or a transcribe error → "[ASR unavailable: …]") into the
                // cache, or every later retry would just replay the failure forever.
                if !text.trim().is_empty() && !crate::quality::is_placeholder_transcript(&text) {
                    if let Some(h) = file_hash {
                        let entry = crate::cache::CacheEntry {
                            audio_hash: String::new(),
                            raw_transcript: text.clone(),
                            normalized_transcript: None,
                            created_at: chrono::Utc::now(),
                            model_id: model_id.clone(),
                        };
                        self.cache.set_chunk_by_hash(h, Some(&chunk_suffix), entry);
                    }
                }
                (text, conf, source, Some(model_id.clone()))
            };

            let normalized = if self.settings.auto_normalize && !raw_transcript.is_empty() {
                let norm_config = crate::normalizer::NormalizationConfig {
                    normalize_numbers: self.settings.auto_normalize,
                    verbalize_numbers: self.settings.verbalize_numbers,
                    normalize_hamza: true,
                    remove_diacritics: false,
                };
                let norm = SoraniNormalizer::with_config(norm_config);
                Some(norm.normalize(&raw_transcript))
            } else {
                None
            };
            let normalizer_version = normalized.as_ref().map(|_| NORMALIZER_VERSION.to_string());

            let speaker_id = diarization_labels.get(chunk_index).and_then(|l| l.clone()).or(speaker_hint.clone());

            let seg_id = Uuid::new_v4().to_string();
            if retain_chunk_pcm {
                pcm_cache.push((seg_id.clone(), f32_pcm));
            }

            // Streaming defer: accumulate this segment's embedding (in segment order) so the caller can
            // cluster the whole file once. The push stays in lockstep with `segments` — both happen only
            // for a RETAINED chunk — so the back-filled labels align by index.
            if let Some(sink) = embedding_sink.as_mut() {
                if let Some(embs) = chunk_embeddings.as_ref() {
                    sink.push(embs.get(chunk_index).cloned().unwrap_or_default());
                }
            }

            segments.push(SpeechSegment {
                id: seg_id,
                created_at: None,
                audio_path: audio_path.clone(),
                raw_transcript,
                normalized_transcript: normalized,
                annotated_transcript: None,
                alignment_json: Some(source_meta.to_alignment_json()),
                duration_ms: chunk_duration_ms,
                speaker_id,
                verified: false,
                confidence,
                ctc_score: None,
                clipping_ratio: Some(quality.clipping_ratio),
                rms_db: Some(quality.rms_db),
                // Already Option: None (unmeasurable, e.g. a short clip) persists as NULL so the
                // quality/jury gates skip the SNR check instead of reading 0.0 as the worst SNR.
                snr_db: quality.snr_db,
                split: None,
                signal_anomaly_score: None,
                verdict: None,
                verdict_transcript: None,
                rationale: None,
                evidence_json: None,
                agreement_score: None,
                escalated: false,
                human_decision: None,
                corrected_at: None,
                is_gold: false,
                alignment_quality: None, // set to 'ctc_forced' or 'energy_heuristic' after align()
                model_version_id,
                confidence_source,
                cloud_call: false,
                decoder_config_hash: None,
                normalizer_version,
                // P0.4 per-segment processing provenance: record whether denoising / diarization
                // ACTUALLY ran for this clip (the setting enabled AND the model was loadable), not the
                // bare setting. This is per-FILE truth duplicated across the file's rows (honest), read
                // at export instead of recomputing from export-day model state (H3). For diarization,
                // `is_available()` reflects whether the CAM++ pass ran, independent of whether THIS
                // segment received a label (streaming defers labeling; single-speaker files get one id).
                denoised: Some(denoiser_service.is_some_and(|service| service.is_active())),
                diarized: Some(embedding_service.is_some_and(|service| service.is_available())),
                // P0.4: the VAD backend that ACTUALLY produced this file/window's regions (silero / energy
                // fallback / none for the short whole-buffer path) — surfaced from the detector, not a
                // path-exists probe (a corrupt Silero falls back to energy at runtime).
                vad_backend: Some(vad_backend.as_str().to_string()),
                // v43: a freshly imported clip has had no human decision, so it has no reviewer to
                // attribute. `record_human_decision_by` fills this in when someone actually decides it.
                reviewed_by: None,
                // v47: NOT measured at import, and None says exactly that. Answering it here costs two
                // extra CAM++ embeddings per chunk on top of the diarization pass, and the calibration
                // that reads it (0.59) was derived on ~14 s clips — applying it to whatever length the
                // planner emits would be a threshold used outside the range it was measured on.
                // `src/bin/speaker_change_probe.rs --persist` fills it for the whole library at once.
                speaker_change_score: None,
            });
        }

        // Round-22 #11: renumber the RETAINED segments to contiguous chunk_index / chunk_count. The loop
        // `continue`s past empty/silent chunks, so chunk_index (the enumerate index over ALL chunk_ranges)
        // has gaps and chunk_count over-counts the segments actually produced. The streaming caller
        // re-applies a whole-file renumber across decode windows; doing it here makes the non-streaming
        // whole-file path emit the same contiguous numbering instead of gappy provenance metadata.
        let retained = segments.len() as u32;
        for (idx, seg) in segments.iter_mut().enumerate() {
            if let Some(meta) = seg.alignment_json.as_deref().and_then(chunking::SegmentSourceMeta::from_alignment_json)
            {
                let mut meta = meta;
                meta.chunk_index = idx as u32;
                meta.chunk_count = retained;
                seg.alignment_json = Some(meta.to_alignment_json());
            }
        }

        Ok((segments, pcm_cache))
    }

    fn persist_segments(
        &self,
        import_writes: &crate::stores::ImportWriteStore,
        segments: Vec<SpeechSegment>,
        source_provenance: Option<&crate::db::SourceAudioProvenance>,
    ) -> AppResult<Vec<SpeechSegment>> {
        if segments.is_empty() {
            return Err(AppError::Validation("No speech chunks produced".into()));
        }

        // insert_segments_batch wraps inserts in its own transaction; do not nest SAVEPOINTs.
        import_writes.publish_segments(&segments, source_provenance)?;

        // NOTE: neither LOOP-0 shadow logging nor background word-alignment runs here. Champion imports
        // bypass this compatibility publisher and use publish_champion_segments only after in-memory
        // drafting. Both follow-up passes therefore always see a real, durably published transcript.

        Ok(segments)
    }

    /// M2.3 / P1.3: for each freshly persisted segment, record whether LOOP-0 WOULD have fired on its
    /// finalized Verbatim-Law transcript (human annotation ▸ champion raw), WITHOUT mutating anything.
    /// Normalized/refined machine strings remain evidence and never drive correction-memory authority. Memories are
    /// loaded once. Best-effort: a load or write failure logs and never fails the import.
    fn shadow_log_loop0(
        &self,
        db: &Database,
        import_writes: &crate::stores::ImportWriteStore,
        segments: &[SpeechSegment],
    ) {
        let memories = match db.load_correction_memories() {
            Ok(memories) => memories,
            Err(error) => {
                tracing::warn!("LOOP-0 shadow logging skipped: failed to load correction memories: {error}");
                return;
            }
        };
        for seg in segments {
            let text = crate::corrections::loop0_draft_text(seg.annotated_transcript.as_deref(), &seg.raw_transcript);
            let would_fire = loop0_would_fire(&memories, text);
            if let Err(error) = import_writes.record_loop0_shadow(&seg.id, would_fire) {
                tracing::warn!("LOOP-0 shadow log write failed for {}: {error}", seg.id);
            }
        }
    }

    /// M2.4: Enqueue background word-alignment for segments. Non-blocking, best-effort, opt-in via
    /// `auto_align`.
    ///
    /// CRITICAL invariant (the whole-file-vs-clip bug class): each segment's `alignment_json` holds
    /// its `{source_start_ms, source_end_ms}` slice offsets, which every LATER reader depends on — the
    /// WSL-7B re-transcribe client, dataset audio export, clip playback, jury acoustic scoring. This
    /// alignment therefore MUST (1) slice the clip out of the source by those offsets before aligning
    /// (word timings clip-local, not smeared across the whole recording) and (2) MERGE its word array
    /// back under a `words` key via `merge_word_timestamps` — NEVER flat-overwrite `alignment_json`
    /// with a bare word array, which would destroy the offsets and silently degrade every reader to
    /// the whole file. (This ran inside `persist_segments` and clobbered offsets; it is now deferred to
    /// after the 7B pass and repaired to slice+merge.)
    fn enqueue_background_alignments(
        &self,
        segments: &[SpeechSegment],
        import_writes: crate::stores::ImportWriteStore,
    ) {
        if !self.settings.auto_align {
            return;
        }
        // Group by source file so each recording is decoded ONCE (a VAD-chunked file yields many
        // segments sharing one audio_path). Carry each segment's source-offset alignment_json + the
        // exact authoritative review projection (human annotation ▸ immutable champion raw).
        // Word chips and seek timers must describe the text the reviewer actually sees after reload;
        // normalized/refined text remains evidence and cannot drive review timing authority.
        let segment_ids: Vec<String> = segments.iter().map(|segment| segment.id.clone()).collect();
        let stored_sources = match import_writes.alignment_sources(&segment_ids) {
            Ok(sources) => sources,
            Err(error) => {
                tracing::warn!("background alignment skipped: canonical source snapshot failed: {error}");
                return;
            }
        };
        type AlignmentWorkItem = (String, Option<String>, String, i64);
        let mut by_path: std::collections::HashMap<String, Vec<AlignmentWorkItem>> = std::collections::HashMap::new();
        for (s, revision) in stored_sources {
            let text = crate::quality::effective_transcript(&s).to_string();
            by_path.entry(s.audio_path.clone()).or_default().push((
                s.id.clone(),
                s.alignment_json.clone(),
                text,
                revision,
            ));
        }
        // Resolved HERE because the thread below is `move` and never captures `self`. That is exactly how
        // this path ended up on `aligner::align` — the free fallback-only stub — instead of the real
        // aligner the foreground path uses: the model root simply was not reachable inside the closure.
        // Per-file resolve for the same reason `Pipeline::align` uses it: `resolve_root_for` finds
        // mms_aligner.onnx in the user dir OR bundled, where the all-or-nothing `resolved_dir()` orphans
        // a bundled aligner as soon as OmniASR is downloaded into the user dir.
        let aligner_root = self.model_manager.resolve_root_for("mms_aligner.onnx");
        let enable_gpu = self.settings.enable_gpu;

        // R3: this DETACHED thread is spawned during import (ImportState::Running) but OUTLIVES the
        // ImportGuard. Keep the legacy background-writer fence for the thread's whole lifetime so a
        // restore cannot replace the database while inference is reading source metadata. The actual
        // write below additionally crosses the shared DatabaseRuntime and compares the exact alignment
        // read before inference, so it is serialized and cannot clobber a newer edit. Acquire the guard
        // HERE so there is no unfenced gap between enqueue and thread start; Drop covers exit and panic.
        let align_writer_guard = crate::commands::BgDbWriterGuard::new();
        std::thread::spawn(move || {
            // Held for the whole alignment thread.
            let _align_writer_guard = align_writer_guard;
            // ONCE for the whole import, not per segment: `ForcedAligner::new` loads a ~365 MB ONNX
            // session. `Pipeline::align` can afford to build one per call because it aligns a single
            // clip; doing that here — across every segment of every file in an import — would not be.
            // A missing model is NOT an error: `new` succeeds with no session and `align` then reports
            // EnergyHeuristic honestly, which is the old behaviour and the correct one when there is
            // genuinely nothing better available.
            let aligner = match aligner::ForcedAligner::new(&aligner_root, enable_gpu) {
                Ok(aligner) => aligner,
                Err(error) => {
                    tracing::warn!("background alignment skipped: aligner unavailable: {error}");
                    return;
                }
            };
            let (mut aligned, mut failed) = (0usize, 0usize);
            for (audio_path, jobs) in by_path {
                let pcm16 =
                    match audio::decode_to_pcm(&audio_path).and_then(|(sr, pcm)| audio::ensure_pcm_16khz(sr, pcm)) {
                        Ok((_, pcm)) => pcm,
                        Err(error) => {
                            tracing::warn!("background alignment: decode failed for {audio_path}: {error}");
                            failed += jobs.len();
                            continue;
                        }
                    };
                for (seg_id, source_alignment, text, expected_revision) in jobs {
                    if text.trim().is_empty() {
                        continue;
                    }
                    // Slice the clip out of the source by its stored offsets BEFORE aligning.
                    let sliced = match chunking::slice_pcm_by_alignment(&pcm16, 16000, source_alignment.as_deref()) {
                        Ok((clip, _)) => clip,
                        Err(error) => {
                            tracing::warn!("background alignment: slice failed for {seg_id}: {error}");
                            failed += 1;
                            continue;
                        }
                    };
                    match aligner.align(&sliced, 16000, &text) {
                        Ok((words, quality)) if !words.is_empty() => {
                            // MERGE under `words`, preserving source_start_ms/source_end_ms. One
                            // atomic write for timings + quality marker: persisting the timings while
                            // the quality stamp failed (the old swallowed `let _ =`) left heuristic
                            // word timings unmarked, and quality.rs only raises the review-risk
                            // reason when the marker is present.
                            let merged = crate::chunking::merge_word_timestamps(source_alignment.as_deref(), &words);
                            match import_writes.update_alignment_if_unchanged(
                                &seg_id,
                                expected_revision,
                                source_alignment.as_deref(),
                                &merged,
                                // The quality the aligner ACTUALLY achieved. This was hardcoded to
                                // EnergyHeuristic, which was true of the stub but is a provenance lie the
                                // moment a real alignment happens — and `quality.rs` raises a review-risk
                                // reason on exactly this value, so the lie cost every background-aligned
                                // clip a false risk flag.
                                quality.as_db_str(),
                            ) {
                                Ok(true) => aligned += 1,
                                Ok(false) => {
                                    tracing::warn!(
                                        "background alignment: skipped stale metadata for {seg_id}; canonical alignment changed during inference"
                                    );
                                    failed += 1;
                                }
                                Err(error) => {
                                    tracing::warn!("background alignment: persist failed for {seg_id}: {error}");
                                    failed += 1;
                                }
                            }
                        }
                        // Empty word list or error: leave the source offsets INTACT (never overwrite).
                        Ok(_) => failed += 1,
                        Err(error) => {
                            tracing::warn!("background alignment failed for {seg_id}: {error}");
                            failed += 1;
                        }
                    }
                }
            }
            if failed > 0 {
                tracing::warn!(
                    "background alignment: {aligned} aligned, {failed} failed/empty (source offsets preserved)"
                );
            } else {
                tracing::debug!("background alignment: {aligned} segment(s) aligned");
            }
        });
    }

    pub(super) fn run_primary_wsl_pass_for_import(
        &self,
        segments: &mut [SpeechSegment],
        cancel: Option<&CancellationToken>,
    ) -> AppResult<Option<String>> {
        if !self.should_use_wsl_primary_asr() || segments.is_empty() {
            return Ok(None);
        }

        // FORCE-USE the champion (fail hard), but do not publish staging rows. Every inference and
        // enabled refinement completes in memory first. A cancellation, process kill, empty draft,
        // worker panic, or identity rotation therefore leaves zero canonical rows for this file.
        let segment_count = segments.len();
        let mut deployment_identity: Option<(String, String)> = None;

        // Run the champion calls a WAVE at a time instead of one clip at a time.
        //
        // MEASURED 2026-08-14: one round trip is 4.62 s on a ~9 s clip and the whole import sustained
        // 8.5 clips/min with both GPUs near idle — the cost is latency, not compute. `WSL_7B_GATE` was
        // built to admit several calls at once and the server pre-forks one replica per GPU, but until
        // now NOTHING ever issued two calls concurrently, so the gate limited a concurrency that never
        // existed and the second card never received work. Setting CORTEX_7B_CONCURRENCY=2 changed the
        // rate by nothing, which is what proved the loop was the bottleneck.
        //
        // Inference/refinement is parallel. The sequential phase only validates returned identity and
        // copies finalized machine-owned fields into the in-memory segments.
        let wave_size = wsl_7b_concurrency().max(1);
        let mut start = 0usize;
        while start < segments.len() {
            let end = (start + wave_size).min(segments.len());

            // Cancellation is checked once per WAVE rather than per segment; `transcribe` also polls
            // the flag inside each in-flight call, so a cancel still lands within ~50 ms.
            if let Some(token) = cancel {
                token.check()?;
            }

            // PHASE A — concurrent, no shared DB.
            let flag = cancel.map(|t| t.as_atomic());
            let jobs: Vec<(String, String, Option<String>)> = segments[start..end]
                .iter()
                .map(|s| (s.id.clone(), s.audio_path.clone(), s.alignment_json.clone()))
                .collect();
            let outcomes: Vec<ChampionAttempt> = std::thread::scope(|scope| {
                let handles: Vec<_> = jobs
                    .iter()
                    .map(|(id, path, aj)| scope.spawn(move || self.attempt_champion(id, path, aj.as_deref(), flag)))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        // A panicked worker must not be read as success. Treat it as an infrastructure
                        // failure so the import halts and rolls back, per the force-champion contract.
                        h.join().unwrap_or_else(|_| ChampionAttempt::Infra("champion worker panicked".to_string()))
                    })
                    .collect()
            });

            // PHASE B — sequential, in the original segment order.
            for (offset, outcome) in outcomes.into_iter().enumerate() {
                let seg = &mut segments[start + offset];
                match outcome {
                    ChampionAttempt::Infra(reason) => {
                        tracing::error!(
                            "WSL 7B import halted before publication (server unavailable: {reason}); {} segment(s) remain unpublished",
                            segment_count
                        );
                        return Err(AppError::Validation(format!(
                            "OmniASR 7B server is not running — start it (e.g. wsl python cortex_7b_server.py from scripts/) and re-import. \
                             The import was halted before any of this file's {segment_count} segment(s) were published. ({reason})"
                        )));
                    }
                    ChampionAttempt::Empty(reason) => {
                        tracing::error!(
                            "WSL 7B primary ASR unavailable before publication: segment {} failed after retries ({reason}); all {segment_count} segment(s) remain unpublished",
                            seg.id
                        );
                        return Err(AppError::Other(format!(
                            "champion produced no usable draft for segment {} after retries ({reason}). \
                             The import was halted before this file's {segment_count} segment(s) were published. \
                             Check the 7B server load and re-import.",
                            seg.id
                        )));
                    }
                    ChampionAttempt::Drafted(draft) => {
                        let model_id = draft.model_version_id.clone().ok_or_else(|| {
                            AppError::Validation(format!(
                                "Champion draft for segment {} omitted its model identity",
                                seg.id
                            ))
                        })?;
                        let deployment_sha256 = draft.deployment_sha256.clone().ok_or_else(|| {
                            AppError::Validation(format!(
                                "Champion draft for segment {} omitted its deployment digest",
                                seg.id
                            ))
                        })?;
                        match deployment_identity.as_ref() {
                            Some((expected_model, expected_sha))
                                if expected_model != &model_id || expected_sha != &deployment_sha256 =>
                            {
                                return Err(AppError::Validation(format!(
                                    "MODEL_IDENTITY_CHANGED: champion deployment rotated while drafting this file (expected {expected_model}@{expected_sha}, received {model_id}@{deployment_sha256}); no segments were published"
                                )));
                            }
                            None => deployment_identity = Some((model_id.clone(), deployment_sha256)),
                            _ => {}
                        }
                        let (derived_transcript, derived_producer_version) =
                            self.derived_review_transcript(&draft.raw_text, &draft.final_text);
                        seg.raw_transcript = draft.raw_text;
                        seg.normalized_transcript = derived_transcript;
                        seg.normalizer_version = derived_producer_version;
                        seg.confidence = draft.confidence;
                        seg.confidence_source = draft.confidence_source;
                        seg.model_version_id = Some(model_id);
                        seg.cloud_call = draft.cloud_call;
                    }
                }
            }

            start = end;
        }
        Ok(deployment_identity.map(|(_, sha)| sha))
    }

    /// Retry and fully refine one segment without publishing it. The resulting draft is copied into
    /// the in-memory file batch; only the later atomic publication boundary can create canonical rows.
    fn attempt_champion(
        &self,
        segment_id: &str,
        audio_path: &str,
        alignment_json: Option<&str>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> ChampionAttempt {
        // The warm 7B server can transiently fail or return an empty result for a clip (e.g. while
        // still under load right after launch), which would otherwise leave that segment stuck at its
        // "[Pending WSL 7B ASR]" placeholder for good (observed in stress testing: 1 of 3 segments).
        // Retry a few times before giving up so an import reliably transcribes every segment; only
        // escalate after the retries are exhausted, rather than silently shipping a pending segment.
        const MAX_ATTEMPTS: usize = 3;
        let mut last_problem = String::from("7B produced no result");
        let mut infra = false;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.transcribe_import_draft_only(segment_id, audio_path, alignment_json, cancel) {
                Ok(draft) => {
                    let usable = !draft.raw_text.trim().is_empty() && !draft.raw_text.contains("[Pending");
                    if usable {
                        return ChampionAttempt::Drafted(draft);
                    }
                    last_problem = "7B returned an empty transcript".to_string();
                    infra = false;
                }
                Err(error) => {
                    let msg = error.to_string();
                    if msg.contains(WSL_7B_EMPTY_RESULT_MARKER) {
                        // transcribe() turns Ok("") into Err so the re-transcribe IPCs cannot
                        // blank-overwrite a stored transcript. For an IMPORT that is not an
                        // infrastructure failure — the server answered, the clip simply had no words.
                        last_problem = "7B returned an empty transcript".to_string();
                        infra = false;
                    } else {
                        // A 5-minute per-attempt timeout means the server is HUNG, not flaky: another
                        // full-timeout attempt only triples the stall. Quick failures (connection
                        // refused) still retry briefly in case the server is mid-launch.
                        let hung = msg.contains("timed out");
                        last_problem = msg;
                        infra = true;
                        if hung {
                            break;
                        }
                    }
                }
            }
            if attempt < MAX_ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(1000));
            }
        }
        if infra {
            ChampionAttempt::Infra(last_problem)
        } else {
            ChampionAttempt::Empty(last_problem)
        }
    }

    /// Import one audio file through the same VAD chunking + ASR path as directory import.
    pub fn import_single_file(&self, path: &Path) -> AppResult<Vec<SpeechSegment>> {
        self.import_single_file_with_events(path, None, None, |_| {})
    }

    /// Import one file with optional cancellation and progress events (for Ctrl+O / long audiobooks).
    pub fn import_single_file_with_events(
        &self,
        path: &Path,
        cancel: Option<CancellationToken>,
        // Retained for call-site symmetry with the directory path; the jury (which consumed this for
        // report correlation) now runs in the import command's background thread, not inline here.
        _agent_run_id: Option<&str>,
        on_event: impl Fn(PipelineEvent),
    ) -> AppResult<Vec<SpeechSegment>> {
        // Acquire path/file immutability before even the duration estimate. This entry point used to
        // probe by path and only seal inside `process_single_file_with_progress`, leaving the first
        // decoder open outside the source authority and allowing a parent-tree replacement between
        // the estimate and the real import.
        let source_lease = crate::media::seal_import_source(path).map_err(|error| {
            AppError::Other(format!("Import source could not be held immutable through publication: {error}"))
        })?;
        let path = source_lease.source_path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
        let duration_ms = audio::get_duration_ms(path)?;
        let estimated_chunks =
            ((duration_ms as f64 / self.settings.max_segment_duration_ms.max(1) as f64).ceil() as usize).max(1);

        on_event(PipelineEvent::Started { total: 1 });
        self.reset_finetuned_counters();
        on_event(PipelineEvent::Phase { phase: "importing".into() });
        self.set_import_status(0, estimated_chunks, &fname);
        // RAII: clear `running` on EVERY exit path, including an early `?` from the open_db below, so a
        // failed single-file import can't leave a phantom in-progress status (mirrors import_directory).
        struct ImportStatusGuard<'a>(&'a ProcessingPipeline);
        impl Drop for ImportStatusGuard<'_> {
            fn drop(&mut self) {
                self.0.finish_import_status();
            }
        }
        let _status_guard = ImportStatusGuard(self);

        let db = self.open_db()?;
        let source_reference_enabled = self.source_reference_enabled();
        if source_reference_enabled {
            on_event(PipelineEvent::Phase { phase: "reference_transcribing".into() });
            on_event(agent_stage(
                "source_reference",
                "running",
                fname.clone(),
                "Building whole-file reference transcript",
                0,
                estimated_chunks,
            ));
            on_event(PipelineEvent::Progress {
                current: 0,
                total: estimated_chunks,
                file: fname.clone(),
                status: "Building whole-file reference transcript".into(),
            });
        }
        let mut chunks_done = 0usize;
        // Imports always use the configured primary engine. Optional cloud tools may be invoked only
        // through their explicit per-segment actions; a consent toggle can never replace the 7B
        // champion for an entire import or create a mixed-engine dataset after a cloud failure.
        let result = self.process_single_file_under_source_lease(
            path,
            &db,
            cancel.as_ref(),
            |current, total| {
                chunks_done = current;
                let total = total.max(estimated_chunks);
                self.set_import_status(current, total, &fname);
                on_event(PipelineEvent::Phase { phase: "transcribing".into() });
                on_event(agent_stage(
                    "audio_chunking",
                    "running",
                    fname.clone(),
                    format!("Preparing chunk {current}/{total}"),
                    current,
                    total,
                ));
                on_event(PipelineEvent::Progress {
                    current,
                    total,
                    file: fname.clone(),
                    status: format!("Transcribing chunk {current}/{total}"),
                });
            },
            &source_lease,
        );

        match &result {
            Ok(segments) => {
                self.set_import_status(segments.len(), segments.len(), &fname);
                let segment_count = segments.len();
                if source_reference_enabled {
                    on_event(agent_stage(
                        "source_reference",
                        "completed",
                        fname.clone(),
                        "Whole-file source reference stage completed or reused",
                        1,
                        1,
                    ));
                }
                on_event(agent_stage(
                    "audio_chunking",
                    "completed",
                    fname.clone(),
                    format!("{segment_count} speech chunk(s) persisted"),
                    segment_count,
                    segment_count.max(1),
                ));
                on_event(multi_model_hypothesis_stage(&db, &self.settings, fname.clone(), segments));

                // Post-import jury adjudication is intentionally NOT run here. The import COMMAND
                // (commands.rs `import_audio_file`) runs it on a background thread with its OWN WAL
                // database connection, so the heavy ASR-bearing jury never holds the shared DB lock
                // and starves the UI's get_segments. Running it here too made single-file import
                // adjudicate — and make any opted-in cloud LLM calls — TWICE and persist two agent
                // import reports for one import. The directory path keeps its own inline jury because
                // it batches every file's segments into a single adjudication.
            }
            Err(_) => {
                self.set_import_status(chunks_done, estimated_chunks, &fname);
            }
        }
        // F2: a fine-tuned→stock downgrade during this single-file import must end LOUD too.
        {
            let attempts = self.finetuned_attempts.load(std::sync::atomic::Ordering::Relaxed);
            let fallbacks = self.finetuned_fallbacks.load(std::sync::atomic::Ordering::Relaxed);
            if let Some(error) = Self::finetuned_downgrade_message(attempts, fallbacks) {
                tracing::error!("finetuned downgrade on import: {error}");
                on_event(PipelineEvent::Error { file: "fine-tuned engine".into(), error });
            }
        }
        on_event(PipelineEvent::Completed {
            total: 1,
            succeeded: if result.is_ok() { 1 } else { 0 },
            failed: if result.is_err() { 1 } else { 0 },
        });
        // `running` is cleared by `_status_guard` on scope exit (covers early-return error paths too).
        result
    }
}
