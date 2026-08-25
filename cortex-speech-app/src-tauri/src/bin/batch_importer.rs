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

fn isolated_import_data_dir(
    explicit: Option<std::ffi::OsString>,
    appdata: Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    let explicit = explicit.filter(|value| !value.is_empty()).ok_or_else(|| {
        "CORTEX_APP_DATA_DIR is required and must point to an existing isolated staging profile; live review imports are forbidden"
            .to_string()
    })?;
    let selected = std::fs::canonicalize(PathBuf::from(explicit)).map_err(|error| {
        format!("CORTEX_APP_DATA_DIR must point to an existing isolated staging directory: {error}")
    })?;
    if !selected.is_dir() {
        return Err("CORTEX_APP_DATA_DIR must point to an existing isolated staging directory".to_string());
    }

    // The importer is an offline staging tool. It must never infer or overlap the mutable profile
    // served to reviewers, even if the GUI is currently closed. Canonical paths close case, `..`,
    // symlink, and junction aliases; rejecting ancestor overlap also keeps staging databases out of
    // the live snapshot tree and keeps the live profile out of a staging tree.
    let appdata = appdata
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "APPDATA is required to identify and protect the live review profile".to_string())?;
    let live = std::fs::canonicalize(PathBuf::from(appdata).join("cortex-speech"))
        .map_err(|error| format!("cannot resolve the live review profile for import isolation: {error}"))?;
    if selected == live || selected.starts_with(&live) || live.starts_with(&selected) {
        return Err(format!(
            "live review imports are forbidden: CORTEX_APP_DATA_DIR must be separate from {}",
            live.display()
        ));
    }
    Ok(selected)
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

/// A BLANK draft is not champion evidence: `is_placeholder_transcript` only recognises the marker
/// strings, so an empty `raw_transcript` passed this gate and was reused (or accepted on the way out)
/// as finished champion work. That is the twice-fixed blank-transcript class — a transcribe path that
/// returns "" as success — and here it would silently publish an untranscribed clip.
fn is_exact_champion_segment(segment: &cortex_speech_app_lib::db::SpeechSegment, champion_model_id: &str) -> bool {
    segment.model_version_id.as_deref() == Some(champion_model_id)
        && !segment.cloud_call
        && !segment.raw_transcript.trim().is_empty()
        && !quality::is_placeholder_transcript(&segment.raw_transcript)
}

/// Re-base one library path onto the directory text THIS run walks; `None` when it is not a file
/// under that directory.
///
/// `Database::audio_paths_with_segments_under` matches case- and separator-insensitively, but every
/// per-file resume check downstream is an EXACT string compare against the path the walker builds
/// (`segment_ids_for_audio_path` in SQL, `resume_completed.contains` in the pipeline). So a re-run
/// typed `D:\Voice` against a library holding `d:/voice/...` printed "Resuming: N file(s) ... already
/// in the library" and then imported all N a SECOND time. Canon puts the duplicate-content baseline
/// at 0, and any duplicate from now on is a RED sweep.
fn rebase_onto_import_dir(stored: &str, target_dir: &str) -> Option<String> {
    let key = |c: u8| if c == b'/' { b'\\' } else { c.to_ascii_lowercase() };
    let head = stored.get(..target_dir.len())?;
    if !head.bytes().zip(target_dir.bytes()).all(|(a, b)| key(a) == key(b)) {
        return None;
    }
    let rest = &stored[target_dir.len()..];
    let tail = match rest.strip_prefix(['/', '\\']) {
        Some(tail) => tail,
        // No separator at the split point means a SIBLING like `voice2\a.wav`, not a file under
        // `voice` — the same over-match the prefix query makes. Adopting it would skip a real import.
        None if target_dir.ends_with(['/', '\\']) => rest,
        None => return None,
    };
    if tail.is_empty() {
        return None;
    }
    // Joined the way the directory walkers build their paths, so the result is byte-identical to the
    // string this run will produce for that file.
    Some(
        Path::new(target_dir)
            .join(tail.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR))
            .to_string_lossy()
            .to_string(),
    )
}

