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
pub fn take_snapshot(db: &Database, data_dir: &Path, keep: usize) -> AppResult<PathBuf> {
    take_snapshot_at(db, data_dir, keep, now_secs())
}

/// `take_snapshot` with an explicit timestamp (testable without same-second collisions).
pub(crate) fn take_snapshot_at(db: &Database, data_dir: &Path, keep: usize, ts: u64) -> AppResult<PathBuf> {
    let root = data_dir.join("snapshots");
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
    Ok(snap_dir)
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

        let snap = take_snapshot_at(&db, data_dir, 10, 1000).unwrap();
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
            take_snapshot_at(&db, data_dir, 2, ts).unwrap();
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
}
