//! Offline operator tool for the immutable flexible review pool.
//!
//! `inventory` proves that every WAV in each named prepared directory is present in SQLite with a
//! usable OmniASR-7B draft. `activate` repeats the same proof, then atomically binds those exact
//! segments to voice characters. Quarantine/reject directories are never discovered implicitly: the
//! operator names the exact final `wavs` directories.

use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::review_pool::{self, PoolMemberInput};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct VoiceSpec {
    name: String,
    directory: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceInventory {
    voice_name: String,
    directory: String,
    disk_wavs: usize,
    matched_files: usize,
    matched_segments: usize,
    usable_7b_segments: usize,
    missing_files: Vec<String>,
    invalid_segments: Vec<String>,
}

fn voice_inventory_ready(report: &VoiceInventory) -> bool {
    // A long prepared WAV is intentionally split into multiple bounded review clips. Completeness is
    // therefore every disk WAV matched at least once and every resulting segment exact/usable—not the
    // false assumption that WAV count must equal segment count.
    report.disk_wavs == report.matched_files
        && report.matched_segments >= report.disk_wavs
        && report.matched_segments == report.usable_7b_segments
        && report.missing_files.is_empty()
        && report.invalid_segments.is_empty()
}

fn usage() -> &'static str {
    "Usage:\n  pool_admin inventory --db <cortex-speech.db> --voice <Name=final-wavs-dir> [--voice ...]\n  pool_admin activate --db <cortex-speech.db> --voice <Name=final-wavs-dir> [--voice ...] [--pool-id <uuid>]\n  pool_admin status --db <cortex-speech.db>"
}

fn value_after(args: &[String], flag: &str) -> Result<String, String> {
    let position = args.iter().position(|arg| arg == flag).ok_or_else(|| format!("missing {flag}"))?;
    args.get(position + 1).cloned().ok_or_else(|| format!("missing value after {flag}"))
}

fn voice_specs(args: &[String]) -> Result<Vec<VoiceSpec>, String> {
    let mut specs = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--voice" {
            let raw = args.get(index + 1).ok_or_else(|| "missing value after --voice".to_string())?;
            let (name, directory) =
                raw.split_once('=').ok_or_else(|| format!("voice must be Name=directory, got {raw:?}"))?;
            let name = name.trim();
            let directory = PathBuf::from(directory.trim());
            if name.is_empty() || !directory.is_dir() {
                return Err(format!("voice {raw:?} has an empty name or missing directory"));
            }
            specs.push(VoiceSpec { name: name.to_string(), directory });
            index += 2;
        } else {
            index += 1;
        }
    }
    if specs.is_empty() {
        return Err("at least one --voice Name=directory is required".to_string());
    }
    let mut directories = HashSet::new();
    for spec in &specs {
        let directory = normalized_path(&spec.directory);
        if !directories.insert(directory) {
            return Err(format!("prepared directory {} is specified more than once", spec.directory.display()));
        }
    }
    Ok(specs)
}

fn normalized_path(path: &Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    absolute.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_ascii_lowercase()
}

fn collect_wavs(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![directory.to_path_buf()];
    let mut wavs = Vec::new();
    while let Some(current) = pending.pop() {
        let entries = std::fs::read_dir(&current)
            .map_err(|error| format!("cannot read prepared directory {}: {error}", current.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("cannot read entry under {}: {error}", current.display()))?;
            let kind =
                entry.file_type().map_err(|error| format!("cannot inspect {}: {error}", entry.path().display()))?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && entry.path().extension().is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
            {
                wavs.push(entry.path());
            }
        }
    }
    wavs.sort_unstable();
    Ok(wavs)
}

