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
        let canonical = Self::ensure_imported(db, requested_path)?;
        self.register_cached(data_dir, &canonical)
    }

    /// DB-only membership check (no file I/O). Returns the canonicalized source path so the caller
    /// can RELEASE the db lock before the (potentially gigabyte) cache copy in `register_cached` —
    /// otherwise the global db mutex is held across `std::fs::copy` and every other DB-touching IPC
    /// (notably the UI's get_segments) stalls for the copy duration.
    pub fn ensure_imported(db: &Database, requested_path: &str) -> Result<String, String> {
        // Use the audio_path index (migration v13) for a single O(log N) lookup. Try the canonical
        // and original forms: Windows `canonicalize()` adds a \\?\ prefix while the DB may hold the
        // original non-canonical path.
        let canonical = validate::validate_file_path(requested_path)?;
        if db.get_segment_by_audio_path(&canonical).map_err(|e| format!("Media path check failed: {e}"))?.is_some() {
            return Ok(canonical);
        }
        if requested_path != canonical
            && db
                .get_segment_by_audio_path(requested_path)
                .map_err(|e| format!("Media path check (original) failed: {e}"))?
                .is_some()
        {
            return Ok(canonical);
        }
        Err("Media playback is limited to files already imported into this dataset".to_string())
    }

    /// db-locked, FAST: validate the path and confirm it is an imported media file, returning the
    /// canonical source path. Round-25 #7: split out so the caller can RELEASE the global db Mutex
    /// before the (potentially multi-GB) file copy in [`grant_source`] — holding the global db lock
    /// across `std::fs::copy` froze every other DB command for the length of the copy. This is the
    /// `PathBuf`-returning sibling of [`ensure_imported`]; both share the same DB-only membership
    /// check via [`ensure_imported_media_path`].
    pub fn validate_source(&mut self, db: &Database, requested_path: &str) -> Result<PathBuf, String> {
        self.prune_expired();
        let canonical = validate::validate_file_path(requested_path)?;
        self.ensure_imported_media_path(db, requested_path, &canonical)?;
        Ok(PathBuf::from(canonical))
    }

    /// Grant + cache an ALREADY-validated source path. Touches NO database, so the full-file copy is
    /// safe to run without the db lock held. Only the media-registry mutex serializes concurrent
    /// callers, which does not contend with get_segments.
    pub fn register_cached(&mut self, data_dir: &Path, canonical: &str) -> Result<MediaGrant, String> {
        self.grant_source(data_dir, PathBuf::from(canonical))
    }

    /// NO db lock: copy the source into the media cache and grant a TTL token. The expensive
    /// `std::fs::copy` runs here, so this MUST be called with the global db Mutex released (see
    /// [`validate_source`] / [`ensure_imported`]).
    pub fn grant_source(&mut self, data_dir: &Path, source_path: PathBuf) -> Result<MediaGrant, String> {
        self.prune_expired();
        if let Some(grant) = self.existing_grant_for_source(&source_path) {
            return Ok(grant);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let ext = source_path
            .extension()
            .and_then(|e| e.to_str())
            .map(validate::sanitize_filename)
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| "audio".to_string());
        let cache_dir = media_cache_dir(data_dir);
        std::fs::create_dir_all(&cache_dir).map_err(|e| format!("Create media cache: {e}"))?;
        self.prune_orphaned_cache_files(&cache_dir);
        let cached_path = cache_dir.join(format!("{id}.{ext}"));
        // Materialize the source INTO the asset-protocol scope so the WebView can play it — preferring a
        // hard link (instant, zero extra disk) over a whole-file copy. A multi-GB audiobook otherwise
        // copies in full into the temp cache on every grant (P1 durability). `source_bytes = 0` when the
        // size can't be read → the copy-path room check no-ops (graceful-degrade).
        let source_bytes = std::fs::metadata(&source_path).map(|m| m.len()).unwrap_or(0);
        match link_or_copy_into_cache(&source_path, &cached_path, source_bytes, &cache_dir)? {
            CacheMaterialization::HardLinked => {
                tracing::debug!("media cache: hard-linked {} (no copy)", source_path.display());
            }
            CacheMaterialization::Copied => {
                tracing::debug!("media cache: copied {} MB (cross-volume fallback)", source_bytes / 1_048_576);
            }
        }

        let expires_at = Utc::now() + Duration::minutes(MEDIA_TTL_MINUTES);
        self.grants.insert(id.clone(), GrantRecord { source_path, cached_path: cached_path.clone(), expires_at });

        Ok(MediaGrant { id, path: cached_path.to_string_lossy().to_string(), expires_at: expires_at.to_rfc3339() })
    }

    /// DB-only membership check using the audio_path index (migration v13): a single O(log N) lookup,
    /// trying both the canonical and original (pre-canonicalize) forms because Windows
    /// `canonicalize()` adds a `\\?\` prefix while the DB may store the original non-canonical path.
    fn ensure_imported_media_path(&self, db: &Database, original: &str, canonical: &str) -> Result<(), String> {
        if db.get_segment_by_audio_path(canonical).map_err(|e| format!("Media path check failed: {e}"))?.is_some() {
            return Ok(());
        }
        if original != canonical
            && db
                .get_segment_by_audio_path(original)
                .map_err(|e| format!("Media path check (original) failed: {e}"))?
                .is_some()
        {
            return Ok(());
        }
        Err("Media playback is limited to files already imported into this dataset".to_string())
    }

    pub fn resolve(&mut self, id: &str) -> Result<String, String> {
        self.prune_expired();
        let record = self.grants.get_mut(id).ok_or_else(|| "Media grant is missing or expired".to_string())?;
        if !record.cached_path.exists() {
            return Err("Cached media file is missing".to_string());
        }
        // Sliding TTL: resolving means the frontend is (re)loading this clip (the AudioPlayer
        // re-resolves on load/replay), so keep it alive. Without this, a clip the user is still
        // working with expires after MEDIA_TTL_MINUTES and the next prune (triggered by granting any
        // other clip, or idle time) deletes the file out from under the playing <audio> element,
        // making a later play/seek fail with "Cached media file is missing".
        record.expires_at = Utc::now() + Duration::minutes(MEDIA_TTL_MINUTES);
        Ok(record.cached_path.to_string_lossy().to_string())
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

        // Find a live grant for this source and REFRESH its TTL on reuse — re-registering the same
        // clip (e.g. re-selecting a segment) means it's still in use, so it must keep the clip alive,
        // not return a grant that is about to expire.
        self.grants.iter_mut().find_map(|(id, record)| {
            if record.source_path == source_path && record.expires_at > now && record.cached_path.exists() {
                record.expires_at = now + Duration::minutes(MEDIA_TTL_MINUTES);
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

/// How a source was materialized into the media cache. A hard link shares the source's bytes (no copy,
/// no extra disk); a copy is the cross-volume / linkless-filesystem fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheMaterialization {
    HardLinked,
    Copied,
}

/// Put `source` at `cached` inside the media cache for asset-scope playback WITHOUT a whole-file copy
/// when possible. A hard link is instant and consumes zero extra disk (same volume), and removing the
/// cached entry later never touches the source (a separate directory entry to the same inode). Falls
/// back to a real `std::fs::copy` across volumes or on filesystems without hard-link support — and only
/// THEN checks disk room, since only a copy can exhaust the volume. A hard link serves byte-identical
/// audio (the same inode), so there is no offset/corruption risk versus the source.
fn link_or_copy_into_cache(
    source: &Path,
    cached: &Path,
    source_bytes: u64,
    cache_dir: &Path,
) -> Result<CacheMaterialization, String> {
    match std::fs::hard_link(source, cached) {
        Ok(()) => Ok(CacheMaterialization::HardLinked),
        Err(_link_err) => {
            // Cross-volume or unsupported FS: fall back to a full copy, but refuse it if it wouldn't fit
            // (a partial copy could corrupt a co-located WAL when the disk hits zero).
            ensure_cache_room(source_bytes, crate::health::free_disk_bytes_for(cache_dir))?;
            std::fs::copy(source, cached).map_err(|e| format!("Copy media into app cache: {e}"))?;
            Ok(CacheMaterialization::Copied)
        }
    }
}

/// Headroom kept free on the media-cache volume beyond the file being copied, so caching a clip can
/// never drive the disk to 0 — which would corrupt the SQLite WAL and every other writer sharing the
/// volume. 64 MiB comfortably covers an in-flight WAL + OS slack.
const MEDIA_CACHE_MARGIN_BYTES: u64 = 64 * 1_048_576;

/// Pure decision (unit-tested): refuse to START a whole-file cache copy that would exhaust the disk.
///
/// `free_bytes = None` means the volume could not be resolved (`free_disk_bytes_for`) — degrade
/// gracefully and allow the copy rather than block playback on an inability to measure. Otherwise the
/// copy needs the source size PLUS [`MEDIA_CACHE_MARGIN_BYTES`] of headroom, or it is refused with a
/// clear, actionable error instead of a cryptic half-written-file `std::fs::copy` failure.
fn ensure_cache_room(source_bytes: u64, free_bytes: Option<u64>) -> Result<(), String> {
    let Some(free) = free_bytes else { return Ok(()) };
    let needed = source_bytes.saturating_add(MEDIA_CACHE_MARGIN_BYTES);
    if free < needed {
        return Err(format!(
            "Not enough free disk space to cache this clip for playback: needs {} MB (file {} MB + {} MB headroom), \
             but only {} MB is free on the media-cache volume. Free some space and try again.",
            needed / 1_048_576,
            source_bytes / 1_048_576,
            MEDIA_CACHE_MARGIN_BYTES / 1_048_576,
            free / 1_048_576,
        ));
    }
    Ok(())
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
    fn resolve_and_reuse_refresh_the_grant_ttl() {
        // Round-18: the grant TTL was never refreshed by resolve/reuse, so prune_expired could delete a
        // clip the user is still interacting with after MEDIA_TTL_MINUTES. resolving (and re-registering)
        // must push the expiry back to a full TTL.
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("sample.wav");
        std::fs::write(&audio, b"audio").unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();

        let mut registry = MediaRegistry::default();
        let grant = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();

        // Artificially age the grant to nearly-expired, then resolve.
        registry.grants.get_mut(&grant.id).unwrap().expires_at = Utc::now() + Duration::seconds(2);
        registry.resolve(&grant.id).unwrap();
        assert!(
            registry.grants.get(&grant.id).unwrap().expires_at > Utc::now() + Duration::minutes(MEDIA_TTL_MINUTES - 1),
            "resolve must refresh the grant TTL"
        );

        // The reuse path (re-registering the same source) also refreshes.
        registry.grants.get_mut(&grant.id).unwrap().expires_at = Utc::now() + Duration::seconds(2);
        let reused = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();
        assert_eq!(reused.id, grant.id, "same source reuses the live grant");
        assert!(
            registry.grants.get(&grant.id).unwrap().expires_at > Utc::now() + Duration::minutes(MEDIA_TTL_MINUTES - 1),
            "reuse must refresh the grant TTL"
        );
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
    fn cache_room_allows_when_volume_unresolved() {
        // free_bytes = None (volume could not be resolved) must not block playback.
        assert!(ensure_cache_room(500 * 1_048_576, None).is_ok());
    }

    #[test]
    fn cache_room_allows_with_ample_space() {
        // 100 MB file, 10 GB free — comfortably above file + 64 MB margin.
        assert!(ensure_cache_room(100 * 1_048_576, Some(10 * 1024 * 1_048_576)).is_ok());
    }

    #[test]
    fn cache_room_refuses_when_free_below_file_size() {
        let err = ensure_cache_room(500 * 1_048_576, Some(100 * 1_048_576)).unwrap_err();
        assert!(err.contains("Not enough free disk space"), "{err}");
        assert!(err.contains("500 MB") || err.contains("564 MB"), "message names the requirement: {err}");
    }

    #[test]
    fn cache_room_enforces_the_headroom_margin() {
        // Free space is ABOVE the raw file size but BELOW file + 64 MB headroom — must still refuse,
        // so the copy can't drive the DB/WAL volume to zero.
        let source = 500 * 1_048_576;
        let free_just_over_file = source + 1_048_576; // file + 1 MB, < file + 64 MB margin
        assert!(ensure_cache_room(source, Some(free_just_over_file)).is_err());
        // Exactly file + margin is the boundary: allowed.
        assert!(ensure_cache_room(source, Some(source + MEDIA_CACHE_MARGIN_BYTES)).is_ok());
    }

    #[test]
    fn materializes_into_cache_via_hard_link_not_a_copy_on_the_same_volume() {
        // The media cache and a source under the same TempDir share a volume, so materialization must be
        // an instant hard link — no whole-file copy, no extra disk (the whole point of this change).
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("book.wav");
        std::fs::write(&src, b"original-audio").unwrap();
        let cache_dir = tmp.path().join("media-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cached = cache_dir.join("clip.wav");

        let how = link_or_copy_into_cache(&src, &cached, 14, &cache_dir).unwrap();
        assert_eq!(how, CacheMaterialization::HardLinked, "same-volume must hard-link, not copy");
        assert_eq!(std::fs::read(&cached).unwrap(), b"original-audio", "linked clip serves the source bytes");

        // A hard link shares the source inode: an in-place rewrite of the source is visible through the
        // cached entry. A whole-file COPY would NOT reflect it — so this proves we linked, not copied.
        std::fs::write(&src, b"changed").unwrap();
        assert_eq!(
            std::fs::read(&cached).unwrap(),
            b"changed",
            "the cached entry must share the source's bytes (hard link, not a copy)"
        );
    }

    #[test]
    fn removing_the_cached_hard_link_never_deletes_the_source() {
        // Prune/expiry removes the cached entry; because it is a hard link (not the source itself), the
        // owner's original audio file must survive untouched.
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("book.wav");
        std::fs::write(&src, b"keep me").unwrap();
        let cache_dir = tmp.path().join("media-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cached = cache_dir.join("clip.wav");
        link_or_copy_into_cache(&src, &cached, 7, &cache_dir).unwrap();

        remove_cached_media_file(&cached, "test");
        assert!(!cached.exists(), "cache entry removed");
        assert!(src.exists(), "the source audio must survive removal of its cache hard link");
        assert_eq!(std::fs::read(&src).unwrap(), b"keep me", "source content intact");
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
