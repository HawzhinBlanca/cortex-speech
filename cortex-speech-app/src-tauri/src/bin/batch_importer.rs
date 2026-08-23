use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::{AppSettings, AsrModelSize, LlmMode};
use std::path::PathBuf;
use std::sync::Arc;

fn app_data_dir() -> PathBuf {
    std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|path| PathBuf::from(path).join("cortex-speech")))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("cortex-speech"))
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
