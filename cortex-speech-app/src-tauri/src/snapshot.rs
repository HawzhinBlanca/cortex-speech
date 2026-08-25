//! P3.1 / M0.4b — rotating auto-snapshots of the app's irreplaceable state.
//!
//! The whole readiness plan hinges on weeks of the owner's review decisions accumulating in one
//! SQLite file. Manual `db_backup`/`db_restore` (M0.4) exist, but nothing took snapshots on its own —
//! so a single corruption event before the gold marathon could destroy everything. This module takes
//! a rotating snapshot (SQLite online backup, safe on a live DB) of `cortex-speech.db` plus the small
//! config state files, on start and periodically, keeping the newest N.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::Database;
use crate::error::{AppError, AppResult};

/// Small state files copied alongside the DB. A promoted rotating snapshot must contain this entire
/// recovery contract; the paid-review pilot policy is special-cased below because losing it silently
/// changes bounded paid work into unrestricted mode.
/// `champion.json` is the future retrain-champion pointer (M5) — harmless to list before it exists.
///
/// SINGLE SOURCE OF TRUTH: `restore_db_from_snapshot` (commands.rs) restores exactly this set, so the
/// save-side and restore-side can never drift — a file added here is automatically restored, not
/// silently snapshotted-but-never-restored.
/// `reviewer_dialects.json`, `voice_focus.json`, and `review_pilot_policy.json` are QUEUE/PAY POLICY,
/// and leaving them out made the
/// restore silently permissive: a MISSING policy file means "no restriction" (only a
/// present-but-broken one fails closed, owner instruction 2026-08-20), so a library restored
/// without them serves every reviewer every clip — the dialect fence gone and the collection focus
/// gone, with nothing in the UI to say so. Found 2026-08-20 by an external audit; the same restore
/// that proves the corpus survived would quietly undo who may review what.
#[cfg(test)]
pub(crate) const EXTRA_STATE: &[&str] =
    &["settings.json", "champion.json", "reviewer_dialects.json", "voice_focus.json", "review_pilot_policy.json"];

/// Files whose absence is a legitimate runtime state. A recovery artifact must nevertheless record
/// that absence explicitly: a missing file in a snapshot is otherwise indistinguishable from a
/// failed copy, and for the queue-routing files that ambiguity can widen paid review silently.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OptionalSnapshotState {
    pub live_file: &'static str,
    pub absent_file: &'static str,
    pub absent_bytes: &'static [u8],
}

pub(crate) const OPTIONAL_SNAPSHOT_STATE: &[OptionalSnapshotState] = &[
    OptionalSnapshotState {
        live_file: "settings.json",
        absent_file: "settings.json.absent",
        absent_bytes: b"cortex-snapshot-state-absent-v1:settings.json\n",
    },
    OptionalSnapshotState {
        live_file: "champion.json",
        absent_file: "champion.json.absent",
        absent_bytes: b"cortex-snapshot-state-absent-v1:champion.json\n",
    },
    OptionalSnapshotState {
        live_file: "reviewer_dialects.json",
        absent_file: "reviewer_dialects.json.absent",
        absent_bytes: b"cortex-snapshot-state-absent-v1:reviewer_dialects.json\n",
    },
    OptionalSnapshotState {
        live_file: "voice_focus.json",
        absent_file: "voice_focus.json.absent",
        absent_bytes: b"cortex-snapshot-state-absent-v1:voice_focus.json\n",
    },
];

const SNAPSHOT_PREFIX: &str = "snapshot_";
const DB_FILE: &str = "cortex-speech.db";
/// Rotation-exempt snapshots live under `<data_dir>/snapshots/pinned/` — the rolling prune only
/// touches `snapshot_*` dirs at the root, so nothing here is ever auto-evicted.
const PINNED_DIR: &str = "pinned";
/// Canonical per-snapshot inventory: size + SHA-256 of every file in the tree.
pub(crate) const MANIFEST_FILE: &str = "SNAPSHOT_MANIFEST.json";

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(serde::Deserialize)]
struct SnapshotPilotSession {
    db_path: String,
    reviewers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pilot_spot_checks: Vec<(String, String)>,
    pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
}

fn exact_pilot_reviewer(
    policy: &crate::review_pilot::ReviewPilotPolicy,
    actual: &str,
    source: &str,
) -> Result<String, String> {
    policy
        .reviewers
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(actual.trim()))
        .map(|entry| entry.name.clone())
        .ok_or_else(|| format!("{source} contains unauthorized reviewer {actual:?}"))
}

/// A policy-bearing v59 snapshot is restorable only when SQLite already owns every hidden key that
/// has escaped into a session or completion event. Zero is valid before traffic; missing history is
/// not. The whole current-schema fingerprint is checked here so a dropped/changed authority trigger
/// cannot be hidden by an otherwise healthy backup.
fn validate_active_pilot_snapshot_authority(
    connection: &rusqlite::Connection,
    live_data_dir: Option<&Path>,
    live_db_path: Option<&Path>,
    policy: &crate::review_pilot::ReviewPilotPolicy,
) -> Result<(), String> {
    let schema_version: i64 = connection
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |row| row.get(0))
        .map_err(|error| format!("snapshot hidden-key schema version cannot be read: {error}"))?;
    if schema_version < crate::review_pilot::REVIEW_PILOT_HIDDEN_KEYS_SCHEMA_VERSION {
        // The dedicated pre-migration pin is intentionally archival. Production named restore and
        // the external drill reject policy-bearing pre-v59 artifacts.
        return Ok(());
    }
    crate::db::validate_schema_contract_at_version(connection, schema_version)
        .map_err(|error| format!("snapshot hidden-key schema contract is not exact: {error}"))?;

    let names = policy.reviewer_names();
    if names.len() != 2 {
        return Err("snapshot pilot roster must contain exactly two distinct reviewers".to_string());
    }
    let digest = policy.policy_sha256()?;
    let baseline = policy.after_review_event_id;
    let maximum: i64 = connection
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("snapshot pilot review history cannot be read: {error}"))?;
    if baseline > maximum {
        return Err(format!("snapshot pilot baseline {baseline} is ahead of review-event maximum {maximum}"));
    }
    let conflicting: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM review_pilot_hidden_keys
              WHERE (policy_sha256 = ?1 OR after_review_event_id = ?2)
                AND NOT (policy_sha256 = ?1 AND after_review_event_id = ?2)",
            rusqlite::params![digest, baseline],
            |row| row.get(0),
        )
        .map_err(|error| format!("snapshot hidden-key namespace cannot be verified: {error}"))?;
    if conflicting != 0 {
        return Err(format!(
            "snapshot has {conflicting} hidden-key grant(s) that disagree with the active policy SHA/baseline"
        ));
    }

    let mut grants: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    let mut reviewer_counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut statement = connection
        .prepare(
            "SELECT reviewer, segment_id FROM review_pilot_hidden_keys
              WHERE policy_sha256 = ?1 AND after_review_event_id = ?2
              ORDER BY reviewer COLLATE NOCASE, segment_id",
        )
        .map_err(|error| format!("snapshot hidden-key grants cannot be read: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![digest, baseline], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| format!("snapshot hidden-key grants cannot be read: {error}"))?;
    for row in rows {
        let (actual_reviewer, segment_id) =
            row.map_err(|error| format!("snapshot hidden-key grant is unreadable: {error}"))?;
        let reviewer = exact_pilot_reviewer(policy, &actual_reviewer, "durable hidden-key authority")?;
        crate::validation::input::validate_identifier(&segment_id)
            .map_err(|error| format!("durable hidden-key authority has invalid segment {segment_id:?}: {error}"))?;
        let key = (reviewer.to_ascii_lowercase(), segment_id);
        if !grants.insert(key.clone()) {
            return Err(format!("snapshot hidden-key authority duplicates {}/{}", key.0, key.1));
        }
        *reviewer_counts.entry(key.0).or_default() += 1;
    }
    let per_reviewer = usize::try_from(crate::review_pilot::REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER)
        .map_err(|_| "snapshot hidden-key reviewer quota is invalid".to_string())?;
    let total = usize::try_from(crate::review_pilot::REVIEW_PILOT_TOTAL_HIDDEN_QC)
        .map_err(|_| "snapshot hidden-key global quota is invalid".to_string())?;
    if reviewer_counts.values().any(|count| *count > per_reviewer) || grants.len() > total {
        return Err("snapshot hidden-key authority exceeds the exact 2-per-reviewer/4-total quota".to_string());
    }

    let mut completed = std::collections::BTreeSet::new();
    let mut statement = connection
        .prepare(
            "SELECT id, segment_id, reviewer, action FROM review_events
              WHERE id > ?1 AND source = 'couch_spot_check' ORDER BY id",
        )
        .map_err(|error| format!("snapshot hidden completion history cannot be read: {error}"))?;
    let events = statement
        .query_map([baseline], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?))
        })
        .map_err(|error| format!("snapshot hidden completion history cannot be read: {error}"))?;
    for event in events {
        let (event_id, segment_id, actual_reviewer, action) =
            event.map_err(|error| format!("snapshot hidden completion event is unreadable: {error}"))?;
        let reviewer = exact_pilot_reviewer(policy, &actual_reviewer, &format!("hidden event {event_id}"))?;
        crate::validation::input::validate_identifier(&segment_id)
            .map_err(|error| format!("hidden event {event_id} has invalid segment: {error}"))?;
        if !matches!(action.as_str(), "accept" | "edit" | "reject" | "skip") {
            return Err(format!("hidden event {event_id} has invalid action {action:?}"));
        }
        let key = (reviewer.to_ascii_lowercase(), segment_id.clone());
        if !grants.contains(&key) {
            return Err(format!("hidden event {event_id} has no durable active-policy grant"));
        }
        if !completed.insert(key.clone()) {
            return Err(format!("hidden key {}/{} has multiple completion events", key.0, key.1));
        }
        let mut result_statement = connection
            .prepare("SELECT action FROM spot_checks WHERE segment_id = ?1 AND reviewer = ?2 COLLATE NOCASE")
            .map_err(|error| format!("hidden event {event_id} result cannot be read: {error}"))?;
        let observed = result_statement
            .query_map(rusqlite::params![segment_id, reviewer], |row| row.get::<_, String>(0))
            .map_err(|error| format!("hidden event {event_id} result cannot be read: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("hidden event {event_id} result cannot be read: {error}"))?;
        if observed != [action.clone()] {
            return Err(format!("hidden event {event_id} result mismatch: event={action:?}, results={observed:?}"));
        }
    }

    if let (Some(data_dir), Some(db_path)) = (live_data_dir, live_db_path) {
        let session_path = data_dir.join("couch_session.json");
        match fs::read(&session_path) {
            Ok(bytes) => {
                let session: SnapshotPilotSession = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("snapshot refused because couch_session.json is invalid: {error}"))?;
                let recorded = fs::canonicalize(&session.db_path)
                    .map_err(|error| format!("snapshot session database path is unavailable: {error}"))?;
                let expected = fs::canonicalize(db_path)
                    .map_err(|error| format!("snapshot database path is unavailable: {error}"))?;
                if recorded != expected {
                    return Err("snapshot session belongs to a different database".to_string());
                }
                let session_policy = session
                    .pilot_policy
                    .ok_or_else(|| "snapshot session is not bound to the active pilot policy".to_string())?;
                if session_policy.policy_sha256()? != digest {
                    return Err("snapshot session is bound to a different pilot policy".to_string());
                }
                let paired = session.reviewers.values().cloned().collect::<Vec<_>>();
                if !policy.matches_session(&paired) {
                    return Err("snapshot session reviewer roster does not match the active pilot".to_string());
                }
                let mut seen = std::collections::BTreeSet::new();
                for (segment_id, actual_reviewer) in session.pilot_spot_checks {
                    crate::validation::input::validate_identifier(&segment_id)
                        .map_err(|error| format!("snapshot session hidden key is invalid: {error}"))?;
                    let reviewer = exact_pilot_reviewer(policy, &actual_reviewer, "snapshot session hidden cache")?;
                    let key = (reviewer.to_ascii_lowercase(), segment_id);
                    if !seen.insert(key.clone()) {
                        return Err(format!("snapshot session duplicates hidden key {}/{}", key.0, key.1));
                    }
                    if !grants.contains(&key) {
                        return Err(format!(
                            "snapshot session hidden key {}/{} has no durable active-policy grant",
                            key.0, key.1
                        ));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("snapshot session cannot be read: {error}")),
        }
    }
    Ok(())
}

/// Capture paid-review policy with explicit absence semantics. A new snapshot contains exactly one
/// of the validated policy or an absence marker. An unreadable/invalid active policy fails the whole
/// snapshot instead of being misrepresented as intentionally absent.
fn capture_review_pilot_state(db: &Database, primary_data_dir: &Path, staging: &Path) -> AppResult<()> {
    let source = primary_data_dir.join(crate::review_pilot::REVIEW_PILOT_FILE);
    crate::atomic_file::recover_interrupted_replace(&source).map_err(|error| {
        AppError::Other(format!(
            "snapshot refused because interrupted {} recovery failed: {error}",
            crate::review_pilot::REVIEW_PILOT_FILE
        ))
    })?;
    match fs::read(&source) {
        Ok(bytes) => {
            let raw = std::str::from_utf8(&bytes)
                .map_err(|error| AppError::Other(format!("review pilot policy is not UTF-8: {error}")))?;
            let policy = crate::review_pilot::parse(raw).map_err(AppError::Other)?;
            crate::review_pilot::validate_controlled_focus(primary_data_dir).map_err(|error| {
                AppError::Other(format!(
                    "snapshot refused because the active controlled-pilot focus is not exact: {error}"
                ))
            })?;
            let max_event_id: i64 = db
                .connection()
                .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
                .map_err(AppError::Database)?;
            if policy.after_review_event_id > max_event_id {
                return Err(AppError::Other(format!(
                    "snapshot refused because {} baseline {} is ahead of the snapshotted database review-event maximum {max_event_id}",
                    crate::review_pilot::REVIEW_PILOT_FILE,
                    policy.after_review_event_id
                )));
            }
            validate_active_pilot_snapshot_authority(
                db.connection(),
                Some(primary_data_dir),
                Some(Path::new(db.path())),
                &policy,
            )
            .map_err(|error| {
                AppError::Other(format!("snapshot refused because hidden-key authority is incomplete: {error}"))
            })?;
            fs::write(staging.join(crate::review_pilot::REVIEW_PILOT_FILE), bytes).map_err(AppError::Io)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::write(
                staging.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE),
                crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES,
            )
            .map_err(AppError::Io)?;
        }
        Err(error) => {
            return Err(AppError::Other(format!(
                "snapshot refused because active {} could not be read: {error}",
                crate::review_pilot::REVIEW_PILOT_FILE
            )))
        }
    }
    Ok(())
}

/// Capture one of `{live bytes, explicit absence}` for every legally-optional config file.
/// Interrupted atomic replacements are recovered first so a crash cannot be mislabelled as an
/// intentional absence. Non-regular/unreadable sources fail the whole snapshot.
fn capture_optional_state(primary_data_dir: &Path, staging: &Path) -> AppResult<()> {
    for state in OPTIONAL_SNAPSHOT_STATE {
        let source = primary_data_dir.join(state.live_file);
        crate::atomic_file::recover_interrupted_replace(&source).map_err(|error| {
            AppError::Other(format!(
                "snapshot refused because interrupted {} recovery failed: {error}",
                state.live_file
            ))
        })?;
        match fs::symlink_metadata(&source) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                    return Err(AppError::Other(format!(
                        "snapshot refused because active {} is not a regular file",
                        state.live_file
                    )));
                }
                let bytes = fs::read(&source).map_err(|error| {
                    AppError::Other(format!(
                        "snapshot refused because active {} could not be read: {error}",
                        state.live_file
                    ))
                })?;
                fs::write(staging.join(state.live_file), bytes).map_err(AppError::Io)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::write(staging.join(state.absent_file), state.absent_bytes).map_err(AppError::Io)?;
            }
            Err(error) => {
                return Err(AppError::Other(format!(
                    "snapshot refused because active {} could not be inspected: {error}",
                    state.live_file
                )))
            }
        }
    }
    Ok(())
}

