use crate::atomic_file::{remove_file_on_error, replace_file};
use crate::db::Database;
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Session state — saved periodically for crash recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        }
    }

    pub fn save_path(&self) -> PathBuf {
        self.save_dir.join("session.json")
    }

    pub fn auto_save(&mut self, db: &Database) -> AppResult<()> {
        let now = now_epoch_secs();
        if now - self.last_save < self.save_interval_secs {
            return Ok(());
        }
        self.save(db)?;
        self.last_save = now;
        Ok(())
    }

    pub fn save(&self, db: &Database) -> AppResult<()> {
        let state = SessionState::from_db(db)?;
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
        if !path.exists() {
            return None;
        }
        let json = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    pub fn restore(&self) -> AppResult<Option<SessionState>> {
        let path = self.save_path();
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
        let manager = SessionManager::new(tmp.path().to_path_buf());
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let restored = manager.restore().unwrap();
        assert!(restored.is_none());
    }

    #[test]
    fn test_session_restore_quarantines_corrupt_file() {
        let tmp = TempDir::new().unwrap();
        let manager = SessionManager::new(tmp.path().to_path_buf());
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