fn inventory(db: &Database, specs: &[VoiceSpec]) -> Result<(Vec<VoiceInventory>, Vec<PoolMemberInput>), String> {
    let champion_model_id = review_pool::current_champion_7b_model_id(db)?;
    let mut by_path: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    let mut statement = db
        .connection()
        .prepare("SELECT id, audio_path, raw_transcript, COALESCE(model_version_id, '') FROM speech_segments")
        .map_err(|error| format!("library inventory cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?))
        })
        .map_err(|error| format!("library inventory cannot be read: {error}"))?;
    for row in rows {
        let (id, audio_path, raw_transcript, model_id) =
            row.map_err(|error| format!("library inventory row is unreadable: {error}"))?;
        by_path.entry(normalized_path(Path::new(&audio_path))).or_default().push((id, raw_transcript, model_id));
    }

    let mut reports = Vec::new();
    let mut members = Vec::new();
    let mut assigned_segments: HashMap<String, String> = HashMap::new();
    for spec in specs {
        let wavs = collect_wavs(&spec.directory)?;
        if wavs.is_empty() {
            return Err(format!("{} contains no WAV files", spec.directory.display()));
        }
        let mut matched_files = 0;
        let mut matched_segments = 0;
        let mut usable_7b_segments = 0;
        let mut missing_files = Vec::new();
        let mut invalid_segments = Vec::new();
        for wav in &wavs {
            let key = normalized_path(wav);
            let Some(segments) = by_path.get(&key) else {
                missing_files.push(wav.to_string_lossy().to_string());
                continue;
            };
            matched_files += 1;
            matched_segments += segments.len();
            for (segment_id, raw_transcript, model_id) in segments {
                let draft = raw_transcript.trim();
                let usable = !draft.is_empty()
                    && !(draft.starts_with('[') && draft.ends_with(']'))
                    && model_id == &champion_model_id;
                if !usable {
                    invalid_segments.push(format!("{segment_id}:{model_id}"));
                    continue;
                }
                usable_7b_segments += 1;
                if let Some(existing) = assigned_segments.insert(segment_id.clone(), spec.name.clone()) {
                    if !existing.eq_ignore_ascii_case(&spec.name) {
                        return Err(format!(
                            "segment {segment_id} appears in both voice {existing} and voice {}",
                            spec.name
                        ));
                    }
                }
                members.push(PoolMemberInput { segment_id: segment_id.clone(), voice_name: spec.name.clone() });
            }
        }
        reports.push(VoiceInventory {
            voice_name: spec.name.clone(),
            directory: std::fs::canonicalize(&spec.directory)
                .unwrap_or_else(|_| spec.directory.clone())
                .to_string_lossy()
                .to_string(),
            disk_wavs: wavs.len(),
            matched_files,
            matched_segments,
            usable_7b_segments,
            missing_files,
            invalid_segments,
        });
    }
    members.sort_unstable_by(|left, right| left.segment_id.cmp(&right.segment_id));
    members.dedup_by(|left, right| left.segment_id == right.segment_id);
    Ok((reports, members))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).ok_or_else(|| usage().to_string())?;
    let db_path = PathBuf::from(value_after(&args, "--db")?);
    if !db_path.is_file() {
        return Err(format!("database does not exist: {}", db_path.display()).into());
    }
    let _instance_lock = if command == "activate" {
        let data_dir =
            db_path.parent().ok_or_else(|| format!("database has no parent data directory: {}", db_path.display()))?;
        Some(
            cortex_speech_app_lib::flock::InstanceLock::try_lock(data_dir)
                .map_err(|error| format!("activation requires Cortex and every writer to be stopped: {error}"))?,
        )
    } else {
        None
    };
    let db = Database::open_with_retry(&db_path.to_string_lossy())?;
    let schema_version = cortex_speech_app_lib::migrations::validate_applied_history(db.connection())?;
    if schema_version != cortex_speech_app_lib::migrations::max_supported_version() {
        return Err(format!(
            "database schema is {schema_version}; this tool requires {}",
            cortex_speech_app_lib::migrations::max_supported_version()
        )
        .into());
    }

    match command {
        "status" => {
            let pool = review_pool::load(&db)?;
            let output = match pool {
                Some(pool) => serde_json::json!({
                    "active": true,
                    "poolId": pool.pool_id,
                    "focusSegmentCount": pool.focus_segment_count,
                    "focusSha256": pool.focus_sha256,
                    "championModelVersionId": pool.champion_model_version_id,
                    "championDeploymentSha256": pool.champion_deployment_sha256,
                    "coverageByVoice": review_pool::coverage_by_voice(&db)?,
                }),
                None => serde_json::json!({ "active": false }),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        "inventory" | "activate" => {
            let specs = voice_specs(&args)?;
            let (reports, members) = inventory(&db, &specs)?;
            let ready = reports.iter().all(voice_inventory_ready);
            if command == "inventory" {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ready": ready,
                        "voices": reports,
                        "poolSegments": members.len(),
                    }))?
                );
                if !ready {
                    return Err("inventory is incomplete; activation would be refused".into());
                }
            } else {
                if !ready {
                    println!("{}", serde_json::to_string_pretty(&reports)?);
                    return Err("review pool activation refused: prepared WAV inventory is incomplete or not 7B".into());
                }
                let existing = review_pool::load(&db)?;
                let pool_id = match existing.as_ref() {
                    Some(pool) => pool.pool_id.clone(),
                    None => args
                        .iter()
                        .position(|arg| arg == "--pool-id")
                        .and_then(|position| args.get(position + 1))
                        .cloned()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().hyphenated().to_string()),
                };
                let pool = review_pool::activate(&db, &pool_id, &members)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "active": true,
                        "poolId": pool.pool_id,
                        "focusSegmentCount": pool.focus_segment_count,
                        "focusSha256": pool.focus_sha256,
                        "championModelVersionId": pool.champion_model_version_id,
                        "championDeploymentSha256": pool.champion_deployment_sha256,
                        "voices": reports,
                        "coverageByVoice": review_pool::coverage_by_voice(&db)?,
                    }))?
                );
            }
        }
        _ => return Err(usage().into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> VoiceInventory {
        VoiceInventory {
            voice_name: "Kawa".into(),
            directory: r"D:\Kawa_TTS_Dataset\wavs".into(),
            disk_wavs: 2,
            matched_files: 2,
            matched_segments: 3,
            usable_7b_segments: 3,
            missing_files: Vec::new(),
            invalid_segments: Vec::new(),
        }
    }

    #[test]
    fn long_wavs_may_expand_to_multiple_exact_review_segments() {
        assert!(voice_inventory_ready(&report()));
    }

    #[test]
    fn readiness_fails_for_a_missing_wav_or_any_invalid_segment() {
        let mut missing = report();
        missing.matched_files = 1;
        assert!(!voice_inventory_ready(&missing));

        let mut invalid = report();
        invalid.invalid_segments.push("segment:wrong-model".into());
        assert!(!voice_inventory_ready(&invalid));
    }
}