// ── Snapshot health (true-10 audit): the safety net must never fail silently for months. ────────
// take_snapshot records every outcome here; health_check surfaces it. A guard-skip (Ok(None)) is
// neither a success nor a failure — last_success simply ages, which is itself the honest signal.
static LAST_SUCCESS_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CONSECUTIVE_FAILURES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Point-in-time view of the auto-snapshot safety net for `health_check`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotHealth {
    /// Unix seconds of the last successful snapshot this run; `None` if none succeeded yet.
    pub last_success_epoch_secs: Option<u64>,
    pub consecutive_failures: usize,
}

pub fn snapshot_health() -> SnapshotHealth {
    let last = LAST_SUCCESS_EPOCH.load(std::sync::atomic::Ordering::Relaxed);
    SnapshotHealth {
        last_success_epoch_secs: (last > 0).then_some(last),
        consecutive_failures: CONSECUTIVE_FAILURES.load(std::sync::atomic::Ordering::Relaxed),
    }
}

/// Take a rotating snapshot into `<data_dir>/snapshots/snapshot_<ts>/`, then prune to newest `keep`.
/// Returns `Ok(None)` when the EMPTY-DB GUARD refuses the snapshot (see below) — a skip, not an error.
pub fn take_snapshot(db: &Database, data_dir: &Path, keep: usize) -> AppResult<Option<PathBuf>> {
    take_snapshot_with_quarantine_source(db, data_dir, data_dir, keep)
}

/// `take_snapshot` for a snapshot tree that does NOT live in the primary data dir (the
/// second-directory backup): `quarantine_dir` is where `*.corrupt.*` quarantine files actually
/// appear — the PRIMARY data dir. The quarantine prune-pin and the accumulation cap used to inspect
/// the snapshot tree's own parent, so during an unacknowledged corruption the OFF-DRIVE tree — the
/// copy that matters most in a corruption — kept pruning its pre-corruption history (round-24 hunt
/// #6). Health counters cover both trees, as before.
pub fn take_snapshot_with_quarantine_source(
    db: &Database,
    data_dir: &Path,
    quarantine_dir: &Path,
    keep: usize,
) -> AppResult<Option<PathBuf>> {
    let result = take_snapshot_at_from(db, data_dir, quarantine_dir, keep, now_secs());
    match &result {
        Ok(Some(_)) => {
            LAST_SUCCESS_EPOCH.store(now_secs(), std::sync::atomic::Ordering::Relaxed);
            CONSECUTIVE_FAILURES.store(0, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(None) => {}
        Err(_) => {
            CONSECUTIVE_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    result
}

/// Second-directory (off-drive) backup: the SAME snapshot + prune as the primary, but it MUST NOT touch
/// the shared health counters. The off-drive tree is a BONUS copy; letting its success reset the failure
/// streak — or its failure inflate it — would MASK the PRIMARY snapshot tree, the exact surface
/// `health_check` exists to protect. Round-25 hunt: a primary failure followed by an off-drive success
/// reset `consecutive_failures` to 0 and stamped `last_success`, so a silently-failing primary safety net
/// read as healthy for as long as the off-drive kept working. The off-drive's own failure is warn-logged
/// by the caller; health reflects the primary tree only.
pub fn take_offsite_snapshot(
    db: &Database,
    data_dir: &Path,
    quarantine_dir: &Path,
    keep: usize,
) -> AppResult<Option<PathBuf>> {
    validate_offsite_dir(data_dir, quarantine_dir)?;
    take_snapshot_at_from(db, data_dir, quarantine_dir, keep, now_secs())
}

/// Refuse an off-drive backup target that would not actually be off-drive.
///
/// `backup_second_dir` is free text typed into Settings and re-read every interval, and until now it
/// was handed straight to the snapshot writer. Two typos it could not survive:
///
/// * a RELATIVE path, which resolves against the process's working directory — not a place the owner
///   chose, and not the same place across launches;
/// * the primary data dir, or anything inside it, which puts the "off-drive" copy on the very disk
///   whose loss it exists to survive — and, when it lands inside `snapshots/`, makes each backup
///   copy the previous backups until the disk fills.
///
/// Wrong is fine and fixable; wrong while REPORTING a healthy second copy is the failure that costs
/// a corpus, so this fails loudly instead.
pub(crate) fn validate_offsite_dir(target: &Path, primary_data_dir: &Path) -> AppResult<()> {
    if !target.is_absolute() {
        return Err(crate::error::AppError::Validation(format!(
            "second-directory backup path must be absolute (got {}); a relative path follows the \
             process's working directory, not a drive you chose",
            target.display()
        )));
    }
    // Compare what we can: canonicalize resolves symlinks/8.3 names, but only for paths that already
    // exist. A target that does not exist yet is created by the snapshot itself, so fall back to the
    // literal path — the containment check is what matters either way.
    let resolved = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (target_abs, primary_abs) = (resolved(target), resolved(primary_data_dir));
    if target_abs == primary_abs || target_abs.starts_with(&primary_abs) {
        return Err(crate::error::AppError::Validation(format!(
            "second-directory backup path {} is inside the primary data directory {} — that is the \
             same disk it exists to survive, and it makes every backup copy the previous ones",
            target.display(),
            primary_data_dir.display()
        )));
    }
    Ok(())
}

/// Take a PINNED, rotation-exempt snapshot into `<data_dir>/snapshots/pinned/<label>_<ts>/`,
/// keeping only the newest `keep_pinned` snapshots that share `label`. Used for the two moments the
/// rolling ~100-minute window cannot cover (true-10 audit 2026-07-09): (a) BEFORE running pending
/// migrations — a semantically-buggy migration commits cleanly and is immediately re-snapshotted, so
/// without this the last pre-upgrade state rotates away within keep cycles of first launch; (b)
/// BEFORE a restore overwrites the live DB — a mis-restore of the wrong snapshot was otherwise
/// recoverable only from a ≤10-min-old rolling snapshot that itself rotates out.
pub fn take_pinned_snapshot(db: &Database, data_dir: &Path, label: &str, keep_pinned: usize) -> AppResult<PathBuf> {
    let _capture = crate::database_runtime::begin_snapshot_capture(data_dir).map_err(AppError::Other)?;
    take_pinned_snapshot_at(db, data_dir, label, keep_pinned, now_secs())
}

/// The mandatory pre-restore pin is the one intentional exception to normal capture admission: the
/// restore already owns its snapshot/restore gate and the DB mutex, so reacquiring it would deadlock.
/// The explicit reservation capability binds this bypass to the caller's live ownership lifetime;
/// ambient process-global state is not sufficient proof.
pub(crate) fn take_pinned_snapshot_during_restore(
    reservation: &crate::database_runtime::RestoreReservation<'_>,
    db: &Database,
    data_dir: &Path,
    label: &str,
    keep_pinned: usize,
) -> AppResult<PathBuf> {
    if !reservation.is_active() {
        return Err(AppError::Other(
            "pre-restore pinned snapshot requires an active exclusive restore reservation".to_string(),
        ));
    }
    take_pinned_snapshot_at(db, data_dir, label, keep_pinned, now_secs())
}

/// Initialize a database only after a durable, rotation-exempt copy of its pre-migration pages exists.
///
/// A schema version above zero proves this is an established profile rather than a pristine file. If
/// that version trails this binary, the safety pin is mandatory: a cleanly committed but semantically
/// wrong migration cannot be undone by SQLite's transaction, and post-migration rotating snapshots
/// would eventually evict every pre-upgrade copy. Both production database entry points call this one
/// helper so a future startup refactor cannot quietly restore warn-and-continue behavior.
pub fn initialize_with_required_pre_migration_pin(db: &Database, data_dir: &Path) -> AppResult<Option<PathBuf>> {
    let current = crate::migrations::get_current_version(db)?;
    let max_known = crate::migrations::max_supported_version();
    let pinned = if current > 0 && current < max_known {
        Some(take_pinned_snapshot(db, data_dir, &format!("premigration_v{current}_to_v{max_known}"), 3)?)
    } else {
        None
    };
    db.initialize()?;
    Ok(pinned)
}

/// `take_pinned_snapshot` with an explicit timestamp (testable without wall-clock sleeps).
pub(crate) fn take_pinned_snapshot_at(
    db: &Database,
    data_dir: &Path,
    label: &str,
    keep_pinned: usize,
    ts: u64,
) -> AppResult<PathBuf> {
    let pinned_root = data_dir.join("snapshots").join(PINNED_DIR);
    // Build in a STAGING dir (the '.' prefix keeps it out of every `{label}_` scan), then promote by
    // rename — a failed backup must never leave a partial dir that counts as a real pin, and a
    // second same-label pin in the same wall-clock second must never silently OVERWRITE the previous
    // pin's database (round-24 hunt #5/#7: create_dir_all succeeded on the existing dir and
    // db.backup clobbered it — destroying the very state the pin existed to preserve).
    sweep_stale_staging_dirs(&pinned_root);
    let staging = pinned_root.join(format!("{STAGING_PREFIX}{label}_{ts:010}"));
    fs::create_dir_all(&staging).map_err(AppError::Io)?;
    if let Err(e) = db.backup(staging.join(DB_FILE)) {
        remove_staging_dir(&staging);
        return Err(e);
    }
    if let Err(error) = capture_optional_state(data_dir, &staging) {
        remove_staging_dir(&staging);
        return Err(error);
    }
    if let Err(error) = capture_review_pilot_state(db, data_dir, &staging) {
        remove_staging_dir(&staging);
        return Err(error);
    }
    if let Err(error) = write_snapshot_manifest(&staging, ts) {
        remove_staging_dir(&staging);
        return Err(error);
    }
    match verify_snapshot_manifest_for_restore(&staging) {
        Ok(true) => {}
        Ok(false) => {
            remove_staging_dir(&staging);
            return Err(AppError::Other("new pinned snapshot staging unexpectedly has no manifest".to_string()));
        }
        Err(error) => {
            remove_staging_dir(&staging);
            return Err(AppError::Other(format!(
                "pinned snapshot refused before promotion because its recovery contract is incomplete: {error}"
            )));
        }
    }
    // Promote under the first FREE timestamped name — a same-second sibling bumps forward instead of
    // being overwritten.
    let mut final_ts = ts;
    let mut snap_dir = pinned_root.join(format!("{label}_{final_ts:010}"));
    while snap_dir.exists() {
        final_ts += 1;
        snap_dir = pinned_root.join(format!("{label}_{final_ts:010}"));
    }
    if let Err(e) = fs::rename(&staging, &snap_dir) {
        remove_staging_dir(&staging);
        return Err(AppError::Io(e));
    }
    // Bound same-label accumulation (newest keep_pinned survive) so repeated upgrades/restores
    // can't grow without limit; different labels never evict each other.
    let mut same_label: Vec<PathBuf> = fs::read_dir(&pinned_root)
        .map_err(AppError::Io)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&format!("{label}_")))
        })
        .collect();
    same_label.sort();
    while same_label.len() > keep_pinned.max(1) {
        let oldest = same_label.remove(0);
        if let Err(e) = fs::remove_dir_all(&oldest) {
            tracing::warn!("pinned snapshot: could not prune {}: {e}", oldest.display());
        }
    }
    Ok(snap_dir)
}

/// `take_snapshot` with an explicit timestamp (testable without same-second collisions). Test-only
/// convenience: production routes through `take_snapshot_at_from` so the quarantine dir is explicit.
#[cfg(test)]
pub(crate) fn take_snapshot_at(db: &Database, data_dir: &Path, keep: usize, ts: u64) -> AppResult<Option<PathBuf>> {
    seed_test_required_snapshot_state(data_dir)?;
    take_snapshot_at_from(db, data_dir, data_dir, keep, ts)
}