/// Rehydrate the cross-run content-dedup map from the library, exactly as the desktop does at startup
/// (`lib.rs`). `AudioFingerprint::new()` starts EMPTY, so without this the headless importer — the
/// owner's primary import lane — had NO cross-run duplicate detection at all: a recording already in
/// the library, offered again under another name, was admitted silently.
///
/// Best-effort like the desktop's: a failed read costs this run cross-run dedup and says so loudly
/// rather than blocking the import. Rows predating v51 have no content hash and can never reject an
/// import, so their count is reported separately instead of implied by silence.
fn rehydrate_dedup_from_library(db: &Database, fingerprint: &AudioFingerprint) -> usize {
    match db.load_audio_identities() {
        Ok(known) => {
            let unhashed = known.iter().filter(|row| row.content.is_none()).count();
            let rehydrated = fingerprint.rehydrate(known);
            println!("Audio dedup: rehydrated {rehydrated} recording identity/identities from the library.");
            if unhashed > 0 {
                println!(
                    "Audio dedup: {unhashed} recording(s) have no content hash and can never reject an import — run `backfill_fingerprints --apply`."
                );
            }
            rehydrated
        }
        Err(error) => {
            println!("Audio dedup: could not rehydrate identities ({error}) — within-run dedup only.");
            0
        }
    }
}

