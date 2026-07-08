use crate::atomic_file::{remove_file_on_error, replace_file};
use crate::db::Database;
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Session state — saved periodically for crash recovery.
///
/// NOTE (round-25 #4): only `segment_count` / `verified_count` currently round-trip. `from_db` is the
/// sole writer and rebuilds the UI fields (`selected_segment_id`, `search_query`, filters, `view_mode`,
/// panels) from `Self::new()` defaults, because the frontend does not yet pass live UI state into
/// `save_session`. Those fields are RESERVED for that future wiring — do not rely on them being
/// restored. Round-25 #5: `#[serde(default)]` lets an older/newer `session.json` deserialize by filling
/// any missing field from `Self::default()` instead of quarantining a benign version-skewed file as
/// corrupt (the `version` field is informational; there is no migration branch).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    pub version: String,
    pub last_saved: u64,
    pub selected_segment_id: Option<String>,
    pub filter_verified: Option<bool>,
    pub search_query: String,
    pub sort_order: String,
    pub view_mode: String,
    pub left_panel: String,
    pub right_panel: String,
    pub segment_count: usize,
    pub verified_count: usize,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            last_saved: now_epoch_secs(),
            selected_segment_id: None,
            filter_verified: None,
            search_query: String::new(),
            sort_order: "newest".to_string(),
            view_mode: "transcribe".to_string(),
            left_panel: "segments".to_string(),
            right_panel: "none".to_string(),
            segment_count: 0,
            verified_count: 0,
        }
    }

    pub fn from_db(db: &Database) -> AppResult<Self> {
        let segments = db.get_segments(None)?;
        let verified_count = segments.iter().filter(|s| s.verified).count();
        Ok(Self { segment_count: segments.len(), verified_count, ..Self::new() })
    }
}

// ── Production note ──────────────────────────────────────────────────
// Session files contain raw transcript data as plain JSON with no
// encryption. In production:
//   - On Unix: ensure the session directory permissions are 0700 so that
//     only the owning user can read/write session state
//     (e.g. `chmod 0700 /path/to/session`).
//   - On Windows: NTFS ACLs can be used but granular POSIX-style
//     permissions are not available. Consider encrypting session data
//     at rest if transcripts are sensitive.
// ──────────────────────────────────────────────────────────────────────

/// Session manager — auto-saves state periodically.
pub struct SessionManager {
    save_dir: PathBuf,
    save_interval_secs: u64,
    last_save: u64,
    // Last-known UI view-state, held in memory so EVERY save persists it — otherwise the periodic
    // counts-only auto_save (which derives state via `from_db`) would reset it to defaults and the
    // explicit frontend save would be clobbered seconds later. Seeded from disk on `restore`.
    search_query: String,
    sort_order: String,
    // M2.6: Current review segment ID for cursor persistence across restarts.
    selected_segment_id: Option<String>,
    // M2.6/P1.5: the review verified-filter, held in memory so every save persists it.
    filter_verified: Option<bool>,
}

impl SessionManager {
    pub fn new(save_dir: PathBuf) -> Self {
        if let Err(error) = std::fs::create_dir_all(&save_dir) {
            tracing::warn!("Failed to create session directory {}: {error}", save_dir.display());
        }
        Self {
            save_dir,
            save_interval_secs: 60, // auto-save every 60 seconds
            last_save: 0,
            search_query: String::new(),
            sort_order: "newest".to_string(),
            selected_segment_id: None,
            filter_verified: None,
        }
    }

    /// Update the in-memory view-state that every subsequent save persists. Called from the frontend
    /// whenever the user's search query or sort order changes, so a later counts-only auto_save keeps
    /// it instead of resetting it.
    pub fn set_view_state(&mut self, search_query: String, sort_order: String, filter_verified: Option<bool>) {
        self.search_query = search_query;
        self.sort_order = sort_order;
        self.filter_verified = filter_verified;
    }

    /// M2.6: Set the current review segment ID for cursor persistence across restarts.
    pub fn set_current_segment(&mut self, segment_id: &str) {
        self.selected_segment_id = Some(segment_id.to_string());
    }