#[cfg(test)]
fn seed_test_required_snapshot_state(data_dir: &Path) -> AppResult<()> {
    fs::create_dir_all(data_dir).map_err(AppError::Io)?;
    let settings = serde_json::to_vec_pretty(&crate::settings::AppSettings::default())
        .map_err(|error| AppError::Other(format!("test settings serialize: {error}")))?;
    for (name, bytes) in [
        ("settings.json", settings.as_slice()),
        ("champion.json", br#"{"schema":2,"champions":{}}"#.as_slice()),
        ("reviewer_dialects.json", b"{}".as_slice()),
        ("voice_focus.json", br#"{"name":"test","segment_ids":["test-segment"]}"#.as_slice()),
    ] {
        let path = data_dir.join(name);
        if !path.exists() {
            fs::write(path, bytes).map_err(AppError::Io)?;
        }
    }
    Ok(())
}

/// `dest_dir` is where the snapshot TREE is written; `primary_data_dir` is the live library the
/// state files are read from. For a local snapshot they are the same directory. For the off-drive
/// copy they are NOT: the destination is the owner's second disk, which holds no `settings.json` or
/// `champion.json` of its own.
///
/// Measured 2026-08-19: this function read `EXTRA_STATE` from the DESTINATION, so every off-drive
/// snapshot silently contained the database and nothing else — the copy is best-effort, so the two
/// missing files only produced a debug-level warning. A restore from that tree would come back with
/// no champion pointer (the app then serves NO champion at all) and no settings, which is precisely
/// the disaster the off-drive copy exists to survive.
pub(crate) fn take_snapshot_at_from(
    db: &Database,
    dest_dir: &Path,
    primary_data_dir: &Path,
    keep: usize,
    ts: u64,
) -> AppResult<Option<PathBuf>> {
    // Serialize the complete DB+config capture against named restores. The guard is held until the
    // staging tree is either promoted or removed, so no restore generation can be mixed with config
    // from another generation. It also refuses a durable marker left by an interrupted restore.
    let _capture = crate::database_runtime::begin_snapshot_capture(primary_data_dir).map_err(AppError::Other)?;
    let root = dest_dir.join("snapshots");

    // THE EMPTY-DB GUARD (B2, true-10 audit blocker): after a corruption quarantine the app opens a
    // FRESH EMPTY database. Snapshotting that empty DB every cycle would rotate out (keep=N) every
    // pre-corruption snapshot within N cycles — the safety net destroying the only good copies of
    // weeks of review labor. An empty library never needs a fresh snapshot when history exists, so:
    // zero segments + at least one prior snapshot => refuse (skip, log loudly). The first-run case
    // (zero segments, NO prior snapshots) still snapshots normally.
    if db.segment_count().unwrap_or(0) == 0 && has_any_snapshot(&root) {
        tracing::warn!(
            "snapshot: live DB has 0 segments but prior snapshots exist — refusing to snapshot (and rotate) \
             so pre-corruption history is never evicted by an empty library"
        );
        return Ok(None);
    }

    // ACCUMULATION CAP while a quarantine pins pruning (true-10 audit 2026-07-09): the prune-pin
    // below is correct, but with snapshots resuming after a re-import (segment_count > 0) it meant a
    // full DB copy every cycle, unbounded (~144/day), until disk pressure. History is already frozen
    // by the pin — additional copies beyond 2×keep add no protection, so stop taking new ones.
    if has_unacknowledged_quarantine(primary_data_dir) {
        let existing = fs::read_dir(&root)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir() && e.file_name().to_str().is_some_and(|n| n.starts_with(SNAPSHOT_PREFIX)))
            .count();
        if existing >= keep.saturating_mul(2).max(2) {
            tracing::warn!(
                "snapshot: quarantine pin active and {existing} snapshots already held — skipping new \
                 snapshots until the quarantine is acknowledged (history is pinned; more copies add nothing)"
            );
            return Ok(None);
        }
    }

    // Build in a STAGING dir, promote by rename (round-24 hunt #5): a failed db.backup used to leave
    // a `snapshot_<ts>` dir holding a partial database, and that garbage dir then counted as a REAL
    // snapshot everywhere — has_any_snapshot (arming the empty-DB guard against a legitimate first
    // snapshot), the prune keep-set (occupying a slot and evicting a good older snapshot), and the
    // quarantine accumulation cap. The '.' prefix keeps staging out of every SNAPSHOT_PREFIX scan.
    sweep_stale_staging_dirs(&root);
    let staging = root.join(format!("{STAGING_PREFIX}{SNAPSHOT_PREFIX}{ts:010}"));
    fs::create_dir_all(&staging).map_err(AppError::Io)?;

    // The DB is the critical artifact — its failure fails the snapshot (and removes the staging dir).
    if let Err(e) = db.backup(staging.join(DB_FILE)) {
        remove_staging_dir(&staging);
        return Err(e);
    }

    // A promoted rotating snapshot is a complete recovery artifact. Paid-review policy is handled
    // separately because it must be either a validated copy or an explicit absence marker.
    if let Err(error) = capture_optional_state(primary_data_dir, &staging) {
        remove_staging_dir(&staging);
        return Err(error);
    }
    if let Err(error) = capture_review_pilot_state(db, primary_data_dir, &staging) {
        remove_staging_dir(&staging);
        return Err(error);
    }

    if let Err(e) = write_snapshot_manifest(&staging, ts) {
        remove_staging_dir(&staging);
        return Err(e);
    }
    match verify_snapshot_manifest_for_restore(&staging) {
        Ok(true) => {}
        Ok(false) => {
            remove_staging_dir(&staging);
            return Err(AppError::Other("new snapshot staging unexpectedly has no manifest".to_string()));
        }
        Err(error) => {
            remove_staging_dir(&staging);
            return Err(AppError::Other(format!(
                "snapshot refused before promotion because its recovery contract is incomplete: {error}"
            )));
        }
    }

    let snap_dir = root.join(format!("{SNAPSHOT_PREFIX}{ts:010}"));
    if let Err(e) = fs::rename(&staging, &snap_dir) {
        remove_staging_dir(&staging);
        return Err(AppError::Io(e));
    }

    prune_snapshots_from(&root, primary_data_dir, keep)?;
    Ok(Some(snap_dir))
}

/// Write the canonical inventory of a finished snapshot: size and SHA-256 of every file in it.
///
/// Without this, "the off-drive copy exists" is the strongest claim available — and that claim was
/// true while the off-drive tree silently held the database alone (2026-08-19). A manifest makes the
/// two copies COMPARABLE: identical required hashes, or a restore that must not be trusted.
///
/// Written into staging BEFORE the promoting rename, so a promoted snapshot always has one and a
/// crashed one is discarded whole.
fn write_snapshot_manifest(staging: &Path, ts: u64) -> AppResult<()> {
    let mut files: Vec<(String, u64, String)> = Vec::new();
    let mut entries: Vec<PathBuf> =
        fs::read_dir(staging).map_err(AppError::Io)?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    entries.sort();
    for path in entries {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) if name != MANIFEST_FILE => name.to_string(),
            _ => continue,
        };
        let size = path.metadata().map_err(AppError::Io)?.len();
        let digest = crate::models::compute_file_sha256(&path)
            .map_err(|e| AppError::Other(format!("snapshot manifest hash for {name}: {e}")))?;
        files.push((name, size, digest));
    }
    let payload = serde_json::json!({
        "schema": 1,
        "reviewPilotPolicyStateSchema": 1,
        "createdAtEpochSecs": ts,
        "appGitSha": crate::GIT_SHA,
        "files": files
            .iter()
            .map(|(name, size, digest)| serde_json::json!({"path": name, "sizeBytes": size, "sha256": digest}))
            .collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&payload)
        .map_err(|e| AppError::Other(format!("snapshot manifest serialize: {e}")))?;
    fs::write(staging.join(MANIFEST_FILE), text.as_bytes()).map_err(AppError::Io)?;
    Ok(())
}

/// Verify every byte promised by a snapshot manifest before a restore can touch the live database.
/// `Ok(false)` is reserved for a truly legacy tree with NO manifest. Once a manifest exists, malformed
/// JSON, duplicates, traversal, missing required state, and any size/hash mismatch are hard failures.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotManifestFile {
    path: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotManifestV1 {
    schema: u64,
    review_pilot_policy_state_schema: u64,
    created_at_epoch_secs: u64,
    app_git_sha: String,
    files: Vec<SnapshotManifestFile>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotManifestV2 {
    schema: u64,
    created_at_epoch_secs: u64,
    app_git_sha: String,
    source_data_dir: String,
    database_evidence: SnapshotDatabaseEvidence,
    files: Vec<SnapshotManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotDatabaseEvidence {
    quick_check: Vec<String>,
    integrity_check: Vec<String>,
    foreign_key_violation_count: u64,
    schema_version: u64,
    row_counts: SnapshotRowCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRowCounts {
    speech_segments: u64,
    review_events: u64,
    spot_checks: u64,
    model_versions: u64,
    import_jobs: u64,
    import_job_files: u64,
    #[serde(default)]
    review_pilot_hidden_keys: Option<u64>,
    #[serde(default)]
    review_campaign_registry: Option<u64>,
    #[serde(default)]
    review_campaign_focus: Option<u64>,
    #[serde(default)]
    review_campaign_transitions: Option<u64>,
    #[serde(default)]
    independent_review_decisions: Option<u64>,
    #[serde(default)]
    independent_review_reversals: Option<u64>,
    #[serde(default)]
    review_campaign_adjudications: Option<u64>,
    #[serde(default)]
    review_pool_registry: Option<u64>,
    #[serde(default)]
    review_pool_members: Option<u64>,
    #[serde(default)]
    review_pool_decisions: Option<u64>,
    #[serde(default)]
    review_pool_reversals: Option<u64>,
    #[serde(default)]
    review_pool_owner_adjudications: Option<u64>,
    #[serde(default)]
    review_pool_voice_certificates: Option<u64>,
    #[serde(default)]
    review_pool_dedup_manifests: Option<u64>,
    #[serde(default)]
    review_pool_duplicate_exclusions: Option<u64>,
}

fn safe_manifest_name(name: &str) -> Result<(), String> {
    let mut components = Path::new(name).components();
    let reserved = name.split('.').next().map(str::to_ascii_lowercase).is_some_and(|base| {
        matches!(base.as_str(), "con" | "prn" | "aux" | "nul")
            || (base.len() == 4
                && (base.starts_with("com") || base.starts_with("lpt"))
                && matches!(base.as_bytes()[3], b'1'..=b'9'))
    });
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.eq_ignore_ascii_case(MANIFEST_FILE)
        || !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
        || name.contains(['/', '\\'])
        || name.chars().any(|ch| ch.is_control() || "<>:\"|?*".contains(ch))
        || name.ends_with([' ', '.'])
        || reserved
    {
        return Err(format!("snapshot manifest contains unsafe file path '{name}'"));
    }
    Ok(())
}

fn read_pragma_strings(connection: &rusqlite::Connection, pragma: &str) -> Result<Vec<String>, String> {
    let mut statement = connection.prepare(pragma).map_err(|error| format!("snapshot DB evidence failed: {error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("snapshot DB evidence failed: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|error| format!("snapshot DB evidence failed: {error}"))
}

fn inspect_schema2_database_evidence(path: &Path) -> Result<SnapshotDatabaseEvidence, String> {
    let source = crate::db::Database::open_immutable_connection(path)
        .map_err(|error| format!("schema-2 snapshot database could not be opened immutable: {error}"))?;
    let mut connection = rusqlite::Connection::open_in_memory()
        .map_err(|error| format!("schema-2 evidence staging could not be opened: {error}"))?;
    {
        let backup = rusqlite::backup::Backup::new(&source, &mut connection)
            .map_err(|error| format!("schema-2 evidence staging failed: {error}"))?;
        backup
            .run_to_completion(4096, std::time::Duration::from_millis(1), None)
            .map_err(|error| format!("schema-2 evidence staging failed: {error}"))?;
    }
    let nonnegative = |table: &str| -> Result<u64, String> {
        let value: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| row.get(0))
            .map_err(|error| format!("schema-2 snapshot database could not count {table}: {error}"))?;
        u64::try_from(value).map_err(|_| format!("schema-2 snapshot database has a negative count for {table}"))
    };
    let foreign_key_violation_count = {
        let mut statement = connection
            .prepare("PRAGMA foreign_key_check")
            .map_err(|error| format!("schema-2 snapshot foreign-key evidence failed: {error}"))?;
        let mut rows =
            statement.query([]).map_err(|error| format!("schema-2 snapshot foreign-key evidence failed: {error}"))?;
        let mut count = 0u64;
        while rows.next().map_err(|error| format!("schema-2 snapshot foreign-key evidence failed: {error}"))?.is_some()
        {
            count = count.saturating_add(1);
        }
        count
    };
    let schema_version: i64 = connection
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |row| row.get(0))
        .map_err(|error| format!("schema-2 snapshot migration history is unavailable: {error}"))?;
    let count_from = |introduced_in: i64, table: &str| -> Result<Option<u64>, String> {
        if schema_version >= introduced_in {
            nonnegative(table).map(Some)
        } else {
            Ok(None)
        }
    };
    Ok(SnapshotDatabaseEvidence {
        quick_check: read_pragma_strings(&connection, "PRAGMA quick_check")?,
        integrity_check: read_pragma_strings(&connection, "PRAGMA integrity_check")?,
        foreign_key_violation_count,
        schema_version: u64::try_from(schema_version)
            .map_err(|_| "schema-2 snapshot has a negative migration version".to_string())?,
        row_counts: SnapshotRowCounts {
            speech_segments: nonnegative("speech_segments")?,
            review_events: nonnegative("review_events")?,
            spot_checks: nonnegative("spot_checks")?,
            model_versions: nonnegative("model_versions")?,
            import_jobs: nonnegative("import_jobs")?,
            import_job_files: nonnegative("import_job_files")?,
            review_pilot_hidden_keys: count_from(59, "review_pilot_hidden_keys")?,
            review_campaign_registry: count_from(61, "review_campaign_registry")?,
            review_campaign_focus: count_from(61, "review_campaign_focus")?,
            review_campaign_transitions: count_from(61, "review_campaign_transitions")?,
            independent_review_decisions: count_from(61, "independent_review_decisions")?,
            independent_review_reversals: count_from(61, "independent_review_reversals")?,
            review_campaign_adjudications: count_from(61, "review_campaign_adjudications")?,
            review_pool_registry: count_from(62, "review_pool_registry")?,
            review_pool_members: count_from(62, "review_pool_members")?,
            review_pool_decisions: count_from(62, "review_pool_decisions")?,
            review_pool_reversals: count_from(62, "review_pool_reversals")?,
            review_pool_owner_adjudications: count_from(63, "review_pool_owner_adjudications")?,
            review_pool_voice_certificates: count_from(63, "review_pool_voice_certificates")?,
            review_pool_dedup_manifests: count_from(64, "review_pool_dedup_manifests")?,
            review_pool_duplicate_exclusions: count_from(64, "review_pool_duplicate_exclusions")?,
        },
    })
}

fn validate_champion_pointer(bytes: &[u8]) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| format!("champion.json is invalid JSON: {error}"))?;
    let root = value.as_object().ok_or_else(|| "champion.json must be an object".to_string())?;
    if root.len() != 2 || !root.contains_key("schema") || !root.contains_key("champions") {
        return Err("champion.json must contain exactly schema and champions".to_string());
    }
    if root.get("schema").and_then(serde_json::Value::as_u64) != Some(2) {
        return Err("champion.json schema must be exactly 2".to_string());
    }
    let champions = root
        .get("champions")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "champion.json champions must be an object".to_string())?;
    let expected: std::collections::HashSet<&str> =
        ["modelVersionId", "deploymentManifestPath", "deploymentSha256", "source", "license"].into_iter().collect();
    for (family, entry) in champions {
        if family.trim().is_empty() {
            return Err("champion.json contains an empty family name".to_string());
        }
        let object = entry.as_object().ok_or_else(|| format!("champion.json family '{family}' must be an object"))?;
        if object.keys().map(String::as_str).collect::<std::collections::HashSet<_>>() != expected {
            return Err(format!("champion.json family '{family}' has an invalid field set"));
        }
        if object.values().any(|value| !value.is_string()) {
            return Err(format!("champion.json family '{family}' fields must all be strings"));
        }
    }
    Ok(())
}

