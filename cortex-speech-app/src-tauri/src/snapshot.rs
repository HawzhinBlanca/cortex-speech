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

/// Small state files copied alongside the DB (best-effort; absent files are skipped). `champion.json`
/// is the future retrain-champion pointer (M5) — harmless to list before it exists.
///
/// SINGLE SOURCE OF TRUTH: `restore_db_from_snapshot` (commands.rs) restores exactly this set, so the
/// save-side and restore-side can never drift — a file added here is automatically restored, not
/// silently snapshotted-but-never-restored.
pub(crate) const EXTRA_STATE: &[&str] = &["settings.json", "champion.json"];

const SNAPSHOT_PREFIX: &str = "snapshot_";
const DB_FILE: &str = "cortex-speech.db";
/// Rotation-exempt snapshots live under `<data_dir>/snapshots/pinned/` — the rolling prune only
/// touches `snapshot_*` dirs at the root, so nothing here is ever auto-evicted.
const PINNED_DIR: &str = "pinned";

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
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
    let result = take_snapshot_at(db, data_dir, keep, now_secs());
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

/// Take a PINNED, rotation-exempt snapshot into `<data_dir>/snapshots/pinned/<label>_<ts>/`,
/// keeping only the newest `keep_pinned` snapshots that share `label`. Used for the two moments the
/// rolling ~100-minute window cannot cover (true-10 audit 2026-07-09): (a) BEFORE running pending
/// migrations — a semantically-buggy migration commits cleanly and is immediately re-snapshotted, so
/// without this the last pre-upgrade state rotates away within keep cycles of first launch; (b)
/// BEFORE a restore overwrites the live DB — a mis-restore of the wrong snapshot was otherwise
/// recoverable only from a ≤10-min-old rolling snapshot that itself rotates out.
pub fn take_pinned_snapshot(db: &Database, data_dir: &Path, label: &str, keep_pinned: usize) -> AppResult<PathBuf> {
    let pinned_root = data_dir.join("snapshots").join(PINNED_DIR);
    let snap_dir = pinned_root.join(format!("{label}_{:010}", now_secs()));
    fs::create_dir_all(&snap_dir).map_err(AppError::Io)?;
    db.backup(snap_dir.join(DB_FILE))?;
    for name in EXTRA_STATE {
        let src = data_dir.join(name);
        if src.is_file() {
            if let Err(e) = fs::copy(&src, snap_dir.join(name)) {
                tracing::warn!("pinned snapshot: could not copy {name}: {e}");
            }
        }
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

/// `take_snapshot` with an explicit timestamp (testable without same-second collisions).
pub(crate) fn take_snapshot_at(db: &Database, data_dir: &Path, keep: usize, ts: u64) -> AppResult<Option<PathBuf>> {
    let root = data_dir.join("snapshots");

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
    if has_unacknowledged_quarantine(data_dir) {
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

    let snap_dir = root.join(format!("{SNAPSHOT_PREFIX}{ts:010}"));
    fs::create_dir_all(&snap_dir).map_err(AppError::Io)?;

    // The DB is the critical artifact — its failure fails the snapshot. Online backup (page copy) is
    // safe while the app reads/writes the source.
    db.backup(snap_dir.join(DB_FILE))?;

    // Config/state files are best-effort — a missing or unreadable one must not lose the DB snapshot.
    for name in EXTRA_STATE {
        let src = data_dir.join(name);
        if src.is_file() {
            if let Err(e) = fs::copy(&src, snap_dir.join(name)) {
                tracing::warn!("snapshot: could not copy {name}: {e}");
            }
        }
    }

    prune_snapshots(&root, keep)?;
    Ok(Some(snap_dir))
}

/// True when at least one `snapshot_<ts>` dir already exists under the snapshots root.
fn has_any_snapshot(snapshots_root: &Path) -> bool {
    fs::read_dir(snapshots_root).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.path().is_dir() && entry.file_name().to_str().is_some_and(|name| name.starts_with(SNAPSHOT_PREFIX))
        })
    })
}

