use crate::aligner;
use crate::asr;
use crate::audio;
use crate::cache::TranscriptCache;
use crate::cancel::CancellationToken;
use crate::chunking::{self, MAX_PCM_SAMPLES};
use crate::db::{Database, SegmentHypothesis, SourceTranscriptRecord, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::fingerprint::AudioFingerprint;
use crate::models::ModelManager;
use crate::normalizer::SoraniNormalizer;
use crate::settings::AppSettings;
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use uuid::Uuid;

const SUBPROCESS_ERROR_PREVIEW_CHARS: usize = 4096;
const SOURCE_AUDIO_HASH_BUFFER_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct SourceAudioIdentity {
    pub(crate) content_hash: String,
    pub(crate) size_bytes: i64,
}

pub(crate) fn source_audio_identity(path: &Path) -> AppResult<SourceAudioIdentity> {
    let metadata = std::fs::metadata(path)?;
    let size_bytes = i64::try_from(metadata.len())
        .map_err(|_| AppError::Validation(format!("Audio file is too large to index safely: {}", path.display())))?;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; SOURCE_AUDIO_HASH_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(SourceAudioIdentity { content_hash: hasher.finalize().to_hex().to_string(), size_bytes })
}

/// Apply LOOP-0 correction memories to a finalized transcript when the opt-in is enabled. Returns
/// the transcript unchanged when disabled, empty, or when nothing fires. Best-effort: a memory-load
/// failure logs and returns the original transcript rather than failing the transcription.
pub(crate) fn apply_loop0_firing(enabled: bool, db: &crate::db::Database, transcript: &str) -> String {
    if !enabled || transcript.trim().is_empty() {
        return transcript.to_string();
    }
    match db.load_correction_memories() {
        Ok(memories) if !memories.is_empty() => crate::corrections::apply_memories(
            transcript,
            &memories,
            &crate::corrections::FiringConfig::default(),
        ),
        Ok(_) => transcript.to_string(),
        Err(error) => {
            tracing::warn!("LOOP-0 firing skipped: failed to load correction memories: {error}");
            transcript.to_string()
        }
    }
}

fn subprocess_error_preview(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "(no stderr output)".to_string();
    }

    let mut chars = trimmed.chars();
    let mut preview: String = chars.by_ref().take(SUBPROCESS_ERROR_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        preview.push_str("\n[truncated subprocess stderr]");
    }
    preview
}

fn lock_decoded_windows(windows: &Mutex<Vec<audio::PcmWindow>>) -> MutexGuard<'_, Vec<audio::PcmWindow>> {
    windows.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned decoded PCM window accumulator");
        poisoned.into_inner()
    })
}

fn log_hypothesis_population_failure(segment_id: &str, error: &AppError) {
    tracing::error!("Failed to populate ASR hypotheses for {segment_id}: {error}");
}

fn insert_hypothesis_checked(
    db: &Database,
    segment_id: &str,
    model_id: &str,
    transcript: String,
    confidence: Option<f64>,
) -> AppResult<()> {
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment_id.to_string(),
        model_id: model_id.to_string(),
        transcript,
        confidence,
    })
    .map_err(|error| AppError::Other(format!("Failed to insert {model_id} hypothesis for {segment_id}: {error}")))
}