pub(crate) fn validate_present_optional_state(name: &str, bytes: &[u8]) -> Result<(), String> {
    match name {
        "settings.json" => crate::settings::AppSettings::parse_recovery_bytes(bytes).map(|_| ()),
        "champion.json" => validate_champion_pointer(bytes),
        "reviewer_dialects.json" => {
            let text =
                std::str::from_utf8(bytes).map_err(|error| format!("reviewer_dialects.json is not UTF-8: {error}"))?;
            crate::dialect::parse_roster_text(text).map(|_| ())
        }
        "voice_focus.json" => {
            let text = std::str::from_utf8(bytes).map_err(|error| format!("voice_focus.json is not UTF-8: {error}"))?;
            crate::voice_focus::parse_focus_text(text).map(|_| ())
        }
        _ => Err(format!("unknown optional snapshot state '{name}'")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptionalSnapshotRestore {
    Install(Vec<u8>),
    ExplicitlyAbsent,
    /// Manifestless historical trees cannot prove whether a missing file was intentional. Preserve
    /// the current live state rather than silently widening/removing a routing policy.
    PreserveLegacy,
}

pub(crate) fn inspect_optional_state_for_restore(
    snapshot_dir: &Path,
    state: OptionalSnapshotState,
    manifest_verified: bool,
) -> Result<OptionalSnapshotRestore, String> {
    let live = snapshot_dir.join(state.live_file);
    let absent = snapshot_dir.join(state.absent_file);
    let read_optional = |path: &Path| -> Result<Option<Vec<u8>>, String> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("snapshot state {} is unreadable: {error}", path.display())),
        }
    };
    match (read_optional(&live)?, read_optional(&absent)?) {
        (Some(_), Some(_)) => {
            Err(format!("snapshot is ambiguous: it contains both {} and {}", state.live_file, state.absent_file))
        }
        (Some(bytes), None) => {
            validate_present_optional_state(state.live_file, &bytes)?;
            Ok(OptionalSnapshotRestore::Install(bytes))
        }
        (None, Some(marker)) => {
            if marker != state.absent_bytes {
                return Err(format!("snapshot {} has invalid contents", state.absent_file));
            }
            Ok(OptionalSnapshotRestore::ExplicitlyAbsent)
        }
        (None, None) if manifest_verified => {
            Err(format!("manifest-bearing snapshot is missing both {} and {}", state.live_file, state.absent_file))
        }
        (None, None) => {
            tracing::warn!(
                "LEGACY MANIFEST-LESS SNAPSHOT: neither {} nor {} is present; preserving current live state",
                state.live_file,
                state.absent_file
            );
            Ok(OptionalSnapshotRestore::PreserveLegacy)
        }
    }
}

/// Verify the complete recovery contract without mutating the snapshot tree.
///
/// Public so the read-only private-production certifier can reject a fresh-looking directory whose
/// manifest, database evidence, or file hashes no longer verify. Restore uses this same authority.
pub fn verify_snapshot_manifest_for_restore(snapshot_dir: &Path) -> Result<bool, String> {
    let manifest_path = snapshot_dir.join(MANIFEST_FILE);
    let raw = match fs::read(&manifest_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("snapshot manifest is unreadable: {error}")),
    };
    let probe: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|error| format!("snapshot manifest is invalid JSON: {error}"))?;
    let schema = probe
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "snapshot manifest schema must be exactly integer 1 or 2".to_string())?;
    let (files, database_evidence) = match schema {
        1 => {
            let manifest: SnapshotManifestV1 = serde_json::from_slice(&raw)
                .map_err(|error| format!("snapshot schema-1 manifest contract is invalid: {error}"))?;
            if manifest.schema != 1 || manifest.review_pilot_policy_state_schema != 1 {
                return Err("snapshot schema-1 manifest policy-state schema must be exactly 1".to_string());
            }
            if manifest.app_git_sha.is_empty() {
                return Err("snapshot manifest appGitSha must be non-empty".to_string());
            }
            let _ = manifest.created_at_epoch_secs;
            (manifest.files, None)
        }
        2 => {
            let manifest: SnapshotManifestV2 = serde_json::from_slice(&raw)
                .map_err(|error| format!("snapshot schema-2 manifest contract is invalid: {error}"))?;
            if manifest.schema != 2 || manifest.app_git_sha.is_empty() || manifest.source_data_dir.is_empty() {
                return Err("snapshot schema-2 manifest identity fields are invalid".to_string());
            }
            let _ = manifest.created_at_epoch_secs;
            (manifest.files, Some(manifest.database_evidence))
        }
        _ => return Err("snapshot manifest schema must be exactly integer 1 or 2".to_string()),
    };

    let mut declared = std::collections::BTreeMap::<String, SnapshotManifestFile>::new();
    let mut declared_folded = std::collections::HashSet::new();
    for row in files {
        safe_manifest_name(&row.path)?;
        let folded = row.path.to_lowercase();
        if !declared_folded.insert(folded) || declared.insert(row.path.clone(), row).is_some() {
            return Err("snapshot manifest contains a duplicate/case-colliding file".to_string());
        }
    }
    let mut actual = std::collections::BTreeMap::<String, PathBuf>::new();
    let mut actual_folded = std::collections::HashSet::new();
    let entries = fs::read_dir(snapshot_dir).map_err(|error| format!("snapshot directory is unreadable: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("snapshot directory entry is unreadable: {error}"))?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| "snapshot contains a non-UTF-8 file name".to_string())?
            .to_string();
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("snapshot file '{name}' is unreadable: {error}"))?;
        if name == MANIFEST_FILE {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err("snapshot manifest must be a regular, non-symlink file".to_string());
            }
            continue;
        }
        safe_manifest_name(&name)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(format!("snapshot file '{name}' must be a regular, non-symlink file"));
        }
        if !actual_folded.insert(name.to_lowercase()) || actual.insert(name.clone(), entry.path()).is_some() {
            return Err(format!("snapshot tree contains a duplicate/case-colliding file '{name}'"));
        }
    }
    let declared_names = declared.keys().cloned().collect::<std::collections::BTreeSet<_>>();
    let actual_names = actual.keys().cloned().collect::<std::collections::BTreeSet<_>>();
    if declared_names != actual_names {
        let missing = declared_names.difference(&actual_names).cloned().collect::<Vec<_>>();
        let unlisted = actual_names.difference(&declared_names).cloned().collect::<Vec<_>>();
        return Err(format!("snapshot manifest inventory is not exact (missing={missing:?}, unlisted={unlisted:?})"));
    }
    for (name, row) in &declared {
        if row.sha256.len() != 64
            || !row.sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!("snapshot manifest SHA-256 for '{name}' must be 64 lowercase hex digits"));
        }
        let path = &actual[name];
        let metadata = fs::metadata(path).map_err(|error| format!("snapshot file '{name}' is unreadable: {error}"))?;
        if metadata.len() != row.size_bytes {
            return Err(format!(
                "snapshot manifest size mismatch for '{name}': expected {}, got {}",
                row.size_bytes,
                metadata.len()
            ));
        }
        let actual_sha = crate::models::compute_file_sha256(path)
            .map_err(|error| format!("snapshot manifest could not hash '{name}': {error}"))?;
        if actual_sha != row.sha256 {
            return Err(format!("snapshot manifest SHA-256 mismatch for '{name}'"));
        }
    }

    if !declared.contains_key(DB_FILE) {
        return Err(format!("snapshot manifest is incomplete: missing required '{DB_FILE}'"));
    }
    for state in OPTIONAL_SNAPSHOT_STATE {
        let present = declared.contains_key(state.live_file);
        let absent = declared.contains_key(state.absent_file);
        if present == absent {
            return Err(format!(
                "snapshot manifest must contain exactly one of {} or {}",
                state.live_file, state.absent_file
            ));
        }
        if present {
            validate_present_optional_state(
                state.live_file,
                &fs::read(&actual[state.live_file]).map_err(|error| {
                    format!("snapshot state {} is unreadable after hash verification: {error}", state.live_file)
                })?,
            )?;
        } else if fs::read(&actual[state.absent_file])
            .map_err(|error| format!("snapshot absence marker {} is unreadable: {error}", state.absent_file))?
            != state.absent_bytes
        {
            return Err(format!("snapshot absence marker {} has invalid contents", state.absent_file));
        }
    }
    let policy_present = declared.contains_key(crate::review_pilot::REVIEW_PILOT_FILE);
    let absence_present = declared.contains_key(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE);
    if policy_present == absence_present {
        return Err(format!(
            "snapshot manifest must contain exactly one of {} or {}",
            crate::review_pilot::REVIEW_PILOT_FILE,
            crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE
        ));
    }
    if policy_present {
        let bytes = fs::read(&actual[crate::review_pilot::REVIEW_PILOT_FILE])
            .map_err(|error| format!("snapshot pilot policy is unreadable: {error}"))?;
        let text =
            std::str::from_utf8(&bytes).map_err(|error| format!("snapshot pilot policy is not UTF-8: {error}"))?;
        let policy = crate::review_pilot::parse(text)?;
        crate::review_pilot::validate_controlled_focus(snapshot_dir)
            .map_err(|error| format!("snapshot controlled-pilot focus is invalid: {error}"))?;
        let connection = crate::db::Database::open_immutable_connection(&actual[DB_FILE])
            .map_err(|error| format!("snapshot pilot policy could not bind to its database: {error}"))?;
        let max_event_id: i64 = connection
            .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
            .map_err(|error| format!("snapshot pilot baseline could not be verified: {error}"))?;
        if policy.after_review_event_id > max_event_id {
            return Err(format!(
                "snapshot pilot baseline {} is ahead of its database review-event maximum {max_event_id}",
                policy.after_review_event_id
            ));
        }
        validate_active_pilot_snapshot_authority(&connection, None, None, &policy)
            .map_err(|error| format!("snapshot pilot hidden-key authority is incomplete or incoherent: {error}"))?;
    } else if fs::read(&actual[crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE])
        .map_err(|error| format!("snapshot pilot absence marker is unreadable: {error}"))?
        != crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES
    {
        return Err(format!("snapshot {} has invalid contents", crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE));
    }
    if let Some(expected) = database_evidence {
        let actual_evidence = inspect_schema2_database_evidence(&actual[DB_FILE])?;
        if actual_evidence.quick_check != ["ok"] || actual_evidence.integrity_check != ["ok"] {
            return Err(format!("schema-2 snapshot database failed SQLite checks: {actual_evidence:?}"));
        }
        if actual_evidence != expected {
            return Err(format!(
                "schema-2 snapshot database evidence does not match its manifest: expected {expected:?}, got {actual_evidence:?}"
            ));
        }
    }
    Ok(true)
}

/// Required state a restore cannot come back without.
///
/// The PRODUCTION consumer of this contract is `scripts/restore_drill.py`, which reads a snapshot
/// from outside the app (the app may not be installable on the machine doing the recovery). This
/// Rust copy exists so the writer's own test asserts the same required set the drill enforces — if
/// the two ever disagree, a snapshot could satisfy one and fail the other.
#[cfg(test)]
pub(crate) fn manifest_missing_required(manifest: &serde_json::Value) -> Vec<String> {
    let present: Vec<&str> = manifest
        .get("files")
        .and_then(|f| f.as_array())
        .map(|rows| rows.iter().filter_map(|r| r.get("path").and_then(|p| p.as_str())).collect())
        .unwrap_or_default();
    let mut missing = Vec::new();
    if !present.contains(&DB_FILE) {
        missing.push(DB_FILE.to_string());
    }
    for state in OPTIONAL_SNAPSHOT_STATE {
        if !present.contains(&state.live_file) && !present.contains(&state.absent_file) {
            missing.push(state.live_file.to_string());
        }
    }
    if !present.contains(&crate::review_pilot::REVIEW_PILOT_FILE)
        && !present.contains(&crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE)
    {
        missing.push(crate::review_pilot::REVIEW_PILOT_FILE.to_string());
    }
    missing
}

/// Staging dirs start with '.', so no `snapshot_`/`<label>_` scan ever counts one.
const STAGING_PREFIX: &str = ".staging_";

/// Best-effort removal of a staging dir after a failed build (warn, never fail the caller further).
fn remove_staging_dir(staging: &Path) {
    if let Err(e) = fs::remove_dir_all(staging) {
        tracing::warn!("snapshot: could not remove staging dir {}: {e}", staging.display());
    }
}

/// Best-effort sweep of stale staging dirs left by a crash mid-snapshot (warn, never fail).
fn sweep_stale_staging_dirs(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && entry.file_name().to_str().is_some_and(|n| n.starts_with(STAGING_PREFIX)) {
            remove_staging_dir(&path);
        }
    }
}