    pub fn save_path(&self) -> PathBuf {
        self.save_dir.join("session.json")
    }

    pub fn auto_save(&mut self, db: &Database) -> AppResult<()> {
        let now = now_epoch_secs();
        // saturating_sub, not `-`: if the wall clock jumps backward (NTP correction, manual set) so
        // now < last_save, a plain u64 subtraction underflows (panic in debug / wrap in release). The
        // saturating form yields 0 (< interval) so a backward jump simply defers the next save.
        if now.saturating_sub(self.last_save) < self.save_interval_secs {
            return Ok(());
        }
        self.save(db)?;
        self.last_save = now;
        Ok(())
    }

    pub fn save(&self, db: &Database) -> AppResult<()> {
        // Counts come from the DB; the view-state fields come from the held in-memory state so a
        // periodic auto_save preserves the user's last search/sort rather than resetting them.
        let mut state = SessionState::from_db(db)?;
        state.search_query = self.search_query.clone();
        state.sort_order = self.sort_order.clone();
        // M2.6: Persist the current review segment + filter for cursor recovery on restart.
        state.selected_segment_id = self.selected_segment_id.clone();
        state.filter_verified = self.filter_verified;
        let json = serde_json::to_string_pretty(&state)
            .map_err(|e| crate::error::AppError::Other(format!("Session serialize: {e}")))?;
        let tmp_path = self.save_dir.join("session.json.tmp");
        let final_path = self.save_path();
        remove_file_on_error(
            &tmp_path,
            (|| -> AppResult<()> {
                std::fs::write(&tmp_path, &json).map_err(crate::error::AppError::Io)?;
                replace_file(&tmp_path, &final_path).map_err(crate::error::AppError::Io)?;
                Ok(())
            })(),
        )
    }

    pub fn load(&self) -> Option<SessionState> {
        let path = self.save_path();
        // Round-25 #3: promote an orphaned .replace-bak-* left by an interrupted atomic replace.
        let _ = crate::atomic_file::recover_interrupted_replace(&path);
        if !path.exists() {
            return None;
        }
        let json = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    pub fn restore(&mut self) -> AppResult<Option<SessionState>> {
        let path = self.save_path();
        // Round-25 #3: a hard crash between replace_file's two renames can leave session.json missing
        // with the only good copy stranded at a .replace-bak-* sibling. Promote it back into place
        // (mirrors AppSettings::load) BEFORE the path.exists() check, so a recoverable session is not
        // silently lost. Round-25 #6: garbage-collect stale temp/backup/quarantine files while here.
        if let Ok(true) = crate::atomic_file::recover_interrupted_replace(&path) {
            tracing::warn!("Recovered an interrupted session.json replace from its backup copy");
        }
        self.prune_session_dir();
        if path.exists() {
            // Clean stale sessions older than 7 days
            if let Ok(metadata) = std::fs::metadata(&path) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(age) = SystemTime::now().duration_since(modified) {
                        if age.as_secs() > 7 * 24 * 3600 {
                            tracing::info!("Removing stale session file ({} days old)", age.as_secs() / 86400);
                            self.clear();
                            return Ok(None);
                        }
                    }
                }
            }
        }

        if !path.exists() {
            return Ok(None);
        }

        let json = match std::fs::read_to_string(&path) {
            Ok(json) => json,
            Err(e) => {
                tracing::warn!("Could not read session file at {}: {e}", path.display());
                return Ok(None);
            }
        };

