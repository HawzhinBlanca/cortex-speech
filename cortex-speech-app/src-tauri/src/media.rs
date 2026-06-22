use crate::db::Database;
use crate::validation::input as validate;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MEDIA_TTL_MINUTES: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaGrant {
    pub id: String,
    pub path: String,
    pub expires_at: String,
}

#[derive(Debug, Clone)]
struct GrantRecord {
    source_path: PathBuf,
    cached_path: PathBuf,
    expires_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct MediaRegistry {
    grants: HashMap<String, GrantRecord>,
}

/// The directory the registry copies playable clips into, relative to the app data dir.
///
/// This is the SINGLE source of truth shared by the writer (`register`, below) and the
/// asset-protocol scope grant in `lib.rs` setup. The Tauri WebView can only load an `asset://`
/// URL whose path is inside the asset-protocol scope, and the static `$APPDATA/media-cache/**`
/// scope in `tauri.conf.json` resolves (Tauri v2) to the bundle-identifier-qualified app-data dir,
/// NOT to `get_app_data_dir()`'s `%APPDATA%\cortex-speech`. So the scope is granted at runtime from
/// THIS function; if the two ever computed different directories, in-app playback would silently
/// break (every clip would 403). Keeping it in one place makes that drift impossible.
pub fn media_cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("media-cache")
}

impl MediaRegistry {
    pub fn register(&mut self, db: &Database, data_dir: &Path, requested_path: &str) -> Result<MediaGrant, String> {
        self.prune_expired();
        let canonical = validate::validate_file_path(requested_path)?;
        self.ensure_imported_media_path(db, requested_path, &canonical)?;
        let source_path = PathBuf::from(&canonical);

        if let Some(grant) = self.existing_grant_for_source(&source_path) {
            return Ok(grant);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let ext = Path::new(&canonical)
            .extension()
            .and_then(|e| e.to_str())
            .map(validate::sanitize_filename)
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| "audio".to_string());
        let cache_dir = media_cache_dir(data_dir);
        std::fs::create_dir_all(&cache_dir).map_err(|e| format!("Create media cache: {e}"))?;
        self.prune_orphaned_cache_files(&cache_dir);
        let cached_path = cache_dir.join(format!("{id}.{ext}"));
        std::fs::copy(&canonical, &cached_path).map_err(|e| format!("Copy media into app cache: {e}"))?;

        let expires_at = Utc::now() + Duration::minutes(MEDIA_TTL_MINUTES);
        self.grants.insert(id.clone(), GrantRecord { source_path, cached_path: cached_path.clone(), expires_at });

        Ok(MediaGrant { id, path: cached_path.to_string_lossy().to_string(), expires_at: expires_at.to_rfc3339() })
    }

    pub fn resolve(&mut self, id: &str) -> Result<String, String> {
        self.prune_expired();
        let record = self.grants.get(id).ok_or_else(|| "Media grant is missing or expired".to_string())?;
        if !record.cached_path.exists() {
            return Err("Cached media file is missing".to_string());
        }
        Ok(record.cached_path.to_string_lossy().to_string())
    }

    fn ensure_imported_media_path(&self, db: &Database, original: &str, canonical: &str) -> Result<(), String> {
        // Use the audio_path index (migration v13) for a single O(log N) lookup.
        // Try both the canonical and original forms: on Windows, `canonicalize()` adds
        // a \\?\ prefix, while the DB may store the path in its original non-canonical form.
        let found = db.get_segment_by_audio_path(canonical).map_err(|e| format!("Media path check failed: {e}"))?;
        if found.is_some() {
            return Ok(());
        }
        // Second try: match using the original (pre-canonicalize) path.
        if original != canonical {
            let found2 = db
                .get_segment_by_audio_path(original)
                .map_err(|e| format!("Media path check (original) failed: {e}"))?;
            if found2.is_some() {
                return Ok(());
            }
        }
        Err("Media playback is limited to files already imported into this dataset".to_string())
    }

