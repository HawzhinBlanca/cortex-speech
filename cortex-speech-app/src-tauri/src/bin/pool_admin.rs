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

const CERTIFICATION_REPORT_SCHEMA: u32 = 3;

#[derive(Debug, Clone)]
struct VoiceSpec {
    name: String,
    directory: PathBuf,
}

/// One library row matched to a prepared WAV: its draft, the model that produced it, and the exact
/// audio identity `review_pool::activate` binds. Named rather than a wide tuple so adding the
/// identity columns did not have to widen every destructuring site.
#[derive(Debug, Clone)]
struct LibrarySegment {
    id: String,
    raw_transcript: String,
    model_id: String,
    audio_content_hash: String,
    source_start_ms: Option<i64>,
    source_end_ms: Option<i64>,
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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ResolutionAuthorityTotals {
    consensus_agreements: usize,
    owner_adjudications: usize,
    unresolved_conflicts: usize,
}

fn resolution_authority_totals<'a>(
    rows: impl IntoIterator<Item = &'a review_pool::SegmentResolution>,
) -> ResolutionAuthorityTotals {
    let mut totals =
        ResolutionAuthorityTotals { consensus_agreements: 0, owner_adjudications: 0, unresolved_conflicts: 0 };
    for row in rows {
        match row.status.as_str() {
            "resolved" => totals.consensus_agreements += 1,
            "ownerResolved" => totals.owner_adjudications += 1,
            "ownerConflict" => totals.unresolved_conflicts += 1,
            _ => {}
        }
    }
    totals
}

fn voice_inventory_ready(report: &VoiceInventory) -> bool {
    // A long prepared WAV is intentionally split into multiple bounded review clips. Completeness is
    // therefore every disk WAV matched at least once and every resulting segment exact/usable—not the
    // false assumption that WAV count must equal segment count. The doubled-generation check that the
    // old equality used to provide incidentally now lives in `inventory` as an explicit refusal
    // (`first_overlapping_window`), so relaxing this predicate no longer costs it.
    report.disk_wavs == report.matched_files
        && report.matched_segments >= report.disk_wavs
        && report.matched_segments == report.usable_7b_segments
        && report.missing_files.is_empty()
        && report.invalid_segments.is_empty()
}

/// The first pair of canonical source windows of ONE recording that OVERLAP, as `(earlier, later)`
/// segment ids. Sorts `windows` in place.
///
/// `review_pool::activate` refuses two members that share the exact (content hash, start, end)
/// activation triple, so an identical-settings double import already collides there. A SPAN-DIVERGENT
/// double — the same recording re-imported under a different `max_segment_duration` or VAD threshold —
/// produces different windows over the same audio, clears that triple, and would bind BOTH generations
/// into the pool: the same audio servable and PAYABLE twice. Overlap within one content hash is the
/// only check that sees it, and it does NOT re-break the bounded-span case: contiguous clips cut from
/// one long prepared WAV never overlap.
fn first_overlapping_window(spans: &mut [(i64, i64, String)]) -> Option<(String, String)> {
    spans.sort_unstable();
    spans.windows(2).find(|pair| pair[1].0 < pair[0].1).map(|pair| (pair[0].2.clone(), pair[1].2.clone()))
}

fn usage() -> &'static str {
    "Usage:\n  pool_admin migrate --db <cortex-speech.db>\n  pool_admin inventory --db <cortex-speech.db> --voice <Name=final-wavs-dir> [--voice ...]\n  pool_admin activate --db <cortex-speech.db> --voice <Name=final-wavs-dir> [--voice ...] [--pool-id <uuid>]\n  pool_admin apply-dedup --db <cortex-speech.db> --manifest <review-pool-dedup.json>\n  pool_admin status --db <cortex-speech.db>\n  pool_admin certify --db <cortex-speech.db> [--full-integrity] [--require-review-ready | --require-final-ready]\n  pool_admin probe --db <cortex-speech.db> --reviewer <Name> [--dialect <Name> ...]\n  pool_admin benchmark --db <cortex-speech.db> --reviewer <Name> [--dialect <Name> ...] [--iterations <1..100>]\n  pool_admin benchmark-commit --db <DISPOSABLE-clone.db> --iterations <1..500> --confirm-disposable\n  pool_admin stamp-rights --db <cortex-speech.db>\n  pool_admin adjudicate --db <cortex-speech.db> --segment <id> (--retain-text <text> | --reject) --operation-id <uuid>\n  pool_admin export --db <cortex-speech.db> --voice-name <Name> --output <directory>"
}