/// True when at least one `snapshot_<ts>` dir already exists under the snapshots root.
fn has_any_snapshot(snapshots_root: &Path) -> bool {
    fs::read_dir(snapshots_root).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.path().is_dir() && entry.file_name().to_str().is_some_and(|name| name.starts_with(SNAPSHOT_PREFIX))
        })
    })
}

fn parse_fixed_timestamp(value: &str) -> Option<u64> {
    (value.len() == 10 && value.bytes().all(|byte| byte.is_ascii_digit())).then(|| value.parse().ok()).flatten()
}

fn parse_pinned_name(value: &str) -> Option<u64> {
    let (label, timestamp) = value.rsplit_once('_')?;
    let valid_label = !label.is_empty()
        && label.len() <= 64
        && label.bytes().next().is_some_and(|byte| byte.is_ascii_alphanumeric())
        && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    valid_label.then(|| parse_fixed_timestamp(timestamp)).flatten()
}

/// Resolve the opaque selector returned by `list_snapshots` without ever accepting an arbitrary
/// filesystem path. Rotating selectors are `snapshot_<10 digits>`; rotation-exempt/recovery pins are
/// `pinned/<safe-label>_<10 digits>`. This is the single app restore path for Rust schema-1 and
/// headless schema-2 artifacts.
pub(crate) fn resolve_snapshot_dir(data_dir: &Path, selector: &str) -> Result<PathBuf, String> {
    let root = data_dir.join("snapshots");
    let path = if let Some(timestamp) = selector.strip_prefix(SNAPSHOT_PREFIX) {
        if parse_fixed_timestamp(timestamp).is_none() {
            return Err(format!("invalid snapshot selector '{selector}'"));
        }
        root.join(selector)
    } else if let Some(name) = selector.strip_prefix("pinned/") {
        if name.contains(['/', '\\']) || parse_pinned_name(name).is_none() {
            return Err(format!("invalid pinned snapshot selector '{selector}'"));
        }
        root.join(PINNED_DIR).join(name)
    } else {
        return Err(format!("invalid snapshot selector '{selector}'"));
    };
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| format!("snapshot '{selector}' is unavailable: {error}"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(format!("snapshot '{selector}' must be a real directory, not a link"));
    }
    Ok(path)
}

fn snapshot_info(path: &Path, name: String, timestamp: u64) -> SnapshotInfo {
    let db_file = path.join(DB_FILE);
    let db_size_bytes = fs::metadata(&db_file).map(|metadata| metadata.len()).unwrap_or(0);
    let segment_count = count_snapshot_segments_readonly(&db_file);
    SnapshotInfo { name, timestamp, db_size_bytes, segment_count }
}

/// List the existing snapshots (newest first) with the metadata the restore picker shows.
pub fn list_snapshots(data_dir: &Path) -> Vec<SnapshotInfo> {
    let root = data_dir.join("snapshots");
    let mut snaps = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else { continue };
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            let Some(timestamp) = name.strip_prefix(SNAPSHOT_PREFIX).and_then(parse_fixed_timestamp) else {
                continue;
            };
            snaps.push(snapshot_info(&path, name, timestamp));
        }
    }
    let pinned_root = root.join(PINNED_DIR);
    if let Ok(entries) = fs::read_dir(&pinned_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else { continue };
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let Some(base) = entry.file_name().to_str().map(str::to_string) else { continue };
            let Some(timestamp) = parse_pinned_name(&base) else { continue };
            snaps.push(snapshot_info(&path, format!("pinned/{base}"), timestamp));
        }
    }
    snaps.sort_by_key(|snap| std::cmp::Reverse(snap.timestamp));
    snaps
}

/// Count segments in a snapshot DB WITHOUT mutating it — a strictly read-only open with NO
/// journal-mode pragma, so a frozen snapshot inspected by the restore picker / quarantine poll is never
/// written to (see list_snapshots). Returns None if the snapshot can't be opened/read.
fn count_snapshot_segments_readonly(db_file: &Path) -> Option<i64> {
    let conn = crate::db::Database::open_immutable_connection(db_file).ok()?;
    conn.query_row("SELECT COUNT(*) FROM speech_segments", [], |row| row.get::<_, i64>(0)).ok()
}

/// Metadata for one snapshot in the restore picker.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    pub name: String,
    pub timestamp: u64,
    pub db_size_bytes: u64,
    pub segment_count: Option<i64>,
}

/// True when the data dir holds an UNACKNOWLEDGED corruption quarantine — a `*.corrupt.*` file the user
/// has not yet cleared. Matches `get_quarantine_notice`'s detection (main files only, not `-wal`/`-shm`
/// sidecars) so the "quarantine present" signal is identical on both sides.
fn has_unacknowledged_quarantine(data_dir: &Path) -> bool {
    fs::read_dir(data_dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.contains(".corrupt.") && !name.ends_with("-wal") && !name.ends_with("-shm"))
        })
    })
}

/// Keep the newest `keep` snapshot dirs (ordered by the timestamp embedded in the name), delete older.
/// Quarantine files are assumed to live in the tree's own parent — for a snapshot tree OUTSIDE the
/// primary data dir (second-directory backup), use `prune_snapshots_from` with the primary dir.
pub fn prune_snapshots(snapshots_root: &Path, keep: usize) -> AppResult<()> {
    let parent = snapshots_root.parent().map(Path::to_path_buf).unwrap_or_else(|| snapshots_root.to_path_buf());
    prune_snapshots_from(snapshots_root, &parent, keep)
}

pub(crate) fn prune_snapshots_from(snapshots_root: &Path, quarantine_dir: &Path, keep: usize) -> AppResult<()> {
    if !snapshots_root.is_dir() {
        return Ok(());
    }
    // #4.5 data-safety: while an UNACKNOWLEDGED corruption quarantine exists (a `*.corrupt.*` file in the
    // PRIMARY data dir the user has not cleared), refuse to prune — pin ALL pre-quarantine history so a
    // post-quarantine re-import can't rotate out the only good copies of weeks of review labor. The
    // empty-DB guard in `take_snapshot_at` only holds until the first re-import (segment_count > 0 lets it
    // snapshot + prune again); this holds the line until the user acknowledges by clearing the quarantine
    // files. Snapshots may accumulate meanwhile — the correct trade for an active, unresolved corruption.
    // `quarantine_dir` is threaded from the caller so the SECOND-DIRECTORY tree pins on the primary
    // data dir's quarantine too (round-24 hunt #6 — it used to inspect its own parent, where
    // *.corrupt.* files never appear, and kept pruning the off-drive history during a corruption).
    if has_unacknowledged_quarantine(quarantine_dir) {
        tracing::warn!(
            "snapshot: unacknowledged corruption quarantine present — refusing to prune so pre-quarantine \
             history is pinned until the *.corrupt.* files are cleared"
        );
        return Ok(());
    }
    let snaps: Vec<(u64, PathBuf)> = fs::read_dir(snapshots_root)
        .map_err(AppError::Io)?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let ts = path.file_name()?.to_str()?.strip_prefix(SNAPSHOT_PREFIX)?.parse::<u64>().ok()?;
            Some((ts, path))
        })
        .collect();
    let keep_set = select_snapshots_to_keep(&snaps.iter().map(|(ts, _)| *ts).collect::<Vec<_>>(), keep);
    for (ts, path) in snaps {
        if !keep_set.contains(&ts) {
            if let Err(e) = fs::remove_dir_all(&path) {
                tracing::warn!("snapshot: could not prune {}: {e}", path.display());
            }
        }
    }
    Ok(())
}

/// TIERED retention (true-10 audit 2026-07-09): the single keep-newest-N tier gave an automatic
/// recovery horizon of only ~100 minutes — damage not noticed within it (a bad merge, an accidental
/// mass delete, a mis-restore) was permanently baked into every surviving snapshot. Keep:
/// * the newest `keep` snapshots (the rolling 10-minute tier, unchanged), PLUS
/// * the newest snapshot of each of the last 7 distinct DAYS, PLUS
/// * the newest snapshot of each of the last 4 distinct WEEKS,
///
/// all measured relative to the newest snapshot's own timestamp (deterministic — no wall clock).
/// Pure so it is directly testable.
fn select_snapshots_to_keep(timestamps: &[u64], keep: usize) -> std::collections::HashSet<u64> {
    let mut sorted: Vec<u64> = timestamps.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a)); // newest first
    let mut kept: std::collections::HashSet<u64> = sorted.iter().take(keep).copied().collect();
    let Some(&newest) = sorted.first() else { return kept };
    const DAY: u64 = 86_400;
    const WEEK: u64 = 7 * DAY;
    let mut kept_days: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut kept_weeks: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for &ts in &sorted {
        let age = newest.saturating_sub(ts);
        let day = ts / DAY;
        let week = ts / WEEK;
        // Newest-first iteration ⇒ the first snapshot seen for a day/week is that period's newest.
        if age < 7 * DAY && kept_days.insert(day) {
            kept.insert(ts);
        }
        if age < 4 * WEEK && kept_weeks.insert(week) {
            kept.insert(ts);
        }
    }
    kept
}

/// Move every quarantine artifact (`*.corrupt.*` main files AND their `-wal`/`-shm` sidecars) into
/// `<data_dir>/quarantine/`, releasing the prune pin EXPLICITLY rather than by deletion — the
/// quarantined bytes stay salvageable (`.recover`) in a discoverable folder. Returns how many files
/// moved. Previously the pin had NO in-app release: pruning stayed refused forever while snapshots
/// accumulated a full DB copy every 10 minutes (true-10 audit 2026-07-09).
pub fn acknowledge_quarantine(data_dir: &Path) -> AppResult<usize> {
    let archive = data_dir.join("quarantine");
    let mut moved = 0usize;
    for entry in fs::read_dir(data_dir).map_err(AppError::Io)?.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if !name_str.contains(".corrupt.") {
            continue;
        }
        fs::create_dir_all(&archive).map_err(AppError::Io)?;
        let dest = archive.join(name_str);
        if let Err(e) = fs::rename(entry.path(), &dest) {
            tracing::warn!("quarantine acknowledge: could not move {name_str}: {e}");
        } else {
            moved += 1;
        }
    }
    Ok(moved)
}