/// Import final, one-character WAVs two files at a time so the two warm 7B workers receive genuinely
/// concurrent work. Each worker owns its SQLite connection; all durable journal bookkeeping stays on
/// the coordinator connection. A wave is joined before its results are accepted, and any empty or
/// non-champion result is removed and halts the run. A prepared WAV may still exceed the app's maximum
/// review duration, in which case multiple source-span segments are the correct lossless result.
fn import_prepared_voice_parallel(
    pipeline: &ProcessingPipeline,
    db: &Database,
    db_path: &Path,
    target_dir: &Path,
) -> Result<(usize, usize), String> {
    let files = collect_prepared_wavs(target_dir)?;
    let total = files.len();
    let champion_model_id = review_pool::current_champion_7b_model_id(db)?;
    let target_text = target_dir.to_string_lossy().to_string();
    let job_id = db
        .begin_import_job(&target_text, total)
        .map_err(|error| format!("cannot create prepared import journal: {error}"))?;

    // The library may hold this directory under a different case or separator than the one typed on
    // this run's command line, and `segment_ids_for_audio_path` matches EXACTLY. Without this map the
    // re-run finds nothing for every file and imports the whole directory a second time.
    let mut stored_by_walked: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for stored in db
        .audio_paths_with_segments_under(&target_text)
        .map_err(|error| format!("cannot read what is already imported from {}: {error}", target_dir.display()))?
    {
        if let Some(walked) = rebase_onto_import_dir(&stored, &target_text) {
            stored_by_walked.insert(walked, stored);
        }
    }

    // ONE number for two readers. The process-wide champion gate re-reads CORTEX_7B_CONCURRENCY on
    // every acquire and defaults to a SINGLE permit, so with the variable unset the second prepared
    // worker just blocked on the gate while this line claimed both warm GPU workers were busy. Pin the
    // variable to the number this mode actually runs, before any champion call, so the gate admits
    // exactly as many requests as there are workers and the printed limit is the real one.
    let file_concurrency = std::env::var("CORTEX_7B_CONCURRENCY")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| (1..=2).contains(value))
        .unwrap_or(2);
    std::env::set_var("CORTEX_7B_CONCURRENCY", file_concurrency.to_string());
    println!(
        "Prepared-file concurrency: {file_concurrency} (one file per warm GPU worker; champion gate pinned to {file_concurrency} permit(s))."
    );

    let mut succeeded = 0usize;
    let mut pending = Vec::new();
    for file in files {
        let path_text = file.to_string_lossy().to_string();
        let stored_path = stored_by_walked.get(&path_text).unwrap_or(&path_text);
        let existing_ids = db
            .segment_ids_for_audio_path(stored_path)
            .map_err(|error| format!("cannot inspect existing rows for {}: {error}", file.display()))?;
        if existing_ids.is_empty() {
            pending.push(file);
            continue;
        }
        let existing = db
            .get_segments_by_ids(&existing_ids)
            .map_err(|error| format!("cannot read existing rows for {}: {error}", file.display()))?;
        if existing.len() == existing_ids.len()
            && !existing.is_empty()
            && existing.iter().all(|segment| is_exact_champion_segment(segment, &champion_model_id))
        {
            db.mark_import_file_done(&job_id, &path_text)
                .map_err(|error| format!("cannot journal existing file {}: {error}", file.display()))?;
            succeeded += 1;
            println!(
                "Progress: {succeeded}/{total} - {} - Exact champion row reused",
                file.file_name().and_then(|name| name.to_str()).unwrap_or("unknown")
            );
            continue;
        }

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
            let valid = !segments.is_empty()
                && segments.iter().all(|segment| is_exact_champion_segment(segment, &champion_model_id));
            if !valid {
                let ids: Vec<String> = segments.iter().map(|segment| segment.id.clone()).collect();
                if let Err(error) = db.delete_segments_batch(&ids) {
                    return Err(format!(
                        "prepared import produced invalid rows for {} and rollback failed: {error}",
                        file.display()
                    ));
                }
                return Err(format!(
                    "prepared import HALTED: {} produced {} row(s), but one or more local {champion_model_id} rows are required",
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    println!("Starting Batch Importer...");

    let app_data_dir = isolated_import_data_dir(std::env::var_os("CORTEX_APP_DATA_DIR"), std::env::var_os("APPDATA"))?;

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
    // Before ANY import work, in BOTH modes — the map must already know every recording the library
    // holds by the time the first file is offered to it.
    rehydrate_dedup_from_library(&db, &fingerprint);
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
    //
    // The prefix query matches case/separator-insensitively, but the pipeline consults this set with
    // an EXACT compare against the path it walks, so a re-run typed in another case printed
    // "Resuming: N" and then re-imported all N anyway. Re-base each stored path to the string the
    // walker will build for it this run.
    let target_text = target_dir.to_string_lossy().to_string();
    let already_imported: std::collections::HashSet<String> = db
        .audio_paths_with_segments_under(&target_text)
        .map_err(|e| format!("could not read what is already imported from this directory: {e}"))?
        .into_iter()
        .map(|stored| rebase_onto_import_dir(&stored, &target_text).unwrap_or(stored))
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

    /// A blank draft is not evidence of champion work. `is_placeholder_transcript` only knows the
    /// marker strings, so "" used to pass and publish an untranscribed clip as finished.
    #[test]
    fn prepared_reuse_rejects_a_blank_transcript_as_champion_evidence() {
        let mut segment = cortex_speech_app_lib::db::SpeechSegment {
            raw_transcript: String::new(),
            model_version_id: Some("champion-v1".into()),
            ..Default::default()
        };
        assert!(!is_exact_champion_segment(&segment, "champion-v1"), "an empty transcript is not champion work");
        segment.raw_transcript = "   \n\t ".into();
        assert!(!is_exact_champion_segment(&segment, "champion-v1"), "whitespace-only is still empty");
    }

    /// A re-run typed with a different case or separator must resolve to the SAME per-file key the
    /// prefix query already matched, or the importer prints "Resuming: N" and re-imports all N —
    /// duplicate content, whose canon baseline is 0.
    #[test]
    fn resume_keys_survive_a_differently_typed_directory() {
        let sep = std::path::MAIN_SEPARATOR;
        let typed = format!("D:{sep}Voice");
        let walked = format!("{typed}{sep}lamo_000056.wav");

        assert_eq!(rebase_onto_import_dir("d:/voice/lamo_000056.wav", &typed).as_deref(), Some(walked.as_str()));
        // Idempotent: a path already in this run's spelling rebases to itself.
        assert_eq!(rebase_onto_import_dir(&walked, &typed).as_deref(), Some(walked.as_str()));
        // A trailing separator on the typed directory is not an off-by-one.
        assert_eq!(
            rebase_onto_import_dir("d:/voice/lamo_000056.wav", &format!("{typed}{sep}")).as_deref(),
            Some(walked.as_str())
        );
        // Nested files keep their sub-path, spelled with this platform's separator.
        assert_eq!(rebase_onto_import_dir("d:/voice/ep01/a.wav", &typed), Some(format!("{typed}{sep}ep01{sep}a.wav")));
        // A SIBLING directory sharing the prefix is not under it: adopting it would skip a real import.
        assert_eq!(rebase_onto_import_dir("d:/voice2/lamo_000056.wav", &typed), None);
        assert_eq!(rebase_onto_import_dir("e:/elsewhere/lamo_000056.wav", &typed), None);
        // The directory itself is not a file under itself.
        assert_eq!(rebase_onto_import_dir("d:/voice", &typed), None);
        assert_eq!(rebase_onto_import_dir("d:/voice/", &typed), None);
    }

    /// The headless importer is the owner's primary import lane; it must start knowing every
    /// recording the library already holds, exactly like the desktop does at startup.
    #[test]
    fn library_identities_rehydrate_before_any_import() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open_with_retry(&dir.path().join("cortex-speech.db").to_string_lossy()).expect("open db");
        cortex_speech_app_lib::snapshot::initialize_with_required_pre_migration_pin(&db, dir.path())
            .expect("initialize schema through the production admission guard");
        let segment = cortex_speech_app_lib::db::SpeechSegment {
            id: "seg-1".into(),
            audio_path: "d:/voice/lamo_000056.wav".into(),
            raw_transcript: "دەنگ".into(),
            ..Default::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        let identity =
            cortex_speech_app_lib::fingerprint::AudioIdentity { spectral: 0xDEAD_BEEF, content: "abc123".into() };
        db.set_audio_identity(&segment.audio_path, &identity).expect("store identity");

        // A fresh importer process starts with an EMPTY map — that is the whole defect.
        let fingerprint = AudioFingerprint::new();
        assert_eq!(fingerprint.count(), 0);
        assert_eq!(rehydrate_dedup_from_library(&db, &fingerprint), 1);
        // Offered again under a different file name — the cross-run duplicate this lane could not see.
        assert!(
            fingerprint
                .check_and_register_identity(&identity, Some(Path::new("d:/voice/copy_of_lamo_000056.wav")))
                .is_err(),
            "a recording already in the library must be refused on a later run"
        );
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
    #[test]
    fn production_import_requires_an_explicit_nonoverlapping_staging_profile() {
        let root = tempfile::tempdir().expect("tempdir");
        let appdata = root.path().join("appdata");
        let live = appdata.join("cortex-speech");
        let live_child = live.join("staging");
        let isolated = root.path().join("isolated-import");
        std::fs::create_dir_all(&live_child).expect("live fixture");
        std::fs::create_dir_all(&isolated).expect("isolated fixture");
        let appdata_value = Some(appdata.as_os_str().to_owned());

        let absent = isolated_import_data_dir(None, appdata_value.clone()).unwrap_err();
        assert!(absent.contains("CORTEX_APP_DATA_DIR is required"));
        let exact_live =
            isolated_import_data_dir(Some(live.as_os_str().to_owned()), appdata_value.clone()).unwrap_err();
        assert!(exact_live.contains("live review imports are forbidden"));
        let live_descendant =
            isolated_import_data_dir(Some(live_child.as_os_str().to_owned()), appdata_value.clone()).unwrap_err();
        assert!(live_descendant.contains("live review imports are forbidden"));
        let live_ancestor =
            isolated_import_data_dir(Some(appdata.as_os_str().to_owned()), appdata_value.clone()).unwrap_err();
        assert!(live_ancestor.contains("live review imports are forbidden"));
        let missing = isolated_import_data_dir(
            Some(root.path().join("typo-does-not-exist").as_os_str().to_owned()),
            appdata_value.clone(),
        )
        .unwrap_err();
        assert!(missing.contains("existing isolated staging directory"));

        assert_eq!(
            isolated_import_data_dir(Some(isolated.as_os_str().to_owned()), appdata_value).unwrap(),
            std::fs::canonicalize(isolated).unwrap()
        );
    }
}