/// List the existing snapshots (newest first) with the metadata the restore picker shows.
pub fn list_snapshots(data_dir: &Path) -> Vec<SnapshotInfo> {
    let root = data_dir.join("snapshots");
    let mut snaps: Vec<SnapshotInfo> = fs::read_dir(&root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            let ts = name.strip_prefix(SNAPSHOT_PREFIX)?.parse::<u64>().ok()?;
            let db_file = path.join(DB_FILE);
            let db_size_bytes = fs::metadata(&db_file).map(|m| m.len()).unwrap_or(0);
            // Segment count via a strictly READ-ONLY connection. Database::open runs
            // `PRAGMA journal_mode=WAL` — a WRITE that mutates the file header and spawns -wal/-shm
            // sidecars. Doing that to a FROZEN snapshot on every restore-picker open / quarantine-banner
            // poll violates its immutability and could leave a -wal beside it that a later restore
            // unexpectedly replays. A snapshot that can't open reports None, so a damaged one stays
            // visibly distinct from an empty one in the picker.
            let segment_count = count_snapshot_segments_readonly(&db_file);
            Some(SnapshotInfo { name, timestamp: ts, db_size_bytes, segment_count })
        })
        .collect();
    snaps.sort_by_key(|snap| std::cmp::Reverse(snap.timestamp));
    snaps
}

/// Count segments in a snapshot DB WITHOUT mutating it — a strictly read-only open with NO
/// journal-mode pragma, so a frozen snapshot inspected by the restore picker / quarantine poll is never
/// written to (see list_snapshots). Returns None if the snapshot can't be opened/read.
fn count_snapshot_segments_readonly(db_file: &Path) -> Option<i64> {
    let conn = rusqlite::Connection::open_with_flags(
        db_file,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
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
pub fn prune_snapshots(snapshots_root: &Path, keep: usize) -> AppResult<()> {
    if !snapshots_root.is_dir() {
        return Ok(());
    }
    // #4.5 data-safety: while an UNACKNOWLEDGED corruption quarantine exists (a `*.corrupt.*` file in the
    // data dir the user has not cleared), refuse to prune — pin ALL pre-quarantine history so a
    // post-quarantine re-import can't rotate out the only good copies of weeks of review labor. The
    // empty-DB guard in `take_snapshot_at` only holds until the first re-import (segment_count > 0 lets it
    // snapshot + prune again); this holds the line until the user acknowledges by clearing the quarantine
    // files. Snapshots may accumulate meanwhile — the correct trade for an active, unresolved corruption.
    if let Some(data_dir) = snapshots_root.parent() {
        if has_unacknowledged_quarantine(data_dir) {
            tracing::warn!(
                "snapshot: unacknowledged corruption quarantine present — refusing to prune so pre-quarantine \
                 history is pinned until the *.corrupt.* files are cleared"
            );
            return Ok(());
        }
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
        std::fs::write(data_dir.join("settings.json"), b"{\"k\":1}").unwrap();
        let db = seeded_db();

        let snap = take_snapshot_at(&db, data_dir, 10, 1000).unwrap().expect("non-empty db snapshots");
        // The DB backup opens as a valid database with the row intact.
        let restored = Database::open(snap.join(DB_FILE).to_str().unwrap()).unwrap();
        assert_eq!(restored.segment_count().unwrap(), 1, "the snapshot DB preserves the data");
        // settings.json was copied; a listed-but-absent champion.json is simply skipped.
        assert!(snap.join("settings.json").is_file(), "config state is copied");
        assert!(!snap.join("champion.json").exists(), "absent state files are skipped, not errored");
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
        assert!(take_snapshot(&db, good_dir.path(), 3).unwrap().is_some());
        let after_ok = snapshot_health();
        assert_eq!(after_ok.consecutive_failures, 0, "success resets the failure streak");
        assert!(after_ok.last_success_epoch_secs.is_some(), "success stamps last_success");
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
    fn list_snapshots_reports_newest_first_with_counts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = seeded_db();
        take_snapshot_at(&db, tmp.path(), 5, 100).unwrap().unwrap();
        take_snapshot_at(&db, tmp.path(), 5, 200).unwrap().unwrap();
        let listed = list_snapshots(tmp.path());
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].timestamp, 200, "newest first");
        assert_eq!(listed[1].timestamp, 100);
        assert_eq!(listed[0].segment_count, Some(1), "segment count read from the snapshot DB");
        assert!(listed[0].db_size_bytes > 0);
        assert_eq!(listed[0].name, "snapshot_0000000200");
    }
}
