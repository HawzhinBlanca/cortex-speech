use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::{AppSettings, AsrModelSize, LlmMode};
use cortex_speech_app_lib::{quality, review_pool};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

fn app_data_dir() -> PathBuf {
    std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|path| PathBuf::from(path).join("cortex-speech")))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("cortex-speech"))
}

fn collect_prepared_wavs(directory: &Path) -> Result<Vec<PathBuf>, String> {
    fn visit(directory: &Path, depth: usize, wavs: &mut Vec<PathBuf>) -> Result<(), String> {
        if depth > 32 {
            return Err(format!("prepared voice directory nesting exceeds 32 levels at {}", directory.display()));
        }
        let entries = std::fs::read_dir(directory)
            .map_err(|error| format!("cannot read prepared voice directory {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("cannot read entry under {}: {error}", directory.display()))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
            if kind.is_dir() {
                visit(&path, depth + 1, wavs)?;
            } else if kind.is_file() && path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
            {
                wavs.push(path);
            }
        }
        Ok(())
    }

    let mut wavs = Vec::new();
    visit(directory, 0, &mut wavs)?;
    wavs.sort_unstable();
    if wavs.is_empty() {
        return Err(format!("no WAV files found in prepared voice directory {}", directory.display()));
    }
    Ok(wavs)
}

fn is_exact_champion_segment(segment: &cortex_speech_app_lib::db::SpeechSegment, champion_model_id: &str) -> bool {
    segment.model_version_id.as_deref() == Some(champion_model_id)
        && !segment.cloud_call
        && !quality::is_placeholder_transcript(&segment.raw_transcript)
}

/// Import final, one-character WAVs two files at a time so the two warm 7B workers receive genuinely
/// concurrent work. Each worker owns its SQLite connection; all durable journal bookkeeping stays on
/// the coordinator connection. A wave is joined before its results are accepted, and any non-champion
/// or non-1:1 result is removed and halts the run.
fn import_prepared_voice_parallel(
    pipeline: &ProcessingPipeline,
    db: &Database,
    db_path: &Path,
    target_dir: &Path,
) -> Result<(usize, usize), String> {
    let files = collect_prepared_wavs(target_dir)?;
    let total = files.len();
    let champion_model_id = review_pool::current_champion_7b_model_id(db)?;
    let job_id = db
        .begin_import_job(&target_dir.to_string_lossy(), total)
        .map_err(|error| format!("cannot create prepared import journal: {error}"))?;
    let file_concurrency = std::env::var("CORTEX_7B_CONCURRENCY")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| (1..=2).contains(value))
        .unwrap_or(2);
    println!("Prepared-file concurrency: {file_concurrency} (one file per warm GPU worker).");

    let mut succeeded = 0usize;
    let mut pending = Vec::new();
    for file in files {
        let path_text = file.to_string_lossy().to_string();
        let existing_ids = db
            .segment_ids_for_audio_path(&path_text)
            .map_err(|error| format!("cannot inspect existing rows for {}: {error}", file.display()))?;
        if existing_ids.is_empty() {
            pending.push(file);
            continue;
        }
        let existing = db
            .get_segments_by_ids(&existing_ids)
            .map_err(|error| format!("cannot read existing rows for {}: {error}", file.display()))?;
        if existing.len() == 1 && is_exact_champion_segment(&existing[0], &champion_model_id) {
            db.mark_import_file_done(&job_id, &path_text)
                .map_err(|error| format!("cannot journal existing file {}: {error}", file.display()))?;
            succeeded += 1;
            println!(
                "Progress: {succeeded}/{total} - {} - Exact champion row reused",
                file.file_name().and_then(|name| name.to_str()).unwrap_or("unknown")
            );
            continue;
        }

        // This directory is declared prepared and therefore must be 1 WAV -> 1 exact champion row.
        // Delete only the incomplete/non-canonical stage; the database's review-authority trigger
        // refuses this operation if any human evidence exists, turning that case into a hard stop.
        db.delete_segments_batch(&existing_ids)
            .map_err(|error| format!("cannot replace invalid staged rows for {}: {error}", file.display()))?;
        pending.push(file);
    }

    if pending.is_empty() {
        db.complete_import_job(&job_id).map_err(|error| format!("cannot complete prepared import journal: {error}"))?;
        return Ok((total, succeeded));
    }

    // Run the full startup/integrity validation sequentially exactly once per worker. Opening both
    // inside each concurrent wave made their FTS integrity probes race each other for SQLite locks;
    // the second correctly failed closed, but no inference concurrency was reached. The validated
    // connections themselves are Send and remain owned by one worker slot at a time below.
    let worker_count = file_concurrency.min(pending.len());
    let mut worker_databases = Vec::with_capacity(worker_count);
    for worker_index in 0..worker_count {
        worker_databases
            .push(Database::open_with_retry(&db_path.to_string_lossy()).map_err(|error| {
                format!("prepared worker {} database validation failed: {error}", worker_index + 1)
            })?);
    }

    for wave in pending.chunks(worker_count) {
        let outcomes: Vec<(PathBuf, Result<Vec<cortex_speech_app_lib::db::SpeechSegment>, String>)> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = worker_databases
                    .iter_mut()
                    .zip(wave.iter().cloned())
                    .map(|(worker_db, file)| {
                        let worker_pipeline = pipeline.clone();
                        scope.spawn(move || {
                            let result = worker_pipeline
                                .process_single_file(&file, worker_db)
                                .map_err(|error| error.to_string());
                            (file, result)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle.join().unwrap_or_else(|_| {
                            (PathBuf::from("<panicked prepared worker>"), Err("prepared import worker panicked".into()))
                        })
                    })
                    .collect()
            });

        for (file, result) in outcomes {
            let segments = result.map_err(|error| {
                format!("prepared import HALTED at {} after {succeeded}/{total} completed: {error}", file.display())
            })?;
            let valid = segments.len() == 1 && is_exact_champion_segment(&segments[0], &champion_model_id);
            if !valid {
                let ids: Vec<String> = segments.iter().map(|segment| segment.id.clone()).collect();
                if let Err(error) = db.delete_segments_batch(&ids) {
                    return Err(format!(
                        "prepared import produced invalid rows for {} and rollback failed: {error}",
                        file.display()
                    ));
                }
                return Err(format!(
                    "prepared import HALTED: {} produced {} row(s), but exactly one local {champion_model_id} row is required",
                    file.display(),
                    segments.len()
                ));
            }
            let path_text = file.to_string_lossy().to_string();
            db.mark_import_file_done(&job_id, &path_text)
                .map_err(|error| format!("cannot journal completed file {}: {error}", file.display()))?;
            succeeded += 1;
            println!(
                "Progress: {succeeded}/{total} - {} - OmniASR-7B champion complete",
                file.file_name().and_then(|name| name.to_str()).unwrap_or("unknown")
            );
        }
    }

    db.complete_import_job(&job_id).map_err(|error| format!("cannot complete prepared import journal: {error}"))?;
    Ok((total, succeeded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_reuse_requires_exact_local_champion_evidence() {
        let mut segment = cortex_speech_app_lib::db::SpeechSegment {
            raw_transcript: "دەنگێکی ڕاستەقینە".into(),
            model_version_id: Some("champion-v1".into()),
            ..Default::default()
        };
        assert!(is_exact_champion_segment(&segment, "champion-v1"));

        segment.cloud_call = true;
        assert!(!is_exact_champion_segment(&segment, "champion-v1"));
        segment.cloud_call = false;
        segment.raw_transcript = "[Pending WSL 7B ASR]".into();
        assert!(!is_exact_champion_segment(&segment, "champion-v1"));
        segment.raw_transcript = "دەنگ".into();
        assert!(!is_exact_champion_segment(&segment, "different-deployment"));
    }

    #[test]
    fn prepared_inventory_accepts_only_wavs_and_is_deterministic() {
        let root = tempfile::tempdir().expect("tempdir");
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).expect("nested directory");
        std::fs::write(root.path().join("b.WAV"), b"wave").expect("wav b");
        std::fs::write(nested.join("a.wav"), b"wave").expect("wav a");
        std::fs::write(root.path().join("ignore.mp3"), b"not selected").expect("other file");

        let files = collect_prepared_wavs(root.path()).expect("collect prepared wavs");
        assert_eq!(files, vec![root.path().join("b.WAV"), nested.join("a.wav")]);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    println!("Starting Batch Importer...");

    let app_data_dir = app_data_dir();

    // Single-instance guard shared with the GUI (same cortex.lock): refuse to run against the live DB
    // while the app — or another importer — is open, so two writers never contend on the WAL DB or the
    // one warm 7B server, and per-process import dedup can't double-import a file. Return the error from
    // main (a recoverable, non-panicking exit) rather than aborting inside processing.
    let _lock = cortex_speech_app_lib::flock::InstanceLock::try_lock(&app_data_dir)
        .map_err(|e| format!("Cannot start batch importer: {e}. Close the Cortex app (or another importer) first."))?;

    let db_path = app_data_dir.join("cortex-speech.db");

    let db = Database::open_with_retry(&db_path.to_string_lossy())?;
    if let Some(path) = cortex_speech_app_lib::snapshot::initialize_with_required_pre_migration_pin(&db, &app_data_dir)?
    {
        println!("Pre-migration database safety pin: {}", path.display());
    }

    // This binary persists production drafts, so it must share the desktop's champion-only loader;
    // raw `load()` is reserved for explicit offline diagnostic tools.
    let mut settings = AppSettings::load_production(&app_data_dir.join("settings.json"));

    let mut args = std::env::args().skip(1);
    let first_arg = args.next();
    let (prepared_voice, target_dir) = match first_arg.as_deref() {
        Some("--prepared-voice") => (true, args.next().map(PathBuf::from)),
        Some(path) => (false, Some(PathBuf::from(path))),
        None => (
            std::env::var_os("CORTEX_IMPORT_PREPARED_VOICE").as_deref() == Some(std::ffi::OsStr::new("1")),
            std::env::var_os("CORTEX_IMPORT_DIR").map(PathBuf::from),
        ),
    };
    if args.next().is_some() {
        return Err("Usage: batch_importer [--prepared-voice] <audio-directory>".into());
    }

    // Prepared voice datasets are already one-speaker, cleaned, and chunked. This mode removes every
    // optional inference path that could waste CPU/RAM, alter the prepared audio, or confuse model
    // provenance. The production loader above already forces the champion; repeat the invariant here
    // so a future settings refactor fails closed at this executable boundary.
    if prepared_voice {
        settings.enforce_desktop_asr_canon();
        settings.multi_engine_hypotheses = false;
        settings.use_finetuned_asr = false;
        settings.enable_diarization = false;
        settings.enable_denoising = false;
        settings.auto_align = false;
        settings.assign_speaker_from_filename = false;
        settings.llm_mode = LlmMode::None;
        settings.cloud_llm_opt_in = false;
        settings.jury_cloud_opt_in = false;
        settings.ger_refinement_enabled = false;
        println!(
            "Prepared-voice mode: OmniASR-7B champion only; auxiliary ASR, cloud reference/refinement, diarization, denoising, and forced alignment are disabled."
        );
    }
    if settings.asr_model_size != AsrModelSize::WSL7B || settings.multi_engine_hypotheses || settings.use_finetuned_asr
    {
        return Err("Production batch import refused: OmniASR-7B is not the sole ASR engine.".into());
    }

    let normalizer = Arc::new(SoraniNormalizer::new());
    let cache = Arc::new(TranscriptCache::new(1000));
    let fingerprint = Arc::new(AudioFingerprint::new());
    let model_manager = Arc::new(ModelManager::new(app_data_dir.join("models")));

    let pipeline = ProcessingPipeline::new(
        db_path.to_string_lossy().to_string(),
        normalizer,
        cache,
        fingerprint,
        Arc::new(settings),
        model_manager,
    );

    let Some(target_dir) = target_dir else {
        return Err("Usage: batch_importer [--prepared-voice] <audio-directory> or set CORTEX_IMPORT_DIR.".into());
    };

    println!("Importing directory: {}", target_dir.display());

    if prepared_voice {
        let (total, succeeded) = import_prepared_voice_parallel(&pipeline, &db, &db_path, &target_dir)?;
        if total != succeeded {
            return Err(format!("Prepared import incomplete: {succeeded}/{total} WAVs completed").into());
        }
        println!("Completed: Total {total}, Succeeded {succeeded}, Failed 0");
        println!("Batch Importer Finished!");
        return Ok(());
    }

    // import_directory returns Ok(()) even when every file failed or the dir has zero audio files
    // (per-file faults only emit PipelineEvent::Error). Capture the final tally so this binary's EXIT
    // CODE reflects reality — otherwise a cron/CI wrapper pointed at a mistyped/empty dir sees exit 0
    // and believes the import succeeded when it did nothing.
    let outcome = std::cell::Cell::new((0usize, 0usize, 0usize)); // (total, succeeded, failed)

    // RE-RUNNING A DIRECTORY IS A RESUME, NOT A FRESH IMPORT.
    //
    // Halt-on-first-failure means a big import stops partway by design, and a growing folder means
    // the same directory is imported again and again. Both make the re-run the normal case, and a
    // re-run without this is destructive: `AudioFingerprint::new()` starts with an empty map, so the
    // duplicate check cannot see the previous run at all, and every already-imported file is
    // processed a second time and persisted AGAIN under the same `audio_path`. That is the
    // 2026-08-14 shape, where one folder re-import silently doubled 494 already-reviewed clips.
    //
    // Handing the importer the set of paths it already holds turns that into the resume the
    // machinery was written for: finished files are adopted (never re-persisted), and a file left
    // mid-stage — placeholder or empty drafts from an interrupted run — is discarded and redone.
    let already_imported: std::collections::HashSet<String> = db
        .audio_paths_with_segments_under(&target_dir.to_string_lossy())
        .map_err(|e| format!("could not read what is already imported from this directory: {e}"))?
        .into_iter()
        .collect();
    if already_imported.is_empty() {
        println!("Fresh import: this directory has no clips in the library yet.");
    } else {
        println!("Resuming: {} file(s) from this directory are already in the library.", already_imported.len());
    }

    pipeline.import_directory_with_agent_run_id(&target_dir, None, None, Some(&already_imported), |event| {
        use cortex_speech_app_lib::pipeline::PipelineEvent;
        match event {
            PipelineEvent::Progress { current, total, file, status } => {
                println!("Progress: {}/{} - {} - {}", current, total, file, status);
            }
            PipelineEvent::Completed { total, succeeded, failed } => {
                println!("Completed: Total {}, Succeeded {}, Failed {}", total, succeeded, failed);
                outcome.set((total, succeeded, failed));
            }
            PipelineEvent::Error { file, error } => {
                println!("Error in {}: {}", file, error);
            }
            _ => {}
        }
    })?;

    let (total, succeeded, failed) = outcome.get();
    if total == 0 {
        return Err(format!("No audio files found to import in {}", target_dir.display()).into());
    }
    if succeeded == 0 {
        return Err(format!("Import failed: all {failed} file(s) failed").into());
    }

    println!("Batch Importer Finished!");
    Ok(())
}
