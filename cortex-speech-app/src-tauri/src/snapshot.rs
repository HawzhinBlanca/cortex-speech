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
const EXTRA_STATE: &[&str] = &["settings.json", "champion.json"];

const SNAPSHOT_PREFIX: &str = "snapshot_";
const DB_FILE: &str = "cortex-speech.db";

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Take a rotating snapshot into `<data_dir>/snapshots/snapshot_<ts>/`, then prune to newest `keep`.
/// Returns `Ok(None)` when the EMPTY-DB GUARD refuses the snapshot (see below) — a skip, not an error.
pub fn take_snapshot(db: &Database, data_dir: &Path, keep: usize) -> AppResult<Option<PathBuf>> {
    take_snapshot_at(db, data_dir, keep, now_secs())
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
            // Segment count via a plain read connection; a snapshot that can't open reports None so a
            // damaged snapshot is visibly distinct from an empty one in the picker.
            let segment_count =
                Database::open(db_file.to_string_lossy().as_ref()).ok().and_then(|db| db.segment_count().ok());
            Some(SnapshotInfo { name, timestamp: ts, db_size_bytes, segment_count })
        })
        .collect();
    snaps.sort_by_key(|snap| std::cmp::Reverse(snap.timestamp));
    snaps
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

/// Keep the newest `keep` snapshot dirs (ordered by the timestamp embedded in the name), delete older.
pub fn prune_snapshots(snapshots_root: &Path, keep: usize) -> AppResult<()> {
    if !snapshots_root.is_dir() {
        return Ok(());
    }
    let mut snaps: Vec<(u64, PathBuf)> = fs::read_dir(snapshots_root)
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
    snaps.sort_by_key(|(ts, _)| *ts);
    while snaps.len() > keep {
        let (_, oldest) = snaps.remove(0);
        if let Err(e) = fs::remove_dir_all(&oldest) {
            tracing::warn!("snapshot: could not prune {}: {e}", oldest.display());
        }
    }
    Ok(())
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
