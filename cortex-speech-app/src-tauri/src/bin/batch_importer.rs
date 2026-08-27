use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::{AppSettings, AsrModelSize, LlmMode};
use cortex_speech_app_lib::{quality, rehydrate_dedup_index, review_pool, DedupReadiness};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

const STAGING_SENTINEL_NAME: &str = ".cortex-import-staging.json";
const STAGING_PURPOSE: &str = "cortex-batch-import-staging-profile";
const STAGING_TOKEN_ENV: &str = "CORTEX_IMPORT_STAGING_TOKEN";
const SQLITE_DB_NAME: &str = "cortex-speech.db";
const SQLITE_HEADER_LEN: usize = 72;
const SENTINEL_MAX_BYTES: u64 = 8 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportStagingSentinel {
    schema: u32,
    purpose: String,
    profile_token: String,
    sqlite_application_id: u32,
    canonical_profile: String,
    created_at_utc: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MintedImportStagingProfile {
    data_dir: String,
    profile_token: String,
    sqlite_application_id: u32,
    token_environment_variable: &'static str,
}

fn valid_staging_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn staging_application_id(token: &str) -> Result<u32, String> {
    if !valid_staging_token(token) {
        return Err(format!("{STAGING_TOKEN_ENV} is missing or malformed"));
    }
    let prefix = u32::from_str_radix(&token[..8], 16)
        .map_err(|error| format!("cannot derive the staging SQLite identity: {error}"))?;
    Ok((prefix & 0x7fff_ffff).max(1))
}

/// Read SQLite's application_id directly from the immutable main-file header. Opening an unknown
/// database through SQLite, even read-only, may create or update SHM state; a raw header read makes
/// every containment refusal happen before any database/WAL mutation is possible.
fn sqlite_application_id_from_header(db_path: &Path) -> Result<u32, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(db_path)
        .map_err(|error| format!("cannot read the import-staging SQLite identity: {error}"))?;
    let mut header = [0u8; SQLITE_HEADER_LEN];
    file.read_exact(&mut header)
        .map_err(|error| format!("the import-staging SQLite header is missing or truncated: {error}"))?;
    if &header[..16] != b"SQLite format 3\0" {
        return Err("the import-staging database is not a SQLite 3 file".to_string());
    }
    Ok(u32::from_be_bytes([header[68], header[69], header[70], header[71]]))
}

fn read_staging_sentinel(profile: &Path) -> Result<ImportStagingSentinel, String> {
    let path = profile.join(STAGING_SENTINEL_NAME);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "CORTEX_APP_DATA_DIR is not a batch-importer-minted staging profile; live review imports are forbidden ({error})"
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("the import-staging sentinel must be a regular, non-link file".to_string());
    }
    if metadata.len() == 0 || metadata.len() > SENTINEL_MAX_BYTES {
        return Err("the import-staging sentinel has an invalid size".to_string());
    }
    let canonical =
        std::fs::canonicalize(&path).map_err(|error| format!("cannot resolve the import-staging sentinel: {error}"))?;
    if canonical.parent() != Some(profile) {
        return Err("the import-staging sentinel resolves outside its profile".to_string());
    }
    let bytes =
        std::fs::read(&canonical).map_err(|error| format!("cannot read the import-staging sentinel: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid import-staging sentinel: {error}"))
}