    fn existing_grant_for_source(&mut self, source_path: &Path) -> Option<MediaGrant> {
        let now = Utc::now();
        let stale: Vec<String> = self
            .grants
            .iter()
            .filter_map(|(id, record)| {
                if record.source_path == source_path && (record.expires_at <= now || !record.cached_path.exists()) {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        for id in stale {
            if let Some(record) = self.grants.remove(&id) {
                remove_cached_media_file(&record.cached_path, "stale grant");
            }
        }

        self.grants.iter().find_map(|(id, record)| {
            if record.source_path == source_path && record.expires_at > now && record.cached_path.exists() {
                Some(MediaGrant {
                    id: id.clone(),
                    path: record.cached_path.to_string_lossy().to_string(),
                    expires_at: record.expires_at.to_rfc3339(),
                })
            } else {
                None
            }
        })
    }

    fn prune_expired(&mut self) {
        let now = Utc::now();
        let expired: Vec<String> = self
            .grants
            .iter()
            .filter_map(|(id, record)| if record.expires_at <= now { Some(id.clone()) } else { None })
            .collect();
        for id in expired {
            if let Some(record) = self.grants.remove(&id) {
                remove_cached_media_file(&record.cached_path, "expired grant");
            }
        }
    }

    fn prune_orphaned_cache_files(&self, cache_dir: &Path) {
        let active_paths: HashSet<PathBuf> = self.grants.values().map(|record| record.cached_path.clone()).collect();
        let Ok(entries) = std::fs::read_dir(cache_dir) else {
            return;
        };

        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_file() || active_paths.contains(&path) {
                continue;
            }

            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("Failed to remove stale media cache file {}: {e}", path.display());
            }
        }
    }
}

fn remove_cached_media_file(path: &Path, context: &str) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!("Failed to remove {context} cached media file {}: {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SpeechSegment;
    use tempfile::TempDir;

    fn segment(path: &Path) -> SpeechSegment {
        SpeechSegment {
            id: "seg-1".into(),
            created_at: None,
            audio_path: path.to_string_lossy().to_string(),
            raw_transcript: "test".into(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 1000,
            speaker_id: None,
            verified: false,
            confidence: None,
            ctc_score: None,
            clipping_ratio: None,
            rms_db: None,
            snr_db: None,
            split: None,
            ood_score: None,
            ..SpeechSegment::default()
        }
    }

    // The asset-protocol scope grant in lib.rs setup and the registry writer MUST target the same
    // directory, or the WebView refuses every cached clip's asset:// URL and no audio can play (the
    // shipped bug: the static `$APPDATA/media-cache/**` scope resolved to the identifier-qualified
    // app-data dir, a sibling of get_app_data_dir()'s %APPDATA%\cortex-speech). Both sides now derive
    // the dir from media_cache_dir(); this pins that the registry's cache file really lands under it,
    // so the runtime grant (which uses the same function) authorizes exactly what the registry wrote.
    #[test]
    fn registry_writes_into_media_cache_dir() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("sample.wav");
        std::fs::write(&audio, b"audio").unwrap();

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();

        let mut registry = MediaRegistry::default();
        let grant = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();

        let expected_dir = media_cache_dir(tmp.path());
        let cached = std::path::Path::new(&grant.path);
        assert_eq!(
            cached.parent(),
            Some(expected_dir.as_path()),
            "cached clip must live in media_cache_dir(data_dir) — the exact dir lib.rs grants to the asset scope"
        );
        assert!(cached.exists());
    }

    #[test]
    fn grants_imported_media_and_rejects_arbitrary_files() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("sample.wav");
        let other = tmp.path().join("other.wav");
        std::fs::write(&audio, b"audio").unwrap();
        std::fs::write(&other, b"other").unwrap();

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();

        let mut registry = MediaRegistry::default();
        let grant = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();
        assert!(Path::new(&grant.path).exists());
        assert!(registry.resolve(&grant.id).unwrap().contains("media-cache"));
        assert!(registry.register(&db, tmp.path(), &other.to_string_lossy()).is_err());
    }

    #[test]
    fn reuses_live_cached_copy_for_same_imported_media() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("sample.wav");
        std::fs::write(&audio, b"audio").unwrap();

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();

        let mut registry = MediaRegistry::default();
        let first = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();
        let second = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(first.path, second.path);
        let cache_dir = tmp.path().join("media-cache");
        let cached_files = std::fs::read_dir(cache_dir).unwrap().count();
        assert_eq!(cached_files, 1, "same imported audio should only have one live cache copy");
    }

    #[test]
    fn recreates_cache_if_reused_grant_file_was_removed() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("sample.wav");
        std::fs::write(&audio, b"audio").unwrap();

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();

        let mut registry = MediaRegistry::default();
        let first = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();
        std::fs::remove_file(&first.path).unwrap();

        let second = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();

        assert_ne!(first.id, second.id);
        assert!(Path::new(&second.path).exists());
        assert!(registry.resolve(&second.id).is_ok());
        assert!(registry.resolve(&first.id).is_err());
    }

    #[test]
    fn removes_orphaned_cache_files_before_new_grant() {
        let tmp = TempDir::new().unwrap();
        let cache_dir = tmp.path().join("media-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let stale = cache_dir.join("stale.wav");
        std::fs::write(&stale, b"stale").unwrap();
        let audio = tmp.path().join("sample.wav");
        std::fs::write(&audio, b"audio").unwrap();

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();

        let mut registry = MediaRegistry::default();
        let grant = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();

        assert!(!stale.exists());
        assert!(Path::new(&grant.path).exists());
    }

    #[test]
    fn keeps_live_cache_files_when_pruning_orphans() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("sample.wav");
        let second_audio = tmp.path().join("sample-2.wav");
        std::fs::write(&audio, b"audio").unwrap();
        std::fs::write(&second_audio, b"audio2").unwrap();

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();
        let mut second_segment = segment(&second_audio);
        second_segment.id = "seg-2".into();
        db.insert_segment(&second_segment).unwrap();

        let mut registry = MediaRegistry::default();
        let first = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();
        let second = registry.register(&db, tmp.path(), &second_audio.to_string_lossy()).unwrap();

        assert!(Path::new(&first.path).exists());
        assert!(Path::new(&second.path).exists());
        assert_eq!(std::fs::read_dir(tmp.path().join("media-cache")).unwrap().count(), 2);
    }
}
