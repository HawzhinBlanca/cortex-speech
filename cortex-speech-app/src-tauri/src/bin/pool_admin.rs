//! Offline operator tool for the immutable flexible review pool.
//!
//! `inventory` proves that every WAV in each named prepared directory is present in SQLite with a
//! usable OmniASR-7B draft. `activate` repeats the same proof, then atomically binds those exact
//! segments to voice characters. Quarantine/reject directories are never discovered implicitly: the
//! operator names the exact final `wavs` directories.

use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::review_pool::{self, PoolMemberInput};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
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
    "Usage:\n  pool_admin migrate --db <cortex-speech.db>\n  pool_admin inventory --db <cortex-speech.db> --voice <Name=final-wavs-dir> [--voice ...]\n  pool_admin activate --db <cortex-speech.db> --voice <Name=final-wavs-dir> [--voice ...] [--pool-id <uuid>]\n  pool_admin status --db <cortex-speech.db>\n  pool_admin certify --db <cortex-speech.db> [--full-integrity] [--require-review-ready | --require-final-ready]\n  pool_admin probe --db <cortex-speech.db> --reviewer <Name> [--dialect <Name> ...]\n  pool_admin benchmark --db <cortex-speech.db> --reviewer <Name> [--dialect <Name> ...] [--iterations <1..100>]\n  pool_admin benchmark-commit --db <DISPOSABLE-clone.db> --iterations <1..500> --confirm-disposable\n  pool_admin stamp-rights --db <cortex-speech.db>\n  pool_admin adjudicate --db <cortex-speech.db> --segment <id> (--retain-text <text> | --reject) --operation-id <uuid>\n  pool_admin export --db <cortex-speech.db> --voice-name <Name> --output <directory>"
}

fn value_after(args: &[String], flag: &str) -> Result<String, String> {
    let position = args.iter().position(|arg| arg == flag).ok_or_else(|| format!("missing {flag}"))?;
    args.get(position + 1).cloned().ok_or_else(|| format!("missing value after {flag}"))
}

fn optional_value_after(args: &[String], flag: &str) -> Result<Option<String>, String> {
    let Some(position) = args.iter().position(|arg| arg == flag) else {
        return Ok(None);
    };
    args.get(position + 1).cloned().map(Some).ok_or_else(|| format!("missing value after {flag}"))
}

fn unix_time_ms() -> Result<i64, String> {
    let value = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
        .as_millis();
    i64::try_from(value).map_err(|_| "system clock value exceeds SQLite integer range".to_string())
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

fn repeated_values(args: &[String], flag: &str) -> Result<Vec<String>, String> {
    let mut values = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if arg == flag {
            values.push(args.get(index + 1).cloned().ok_or_else(|| format!("missing value after {flag}"))?);
        }
    }
    Ok(values)
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

#[derive(Debug, Clone)]
struct CommitBenchmarkClip {
    segment_id: String,
    raw_transcript: String,
    revision: i64,
    audio_content_hash: String,
    source_start_ms: i64,
    source_end_ms: i64,
    duration_ms: i64,
}

fn commit_benchmark_clip(db: &Database, segment_id: &str) -> Result<CommitBenchmarkClip, String> {
    db.connection()
        .query_row(
            "SELECT segment.id, member.raw_transcript, segment.review_revision,
                    member.audio_content_hash, member.source_start_ms, member.source_end_ms,
                    member.duration_ms
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              WHERE member.segment_id=?1 AND segment.verified=1
                AND segment.human_decision IN ('accept','edit','reject')",
            [segment_id],
            |row| {
                Ok(CommitBenchmarkClip {
                    segment_id: row.get(0)?,
                    raw_transcript: row.get(1)?,
                    revision: row.get(2)?,
                    audio_content_hash: row.get(3)?,
                    source_start_ms: row.get(4)?,
                    source_end_ms: row.get(5)?,
                    duration_ms: row.get(6)?,
                })
            },
        )
        .map_err(|error| format!("commit benchmark clip {segment_id} cannot be loaded: {error}"))
}

fn commit_benchmark_worker(
    db: Database,
    pool: review_pool::ReviewPool,
    clip: CommitBenchmarkClip,
    reviewer: String,
    iterations: usize,
) -> Result<Vec<f64>, String> {
    let mut commits = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let operation_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let started = std::time::Instant::now();
        let decision_id = review_pool::record_decision(
            &db,
            &pool,
            &review_pool::PoolDecisionInput {
                segment_id: &clip.segment_id,
                reviewer: &reviewer,
                action: "edit",
                submitted_transcript: Some(&clip.raw_transcript),
                served_transcript: &clip.raw_transcript,
                served_revision: clip.revision,
                audio_content_hash: Some(&clip.audio_content_hash),
                source_start_ms: Some(clip.source_start_ms),
                source_end_ms: Some(clip.source_end_ms),
                duration_ms: clip.duration_ms,
                requested_action: "edit",
                requested_transcript: &clip.raw_transcript,
                operation_id: &operation_id,
                operation_payload_hash: &if reviewer.ends_with('A') { "a".repeat(64) } else { "b".repeat(64) },
                created_at_ms: 1_000_000_i64.saturating_add(index as i64),
            },
        )?
        .ok_or_else(|| "commit benchmark decision unexpectedly changed zero rows".to_string())?;
        commits.push(started.elapsed().as_secs_f64() * 1000.0);
        review_pool::reverse_decision(
            &db,
            &pool,
            decision_id,
            &reviewer,
            &uuid::Uuid::new_v4().hyphenated().to_string(),
            2_000_000_i64.saturating_add(index as i64),
        )?;
    }
    Ok(commits)
}