/// Count a snapshot-loop PANIC as a failure so the health surface sees it — a bare thread panic
/// killed the loop silently for the rest of the session (true-10 audit 2026-07-09).
pub fn record_snapshot_panic() {
    CONSECUTIVE_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "s1".to_string(),
            audio_path: "/a.wav".to_string(),
            raw_transcript: "ڕەفەرێنس".to_string(),
            ..Default::default()
        })
        .unwrap();
        db
    }

    fn pilot_policy() -> crate::review_pilot::ReviewPilotPolicy {
        crate::review_pilot::parse(
            r#"{
              "schema_version": 1,
              "after_review_event_id": 0,
              "max_total_corpus_actions": 20,
              "reviewers": [
                {"name": "Hawzhin", "max_corpus_actions": 10},
                {"name": "Pavel", "max_corpus_actions": 10}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn active_pilot_snapshot_requires_exact_current_schema_and_completed_event_grants() {
        let db = seeded_db();
        let policy = pilot_policy();
        validate_active_pilot_snapshot_authority(db.connection(), None, None, &policy).unwrap();

        db.connection().execute("DROP TRIGGER review_pilot_hidden_keys_quota_insert", []).unwrap();
        let schema_error = validate_active_pilot_snapshot_authority(db.connection(), None, None, &policy).unwrap_err();
        assert!(schema_error.contains("schema contract") && schema_error.contains("missing"), "{schema_error}");

        let db = seeded_db();
        let operation_id = uuid::Uuid::new_v4().to_string();
        db.connection()
            .execute(
                "INSERT INTO review_events
                    (segment_id, reviewer, action, source, timestamp_ms, operation_id,
                     operation_payload_hash, requested_action, requested_transcript,
                     served_transcript, served_revision, app_git_sha, playback_guard_version)
                 VALUES ('hidden-complete', 'Hawzhin', 'accept', 'couch_spot_check', 1, ?1,
                         ?2, 'accept', '', 'ڕەفەرێنس', 0, ?3,
                         'content-hash-raw-counter-v3')",
                rusqlite::params![operation_id, "a".repeat(64), crate::GIT_SHA],
            )
            .unwrap();
        let event_error = validate_active_pilot_snapshot_authority(db.connection(), None, None, &policy).unwrap_err();
        assert!(event_error.contains("no durable active-policy grant"), "{event_error}");
    }

    #[test]
    fn active_pilot_pre_migration_snapshot_accepts_exact_v59_and_rejects_drift() {
        let db = seeded_db();
        crate::migrations::rollback(&db, 1).unwrap();
        let policy = pilot_policy();

        validate_active_pilot_snapshot_authority(db.connection(), None, None, &policy).unwrap();

        db.connection().execute("DROP TRIGGER review_pilot_hidden_keys_quota_insert", []).unwrap();
        let error = validate_active_pilot_snapshot_authority(db.connection(), None, None, &policy).unwrap_err();
        assert!(error.contains("schema contract") && error.contains("missing"), "{error}");
    }

    #[test]
    fn active_pilot_snapshot_requires_live_session_keys_to_be_durable() {
        let profile = tempfile::TempDir::new().unwrap();
        let db_path = profile.path().join(DB_FILE);
        let db = Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        db.initialize().unwrap();
        let policy = pilot_policy();
        std::fs::write(
            profile.path().join("couch_session.json"),
            serde_json::to_vec(&serde_json::json!({
                "db_path": &db_path,
                "reviewers": {"token-h": "Hawzhin", "token-p": "Pavel"},
                "pilot_spot_checks": [["hidden-session", "Hawzhin"]],
                "pilot_policy": &policy,
            }))
            .unwrap(),
        )
        .unwrap();
        let error =
            validate_active_pilot_snapshot_authority(db.connection(), Some(profile.path()), Some(&db_path), &policy)
                .unwrap_err();
        assert!(error.contains("no durable active-policy grant"), "{error}");

        db.connection()
            .execute(
                "INSERT INTO review_pilot_hidden_keys
                    (policy_sha256, after_review_event_id, reviewer, segment_id)
                 VALUES (?1, 0, 'Hawzhin', 'hidden-session')",
                [policy.policy_sha256().unwrap()],
            )
            .unwrap();
        validate_active_pilot_snapshot_authority(db.connection(), Some(profile.path()), Some(&db_path), &policy)
            .unwrap();
    }

    #[test]
    fn second_directory_snapshot_survives_primary_loss_and_restores() {
        // The Week-2 backup/restore DRILL, as a repeatable gate: rows written on a primary profile,
        // snapshotted into a SECOND directory (another-drive stand-in), the primary destroyed, and the
        // data recovered into a FRESH profile through the PRODUCTION restore path (integrity check +
        // page copy + in-place migration re-run). Proves second-dir snapshots are restore-complete.
        let primary = tempfile::TempDir::new().unwrap();
        let second = tempfile::TempDir::new().unwrap();
        let fresh = tempfile::TempDir::new().unwrap();

        // 1. Real rows on the primary.
        let db_path = primary.path().join(DB_FILE);
        let db = Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        db.initialize().unwrap();
        for i in 0..25 {
            db.insert_segment(&crate::db::SpeechSegment {
                id: format!("drill-{i:03}"),
                audio_path: "/a.wav".to_string(),
                raw_transcript: format!("ڕیزبەندی {i}"),
                ..Default::default()
            })
            .unwrap();
        }

        // 2. Snapshot into the SECOND directory (exactly what the periodic thread does when
        //    backup_second_dir is set): lands under <second>/snapshots/snapshot_<ts>/.
        let snap = take_snapshot_at(&db, second.path(), 10, 7777).unwrap().expect("non-empty db snapshots");
        assert!(snap.starts_with(second.path()), "snapshot must live in the second dir: {}", snap.display());
        drop(db);

        // 3. Destroy the primary profile entirely (disk-loss stand-in).
        drop(primary);

        // 4. Recover on a fresh profile via the production restore: open+initialize an empty DB, then
        //    Database::restore from the second-dir snapshot file.
        let fresh_db_path = fresh.path().join(DB_FILE);
        let mut recovered = Database::open(fresh_db_path.to_string_lossy().as_ref()).unwrap();
        recovered.initialize().unwrap();
        recovered.restore(snap.join(DB_FILE)).expect("restore from the second-dir snapshot");

        // 5. Every row is back, content intact.
        let rows = recovered.get_segments(None).unwrap();
        assert_eq!(rows.len(), 25, "all 25 rows must survive the round trip");
        assert!(rows.iter().any(|s| s.id == "drill-013" && s.raw_transcript == "ڕیزبەندی 13"));
    }

    #[test]
    fn read_only_segment_count_does_not_mutate_the_frozen_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        let snap = take_snapshot_at(&db, tmp.path(), 10, 1000).unwrap().expect("non-empty db snapshots");
        let snap_db = snap.join(DB_FILE);
        // Clear any WAL sidecars from creation so we can detect ones NEW to inspection.
        let _ = std::fs::remove_file(snap.join(format!("{DB_FILE}-wal")));
        let _ = std::fs::remove_file(snap.join(format!("{DB_FILE}-shm")));

        // The read-only count returns the right number WITHOUT writing to the frozen snapshot. The old
        // Database::open path ran `PRAGMA journal_mode=WAL` and would re-create the -wal/-shm sidecars.
        assert_eq!(count_snapshot_segments_readonly(&snap_db), Some(1));
        assert!(!snap.join(format!("{DB_FILE}-wal")).exists(), "read-only inspection must not create a -wal");
        assert!(!snap.join(format!("{DB_FILE}-shm")).exists(), "read-only inspection must not create a -shm");
    }

    #[test]
    fn take_snapshot_backs_up_db_and_copies_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let db = seeded_db();

        let snap = take_snapshot_at(&db, data_dir, 10, 1000).unwrap().expect("non-empty db snapshots");
        // The DB backup opens as a valid database with the row intact.
        let restored = Database::open(snap.join(DB_FILE).to_str().unwrap()).unwrap();
        assert_eq!(restored.segment_count().unwrap(), 1, "the snapshot DB preserves the data");
        // The helper seeds the complete new-format recovery contract; every required state file is copied.
        assert!(snap.join("settings.json").is_file(), "config state is copied");
        assert!(snap.join("champion.json").exists(), "new snapshots cannot promote without required state");
        assert_eq!(
            std::fs::read(snap.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE)).unwrap(),
            crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES,
            "pilot absence must be explicit; a missing copy can never mean unrestricted mode"
        );
        assert!(!snap.join(crate::review_pilot::REVIEW_PILOT_FILE).exists());
    }

    #[test]
    fn rotating_snapshot_records_every_legally_absent_config_and_refuses_restore_pending() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        let snap = take_snapshot_at_from(&db, tmp.path(), tmp.path(), 10, 1000)
            .unwrap()
            .expect("absent defaults are a complete recovery state");
        for state in OPTIONAL_SNAPSHOT_STATE {
            assert!(!snap.join(state.live_file).exists());
            assert_eq!(std::fs::read(snap.join(state.absent_file)).unwrap(), state.absent_bytes);
        }
        assert!(verify_snapshot_manifest_for_restore(&snap).unwrap());

        std::fs::write(tmp.path().join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"interrupted restore")
            .unwrap();
        let error = take_snapshot_at_from(&db, tmp.path(), tmp.path(), 10, 2000).unwrap_err().to_string();
        assert!(error.contains("restore barrier"), "{error}");
        assert!(!tmp.path().join("snapshots").join("snapshot_0000002000").exists());
    }

    #[test]
    fn rust_restore_verifier_accepts_exact_schema2_headless_contract_and_rejects_evidence_drift() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        let snap = take_snapshot_at(&db, tmp.path(), 10, 1000).unwrap().unwrap();
        let schema1: serde_json::Value =
            serde_json::from_slice(&std::fs::read(snap.join(MANIFEST_FILE)).unwrap()).unwrap();
        let evidence = inspect_schema2_database_evidence(&snap.join(DB_FILE)).unwrap();
        assert_eq!(evidence.schema_version, 65);
        assert_eq!(evidence.row_counts.review_pilot_hidden_keys, Some(0));
        assert_eq!(evidence.row_counts.review_campaign_registry, Some(0));
        assert_eq!(evidence.row_counts.review_pool_registry, Some(0));
        assert_eq!(evidence.row_counts.review_pool_owner_adjudications, Some(0));
        assert_eq!(evidence.row_counts.review_pool_voice_certificates, Some(0));
        assert_eq!(evidence.row_counts.review_pool_dedup_manifests, Some(0));
        assert_eq!(evidence.row_counts.review_pool_duplicate_exclusions, Some(0));
        let schema2 = serde_json::json!({
            "schema": 2,
            "createdAtEpochSecs": 1000,
            "appGitSha": crate::GIT_SHA,
            "sourceDataDir": "C:/disposable/source",
            "databaseEvidence": evidence,
            "files": schema1["files"].clone(),
        });
        std::fs::write(snap.join(MANIFEST_FILE), serde_json::to_vec_pretty(&schema2).unwrap()).unwrap();
        assert!(verify_snapshot_manifest_for_restore(&snap).unwrap());

        let mut drifted = schema2;
        drifted["databaseEvidence"]["rowCounts"]["speech_segments"] = serde_json::json!(999);
        std::fs::write(snap.join(MANIFEST_FILE), serde_json::to_vec_pretty(&drifted).unwrap()).unwrap();
        let error = verify_snapshot_manifest_for_restore(&snap).unwrap_err();
        assert!(error.contains("evidence does not match"), "{error}");
    }

    #[test]
    fn an_invalid_active_pilot_policy_fails_the_snapshot_instead_of_becoming_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        std::fs::write(tmp.path().join(crate::review_pilot::REVIEW_PILOT_FILE), b"{}").unwrap();

        let error = take_snapshot_at(&db, tmp.path(), 10, 1000).unwrap_err().to_string();
        assert!(error.contains(crate::review_pilot::REVIEW_PILOT_FILE), "{error}");
        assert!(
            !tmp.path().join("snapshots").join("snapshot_0000001000").exists(),
            "a snapshot with untrustworthy pay policy must never be promoted"
        );
    }

    #[test]
    fn active_pilot_snapshot_refuses_missing_or_nonexact_focus_before_promotion() {
        const POLICY: &[u8] = br#"{
          "schema_version": 1,
          "after_review_event_id": 0,
          "max_total_corpus_actions": 20,
          "reviewers": [
            {"name": "Hawzhin", "max_corpus_actions": 10},
            {"name": "Pavel", "max_corpus_actions": 10}
          ]
        }"#;
        for (replacement, expected) in [
            (None, "is required"),
            (Some(br#"{"segment_ids":["focus-a"]}"#.as_slice()), "expected exactly 2"),
            (Some(br#"{"segment_ids":["focus-a","focus-wrong"]}"#.as_slice()), "digest mismatch"),
        ] {
            let tmp = tempfile::TempDir::new().unwrap();
            let db = seeded_db();
            seed_test_required_snapshot_state(tmp.path()).unwrap();
            crate::review_pilot::install_test_focus(tmp.path(), ["focus-a", "focus-b"]);
            std::fs::write(tmp.path().join(crate::review_pilot::REVIEW_PILOT_FILE), POLICY).unwrap();
            let focus = tmp.path().join(crate::voice_focus::VOICE_FOCUS_FILE);
            match replacement {
                Some(bytes) => std::fs::write(&focus, bytes).unwrap(),
                None => std::fs::remove_file(&focus).unwrap(),
            }

            let error = take_snapshot_at_from(&db, tmp.path(), tmp.path(), 10, 1000).unwrap_err().to_string();
            assert!(error.contains(expected), "expected {expected:?} in {error}");
            assert!(
                !tmp.path().join("snapshots").join("snapshot_0000001000").exists(),
                "an inexact controlled-pilot focus must never be promoted"
            );
        }
    }

    #[test]
    fn snapshot_never_promotes_an_unrestorable_contract_or_policy_baseline_ahead_of_its_db() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        seed_test_required_snapshot_state(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("voice_focus.json"), b"{}").unwrap();
        let invalid = take_snapshot_at_from(&db, tmp.path(), tmp.path(), 10, 1000).unwrap_err().to_string();
        assert!(invalid.contains("voice_focus.json") && invalid.contains("no segment ids"), "{invalid}");
        assert!(
            std::fs::read_dir(tmp.path().join("snapshots"))
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(SNAPSHOT_PREFIX)),
            "a semantically invalid recovery tree must remain staging-only and be cleaned"
        );

        crate::review_pilot::install_test_focus(tmp.path(), ["x"]);
        std::fs::write(
            tmp.path().join(crate::review_pilot::REVIEW_PILOT_FILE),
            br#"{
              "schema_version": 1,
              "after_review_event_id": 1,
              "max_total_corpus_actions": 20,
              "reviewers": [
                {"name": "Hawzhin", "max_corpus_actions": 10},
                {"name": "Pavel", "max_corpus_actions": 10}
              ]
            }"#,
        )
        .unwrap();
        let ahead = take_snapshot_at_from(&db, tmp.path(), tmp.path(), 10, 2000).unwrap_err().to_string();
        assert!(ahead.contains("baseline 1") && ahead.contains("maximum 0"), "{ahead}");
    }

    #[test]
    fn rotation_keeps_newest_and_prunes_oldest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let db = seeded_db();
        for ts in [100u64, 200, 300] {
            take_snapshot_at(&db, data_dir, 2, ts).unwrap().expect("non-empty db snapshots");
        }
        let root = data_dir.join("snapshots");
        let remaining: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        assert_eq!(remaining.len(), 2, "keep=2 retains exactly two snapshots");
        assert!(remaining.contains(&"snapshot_0000000300".to_string()), "newest kept");
        assert!(remaining.contains(&"snapshot_0000000200".to_string()), "second-newest kept");
        assert!(!remaining.contains(&"snapshot_0000000100".to_string()), "oldest pruned");
    }

    #[test]
    fn prune_is_a_noop_when_root_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        prune_snapshots(&tmp.path().join("nope"), 5).unwrap(); // must not error
    }

    #[test]
    fn prune_refuses_while_an_unacknowledged_quarantine_exists() {
        // #4.5 part 1: the empty-DB guard only holds until the user re-imports (segment_count > 0). Once
        // they do, a non-empty DB snapshots + prunes again — and would rotate out the pre-quarantine
        // history within keep cycles. While a `*.corrupt.*` file is still present (quarantine not cleared),
        // pruning must REFUSE so that history stays pinned, even with a non-empty (re-populated) library.
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let db = seeded_db(); // NON-empty — the empty-DB guard would NOT fire here
                              // Simulate a corruption quarantine the user has not cleared.
        std::fs::write(data_dir.join("cortex-speech.corrupt.1781500000"), b"quarantined db").unwrap();
        // A -wal sidecar of the quarantine must NOT itself count (parity with get_quarantine_notice).
        std::fs::write(data_dir.join("cortex-speech.corrupt.1781500000-wal"), b"").unwrap();

        for ts in [100u64, 200, 300, 400] {
            take_snapshot_at(&db, data_dir, 2, ts).unwrap().expect("non-empty db still snapshots");
        }
        let root = data_dir.join("snapshots");
        let kept = std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.path().is_dir()).count();
        assert_eq!(kept, 4, "with an unacknowledged quarantine, keep=2 must NOT prune — all history is pinned");

        // Acknowledge the quarantine (clear the files), then a fresh snapshot prunes normally again.
        std::fs::remove_file(data_dir.join("cortex-speech.corrupt.1781500000")).unwrap();
        std::fs::remove_file(data_dir.join("cortex-speech.corrupt.1781500000-wal")).unwrap();
        take_snapshot_at(&db, data_dir, 2, 500).unwrap().expect("snapshots");
        let kept_after = std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.path().is_dir()).count();
        assert_eq!(kept_after, 2, "once the quarantine is cleared, keep=2 prunes back to two");
    }

    /// The off-drive target is free text from Settings, and a wrong one is dangerous precisely
    /// because it still LOOKS like a working second copy.
    #[test]
    fn offsite_backup_refuses_a_target_that_is_not_actually_off_drive() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("cortex-speech");
        std::fs::create_dir_all(primary.join("snapshots")).unwrap();

        // The owner's real configuration: a separate drive, outside the data dir.
        let elsewhere = tmp.path().join("offsite");
        validate_offsite_dir(&elsewhere, &primary).expect("a sibling directory is a valid off-drive target");

        // A relative path lands wherever the process happens to be running from.
        assert!(validate_offsite_dir(Path::new("backups"), &primary).is_err(), "relative paths must be refused");

        // The data dir itself, and anything under it — same disk, and inside `snapshots/` each
        // backup would start copying the previous ones.
        assert!(validate_offsite_dir(&primary, &primary).is_err(), "the primary data dir is not a backup");
        assert!(
            validate_offsite_dir(&primary.join("snapshots"), &primary).is_err(),
            "a path inside the data dir is not off-drive"
        );
    }

    #[test]
    fn empty_db_guard_refuses_to_evict_good_snapshots() {
        // B2 (true-10 audit blocker): a corruption quarantine boots an EMPTY DB; snapshotting it every
        // cycle would rotate out all pre-corruption snapshots within keep=N cycles — the safety net
        // destroying the only good copies. Zero segments + prior snapshots => refuse.
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let good_db = seeded_db();
        let good_snap = take_snapshot_at(&good_db, data_dir, 2, 1000).unwrap().expect("seeded db snapshots");

        // Simulate post-quarantine: a fresh EMPTY database.
        let empty_db = Database::open(":memory:").unwrap();
        empty_db.initialize().unwrap();
        for ts in [2000u64, 3000, 4000] {
            let result = take_snapshot_at(&empty_db, data_dir, 2, ts).unwrap();
            assert!(result.is_none(), "empty DB with prior snapshots must be refused (ts {ts})");
        }
        // The good snapshot survived every refused cycle — nothing was rotated out.
        assert!(good_snap.join(DB_FILE).is_file(), "pre-corruption snapshot still intact");
        let restored = Database::open(good_snap.join(DB_FILE).to_str().unwrap()).unwrap();
        assert_eq!(restored.segment_count().unwrap(), 1, "the good copy is still restorable");
    }

    #[test]
    fn empty_db_first_run_still_snapshots() {
        // First-run case: zero segments AND zero prior snapshots — the guard must not block the very
        // first snapshot of a brand-new library.
        let tmp = tempfile::TempDir::new().unwrap();
        let empty_db = Database::open(":memory:").unwrap();
        empty_db.initialize().unwrap();
        let snap = take_snapshot_at(&empty_db, tmp.path(), 5, 1000).unwrap();
        assert!(snap.is_some(), "a brand-new empty library still gets its first snapshot");
    }

    #[test]
    fn snapshot_health_tracks_success_and_consecutive_failures() {
        // True-10 audit: the safety net must never fail silently — health_check reads these
        // counters. A failing snapshot (data_dir path occupied by a FILE, so the snapshot dir
        // cannot be created) increments consecutive_failures; a success resets them and stamps
        // last_success. (Statics are process-wide; assert on relative movement, not absolutes.)
        let db = seeded_db();

        // Failure: 'snapshots' cannot be created because a file sits at data_dir/snapshots' parent.
        let tmp = tempfile::TempDir::new().unwrap();
        let blocked_data_dir = tmp.path().join("blocked");
        std::fs::write(&blocked_data_dir, b"a file, not a dir").unwrap();
        let before = snapshot_health().consecutive_failures;
        assert!(take_snapshot(&db, &blocked_data_dir, 3).is_err(), "file-at-data-dir must fail");
        let after_fail = snapshot_health();
        assert_eq!(after_fail.consecutive_failures, before + 1, "failure increments the counter");

        // Success: counters reset, last_success stamped.
        let good_dir = tempfile::TempDir::new().unwrap();
        seed_test_required_snapshot_state(good_dir.path()).unwrap();
        assert!(take_snapshot(&db, good_dir.path(), 3).unwrap().is_some());
        let after_ok = snapshot_health();
        assert_eq!(after_ok.consecutive_failures, 0, "success resets the failure streak");
        assert!(after_ok.last_success_epoch_secs.is_some(), "success stamps last_success");

        // Round-25 hunt: an off-drive (second-directory) backup SUCCESS must NOT touch the shared health
        // counters — else a succeeding off-drive tree MASKS a failing PRIMARY snapshot tree and
        // health_check reads a false green. Force a fresh primary failure, then a good off-drive
        // snapshot, and assert the primary's failure streak SURVIVES and last_success is NOT re-stamped.
        assert!(take_snapshot(&db, &blocked_data_dir, 3).is_err(), "second primary failure");
        let failures_before_offsite = snapshot_health().consecutive_failures;
        assert!(failures_before_offsite >= 1, "a primary failure is outstanding before the off-drive run");
        let last_success_before = snapshot_health().last_success_epoch_secs;
        let offsite_dir = tempfile::TempDir::new().unwrap();
        assert!(
            take_offsite_snapshot(&db, offsite_dir.path(), good_dir.path(), 3).unwrap().is_some(),
            "the off-drive snapshot itself succeeds"
        );
        let after_offsite = snapshot_health();
        assert_eq!(
            after_offsite.consecutive_failures, failures_before_offsite,
            "an off-drive SUCCESS must not reset the primary's failure streak (no masking)"
        );
        assert_eq!(
            after_offsite.last_success_epoch_secs, last_success_before,
            "an off-drive SUCCESS must not stamp last_success — health reflects the PRIMARY tree only"
        );
    }

    #[test]
    fn tiered_retention_keeps_daily_and_weekly_history() {
        // True-10 audit 2026-07-09: keep-newest-N alone gave a ~100-minute recovery horizon. The
        // tiered selector must ALSO keep the newest snapshot of each of the last 7 days and each of
        // the last 4 weeks, so yesterday's state survives a day of 10-minute rotation.
        const DAY: u64 = 86_400;
        // Well inside day 60 (the 12 rolling entries span 6,600 s and must all stay in the same
        // day — a boundary-straddling fixture would let a rolling entry claim day 59's daily slot),
        // and large enough that newest - 40*DAY never underflows u64.
        let newest = 60 * DAY + 50_000;
        // 10-min-apart rolling snapshots today + one per day for 9 days + one per week older.
        let mut ts: Vec<u64> = (0..12).map(|i| newest - i * 600).collect(); // today, rolling
        ts.extend((1..=9).map(|d| newest - d * DAY)); // daily history
        ts.push(newest - 20 * DAY); // ~3 weeks old
        ts.push(newest - 26 * DAY); // <4 weeks old
        ts.push(newest - 40 * DAY); // >4 weeks old — must NOT be tier-kept
        let kept = select_snapshots_to_keep(&ts, 10);

        // Rolling tier: the newest 10 survive.
        for i in 0..10 {
            assert!(kept.contains(&(newest - i * 600)), "rolling snapshot {i} kept");
        }
        // Daily tier: yesterday .. 6 days ago survive (age < 7 days).
        for d in 1..=6 {
            assert!(kept.contains(&(newest - d * DAY)), "day-{d} snapshot kept");
        }
        // Weekly tier: the ~3-week and <4-week snapshots survive; the >4-week one does not.
        assert!(kept.contains(&(newest - 20 * DAY)), "3-week-old weekly representative kept");
        assert!(kept.contains(&(newest - 26 * DAY)), "<4-week-old weekly representative kept");
        assert!(!kept.contains(&(newest - 40 * DAY)), "beyond 4 weeks is not tier-protected");
        // And 8/9 days ago are outside the daily tier but 8d falls in a kept week only if it is that
        // week's newest — the guarantee under test is the horizon, not per-item survival.
    }

    #[test]
    fn pinned_snapshots_are_rotation_exempt_and_label_capped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let db = seeded_db();
        seed_test_required_snapshot_state(data_dir).unwrap();
        // A pinned pre-migration snapshot...
        let pinned = take_pinned_snapshot(&db, data_dir, "premigration_v33_to_v34", 3).unwrap();
        assert!(pinned.join(DB_FILE).is_file());
        // ...survives heavy rolling rotation (keep=1).
        for ts in [100u64, 200, 300, 400] {
            take_snapshot_at(&db, data_dir, 1, ts).unwrap().expect("non-empty db snapshots");
        }
        assert!(pinned.join(DB_FILE).is_file(), "pinned snapshot must never be rotated out");
        // Same-label pinning is capped at keep_pinned (here 1): a second one evicts the first.
        let second = take_pinned_snapshot(&db, data_dir, "prerestore", 1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100)); // distinct now_secs() timestamp
        let third = take_pinned_snapshot(&db, data_dir, "prerestore", 1).unwrap();
        assert!(third.join(DB_FILE).is_file());
        assert!(!second.exists(), "same-label pinned snapshots are capped at keep_pinned");
        assert!(pinned.join(DB_FILE).is_file(), "different labels never evict each other");
    }

    #[test]
    fn pending_migrations_cannot_run_until_the_exact_pre_upgrade_database_is_pinned() {
        let profile = tempfile::TempDir::new().unwrap();
        seed_test_required_snapshot_state(profile.path()).unwrap();
        let db_path = profile.path().join(DB_FILE);
        let db = Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        db.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&db, 8).unwrap(), vec![65, 64, 63, 62, 61, 60, 59, 58]);
        db.insert_segment(&crate::db::SpeechSegment {
            id: "pre-upgrade-row".to_string(),
            audio_path: "/must-survive.wav".to_string(),
            raw_transcript: "پێش نوێکردنەوە".to_string(),
            ..Default::default()
        })
        .unwrap();

        // Make the snapshot root impossible to create. The shared guard must return before v58-v60 run.
        let blocked_root = profile.path().join("not-a-directory");
        std::fs::write(&blocked_root, b"block child creation").unwrap();
        let error = initialize_with_required_pre_migration_pin(&db, &blocked_root).unwrap_err().to_string();
        assert!(!error.is_empty());
        assert_eq!(crate::migrations::get_current_version(&db).unwrap(), 57, "a failed pin must leave v58-v60 pending");

        // With a usable profile directory, the helper first promotes the v57 pages and only then runs v58-v60.
        let pin = initialize_with_required_pre_migration_pin(&db, profile.path())
            .unwrap()
            .expect("an established v57 profile requires a pin");
        assert_eq!(crate::migrations::get_current_version(&db).unwrap(), 65);
        assert!(verify_snapshot_manifest_for_restore(&pin).unwrap(), "the migration pin must be self-verifying");
        let pinned = Database::open(pin.join(DB_FILE).to_string_lossy().as_ref()).unwrap();
        assert_eq!(crate::migrations::get_current_version(&pinned).unwrap(), 57);
        assert_eq!(pinned.segment_count().unwrap(), 1, "the pre-upgrade pin must contain the live row");

        assert!(
            initialize_with_required_pre_migration_pin(&db, profile.path()).unwrap().is_none(),
            "an already-current schema must not create redundant migration pins"
        );
    }

    #[test]
    fn pending_migration_pin_accepts_and_preserves_legally_absent_config() {
        let profile = tempfile::TempDir::new().unwrap();
        let db_path = profile.path().join(DB_FILE);
        let db = Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        db.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&db, 8).unwrap(), vec![65, 64, 63, 62, 61, 60, 59, 58]);

        let pin = initialize_with_required_pre_migration_pin(&db, profile.path())
            .unwrap()
            .expect("a v57 profile requires a complete safety pin even when config uses defaults");
        assert_eq!(crate::migrations::get_current_version(&db).unwrap(), 65);
        for state in OPTIONAL_SNAPSHOT_STATE {
            assert_eq!(std::fs::read(pin.join(state.absent_file)).unwrap(), state.absent_bytes);
        }
        assert!(verify_snapshot_manifest_for_restore(&pin).unwrap());
    }

    #[test]
    fn acknowledge_quarantine_archives_files_and_releases_the_prune_pin() {
        // True-10 audit 2026-07-09: the prune pin had no in-app release — snapshots accumulated a
        // full DB copy every 10 minutes forever. Acknowledge moves *.corrupt.* (with sidecars) into
        // <data_dir>/quarantine/, keeping the bytes salvageable while releasing the pin.
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let db = seeded_db();
        std::fs::write(data_dir.join("cortex-speech.corrupt.1781500000"), b"bad db").unwrap();
        std::fs::write(data_dir.join("cortex-speech.corrupt.1781500000-wal"), b"wal").unwrap();

        for ts in [100u64, 200, 300] {
            take_snapshot_at(&db, data_dir, 2, ts).unwrap().expect("snapshots");
        }
        let root = data_dir.join("snapshots");
        let count_dirs = |root: &std::path::Path| {
            std::fs::read_dir(root)
                .unwrap()
                .flatten()
                .filter(|e| e.path().is_dir() && e.file_name().to_str().is_some_and(|n| n.starts_with(SNAPSHOT_PREFIX)))
                .count()
        };
        assert_eq!(count_dirs(&root), 3, "pin holds while unacknowledged");

        let moved = acknowledge_quarantine(data_dir).unwrap();
        assert_eq!(moved, 2, "main file + sidecar are archived");
        assert!(data_dir.join("quarantine").join("cortex-speech.corrupt.1781500000").is_file());
        assert!(!data_dir.join("cortex-speech.corrupt.1781500000").exists());

        take_snapshot_at(&db, data_dir, 2, 400).unwrap().expect("snapshots");
        assert_eq!(count_dirs(&root), 2, "after acknowledge, pruning resumes to keep=2");
    }

    #[test]
    fn quarantine_pin_caps_snapshot_accumulation() {
        // While pinned, history is frozen — copies beyond 2×keep add nothing, so taking must stop.
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let db = seeded_db();
        std::fs::write(data_dir.join("cortex-speech.corrupt.1781500000"), b"bad db").unwrap();
        for ts in [100u64, 200, 300, 400] {
            take_snapshot_at(&db, data_dir, 2, ts).unwrap().expect("under the cap, still snapshots");
        }
        // 4 == 2×keep held: the next take is refused (skip), not an error.
        assert!(take_snapshot_at(&db, data_dir, 2, 500).unwrap().is_none(), "cap reached — no new copies");
    }

    #[test]
    fn failed_snapshot_is_built_atomically_and_cleans_up_its_staging() {
        // Round-24 hunt #5: a snapshot that fails mid-build used to leave a `snapshot_<ts>` dir
        // holding a PARTIAL database, and that garbage dir then counted as a REAL snapshot in
        // has_any_snapshot (arming the empty-DB guard against a legitimate first snapshot), the prune
        // keep-set (evicting a good older snapshot), and the quarantine cap. The snapshot is now
        // built in a `.staging_` dir and promoted by a single atomic rename, so a `snapshot_<ts>`
        // name only ever refers to a fully-built dir. A failure must leave NO `.staging_` residue and
        // create NO new `snapshot_<ts>` dir.
        //
        // Deterministic + portable injection: occupy the promote TARGET with a non-empty dir so
        // `fs::rename(staging, target)` fails on both platforms. (The db.backup-failure leg shares
        // the identical remove_staging_dir + Err cleanup path.) The pre-existing target is NOT a
        // partial from this run — the guarantee under test is that the FAILED run adds no garbage.
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let db = seeded_db();
        let root = data_dir.join("snapshots");
        let target = root.join(format!("{SNAPSHOT_PREFIX}{:010}", 1000));
        std::fs::create_dir_all(target.join("occupied")).unwrap();
        std::fs::write(target.join("occupied").join("x"), b"x").unwrap();

        let result = take_snapshot_at(&db, data_dir, 5, 1000);
        assert!(result.is_err(), "promotion onto a non-empty target must fail the snapshot");

        // The failed run leaked NO staging dir...
        let leaked_staging = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(STAGING_PREFIX)));
        assert!(!leaked_staging, "a failed snapshot must clean up its .staging_ dir");
        // ...and a later snapshot (distinct ts, free target) still succeeds normally.
        let ok = take_snapshot_at(&db, data_dir, 5, 2000).unwrap();
        assert!(ok.is_some(), "a later snapshot succeeds after the failed attempt");
        assert!(root.join(format!("{SNAPSHOT_PREFIX}{:010}", 2000)).join(DB_FILE).is_file());
    }

    #[test]
    fn same_second_pinned_snapshots_do_not_overwrite_each_other() {
        // Round-24 hunt #7: <label>_<seconds> collided for two pins in the same wall-clock second —
        // create_dir_all succeeded on the existing dir and db.backup silently OVERWROTE the previous
        // pin's database. Both pins must survive as distinct dirs.
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let db = seeded_db();
        seed_test_required_snapshot_state(data_dir).unwrap();

        let first = take_pinned_snapshot_at(&db, data_dir, "prerestore", 5, 4242).unwrap();
        let second = take_pinned_snapshot_at(&db, data_dir, "prerestore", 5, 4242).unwrap();

        assert_ne!(first, second, "a same-second pin must get a distinct dir, not overwrite");
        assert!(first.join(DB_FILE).is_file(), "the first pin's database survives");
        assert!(second.join(DB_FILE).is_file(), "the second pin has its own database");
    }

    #[test]
    fn pinned_snapshot_never_promotes_an_incomplete_or_unverifiable_recovery_contract() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        seed_test_required_snapshot_state(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("champion.json"), b"{}").unwrap();

        let error =
            take_pinned_snapshot_at(&db, tmp.path(), "premigration_v57_to_v58", 3, 1000).unwrap_err().to_string();
        assert!(error.contains("champion.json") && error.contains("schema"), "{error}");
        let pinned_root = tmp.path().join("snapshots").join(PINNED_DIR);
        assert!(
            std::fs::read_dir(&pinned_root).unwrap().flatten().next().is_none(),
            "a failed pin must leave neither a promoted artifact nor staging residue"
        );

        std::fs::write(tmp.path().join("champion.json"), br#"{"schema":2,"champions":{}}"#).unwrap();
        let pin = take_pinned_snapshot_at(&db, tmp.path(), "premigration_v57_to_v58", 3, 2000).unwrap();
        assert!(verify_snapshot_manifest_for_restore(&pin).unwrap());
    }

    #[test]
    fn second_directory_tree_is_pinned_during_primary_quarantine() {
        // Round-24 hunt #6: the prune-pin inspected the snapshot tree's OWN parent for *.corrupt.*
        // files — for the second-directory (off-drive) tree that parent never holds them, so the
        // off-drive pre-corruption history kept rotating out during an unacknowledged quarantine.
        // With the quarantine source threaded from the primary dir, the off-drive tree pins too.
        let primary = tempfile::TempDir::new().unwrap();
        let second = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        seed_test_required_snapshot_state(primary.path()).unwrap();
        std::fs::write(primary.path().join("cortex-speech.corrupt.1781500000"), b"bad db").unwrap();

        for ts in [100u64, 200, 300, 400] {
            take_snapshot_at_from(&db, second.path(), primary.path(), 2, ts).unwrap().expect("non-empty db snapshots");
        }
        let kept = std::fs::read_dir(second.path().join("snapshots"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir() && e.file_name().to_str().is_some_and(|n| n.starts_with(SNAPSHOT_PREFIX)))
            .count();
        assert_eq!(kept, 4, "the off-drive tree must pin ALL history while the PRIMARY quarantine is unacknowledged");
    }

    #[test]
    fn list_snapshots_reports_newest_first_with_counts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        take_snapshot_at(&db, tmp.path(), 5, 100).unwrap().unwrap();
        take_snapshot_at(&db, tmp.path(), 5, 200).unwrap().unwrap();
        let pinned = take_pinned_snapshot_at(&db, tmp.path(), "premigration_v57_to_v58", 3, 300).unwrap();
        let listed = list_snapshots(tmp.path());
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].timestamp, 300, "pinned recovery artifacts participate in the same picker");
        assert_eq!(listed[0].name, "pinned/premigration_v57_to_v58_0000000300");
        assert_eq!(resolve_snapshot_dir(tmp.path(), &listed[0].name).unwrap(), pinned);
        assert!(resolve_snapshot_dir(tmp.path(), "pinned/../escape_0000000300").is_err());
        assert_eq!(listed[1].timestamp, 200, "newest rotating snapshot follows");
        assert_eq!(listed[2].timestamp, 100);
        assert_eq!(listed[0].segment_count, Some(1), "segment count read from the snapshot DB");
        assert!(listed[0].db_size_bytes > 0);
        assert_eq!(listed[1].name, "snapshot_0000000200");
    }
}