fn isolated_import_data_dir(explicit: Option<OsString>, supplied_token: Option<OsString>) -> Result<PathBuf, String> {
    let explicit = explicit.filter(|value| !value.is_empty()).ok_or_else(|| {
        "CORTEX_APP_DATA_DIR is required and must point to a batch-importer-minted staging profile; live review imports are forbidden"
            .to_string()
    })?;
    let explicit_path = PathBuf::from(explicit);
    let explicit_metadata = std::fs::symlink_metadata(&explicit_path)
        .map_err(|error| format!("CORTEX_APP_DATA_DIR must point to an existing minted staging directory: {error}"))?;
    if explicit_metadata.file_type().is_symlink() {
        return Err("CORTEX_APP_DATA_DIR must not be a symlink, junction, or path alias".to_string());
    }
    let selected = std::fs::canonicalize(&explicit_path)
        .map_err(|error| format!("CORTEX_APP_DATA_DIR must resolve to a minted staging directory: {error}"))?;
    if !selected.is_dir() {
        return Err("CORTEX_APP_DATA_DIR must point to an existing minted staging directory".to_string());
    }

    let token = supplied_token
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{STAGING_TOKEN_ENV} is required for a minted import staging profile"))?;
    let expected_application_id = staging_application_id(&token)?;
    let sentinel = read_staging_sentinel(&selected)?;
    if sentinel.schema != 1 || sentinel.purpose != STAGING_PURPOSE {
        return Err("the import-staging sentinel schema or purpose is not supported".to_string());
    }
    if sentinel.profile_token != token {
        return Err("the import-staging sentinel token does not match this importer run".to_string());
    }
    if sentinel.sqlite_application_id != expected_application_id {
        return Err("the import-staging sentinel SQLite identity does not match its token".to_string());
    }
    if sentinel.canonical_profile != selected.to_string_lossy() {
        return Err("the import-staging sentinel is bound to a different canonical profile".to_string());
    }
    if sentinel.created_at_utc.trim().is_empty() {
        return Err("the import-staging sentinel has no creation timestamp".to_string());
    }

    let db_path = selected.join(SQLITE_DB_NAME);
    let db_metadata = std::fs::symlink_metadata(&db_path)
        .map_err(|error| format!("the minted import-staging database is missing: {error}"))?;
    if db_metadata.file_type().is_symlink() || !db_metadata.is_file() {
        return Err("the import-staging database must be a regular, non-link file".to_string());
    }
    let canonical_db = std::fs::canonicalize(&db_path)
        .map_err(|error| format!("cannot resolve the minted import-staging database: {error}"))?;
    if canonical_db.parent() != Some(selected.as_path()) {
        return Err("the import-staging database resolves outside its profile".to_string());
    }
    if sqlite_application_id_from_header(&canonical_db)? != expected_application_id {
        return Err("the SQLite database identity does not match the minted import-staging contract".to_string());
    }
    Ok(selected)
}

/// Minting is deliberately create-new only. It can never attach a staging marker to an existing
/// profile, so a relocated production database cannot be made eligible by a typo or convenience
/// flag. A partially minted directory has no valid sentinel and remains ineligible.
fn mint_import_staging_profile(target: &Path) -> Result<MintedImportStagingProfile, String> {
    let leaf = target
        .file_name()
        .ok_or_else(|| "--init-staging-profile requires a new child directory".to_string())?
        .to_owned();
    if std::fs::symlink_metadata(target).is_ok() {
        return Err("refusing to attach an import-staging identity to an existing path".to_string());
    }
    let parent =
        target.parent().ok_or_else(|| "--init-staging-profile requires a path with an existing parent".to_string())?;
    let parent =
        std::fs::canonicalize(parent).map_err(|error| format!("cannot resolve the staging profile parent: {error}"))?;
    if !parent.is_dir() {
        return Err("the staging profile parent is not a directory".to_string());
    }
    let target = parent.join(leaf);
    std::fs::create_dir(&target).map_err(|error| format!("cannot create the new staging profile: {error}"))?;
    let profile =
        std::fs::canonicalize(&target).map_err(|error| format!("cannot resolve the new staging profile: {error}"))?;
    if profile.parent() != Some(parent.as_path()) {
        return Err("the new staging profile escaped its canonical parent".to_string());
    }

    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let application_id = staging_application_id(&token)?;
    let db_path = profile.join(SQLITE_DB_NAME);
    let db_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&db_path)
        .map_err(|error| format!("cannot create the staging SQLite database: {error}"))?;
    db_file.sync_all().map_err(|error| format!("cannot fsync the new staging SQLite file: {error}"))?;
    drop(db_file);

    {
        let connection = rusqlite::Connection::open(&db_path)
            .map_err(|error| format!("cannot initialize the staging SQLite identity: {error}"))?;
        connection
            .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")
            .map_err(|error| format!("cannot initialize staging SQLite durability: {error}"))?;
        connection
            .pragma_update(None, "application_id", application_id)
            .map_err(|error| format!("cannot write the staging SQLite identity: {error}"))?;
        let observed: u32 = connection
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(|error| format!("cannot verify the staging SQLite identity: {error}"))?;
        if observed != application_id {
            return Err("the staging SQLite identity did not persist".to_string());
        }
    }
    OpenOptions::new()
        .write(true)
        .open(&db_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("cannot fsync the initialized staging SQLite database: {error}"))?;

    let sentinel = ImportStagingSentinel {
        schema: 1,
        purpose: STAGING_PURPOSE.to_string(),
        profile_token: token.clone(),
        sqlite_application_id: application_id,
        canonical_profile: profile.to_string_lossy().to_string(),
        created_at_utc: chrono::Utc::now().to_rfc3339(),
    };
    let mut sentinel_bytes = serde_json::to_vec(&sentinel)
        .map_err(|error| format!("cannot serialize the import-staging sentinel: {error}"))?;
    sentinel_bytes.push(b'\n');
    let sentinel_path = profile.join(STAGING_SENTINEL_NAME);
    let mut sentinel_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&sentinel_path)
        .map_err(|error| format!("cannot create the import-staging sentinel: {error}"))?;
    sentinel_file
        .write_all(&sentinel_bytes)
        .and_then(|_| sentinel_file.flush())
        .and_then(|_| sentinel_file.sync_all())
        .map_err(|error| format!("cannot durably publish the import-staging sentinel: {error}"))?;

    // Self-validate the fully published contract before telling the caller it can be used.
    isolated_import_data_dir(Some(profile.as_os_str().to_owned()), Some(OsString::from(&token)))?;
    Ok(MintedImportStagingProfile {
        data_dir: profile.to_string_lossy().to_string(),
        profile_token: token,
        sqlite_application_id: application_id,
        token_environment_variable: STAGING_TOKEN_ENV,
    })
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