fn current_epoch_secs() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))
}

fn snapshot_epoch(name: &str, pinned: bool) -> Option<u64> {
    let value = if pinned { name.rsplit_once('_')?.1 } else { name.strip_prefix("snapshot_")? };
    if value.len() == 10 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse::<u64>().ok()
    } else {
        None
    }
}

fn latest_snapshot(root: &Path, now: u64) -> serde_json::Value {
    let mut latest: Option<(u64, PathBuf)> = None;
    for (directory, pinned) in [(root.to_path_buf(), false), (root.join("pinned"), true)] {
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                let epoch = entry.file_name().to_str().and_then(|name| snapshot_epoch(name, pinned));
                if !path.is_dir()
                    || !path.join("cortex-speech.db").is_file()
                    || !path.join("SNAPSHOT_MANIFEST.json").is_file()
                {
                    continue;
                }
                if let Some(epoch) = epoch {
                    if latest.as_ref().map_or(true, |(current, _)| epoch > *current) {
                        latest = Some((epoch, path));
                    }
                }
            }
        }
    }
    match latest {
        Some((epoch, path)) => {
            let age = now.saturating_sub(epoch);
            let verification = cortex_speech_app_lib::snapshot::verify_snapshot_manifest_for_restore(&path);
            let verified = verification.as_ref().is_ok_and(|value| *value);
            serde_json::json!({
                "root": root.to_string_lossy(),
                "path": path.to_string_lossy(),
                "createdAtEpochSecs": epoch,
                "ageSecs": age,
                "targetRpoSecs": 600,
                "verified": verified,
                "verificationError": verification.err(),
                "fresh": age <= 660 && verified,
            })
        }
        None => serde_json::json!({
            "root": root.to_string_lossy(),
            "createdAtEpochSecs": null,
            "ageSecs": null,
            "targetRpoSecs": 600,
            "verified": false,
            "verificationError": null,
            "fresh": false,
        }),
    }
}