#[cfg(test)]
mod offsite_state_tests {
    use super::*;
    use crate::db::Database;

    const VALID_PILOT_POLICY: &[u8] = br#"{
      "schema_version": 1,
      "after_review_event_id": 0,
      "max_total_corpus_actions": 20,
      "reviewers": [
        {"name": "Hawzhin", "max_corpus_actions": 10},
        {"name": "Pavel", "max_corpus_actions": 10}
      ]
    }"#;

    /// The off-drive copy must carry the state files needed to actually RECOVER, not just the DB.
    ///
    /// Measured 2026-08-19: `take_snapshot_at_from` read `EXTRA_STATE` from the DESTINATION
    /// directory. For a local snapshot destination == primary, so it worked and every test passed;
    /// for the off-drive copy the destination is the owner's second disk, which holds no
    /// `settings.json` or `champion.json` of its own. The copy is best-effort, so both files were
    /// skipped with only a warning and every off-drive snapshot silently contained the database
    /// alone. Restoring from it would come back with no champion pointer — and the server refuses to
    /// start without one, so the "backup" would resurrect a library that cannot transcribe.
    #[test]
    fn an_offsite_snapshot_carries_settings_and_champion_from_the_live_library() {
        let primary = tempfile::TempDir::new().unwrap();
        let offsite = tempfile::TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        seed_test_required_snapshot_state(primary.path()).unwrap();
        let settings = crate::settings::AppSettings {
            backup_second_dir: "X:/recovery".to_string(),
            ..crate::settings::AppSettings::default()
        };
        std::fs::write(primary.path().join("settings.json"), serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
        std::fs::write(primary.path().join("reviewer_dialects.json"), br#"{"Sara":["sorani"]}"#).unwrap();
        crate::review_pilot::install_test_focus(primary.path(), ["a"]);
        std::fs::write(primary.path().join("review_pilot_policy.json"), VALID_PILOT_POLICY).unwrap();

        let snap = take_offsite_snapshot(&db, offsite.path(), primary.path(), 3).unwrap().expect("snapshot");

        assert!(snap.starts_with(offsite.path()), "the tree must be written to the off-drive target");
        for name in EXTRA_STATE {
            let copied = snap.join(name);
            assert!(
                copied.is_file(),
                "{name} is missing from the off-drive snapshot — a restore from it could not recover \
                 the champion pointer or the owner's settings"
            );
        }
        assert_eq!(
            std::fs::read(snap.join("champion.json")).unwrap(),
            std::fs::read(primary.path().join("champion.json")).unwrap(),
            "the copied champion pointer must be the LIVE one, byte for byte"
        );
        assert!(snap.join(DB_FILE).is_file(), "the database itself must still be there");
    }

    /// A snapshot must be able to PROVE what it contains, and the two copies must be comparable.
    #[test]
    fn every_snapshot_carries_a_manifest_that_matches_its_bytes() {
        let primary = tempfile::TempDir::new().unwrap();
        let offsite = tempfile::TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_test_required_snapshot_state(primary.path()).unwrap();
        crate::review_pilot::install_test_focus(primary.path(), ["manifest-focus"]);
        std::fs::write(primary.path().join("review_pilot_policy.json"), VALID_PILOT_POLICY).unwrap();

        let snap = take_offsite_snapshot(&db, offsite.path(), primary.path(), 3).unwrap().unwrap();
        let original_settings = std::fs::read(snap.join("settings.json")).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(snap.join(MANIFEST_FILE)).unwrap()).unwrap();

        assert!(
            manifest_missing_required(&manifest).is_empty(),
            "manifest omits required recovery state: {:?}",
            manifest_missing_required(&manifest)
        );
        for row in manifest["files"].as_array().unwrap() {
            let name = row["path"].as_str().unwrap();
            let path = snap.join(name);
            assert_eq!(
                row["sizeBytes"].as_u64().unwrap(),
                path.metadata().unwrap().len(),
                "{name}: manifest size disagrees with the file on disk"
            );
            assert_eq!(
                row["sha256"].as_str().unwrap(),
                crate::models::compute_file_sha256(&path).unwrap(),
                "{name}: manifest hash disagrees with the file on disk"
            );
        }
        assert!(verify_snapshot_manifest_for_restore(&snap).unwrap(), "a complete new snapshot must verify");

        // Even a self-consistent manifest (updated size/hash) cannot bless a different focus set
        // while the paid-pilot policy is present. This proves semantic binding, not just tamper hash.
        crate::review_pilot::install_test_focus(&snap, ["manifest-focus"]);
        let focus_path = snap.join(crate::voice_focus::VOICE_FOCUS_FILE);
        let refresh_focus_row = |manifest: &mut serde_json::Value| {
            let row = manifest["files"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|row| row["path"] == crate::voice_focus::VOICE_FOCUS_FILE)
                .unwrap();
            row["sizeBytes"] = serde_json::json!(focus_path.metadata().unwrap().len());
            row["sha256"] = serde_json::json!(crate::models::compute_file_sha256(&focus_path).unwrap());
        };
        refresh_focus_row(&mut manifest);
        std::fs::write(snap.join(MANIFEST_FILE), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        assert!(verify_snapshot_manifest_for_restore(&snap).unwrap());
        let exact_focus = std::fs::read(&focus_path).unwrap();
        std::fs::write(&focus_path, br#"{"segment_ids":["manifest-wrong"]}"#).unwrap();
        refresh_focus_row(&mut manifest);
        std::fs::write(snap.join(MANIFEST_FILE), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        let focus_error = verify_snapshot_manifest_for_restore(&snap).unwrap_err();
        assert!(focus_error.contains("digest mismatch"), "{focus_error}");
        std::fs::write(&focus_path, exact_focus).unwrap();
        refresh_focus_row(&mut manifest);
        std::fs::write(snap.join(MANIFEST_FILE), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        assert!(verify_snapshot_manifest_for_restore(&snap).unwrap());

        // Every failure is pre-restore and loud: mismatched bytes, an incomplete inventory, and an
        // invalid manifest can never be downgraded to the manifest-less legacy path.
        std::fs::write(snap.join("settings.json"), b"tampered").unwrap();
        let mismatch = verify_snapshot_manifest_for_restore(&snap).unwrap_err();
        assert!(mismatch.contains("SHA-256 mismatch") || mismatch.contains("size mismatch"), "{mismatch}");
        std::fs::write(snap.join("settings.json"), &original_settings).unwrap();

        let mut incomplete = manifest.clone();
        incomplete["files"].as_array_mut().unwrap().retain(|row| row["path"] != "champion.json");
        std::fs::remove_file(snap.join("champion.json")).unwrap();
        std::fs::write(snap.join(MANIFEST_FILE), serde_json::to_vec_pretty(&incomplete).unwrap()).unwrap();
        let missing = verify_snapshot_manifest_for_restore(&snap).unwrap_err();
        assert!(missing.contains("exactly one") && missing.contains("champion.json"), "{missing}");

        std::fs::write(snap.join(MANIFEST_FILE), b"{not json}").unwrap();
        assert!(verify_snapshot_manifest_for_restore(&snap).unwrap_err().contains("invalid JSON"));
    }

    /// The point of the manifest: an incomplete tree is DETECTABLE rather than merely present.
    #[test]
    fn a_snapshot_missing_required_state_is_reported_as_missing() {
        let manifest = serde_json::json!({"schema": 1, "files": [{"path": "cortex-speech.db"}]});
        let missing = manifest_missing_required(&manifest);
        assert!(missing.contains(&"settings.json".to_string()), "{missing:?}");
        assert!(missing.contains(&"champion.json".to_string()), "{missing:?}");
    }

    /// The local snapshot path is unchanged by the fix: destination and primary are one directory.
    #[test]
    fn a_local_snapshot_still_carries_its_own_state_files() {
        let data = tempfile::TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        seed_test_required_snapshot_state(data.path()).unwrap();
        crate::review_pilot::install_test_focus(data.path(), ["local-focus"]);
        std::fs::write(data.path().join("review_pilot_policy.json"), VALID_PILOT_POLICY).unwrap();

        let snap = take_snapshot(&db, data.path(), 3).unwrap().expect("snapshot");
        for name in EXTRA_STATE {
            assert!(snap.join(name).is_file(), "local snapshot lost {name}");
        }
    }
}