const DETACHED_READ_COMMANDS: &[&str] = &["certify"];
const DIRECT_READ_COMMANDS: &[&str] = &["inventory", "status", "probe", "benchmark"];
const WRITE_COMMANDS: &[&str] =
    &["migrate", "activate", "apply-dedup", "benchmark-commit", "stamp-rights", "adjudicate", "export"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseAccess {
    DetachedRead,
    DirectRead,
    LockedWrite,
}

fn command_database_access(command: &str) -> Result<DatabaseAccess, String> {
    if DETACHED_READ_COMMANDS.contains(&command) {
        Ok(DatabaseAccess::DetachedRead)
    } else if DIRECT_READ_COMMANDS.contains(&command) {
        Ok(DatabaseAccess::DirectRead)
    } else if WRITE_COMMANDS.contains(&command) {
        Ok(DatabaseAccess::LockedWrite)
    } else {
        Err(usage().to_string())
    }
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
                    AND NOT EXISTS (
                        SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                         WHERE exclusion.pool_id=member.pool_id AND exclusion.segment_id=member.segment_id
                    )
                 UNION ALL
                 SELECT member.voice_name, trim(decision.reviewer), decision.action
                   FROM effective_independent_review_decisions_v61 decision
                   JOIN review_pool_members member ON member.segment_id=decision.segment_id
                  WHERE NOT EXISTS (
                        SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                         WHERE exclusion.pool_id=member.pool_id AND exclusion.segment_id=member.segment_id
                    )
                 UNION ALL
                 SELECT member.voice_name, trim(decision.reviewer), decision.action
                   FROM effective_review_pool_decisions_v62 decision
                   JOIN review_pool_members member
                     ON member.pool_id=decision.pool_id AND member.segment_id=decision.segment_id
                  WHERE NOT EXISTS (
                        SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                         WHERE exclusion.pool_id=member.pool_id AND exclusion.segment_id=member.segment_id
                    )
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
              WHERE NOT EXISTS (
                    SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                     WHERE exclusion.pool_id=member.pool_id AND exclusion.segment_id=member.segment_id
              )
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
    let mut by_path: HashMap<String, Vec<LibrarySegment>> = HashMap::new();
    let mut statement = db
        .connection()
        .prepare(
            "SELECT id, audio_path, raw_transcript, COALESCE(model_version_id, ''),
                    COALESCE(audio_content_hash, ''),
                    json_extract(alignment_json, '$.source_start_ms'),
                    json_extract(alignment_json, '$.source_end_ms')
               FROM speech_segments",
        )
        .map_err(|error| format!("library inventory cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                LibrarySegment {
                    id: row.get(0)?,
                    raw_transcript: row.get(2)?,
                    model_id: row.get(3)?,
                    audio_content_hash: row.get(4)?,
                    source_start_ms: row.get(5)?,
                    source_end_ms: row.get(6)?,
                },
            ))
        })
        .map_err(|error| format!("library inventory cannot be read: {error}"))?;
    for row in rows {
        let (audio_path, segment) = row.map_err(|error| format!("library inventory row is unreadable: {error}"))?;
        by_path.entry(normalized_path(Path::new(&audio_path))).or_default().push(segment);
    }

    let mut reports = Vec::new();
    let mut members = Vec::new();
    let mut assigned_segments: HashMap<String, String> = HashMap::new();
    // Every canonical audio window this activation would bind, keyed by RECORDING identity. All clips
    // cut from one source share its `audio_content_hash` (blake3 over the decoded PCM), so this groups
    // the two generations of a doubled import together even when they were imported under different
    // file names — which is where the money leak hides.
    let mut windows_by_recording: HashMap<String, Vec<(i64, i64, String)>> = HashMap::new();
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
            for segment in segments {
                let segment_id = &segment.id;
                let model_id = &segment.model_id;
                let draft = segment.raw_transcript.trim();
                // The shared authority, not a re-implementation: an inventory that called 'n/a' usable
                // while the serving queue did not is exactly the drift this pool gate exists to catch.
                let usable =
                    !cortex_speech_app_lib::quality::is_placeholder_transcript(draft) && model_id == &champion_model_id;
                if !usable {
                    invalid_segments.push(format!("{segment_id}:{model_id}"));
                    continue;
                }
                usable_7b_segments += 1;
                // Activation binds (content hash, start, end); a clip missing either half of that
                // identity is refused there, so refuse it HERE too rather than let `inventory` report
                // an activation that cannot happen.
                if segment.audio_content_hash.is_empty() {
                    return Err(format!("segment {segment_id} has no canonical audio-content hash"));
                }
                let (Some(start_ms), Some(end_ms)) = (segment.source_start_ms, segment.source_end_ms) else {
                    return Err(format!("segment {segment_id} has no canonical source span"));
                };
                match assigned_segments.insert(segment_id.clone(), spec.name.clone()) {
                    Some(existing) if !existing.eq_ignore_ascii_case(&spec.name) => {
                        return Err(format!(
                            "segment {segment_id} appears in both voice {existing} and voice {}",
                            spec.name
                        ))
                    }
                    // Already counted from a nested/overlapping prepared directory: one segment is one
                    // window, and re-adding it would look like an overlap with itself.
                    Some(_) => {}
                    None => windows_by_recording.entry(segment.audio_content_hash.clone()).or_default().push((
                        start_ms,
                        end_ms,
                        segment_id.clone(),
                    )),
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
    for (content_hash, mut windows) in windows_by_recording {
        if let Some((earlier, later)) = first_overlapping_window(&mut windows) {
            return Err(format!(
                "segments {earlier} and {later} cover overlapping audio of recording {content_hash}: \
                 a doubled import generation would be servable and payable twice"
            ));
        }
    }
    members.sort_unstable_by(|left, right| left.segment_id.cmp(&right.segment_id));
    members.dedup_by(|left, right| left.segment_id == right.segment_id);
    Ok((reports, members))
}

/// Everything `certify` prints and enforces, computed without touching the process boundary so the
/// gate verdicts are testable against fixture databases. `main` only prints the report and applies
/// the `--require-*` flags to the two readiness booleans returned here.
#[derive(Debug)]
struct CertificationOutcome {
    report: serde_json::Value,
    review_ready: bool,
    final_dataset_ready: bool,
}

fn certification_outcome(
    db: &Database,
    data_dir: &Path,
    schema_version: i64,
    full_integrity_requested: bool,
) -> Result<CertificationOutcome, Box<dyn std::error::Error>> {
    let pool = review_pool::load(db)?.ok_or("review pool is not active")?;
    let coverage = review_pool::coverage_by_voice(db)?;
    let resolutions = review_pool::segment_resolutions(db, None)?;
    let resolution_authority = resolution_authority_totals(&resolutions);
    let resolution_summary = review_pool::resolution_summary(db)?;
    let dedup = review_pool::dedup_status(db)?;
    let rights = review_pool::rights_coverage(db)?;
    let audio = audio_coverage(db)?;
    let quick_check = sqlite_check(db, "quick_check")?;
    let full_integrity = if full_integrity_requested { Some(sqlite_check(db, "integrity_check")?) } else { None };
    let foreign_key_violations: i64 =
        db.connection().query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
    let database_healthy = quick_check == ["ok"]
        && full_integrity.as_ref().map_or(true, |rows| rows.as_slice() == ["ok"])
        && foreign_key_violations == 0;
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
    let dedup_healthy = dedup.applied
        && dedup.unconfirmed_risk_count == 0
        && dedup.source_segment_count == dedup.canonical_segment_count.saturating_add(dedup.excluded_segment_count);
    let review_ready =
        database_healthy && audio_healthy && local_fresh && offsite_fresh && disk_healthy && dedup_healthy;
    let all_resolved = resolution_summary.resolved_clips == resolution_summary.total_clips
        && resolution_summary.needs_first_or_second_review == 0
        && resolution_summary.needs_third_review == 0
        && resolution_summary.owner_conflicts == 0;
    let mut voice_outcomes: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for voice in &coverage {
        let voice_rows: Vec<_> = resolutions.iter().filter(|row| row.voice_name == voice.voice_name).collect();
        let retained = voice_rows.iter().filter(|row| row.final_action.as_deref() == Some("retain")).count();
        let rejected = voice_rows.iter().filter(|row| row.final_action.as_deref() == Some("reject")).count();
        let authority = resolution_authority_totals(voice_rows.iter().copied());
        let certificate = review_pool::voice_certificate(db, &voice.voice_name)?;
        voice_outcomes.insert(
            voice.voice_name.clone(),
            serde_json::json!({
                "total": voice_rows.len(),
                "retained": retained,
                "rejected": rejected,
                "unresolved": voice_rows.len().saturating_sub(retained + rejected),
                "consensusAgreements": authority.consensus_agreements,
                "ownerAdjudications": authority.owner_adjudications,
                "unresolvedConflicts": authority.unresolved_conflicts,
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
        "reportSchema": CERTIFICATION_REPORT_SCHEMA,
        "readOnly": true,
        "generatedAtEpochSecs": now,
        "appGitSha": cortex_speech_app_lib::GIT_SHA,
        "databaseSchemaVersion": schema_version,
        "pool": {
            "poolId": pool.pool_id,
            "focusSegmentCount": pool.focus_segment_count,
            "focusSha256": pool.focus_sha256,
            "reviewSegmentCount": pool.review_segment_count,
            "excludedDuplicateCount": pool.excluded_duplicate_count,
            "duplicateFamilyCount": pool.duplicate_family_count,
            "dedupManifestSha256": pool.dedup_manifest_sha256,
            "championModelVersionId": pool.champion_model_version_id,
            "championDeploymentSha256": pool.champion_deployment_sha256,
        },
        "resolutionSummary": resolution_summary,
        "dedup": dedup,
        "resolutionAuthority": resolution_authority,
        "coverageByVoice": coverage,
        "voiceOutcomes": voice_outcomes,
        "reviewerVoiceTotals": reviewer_voice_totals(db)?,
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
            "duplicateExclusionsBound": dedup_healthy,
            "allClipsResolved": all_resolved,
            "rightsComplete": rights.all_exact,
            "everyVoiceCertified": every_voice_certified,
            "finalDatasetReady": final_dataset_ready,
        },
    });
    Ok(CertificationOutcome { report, review_ready, final_dataset_ready })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).ok_or_else(|| usage().to_string())?;
    let database_access = command_database_access(command)?;
    let db_path = PathBuf::from(value_after(&args, "--db")?);
    if !db_path.is_file() {
        return Err(format!("database does not exist: {}", db_path.display()).into());
    }
    let _instance_lock = if database_access == DatabaseAccess::LockedWrite {
        let data_dir =
            db_path.parent().ok_or_else(|| format!("database has no parent data directory: {}", db_path.display()))?;
        Some(
            cortex_speech_app_lib::flock::InstanceLock::try_lock(data_dir)
                .map_err(|error| format!("{command} requires Cortex and every writer to be stopped: {error}"))?,
        )
    } else {
        None
    };
    // Observational tools must never use the startup/recovery opener. Ordinary reads use SQLite's
    // source-enforced READ_ONLY flag and one stable transaction. Certification needs a detached,
    // writable in-memory copy because this build of SQLite's FTS5 integrity validation performs an
    // internal write even for PRAGMA quick_check/integrity_check; source READ_ONLY correctly refuses
    // that operation and would produce a false corruption report. The backup is WAL-consistent and
    // any internal or accidental write stays disposable. Only instance-locked commands receive source
    // write authority.
    let db = match database_access {
        DatabaseAccess::DetachedRead => Database::open_detached_read_snapshot(&db_path.to_string_lossy())?,
        DatabaseAccess::DirectRead => Database::open_read_only(&db_path.to_string_lossy())?,
        DatabaseAccess::LockedWrite => Database::open_with_retry(&db_path.to_string_lossy())?,
    };
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

    if command == "apply-dedup" {
        let manifest_path = PathBuf::from(value_after(&args, "--manifest")?);
        let manifest_json = std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("cannot read dedup manifest {}: {error}", manifest_path.display()))?;
        let status = review_pool::apply_dedup_manifest(&db, &manifest_json)?;
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
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
                    "reviewSegmentCount": pool.review_segment_count,
                    "excludedDuplicateCount": pool.excluded_duplicate_count,
                    "duplicateFamilyCount": pool.duplicate_family_count,
                    "dedupManifestSha256": pool.dedup_manifest_sha256,
                    "dedup": review_pool::dedup_status(&db)?,
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
            let data_dir = db_path
                .parent()
                .ok_or_else(|| format!("database has no parent data directory: {}", db_path.display()))?;
            let outcome =
                certification_outcome(&db, data_dir, schema_version, args.iter().any(|arg| arg == "--full-integrity"))?;
            println!("{}", serde_json::to_string_pretty(&outcome.report)?);
            if args.iter().any(|arg| arg == "--require-review-ready") && !outcome.review_ready {
                return Err("review-readiness certification failed".into());
            }
            if args.iter().any(|arg| arg == "--require-final-ready") && !outcome.final_dataset_ready {
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
    fn a_span_divergent_double_import_generation_is_refused() {
        // The hole e021ffe opened: relaxing `matched_segments == disk_wavs` to `>=` was correct for
        // bounded spans, but it was also the only thing that could notice a DOUBLED import generation.
        // Two generations of one recording cut at different `max_segment_duration` / VAD settings share
        // no (content hash, start, end) triple, so `review_pool::activate`'s identical-window check waves
        // them through — and every second of that audio becomes servable and PAYABLE twice.
        let mut doubled = vec![
            (0, 5_000, "gen-a-1".to_string()),
            (5_000, 10_000, "gen-a-2".to_string()),
            (0, 3_000, "gen-b-1".to_string()),
            (3_000, 10_000, "gen-b-2".to_string()),
        ];
        assert!(first_overlapping_window(&mut doubled).is_some(), "a span-divergent double must be refused");

        // The identical-settings double, which activation also refuses on the activation triple.
        let mut identical = vec![(0, 5_000, "first".to_string()), (0, 5_000, "second".to_string())];
        assert!(first_overlapping_window(&mut identical).is_some());

        // And the case e021ffe fixed must still pass: one long prepared WAV cut into contiguous
        // bounded review clips is NOT a duplicate generation.
        let mut bounded = vec![
            (0, 5_000, "clip-1".to_string()),
            (5_000, 10_000, "clip-2".to_string()),
            (10_000, 12_500, "clip-3".to_string()),
        ];
        assert_eq!(first_overlapping_window(&mut bounded), None, "contiguous bounded spans must still activate");
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
    fn every_admin_command_has_an_explicit_read_or_locked_write_boundary() {
        for command in DETACHED_READ_COMMANDS {
            assert_eq!(command_database_access(command), Ok(DatabaseAccess::DetachedRead), "{command}");
        }
        for command in DIRECT_READ_COMMANDS {
            assert_eq!(command_database_access(command), Ok(DatabaseAccess::DirectRead), "{command}");
        }
        for command in WRITE_COMMANDS {
            assert_eq!(command_database_access(command), Ok(DatabaseAccess::LockedWrite), "{command}");
        }
        assert!(command_database_access("unknown").is_err());
        assert!(DETACHED_READ_COMMANDS.iter().all(|command| !DIRECT_READ_COMMANDS.contains(command)));
        assert!(DETACHED_READ_COMMANDS.iter().all(|command| !WRITE_COMMANDS.contains(command)));
        assert!(DIRECT_READ_COMMANDS.iter().all(|command| !WRITE_COMMANDS.contains(command)));
    }

    #[test]
    fn certification_distinguishes_human_agreement_owner_adjudication_and_conflict() {
        let row = |segment_id: &str, status: &str| review_pool::SegmentResolution {
            segment_id: segment_id.to_string(),
            voice_name: "Lamo".to_string(),
            status: status.to_string(),
            final_action: None,
            final_transcript: None,
            evidence_sha256: "0".repeat(64),
            reviewer_count: 0,
            agreeing_reviewers: Vec::new(),
        };
        let rows = vec![
            row("agreement-a", "resolved"),
            row("agreement-b", "resolved"),
            row("owner", "ownerResolved"),
            row("conflict", "ownerConflict"),
            row("pending", "pending"),
        ];
        assert_eq!(
            resolution_authority_totals(&rows),
            ResolutionAuthorityTotals { consensus_agreements: 2, owner_adjudications: 1, unresolved_conflicts: 1 }
        );
    }

    #[test]
    fn current_schema_has_live_submission_idempotency_authority() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(submission_idempotency_authority(&db).unwrap());
    }

    // ── File-backed fixtures (same idiom as the review_pool test fixtures) ────────────────────────

    const TEST_CHAMPION: &str = "omniasr-7b-test-champion";

    fn seed_champion(db: &Database) {
        cortex_speech_app_lib::registry::register_candidate(
            db,
            &cortex_speech_app_lib::registry::NewModelVersion {
                id: TEST_CHAMPION.to_string(),
                family: cortex_speech_app_lib::deployment::OMNIASR_7B_FAMILY.to_string(),
                model_card_name: Some("test champion".to_string()),
                checkpoint_sha256: "c".repeat(64),
                checkpoint_path: "/test/champion.json".to_string(),
                source: "cortex-finetuned".to_string(),
                license: "owner-full-rights".to_string(),
            },
        )
        .unwrap();
        db.connection().execute("UPDATE model_versions SET status='champion' WHERE id=?1", [TEST_CHAMPION]).unwrap();
    }

    /// Decision columns can only be written below the verbatim-law schema, so fixture rows are
    /// inserted at 59 and migrated forward — exactly how the review_pool fixtures do it.
    fn insert_rows_at_v59(db: &Database, rows: &[cortex_speech_app_lib::db::SpeechSegment]) {
        let rolled_back: Vec<i64> = cortex_speech_app_lib::migrations::MIGRATIONS
            .iter()
            .filter(|migration| migration.version > 59)
            .rev()
            .map(|migration| migration.version)
            .collect();
        assert_eq!(cortex_speech_app_lib::migrations::rollback(db, rolled_back.len()).unwrap(), rolled_back);
        for row in rows {
            db.insert_segment_full(row).unwrap();
        }
        let reapplied: Vec<i64> = rolled_back.iter().rev().copied().collect();
        assert_eq!(cortex_speech_app_lib::migrations::run_migrations(db).unwrap(), reapplied);
    }

    fn fixture_segment(
        id: &str,
        audio_path: &Path,
        reviewed_by: Option<&str>,
    ) -> cortex_speech_app_lib::db::SpeechSegment {
        cortex_speech_app_lib::db::SpeechSegment {
            id: id.to_string(),
            audio_path: audio_path.to_string_lossy().to_string(),
            raw_transcript: "دەقی چامپیۆن".to_string(),
            annotated_transcript: reviewed_by.map(|_| "دەقی دروست".to_string()),
            verdict: reviewed_by.map(|_| "human_edit".to_string()),
            verdict_transcript: reviewed_by.map(|_| "دەقی دروست".to_string()),
            human_decision: reviewed_by.map(|_| "edit".to_string()),
            reviewed_by: reviewed_by.map(str::to_string),
            verified: reviewed_by.is_some(),
            duration_ms: 1_000,
            model_version_id: Some(TEST_CHAMPION.to_string()),
            alignment_json: Some(r#"{"source_start_ms":0,"source_end_ms":1000}"#.to_string()),
            ..cortex_speech_app_lib::db::SpeechSegment::default()
        }
    }

    fn clip_hash(index: usize) -> String {
        format!("{:064x}", index + 1)
    }

    const FIXTURE_POOL_ID: &str = "123e4567-e89b-42d3-a456-426614174060";

    /// A live, activated pool in `data_dir`: one WAV + one library row per clip, every identity
    /// column certify reads populated the way the app populates it.
    fn pool_fixture(data_dir: &Path, clips: &[(&str, bool)]) -> (Database, review_pool::ReviewPool) {
        let db = Database::open(&data_dir.join("cortex-speech.db").to_string_lossy()).unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        let rows: Vec<_> = clips
            .iter()
            .map(|(id, reviewed)| {
                let audio = data_dir.join(format!("{id}.wav"));
                std::fs::write(&audio, b"wav").unwrap();
                fixture_segment(id, &audio, reviewed.then_some("ReviewerA"))
            })
            .collect();
        insert_rows_at_v59(&db, &rows);
        for (index, (id, _)) in clips.iter().enumerate() {
            db.connection()
                .execute(
                    "UPDATE speech_segments SET audio_content_hash=?1 WHERE id=?2",
                    rusqlite::params![clip_hash(index), id],
                )
                .unwrap();
        }
        let members: Vec<PoolMemberInput> = clips
            .iter()
            .map(|(id, _)| PoolMemberInput { segment_id: id.to_string(), voice_name: "Lamo".to_string() })
            .collect();
        let pool = review_pool::activate(&db, FIXTURE_POOL_ID, &members).unwrap();
        (db, pool)
    }

    fn pool_decision(
        db: &Database,
        pool: &review_pool::ReviewPool,
        segment_id: &str,
        hash: &str,
        reviewer: &str,
        action: &str,
        text: Option<&str>,
        at: i64,
    ) -> i64 {
        let (_, revision) = db.get_segment_by_id_with_revision(segment_id).unwrap().unwrap();
        let skip = action == "skip";
        review_pool::record_decision(
            db,
            pool,
            &review_pool::PoolDecisionInput {
                segment_id,
                reviewer,
                action,
                submitted_transcript: text,
                served_transcript: "دەقی چامپیۆن",
                served_revision: revision,
                audio_content_hash: (!skip).then_some(hash),
                source_start_ms: (!skip).then_some(0),
                source_end_ms: (!skip).then_some(1_000),
                duration_ms: 1_000,
                requested_action: action,
                requested_transcript: text.unwrap_or(""),
                operation_id: &uuid::Uuid::new_v4().hyphenated().to_string(),
                operation_payload_hash: &"b".repeat(64),
                created_at_ms: at,
            },
        )
        .unwrap()
        .unwrap()
    }

    /// A trivial (no-duplicate) dedup manifest bound directly to the frozen pool authority, the rows
    /// `dedup_status` reads. The validate-insert trigger holds it to the registry's frozen counts.
    fn bind_trivial_dedup_manifest(db: &Database, pool: &review_pool::ReviewPool) {
        let manifest_sha256 = "d".repeat(64);
        let manifest_json = serde_json::json!({
            "manifestSchema": 1,
            "manifestSha256": manifest_sha256,
            "pool": {
                "poolId": pool.pool_id,
                "sourceFocusSegmentCount": pool.focus_segment_count,
                "sourceFocusSha256": pool.focus_sha256,
            },
            "algorithm": { "id": "cortex-cross-file-waveform-correlation-v1" },
            "summary": {
                "duplicateFamilies": 0,
                "excludedMembers": 0,
                "canonicalMembers": pool.focus_segment_count,
                "unconfirmedRiskGroups": 0,
            },
        })
        .to_string();
        db.connection()
            .execute(
                "INSERT INTO review_pool_dedup_manifests
                 (pool_id, source_focus_segment_count, source_focus_sha256, algorithm_id, family_count,
                  excluded_count, canonical_count, unconfirmed_risk_count, manifest_json, manifest_sha256,
                  app_git_sha, created_at_ms)
                 VALUES (?1, ?2, ?3, 'cortex-cross-file-waveform-correlation-v1', 0, 0, ?2, 0, ?4, ?5, ?6, 1)",
                rusqlite::params![
                    pool.pool_id,
                    pool.focus_segment_count as i64,
                    pool.focus_sha256,
                    manifest_json,
                    manifest_sha256,
                    "0".repeat(40),
                ],
            )
            .unwrap();
    }

    // ── Argument helpers ──────────────────────────────────────────────────────────────────────────

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn flag_parsers_distinguish_a_missing_flag_from_a_missing_value() {
        let parsed = args(&["--db", "x.db", "--flag"]);
        assert_eq!(value_after(&parsed, "--db").unwrap(), "x.db");
        assert_eq!(value_after(&parsed, "--missing").unwrap_err(), "missing --missing");
        assert_eq!(value_after(&parsed, "--flag").unwrap_err(), "missing value after --flag");
        assert_eq!(optional_value_after(&parsed, "--db").unwrap(), Some("x.db".to_string()));
        assert_eq!(optional_value_after(&parsed, "--missing").unwrap(), None);
        assert_eq!(optional_value_after(&parsed, "--flag").unwrap_err(), "missing value after --flag");
    }

    #[test]
    fn repeated_values_collects_in_order_and_refuses_a_trailing_flag() {
        let parsed = args(&["--dialect", "Hawleri", "--other", "x", "--dialect", "Slemani"]);
        assert_eq!(repeated_values(&parsed, "--dialect").unwrap(), ["Hawleri", "Slemani"]);
        assert!(repeated_values(&parsed, "--absent").unwrap().is_empty());
        assert_eq!(repeated_values(&args(&["--dialect"]), "--dialect").unwrap_err(), "missing value after --dialect");
    }

    #[test]
    fn usage_documents_every_dispatched_command() {
        for command in DETACHED_READ_COMMANDS.iter().chain(DIRECT_READ_COMMANDS).chain(WRITE_COMMANDS) {
            assert!(usage().contains(&format!("pool_admin {command} ")), "{command} is missing from usage");
        }
    }

    #[test]
    fn clock_helpers_report_the_present_epoch() {
        assert!(unix_time_ms().unwrap() > 1_700_000_000_000);
        assert!(current_epoch_secs().unwrap() > 1_700_000_000);
    }

    #[test]
    fn normalized_path_lowercases_forward_slashes_and_trims_trailing_separators() {
        assert_eq!(normalized_path(Path::new(r"D:\Voices\KAWA\wavs\")), "d:/voices/kawa/wavs");
        assert_eq!(
            normalized_path(Path::new("D:/Voices/kawa/wavs")),
            normalized_path(Path::new(r"d:\voices\KAWA\wavs"))
        );
    }

    #[test]
    fn collect_wavs_recurses_and_accepts_only_wav_files_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        for name in ["a.wav", "B.WAV", "nested/c.wav"] {
            std::fs::write(dir.path().join(name), b"wav").unwrap();
        }
        for name in ["notes.txt", "d.mp3", "wav"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let mut expected =
            vec![dir.path().join("a.wav"), dir.path().join("B.WAV"), dir.path().join("nested").join("c.wav")];
        expected.sort_unstable();
        assert_eq!(collect_wavs(dir.path()).unwrap(), expected);
        assert!(collect_wavs(&dir.path().join("no-such-dir")).unwrap_err().contains("cannot read prepared directory"));
    }

    #[test]
    fn voice_specs_parses_names_and_refuses_malformed_or_duplicate_specs() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let specs = voice_specs(&[
            "--voice".to_string(),
            format!(" Kawa ={}", first.path().display()),
            "--noise".to_string(),
            "--voice".to_string(),
            format!("Lamo={}", second.path().display()),
        ])
        .unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "Kawa");
        assert_eq!(specs[1].name, "Lamo");

        assert!(voice_specs(&args(&["plain"])).unwrap_err().contains("at least one --voice"));
        assert_eq!(voice_specs(&args(&["--voice"])).unwrap_err(), "missing value after --voice");
        assert!(voice_specs(&args(&["--voice", "KawaNoEquals"])).unwrap_err().contains("must be Name=directory"));
        assert!(voice_specs(&["--voice".to_string(), format!("={}", first.path().display())])
            .unwrap_err()
            .contains("empty name or missing directory"));
        assert!(voice_specs(&args(&["--voice", "Kawa=Z:/definitely/not/a/dir"]))
            .unwrap_err()
            .contains("empty name or missing directory"));
        assert!(voice_specs(&[
            "--voice".to_string(),
            format!("Kawa={}", first.path().display()),
            "--voice".to_string(),
            format!("Lamo={}", first.path().display()),
        ])
        .unwrap_err()
        .contains("specified more than once"));
    }

    // ── Snapshot and settings readers ────────────────────────────────────────────────────────────

    #[test]
    fn latest_snapshot_reports_an_absent_root_as_never_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let report = latest_snapshot(&dir.path().join("snapshots"), 1_800_000_000);
        assert_eq!(report["createdAtEpochSecs"], serde_json::Value::Null);
        assert_eq!(report["verified"], serde_json::json!(false));
        assert_eq!(report["fresh"], serde_json::json!(false));
    }

    #[test]
    fn latest_snapshot_picks_the_newest_complete_candidate_but_never_trusts_a_bad_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("snapshots");
        let complete = |path: &Path| {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("cortex-speech.db"), b"db").unwrap();
            std::fs::write(path.join("SNAPSHOT_MANIFEST.json"), b"not json").unwrap();
        };
        complete(&root.join("snapshot_1700000000"));
        complete(&root.join("snapshot_123")); // malformed epoch: never a candidate
        complete(&root.join("pinned").join("premigration_v62_to_v63_1800000000"));
        std::fs::create_dir_all(root.join("snapshot_1900000000")).unwrap(); // newer but incomplete
        let report = latest_snapshot(&root, 1_800_000_600);
        assert_eq!(report["createdAtEpochSecs"], serde_json::json!(1_800_000_000_u64));
        assert_eq!(report["ageSecs"], serde_json::json!(600));
        assert!(report["path"].as_str().unwrap().contains("premigration_v62_to_v63_1800000000"));
        // Recent enough, but its manifest fails verification — a bad manifest can never be fresh.
        assert_eq!(report["verified"], serde_json::json!(false));
        assert_eq!(report["fresh"], serde_json::json!(false));
    }

    #[test]
    fn a_real_snapshot_taken_now_is_verified_and_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("cortex-speech.db").to_string_lossy()).unwrap();
        db.initialize().unwrap();
        let taken = cortex_speech_app_lib::snapshot::take_snapshot(&db, dir.path(), 3)
            .unwrap()
            .expect("first-run snapshot must be taken");
        let report = latest_snapshot(&dir.path().join("snapshots"), current_epoch_secs().unwrap());
        assert_eq!(report["path"].as_str().unwrap(), taken.to_string_lossy());
        assert_eq!(report["verified"], serde_json::json!(true), "{report}");
        assert_eq!(report["fresh"], serde_json::json!(true), "{report}");
    }

    #[test]
    fn configured_offsite_snapshots_requires_a_nonblank_configured_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(configured_offsite_snapshots(dir.path()), None, "no settings.json");
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, b"{not json").unwrap();
        assert_eq!(configured_offsite_snapshots(dir.path()), None, "invalid JSON");
        std::fs::write(&settings, br#"{"other": 1}"#).unwrap();
        assert_eq!(configured_offsite_snapshots(dir.path()), None, "key absent");
        std::fs::write(&settings, br#"{"backup_second_dir": "   "}"#).unwrap();
        assert_eq!(configured_offsite_snapshots(dir.path()), None, "blank value");
        std::fs::write(&settings, br#"{"backup_second_dir": "E:/backup root"}"#).unwrap();
        assert_eq!(configured_offsite_snapshots(dir.path()), Some(PathBuf::from("E:/backup root").join("snapshots")));
    }

    // ── Database probes ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn sqlite_check_surfaces_pragma_rows_and_refuses_malformed_pragmas() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert_eq!(sqlite_check(&db, "quick_check").unwrap(), ["ok"]);
        assert!(sqlite_check(&db, "quick_check(").unwrap_err().contains("cannot start"));
    }

    #[test]
    fn submission_idempotency_authority_reports_a_dropped_collision_trigger() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(submission_idempotency_authority(&db).unwrap());
        db.connection().execute_batch("DROP TRIGGER review_pool_decision_validate_insert;").unwrap();
        assert!(!submission_idempotency_authority(&db).unwrap());
    }

    #[test]
    fn submission_idempotency_authority_requires_the_unique_operation_id_index() {
        let db = Database::open(":memory:").unwrap();
        db.connection()
            .execute_batch(
                "CREATE TABLE review_pool_decisions (
                     id INTEGER PRIMARY KEY, pool_id TEXT, segment_id TEXT, reviewer TEXT,
                     operation_id TEXT, operation_payload_hash TEXT
                 );",
            )
            .unwrap();
        assert!(!submission_idempotency_authority(&db).unwrap());
    }

    // ── Pool-backed helpers ──────────────────────────────────────────────────────────────────────

    #[test]
    fn commit_benchmark_clip_loads_only_verified_human_decided_pool_members() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _pool) = pool_fixture(dir.path(), &[("a", true), ("b", false)]);
        let clip = commit_benchmark_clip(&db, "a").unwrap();
        assert_eq!(clip.segment_id, "a");
        assert_eq!(clip.raw_transcript, "دەقی چامپیۆن");
        assert_eq!(clip.audio_content_hash, clip_hash(0));
        assert_eq!((clip.source_start_ms, clip.source_end_ms, clip.duration_ms), (0, 1_000, 1_000));
        assert!(commit_benchmark_clip(&db, "b").unwrap_err().contains("cannot be loaded"), "unreviewed clip");
        assert!(commit_benchmark_clip(&db, "missing").unwrap_err().contains("cannot be loaded"));
    }

    #[test]
    fn commit_benchmark_worker_times_each_commit_and_reverses_it() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cortex-speech.db");
        let (db, pool) = pool_fixture(dir.path(), &[("clip", true)]);
        let clip = commit_benchmark_clip(&db, "clip").unwrap();
        let samples = commit_benchmark_worker(db, pool, clip, "CommitBenchA".to_string(), 3).unwrap();
        assert_eq!(samples.len(), 3);
        assert!(samples.iter().all(|sample| sample.is_finite() && *sample >= 0.0));
        let reopened = Database::open(&db_path.to_string_lossy()).unwrap();
        let count = |sql: &str| -> i64 { reopened.connection().query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM review_pool_decisions"), 3);
        assert_eq!(count("SELECT COUNT(*) FROM review_pool_reversals"), 3);
        assert_eq!(count("SELECT COUNT(*) FROM effective_review_pool_decisions_v62"), 0, "every commit reversed");
    }

    #[test]
    fn audio_coverage_counts_missing_recordings_by_voice() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _pool) = pool_fixture(dir.path(), &[("a", true), ("b", false)]);
        let all_present = audio_coverage(&db).unwrap();
        assert_eq!(all_present["recordings"], serde_json::json!(2));
        assert_eq!(all_present["clips"], serde_json::json!(2));
        assert_eq!(all_present["missingRecordings"], serde_json::json!(0));
        assert_eq!(all_present["allAvailable"], serde_json::json!(true));
        std::fs::remove_file(dir.path().join("b.wav")).unwrap();
        let one_missing = audio_coverage(&db).unwrap();
        assert_eq!(one_missing["missingRecordings"], serde_json::json!(1));
        assert_eq!(one_missing["missingClips"], serde_json::json!(1));
        assert_eq!(one_missing["missingClipsByVoice"], serde_json::json!({"Lamo": 1}));
        assert_eq!(one_missing["allAvailable"], serde_json::json!(false));
    }

    #[test]
    fn audio_coverage_never_calls_an_empty_pool_available() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let report = audio_coverage(&db).unwrap();
        assert_eq!(report["recordings"], serde_json::json!(0));
        assert_eq!(report["allAvailable"], serde_json::json!(false));
    }

    #[test]
    fn reviewer_voice_totals_counts_desktop_pool_and_skip_evidence_per_voice() {
        let dir = tempfile::tempdir().unwrap();
        let (db, pool) = pool_fixture(dir.path(), &[("clip", true)]);
        let hash = clip_hash(0);
        pool_decision(&db, &pool, "clip", &hash, "ReviewerC", "skip", None, 4_000_000);
        pool_decision(&db, &pool, "clip", &hash, "ReviewerB", "edit", Some("دەقی دروست"), 5_000_000);
        let rows = reviewer_voice_totals(&db).unwrap();
        assert_eq!(rows.len(), 3, "{rows:?}");
        for (row, key, judgments, skips) in
            [(&rows[0], "reviewera", 1, 0), (&rows[1], "reviewerb", 1, 0), (&rows[2], "reviewerc", 0, 1)]
        {
            assert_eq!(row["voiceName"], serde_json::json!("Lamo"));
            assert_eq!(row["reviewerKey"], serde_json::json!(key));
            assert_eq!(row["judgments"], serde_json::json!(judgments), "{key}");
            assert_eq!(row["skips"], serde_json::json!(skips), "{key}");
        }
    }

    // ── Inventory ────────────────────────────────────────────────────────────────────────────────

    fn library_db(rows: Vec<cortex_speech_app_lib::db::SpeechSegment>, hashes: &[(&str, &str)]) -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_champion(&db);
        insert_rows_at_v59(&db, &rows);
        for (id, hash) in hashes {
            db.connection()
                .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id=?2", rusqlite::params![hash, id])
                .unwrap();
        }
        db
    }

    fn library_row(id: &str, wav: &Path, span: Option<(i64, i64)>) -> cortex_speech_app_lib::db::SpeechSegment {
        let mut row = fixture_segment(id, wav, None);
        row.alignment_json = span.map(|(start, end)| format!(r#"{{"source_start_ms":{start},"source_end_ms":{end}}}"#));
        row
    }

    fn spec(name: &str, directory: &Path) -> VoiceSpec {
        VoiceSpec { name: name.to_string(), directory: directory.to_path_buf() }
    }

    fn write_wav(directory: &Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, b"wav").unwrap();
        path
    }

    #[test]
    fn inventory_reports_matched_missing_and_unusable_segments_per_voice() {
        let dir = tempfile::tempdir().unwrap();
        let healthy_dir = dir.path().join("healthy");
        let broken_dir = dir.path().join("broken");
        std::fs::create_dir_all(&healthy_dir).unwrap();
        std::fs::create_dir_all(&broken_dir).unwrap();
        let long_wav = write_wav(&healthy_dir, "long.wav");
        let matched_wav = write_wav(&broken_dir, "matched.wav");
        let wrong_model_wav = write_wav(&broken_dir, "wrong-model.wav");
        let placeholder_wav = write_wav(&broken_dir, "placeholder.wav");
        let orphan_wav = write_wav(&broken_dir, "orphan.wav");
        let mut wrong_model = library_row("wrong", &wrong_model_wav, Some((0, 1_000)));
        wrong_model.model_version_id = None;
        let mut placeholder = library_row("placeholder", &placeholder_wav, Some((0, 1_000)));
        placeholder.raw_transcript = "n/a".to_string();
        let db = library_db(
            vec![
                // One long prepared WAV legitimately split into two bounded, contiguous clips.
                library_row("long-1", &long_wav, Some((0, 1_000))),
                library_row("long-2", &long_wav, Some((1_000, 2_000))),
                library_row("matched", &matched_wav, Some((0, 1_000))),
                wrong_model,
                placeholder,
            ],
            &[("long-1", &clip_hash(0)), ("long-2", &clip_hash(0)), ("matched", &clip_hash(1))],
        );
        let (reports, members) = inventory(&db, &[spec("Kawa", &healthy_dir), spec("Lamo", &broken_dir)]).unwrap();
        assert_eq!(reports.len(), 2);

        let healthy = &reports[0];
        assert_eq!(healthy.voice_name, "Kawa");
        assert_eq!((healthy.disk_wavs, healthy.matched_files, healthy.matched_segments), (1, 1, 2));
        assert_eq!(healthy.usable_7b_segments, 2);
        assert!(voice_inventory_ready(healthy), "bounded clips of one long WAV are complete");

        let broken = &reports[1];
        assert_eq!((broken.disk_wavs, broken.matched_files, broken.matched_segments), (4, 3, 3));
        assert_eq!(broken.usable_7b_segments, 1);
        assert_eq!(broken.missing_files, [orphan_wav.to_string_lossy().to_string()]);
        // The migration chain backfills a NULL model id to the pre-registry marker, so the
        // non-champion row is reported under exactly that provenance.
        assert_eq!(broken.invalid_segments, ["placeholder:omniasr-7b-test-champion", "wrong:unknown@pre-registry"]);
        assert!(!voice_inventory_ready(broken));

        let member_ids: Vec<&str> = members.iter().map(|member| member.segment_id.as_str()).collect();
        assert_eq!(member_ids, ["long-1", "long-2", "matched"], "only usable segments become pool members");
    }

    #[test]
    fn inventory_refuses_usable_segments_without_activation_identity_and_empty_directories() {
        let dir = tempfile::tempdir().unwrap();
        let voice_dir = dir.path().join("kawa");
        std::fs::create_dir_all(&voice_dir).unwrap();
        let wav = write_wav(&voice_dir, "clip.wav");

        let no_hash = library_db(vec![library_row("clip", &wav, Some((0, 1_000)))], &[]);
        assert!(inventory(&no_hash, &[spec("Kawa", &voice_dir)])
            .unwrap_err()
            .contains("has no canonical audio-content hash"));

        let no_span = library_db(vec![library_row("clip", &wav, None)], &[("clip", &clip_hash(0))]);
        assert!(inventory(&no_span, &[spec("Kawa", &voice_dir)]).unwrap_err().contains("has no canonical source span"));

        let empty_dir = dir.path().join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let db = library_db(vec![library_row("clip", &wav, Some((0, 1_000)))], &[("clip", &clip_hash(0))]);
        assert!(inventory(&db, &[spec("Kawa", &empty_dir)]).unwrap_err().contains("contains no WAV files"));
    }

    #[test]
    fn inventory_binds_each_segment_to_one_voice_and_dedups_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let nested = parent.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let wav = write_wav(&nested, "clip.wav");
        let db = library_db(vec![library_row("clip", &wav, Some((0, 1_000)))], &[("clip", &clip_hash(0))]);

        let conflict = inventory(&db, &[spec("Kawa", &parent), spec("Lamo", &nested)]).unwrap_err();
        assert!(conflict.contains("appears in both voice Kawa and voice Lamo"), "{conflict}");

        // The same voice under a nested/overlapping prepared directory is one window, not a double.
        let (reports, members) = inventory(&db, &[spec("Kawa", &parent), spec("KAWA", &nested)]).unwrap();
        assert!(reports.iter().all(voice_inventory_ready));
        assert_eq!(members.len(), 1);
    }

    #[test]
    fn inventory_refuses_a_span_divergent_double_import_generation() {
        let dir = tempfile::tempdir().unwrap();
        let voice_dir = dir.path().join("kawa");
        std::fs::create_dir_all(&voice_dir).unwrap();
        let first = write_wav(&voice_dir, "gen-a.wav");
        let second = write_wav(&voice_dir, "gen-b.wav");
        // Two import generations of ONE recording (same content hash) cut at different spans.
        let db = library_db(
            vec![library_row("gen-a", &first, Some((0, 5_000))), library_row("gen-b", &second, Some((4_000, 9_000)))],
            &[("gen-a", &clip_hash(0)), ("gen-b", &clip_hash(0))],
        );
        let error = inventory(&db, &[spec("Kawa", &voice_dir)]).unwrap_err();
        assert!(error.contains("segments gen-a and gen-b cover overlapping audio"), "{error}");
        assert!(error.contains("servable and payable twice"), "{error}");
    }

    // ── Certification ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn certification_requires_an_active_pool() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let error =
            certification_outcome(&db, dir.path(), cortex_speech_app_lib::migrations::max_supported_version(), false)
                .unwrap_err();
        assert!(error.to_string().contains("review pool is not active"));
    }

    #[test]
    fn certification_gates_pass_individually_on_a_fully_reviewed_pool() {
        let dir = tempfile::tempdir().unwrap();
        let (db, pool) = pool_fixture(dir.path(), &[("clip", true)]);
        let hash = clip_hash(0);
        pool_decision(&db, &pool, "clip", &hash, "ReviewerC", "skip", None, 4_000_000);
        // Second distinct reviewer agrees with the desktop verdict: consensus resolution.
        pool_decision(&db, &pool, "clip", &hash, "ReviewerB", "edit", Some("دەقی دروست"), 5_000_000);
        review_pool::stamp_owner_supplied_pool_rights(&db).unwrap();
        bind_trivial_dedup_manifest(&db, &pool);
        cortex_speech_app_lib::snapshot::take_snapshot(&db, dir.path(), 3).unwrap().expect("snapshot must be taken");

        let outcome =
            certification_outcome(&db, dir.path(), cortex_speech_app_lib::migrations::max_supported_version(), true)
                .unwrap();
        let report = &outcome.report;
        assert_eq!(report["reportSchema"], serde_json::json!(3));
        assert_eq!(report["readOnly"], serde_json::json!(true));
        assert_eq!(
            report["databaseSchemaVersion"],
            serde_json::json!(cortex_speech_app_lib::migrations::max_supported_version())
        );
        assert_eq!(report["pool"]["poolId"], serde_json::json!(FIXTURE_POOL_ID));
        assert_eq!(report["pool"]["focusSegmentCount"], serde_json::json!(1));
        assert_eq!(report["database"]["quickCheck"], serde_json::json!(["ok"]));
        assert_eq!(report["database"]["fullIntegrityCheck"], serde_json::json!(["ok"]));
        assert_eq!(report["database"]["foreignKeyViolations"], serde_json::json!(0));
        assert_eq!(report["database"]["healthy"], serde_json::json!(true));
        assert_eq!(report["audio"]["allAvailable"], serde_json::json!(true));
        assert_eq!(report["dedup"]["applied"], serde_json::json!(true));
        assert_eq!(report["resolutionSummary"]["totalClips"], serde_json::json!(1));
        assert_eq!(report["resolutionSummary"]["resolvedClips"], serde_json::json!(1));
        assert_eq!(report["resolutionAuthority"]["consensusAgreements"], serde_json::json!(1));
        assert_eq!(report["resolutionAuthority"]["ownerAdjudications"], serde_json::json!(0));
        assert_eq!(report["voiceOutcomes"]["Lamo"]["total"], serde_json::json!(1));
        assert_eq!(report["voiceOutcomes"]["Lamo"]["retained"], serde_json::json!(1));
        assert_eq!(report["voiceOutcomes"]["Lamo"]["unresolved"], serde_json::json!(0));
        assert_eq!(report["voiceOutcomes"]["Lamo"]["certificate"], serde_json::Value::Null);
        assert_eq!(report["lastDecisionAtMs"], serde_json::json!(5_000_000));
        assert_eq!(report["snapshots"]["local"]["fresh"], serde_json::json!(true));
        assert_eq!(report["snapshots"]["offsite"], serde_json::json!({"configured": false, "fresh": false}));
        let totals = report["reviewerVoiceTotals"].as_array().unwrap();
        assert_eq!(totals.len(), 3);
        assert_eq!(totals[2]["reviewerKey"], serde_json::json!("reviewerc"));
        assert_eq!(totals[2]["skips"], serde_json::json!(1));

        assert_eq!(report["gates"]["duplicateExclusionsBound"], serde_json::json!(true));
        assert_eq!(report["gates"]["allClipsResolved"], serde_json::json!(true));
        assert_eq!(report["gates"]["rightsComplete"], serde_json::json!(true));
        assert_eq!(report["gates"]["everyVoiceCertified"], serde_json::json!(false), "no export certificate yet");
        // No offsite snapshot tree is configured, so review-readiness must refuse regardless of
        // how healthy everything else is.
        assert_eq!(report["gates"]["reviewReady"], serde_json::json!(false));
        assert_eq!(report["gates"]["finalDatasetReady"], serde_json::json!(false));
        assert!(!outcome.review_ready);
        assert!(!outcome.final_dataset_ready);
    }

    #[test]
    fn certification_flags_missing_pool_audio_and_unresolved_clips() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _pool) = pool_fixture(dir.path(), &[("a", true), ("b", false)]);
        std::fs::remove_file(dir.path().join("b.wav")).unwrap();
        let outcome =
            certification_outcome(&db, dir.path(), cortex_speech_app_lib::migrations::max_supported_version(), false)
                .unwrap();
        let report = &outcome.report;
        assert_eq!(report["database"]["healthy"], serde_json::json!(true));
        assert_eq!(report["database"]["fullIntegrityCheck"], serde_json::Value::Null);
        assert_eq!(report["audio"]["allAvailable"], serde_json::json!(false));
        assert_eq!(report["audio"]["missingRecordings"], serde_json::json!(1));
        assert_eq!(report["audio"]["missingClipsByVoice"], serde_json::json!({"Lamo": 1}));
        assert_eq!(report["resolutionSummary"]["totalClips"], serde_json::json!(2));
        assert_eq!(report["resolutionSummary"]["resolvedClips"], serde_json::json!(0));
        assert_eq!(report["resolutionSummary"]["needsFirstOrSecondReview"], serde_json::json!(2));
        assert_eq!(report["snapshots"]["local"]["fresh"], serde_json::json!(false));
        assert_eq!(report["gates"]["duplicateExclusionsBound"], serde_json::json!(false), "no dedup manifest bound");
        assert_eq!(report["gates"]["allClipsResolved"], serde_json::json!(false));
        assert_eq!(report["gates"]["rightsComplete"], serde_json::json!(false), "rights never stamped");
        assert_eq!(report["gates"]["reviewReady"], serde_json::json!(false));
        assert_eq!(report["gates"]["finalDatasetReady"], serde_json::json!(false));
        assert!(!outcome.review_ready);
        assert!(!outcome.final_dataset_ready);
    }
}