fn configured_offsite_snapshots(data_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(data_dir.join("settings.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("backup_second_dir")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join("snapshots"))
}

fn reviewer_voice_totals(db: &Database) -> Result<Vec<serde_json::Value>, String> {
    let mut statement = db
        .connection()
        .prepare(
            "WITH evidence(voice_name, reviewer, action) AS (
                 SELECT member.voice_name, trim(COALESCE(segment.reviewed_by, '@desktop-owner')),
                        segment.human_decision
                   FROM review_pool_members member
                   JOIN speech_segments segment ON segment.id=member.segment_id
                  WHERE segment.verified=1
                    AND segment.human_decision IN ('accept','edit','reject')
                 UNION ALL
                 SELECT member.voice_name, trim(decision.reviewer), decision.action
                   FROM effective_independent_review_decisions_v61 decision
                   JOIN review_pool_members member ON member.segment_id=decision.segment_id
                 UNION ALL
                 SELECT member.voice_name, trim(decision.reviewer), decision.action
                   FROM effective_review_pool_decisions_v62 decision
                   JOIN review_pool_members member
                     ON member.pool_id=decision.pool_id AND member.segment_id=decision.segment_id
             )
             SELECT voice_name, lower(reviewer), MIN(reviewer),
                    SUM(CASE WHEN action='skip' THEN 0 ELSE 1 END),
                    SUM(CASE WHEN action='skip' THEN 1 ELSE 0 END)
               FROM evidence
              GROUP BY voice_name, lower(reviewer)
              ORDER BY voice_name, lower(reviewer)",
        )
        .map_err(|error| format!("reviewer/voice totals cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(serde_json::json!({
                "voiceName": row.get::<_, String>(0)?,
                "reviewerKey": row.get::<_, String>(1)?,
                "reviewer": row.get::<_, String>(2)?,
                "judgments": row.get::<_, i64>(3)?,
                "skips": row.get::<_, i64>(4)?,
            }))
        })
        .map_err(|error| format!("reviewer/voice totals cannot be read: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|error| format!("reviewer/voice totals are unreadable: {error}"))
}

fn audio_coverage(db: &Database) -> Result<serde_json::Value, String> {
    let mut statement = db
        .connection()
        .prepare(
            "SELECT member.voice_name, segment.audio_path, COUNT(*)
               FROM review_pool_members member
               JOIN speech_segments segment ON segment.id=member.segment_id
              GROUP BY member.voice_name, segment.audio_path
              ORDER BY member.voice_name, segment.audio_path",
        )
        .map_err(|error| format!("pool audio coverage cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))
        .map_err(|error| format!("pool audio coverage cannot be read: {error}"))?;
    let mut recordings = 0_i64;
    let mut clips = 0_i64;
    let mut missing_recordings = 0_i64;
    let mut missing_clips = 0_i64;
    let mut missing_by_voice: HashMap<String, i64> = HashMap::new();
    for row in rows {
        let (voice, path, count) = row.map_err(|error| format!("pool audio coverage is unreadable: {error}"))?;
        recordings += 1;
        clips += count;
        if !Path::new(&path).is_file() {
            missing_recordings += 1;
            missing_clips += count;
            *missing_by_voice.entry(voice).or_default() += count;
        }
    }
    Ok(serde_json::json!({
        "recordings": recordings,
        "clips": clips,
        "missingRecordings": missing_recordings,
        "missingClips": missing_clips,
        "missingClipsByVoice": missing_by_voice,
        "allAvailable": recordings > 0 && missing_recordings == 0,
    }))
}

fn sqlite_check(db: &Database, pragma: &str) -> Result<Vec<String>, String> {
    let sql = format!("PRAGMA {pragma}");
    let mut statement = db.connection().prepare(&sql).map_err(|error| format!("{pragma} cannot start: {error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("{pragma} cannot run: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|error| format!("{pragma} result is unreadable: {error}"))
}

fn submission_idempotency_authority(db: &Database) -> Result<bool, String> {
    let mut indexes = db
        .connection()
        .prepare("PRAGMA index_list('review_pool_decisions')")
        .map_err(|error| format!("review-pool idempotency indexes cannot be read: {error}"))?;
    let rows = indexes
        .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))
        .map_err(|error| format!("review-pool idempotency indexes cannot be read: {error}"))?;
    let mut unique_operation_id = false;
    for row in rows {
        let (name, unique) = row.map_err(|error| format!("review-pool idempotency index is unreadable: {error}"))?;
        if unique != 1 {
            continue;
        }
        let mut info = db
            .connection()
            .prepare(&format!("PRAGMA index_info(\"{}\")", name.replace('"', "\"\"")))
            .map_err(|error| format!("review-pool idempotency index {name} cannot be inspected: {error}"))?;
        let columns = info
            .query_map([], |row| row.get::<_, String>(2))
            .map_err(|error| format!("review-pool idempotency index {name} cannot be inspected: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("review-pool idempotency index {name} is unreadable: {error}"))?;
        if columns == ["operation_id"] {
            unique_operation_id = true;
        }
    }
    let collision_triggers: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
              WHERE type='trigger' AND name IN (
                    'review_pool_decision_validate_insert',
                    'review_events_v62_pool_operation_collision',
                    'independent_review_v62_pool_operation_collision'
              )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("review-pool collision triggers cannot be inspected: {error}"))?;
    let probe_operation = "00000000-0000-4000-8000-000000000000";
    let lookup_works = review_pool::operation(db, probe_operation)?.is_none();
    Ok(unique_operation_id && collision_triggers == 3 && lookup_works)
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
                let usable = !(draft.is_empty() || draft.starts_with('[') && draft.ends_with(']'))
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
    let _instance_lock =
        if matches!(command, "migrate" | "activate" | "benchmark-commit" | "stamp-rights" | "adjudicate" | "export") {
            let data_dir = db_path
                .parent()
                .ok_or_else(|| format!("database has no parent data directory: {}", db_path.display()))?;
            Some(
                cortex_speech_app_lib::flock::InstanceLock::try_lock(data_dir)
                    .map_err(|error| format!("{command} requires Cortex and every writer to be stopped: {error}"))?,
            )
        } else {
            None
        };
    let db = Database::open_with_retry(&db_path.to_string_lossy())?;
    if command == "migrate" {
        let before = cortex_speech_app_lib::migrations::get_current_version(&db)?;
        let data_dir =
            db_path.parent().ok_or_else(|| format!("database has no parent data directory: {}", db_path.display()))?;
        let pinned = cortex_speech_app_lib::snapshot::initialize_with_required_pre_migration_pin(&db, data_dir)?;
        let after = cortex_speech_app_lib::migrations::validate_applied_history(db.connection())?;
        if after != cortex_speech_app_lib::migrations::max_supported_version() {
            return Err(format!(
                "database migration stopped at schema {after}; this tool requires {}",
                cortex_speech_app_lib::migrations::max_supported_version()
            )
            .into());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "migrated": before != after,
                "beforeSchemaVersion": before,
                "afterSchemaVersion": after,
                "preMigrationPinnedSnapshot": pinned,
                "appGitSha": cortex_speech_app_lib::GIT_SHA,
            }))?
        );
        return Ok(());
    }
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
                    "resolutionSummary": review_pool::resolution_summary(&db)?,
                    "voiceCertificates": review_pool::coverage_by_voice(&db)?
                        .into_iter()
                        .map(|voice| {
                            review_pool::voice_certificate(&db, &voice.voice_name)
                                .map(|certificate| serde_json::json!({
                                    "voiceName": voice.voice_name,
                                    "certificate": certificate,
                                }))
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                }),
                None => serde_json::json!({ "active": false }),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        "certify" => {
            let pool = review_pool::load(&db)?.ok_or("review pool is not active")?;
            let coverage = review_pool::coverage_by_voice(&db)?;
            let resolutions = review_pool::segment_resolutions(&db, None)?;
            let resolution_summary = review_pool::resolution_summary(&db)?;
            let rights = review_pool::rights_coverage(&db)?;
            let audio = audio_coverage(&db)?;
            let quick_check = sqlite_check(&db, "quick_check")?;
            let full_integrity = if args.iter().any(|arg| arg == "--full-integrity") {
                Some(sqlite_check(&db, "integrity_check")?)
            } else {
                None
            };
            let foreign_key_violations: i64 =
                db.connection().query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
            let database_healthy = quick_check == ["ok"]
                && full_integrity.as_ref().map_or(true, |rows| rows.as_slice() == ["ok"])
                && foreign_key_violations == 0;
            let data_dir = db_path
                .parent()
                .ok_or_else(|| format!("database has no parent data directory: {}", db_path.display()))?;
            let now = current_epoch_secs()?;
            let local_snapshot = latest_snapshot(&data_dir.join("snapshots"), now);
            let offsite_root = configured_offsite_snapshots(data_dir);
            let offsite_snapshot = offsite_root
                .as_deref()
                .map(|root| latest_snapshot(root, now))
                .unwrap_or_else(|| serde_json::json!({"configured": false, "fresh": false}));
            let local_fresh = local_snapshot.get("fresh").and_then(serde_json::Value::as_bool) == Some(true);
            let offsite_fresh = offsite_snapshot.get("fresh").and_then(serde_json::Value::as_bool) == Some(true);
            let free_disk_bytes = cortex_speech_app_lib::health::free_disk_bytes_for(data_dir);
            let disk_healthy = free_disk_bytes.is_some_and(|bytes| bytes >= 20 * 1024 * 1024 * 1024);
            let audio_healthy = audio.get("allAvailable").and_then(serde_json::Value::as_bool) == Some(true);
            let review_ready = database_healthy && audio_healthy && local_fresh && offsite_fresh && disk_healthy;
            let all_resolved = resolution_summary.resolved_clips == resolution_summary.total_clips
                && resolution_summary.needs_first_or_second_review == 0
                && resolution_summary.needs_third_review == 0
                && resolution_summary.owner_conflicts == 0;
            let mut voice_outcomes: BTreeMap<String, serde_json::Value> = BTreeMap::new();
            for voice in &coverage {
                let voice_rows: Vec<_> = resolutions.iter().filter(|row| row.voice_name == voice.voice_name).collect();
                let retained = voice_rows.iter().filter(|row| row.final_action.as_deref() == Some("retain")).count();
                let rejected = voice_rows.iter().filter(|row| row.final_action.as_deref() == Some("reject")).count();
                let certificate = review_pool::voice_certificate(&db, &voice.voice_name)?;
                voice_outcomes.insert(
                    voice.voice_name.clone(),
                    serde_json::json!({
                        "total": voice_rows.len(),
                        "retained": retained,
                        "rejected": rejected,
                        "unresolved": voice_rows.len().saturating_sub(retained + rejected),
                        "certificate": certificate,
                    }),
                );
            }
            let every_voice_certified =
                voice_outcomes.values().all(|row| row.get("certificate").is_some_and(|value| !value.is_null()));
            let final_dataset_ready = review_ready && all_resolved && rights.all_exact && every_voice_certified;
            let last_decision_at_ms: Option<i64> = db.connection().query_row(
                "SELECT MAX(created_at_ms) FROM (
                     SELECT decision.created_at_ms
                       FROM effective_review_pool_decisions_v62 decision
                     UNION ALL
                     SELECT decision.created_at_ms
                       FROM effective_independent_review_decisions_v61 decision
                       JOIN review_pool_members member ON member.segment_id=decision.segment_id
                     UNION ALL
                     SELECT event.timestamp_ms
                       FROM review_events event
                       JOIN review_pool_members member ON member.segment_id=event.segment_id
                 )",
                [],
                |row| row.get(0),
            )?;
            let report = serde_json::json!({
                "reportSchema": 1,
                "readOnly": true,
                "generatedAtEpochSecs": now,
                "appGitSha": cortex_speech_app_lib::GIT_SHA,
                "databaseSchemaVersion": schema_version,
                "pool": {
                    "poolId": pool.pool_id,
                    "focusSegmentCount": pool.focus_segment_count,
                    "focusSha256": pool.focus_sha256,
                    "championModelVersionId": pool.champion_model_version_id,
                    "championDeploymentSha256": pool.champion_deployment_sha256,
                },
                "resolutionSummary": resolution_summary,
                "coverageByVoice": coverage,
                "voiceOutcomes": voice_outcomes,
                "reviewerVoiceTotals": reviewer_voice_totals(&db)?,
                "lastDecisionAtMs": last_decision_at_ms,
                "rights": rights,
                "audio": audio,
                "database": {
                    "quickCheck": quick_check,
                    "fullIntegrityCheck": full_integrity,
                    "foreignKeyViolations": foreign_key_violations,
                    "healthy": database_healthy,
                },
                "disk": {
                    "freeBytes": free_disk_bytes,
                    "minimumFreeBytes": 20_u64 * 1024 * 1024 * 1024,
                    "healthy": disk_healthy,
                },
                "snapshots": {
                    "local": local_snapshot,
                    "offsite": offsite_snapshot,
                },
                "gates": {
                    "reviewReady": review_ready,
                    "allClipsResolved": all_resolved,
                    "rightsComplete": rights.all_exact,
                    "everyVoiceCertified": every_voice_certified,
                    "finalDatasetReady": final_dataset_ready,
                },
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
            if args.iter().any(|arg| arg == "--require-review-ready") && !review_ready {
                return Err("review-readiness certification failed".into());
            }
            if args.iter().any(|arg| arg == "--require-final-ready") && !final_dataset_ready {
                return Err("final-dataset certification failed".into());
            }
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
        "probe" => {
            let reviewer = value_after(&args, "--reviewer")?;
            let dialects = repeated_values(&args, "--dialect")?;
            let pool = review_pool::load(&db)?.ok_or("review pool is not active")?;
            let allowed = (!dialects.is_empty()).then_some(dialects.as_slice());
            let available = review_pool::pending_segment_ids(&db, &pool, &reviewer, allowed)?;
            let segment_id = available.first().ok_or("canonical queue has no audio sample for this reviewer")?;
            let segment = db.get_segment_by_id(segment_id)?.ok_or("canonical queue sample disappeared")?;
            let audio = cortex_speech_app_lib::agentic::segment_audio_as_wav_bytes(&segment)?;
            let valid_wav = audio.len() >= 44 && audio.starts_with(b"RIFF") && audio.get(8..12) == Some(&b"WAVE"[..]);
            let idempotency = submission_idempotency_authority(&db)?;
            let passes = valid_wav && idempotency;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "readOnly": true,
                    "reviewer": reviewer,
                    "dialects": dialects,
                    "availableClips": available.len(),
                    "sampleSegmentId": segment_id,
                    "sampleAudioBytes": audio.len(),
                    "sampleAudioValidWav": valid_wav,
                    "submissionIdempotencyAuthority": idempotency,
                    "passes": passes,
                }))?
            );
            if !passes {
                return Err("private-production reviewer probe failed".into());
            }
        }
        "benchmark" => {
            let reviewer = value_after(&args, "--reviewer")?;
            let dialects = repeated_values(&args, "--dialect")?;
            let iterations = optional_value_after(&args, "--iterations")?
                .map(|value| value.parse::<usize>().map_err(|_| "--iterations must be an integer".to_string()))
                .transpose()?
                .unwrap_or(10);
            if !(1..=100).contains(&iterations) {
                return Err("--iterations must be between 1 and 100".into());
            }
            let pool = review_pool::load(&db)?.ok_or("review pool is not active")?;
            let allowed = (!dialects.is_empty()).then_some(dialects.as_slice());
            let mut samples_ms = Vec::with_capacity(iterations);
            let mut available = 0usize;
            for _ in 0..iterations {
                let started = std::time::Instant::now();
                available = review_pool::pending_segment_ids(&db, &pool, &reviewer, allowed)?.len();
                samples_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            samples_ms.sort_by(f64::total_cmp);
            let percentile_index = (samples_ms.len() * 95).div_ceil(100) - 1;
            let p95_ms = samples_ms[percentile_index];
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "reviewer": reviewer,
                    "dialects": dialects,
                    "iterations": iterations,
                    "availableClips": available,
                    "p95Ms": p95_ms,
                    "maxMs": samples_ms.last(),
                    "targetP95Ms": 750,
                    "passes": p95_ms <= 750.0,
                    "samplesMs": samples_ms,
                }))?
            );
        }
        "benchmark-commit" => {
            if !args.iter().any(|arg| arg == "--confirm-disposable") {
                return Err(
                    "benchmark-commit appends test decisions; pass --confirm-disposable only for an isolated clone"
                        .into(),
                );
            }
            let iterations = optional_value_after(&args, "--iterations")?
                .map(|value| value.parse::<usize>().map_err(|_| "--iterations must be an integer".to_string()))
                .transpose()?
                .unwrap_or(100);
            if !(1..=500).contains(&iterations) {
                return Err("--iterations must be between 1 and 500".into());
            }
            let candidates: Vec<review_pool::SegmentResolution> = review_pool::segment_resolutions(&db, None)?
                .into_iter()
                .filter(|row| row.status == "pending" && row.reviewer_count == 1)
                .take(2)
                .collect();
            if candidates.len() != 2 {
                return Err("benchmark-commit requires two one-review clips in the disposable clone".into());
            }
            let left_clip = commit_benchmark_clip(&db, &candidates[0].segment_id)?;
            let right_clip = commit_benchmark_clip(&db, &candidates[1].segment_id)?;
            let left_db = Database::open_with_retry(&db_path.to_string_lossy())?;
            let left_pool = review_pool::load(&left_db)?.ok_or("review pool is not active")?;
            let right_db = Database::open_with_retry(&db_path.to_string_lossy())?;
            let right_pool = review_pool::load(&right_db)?.ok_or("review pool is not active")?;
            let left = std::thread::spawn(move || {
                commit_benchmark_worker(left_db, left_pool, left_clip, "CommitBenchA".to_string(), iterations)
            });
            let right = std::thread::spawn(move || {
                commit_benchmark_worker(right_db, right_pool, right_clip, "CommitBenchB".to_string(), iterations)
            });
            let mut samples_ms = left.join().map_err(|_| "left commit benchmark thread panicked")??;
            samples_ms.extend(right.join().map_err(|_| "right commit benchmark thread panicked")??);
            samples_ms.sort_by(f64::total_cmp);
            let percentile_index = (samples_ms.len() * 95).div_ceil(100) - 1;
            let p95_ms = samples_ms[percentile_index];
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "simultaneousReviewers": 2,
                    "commits": samples_ms.len(),
                    "p95Ms": p95_ms,
                    "maxMs": samples_ms.last(),
                    "targetP95Ms": 500,
                    "passes": p95_ms <= 500.0,
                    "samplesMs": samples_ms,
                }))?
            );
        }
        "stamp-rights" => {
            let report = review_pool::stamp_owner_supplied_pool_rights(&db)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "adjudicate" => {
            let pool = review_pool::load(&db)?.ok_or("review pool is not active")?;
            let segment_id = value_after(&args, "--segment")?;
            let operation_id = value_after(&args, "--operation-id")?;
            let retain_text = optional_value_after(&args, "--retain-text")?;
            let reject = args.iter().any(|arg| arg == "--reject");
            let (final_action, final_transcript) = match (retain_text.as_deref(), reject) {
                (Some(text), false) if !text.trim().is_empty() => ("retain", Some(text)),
                (None, true) => ("reject", None),
                _ => return Err("adjudicate requires exactly one of --retain-text <non-empty-text> or --reject".into()),
            };
            let adjudication_id = review_pool::record_owner_adjudication(
                &db,
                &pool,
                &review_pool::OwnerAdjudicationInput {
                    segment_id: &segment_id,
                    final_action,
                    final_transcript,
                    operation_id: &operation_id,
                    created_at_ms: unix_time_ms()?,
                },
            )?;
            let resolution = review_pool::segment_resolutions(&db, None)?
                .into_iter()
                .find(|row| row.segment_id == segment_id)
                .ok_or("adjudicated segment disappeared from the active pool")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "adjudicationId": adjudication_id,
                    "resolution": resolution,
                }))?
            );
        }
        "export" => {
            let voice_name = value_after(&args, "--voice-name")?;
            let output_dir = value_after(&args, "--output")?;
            let result = cortex_speech_app_lib::review_pool_export::export_voice(
                &db,
                &cortex_speech_app_lib::review_pool_export::PoolDatasetOptions { output_dir, voice_name },
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
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

    #[test]
    fn snapshot_epoch_accepts_only_canonical_rotating_and_pinned_names() {
        assert_eq!(snapshot_epoch("snapshot_1787526734", false), Some(1_787_526_734));
        assert_eq!(snapshot_epoch("premigration_v62_to_v63_1787526734", true), Some(1_787_526_734));
        for (name, pinned) in [
            ("snapshot_123", false),
            ("snapshot_1787526734.extra", false),
            ("premigration_v62_to_v63", true),
            ("premigration_v62_to_v63_178752673x", true),
        ] {
            assert_eq!(snapshot_epoch(name, pinned), None);
        }
    }

    #[test]
    fn current_schema_has_live_submission_idempotency_authority() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(submission_idempotency_authority(&db).unwrap());
    }
}