fn parse_wsl_segment_result(stdout: &str) -> AppResult<(String, Option<f64>)> {
    #[derive(serde::Deserialize)]
    struct WslResult {
        raw_transcript: String,
        confidence: Option<f64>,
    }

    let mut raw_transcript = String::new();
    let mut confidence: Option<f64> = None;
    for line in stdout.lines() {
        if let Some(stripped) = line.strip_prefix("__RESULT__=") {
            if let Ok(res) = serde_json::from_str::<WslResult>(stripped) {
                raw_transcript = res.raw_transcript;
                confidence = res.confidence;
            }
        }
    }

    if raw_transcript.trim().is_empty() {
        return Err(AppError::Other("WSL 7B ASR process did not return a valid transcript.".into()));
    }

    Ok((raw_transcript, confidence))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportStatus {
    pub running: bool,
    pub current: usize,
    pub total: usize,
    pub file: String,
}

#[derive(Debug, Clone)]
pub enum PipelineEvent {
    Started { total: usize },
    Phase { phase: String },
    AgentStage { stage: String, status: String, file: String, detail: String, current: usize, total: usize },
    Progress { current: usize, total: usize, file: String, status: String },
    Completed { total: usize, succeeded: usize, failed: usize },
    Error { file: String, error: String },
}

fn agent_stage(
    stage: &str,
    status: &str,
    file: impl Into<String>,
    detail: impl Into<String>,
    current: usize,
    total: usize,
) -> PipelineEvent {
    PipelineEvent::AgentStage {
        stage: stage.to_string(),
        status: status.to_string(),
        file: file.into(),
        detail: detail.into(),
        current,
        total,
    }
}

fn multi_model_hypothesis_stage(db: &Database, file: impl Into<String>, segments: &[SpeechSegment]) -> PipelineEvent {
    let file = file.into();
    let total = segments.len().max(1);
    if segments.is_empty() {
        return agent_stage(
            "multi_model_hypotheses",
            "blocked",
            file,
            "No speech segments were persisted, so multi-model hypothesis coverage could not be verified",
            0,
            total,
        );
    }

    let mut covered = 0usize;
    let mut blocked_ids = Vec::new();
    let mut observed_models = std::collections::BTreeSet::new();
    for segment in segments {
        match db.get_hypotheses_for_segment(&segment.id) {
            Ok(hypotheses) => {
                let coverage = crate::quality::hypothesis_coverage_for_model_outputs(&hypotheses);
                observed_models.extend(coverage.non_empty_models.iter().cloned());
                if coverage.passes_minimum {
                    covered += 1;
                } else {
                    blocked_ids.push(segment.id.clone());
                }
            }
            Err(error) => {
                return agent_stage(
                    "multi_model_hypotheses",
                    "blocked",
                    file,
                    format!("Failed to verify multi-model hypothesis coverage from the database: {error}"),
                    covered,
                    total,
                );
            }
        }
    }

    if blocked_ids.is_empty() {
        let models = if observed_models.is_empty() {
            "none".to_string()
        } else {
            observed_models.into_iter().collect::<Vec<_>>().join(", ")
        };
        return agent_stage(
            "multi_model_hypotheses",
            "completed",
            file,
            format!("Verified multi-model hypothesis coverage for {covered}/{total} segment(s): {models}"),
            covered,
            total,
        );
    }

    let preview = blocked_ids.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    let suffix = if blocked_ids.len() > 5 { format!(" and {} more", blocked_ids.len() - 5) } else { String::new() };
    agent_stage(
        "multi_model_hypotheses",
        "blocked",
        file,
        format!(
            "Only {covered}/{total} segment(s) have the required non-empty multi-model hypothesis coverage; blocked segment(s): {preview}{suffix}"
        ),
        covered,
        total,
    )
}

#[derive(Clone)]
pub struct ProcessingPipeline {
    db_path: String,
    _normalizer: Arc<SoraniNormalizer>,
    cache: Arc<TranscriptCache>,
    fingerprint: Arc<AudioFingerprint>,
    settings: Arc<AppSettings>,
    model_manager: Arc<ModelManager>,
    asr_pool: Arc<asr::AsrPool>,
    import_status: Arc<Mutex<ImportStatus>>,
    diarization_service: Arc<Mutex<Option<crate::diarization::SpeakerEmbeddingService>>>,
    denoiser_service: Arc<Mutex<Option<crate::denoiser::DenoiserService>>>,
}

impl ProcessingPipeline {
    pub fn new(
        db_path: String,
        normalizer: Arc<SoraniNormalizer>,
        cache: Arc<TranscriptCache>,
        fingerprint: Arc<AudioFingerprint>,
        settings: Arc<AppSettings>,
        model_manager: Arc<ModelManager>,
    ) -> Self {
        Self {
            db_path,
            _normalizer: normalizer,
            cache,
            fingerprint,
            settings,
            model_manager,
            asr_pool: Arc::new(asr::AsrPool::new()),
            import_status: Arc::new(Mutex::new(ImportStatus::default())),
            diarization_service: Arc::new(Mutex::new(None)),
            denoiser_service: Arc::new(Mutex::new(None)),
        }
    }

    pub fn update_settings(&mut self, settings: AppSettings) {
        self.settings = Arc::new(settings);
    }

    pub fn settings_snapshot(&self) -> AppSettings {
        self.settings.as_ref().clone()
    }

    /// Pre-load the pooled Meta OmniASR CTC recognizer.
    pub fn warmup_asr(&self) -> Result<(), String> {
        if self.should_use_wsl_primary_asr() {
            tracing::info!("WSL 7B model selected: skipping local ONNX ASR pool warm-up.");
            return Ok(());
        }
        let model_dir = self.model_manager.resolved_dir();
        self.asr_pool.warmup(&model_dir, &self.asr_config())
    }

    fn asr_config(&self) -> asr::AsrLoadConfig {
        asr::AsrLoadConfig {
            model_size: self.active_local_asr_model_size(),
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        }
    }

    fn active_local_asr_model_size(&self) -> crate::settings::AsrModelSize {
        let model_dir = self.model_manager.resolved_dir();
        if self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
            if asr::omniasr_model_present(&model_dir, &crate::settings::AsrModelSize::CTC1B) {
                return crate::settings::AsrModelSize::CTC1B;
            }
            if asr::omniasr_model_present(&model_dir, &crate::settings::AsrModelSize::CTC300M) {
                return crate::settings::AsrModelSize::CTC300M;
            }
            return crate::settings::AsrModelSize::CTC300M;
        }
        asr::select_available_model_size(&model_dir, &self.settings.asr_model_size)
    }

    fn should_use_wsl_primary_asr(&self) -> bool {
        self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B
            && self.settings.external_asr_script_path().is_some()
    }

    fn local_asr_model_id(&self) -> &'static str {
        match self.active_local_asr_model_size() {
            crate::settings::AsrModelSize::CTC1B => "omniasr-ctc-1b",
            crate::settings::AsrModelSize::CTC300M | crate::settings::AsrModelSize::WSL7B => "omniasr-ctc-300m",
        }
    }

    fn with_asr<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut asr::KurdishAsrService) -> R,
    {
        let model_dir = self.model_manager.resolved_dir();
        self.asr_pool.with_service(&model_dir, &self.asr_config(), f)
    }

    /// Run the permanent gold-set eval end-to-end against the live local ASR engine.
    ///
    /// Unlike the model-agnostic [`crate::eval::run_gold_eval`] (which trusts caller
    /// hypotheses), this loads each gold clip, decodes + resamples it exactly like the
    /// import path, runs the pooled OmniASR CTC recognizer, and scores the *raw* ASR
    /// output against the gold reference — the only way a published WER/CER is
    /// reproducible from audio rather than asserted. Opens its own DB connection so no
    /// `AppState` lock is held across the (slow) ASR loop.
    pub fn run_gold_eval_asr(&self, model_id: Option<&str>) -> AppResult<crate::eval::EvalRunResult> {
        let db = self.open_db()?;
        let model_id = model_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.local_asr_model_id().to_string());
        crate::eval::run_gold_eval_with_transcriber(&db, &model_id, |seg| {
            self.transcribe_audio_file_raw(&seg.audio_path)
        })
    }

    /// Decode an audio file and return the *raw* local-ASR transcript — no LLM
    /// refinement, no normalization — i.e. the exact hypothesis used for gold WER/CER.
    pub fn transcribe_audio_file_raw(&self, audio_path: &str) -> AppResult<String> {
        let (sample_rate, pcm) = audio::decode_to_pcm(audio_path)?;
        let (_sr, pcm16) = audio::ensure_pcm_16khz(sample_rate, pcm)?;
        let f32_pcm: Vec<f32> = pcm16.iter().map(|&s| s as f32 / 32768.0).collect();
        self.with_asr(|asr| {
            if !asr.is_available() {
                return Err(AppError::Other("ASR model not loaded".into()));
            }
            asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
                .map(|(text, _confidence)| text)
                .map_err(AppError::Other)
        })
    }

    pub fn import_status_handle(&self) -> Arc<Mutex<ImportStatus>> {
        Arc::clone(&self.import_status)
    }

    fn lock_import_status(&self) -> MutexGuard<'_, ImportStatus> {
        self.import_status.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned import status lock");
            poisoned.into_inner()
        })
    }

    fn lock_diarization_service(&self) -> MutexGuard<'_, Option<crate::diarization::SpeakerEmbeddingService>> {
        self.diarization_service.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned diarization service lock");
            poisoned.into_inner()
        })
    }

    fn lock_denoiser_service(&self) -> MutexGuard<'_, Option<crate::denoiser::DenoiserService>> {
        self.denoiser_service.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned denoiser service lock");
            poisoned.into_inner()
        })
    }

    pub fn import_status(&self) -> ImportStatus {
        self.lock_import_status().clone()
    }

    fn set_import_status(&self, current: usize, total: usize, file: &str) {
        let mut status = self.lock_import_status();
        status.running = true;
        status.current = current;
        status.total = total;
        status.file = file.to_string();
    }

    fn finish_import_status(&self) {
        self.lock_import_status().running = false;
    }

    fn open_db(&self) -> AppResult<Database> {
        Database::open(&self.db_path)
    }

    fn source_transcript_dir(&self) -> Option<PathBuf> {
        Path::new(&self.db_path).parent().map(|dir| dir.join("source_transcripts"))
    }

    fn reusable_source_reference_record(
        &self,
        db: &Database,
        existing: &SourceTranscriptRecord,
        current_identity: Option<&SourceAudioIdentity>,
    ) -> AppResult<Option<SourceTranscriptRecord>> {
        let Some(current_identity) = current_identity else {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the current audio file identity could not be verified",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        };
        let identity_matches = existing.audio_content_hash.as_deref() == Some(current_identity.content_hash.as_str())
            && existing.audio_size_bytes == Some(current_identity.size_bytes);
        if !identity_matches {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the stored audio identity does not match the current file",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        }

        if !crate::agentic::is_usable_source_reference_transcript(&existing.transcript_text) {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the stored DB text is empty or unusable",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        }

        let transcript_path = Path::new(&existing.transcript_path);
        let saved_text = match std::fs::read_to_string(transcript_path) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(
                    "Ignoring cached whole-file reference transcript for {} with {} because '{}' could not be read: {}",
                    existing.audio_path,
                    existing.model_id,
                    existing.transcript_path,
                    error
                );
                return Ok(None);
            }
        };
        if !crate::agentic::is_usable_source_reference_transcript(&saved_text) {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because '{}' is empty or unusable",
                existing.audio_path,
                existing.model_id,
                existing.transcript_path
            );
            return Ok(None);
        }

        let saved_text = saved_text.trim().to_string();
        if saved_text == existing.transcript_text.trim() {
            return Ok(Some(existing.clone()));
        }

        let synced = SourceTranscriptRecord {
            transcript_text: saved_text,
            created_at: existing.created_at.clone(),
            ..existing.clone()
        };
        db.upsert_source_transcript(&synced)?;
        tracing::info!(
            "Synced cached whole-file reference transcript for {} with {} from edited text file '{}'",
            existing.audio_path,
            existing.model_id,
            existing.transcript_path
        );
        Ok(Some(synced))
    }

    fn ensure_source_reference_transcripts(
        &self,
        path: &Path,
        db: &Database,
    ) -> AppResult<Vec<SourceTranscriptRecord>> {
        if !self.settings.jury_cloud_opt_in {
            return Ok(Vec::new());
        }
        if self.settings.llm_api_key.trim().is_empty() {
            return Err(AppError::Other(
                "Gemini API key is required for whole-file reference transcript when jury cloud opt-in is enabled"
                    .to_string(),
            ));
        }

        let audio_path = path.to_string_lossy().to_string();
        let output_dir = self
            .source_transcript_dir()
            .ok_or_else(|| AppError::Other("Cannot resolve app data directory for source transcripts".into()))?;
        let current_identity = match source_audio_identity(path) {
            Ok(identity) => Some(identity),
            Err(error) => {
                tracing::warn!(
                    "Cannot verify current audio identity for whole-file source transcript cache at {}: {}",
                    path.display(),
                    error
                );
                None
            }
        };
        let mut records = Vec::new();
        let mut errors = Vec::new();

        for model in self.settings.source_reference_models() {
            if let Some(existing) = db.get_source_transcript(&audio_path, &model)? {
                if let Some(existing) =
                    self.reusable_source_reference_record(db, &existing, current_identity.as_ref())?
                {
                    tracing::info!(
                        "Reusing whole-file reference transcript for {} from {}",
                        path.display(),
                        existing.transcript_path
                    );
                    records.push(existing);
                    continue;
                }
            }

            match crate::agentic::generate_whole_file_reference_transcript(
                path,
                &model,
                &self.settings.llm_api_key,
                &output_dir,
            ) {
                Ok(artifact) => {
                    let identity =
                        current_identity.as_ref().cloned().or_else(|| source_audio_identity(path).ok()).ok_or_else(
                            || {
                                AppError::Other(format!(
                                "Cannot verify audio identity after generating whole-file reference transcript for {}",
                                path.display()
                            ))
                            },
                        )?;
                    let record = SourceTranscriptRecord {
                        audio_path: artifact.audio_path,
                        model_id: artifact.model_id,
                        audio_content_hash: Some(identity.content_hash),
                        audio_size_bytes: Some(identity.size_bytes),
                        transcript_path: artifact.transcript_path,
                        transcript_text: artifact.transcript_text,
                        created_at: None,
                    };
                    db.upsert_source_transcript(&record)?;
                    records.push(record);
                }
                Err(error) => {
                    tracing::warn!(
                        "Whole-file reference transcript failed for {} with {}: {}",
                        path.display(),
                        model,
                        error
                    );
                    errors.push(format!("{model}: {error}"));
                }
            }
        }

        if !errors.is_empty() {
            let scope = if records.is_empty() { "All" } else { "Some" };
            return Err(AppError::Other(format!(
                "{scope} whole-file reference transcript models failed before chunking; refusing to continue with incomplete source-reference evidence: {}",
                errors.join("; ")
            )));
        }
        Ok(records)
    }

    pub fn import_directory(
        &self,
        dir_path: &Path,
        cancel: Option<CancellationToken>,
        callback: impl Fn(PipelineEvent),
    ) -> AppResult<()> {
        self.import_directory_with_agent_run_id(dir_path, cancel, None, callback)
    }

    pub fn import_directory_with_agent_run_id(
        &self,
        dir_path: &Path,
        cancel: Option<CancellationToken>,
        agent_run_id: Option<&str>,
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
        callback(PipelineEvent::Phase { phase: "importing".into() });
        self.set_import_status(0, total, "");
        // RAII: clear import_status.running on EVERY exit path. The per-file `token.check()?` cancel
        // below early-returns before the manual finish_import_status() calls, which used to leave
        // get_import_status() reporting running:true forever after a cancelled directory import.
        struct ImportStatusGuard<'a>(&'a ProcessingPipeline);
        impl Drop for ImportStatusGuard<'_> {
            fn drop(&mut self) {
                self.0.finish_import_status();
            }
        }
        let _status_guard = ImportStatusGuard(self);
        let mut succeeded = 0;
        let mut failed = 0;
        let mut imported_ids = Vec::new();

        for (idx, file) in files.iter().enumerate() {
            if let Some(ref token) = cancel {
                token.check()?;
            }

            let fname = file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

            callback(PipelineEvent::Progress {
                current: idx + 1,
                total,
                file: fname.clone(),
                status: "Processing...".into(),
            });
            self.set_import_status(idx + 1, total, &fname);
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

            let meta = crate::telemetry::Tracer::metadata(vec![
                ("file", fname.clone()),
                ("path", file.to_string_lossy().to_string()),
                ("index", (idx + 1).to_string()),
                ("total", total.to_string()),
            ]);
            let mut result = crate::telemetry::TRACER
                .record_result("pipeline.import_file", meta, || self.process_single_file(file, &db));

            if let Err(ref e) = result {
                if audio::is_transient_decode_error(e) {
                    tracing::warn!("Transient decode error for {}, retrying once: {e}", file.display());
                    std::thread::sleep(Duration::from_millis(500));
                    result = self.process_single_file(file, &db);
                }
            }

            match result {
                Ok(segments) => {
                    callback(PipelineEvent::Phase { phase: "transcribing".into() });
                    let segment_count = segments.len();
                    callback(agent_stage(
                        "source_reference",
                        "completed",
                        fname.clone(),
                        "Whole-file source reference stage completed or reused",
                        idx + 1,
                        total,
                    ));
                    callback(agent_stage(
                        "audio_chunking",
                        "completed",
                        fname.clone(),
                        format!("{segment_count} speech chunk(s) persisted"),
                        segment_count,
                        segment_count.max(1),
                    ));
                    callback(multi_model_hypothesis_stage(&db, fname.clone(), &segments));
                    succeeded += 1;
                    imported_ids.extend(segments.iter().map(|s| s.id.clone()));
                    if segments.len() > 1 {
                        tracing::info!("Imported {} annotatable segments from {}", segments.len(), file.display());
                    }
                }
                Err(e) => {
                    failed += 1;
                    callback(PipelineEvent::Error { file: fname, error: e.to_string() });
                }
            }
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

        callback(PipelineEvent::Completed { total, succeeded, failed });
        self.finish_import_status();
        Ok(())
    }

    /// Decode one source file and persist one or more `SpeechSegment` rows (VAD chunking for long audio).
    pub fn process_single_file(&self, path: &Path, db: &Database) -> AppResult<Vec<SpeechSegment>> {
        self.process_single_file_with_progress(path, db, None, |_, _| {})
    }

    fn process_single_file_with_progress(
        &self,
        path: &Path,
        db: &Database,
        cancel: Option<&CancellationToken>,
        mut on_chunk: impl FnMut(usize, usize),
    ) -> AppResult<Vec<SpeechSegment>> {
        if let Some(token) = cancel {
            token.check()?;
        }

        let duration_ms = audio::get_duration_ms(path)?;
        if duration_ms == 0 {
            return Err(AppError::Validation("Empty audio file".into()));
        }

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
            return self.process_single_file_streaming(path, db, decode_timeout, duration_ms, cancel, on_chunk);
        }

        let (sample_rate, pcm) = audio::decode_to_pcm_with_timeout(path, decode_timeout)?;

        if pcm.is_empty() {
            return Err(AppError::Validation("Empty audio buffer".into()));
        }

        let (sample_rate, pcm) = audio::ensure_pcm_16khz(sample_rate, pcm)?;

        if pcm.len() > MAX_PCM_SAMPLES {
            tracing::warn!(
                "Decoded audio exceeds memory cap ({} samples, ~{} min); chunking will bound each segment",
                pcm.len(),
                pcm.len() / sample_rate as usize / 60
            );
        }

        let _fp = self
            .fingerprint
            .check_and_register(&pcm, sample_rate, Some(path))
            .map_err(|e| AppError::Validation(e.into()))?;

        let chunk_ranges = chunking::plan_speech_chunks(
            &pcm,
            sample_rate,
            self.settings.vad_threshold,
            self.settings.min_segment_duration_ms,
            self.settings.max_segment_duration_ms,
        )?;

        let mut diarization_guard = self.lock_diarization_service();
        if diarization_guard.is_none() {
            let model_dir = self.model_manager.resolved_dir();
            *diarization_guard = Some(crate::diarization::SpeakerEmbeddingService::new(&model_dir));
        }
        let embedding_service = diarization_guard
            .as_ref()
            .ok_or_else(|| AppError::Other("Failed to initialize diarization service".into()))?;

        let mut denoiser_guard = self.lock_denoiser_service();
        if denoiser_guard.is_none() {
            let model_dir = self.model_manager.resolved_dir();
            *denoiser_guard = Some(crate::denoiser::DenoiserService::new(&model_dir));
        }
        let denoiser_service =
            denoiser_guard.as_ref().ok_or_else(|| AppError::Other("Failed to initialize denoiser service".into()))?;

        let (segments, pcm_cache) = self.build_segments_from_pcm(
            path,
            &pcm,
            sample_rate,
            0,
            &chunk_ranges,
            cancel,
            embedding_service,
            denoiser_service,
            &mut on_chunk,
        )?;
        let mut persisted = self.persist_segments(db, segments)?;
        self.run_primary_wsl_pass_for_import(db, &mut persisted)?;
        for (seg_id, f32_pcm) in pcm_cache {
            if let Err(error) = self.populate_hypotheses(db, &seg_id, &f32_pcm) {
                log_hypothesis_population_failure(&seg_id, &error);
            }
        }
        Ok(persisted)
    }

    fn process_single_file_streaming(
        &self,
        path: &Path,
        db: &Database,
        decode_timeout: Duration,
        duration_ms: i64,
        cancel: Option<&CancellationToken>,
        mut on_chunk: impl FnMut(usize, usize),
    ) -> AppResult<Vec<SpeechSegment>> {
        let windows: Arc<Mutex<Vec<audio::PcmWindow>>> = Arc::new(Mutex::new(Vec::new()));
        let acc = Arc::clone(&windows);
        let path_buf = path.to_path_buf();
        audio::decode_pcm_windows_with_timeout(path_buf, audio::DECODE_WINDOW_MS, decode_timeout, move |window| {
            lock_decoded_windows(&acc).push(window);
            Ok(())
        })?;

        let windows = {
            let guard = lock_decoded_windows(&windows);
            guard.clone()
        };

        if windows.is_empty() {
            return Err(AppError::Validation("Empty audio buffer".into()));
        }

        let estimated_total =
            ((duration_ms as f64 / self.settings.max_segment_duration_ms.max(1) as f64).ceil() as usize).max(1);
        let mut global_chunk = 0usize;

        let mut segments = Vec::new();
        let mut all_pcm_cache = Vec::new();
        for window in windows {
            if let Some(token) = cancel {
                token.check()?;
            }
            if window.pcm.is_empty() {
                continue;
            }
            let (sample_rate, pcm) = audio::ensure_pcm_16khz(window.sample_rate, window.pcm)?;
            let _fp = self
                .fingerprint
                .check_and_register(&pcm, sample_rate, Some(path))
                .map_err(|e| AppError::Validation(e.into()))?;

            let base_sample = chunking::ms_to_samples(window.offset_ms.max(0) as u32, sample_rate);
            let chunk_ranges = chunking::plan_speech_chunks(
                &pcm,
                sample_rate,
                self.settings.vad_threshold,
                self.settings.min_segment_duration_ms,
                self.settings.max_segment_duration_ms,
            )?;
            let global_ranges: Vec<(usize, usize)> =
                chunk_ranges.iter().map(|&(s, e)| (base_sample + s, base_sample + e.min(pcm.len()))).collect();

            let mut window_progress = |_: usize, _: usize| {
                global_chunk += 1;
                on_chunk(global_chunk, estimated_total.max(global_chunk));
            };

            let mut diarization_guard = self.lock_diarization_service();
            if diarization_guard.is_none() {
                let model_dir = self.model_manager.resolved_dir();
                *diarization_guard = Some(crate::diarization::SpeakerEmbeddingService::new(&model_dir));
            }
            let embedding_service = diarization_guard
                .as_ref()
                .ok_or_else(|| AppError::Other("Failed to initialize diarization service".into()))?;

            let mut denoiser_guard = self.lock_denoiser_service();
            if denoiser_guard.is_none() {
                let model_dir = self.model_manager.resolved_dir();
                *denoiser_guard = Some(crate::denoiser::DenoiserService::new(&model_dir));
            }
            let denoiser_service = denoiser_guard
                .as_ref()
                .ok_or_else(|| AppError::Other("Failed to initialize denoiser service".into()))?;

            let (window_segs, window_pcm_cache) = self.build_segments_from_pcm(
                path,
                &pcm,
                sample_rate,
                base_sample,
                &global_ranges,
                cancel,
                embedding_service,
                denoiser_service,
                &mut window_progress,
            )?;
            segments.extend(window_segs);
            all_pcm_cache.extend(window_pcm_cache);
        }

        if segments.is_empty() {
            return Err(AppError::Validation("No speech chunks produced".into()));
        }

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

        let mut persisted = self.persist_segments(db, segments)?;
        self.run_primary_wsl_pass_for_import(db, &mut persisted)?;
        for (seg_id, f32_pcm) in all_pcm_cache {
            if let Err(error) = self.populate_hypotheses(db, &seg_id, &f32_pcm) {
                log_hypothesis_population_failure(&seg_id, &error);
            }
        }
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
        cancel: Option<&CancellationToken>,
        embedding_service: &crate::diarization::SpeakerEmbeddingService,
        denoiser_service: &crate::denoiser::DenoiserService,
        on_chunk: &mut impl FnMut(usize, usize),
    ) -> AppResult<(Vec<SpeechSegment>, Vec<(String, Vec<f32>)>)> {
        let chunk_count = chunk_ranges.len() as u32;
        let chunk_total = chunk_ranges.len().max(1);
        let active_asr_model_size = self.active_local_asr_model_size();
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

        let diarization_labels = if self.settings.enable_diarization {
            crate::diarization::label_chunk_speakers(
                pcm,
                sample_rate,
                chunk_ranges,
                self.settings.max_speakers,
                embedding_service,
            )
        } else {
            vec![None; chunk_ranges.len()]
        };

        let mut segments = Vec::with_capacity(chunk_ranges.len());
        let mut pcm_cache = Vec::new();

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
            let chunk_suffix = format!("chunk_{global_start}_{global_end}");
            let source_meta =
                chunking::build_source_meta(global_start, global_end, sample_rate, chunk_index as u32, chunk_count);

            let mut f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();

            // P1-1: Normalize PCM gain to -20 dBFS RMS before denoising and ASR.
            // Prevents low-energy audio (phone calls, distant mics) from producing
            // empty or junk transcripts due to near-zero token activations.
            audio::normalize_pcm_rms(&mut f32_pcm, -20.0);

            if self.settings.enable_denoising {
                let timer = crate::inference::InferenceTimer::start("denoiser");
                f32_pcm = denoiser_service.process(&f32_pcm, audio::TARGET_SAMPLE_RATE);
                timer.finish(true);
            }

            let (raw_transcript, confidence) = if self.should_use_wsl_primary_asr() {
                ("[Pending WSL 7B ASR]".to_string(), None)
            } else if let Some(cached) = self.cache.get_chunk(path, &model_id, Some(&chunk_suffix)) {
                (cached.raw_transcript, None)
            } else {
                let (text, conf) = self.with_asr(|asr| {
                    if asr.is_available() {
                        let timer = crate::inference::InferenceTimer::start("asr");
                        let result = asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE);
                        timer.finish(result.is_ok());
                        match result {
                            Ok((t, c)) => (t, c),
                            Err(e) => {
                                tracing::warn!(
                                    "ASR transcription failed for {} chunk {}: {e}",
                                    path.display(),
                                    chunk_index
                                );
                                (format!("[ASR unavailable: {e}]"), None)
                            }
                        }
                    } else {
                        tracing::warn!("ASR model not available for {} chunk {}", path.display(), chunk_index);
                        (String::new(), Some(0.0))
                    }
                });

                // Only cache a genuine transcription. Never bake a transient failure (model
                // unavailable → empty, or a transcribe error → "[ASR unavailable: …]") into the
                // cache, or every later retry would just replay the failure forever.
                if !text.trim().is_empty() && !crate::quality::is_placeholder_transcript(&text) {
                    let entry = crate::cache::CacheEntry {
                        audio_hash: String::new(),
                        raw_transcript: text.clone(),
                        normalized_transcript: None,
                        created_at: chrono::Utc::now(),
                        model_id: model_id.clone(),
                    };
                    self.cache.set_chunk(path, Some(&chunk_suffix), entry);
                }
                (text, conf)
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

            let speaker_id = diarization_labels.get(chunk_index).and_then(|l| l.clone()).or(speaker_hint.clone());

            let seg_id = Uuid::new_v4().to_string();
            pcm_cache.push((seg_id.clone(), f32_pcm));

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
                snr_db: Some(quality.snr_db),
                split: None,
                ood_score: None,
                verdict: None,
                verdict_transcript: None,
                rationale: None,
                evidence_json: None,
                agent_confidence: None,
                escalated: false,
                human_decision: None,
                corrected_at: None,
                is_gold: false,
                alignment_quality: None, // set to 'ctc_forced' or 'energy_heuristic' after align()
            });
        }

        Ok((segments, pcm_cache))
    }

    fn persist_segments(&self, db: &Database, segments: Vec<SpeechSegment>) -> AppResult<Vec<SpeechSegment>> {
        if segments.is_empty() {
            return Err(AppError::Validation("No speech chunks produced".into()));
        }

        // insert_segments_batch wraps inserts in its own transaction; do not nest SAVEPOINTs.
        db.insert_segments_batch(&segments)?;
        Ok(segments)
    }

    fn run_primary_wsl_pass_for_import(&self, db: &Database, segments: &mut [SpeechSegment]) -> AppResult<usize> {
        if !self.should_use_wsl_primary_asr() || segments.is_empty() {
            return Ok(0);
        }

        let mut updated = 0usize;
        for seg in segments {
            match self.transcribe(Some(seg.id.as_str()), &seg.audio_path, seg.alignment_json.as_deref()) {
                Ok((_raw_text, _corrected_text, _confidence)) => {
                    if self.refresh_segment_from_db(db, seg)? {
                        updated += 1;
                    }
                }
                Err(error) => {
                    self.mark_wsl_primary_unavailable(db, seg, &error.to_string())?;
                }
            }
        }
        Ok(updated)
    }

    fn mark_wsl_primary_unavailable(&self, db: &Database, seg: &mut SpeechSegment, reason: &str) -> AppResult<()> {
        let rationale = format!("WSL 7B primary ASR unavailable before jury: {reason}");
        tracing::warn!("{} ({})", rationale, seg.id);
        db.write_segment_verdict(&seg.id, "escalated", None, Some(&rationale), None, None, true)?;
        self.refresh_segment_from_db(db, seg)?;
        Ok(())
    }

    fn refresh_segment_from_db(&self, db: &Database, seg: &mut SpeechSegment) -> AppResult<bool> {
        let ids = vec![seg.id.clone()];
        if let Some(fresh) = db.get_segments_by_ids(&ids)?.into_iter().next() {
            *seg = fresh;
            Ok(true)
        } else {
            Ok(false)
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
        agent_run_id: Option<&str>,
        on_event: impl Fn(PipelineEvent),
    ) -> AppResult<Vec<SpeechSegment>> {
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
        let duration_ms = audio::get_duration_ms(path)?;
        let estimated_chunks =
            ((duration_ms as f64 / self.settings.max_segment_duration_ms.max(1) as f64).ceil() as usize).max(1);

        on_event(PipelineEvent::Started { total: 1 });
        on_event(PipelineEvent::Phase { phase: "importing".into() });
        self.set_import_status(0, estimated_chunks, &fname);

        let db = self.open_db()?;
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
        let mut chunks_done = 0usize;
        // Cloud STT (ElevenLabs Scribe) first when opted in and a key is configured: one API call,
        // segmented from word timestamps. On ANY error, fall through to the local ASR path below so
        // an import never fails just because the cloud is unavailable.
        let result = 'transcribe: {
            if let Some(scribe_key) = self.scribe_api_key_if_enabled() {
                if let Some(token) = cancel.as_ref() {
                    if let Err(e) = token.check() {
                        break 'transcribe Err(e);
                    }
                }
                on_event(PipelineEvent::Phase { phase: "transcribing".into() });
                on_event(agent_stage(
                    "audio_chunking",
                    "running",
                    fname.clone(),
                    "Transcribing whole file with ElevenLabs Scribe (cloud)",
                    0,
                    1,
                ));
                match self.import_single_file_via_scribe(path, &db, &scribe_key) {
                    Ok(segs) => {
                        chunks_done = segs.len();
                        break 'transcribe Ok(segs);
                    }
                    Err(e) => {
                        tracing::warn!("Scribe import failed ({e}); falling back to local ASR");
                        on_event(agent_stage(
                            "audio_chunking",
                            "running",
                            fname.clone(),
                            "Scribe unavailable — using local ASR",
                            0,
                            estimated_chunks,
                        ));
                    }
                }
            }
            self.process_single_file_with_progress(path, &db, cancel.as_ref(), |current, total| {
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
            })
        };

        match &result {
            Ok(segments) => {
                self.set_import_status(segments.len(), segments.len(), &fname);
                let segment_count = segments.len();
                on_event(agent_stage(
                    "source_reference",
                    "completed",
                    fname.clone(),
                    "Whole-file source reference stage completed or reused",
                    1,
                    1,
                ));
                on_event(agent_stage(
                    "audio_chunking",
                    "completed",
                    fname.clone(),
                    format!("{segment_count} speech chunk(s) persisted"),
                    segment_count,
                    segment_count.max(1),
                ));
                on_event(multi_model_hypothesis_stage(&db, fname.clone(), segments));

                // ── Post-import jury adjudication (mirrors directory import) ────
                let imported_ids: Vec<String> = segments.iter().map(|s| s.id.clone()).collect();
                if !imported_ids.is_empty() {
                    on_event(PipelineEvent::Phase { phase: "adjudicating".into() });
                    on_event(agent_stage(
                        "jury_adjudication",
                        "running",
                        fname.clone(),
                        format!("Adjudicating {} imported segment(s)", imported_ids.len()),
                        0,
                        imported_ids.len(),
                    ));
                    let source_path = path.to_string_lossy().to_string();
                    let mut report_options = crate::runs::AgentImportReportOptions::from_settings(&self.settings);
                    // Correlate the persisted report with the live agent-stage events, which carry
                    // this run id — mirrors the directory path. Previously this report stored a null
                    // run_id, so it could not be joined to the import's stage events.
                    report_options.agent_run_id = agent_run_id.map(str::to_string);
                    match crate::commands::run_jury_pipeline_core(&db, &self.settings, imported_ids.clone()) {
                        Ok(jury_report) => {
                            on_event(agent_stage(
                                "jury_adjudication",
                                "completed",
                                fname.clone(),
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
                                "file",
                                &[source_path],
                                &imported_ids,
                                Some(&jury_report),
                                None,
                                report_options,
                            ) {
                                tracing::error!("Agent import report failed after single-file import: {error}");
                                on_event(PipelineEvent::Error {
                                    file: "agent import report".into(),
                                    error: error.to_string(),
                                });
                            } else {
                                on_event(agent_stage(
                                    "agent_report",
                                    "completed",
                                    fname.clone(),
                                    "Persisted auditable multi-agent import report",
                                    imported_ids.len(),
                                    imported_ids.len(),
                                ));
                            }
                        }
                        Err(error) => {
                            let message =
                                format!("Post-import jury adjudication failed after single-file import: {error}");
                            tracing::error!("{message}");
                            on_event(agent_stage(
                                "jury_adjudication",
                                "blocked",
                                fname.clone(),
                                message.clone(),
                                0,
                                imported_ids.len(),
                            ));
                            // Still persist the report with error context
                            if let Err(report_error) = crate::runs::record_agent_import_report_with_options(
                                &db,
                                "file",
                                &[source_path],
                                &imported_ids,
                                None,
                                Some(error.as_str()),
                                report_options,
                            ) {
                                tracing::error!("Failed to persist error agent report: {report_error}");
                            }
                            on_event(PipelineEvent::Error { file: fname.clone(), error: message });
                        }
                    }
                }
            }
            Err(_) => {
                self.set_import_status(chunks_done, estimated_chunks, &fname);
            }
        }
        on_event(PipelineEvent::Completed { total: 1, succeeded: if result.is_ok() { 1 } else { 0 }, failed: if result.is_err() { 1 } else { 0 } });
        self.finish_import_status();
        result

    }

    /// The ElevenLabs Scribe key to use for cloud STT, or `None` when cloud STT is not opted in or no
    /// key is configured. Mirrors the cloud-LLM opt-in gate; the key lives in `secrets.env` next to
    /// the database and is never read unless the user has explicitly opted in (privacy by default).
    fn scribe_api_key_if_enabled(&self) -> Option<String> {
        if !self.settings.cloud_stt_opt_in {
            return None;
        }
        let data_dir = std::path::Path::new(&self.db_path).parent()?;
        crate::api_keys::ApiKeys::load(data_dir).elevenlabs
    }

    /// Import a file using ElevenLabs Scribe as the transcriber: ONE API call for the whole file,
    /// segmented from Scribe's word timestamps into source-file slices (no local ASR/VAD/diarization).
    /// Persists and returns the segments. The caller falls back to the local path on error, so this
    /// surfaces failures rather than masking them. Acoustic-quality/diarization fields are left unset.
    pub fn import_single_file_via_scribe(
        &self,
        path: &Path,
        db: &Database,
        api_key: &str,
    ) -> AppResult<Vec<SpeechSegment>> {
        let duration_ms = audio::get_duration_ms(path)?;
        if duration_ms == 0 {
            return Err(AppError::Validation("Empty audio file".into()));
        }
        let audio_path = path.to_string_lossy().to_string();
        let scribe_segs = crate::scribe_api::transcribe_segments(
            &audio_path,
            api_key,
            crate::scribe_api::DEFAULT_MODEL,
            crate::scribe_api::SORANI_LANGUAGE_CODE,
        )?;
        let segments = Self::build_scribe_speech_segments(
            &scribe_segs,
            &audio_path,
            duration_ms,
            self.settings.auto_normalize,
            self.settings.verbalize_numbers,
        );
        if segments.is_empty() {
            return Err(AppError::Other("Scribe returned no segments".into()));
        }
        db.insert_segments_batch(&segments)?;
        Ok(segments)
    }

    /// Build persistable [`SpeechSegment`]s from Scribe segments. Each becomes a source-file slice
    /// (`audio_path` + `SegmentSourceMeta` time range) so it plays back the right region; text is
    /// stored in logical (reading) order and normalized when auto-normalize is on. A segment with an
    /// open end (0) extends to the file duration. Acoustic-quality fields stay `None` — Scribe gives
    /// text and timing, not waveform metrics.
    fn build_scribe_speech_segments(
        scribe_segs: &[crate::scribe_api::ScribeSegment],
        audio_path: &str,
        total_duration_ms: i64,
        auto_normalize: bool,
        verbalize_numbers: bool,
    ) -> Vec<SpeechSegment> {
        let chunk_count = scribe_segs.len() as u32;
        scribe_segs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let start = s.source_start_ms.max(0);
                let end = if s.source_end_ms > start { s.source_end_ms } else { total_duration_ms.max(start) };
                let meta = crate::chunking::SegmentSourceMeta {
                    source_start_ms: start,
                    source_end_ms: end,
                    chunk_index: i as u32,
                    chunk_count,
                };
                let normalized = if auto_normalize && !s.text.trim().is_empty() {
                    let cfg = crate::normalizer::NormalizationConfig {
                        normalize_numbers: auto_normalize,
                        verbalize_numbers,
                        normalize_hamza: true,
                        remove_diacritics: false,
                    };
                    Some(crate::normalizer::SoraniNormalizer::with_config(cfg).normalize(&s.text))
                } else {
                    None
                };
                SpeechSegment {
                    id: Uuid::new_v4().to_string(),
                    audio_path: audio_path.to_string(),
                    raw_transcript: s.text.clone(),
                    normalized_transcript: normalized,
                    alignment_json: Some(meta.to_alignment_json()),
                    duration_ms: end.saturating_sub(start).max(0),
                    ..Default::default()
                }
            })
            .collect()
    }

    /// Transcribe an audio file, optionally limited to a source-time range from chunk metadata.
    pub fn transcribe(
        &self,
        segment_id: Option<&str>,
        audio_path: &str,
        alignment_json: Option<&str>,
    ) -> AppResult<(String, String, Option<f64>)> {
        let path = Path::new(audio_path);
        let duration_ms = audio::get_duration_ms(path)?;
        if duration_ms == 0 {
            return Err(AppError::Validation("Empty audio file".into()));
        }

        let decode_timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);
        let (sample_rate, pcm) = audio::decode_to_pcm_with_timeout(path, decode_timeout)?;
        let (sample_rate, pcm) = audio::ensure_pcm_16khz(sample_rate, pcm)?;
        if pcm.is_empty() {
            return Err(AppError::Audio(crate::error::AudioError::EmptyBuffer));
        }

        let (chunk_pcm, chunk_suffix) = chunking::slice_pcm_by_alignment(&pcm, sample_rate, alignment_json)?;

        if self.should_use_wsl_primary_asr() {
            let db = crate::db::Database::open_with_retry(&self.db_path).map_err(|e| AppError::Other(e.to_string()))?;
            let audio_path_str = path.to_string_lossy().to_string();

            let segment_id: Option<String> = if let Some(id) = segment_id {
                Some(id.to_string())
            } else if let Some(aj) = alignment_json {
                db.connection()
                    .query_row(
                        "SELECT id FROM speech_segments WHERE audio_path = ? AND alignment_json = ?",
                        [&audio_path_str, aj],
                        |row| row.get(0),
                    )
                    .ok()
            } else {
                db.connection()
                    .query_row("SELECT id FROM speech_segments WHERE audio_path = ?", [&audio_path_str], |row| {
                        row.get(0)
                    })
                    .ok()
            };

            if let Some(id) = segment_id {
                tracing::info!("Running WSL 7B ASR for segment ID: {}", id);

                drop(db);

                let (raw_transcript, confidence) = self.run_wsl_segment_transcript(&id)?;

                let db =
                    crate::db::Database::open_with_retry(&self.db_path).map_err(|e| AppError::Other(e.to_string()))?;

                let normalized_transcript = if self.settings.auto_normalize && !raw_transcript.is_empty() {
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

                // Use the safe update method: human decisions are never overwritten.
                let updated = db
                    .update_asr_transcript_if_unreviewed(
                        &id,
                        &raw_transcript,
                        normalized_transcript.as_deref(),
                        confidence,
                    )
                    .map_err(|e| AppError::Other(format!("Failed to update segment in database: {}", e)))?;
                if !updated {
                    tracing::info!("WSL 7B: segment {id} has a human decision — transcript not overwritten.");
                }

                // Insert WSL 7B hypothesis for downstream jury comparison.
                if let Err(error) =
                    insert_hypothesis_checked(&db, &id, "omniasr-wsl-7b", raw_transcript.clone(), confidence)
                {
                    tracing::error!("{error}");
                }

                // Populate local hypotheses for comparison
                let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();
                if let Err(error) = self.populate_hypotheses(&db, &id, &f32_pcm) {
                    log_hypothesis_population_failure(&id, &error);
                }

                // Stage 2: Dual-Pass LLM Refinement (OpenRouter when configured + key present)
                let final_text = if let Some(refiner) = self.build_refiner() {
                    tracing::info!("Running LLM refinement on {} bytes...", raw_transcript.len());
                    let refine_result = if self.settings.ger_refinement_enabled {
                        // Generative error correction: prime the refiner with the N-best (populated
                        // just above) + relevant past corrections (relevance-ranked few-shot).
                        let hyps: Vec<String> = db
                            .get_hypotheses_for_segment(&id)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|h| h.transcript)
                            .collect();
                        let few_shot: Vec<(String, String)> = crate::jury::get_few_shot_examples(&db, &id, 3)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|e| (e.wrong_transcript, e.human_fix))
                            .collect();
                        refiner.refine_with_context(&raw_transcript, &hyps, &few_shot)
                    } else {
                        refiner.refine_text(&raw_transcript)
                    };
                    match refine_result {
                        Ok(refined) => {
                            tracing::info!("LLM Refinement successful.");
                            refined
                        }
                        Err(e) => {
                            tracing::warn!("LLM Refinement failed: {}. Falling back to raw transcript.", e);
                            raw_transcript.clone()
                        }
                    }
                } else {
                    raw_transcript.clone()
                };

                // LOOP 0: when enabled, correct previously-learned confusions in the final text
                // before it is returned/stored (opt-in; default off; best-effort).
                let final_text = apply_loop0_firing(self.settings.loop0_firing_enabled, &db, &final_text);

                return Ok((raw_transcript, final_text, confidence));
            } else {
                return Err(AppError::Other(
                    "Segment not found in database. Please import the audio file first to generate speech segments."
                        .into(),
                ));
            }
        }

        let model_id = self.local_asr_model_id().to_string();
        if let Some(cached) = self.cache.get_chunk(path, &model_id, chunk_suffix.as_deref()) {
            let fired = self.fire_loop0_if_enabled(&cached.raw_transcript);
            return Ok((cached.raw_transcript.clone(), fired, None));
        }

        let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let (raw_text, confidence) = self.with_asr(|asr| {
            if !asr.is_available() {
                return Err(AppError::Other("ASR model not loaded".into()));
            }
            let timer = crate::inference::InferenceTimer::start("asr");
            let result = asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE);
            timer.finish(result.is_ok());
            result.map_err(AppError::Other)
        })?;

        // Stage 2: Dual-Pass LLM Refinement (OpenRouter when configured + key present)
        let final_text = if let Some(refiner) = self.build_refiner() {
            tracing::info!("Running LLM refinement on {} bytes...", raw_text.len());
            match refiner.refine_text(&raw_text) {
                Ok(refined) => {
                    tracing::info!("LLM Refinement successful.");
                    refined
                }
                Err(e) => {
                    tracing::warn!("LLM Refinement failed: {}. Falling back to raw transcript.", e);
                    raw_text.clone()
                }
            }
        } else {
            raw_text.clone()
        };

        let entry = crate::cache::CacheEntry {
            audio_hash: String::new(),
            raw_transcript: final_text.clone(),
            normalized_transcript: None,
            created_at: chrono::Utc::now(),
            model_id: model_id.clone(),
        };
        self.cache.set_chunk(path, chunk_suffix.as_deref(), entry);

        if let Some(id) = segment_id {
            if let Ok(db) = self.open_db() {
                if let Err(error) = self.populate_hypotheses(&db, id, &f32_pcm) {
                    log_hypothesis_population_failure(id, &error);
                }
            }
        }

        let final_text = self.fire_loop0_if_enabled(&final_text);
        Ok((raw_text, final_text, confidence))
    }

    /// Apply LOOP-0 firing to a finalized transcript, opening a short-lived DB connection only when
    /// the opt-in is enabled (so the default-off path pays nothing). Best-effort — a db-open failure
    /// logs and returns the original text rather than failing transcription.
    fn fire_loop0_if_enabled(&self, transcript: &str) -> String {
        if !self.settings.loop0_firing_enabled {
            return transcript.to_string();
        }
        match self.open_db() {
            Ok(db) => apply_loop0_firing(true, &db, transcript),
            Err(error) => {
                tracing::warn!("LOOP-0 firing skipped (could not open db): {error}");
                transcript.to_string()
            }
        }
    }

    /// Build the LLM refiner. When the configured mode is the cloud (Gemini) and an OPENROUTER_API_KEY
    /// is present in secrets.env, route through OpenRouter instead — it is verified working and
    /// reaches Gemini-class models, whereas direct Gemini is commonly 429 quota-blocked. Respects
    /// `None` (refinement disabled) and `Local` (the user's own endpoint).
    fn build_refiner(&self) -> Option<crate::llm_refiner::LlmRefiner> {
        use crate::settings::LlmMode;
        let refiner_from_settings = |mode: &LlmMode| {
            crate::llm_refiner::LlmRefiner::new(
                mode,
                self.settings.llm_endpoint.clone(),
                self.settings.llm_api_key.clone(),
                self.settings.llm_system_prompt.clone(),
                self.settings.llm_model.clone(),
            )
        };
        match self.settings.effective_llm_mode() {
            LlmMode::None => None,
            LlmMode::Local => refiner_from_settings(&LlmMode::Local),
            LlmMode::Gemini => {
                // secrets.env lives in the app data dir, next to the database.
                if let Some(data_dir) = std::path::Path::new(&self.db_path).parent() {
                    if let Some(openrouter_key) = crate::api_keys::ApiKeys::load(data_dir).openrouter {
                        return crate::llm_refiner::LlmRefiner::for_openrouter(
                            openrouter_key,
                            String::new(),
                            self.settings.llm_system_prompt.clone(),
                        );
                    }
                }
                refiner_from_settings(&LlmMode::Gemini)
            }
        }
    }

    pub fn run_gold_eval_local(&self, db: &Database, model_id: &str) -> AppResult<crate::eval::EvalRunResult> {
        let gold_segments = crate::eval::list_gold_segments(db)?;
        let mut hypotheses = Vec::new();

        let model_dir = self.model_manager.resolved_dir();
        let config = self.asr_config();
        self.asr_pool.warmup(&model_dir, &config)?;

        for gold in &gold_segments {
            let path = std::path::Path::new(&gold.audio_path);
            if !path.exists() {
                tracing::warn!("Gold segment audio path does not exist: {}", gold.audio_path);
                continue;
            }

            let (_sr, full_pcm) = match audio::decode_to_pcm(path) {
                Ok(pcm) => pcm,
                Err(e) => {
                    tracing::warn!("Failed to decode gold segment {}: {}", gold.id, e);
                    continue;
                }
            };

            let f32_pcm: Vec<f32> = full_pcm.iter().map(|&s| s as f32 / 32768.0).collect();

            let res = self.asr_pool.with_service(&model_dir, &config, |asr| {
                if !asr.is_available() {
                    return Err("ASR service unavailable".to_string());
                }
                asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
            });

            match res {
                Ok((text, _conf)) => {
                    hypotheses.push((gold.id.clone(), text));
                }
                Err(e) => {
                    tracing::warn!("ASR failed for gold segment {}: {}", gold.id, e);
                }
            }
        }

        crate::eval::run_gold_eval(db, model_id, hypotheses)
    }

    pub fn populate_hypotheses(&self, db: &Database, segment_id: &str, f32_pcm: &[f32]) -> AppResult<()> {
        let model_dir = self.model_manager.resolved_dir();

        // 1. OmniASR 300M
        let model_id_300m = "omniasr-ctc-300m";
        let config_300m = asr::AsrLoadConfig {
            model_size: crate::settings::AsrModelSize::CTC300M,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        let res_300m = self.asr_pool.with_service(&model_dir, &config_300m, |asr| {
            if !asr.is_available() {
                return None;
            }
            Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE))
        });
        match res_300m {
            Some(Ok((text, conf))) => insert_hypothesis_checked(db, segment_id, model_id_300m, text, conf)?,
            Some(Err(error)) => {
                tracing::warn!("{model_id_300m} hypothesis transcription failed for {segment_id}: {error}");
            }
            None => tracing::debug!("{model_id_300m} hypothesis model unavailable for {segment_id}"),
        }

        // 2. OmniASR 1B
        let model_id_1b = "omniasr-ctc-1b";
        let config_1b = asr::AsrLoadConfig {
            model_size: crate::settings::AsrModelSize::CTC1B,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        let res_1b = self.asr_pool.with_service(&model_dir, &config_1b, |asr| {
            if !asr.is_available() {
                return None;
            }
            Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE))
        });
        match res_1b {
            Some(Ok((text, conf))) => insert_hypothesis_checked(db, segment_id, model_id_1b, text, conf)?,
            Some(Err(error)) => {
                tracing::warn!("{model_id_1b} hypothesis transcription failed for {segment_id}: {error}");
            }
            None => tracing::debug!("{model_id_1b} hypothesis model unavailable for {segment_id}"),
        }

        self.populate_wsl_hypothesis_if_configured(db, segment_id)?;

        Ok(())
    }

    fn populate_wsl_hypothesis_if_configured(&self, db: &Database, segment_id: &str) -> AppResult<()> {
        if self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
            return Ok(());
        }
        if self.settings.external_asr_script_path().is_none() {
            return Ok(());
        }
        if db
            .get_hypotheses_for_segment(segment_id)?
            .iter()
            .any(|hyp| hyp.model_id == "omniasr-wsl-7b" && !hyp.transcript.trim().is_empty())
        {
            return Ok(());
        }

        match self.run_wsl_segment_transcript(segment_id) {
            Ok((raw_transcript, confidence)) => {
                insert_hypothesis_checked(db, segment_id, "omniasr-wsl-7b", raw_transcript, confidence)?;
            }
            Err(error) => {
                tracing::warn!("omniasr-wsl-7b hypothesis transcription failed for {segment_id}: {error}");
            }
        }
        Ok(())
    }

    fn run_wsl_segment_transcript(&self, segment_id: &str) -> AppResult<(String, Option<f64>)> {
        let Some(external_script) = self.settings.external_asr_script_path() else {
            return Err(AppError::Validation(
                "External ASR provider is not configured. Set the WSL script path in Settings before using the 7B provider.".into(),
            ));
        };

        let mut cmd = std::process::Command::new("wsl");
        cmd.arg("/root/cortex_env/bin/python3")
            .arg(external_script)
            .arg("--segment-id")
            .arg(segment_id)
            .arg("--stdout-only");

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let output = {
            // Spawn with explicit pipes so we keep a KILLABLE Child handle. The previous
            // cmd.output()-on-a-thread approach gave no handle: on timeout the reader
            // thread stayed blocked in output() and the wsl subprocess kept running,
            // leaking one thread + one zombie process per timed-out segment. Now we poll
            // for exit and, on timeout, kill + reap the child so nothing is left running.
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            let mut child =
                cmd.spawn().map_err(|e| AppError::Other(format!("WSL subprocess launch failed: {e}")))?;

            // Drain both pipes on threads so a chatty child can't deadlock on a full pipe
            // buffer; the readers finish when the pipes close (child exit or kill).
            let mut child_stdout = child.stdout.take();
            let mut child_stderr = child.stderr.take();
            let stdout_reader = std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(ref mut s) = child_stdout {
                    use std::io::Read;
                    let _ = s.read_to_end(&mut buf);
                }
                buf
            });
            let stderr_reader = std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(ref mut s) = child_stderr {
                    use std::io::Read;
                    let _ = s.read_to_end(&mut buf);
                }
                buf
            });

            let deadline = std::time::Instant::now() + Duration::from_secs(300);
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => {
                        if std::time::Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            break None;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(AppError::Other(format!("WSL subprocess wait failed: {e}")));
                    }
                }
            };

            let stdout = stdout_reader.join().unwrap_or_default();
            let stderr = stderr_reader.join().unwrap_or_default();
            match status {
                Some(status) => std::process::Output { status, stdout, stderr },
                None => {
                    return Err(AppError::Other(
                        "WSL 7B ASR process timed out after 5 minutes. Check WSL health.".into(),
                    ))
                }
            }
        };
        let stdout_str = String::from_utf8_lossy(&output.stdout);
        let stderr_str = String::from_utf8_lossy(&output.stderr);
        if !stdout_str.is_empty() {
            tracing::debug!("WSL 7B ASR stdout captured ({} bytes).", output.stdout.len());
        }
        if !stderr_str.is_empty() {
            tracing::debug!("WSL 7B ASR stderr captured ({} bytes).", output.stderr.len());
        }

        if !output.status.success() {
            let err_msg = subprocess_error_preview(&stderr_str);
            return Err(AppError::Other(format!("WSL 7B ASR process failed: {}", err_msg)));
        }

        parse_wsl_segment_result(&stdout_str)
    }

    pub fn align(
        &self,
        audio_path: &str,
        text: &str,
        alignment_json: Option<&str>,
    ) -> AppResult<(Vec<aligner::WordTimestamp>, aligner::AlignmentQuality)> {
        let (sample_rate, pcm) = audio::decode_to_pcm_with_timeout(audio_path, Duration::from_secs(120))?;
        let (sample_rate, pcm) = audio::ensure_pcm_16khz(sample_rate, pcm)?;
        if pcm.is_empty() {
            return Err(AppError::Audio(crate::error::AudioError::EmptyBuffer));
        }

        let pcm = chunking::slice_pcm_by_alignment(&pcm, sample_rate, alignment_json)?.0;

        let timer = crate::inference::InferenceTimer::start("align");
        let aligner = aligner::ForcedAligner::new(&self.model_manager.models_dir, self.settings.enable_gpu)
            .map_err(AppError::Other)?;
        let result = aligner.align(&pcm, audio::TARGET_SAMPLE_RATE, text);
        timer.finish(result.is_ok());
        Ok(result?)
    }

    pub fn get_waveform(
        &self,
        audio_path: &str,
        num_points: usize,
        alignment_json: Option<&str>,
    ) -> AppResult<Vec<f32>> {
        let (_sample_rate, pcm) = audio::decode_to_pcm_with_timeout(audio_path, Duration::from_secs(30))?;
        let (sample_rate, pcm) = audio::ensure_pcm_16khz(_sample_rate, pcm)?;
        let pcm = chunking::slice_pcm_by_alignment(&pcm, sample_rate, alignment_json)?.0;
        Ok(audio::compute_waveform(&pcm, num_points))
    }

    /// Clear the audio PCM cache.
    pub fn clear_audio_cache(&self) {
        audio::clear_pcm_cache();
    }

    /// Re-run acoustic diarization on existing segments (grouped by source audio file).
    pub fn rediarize_segments(&self, db: &Database, ids: &[String]) -> AppResult<usize> {
        if !self.settings.enable_diarization {
            return Err(AppError::Validation("Speaker diarization is disabled in settings".into()));
        }

        let all = db.get_segments_by_ids(ids)?;
        let targets: Vec<_> = all.into_iter().collect();
        if targets.is_empty() {
            return Ok(0);
        }

        let mut by_audio: std::collections::HashMap<String, Vec<SpeechSegment>> = std::collections::HashMap::new();
        for seg in targets {
            by_audio.entry(seg.audio_path.clone()).or_default().push(seg);
        }

        let mut updated = 0usize;
        for (audio_path, segs) in by_audio {
            let path = Path::new(&audio_path);
            if !path.exists() {
                continue;
            }
            let duration_ms = match audio::get_duration_ms(path) {
                Ok(duration_ms) => duration_ms,
                Err(error) => {
                    tracing::warn!("Rediarize duration probe failed for {audio_path}: {error}");
                    continue;
                }
            };
            let decode_timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);
            let (sample_rate, pcm) = match audio::decode_to_pcm_with_timeout(path, decode_timeout) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Rediarize decode failed for {audio_path}: {e}");
                    continue;
                }
            };
            let (sample_rate, pcm) = match audio::ensure_pcm_16khz(sample_rate, pcm) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Rediarize resample failed for {audio_path}: {e}");
                    continue;
                }
            };

            let mut chunk_ranges = Vec::new();
            let mut seg_order: Vec<String> = Vec::new();
            for seg in &segs {
                let (start, end) = if let Some(meta) =
                    seg.alignment_json.as_deref().and_then(chunking::SegmentSourceMeta::from_alignment_json)
                {
                    let s = chunking::ms_to_samples(meta.source_start_ms.max(0) as u32, sample_rate);
                    let e = chunking::ms_to_samples(meta.source_end_ms.max(0) as u32, sample_rate);
                    (s, e.min(pcm.len()))
                } else {
                    (0, pcm.len())
                };
                if end > start {
                    chunk_ranges.push((start, end));
                    seg_order.push(seg.id.clone());
                }
            }

            if chunk_ranges.is_empty() {
                continue;
            }

            let embedding_service =
                crate::diarization::SpeakerEmbeddingService::new(&self.model_manager.resolved_dir());
            let labels = crate::diarization::label_chunk_speakers(
                &pcm,
                sample_rate,
                &chunk_ranges,
                self.settings.max_speakers,
                &embedding_service,
            );

            for (idx, seg_id) in seg_order.iter().enumerate() {
                let Some(label) = labels.get(idx).and_then(|l| l.clone()) else {
                    continue;
                };
                let Some(mut seg) = segs.iter().find(|s| &s.id == seg_id).cloned() else {
                    continue;
                };
                seg.speaker_id = Some(label);
                match db.insert_segment(&seg) {
                    Ok(()) => updated += 1,
                    Err(error) => {
                        tracing::error!("Rediarize speaker update failed for {}: {error}", seg.id);
                    }
                }
            }
        }

        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use crate::audio::compute_waveform;
    use crate::cache::TranscriptCache;
    use crate::chunking::{slice_pcm_by_alignment, SegmentSourceMeta};
    use crate::db::{Database, SegmentHypothesis, SourceTranscriptRecord, SpeechSegment};
    use crate::fingerprint::AudioFingerprint;
    use crate::models::ModelManager;
    use crate::normalizer::SoraniNormalizer;
    use crate::settings::{AppSettings, AsrModelSize};
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn loop0_firing_respects_opt_in_and_fires_when_enabled() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        // Capture a CONFIRMED memory: the same confusion corrected on two segments -> hit_count 1,
        // past the anti-one-off guard.
        for id in ["pf-1", "pf-2"] {
            let seg = SpeechSegment {
                id: id.to_string(),
                audio_path: format!("/audio/{id}.wav"),
                raw_transcript: "ئەو ساڵە باش بوو".to_string(),
                ..Default::default()
            };
            db.insert_segment(&seg).unwrap();
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو")).unwrap();
        }
        let input = "ئەو ساڵە باش بوو";
        // Opt-in OFF -> the transcript is untouched.
        assert_eq!(super::apply_loop0_firing(false, &db, input), input, "firing must be off by default");
        // Opt-in ON -> the learned fix fires.
        assert_eq!(
            super::apply_loop0_firing(true, &db, input),
            "ئەو ساڵە خراپ بوو",
            "the learned correction must fire when enabled"
        );
    }

    #[test]
    fn build_refiner_routes_gemini_through_openrouter_when_key_present() {
        use crate::settings::LlmMode;
        let settings =
            AppSettings { llm_mode: LlmMode::Gemini, cloud_llm_opt_in: true, ..AppSettings::default() };
        let (pipeline, dir) = test_pipeline_with_settings(settings);
        std::fs::write(dir.path().join("secrets.env"), "OPENROUTER_API_KEY=test-or-key\n").unwrap();
        let refiner = pipeline.build_refiner().expect("a refiner should be built");
        assert!(
            refiner.endpoint.contains("openrouter.ai"),
            "Gemini mode + OpenRouter key should route through OpenRouter, got: {}",
            refiner.endpoint
        );
    }

    #[test]
    fn build_refiner_none_mode_disables_refinement() {
        use crate::settings::LlmMode;
        let (pipeline, _dir) =
            test_pipeline_with_settings(AppSettings { llm_mode: LlmMode::None, ..AppSettings::default() });
        assert!(pipeline.build_refiner().is_none());
    }

    #[test]
    fn build_refiner_respects_cloud_opt_out() {
        use crate::settings::LlmMode;
        // Gemini selected but cloud NOT opted in -> no refiner, even with a key present (privacy).
        let (pipeline, dir) = test_pipeline_with_settings(AppSettings {
            llm_mode: LlmMode::Gemini,
            cloud_llm_opt_in: false,
            ..AppSettings::default()
        });
        std::fs::write(dir.path().join("secrets.env"), "OPENROUTER_API_KEY=test-or-key\n").unwrap();
        assert!(pipeline.build_refiner().is_none(), "cloud opt-out must disable refinement even with a key");
    }

    #[test]
    fn build_scribe_segments_maps_timing_text_and_meta() {
        use crate::scribe_api::ScribeSegment;
        let segs = vec![
            ScribeSegment { text: "ئەمە یەکەمە".into(), source_start_ms: 0, source_end_ms: 1500 },
            ScribeSegment { text: "دووەم".into(), source_start_ms: 2000, source_end_ms: 3000 },
        ];
        let out = super::ProcessingPipeline::build_scribe_speech_segments(&segs, "/a/b.wav", 5000, false, false);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].raw_transcript, "ئەمە یەکەمە");
        assert_eq!(out[0].audio_path, "/a/b.wav");
        assert_eq!(out[0].duration_ms, 1500);
        assert!(!out[0].id.is_empty(), "each segment gets an id");
        assert!(out[0].normalized_transcript.is_none(), "auto_normalize=false -> no normalized text");
        let m0 =
            crate::chunking::SegmentSourceMeta::from_alignment_json(out[0].alignment_json.as_deref().unwrap()).unwrap();
        assert_eq!((m0.source_start_ms, m0.source_end_ms, m0.chunk_index, m0.chunk_count), (0, 1500, 0, 2));
        let m1 =
            crate::chunking::SegmentSourceMeta::from_alignment_json(out[1].alignment_json.as_deref().unwrap()).unwrap();
        assert_eq!((m1.source_start_ms, m1.source_end_ms), (2000, 3000));
    }

    #[test]
    fn build_scribe_segments_fills_open_end_and_normalizes() {
        use crate::scribe_api::ScribeSegment;
        // An open end (0) extends to the file duration; auto-normalize folds the Arabic Kaf.
        let segs = vec![ScribeSegment { text: "كوردی".into(), source_start_ms: 1000, source_end_ms: 0 }];
        let out = super::ProcessingPipeline::build_scribe_speech_segments(&segs, "/x.wav", 4000, true, false);
        assert_eq!(out.len(), 1);
        let m =
            crate::chunking::SegmentSourceMeta::from_alignment_json(out[0].alignment_json.as_deref().unwrap()).unwrap();
        assert_eq!((m.source_start_ms, m.source_end_ms), (1000, 4000), "open end clamps to duration");
        assert_eq!(out[0].duration_ms, 3000);
        let norm = out[0].normalized_transcript.as_deref().unwrap();
        assert_ne!(norm, "كوردی", "normalizer should fold the Arabic Kaf to Kurdish Kaf");
    }

    #[test]
    fn build_scribe_segments_empty_input() {
        assert!(super::ProcessingPipeline::build_scribe_speech_segments(&[], "/x.wav", 1000, false, false).is_empty());
    }

    #[test]
    fn build_scribe_segments_no_overflow_on_extreme_timestamps() {
        use crate::scribe_api::ScribeSegment;
        // Untrusted timestamps (saturated i64) must not overflow-panic the duration math.
        let segs = vec![ScribeSegment { text: "x".into(), source_start_ms: i64::MIN, source_end_ms: i64::MAX }];
        let out = super::ProcessingPipeline::build_scribe_speech_segments(&segs, "/x.wav", 1000, false, false);
        assert_eq!(out.len(), 1);
        assert!(out[0].duration_ms >= 0, "duration is never negative and never overflows");
    }

    #[test]
    fn scribe_segments_round_trip_through_the_database() {
        // Smoke test: the Scribe import path persists segments AND their playback timing survives a
        // real DB round-trip (migrate -> insert_segments_batch -> read back). Covers what the pure
        // build test cannot: serialization of text, duration, and the SegmentSourceMeta alignment.
        use crate::scribe_api::ScribeSegment;
        let db = crate::db::Database::open(":memory:").expect("open in-memory db");
        db.initialize().expect("migrate schema");
        let scribe = vec![
            ScribeSegment { text: "ئەمە یەکەمە".into(), source_start_ms: 0, source_end_ms: 1500 },
            ScribeSegment { text: "دووەمین پارچە".into(), source_start_ms: 2000, source_end_ms: 5000 },
        ];
        let built = super::ProcessingPipeline::build_scribe_speech_segments(&scribe, "/audio/x.wav", 6000, false, false);
        db.insert_segments_batch(&built).expect("insert batch");

        let mut back = db.get_segments(None).expect("read back");
        assert_eq!(back.len(), 2, "both Scribe segments persisted");
        let start_of = |s: &crate::db::SpeechSegment| {
            crate::chunking::SegmentSourceMeta::from_alignment_json(s.alignment_json.as_deref().unwrap_or(""))
                .map_or(i64::MAX, |m| m.source_start_ms)
        };
        back.sort_by_key(start_of);
        assert_eq!(back[0].raw_transcript, "ئەمە یەکەمە", "text survives the round-trip");
        assert_eq!(back[0].audio_path, "/audio/x.wav");
        assert_eq!(back[0].duration_ms, 1500);
        let m0 =
            crate::chunking::SegmentSourceMeta::from_alignment_json(back[0].alignment_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            (m0.source_start_ms, m0.source_end_ms, m0.chunk_index, m0.chunk_count),
            (0, 1500, 0, 2),
            "alignment time-range + chunk indices survive persistence — playback hits the right slice"
        );
        let m1 =
            crate::chunking::SegmentSourceMeta::from_alignment_json(back[1].alignment_json.as_deref().unwrap()).unwrap();
        assert_eq!((m1.source_start_ms, m1.source_end_ms), (2000, 5000));
    }

    #[test]
    fn scribe_key_gate_requires_opt_in() {
        // Key present but cloud STT NOT opted in -> None (no cloud calls without explicit opt-in).
        let (pipeline, dir) =
            test_pipeline_with_settings(AppSettings { cloud_stt_opt_in: false, ..AppSettings::default() });
        std::fs::write(dir.path().join("secrets.env"), "ELEVENLABS_API_KEY=test-scribe-key\n").unwrap();
        assert!(pipeline.scribe_api_key_if_enabled().is_none());
    }

    #[test]
    fn scribe_key_gate_returns_key_when_opted_in() {
        let (pipeline, dir) =
            test_pipeline_with_settings(AppSettings { cloud_stt_opt_in: true, ..AppSettings::default() });
        std::fs::write(dir.path().join("secrets.env"), "ELEVENLABS_API_KEY=test-scribe-key\n").unwrap();
        assert_eq!(pipeline.scribe_api_key_if_enabled().as_deref(), Some("test-scribe-key"));
    }

    #[test]
    fn scribe_key_gate_none_without_key() {
        let (pipeline, _dir) =
            test_pipeline_with_settings(AppSettings { cloud_stt_opt_in: true, ..AppSettings::default() });
        assert!(pipeline.scribe_api_key_if_enabled().is_none(), "opted in but no key -> None");
    }

    #[test]
    fn cancelled_directory_import_clears_running_status() {
        // Round-2 audit MEDIUM: a cancel mid directory-import early-returned (token.check()?) before
        // finish_import_status, so get_import_status().running stayed true forever. The RAII guard now
        // clears it on every exit path.
        let (pipeline, dir) = test_pipeline_with_settings(AppSettings::default());
        let import_dir = dir.path().join("to_import");
        std::fs::create_dir_all(&import_dir).unwrap();
        std::fs::write(import_dir.join("a.wav"), b"not-real-audio").unwrap();

        let token = crate::cancel::CancellationToken::new();
        token.cancel(); // pre-cancel so the loop's first token.check()? returns Err before any decode

        let res = pipeline.import_directory_with_agent_run_id(&import_dir, Some(token), None, |_evt| {});
        assert!(res.is_err(), "a cancelled import returns Err");
        assert!(!pipeline.import_status().running, "running must be cleared after a cancelled import");
    }

    #[test]
    fn fire_loop0_if_enabled_method_uses_the_pipelines_own_db() {
        // The method (used by the cached / non-WSL transcribe paths) opens its own DB connection.
        let settings = AppSettings { loop0_firing_enabled: true, ..AppSettings::default() };
        let (pipeline, _dir) = test_pipeline_with_settings(settings);
        {
            let db = pipeline.open_db().unwrap();
            db.initialize().unwrap();
            for id in ["fm-1", "fm-2"] {
                let seg = SpeechSegment {
                    id: id.to_string(),
                    audio_path: format!("/a/{id}.wav"),
                    raw_transcript: "ئەو ساڵە باش بوو".to_string(),
                    ..Default::default()
                };
                db.insert_segment(&seg).unwrap();
                db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو")).unwrap();
            }
        }
        assert_eq!(
            pipeline.fire_loop0_if_enabled("ئەو ساڵە باش بوو"),
            "ئەو ساڵە خراپ بوو",
            "the method fires the learned fix using the pipeline's own db when enabled"
        );

        // With the opt-in off, the method is a pure no-op (and never even opens the db).
        let (off_pipeline, _dir2) =
            test_pipeline_with_settings(AppSettings { loop0_firing_enabled: false, ..AppSettings::default() });
        assert_eq!(
            off_pipeline.fire_loop0_if_enabled("ئەو ساڵە باش بوو"),
            "ئەو ساڵە باش بوو",
            "the method is a no-op when the opt-in is off"
        );
    }

    fn test_pipeline_with_settings(settings: AppSettings) -> (super::ProcessingPipeline, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("db.sqlite").to_string_lossy().to_string();
        let models_dir = dir.path().join("models");
        let pipeline = super::ProcessingPipeline::new(
            db_path,
            Arc::new(SoraniNormalizer::new()),
            Arc::new(TranscriptCache::new(10)),
            Arc::new(AudioFingerprint::new()),
            Arc::new(settings),
            Arc::new(ModelManager::new(models_dir)),
        );
        (pipeline, dir)
    }

    fn test_pipeline_for_status() -> (super::ProcessingPipeline, tempfile::TempDir) {
        test_pipeline_with_settings(AppSettings::default())
    }

    #[test]
    fn wsl_without_script_uses_local_asr_fallback() {
        let settings = AppSettings { asr_model_size: AsrModelSize::WSL7B, ..AppSettings::default() };
        let (pipeline, _dir) = test_pipeline_with_settings(settings);

        assert!(!pipeline.should_use_wsl_primary_asr());
        let has_1b = crate::models::ModelManager::new(pipeline.model_manager.resolved_dir()).omniasr_ctc_1b_present();
        let expected_size = if has_1b { AsrModelSize::CTC1B } else { AsrModelSize::CTC300M };
        let expected_id = if has_1b { "omniasr-ctc-1b" } else { "omniasr-ctc-300m" };
        assert_eq!(pipeline.active_local_asr_model_size(), expected_size);
        assert_eq!(pipeline.local_asr_model_id(), expected_id);
    }

    #[test]
    fn wsl_with_script_uses_wsl_primary_asr() {
        let settings = AppSettings {
            asr_model_size: AsrModelSize::WSL7B,
            external_asr_script_path: "/root/cortex_env/omniasr.py".to_string(),
            ..AppSettings::default()
        };
        let (pipeline, _dir) = test_pipeline_with_settings(settings);

        assert!(pipeline.should_use_wsl_primary_asr());
    }

    fn test_segment(id: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: format!("{id}.wav"),
            raw_transcript: "raw transcript".to_string(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 4000,
            speaker_id: None,
            verified: false,
            confidence: Some(0.9),
            ctc_score: None,
            clipping_ratio: Some(0.0),
            rms_db: Some(-20.0),
            snr_db: Some(20.0),
            split: Some("train".to_string()),
            ood_score: None,
            verdict: None,
            verdict_transcript: None,
            rationale: None,
            evidence_json: None,
            agent_confidence: None,
            escalated: false,
            human_decision: None,
            corrected_at: None,
            is_gold: false,
            alignment_quality: None,
        }
    }

    fn insert_hypothesis(db: &Database, segment_id: &str, model_id: &str, transcript: &str) {
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment_id.to_string(),
            model_id: model_id.to_string(),
            transcript: transcript.to_string(),
            confidence: Some(0.9),
        })
        .unwrap();
    }

    fn source_record_for_audio(
        audio_path: &Path,
        model_id: &str,
        transcript_path: &Path,
        transcript_text: &str,
    ) -> SourceTranscriptRecord {
        let identity = super::source_audio_identity(audio_path).expect("hash test audio");
        SourceTranscriptRecord {
            audio_path: audio_path.to_string_lossy().to_string(),
            model_id: model_id.to_string(),
            audio_content_hash: Some(identity.content_hash),
            audio_size_bytes: Some(identity.size_bytes),
            transcript_path: transcript_path.to_string_lossy().to_string(),
            transcript_text: transcript_text.to_string(),
            created_at: None,
        }
    }

    #[test]
    fn multi_model_hypothesis_stage_reports_verified_coverage() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let segment = test_segment("covered-seg");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "best phrase");
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "backup phrase");

        let event = super::multi_model_hypothesis_stage(&db, "covered.wav", std::slice::from_ref(&segment));

        match event {
            super::PipelineEvent::AgentStage { stage, status, detail, current, total, .. } => {
                assert_eq!(stage, "multi_model_hypotheses");
                assert_eq!(status, "completed");
                assert_eq!(current, 1);
                assert_eq!(total, 1);
                assert!(detail.contains("omniasr-wsl-7b"));
                assert!(detail.contains("omniasr-ctc-300m"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn multi_model_hypothesis_stage_blocks_incomplete_coverage() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let covered = test_segment("covered-seg");
        let blocked = test_segment("blocked-seg");
        db.insert_segment(&covered).unwrap();
        db.insert_segment(&blocked).unwrap();
        insert_hypothesis(&db, &covered.id, "omniasr-wsl-7b", "best phrase");
        insert_hypothesis(&db, &covered.id, "omniasr-ctc-300m", "backup phrase");
        insert_hypothesis(&db, &blocked.id, "omniasr-wsl-7b", "single model phrase");

        let event = super::multi_model_hypothesis_stage(&db, "mixed.wav", &[covered, blocked]);

        match event {
            super::PipelineEvent::AgentStage { stage, status, detail, current, total, .. } => {
                assert_eq!(stage, "multi_model_hypotheses");
                assert_eq!(status, "blocked");
                assert_eq!(current, 1);
                assert_eq!(total, 2);
                assert!(detail.contains("Only 1/2"));
                assert!(detail.contains("blocked-seg"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn import_status_recovers_poisoned_lock() {
        let (pipeline, _dir) = test_pipeline_for_status();
        let handle = pipeline.import_status_handle();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = handle.lock().expect("lock import status");
            panic!("poison import status");
        }));

        pipeline.set_import_status(3, 7, "file.wav");
        let status = pipeline.import_status();
        assert!(status.running);
        assert_eq!(status.current, 3);
        assert_eq!(status.total, 7);
        assert_eq!(status.file, "file.wav");

        pipeline.finish_import_status();
        assert!(!pipeline.import_status().running);
    }

    #[test]
    fn service_locks_recover_poisoned_state() {
        let (pipeline, _dir) = test_pipeline_for_status();

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = pipeline.diarization_service.lock().expect("lock diarization service");
            panic!("poison diarization service");
        }));
        assert!(pipeline.lock_diarization_service().is_none());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = pipeline.denoiser_service.lock().expect("lock denoiser service");
            panic!("poison denoiser service");
        }));
        assert!(pipeline.lock_denoiser_service().is_none());
    }

    #[test]
    fn decoded_window_accumulator_recovers_poisoned_lock() {
        let windows = std::sync::Mutex::new(vec![crate::audio::PcmWindow {
            offset_ms: 0,
            sample_rate: 16_000,
            pcm: vec![1, 2, 3],
        }]);

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = windows.lock().expect("lock decoded windows");
            panic!("poison decoded windows");
        }));

        super::lock_decoded_windows(&windows).push(crate::audio::PcmWindow {
            offset_ms: 1000,
            sample_rate: 16_000,
            pcm: vec![4, 5, 6],
        });

        let recovered = super::lock_decoded_windows(&windows).clone();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[1].offset_ms, 1000);
        assert_eq!(recovered[1].pcm, vec![4, 5, 6]);
    }

    #[test]
    fn wsl_primary_import_pass_skips_missing_script_after_local_fallback() {
        let settings = AppSettings { asr_model_size: AsrModelSize::WSL7B, ..AppSettings::default() };
        let (pipeline, dir) = test_pipeline_with_settings(settings);
        let db_path = dir.path().join("db.sqlite");
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let segment = SpeechSegment {
            id: "local-fallback".to_string(),
            audio_path: "C:\\missing\\audio.wav".to_string(),
            raw_transcript: "local fallback transcript".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).unwrap();

        let mut segments = vec![segment];
        let updated = pipeline.run_primary_wsl_pass_for_import(&db, &mut segments).unwrap();

        assert_eq!(updated, 0);
        assert_eq!(segments[0].raw_transcript, "local fallback transcript");
        assert_eq!(segments[0].verdict, None);
        assert!(!segments[0].escalated);
        assert_eq!(segments[0].rationale, None);

        let fresh = db.get_segments_by_ids(&["local-fallback".to_string()]).unwrap().remove(0);
        assert_eq!(fresh.raw_transcript, "local fallback transcript");
        assert_eq!(fresh.verdict, None);
        assert!(!fresh.escalated);
        assert_eq!(fresh.rationale, None);
    }

    #[test]
    fn parses_wsl_segment_result_from_stdout_marker() {
        let stdout = "loading model\n__RESULT__={\"raw_transcript\":\"دەقی دروست\",\"confidence\":0.94}\ndone\n";
        let (text, confidence) = super::parse_wsl_segment_result(stdout).unwrap();
        assert_eq!(text, "دەقی دروست");
        assert_eq!(confidence, Some(0.94));
    }

    #[test]
    fn rejects_missing_wsl_segment_result_stdout_marker() {
        let err = super::parse_wsl_segment_result("loading model\nfinished without result\n").unwrap_err();
        assert!(err.to_string().contains("did not return a valid transcript"));
    }

    #[test]
    fn configured_source_reference_failure_is_fatal_before_chunking() {
        let settings = AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: "test-key".to_string(),
            source_reference_models: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
            ..AppSettings::default()
        };
        let (pipeline, dir) = test_pipeline_with_settings(settings);
        let db_path = dir.path().join("db.sqlite");
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let missing_audio = dir.path().join("missing-source.wav");

        let err = pipeline
            .ensure_source_reference_transcripts(&missing_audio, &db)
            .expect_err("configured source reference failure must be fatal");

        let message = err.to_string();
        assert!(message.contains("All whole-file reference transcript models failed"));
        assert!(message.contains("gemini-2.5-pro"));
        assert!(message.contains("gemini-2.5-flash"));
    }

    #[test]
    fn partial_source_reference_failure_is_fatal_before_chunking() {
        let settings = AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: "test-key".to_string(),
            source_reference_models: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
            ..AppSettings::default()
        };
        let (pipeline, dir) = test_pipeline_with_settings(settings);
        let db_path = dir.path().join("db.sqlite");
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let missing_audio = dir.path().join("missing-source.wav");
        let transcript_path = dir.path().join("existing-reference.txt");
        std::fs::write(&transcript_path, "existing whole-file reference").unwrap();
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: missing_audio.to_string_lossy().to_string(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: None,
            audio_size_bytes: None,
            transcript_path: transcript_path.to_string_lossy().to_string(),
            transcript_text: "existing whole-file reference".to_string(),
            created_at: None,
        })
        .unwrap();

        let err = pipeline
            .ensure_source_reference_transcripts(&missing_audio, &db)
            .expect_err("partial source reference failure must be fatal");

        let message = err.to_string();
        assert!(message.contains("All whole-file reference transcript models failed before chunking"));
        assert!(message.contains("incomplete source-reference evidence"));
        assert!(message.contains("gemini-2.5-pro"));
        assert!(message.contains("gemini-2.5-flash"));
    }

    #[test]
    fn edited_source_reference_file_syncs_database_before_reuse() {
        let settings = AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: "test-key".to_string(),
            source_reference_models: vec!["gemini-2.5-pro".to_string()],
            ..AppSettings::default()
        };
        let (pipeline, dir) = test_pipeline_with_settings(settings);
        let db_path = dir.path().join("db.sqlite");
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let audio = dir.path().join("manual-source.wav");
        std::fs::write(&audio, b"audio-v1").unwrap();
        let transcript_path = dir.path().join("manual-source-reference.txt");
        std::fs::write(&transcript_path, "manual corrected whole-file reference").unwrap();
        let mut record =
            source_record_for_audio(&audio, "gemini-2.5-pro", &transcript_path, "stale database reference");
        record.transcript_text = "stale database reference".to_string();
        db.upsert_source_transcript(&record).unwrap();

        let records = pipeline.ensure_source_reference_transcripts(&audio, &db).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].transcript_text, "manual corrected whole-file reference");
        let synced = db
            .get_source_transcript(&audio.to_string_lossy(), "gemini-2.5-pro")
            .unwrap()
            .expect("synced source transcript");
        assert_eq!(synced.transcript_text, "manual corrected whole-file reference");
    }

    #[test]
    fn changed_audio_path_does_not_reuse_stale_source_reference() {
        let settings = AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: "test-key".to_string(),
            source_reference_models: vec!["gemini-2.5-pro".to_string()],
            ..AppSettings::default()
        };
        let (pipeline, dir) = test_pipeline_with_settings(settings);
        let db_path = dir.path().join("db.sqlite");
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let audio = dir.path().join("same-path.wav");
        std::fs::write(&audio, b"audio-v1").unwrap();
        let transcript_path = dir.path().join("same-path-reference.txt");
        std::fs::write(&transcript_path, "cached whole-file reference").unwrap();
        let record = source_record_for_audio(&audio, "gemini-2.5-pro", &transcript_path, "cached whole-file reference");
        db.upsert_source_transcript(&record).unwrap();

        std::fs::write(&audio, b"audio-v2-with-different-content").unwrap();

        let err = pipeline
            .ensure_source_reference_transcripts(&audio, &db)
            .expect_err("stale source reference must not be reused for changed audio bytes");

        let message = err.to_string();
        assert!(message.contains("All whole-file reference transcript models failed before chunking"));
        assert!(message.contains("gemini-2.5-pro"));
    }

    #[test]
    fn empty_source_reference_file_is_not_reused() {
        let settings = AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: "test-key".to_string(),
            source_reference_models: vec!["gemini-2.5-pro".to_string()],
            ..AppSettings::default()
        };
        let (pipeline, dir) = test_pipeline_with_settings(settings);
        let db_path = dir.path().join("db.sqlite");
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let missing_audio = dir.path().join("missing-source.wav");
        let transcript_path = dir.path().join("empty-reference.txt");
        std::fs::write(&transcript_path, " \n").unwrap();
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: missing_audio.to_string_lossy().to_string(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: None,
            audio_size_bytes: None,
            transcript_path: transcript_path.to_string_lossy().to_string(),
            transcript_text: "existing whole-file reference".to_string(),
            created_at: None,
        })
        .unwrap();

        let err = pipeline
            .ensure_source_reference_transcripts(&missing_audio, &db)
            .expect_err("empty saved reference file must not be reused");

        let message = err.to_string();
        assert!(message.contains("All whole-file reference transcript models failed before chunking"));
        assert!(message.contains("Cannot inspect audio file"));
    }

    #[test]
    fn source_reference_cloud_opt_in_without_key_is_fatal_before_chunking() {
        let settings = AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: String::new(),
            llm_api_key_configured: false,
            source_reference_models: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
            ..AppSettings::default()
        };
        let (pipeline, dir) = test_pipeline_with_settings(settings);
        let db_path = dir.path().join("db.sqlite");
        let db = Database::open(db_path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let missing_audio = dir.path().join("missing-source.wav");

        let err = pipeline
            .ensure_source_reference_transcripts(&missing_audio, &db)
            .expect_err("enabled source reference mode without a key must be fatal");

        let message = err.to_string();
        assert!(message.contains("Gemini API key is required for whole-file reference transcript"));
        assert!(message.contains("jury cloud opt-in is enabled"));
    }

    #[test]
    fn speaker_hint_from_filename_stem_when_multi_chunk_enabled() {
        let settings = AppSettings { assign_speaker_from_filename: true, ..AppSettings::default() };
        let path = Path::new(r"C:\recordings\interview_guest.wav");
        let speaker = if settings.assign_speaker_from_filename {
            path.file_stem().map(|s| s.to_string_lossy().into_owned())
        } else {
            None
        };
        assert_eq!(speaker.as_deref(), Some("interview_guest"));
    }

    #[test]
    fn speaker_hint_skipped_for_single_chunk_setting_off() {
        let settings = AppSettings { assign_speaker_from_filename: false, ..AppSettings::default() };
        let chunk_count = 3u32;
        let speaker =
            if chunk_count > 1 && settings.assign_speaker_from_filename { Some("stem".to_string()) } else { None };
        assert!(speaker.is_none());
    }

    #[test]
    fn decode_pcm_windows_splits_long_wav() {
        use crate::audio;
        use hound::{WavSpec, WavWriter};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("long.wav");
        let spec =
            WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = WavWriter::create(&path, spec).unwrap();
        for i in 0..(100 * 16000) {
            let t = i as f64 / 16000.0;
            let s = (i16::MAX as f64 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16;
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();

        let mut windows = Vec::new();
        audio::decode_pcm_windows(&path, 30_000, |w| {
            windows.push((w.offset_ms, w.pcm.len()));
            Ok(())
        })
        .unwrap();

        assert!(windows.len() >= 3, "expected multiple windows, got {windows:?}");
        assert_eq!(windows.first().map(|w| w.0), Some(0));
    }

    #[test]
    fn should_stream_decode_for_long_file() {
        use crate::chunking;
        assert!(chunking::should_stream_decode(60_000, 15_000));
        assert!(!chunking::should_stream_decode(10_000, 15_000));
    }

    #[test]
    fn get_waveform_chunk_slice_is_shorter_than_full() {
        let pcm: Vec<i16> = (0..32000).map(|i| (i as i16).wrapping_mul(100)).collect();
        let full = compute_waveform(&pcm, 50);
        let meta = SegmentSourceMeta { source_start_ms: 500, source_end_ms: 1500, chunk_index: 0, chunk_count: 1 };
        let (chunk_pcm, _) = slice_pcm_by_alignment(&pcm, 16000, Some(&meta.to_alignment_json())).unwrap();
        let chunk = compute_waveform(&chunk_pcm, 50);
        assert_eq!(full.len(), 50);
        assert_eq!(chunk.len(), 50);
        assert_ne!(full, chunk);
    }

    #[test]
    fn subprocess_error_preview_handles_empty_stderr() {
        assert_eq!(super::subprocess_error_preview(" \r\n\t "), "(no stderr output)");
    }

    #[test]
    fn subprocess_error_preview_caps_long_stderr_without_splitting_chars() {
        let long = format!("{}{}", "ژ".repeat(super::SUBPROCESS_ERROR_PREVIEW_CHARS), "extra");

        let preview = super::subprocess_error_preview(&long);

        assert!(preview.contains("[truncated subprocess stderr]"));
        assert!(!preview.contains("extra"));
        assert_eq!(preview.lines().next().unwrap().chars().count(), super::SUBPROCESS_ERROR_PREVIEW_CHARS);
    }
}