        match serde_json::from_str::<SessionState>(&json) {
            Ok(state) => {
                tracing::info!("Restored session: {} segments, {} verified", state.segment_count, state.verified_count);
                // Seed the in-memory view-state so the next counts-only auto_save preserves the
                // restored fields instead of overwriting them with defaults. P1.5: cursor + filter are
                // seeded too — otherwise an auto_save before the first decision would clobber the
                // restored cursor back to None.
                self.search_query = state.search_query.clone();
                self.sort_order = state.sort_order.clone();
                self.selected_segment_id = state.selected_segment_id.clone();
                self.filter_verified = state.filter_verified;
                Ok(Some(state))
            }
            Err(e) => {
                let quarantine_path = self.quarantine_session_file()?;
                tracing::warn!(
                    "Quarantined corrupt session file at {} after parse error: {e}",
                    quarantine_path.display()
                );
                Ok(None)
            }
        }
    }

    pub fn clear(&self) {
        let path = self.save_path();
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!("Failed to remove session file {}: {error}", path.display()),
        }
    }

    /// Round-25 #6: best-effort garbage collection of the session directory — delete a leftover
    /// `session.json.tmp` (failed write) and any `session.json.replace-bak-*` orphan (after recovery
    /// has had its chance), and cap quarantined `session.corrupt.*.json` files to the most recent few
    /// so repeated crash/upgrade cycles don't accumulate unbounded dead files. Never fails the load.
    fn prune_session_dir(&self) {
        const MAX_QUARANTINE: usize = 5;
        let Ok(entries) = std::fs::read_dir(&self.save_dir) else {
            return;
        };
        let mut corrupt: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name == "session.json.tmp" || name.starts_with("session.json.replace-bak-") {
                let _ = std::fs::remove_file(&path);
            } else if name.starts_with("session.corrupt.") && name.ends_with(".json") {
                let mtime = entry.metadata().and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
                corrupt.push((mtime, path));
            }
        }
        if corrupt.len() > MAX_QUARANTINE {
            corrupt.sort_by_key(|(t, _)| *t); // oldest first
            for (_, path) in corrupt.iter().take(corrupt.len() - MAX_QUARANTINE) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    fn quarantine_session_file(&self) -> AppResult<PathBuf> {
        self.quarantine_session_file_at(now_epoch_secs())
    }

    fn quarantine_session_file_at(&self, timestamp: u64) -> AppResult<PathBuf> {
        let source = self.save_path();
        for attempt in 0..1000 {
            let suffix = if attempt == 0 { format!("{timestamp}") } else { format!("{timestamp}.{attempt}") };
            let candidate = self.save_dir.join(format!("session.corrupt.{suffix}.json"));
            if !candidate.exists() {
                std::fs::rename(&source, &candidate).map_err(AppError::Io)?;
                return Ok(candidate);
            }
        }

        Err(AppError::Other("Could not allocate a unique corrupt session quarantine path".into()))
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_session_save_load() {
        let tmp = TempDir::new().unwrap();
        let manager = SessionManager::new(tmp.path().to_path_buf());
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        manager.save(&db).unwrap();
        let loaded = manager.load();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().segment_count, 0);
    }

    #[test]
    fn test_session_save_replaces_existing_file() {
        let tmp = TempDir::new().unwrap();
        let manager = SessionManager::new(tmp.path().to_path_buf());
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        std::fs::write(manager.save_path(), "{\"version\":\"old\"}").unwrap();

        manager.save(&db).unwrap();

        let saved = std::fs::read_to_string(manager.save_path()).unwrap();
        assert!(saved.contains("\"segment_count\": 0"));
        assert!(!saved.contains("\"old\""));
        assert!(!tmp.path().join("session.json.tmp").exists());
    }

    #[test]
    fn test_session_restore_empty() {
        let tmp = TempDir::new().unwrap();
        let mut manager = SessionManager::new(tmp.path().to_path_buf());
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let restored = manager.restore().unwrap();
        assert!(restored.is_none());
    }

    #[test]
    fn test_session_restore_quarantines_corrupt_file() {
        let tmp = TempDir::new().unwrap();
        let mut manager = SessionManager::new(tmp.path().to_path_buf());
        std::fs::write(manager.save_path(), "{not valid json").unwrap();

        let restored = manager.restore().unwrap();

        assert!(restored.is_none());
        assert!(!manager.save_path().exists());
        let quarantined: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("session.corrupt.") && name.ends_with(".json"))
            .collect();
        assert_eq!(quarantined.len(), 1);
    }

    #[test]
    fn test_session_restore_uses_unique_corrupt_quarantine_name() {
        let tmp = TempDir::new().unwrap();
        let manager = SessionManager::new(tmp.path().to_path_buf());
        let timestamp = 12345;
        std::fs::write(tmp.path().join(format!("session.corrupt.{timestamp}.json")), "old").unwrap();
        std::fs::write(manager.save_path(), "{not valid json").unwrap();

        let quarantined = manager.quarantine_session_file_at(timestamp).unwrap();

        assert_eq!(quarantined.file_name().unwrap().to_string_lossy(), format!("session.corrupt.{timestamp}.1.json"));
        assert!(!manager.save_path().exists());
        assert!(tmp.path().join(format!("session.corrupt.{timestamp}.json")).exists());
        assert!(tmp.path().join(format!("session.corrupt.{timestamp}.1.json")).exists());
    }

    #[test]
    fn test_session_clear() {
        let tmp = TempDir::new().unwrap();
        let manager = SessionManager::new(tmp.path().to_path_buf());
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        manager.save(&db).unwrap();
        assert!(manager.save_path().exists());
        manager.clear();
        assert!(!manager.save_path().exists());
    }

    #[test]
    fn test_session_persists_and_restores_view_state() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        // Session 1: the user sets a search + sort + filter + review cursor, which is saved.
        let mut first = SessionManager::new(tmp.path().to_path_buf());
        first.set_view_state("بەڕێوە".to_string(), "duration".to_string(), Some(false));
        first.set_current_segment("seg-42");
        first.save(&db).unwrap();

        // Session 2 ("restart"): a fresh manager over the same dir restores the full view-state.
        let mut second = SessionManager::new(tmp.path().to_path_buf());
        let restored = second.restore().unwrap().expect("a saved session is restored");
        assert_eq!(restored.search_query, "بەڕێوە");
        assert_eq!(restored.sort_order, "duration");
        assert_eq!(restored.filter_verified, Some(false), "P1.5: the review filter is restored");
        assert_eq!(restored.selected_segment_id.as_deref(), Some("seg-42"), "P1.5: the cursor is restored");

        // P1.5 regression: restore must SEED the manager cursor, so a later counts-only auto_save does
        // NOT clobber the restored cursor back to None before the first decision.
        second.last_save = 0;
        second.auto_save(&db).unwrap();
        let json = std::fs::read_to_string(second.save_path()).unwrap();
        assert!(json.contains("\"selected_segment_id\": \"seg-42\""), "cursor survived auto_save: {json}");
        assert!(json.contains("\"filter_verified\": false"), "filter survived auto_save: {json}");
    }

    #[test]
    fn test_counts_autosave_preserves_view_state() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let mut manager = SessionManager::new(tmp.path().to_path_buf());
        manager.set_view_state("query".to_string(), "confidence".to_string(), None);
        manager.save(&db).unwrap();

        // A later counts-only auto_save (the periodic mutation-triggered path) must NOT reset the
        // view-state to defaults.
        manager.last_save = 0; // force the interval check to fire
        manager.auto_save(&db).unwrap();

        let json = std::fs::read_to_string(manager.save_path()).unwrap();
        assert!(json.contains("\"search_query\": \"query\""), "view-state survived auto_save: {json}");
        assert!(json.contains("\"sort_order\": \"confidence\""));
    }

    #[test]
    fn test_session_auto_save() {
        let tmp = TempDir::new().unwrap();
        let mut manager = SessionManager::new(tmp.path().to_path_buf());
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        // First save should trigger
        manager.auto_save(&db).unwrap();
        assert!(manager.save_path().exists());

        // Second save within interval should not trigger
        manager.last_save = now_epoch_secs();
        let path_modified = std::fs::metadata(manager.save_path()).unwrap().modified().unwrap();
        manager.auto_save(&db).unwrap();
        let path_modified2 = std::fs::metadata(manager.save_path()).unwrap().modified().unwrap();
        assert_eq!(path_modified, path_modified2);
    }
}