fn prepared_path_alias_key(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

fn prepared_stored_paths_by_alias(
    stored_paths: impl IntoIterator<Item = String>,
    target_dir: &str,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut by_alias = std::collections::BTreeMap::<String, Vec<String>>::new();
    for stored in stored_paths {
        if let Some(walked) = rebase_onto_import_dir(&stored, target_dir) {
            by_alias.entry(prepared_path_alias_key(&walked)).or_default().push(stored);
        }
    }
    for candidates in by_alias.values_mut() {
        candidates.sort();
        candidates.dedup();
    }
    by_alias
}

fn segment_has_human_owned_fields(segment: &cortex_speech_app_lib::db::SpeechSegment) -> bool {
    segment.verified
        || segment.is_gold
        || segment.human_decision.is_some()
        || segment.reviewed_by.is_some()
        || segment.corrected_at.is_some()
        || segment.annotated_transcript.is_some()
        || segment.escalated
        || segment.verdict.as_deref().is_some_and(|verdict| {
            verdict.starts_with("human_")
                || matches!(verdict, "escalated" | "auto_accept" | "jury_accept" | "jury_edit")
        })
}

#[derive(Debug, PartialEq, Eq)]
enum PreparedExistingAction {
    Fresh,
    Reuse(Vec<String>),
    Replace(Vec<String>),
}

/// Decide whether prepared rows are reusable only after binding them to the bytes decoded now.
/// The stored transcript/model is not authority for a file that has been replaced in place.
fn inspect_prepared_existing_file(
    db: &Database,
    file: &Path,
    stored_paths: &[String],
    champion_model_id: &str,
) -> Result<PreparedExistingAction, String> {
    if stored_paths.len() > 1 {
        return Err(format!(
            "PREPARED_PATH_AMBIGUITY: {} maps to multiple stored case/separator aliases {:?}; refusing a nondeterministic survivor",
            file.display(), stored_paths
        ));
    }
    let Some(stored_path) = stored_paths.first() else {
        return Ok(PreparedExistingAction::Fresh);
    };
    let existing_ids = db
        .segment_ids_for_audio_path(stored_path)
        .map_err(|error| format!("cannot inspect existing rows for {}: {error}", file.display()))?;
    if existing_ids.is_empty() {
        return Err(format!(
            "PREPARED_INVENTORY_CHANGED: stored source '{}' disappeared while admitting {}",
            stored_path,
            file.display()
        ));
    }
    let existing = db
        .get_segments_by_ids(&existing_ids)
        .map_err(|error| format!("cannot read existing rows for {}: {error}", file.display()))?;
    if existing.len() != existing_ids.len() {
        return Err(format!(
            "PREPARED_INVENTORY_CHANGED: read only {}/{} rows for {}; refusing partial authority",
            existing.len(),
            existing_ids.len(),
            file.display()
        ));
    }

    let (sample_rate, pcm) = cortex_speech_app_lib::audio::decode_to_pcm(file).map_err(|error| {
        format!("cannot decode current prepared WAV {} for identity proof: {error}", file.display())
    })?;
    let current_content = AudioFingerprint::content_hash(&pcm, sample_rate);
    let mut mismatched = Vec::new();
    for segment_id in &existing_ids {
        let stored_content = db
            .segment_audio_content_hash(segment_id)
            .map_err(|error| format!("cannot read source identity for segment {segment_id}: {error}"))?;
        if stored_content.as_deref() != Some(current_content.as_str()) {
            mismatched.push((segment_id.clone(), stored_content));
        }
    }
    if !mismatched.is_empty() {
        let ownership =
            if existing.iter().any(segment_has_human_owned_fields) { " human-owned rows are present;" } else { "" };
        return Err(format!(
            "SOURCE_IDENTITY_DRIFT: {} no longer decodes to the identity stored for {:?};{ownership} refusing same-path replacement or stale transcript reuse",
            file.display(), mismatched
        ));
    }

    if existing.iter().all(|segment| is_exact_champion_segment(segment, champion_model_id)) {
        return Ok(PreparedExistingAction::Reuse(existing_ids));
    }
    if existing.iter().any(segment_has_human_owned_fields) {
        return Err(format!(
            "HUMAN_SOURCE_AUTHORITY: {} has human-owned rows bound to the current audio; prepared import will not replace them",
            file.display()
        ));
    }
    Ok(PreparedExistingAction::Replace(existing_ids))
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
/// This import-only process has nothing useful to do in a degraded mode: a read fault or incomplete
/// durable identity hard-stops before pipeline construction, decode, journal creation, or publication.
fn rehydrate_dedup_from_library(db: &Database, fingerprint: &AudioFingerprint) -> Result<usize, String> {
    match rehydrate_dedup_index(db, fingerprint) {
        DedupReadiness::Ready { rehydrated_recordings } => {
            let rehydrated = rehydrated_recordings;
            println!("Audio dedup: rehydrated {rehydrated} recording identity/identities from the library.");
            Ok(rehydrated)
        }
        DedupReadiness::Unavailable(reason) => {
            eprintln!("Audio dedup is not authoritative ({reason:?}).");
            Err(cortex_speech_app_lib::DEDUP_INDEX_UNAVAILABLE_MESSAGE.to_string())
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
    let stored_by_walked = prepared_stored_paths_by_alias(
        db.audio_paths_with_segments_under(&target_text)
            .map_err(|error| format!("cannot read what is already imported from {}: {error}", target_dir.display()))?,
        &target_text,
    );

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
        let alias_key = prepared_path_alias_key(&path_text);
        let candidates = stored_by_walked.get(&alias_key).map(Vec::as_slice).unwrap_or(&[]);
        match inspect_prepared_existing_file(db, &file, candidates, &champion_model_id)? {
            PreparedExistingAction::Fresh => pending.push(file),
            PreparedExistingAction::Reuse(_) => {
                db.mark_import_file_done(&job_id, &path_text)
                    .map_err(|error| format!("cannot journal existing file {}: {error}", file.display()))?;
                succeeded += 1;
                println!(
                    "Progress: {succeeded}/{total} - {} - Exact champion row and current audio identity reused",
                    file.file_name().and_then(|name| name.to_str()).unwrap_or("unknown")
                );
            }
            PreparedExistingAction::Replace(existing_ids) => {
                // Only an identity-matching, machine-owned invalid stage can reach replacement.
                db.delete_segments_batch(&existing_ids)
                    .map_err(|error| format!("cannot replace invalid staged rows for {}: {error}", file.display()))?;
                pending.push(file);
            }
        }
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

fn require_complete_import(total: usize, succeeded: usize, failed: usize, target_dir: &Path) -> Result<(), String> {
    if total == 0 {
        return Err(format!("No audio files found to import in {}", target_dir.display()));
    }
    if failed > 0 || succeeded != total {
        return Err(format!(
            "Import incomplete: {succeeded}/{total} file(s) succeeded and {failed} failed. Completed files remain durably committed for resume; fix the failure and re-run {}",
            target_dir.display()
        ));
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    println!("Starting Batch Importer...");

    let cli_args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if cli_args.first().is_some_and(|arg| arg == OsStr::new("--init-staging-profile")) {
        if cli_args.len() != 2 {
            return Err("Usage: batch_importer --init-staging-profile <new-profile-directory>".into());
        }
        let minted = mint_import_staging_profile(Path::new(&cli_args[1]))?;
        println!("{}", serde_json::to_string_pretty(&minted)?);
        println!(
            "Use this profile only for offline import staging. Set CORTEX_APP_DATA_DIR and {} to the values above before importing.",
            STAGING_TOKEN_ENV
        );
        return Ok(());
    }

    let app_data_dir =
        isolated_import_data_dir(std::env::var_os("CORTEX_APP_DATA_DIR"), std::env::var_os(STAGING_TOKEN_ENV))?;

    // Single-instance guard shared with the GUI (same cortex.lock): refuse to run against the live DB
    // while the app — or another importer — is open, so two writers never contend on the WAL DB or the
    // one warm 7B server, and per-process import dedup can't double-import a file. Return the error from
    // main (a recoverable, non-panicking exit) rather than aborting inside processing.
    let _lock = cortex_speech_app_lib::flock::InstanceLock::try_lock(&app_data_dir)
        .map_err(|e| format!("Cannot start batch importer: {e}. Close the Cortex app (or another importer) first."))?;

    let db_path = app_data_dir.join(SQLITE_DB_NAME);

    let db = Database::open_with_retry(&db_path.to_string_lossy())?;
    if let Some(path) = cortex_speech_app_lib::snapshot::initialize_with_required_pre_migration_pin(&db, &app_data_dir)?
    {
        println!("Pre-migration database safety pin: {}", path.display());
    }

    // This binary persists production drafts, so it must share the desktop's champion-only loader;
    // raw `load()` is reserved for explicit offline diagnostic tools.
    let mut settings = AppSettings::load_production(&app_data_dir.join("settings.json"));

    let mut args = cli_args.into_iter();
    let first_arg = args.next();
    let (prepared_voice, target_dir) = match first_arg.as_deref() {
        Some(value) if value == OsStr::new("--prepared-voice") => (true, args.next().map(PathBuf::from)),
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
    rehydrate_dedup_from_library(&db, &fingerprint)?;
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
    let outcome = std::cell::Cell::new(None::<(usize, usize, usize)>); // (total, succeeded, failed)

    // RE-RUNNING A DIRECTORY IS A RESUME, NOT A FRESH IMPORT.
    //
    // Halt-on-first-failure means a big import stops partway by design, and a growing folder means
    // the same directory is imported again and again. Both make the re-run the normal case, and a
    // re-run without this is destructive: `AudioFingerprint::new()` starts with an empty map, so the
    // duplicate check cannot see the previous run at all, and every already-imported file is
    // processed a second time and persisted AGAIN under the same `audio_path`. That is the
    // 2026-08-14 shape, where one folder re-import silently doubled 494 already-reviewed clips.
    //
    // Handing the importer the set of candidate paths lets the pipeline perform a resume. The set is
    // NOT authority: before adoption, the pipeline re-reads every row and requires either durable
    // human truth or an exact current local-champion draft, then binds those rows to the canonical PCM
    // decoded from the current source. Placeholder/blank/wrong-model/cloud/source-drift stages are
    // never skipped merely because a prior journal or path query found them.
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
        println!(
            "Resume candidates: {} file(s) have existing rows; each will be re-verified before adoption.",
            already_imported.len()
        );
    }

    pipeline.import_directory_with_agent_run_id(&target_dir, None, None, Some(&already_imported), None, |event| {
        use cortex_speech_app_lib::pipeline::PipelineEvent;
        match event {
            PipelineEvent::Progress { current, total, file, status } => {
                println!("Progress: {}/{} - {} - {}", current, total, file, status);
            }
            PipelineEvent::Completed { total, succeeded, failed } => {
                println!("Completed: Total {}, Succeeded {}, Failed {}", total, succeeded, failed);
                outcome.set(Some((total, succeeded, failed)));
            }
            PipelineEvent::Error { file, error } => {
                println!("Error in {}: {}", file, error);
            }
            _ => {}
        }
    })?;

    let (total, succeeded, failed) = outcome
        .get()
        .ok_or_else(|| "Import incomplete: the pipeline returned without a terminal completion tally".to_string())?;
    require_complete_import(total, succeeded, failed, &target_dir)?;

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

    fn write_identity_wav(path: &Path, sample: i16) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("create identity WAV");
        for _ in 0..3_200 {
            writer.write_sample(sample).expect("write identity sample");
        }
        writer.finalize().expect("finalize identity WAV");
    }

    fn prepared_identity_fixture(human_owned: bool) -> (tempfile::TempDir, Database, PathBuf, String) {
        let directory = tempfile::tempdir().expect("tempdir");
        let db_path = directory.path().join("prepared-identity.db");
        let db = Database::open_with_retry(&db_path.to_string_lossy()).expect("open database");
        cortex_speech_app_lib::snapshot::initialize_with_required_pre_migration_pin(&db, directory.path())
            .expect("initialize database");
        let wav = directory.path().join("voice.wav");
        write_identity_wav(&wav, 1_000);
        let path_text = wav.to_string_lossy().to_string();
        db.insert_segment(&cortex_speech_app_lib::db::SpeechSegment {
            id: "existing-prepared".into(),
            audio_path: path_text.clone(),
            raw_transcript: "دەنگێکی ڕاستەقینە".into(),
            model_version_id: Some("unknown@pre-registry".into()),
            ..Default::default()
        })
        .expect("insert prepared row");
        let (sample_rate, pcm) = cortex_speech_app_lib::audio::decode_to_pcm(&wav).expect("decode original WAV");
        db.set_audio_identity(&path_text, &AudioFingerprint::identify(&pcm, sample_rate))
            .expect("bind original identity");
        if human_owned {
            db.connection()
                .execute(
                    "UPDATE speech_segments
                        SET verified = 1, human_decision = 'accept', reviewed_by = 'owner'
                      WHERE id = 'existing-prepared'",
                    [],
                )
                .expect("mark fixture human-owned");
        }
        (directory, db, wav, path_text)
    }

    #[test]
    fn prepared_reuse_refuses_same_path_pcm_replacement() {
        let (_directory, db, wav, path_text) = prepared_identity_fixture(false);
        let prior_hash = db.segment_audio_content_hash("existing-prepared").unwrap().unwrap();
        write_identity_wav(&wav, 9_000);

        let error = inspect_prepared_existing_file(&db, &wav, std::slice::from_ref(&path_text), "unknown@pre-registry")
            .unwrap_err();
        assert!(error.contains("SOURCE_IDENTITY_DRIFT"), "unexpected refusal: {error}");
        assert_eq!(
            db.segment_audio_content_hash("existing-prepared").unwrap().as_deref(),
            Some(prior_hash.as_str()),
            "replacement bytes must not rebind the older transcript"
        );
        assert_eq!(db.segment_count().unwrap(), 1);
    }

    #[test]
    fn prepared_reuse_refuses_human_owned_source_drift() {
        let (_directory, db, wav, path_text) = prepared_identity_fixture(true);
        let prior_hash = db.segment_audio_content_hash("existing-prepared").unwrap().unwrap();
        write_identity_wav(&wav, -7_000);

        let error = inspect_prepared_existing_file(&db, &wav, std::slice::from_ref(&path_text), "unknown@pre-registry")
            .unwrap_err();
        assert!(error.contains("SOURCE_IDENTITY_DRIFT"), "unexpected refusal: {error}");
        assert!(error.contains("human-owned"), "the refusal must identify protected human authority: {error}");
        let retained = db.get_segment_by_id("existing-prepared").unwrap().unwrap();
        assert!(retained.verified);
        assert_eq!(retained.human_decision.as_deref(), Some("accept"));
        assert_eq!(db.segment_audio_content_hash("existing-prepared").unwrap().as_deref(), Some(prior_hash.as_str()));
    }

    #[test]
    fn prepared_case_separator_alias_ambiguity_fails_closed() {
        let target = r"D:\Voice";
        let walked = r"D:\Voice\Clip.wav";
        let indexed = prepared_stored_paths_by_alias(
            [r"d:/voice/clip.wav".to_string(), r"D:\VOICE\CLIP.WAV".to_string()],
            target,
        );
        let candidates = indexed.get(&prepared_path_alias_key(walked)).expect("logical path indexed");
        assert_eq!(candidates.len(), 2, "every alias candidate must survive indexing");

        let db = Database::open(":memory:").unwrap();
        let error = inspect_prepared_existing_file(&db, Path::new(walked), candidates, "champion-v1").unwrap_err();
        assert!(error.contains("PREPARED_PATH_AMBIGUITY"), "unexpected refusal: {error}");
        assert!(error.contains("multiple stored case/separator aliases"));
    }

    #[test]
    fn prepared_unchanged_replay_is_idempotently_reused() {
        let (_directory, db, wav, path_text) = prepared_identity_fixture(false);
        let candidates = vec![path_text];
        for _ in 0..2 {
            assert_eq!(
                inspect_prepared_existing_file(&db, &wav, &candidates, "unknown@pre-registry").unwrap(),
                PreparedExistingAction::Reuse(vec!["existing-prepared".into()])
            );
        }
        assert_eq!(db.segment_count().unwrap(), 1, "replay must not add or replace a row");
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
            cortex_speech_app_lib::fingerprint::AudioIdentity { spectral: 0xDEAD_BEEF, content: "a".repeat(64) };
        db.set_audio_identity(&segment.audio_path, &identity).expect("store identity");

        // A fresh importer process starts with an EMPTY map — that is the whole defect.
        let fingerprint = AudioFingerprint::new();
        assert_eq!(fingerprint.count(), 0);
        assert_eq!(rehydrate_dedup_from_library(&db, &fingerprint).unwrap(), 1);
        // Offered again under a different file name — the cross-run duplicate this lane could not see.
        assert!(
            fingerprint
                .check_and_register_identity(&identity, Some(Path::new("d:/voice/copy_of_lamo_000056.wav")))
                .is_err(),
            "a recording already in the library must be refused on a later run"
        );
    }

    #[test]
    fn headless_importer_hard_stops_when_the_identity_inventory_cannot_be_read() {
        // No schema makes the inventory SELECT fail deterministically. The binary's only safe mode is
        // to return the same stable admission error as the desktop before constructing its pipeline.
        let db = Database::open(":memory:").unwrap();
        let fingerprint = AudioFingerprint::new();
        let error = rehydrate_dedup_from_library(&db, &fingerprint).unwrap_err();
        assert_eq!(error, cortex_speech_app_lib::DEDUP_INDEX_UNAVAILABLE_MESSAGE);
        assert_eq!(fingerprint.count(), 0);
    }

    #[test]
    fn headless_importer_hard_stops_on_an_active_unhashed_recording() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open_with_retry(&dir.path().join("cortex-speech.db").to_string_lossy()).expect("open db");
        cortex_speech_app_lib::snapshot::initialize_with_required_pre_migration_pin(&db, dir.path())
            .expect("initialize schema through the production admission guard");
        db.insert_segment(&cortex_speech_app_lib::db::SpeechSegment {
            id: "unhashed".into(),
            audio_path: "d:/voice/unhashed.wav".into(),
            raw_transcript: "دەنگ".into(),
            ..Default::default()
        })
        .expect("insert legacy fixture");

        let fingerprint = AudioFingerprint::new();
        let error = rehydrate_dedup_from_library(&db, &fingerprint).unwrap_err();
        assert_eq!(error, cortex_speech_app_lib::DEDUP_INDEX_UNAVAILABLE_MESSAGE);
        assert_eq!(fingerprint.count(), 0);
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
    fn valid_test_token() -> String {
        "0123456789abcdef".repeat(4)
    }

    fn profile_database_bytes(profile: &Path) -> Vec<(String, Vec<u8>)> {
        [SQLITE_DB_NAME.to_string(), format!("{SQLITE_DB_NAME}-wal"), format!("{SQLITE_DB_NAME}-shm")]
            .into_iter()
            .filter_map(|name| std::fs::read(profile.join(&name)).ok().map(|bytes| (name, bytes)))
            .collect()
    }

    /// A caller-supplied relocated owner profile is exactly what the old `%APPDATA%` comparison
    /// admitted. Missing the importer-minted identity must now refuse before even a SQLite read-only
    /// connection can create SHM state; all database-family bytes therefore remain identical.
    #[test]
    fn supplied_relocated_live_profile_is_refused_without_touching_database() {
        let root = tempfile::tempdir().expect("tempdir");
        let relocated_live = root.path().join("relocated-owner-live-profile");
        std::fs::create_dir(&relocated_live).expect("live-like directory");
        {
            let connection = rusqlite::Connection::open(relocated_live.join(SQLITE_DB_NAME)).expect("live-like db");
            connection
                .execute_batch("CREATE TABLE human_decisions(id TEXT PRIMARY KEY, transcript TEXT NOT NULL); INSERT INTO human_decisions VALUES('decision-1', 'owner truth');")
                .expect("live-like truth");
        }
        let before = profile_database_bytes(&relocated_live);

        let error = isolated_import_data_dir(
            Some(relocated_live.as_os_str().to_owned()),
            Some(OsString::from(valid_test_token())),
        )
        .unwrap_err();
        assert!(error.contains("not a batch-importer-minted staging profile"), "unexpected refusal: {error}");
        assert!(error.contains("live review imports are forbidden"), "unexpected refusal: {error}");
        assert_eq!(profile_database_bytes(&relocated_live), before, "a refused live profile must be byte-identical");
    }

    #[test]
    fn only_the_exact_minted_staging_identity_is_admitted() {
        let root = tempfile::tempdir().expect("tempdir");
        let staging = root.path().join("offline-import-staging");
        let minted = mint_import_staging_profile(&staging).expect("mint staging profile");
        assert_eq!(
            isolated_import_data_dir(Some(staging.as_os_str().to_owned()), Some(OsString::from(&minted.profile_token)))
                .expect("minted profile admitted"),
            std::fs::canonicalize(&staging).unwrap()
        );

        let missing_token = isolated_import_data_dir(Some(staging.as_os_str().to_owned()), None).unwrap_err();
        assert!(missing_token.contains(STAGING_TOKEN_ENV));
        let wrong_token =
            isolated_import_data_dir(Some(staging.as_os_str().to_owned()), Some(OsString::from(valid_test_token())))
                .unwrap_err();
        assert!(wrong_token.contains("token does not match"));
        assert!(mint_import_staging_profile(&staging).unwrap_err().contains("existing path"));

        // Copying both marker-bearing files still cannot bless a different (possibly live) path:
        // the signed intent is bound to the canonical directory identity as well as the DB header.
        let relocated = root.path().join("copied-profile");
        std::fs::create_dir(&relocated).expect("relocated directory");
        std::fs::copy(staging.join(SQLITE_DB_NAME), relocated.join(SQLITE_DB_NAME)).expect("copy db");
        std::fs::copy(staging.join(STAGING_SENTINEL_NAME), relocated.join(STAGING_SENTINEL_NAME))
            .expect("copy sentinel");
        let before = profile_database_bytes(&relocated);
        let error = isolated_import_data_dir(
            Some(relocated.as_os_str().to_owned()),
            Some(OsString::from(&minted.profile_token)),
        )
        .unwrap_err();
        assert!(error.contains("bound to a different canonical profile"), "unexpected refusal: {error}");
        assert_eq!(profile_database_bytes(&relocated), before, "a copied contract must be refused without writes");
    }

    #[test]
    fn any_partial_or_inconsistent_import_tally_is_an_incomplete_exit() {
        let target = Path::new("D:/offline-staging/audio");
        assert!(require_complete_import(3, 3, 0, target).is_ok());

        for (total, succeeded, failed) in [(3, 2, 1), (3, 2, 0), (3, 3, 1), (3, 0, 3)] {
            let error = require_complete_import(total, succeeded, failed, target).unwrap_err();
            assert!(error.starts_with("Import incomplete:"), "unexpected partial-import verdict: {error}");
            assert!(error.contains("remain durably committed for resume"));
        }
        assert!(require_complete_import(0, 0, 0, target).unwrap_err().contains("No audio files"));
    }
}
