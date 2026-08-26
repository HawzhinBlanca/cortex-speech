use crate::db::Database;
use crate::validation::input as validate;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

const MEDIA_TTL_MINUTES: i64 = 30;
const MAX_DISTINCT_MEDIA_MATERIALIZATIONS: usize = 2;
const MAX_MEDIA_FLIGHT_FOLLOWERS: usize = 8;
pub(crate) const MEDIA_MATERIALIZATION_BUSY_CODE: &str = "E_MEDIA_MATERIALIZATION_BUSY";

fn try_increment_below(counter: &AtomicUsize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        if current >= limit {
            return false;
        }
        match counter.compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaGrant {
    pub id: String,
    pub path: String,
    pub expires_at: String,
}

#[derive(Debug)]
struct GrantRecord {
    source_path: PathBuf,
    cached_path: PathBuf,
    verified_audio_content_hash: Option<String>,
    // Keep the private cache image open for the grant lifetime. On Windows the handle is opened
    // with FILE_SHARE_READ only, so neither Cortex nor another process can rewrite, replace, rename,
    // or delete the exact bytes the WebView is hearing until the grant is retired.
    _cache_guard: Arc<std::fs::File>,
    // Verified review grants also freeze the imported source inode/path. Otherwise an editor could
    // replace `audio_path` after the old PCM was cached, letting a human decision attach to different
    // bytes once the cache expired. Ordinary Library grants carry no review authority and omit it.
    _source_guard: Option<Arc<std::fs::File>>,
    expires_at: DateTime<Utc>,
}

/// Internal grant identity used by playback-proof commands.  The renderer receives neither source
/// nor cache internals from this type; it can present only the opaque grant UUID.  Re-resolving here
/// proves the grant is still live and the cached artifact still exists at both session issuance and
/// finalization.
#[derive(Debug, Clone)]
pub(crate) struct MediaGrantBinding {
    pub(crate) source_path: PathBuf,
    pub(crate) audio_content_hash: String,
    // Cloning the lease lets IPC release the registry mutex before it takes the database writer.
    // The OS handle keeps the exact cache bytes sealed through that DB transaction.
    _cache_guard: Arc<std::fs::File>,
    _source_guard: Arc<std::fs::File>,
}

/// Exact imported-source bytes held immutable across the final decision transaction. Normal commits
/// clone this from the still-live media grant at O(1) cost. Recovery after grant expiry re-verifies
/// the current source PCM once, then returns the same lease shape.
#[derive(Debug, Clone)]
pub(crate) struct VerifiedMediaSourceLease {
    pub(crate) source_path: PathBuf,
    pub(crate) audio_content_hash: String,
    _source_guard: Arc<std::fs::File>,
}

impl PartialEq for VerifiedMediaSourceLease {
    fn eq(&self, other: &Self) -> bool {
        self.source_path == other.source_path && self.audio_content_hash == other.audio_content_hash
    }
}

impl Eq for VerifiedMediaSourceLease {}

impl MediaGrantBinding {
    pub(crate) fn source_lease(&self) -> VerifiedMediaSourceLease {
        VerifiedMediaSourceLease {
            source_path: self.source_path.clone(),
            audio_content_hash: self.audio_content_hash.clone(),
            _source_guard: Arc::clone(&self._source_guard),
        }
    }
}

/// Recovery path for an exact finalized receipt after its ephemeral media grant expired or the app
/// restarted. Freeze the current source first, then walk its decoded PCM and require the durable
/// imported identity. The returned guard stays alive through the decision commit, closing the
/// verify-then-write race. This is intentionally expensive only on recovery; normal commits reuse the
/// already-verified live grant lease.
pub(crate) fn verify_current_source_lease(
    source_path: &Path,
    expected_audio_content_hash: &str,
) -> Result<VerifiedMediaSourceLease, String> {
    let source_path =
        std::fs::canonicalize(source_path).map_err(|error| format!("Canonicalize current review source: {error}"))?;
    let source_guard = open_immutable_source_guard(&source_path)?;
    let actual = crate::export_bundle::current_canonical_pcm_blake3(&source_path)
        .map_err(|error| format!("Verify current review source PCM: {error}"))?;
    if actual != expected_audio_content_hash {
        return Err("The imported source bytes changed after playback; reload or restore the recording".to_string());
    }
    Ok(VerifiedMediaSourceLease { source_path, audio_content_hash: actual, _source_guard: Arc::new(source_guard) })
}

/// DB-authorized source identity captured before the expensive cache copy. The source path is
/// canonicalized, and the content hash is the one unambiguous decoded-PCM identity shared by every
/// imported segment from that recording.
#[derive(Debug, Clone)]
pub(crate) struct ValidatedMediaSource {
    source_path: PathBuf,
    audio_content_hash: String,
}

#[derive(Default)]
pub struct MediaRegistry {
    grants: HashMap<String, GrantRecord>,
    /// Reserved cache paths that are being built outside this mutex. They are never resolvable and
    /// never authorize playback until [`publish_materialization`] inserts a complete sealed grant.
    materializing_paths: HashSet<PathBuf>,
    /// Exact files retired while the mutex was held. Callers drain this queue and perform deletion
    /// only after releasing the registry, so antivirus/filesystem latency cannot stall resolution.
    retired_artifacts: Vec<RetiredMediaArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MediaMaterializationKey {
    source_path: PathBuf,
    expected_audio_content_hash: Option<String>,
}

#[derive(Debug)]
struct MediaMaterializationPlan {
    id: String,
    source_path: PathBuf,
    cached_path: PathBuf,
    cache_dir: PathBuf,
    expected_audio_content_hash: Option<String>,
}

#[derive(Debug)]
struct PreparedMediaMaterialization {
    id: String,
    source_path: PathBuf,
    cached_path: PathBuf,
    expected_audio_content_hash: Option<String>,
    source_bytes: u64,
    materialized_bytes: u64,
    cache_guard: std::fs::File,
    source_guard: Option<std::fs::File>,
}

/// Opaque retired cache artifact. Keeping the grant/prepared object here defers Windows handle close
/// as well as file deletion until after the registry mutex is released.
#[derive(Debug)]
pub(crate) struct RetiredMediaArtifact {
    cached_path: PathBuf,
    _grant: Option<GrantRecord>,
    _prepared: Option<PreparedMediaMaterialization>,
}

impl RetiredMediaArtifact {
    fn from_grant(grant: GrantRecord) -> Self {
        Self { cached_path: grant.cached_path.clone(), _grant: Some(grant), _prepared: None }
    }

    fn from_prepared(prepared: PreparedMediaMaterialization) -> Self {
        Self { cached_path: prepared.cached_path.clone(), _grant: None, _prepared: Some(prepared) }
    }

    fn from_path(cached_path: PathBuf) -> Self {
        Self { cached_path, _grant: None, _prepared: None }
    }
}

#[derive(Debug, Default)]
struct MediaFlight {
    result: Mutex<Option<Result<MediaGrant, String>>>,
    ready: Condvar,
    followers: AtomicUsize,
}

impl MediaFlight {
    fn wait(&self) -> Result<MediaGrant, String> {
        let mut result = lock_recovering(&self.result, "media materialization result");
        loop {
            if let Some(result) = result.as_ref() {
                return result.clone();
            }
            result = self.ready.wait(result).unwrap_or_else(|poisoned| {
                tracing::warn!("Recovering poisoned media materialization wait");
                poisoned.into_inner()
            });
        }
    }

    fn complete(&self, result: Result<MediaGrant, String>) {
        *lock_recovering(&self.result, "media materialization result") = Some(result);
        self.ready.notify_all();
    }
}

#[derive(Debug, Default)]
struct MediaCoordinatorState {
    flights: HashMap<MediaMaterializationKey, Arc<MediaFlight>>,
}

enum MediaFlightClaim {
    Leader(Arc<MediaFlight>),
    Follower(Arc<MediaFlight>),
}

/// Process-local, bounded single-flight coordinator for cache construction. The command moves this
/// synchronous operation onto Tokio's blocking pool. Exact duplicate requests wait for one result;
/// only two distinct source identities may decode/copy concurrently. Registry locks cover lookup,
/// reservation and publication only, never source metadata, whole-file I/O, decode, hashing or flush.
#[derive(Debug, Default)]
pub(crate) struct MediaMaterializationCoordinator {
    state: Mutex<MediaCoordinatorState>,
}

impl MediaMaterializationCoordinator {
    pub(crate) fn register_verified(
        &self,
        registry: &Arc<Mutex<MediaRegistry>>,
        data_dir: &Path,
        source: ValidatedMediaSource,
    ) -> Result<MediaGrant, String> {
        self.register_with(
            registry,
            data_dir,
            source.source_path,
            Some(source.audio_content_hash),
            materialize_reserved_source,
        )
    }

    pub(crate) fn register_unverified(
        &self,
        registry: &Arc<Mutex<MediaRegistry>>,
        data_dir: &Path,
        canonical_source_path: PathBuf,
    ) -> Result<MediaGrant, String> {
        self.register_with(registry, data_dir, canonical_source_path, None, materialize_reserved_source)
    }

    fn register_with<F>(
        &self,
        registry: &Arc<Mutex<MediaRegistry>>,
        data_dir: &Path,
        source_path: PathBuf,
        expected_audio_content_hash: Option<String>,
        materialize: F,
    ) -> Result<MediaGrant, String>
    where
        F: FnOnce(&MediaMaterializationPlan) -> Result<PreparedMediaMaterialization, String>,
    {
        self.register_with_cleanup(
            registry,
            data_dir,
            source_path,
            expected_audio_content_hash,
            materialize,
            |artifacts| cleanup_retired_media_artifacts(artifacts, "retired"),
        )
    }

    fn register_with_cleanup<F, C>(
        &self,
        registry: &Arc<Mutex<MediaRegistry>>,
        data_dir: &Path,
        source_path: PathBuf,
        expected_audio_content_hash: Option<String>,
        materialize: F,
        cleanup: C,
    ) -> Result<MediaGrant, String>
    where
        F: FnOnce(&MediaMaterializationPlan) -> Result<PreparedMediaMaterialization, String>,
        C: Fn(Vec<RetiredMediaArtifact>),
    {
        let (existing, retired) = {
            let mut registry = lock_recovering(registry, "media registry");
            registry.prune_expired();
            let grant = registry.existing_grant_for_source(&source_path, expected_audio_content_hash.as_deref());
            (grant, registry.take_retired_artifacts())
        };
        cleanup(retired);
        if let Some(grant) = existing {
            return Ok(grant);
        }

        let key = MediaMaterializationKey {
            source_path: source_path.clone(),
            expected_audio_content_hash: expected_audio_content_hash.clone(),
        };
        let flight = match self.claim(key.clone())? {
            MediaFlightClaim::Leader(flight) => flight,
            MediaFlightClaim::Follower(flight) => return flight.wait(),
        };

        // Recheck under the registry mutex after winning the flight. A prior flight may have
        // published between the optimistic lookup and this claim; in that case no new file is built.
        let result = {
            let (reservation, retired) = {
                let mut registry = lock_recovering(registry, "media registry");
                let reservation = registry.reserve_materialization(data_dir, source_path, expected_audio_content_hash);
                (reservation, registry.take_retired_artifacts())
            };
            cleanup(retired);
            match reservation {
                MediaRegistrationReservation::Existing(grant) => Ok(grant),
                MediaRegistrationReservation::Build(plan) => {
                    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| materialize(&plan)));
                    match attempt {
                        Ok(Ok(prepared)) => {
                            let (grant, retired) = {
                                let mut registry = lock_recovering(registry, "media registry");
                                let grant = registry.publish_materialization(prepared);
                                (grant, registry.take_retired_artifacts())
                            };
                            cleanup(retired);
                            Ok(grant)
                        }
                        Ok(Err(error)) => {
                            let retired = {
                                let mut registry = lock_recovering(registry, "media registry");
                                registry.abort_materialization(&plan);
                                registry.take_retired_artifacts()
                            };
                            cleanup(retired);
                            Err(error)
                        }
                        Err(_) => {
                            let retired = {
                                let mut registry = lock_recovering(registry, "media registry");
                                registry.abort_materialization(&plan);
                                registry.take_retired_artifacts()
                            };
                            cleanup(retired);
                            Err("Media cache materialization failed unexpectedly before publication".to_string())
                        }
                    }
                }
            }
        };

        flight.complete(result.clone());
        self.finish(&key, &flight);
        result
    }

    fn claim(&self, key: MediaMaterializationKey) -> Result<MediaFlightClaim, String> {
        let mut state = lock_recovering(&self.state, "media materialization coordinator");
        if let Some(flight) = state.flights.get(&key) {
            if !try_increment_below(&flight.followers, MAX_MEDIA_FLIGHT_FOLLOWERS) {
                return Err(format!(
                    "{MEDIA_MATERIALIZATION_BUSY_CODE}: This audio file is already being prepared and its retry queue is full. Wait for it to finish, then retry."
                ));
            }
            return Ok(MediaFlightClaim::Follower(Arc::clone(flight)));
        }
        if state.flights.len() >= MAX_DISTINCT_MEDIA_MATERIALIZATIONS {
            return Err(format!(
                "{MEDIA_MATERIALIZATION_BUSY_CODE}: Two different audio files are already being prepared. Wait for one to finish, then retry."
            ));
        }
        let flight = Arc::new(MediaFlight::default());
        state.flights.insert(key, Arc::clone(&flight));
        Ok(MediaFlightClaim::Leader(flight))
    }

    fn finish(&self, key: &MediaMaterializationKey, flight: &Arc<MediaFlight>) {
        let mut state = lock_recovering(&self.state, "media materialization coordinator");
        if state.flights.get(key).is_some_and(|current| Arc::ptr_eq(current, flight)) {
            state.flights.remove(key);
        }
    }
}

fn lock_recovering<'a, T>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned {name} lock");
        poisoned.into_inner()
    })
}

enum MediaRegistrationReservation {
    Existing(MediaGrant),
    Build(MediaMaterializationPlan),
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
    #[cfg(test)]
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
    pub(crate) fn validate_playback_source(
        db: &Database,
        requested_path: &str,
    ) -> Result<ValidatedMediaSource, String> {
        let canonical = validate::validate_file_path(requested_path)?;
        let stored_path = if db
            .get_segment_by_audio_path(&canonical)
            .map_err(|error| format!("Media path check failed: {error}"))?
            .is_some()
        {
            canonical.as_str()
        } else if requested_path != canonical
            && db
                .get_segment_by_audio_path(requested_path)
                .map_err(|error| format!("Media path check (original) failed: {error}"))?
                .is_some()
        {
            requested_path
        } else {
            return Err("Media playback is limited to files already imported into this dataset".to_string());
        };
        let audio_content_hash = db
            .source_audio_content_hash(stored_path)
            .map_err(|error| format!("Media identity check failed: {error}"))?
            .ok_or_else(|| {
                "This imported recording has no verified audio identity; repair its fingerprint before playback"
                    .to_string()
            })?;
        Ok(ValidatedMediaSource { source_path: PathBuf::from(canonical), audio_content_hash })
    }

    /// Grant + cache an ALREADY-validated source path. Touches NO database, so the full-file copy is
    /// safe to run without the db lock held. Only the media-registry mutex serializes concurrent
    /// callers, which does not contend with get_segments.
    #[cfg(test)]
    pub fn register_cached(&mut self, data_dir: &Path, canonical: &str) -> Result<MediaGrant, String> {
        self.grant_source(data_dir, PathBuf::from(canonical))
    }

    /// NO db lock: copy the source into the media cache and grant a TTL token. The expensive
    /// `std::fs::copy` runs here, so this MUST be called with the global db Mutex released (see
    /// [`validate_source`] / [`ensure_imported`]).
    #[cfg(test)]
    pub fn grant_source(&mut self, data_dir: &Path, source_path: PathBuf) -> Result<MediaGrant, String> {
        self.grant_source_inner(data_dir, source_path, None)
    }

    /// Materialize a private immutable cache image and prove its decoded PCM is exactly the
    /// source-level identity stored at import. Only grants minted here can authorize policy-4
    /// desktop playback evidence.
    #[cfg(test)]
    pub(crate) fn grant_verified_source(
        &mut self,
        data_dir: &Path,
        source: ValidatedMediaSource,
    ) -> Result<MediaGrant, String> {
        self.grant_source_inner(data_dir, source.source_path, Some(source.audio_content_hash))
    }

    #[cfg(test)]
    fn grant_source_inner(
        &mut self,
        data_dir: &Path,
        source_path: PathBuf,
        expected_audio_content_hash: Option<String>,
    ) -> Result<MediaGrant, String> {
        let reservation = self.reserve_materialization(data_dir, source_path, expected_audio_content_hash);
        cleanup_retired_media_artifacts(self.take_retired_artifacts(), "retired test grant");
        match reservation {
            MediaRegistrationReservation::Existing(grant) => Ok(grant),
            MediaRegistrationReservation::Build(plan) => match materialize_reserved_source(&plan) {
                Ok(prepared) => {
                    let grant = self.publish_materialization(prepared);
                    cleanup_retired_media_artifacts(self.take_retired_artifacts(), "redundant test materialization");
                    Ok(grant)
                }
                Err(error) => {
                    self.abort_materialization(&plan);
                    cleanup_retired_media_artifacts(self.take_retired_artifacts(), "failed test materialization");
                    Err(error)
                }
            },
        }
    }

    /// Reserve one unpublished UUID/path under the short registry mutex. There is deliberately no
    /// filesystem access here: metadata, free-space probing, directory creation, decode/copy, hash
    /// verification and flush all happen after the caller releases this mutex.
    fn reserve_materialization(
        &mut self,
        data_dir: &Path,
        source_path: PathBuf,
        expected_audio_content_hash: Option<String>,
    ) -> MediaRegistrationReservation {
        self.prune_expired();
        if let Some(grant) = self.existing_grant_for_source(&source_path, expected_audio_content_hash.as_deref()) {
            return MediaRegistrationReservation::Existing(grant);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let cache_dir = media_cache_dir(data_dir);
        let cached_path = if expected_audio_content_hash.is_some() {
            cache_dir.join(format!("{id}.wav"))
        } else {
            let extension = source_path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(validate::sanitize_filename)
                .filter(|extension| !extension.is_empty())
                .unwrap_or_else(|| "audio".to_string());
            cache_dir.join(format!("{id}.{extension}"))
        };
        self.materializing_paths.insert(cached_path.clone());
        MediaRegistrationReservation::Build(MediaMaterializationPlan {
            id,
            source_path,
            cached_path,
            cache_dir,
            expected_audio_content_hash,
        })
    }

    /// Atomically expose one fully materialized and sealed file to grant resolution. If another
    /// caller published the exact same source identity first, discard this redundant unpublished
    /// artifact and return the already-authoritative grant.
    fn publish_materialization(&mut self, prepared: PreparedMediaMaterialization) -> MediaGrant {
        self.materializing_paths.remove(&prepared.cached_path);
        self.prune_expired();
        if let Some(existing) =
            self.existing_grant_for_source(&prepared.source_path, prepared.expected_audio_content_hash.as_deref())
        {
            self.retired_artifacts.push(RetiredMediaArtifact::from_prepared(prepared));
            return existing;
        }

        tracing::debug!(
            "media cache: materialized and sealed {} MB from {} MB source",
            prepared.materialized_bytes / 1_048_576,
            prepared.source_bytes / 1_048_576
        );
        let expires_at = Utc::now() + Duration::minutes(MEDIA_TTL_MINUTES);
        let grant = MediaGrant {
            id: prepared.id.clone(),
            path: prepared.cached_path.to_string_lossy().to_string(),
            expires_at: expires_at.to_rfc3339(),
        };
        self.grants.insert(
            prepared.id,
            GrantRecord {
                source_path: prepared.source_path,
                cached_path: prepared.cached_path,
                verified_audio_content_hash: prepared.expected_audio_content_hash,
                _cache_guard: Arc::new(prepared.cache_guard),
                _source_guard: prepared.source_guard.map(Arc::new),
                expires_at,
            },
        );
        grant
    }

    fn abort_materialization(&mut self, plan: &MediaMaterializationPlan) {
        self.materializing_paths.remove(&plan.cached_path);
        self.retired_artifacts.push(RetiredMediaArtifact::from_path(plan.cached_path.clone()));
    }

    pub(crate) fn take_retired_artifacts(&mut self) -> Vec<RetiredMediaArtifact> {
        std::mem::take(&mut self.retired_artifacts)
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

    pub(crate) fn playback_binding(&mut self, id: &str) -> Result<MediaGrantBinding, String> {
        self.prune_expired();
        let record = self.grants.get_mut(id).ok_or_else(|| "Media grant is missing or expired".to_string())?;
        if !record.cached_path.exists() {
            return Err("Cached media file is missing".to_string());
        }
        let audio_content_hash = record.verified_audio_content_hash.clone().ok_or_else(|| {
            "Media grant has no verified audio identity; reload the clip through the review workstation".to_string()
        })?;
        let source_guard = record._source_guard.as_ref().ok_or_else(|| {
            "Verified media grant lost its immutable source lease; reload the clip through the review workstation"
                .to_string()
        })?;
        record.expires_at = Utc::now() + Duration::minutes(MEDIA_TTL_MINUTES);
        Ok(MediaGrantBinding {
            source_path: record.source_path.clone(),
            audio_content_hash,
            _cache_guard: Arc::clone(&record._cache_guard),
            _source_guard: Arc::clone(source_guard),
        })
    }

    fn existing_grant_for_source(
        &mut self,
        source_path: &Path,
        expected_audio_content_hash: Option<&str>,
    ) -> Option<MediaGrant> {
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
                self.retired_artifacts.push(RetiredMediaArtifact::from_grant(record));
            }
        }

        // Find a live grant for this source and REFRESH its TTL on reuse — re-registering the same
        // clip (e.g. re-selecting a segment) means it's still in use, so it must keep the clip alive,
        // not return a grant that is about to expire.
        self.grants.iter_mut().find_map(|(id, record)| {
            if record.source_path == source_path
                && record.verified_audio_content_hash.as_deref() == expected_audio_content_hash
                && record.expires_at > now
                && record.cached_path.exists()
            {
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
                self.retired_artifacts.push(RetiredMediaArtifact::from_grant(record));
            }
        }
    }
}

/// Remove cache artifacts from the previous process generation before the WebView can issue media
/// commands. Runtime materializers never perform directory-wide pruning: doing that from a temporary
/// registry or a parallel builder can delete another in-flight or live grant. Known expired grants
/// are queued individually by [`MediaRegistry::prune_expired`] and deleted after its mutex is released.
pub(crate) fn prune_media_cache_on_startup(cache_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() {
            remove_cached_media_file(&path, "startup-orphaned");
        }
    }
}

/// Perform every blocking/expensive step for one reserved path without holding the registry mutex.
/// The returned object owns both immutable handles and is still unpublished; only
/// [`MediaRegistry::publish_materialization`] can make its UUID resolvable.
fn materialize_reserved_source(plan: &MediaMaterializationPlan) -> Result<PreparedMediaMaterialization, String> {
    std::fs::create_dir_all(&plan.cache_dir).map_err(|error| format!("Create media cache: {error}"))?;
    let source_bytes =
        std::fs::metadata(&plan.source_path).map_err(|error| format!("Read media source metadata: {error}"))?.len();
    let (materialized_bytes, source_guard, parent_target_guard) = if plan.expected_audio_content_hash.is_some() {
        // Review authority always serves one canonical 16 kHz mono PCM timeline produced by the
        // same decoder that minted the imported content identity and source-span coordinates.
        #[cfg(test)]
        {
            let (bytes, guard) =
                materialize_canonical_review_wav(&plan.source_path, &plan.cached_path, &plan.cache_dir)?;
            (bytes, Some(guard), None::<std::fs::File>)
        }
        #[cfg(not(test))]
        {
            let expected = plan
                .expected_audio_content_hash
                .as_deref()
                .ok_or_else(|| "Verified media materialization lost its expected audio identity".to_string())?;
            // Hold the imported source path/inode immutable for the entire parent-side protocol.
            // The child gets a separate read-only handle; FILE_SHARE_READ lets that reader in while
            // refusing writers, replacement, rename and deletion on supported Windows systems.
            let guard = open_immutable_source_guard(&plan.source_path)?;
            let target_guard = create_parent_owned_worker_target(&plan.cached_path)?;
            let raw_before = crate::media_materialization_worker::raw_file_blake3_before(
                &plan.source_path,
                std::time::Instant::now() + std::time::Duration::from_secs(300),
            )?;
            let response = crate::media_materialization_worker::materialize_contained(
                &plan.source_path,
                &plan.cached_path,
                expected,
                &raw_before,
            )?;
            let raw_after = crate::media_materialization_worker::raw_file_blake3_before(
                &plan.source_path,
                std::time::Instant::now() + std::time::Duration::from_secs(300),
            )?;
            if response.source_raw_blake3_after != raw_before || raw_after != raw_before {
                return Err("The media source bytes changed across contained canonicalization".to_string());
            }
            if response.canonical_pcm_blake3 != expected {
                return Err("Contained canonical media did not match the imported audio identity".to_string());
            }
            let target_bytes = std::fs::metadata(&plan.cached_path)
                .map_err(|error| format!("Inspect contained canonical media output: {error}"))?
                .len();
            if target_bytes != response.output_bytes
                || target_bytes > crate::media_materialization_worker::MAX_CANONICAL_REVIEW_WAV_BYTES
            {
                return Err("Contained canonical media output failed its fixed size contract".to_string());
            }
            (response.output_bytes, Some(guard), Some(target_guard))
        }
    } else {
        // A hard link is not an immutable playback image: an in-place edit of the owner's source
        // mutates the cache inode too. Ordinary Library playback receives an independent byte copy.
        let copy_guard = copy_into_cache(&plan.source_path, &plan.cached_path, source_bytes, &plan.cache_dir)?;
        drop(copy_guard);
        (source_bytes, None, None::<std::fs::File>)
    };

    // Close the write handle, then seal and verify the exact private artifact. Nothing is visible in
    // the registry yet, so any failure leaves no grant and the caller removes the reserved file.
    let cache_guard = open_immutable_cache_guard(&plan.cached_path)?;
    if let Some(expected) = plan.expected_audio_content_hash.as_deref() {
        let actual = canonical_review_wav_pcm_blake3_before(
            &plan.cached_path,
            std::time::Instant::now() + std::time::Duration::from_secs(300),
        )
        .map_err(|error| format!("Verify cached media audio identity: {error}"))?;
        if actual != expected {
            return Err(
                "The source audio no longer matches its imported identity; re-import or restore the original recording"
                    .to_string(),
            );
        }
    }
    // This parent-owned read lease denied deletion/replacement from target creation through child
    // completion and final verification. The immutable cache lease now assumes that responsibility.
    drop(parent_target_guard);

    Ok(PreparedMediaMaterialization {
        id: plan.id.clone(),
        source_path: plan.source_path.clone(),
        cached_path: plan.cached_path.clone(),
        expected_audio_content_hash: plan.expected_audio_content_hash.clone(),
        source_bytes,
        materialized_bytes,
        cache_guard,
        source_guard,
    })
}

/// Create the only path the contained decoder may write. The worker is required to find this exact
/// UUID target already present and empty; it is never allowed to choose or create an output path.
#[cfg(not(test))]
fn create_parent_owned_worker_target(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    let creator =
        options.open(path).map_err(|error| format!("Create private canonical media worker target: {error}"))?;
    creator.sync_all().map_err(|error| format!("Flush private canonical media worker target: {error}"))?;

    let mut guard_options = std::fs::OpenOptions::new();
    guard_options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        // Omit FILE_SHARE_DELETE so the path/inode cannot be renamed, deleted, or replaced. Sharing
        // writes is limited to admitting the contained worker onto this unpredictable UUID target.
        guard_options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    let guard =
        guard_options.open(path).map_err(|error| format!("Seal private canonical media worker target: {error}"))?;
    drop(creator);
    Ok(guard)
}

/// Decode one imported source into the exact browser-facing review timeline. The source remains
/// read-share-only for the complete decode on Windows, so an editor, sync client, or attacker cannot
/// change/replace it between validation and the final PCM hash. Output is streamed; even multi-hour
/// recordings never accumulate their decoded samples in memory.
#[cfg(test)]
fn materialize_canonical_review_wav(
    source: &Path,
    cached: &Path,
    cache_dir: &Path,
) -> Result<(u64, std::fs::File), String> {
    materialize_canonical_review_wav_inner(source, cached, cache_dir, false, None)
}

/// Worker-only entry point. The parent already created the private target, so opening any other
/// path or creating a replacement is forbidden. The parent process owns the external kill timeout;
/// this internal deadline also bounds cooperative decode/hash work inside a healthy worker.
pub(crate) fn materialize_canonical_review_wav_into_parent_target(
    source: &Path,
    cached: &Path,
    cache_dir: &Path,
    deadline: std::time::Instant,
) -> Result<u64, String> {
    let (bytes, source_guard) =
        materialize_canonical_review_wav_inner(source, cached, cache_dir, true, Some(deadline))?;
    drop(source_guard);
    Ok(bytes)
}

fn materialize_canonical_review_wav_inner(
    source: &Path,
    cached: &Path,
    cache_dir: &Path,
    target_already_exists: bool,
    deadline: Option<std::time::Instant>,
) -> Result<(u64, std::fs::File), String> {
    let source_guard = open_immutable_source_guard(source)?;
    ensure_media_deadline(deadline, "before duration probe")?;
    let duration_ms = crate::audio::get_duration_ms(source)
        .map_err(|error| format!("Read review media duration before canonicalization: {error}"))?;
    ensure_media_deadline(deadline, "after duration probe")?;
    if duration_ms <= 0 {
        return Err("Review media has no positive decoded duration".to_string());
    }
    // 16 kHz * 1 channel * 16 bits = 32 bytes/ms. One second covers rounding/codec padding while
    // the fixed margin protects the SQLite/WAL volume. The streaming writer still propagates any
    // real disk-full error and its caller removes the unpublished partial artifact.
    let estimated_bytes = u64::try_from(duration_ms)
        .unwrap_or(u64::MAX)
        .saturating_add(1_000)
        .saturating_mul(u64::from(crate::audio::TARGET_SAMPLE_RATE) * 2)
        .saturating_div(1_000)
        .saturating_add(44);
    if estimated_bytes > crate::media_materialization_worker::MAX_CANONICAL_REVIEW_WAV_BYTES {
        return Err("Review media exceeds the fixed 24-hour canonical output limit".to_string());
    }
    ensure_cache_room(estimated_bytes, crate::health::free_disk_bytes_for(cache_dir))?;

    let mut destination_options = std::fs::OpenOptions::new();
    destination_options.write(true);
    if target_already_exists {
        destination_options.truncate(true);
    } else {
        destination_options.create_new(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        destination_options.share_mode(FILE_SHARE_READ);
    }
    let destination =
        destination_options.open(cached).map_err(|error| format!("Create canonical review media cache: {error}"))?;
    let durable_handle =
        destination.try_clone().map_err(|error| format!("Clone canonical review media flush handle: {error}"))?;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: crate::audio::TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::new(std::io::BufWriter::new(destination), spec)
        .map_err(|error| format!("Initialize canonical review WAV: {error}"))?;
    let mut expected_offset_ms = 0_i64;
    let mut sample_count = 0_u64;
    let max_samples = crate::media_materialization_worker::MAX_CANONICAL_REVIEW_WAV_BYTES.saturating_sub(44) / 2;
    crate::audio::decode_pcm_windows(source, crate::audio::DECODE_WINDOW_MS, |window| {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(crate::error::AppError::Other(
                "Canonical review media exceeded its internal decode deadline".into(),
            ));
        }
        if window.sample_rate != crate::audio::TARGET_SAMPLE_RATE || window.offset_ms != expected_offset_ms {
            return Err(crate::error::AppError::Other(format!(
                "canonical review decoder produced a discontinuous timeline at {} ms (expected {} ms, rate {})",
                window.offset_ms, expected_offset_ms, window.sample_rate
            )));
        }
        let window_samples = i64::try_from(window.pcm.len())
            .map_err(|_| crate::error::AppError::Other("canonical review PCM window is too large".into()))?;
        expected_offset_ms = expected_offset_ms
            .checked_add(window_samples.saturating_mul(1_000) / i64::from(crate::audio::TARGET_SAMPLE_RATE))
            .ok_or_else(|| crate::error::AppError::Other("canonical review timeline overflowed".into()))?;
        sample_count = sample_count
            .checked_add(u64::try_from(window.pcm.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| crate::error::AppError::Other("canonical review sample count overflowed".into()))?;
        if sample_count > max_samples {
            return Err(crate::error::AppError::Other("Canonical review media exceeded its fixed output limit".into()));
        }
        for sample in window.pcm {
            writer.write_sample(sample).map_err(|error| {
                crate::error::AppError::Other(format!("Write canonical review WAV sample: {error}"))
            })?;
        }
        Ok(())
    })
    .map_err(|error| format!("Decode canonical review media: {error}"))?;
    if sample_count == 0 {
        return Err("Review media decoded to no PCM samples".to_string());
    }
    ensure_media_deadline(deadline, "before finalization")?;
    writer.finalize().map_err(|error| format!("Finalize canonical review WAV: {error}"))?;
    durable_handle.sync_all().map_err(|error| format!("Flush canonical review media cache: {error}"))?;
    drop(durable_handle);
    let bytes = std::fs::metadata(cached)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("Read canonical review media size: {error}"))?;
    if bytes > crate::media_materialization_worker::MAX_CANONICAL_REVIEW_WAV_BYTES {
        return Err("Canonical review WAV exceeded its fixed output limit".to_string());
    }
    ensure_media_deadline(deadline, "after durable finalization")?;
    Ok((bytes, source_guard))
}

fn ensure_media_deadline(deadline: Option<std::time::Instant>, phase: &str) -> Result<(), String> {
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        Err(format!("Canonical review media exceeded its internal deadline {phase}"))
    } else {
        Ok(())
    }
}

/// Re-derive the content identity directly from the sealed browser-facing PCM samples. Decoding this
/// already-canonical WAV through the general float decoder would quantize every integer sample a
/// second time (`900 -> 899 -> 898`) and falsely report drift. Hound reads the exact signed 16-bit
/// samples the WebView receives, while [`StreamingIdentity`] preserves the import hash layout.
#[cfg(test)]
pub(crate) fn canonical_review_wav_pcm_blake3(path: &Path) -> Result<String, String> {
    canonical_review_wav_pcm_blake3_inner(path, None)
}

pub(crate) fn canonical_review_wav_pcm_blake3_before(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<String, String> {
    canonical_review_wav_pcm_blake3_inner(path, Some(deadline))
}

fn canonical_review_wav_pcm_blake3_inner(path: &Path, deadline: Option<std::time::Instant>) -> Result<String, String> {
    ensure_media_deadline(deadline, "before canonical hash")?;
    let mut reader =
        hound::WavReader::open(path).map_err(|error| format!("Open sealed canonical review WAV: {error}"))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != crate::audio::TARGET_SAMPLE_RATE
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err("Sealed review media is not canonical 16 kHz mono signed PCM16 WAV".to_string());
    }
    let mut identity = crate::fingerprint::StreamingIdentity::new();
    let mut samples = Vec::with_capacity(8_192);
    for sample in reader.samples::<i16>() {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err("Canonical review media exceeded its internal hash deadline".to_string());
        }
        samples.push(sample.map_err(|error| format!("Read sealed canonical review PCM: {error}"))?);
        if samples.len() == samples.capacity() {
            identity.push(&samples, spec.sample_rate);
            samples.clear();
        }
    }
    if !samples.is_empty() {
        identity.push(&samples, spec.sample_rate);
    }
    ensure_media_deadline(deadline, "after canonical hash")?;
    Ok(identity.finish().content)
}

/// Hold the original source immutable while the canonical review WAV is decoded. The separate
/// decoder handle may read concurrently, but no writer/rename/delete can be admitted on Windows.
fn open_immutable_source_guard(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        options.share_mode(FILE_SHARE_READ);
    }
    options.open(path).map_err(|error| format!("Seal review media source for canonical decode: {error}"))
}

/// Put an independent, durable byte image inside the asset-protocol scope. A hard link is
/// deliberately forbidden: it aliases later in-place source edits into an already-authorized
/// playback grant. The copy is not published until its length is exact and `sync_all` succeeds.
fn copy_into_cache(source: &Path, cached: &Path, source_bytes: u64, cache_dir: &Path) -> Result<std::fs::File, String> {
    ensure_cache_room(source_bytes, crate::health::free_disk_bytes_for(cache_dir))?;
    let mut source_options = std::fs::OpenOptions::new();
    source_options.read(true);
    let mut destination_options = std::fs::OpenOptions::new();
    destination_options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        // Freeze the source for the copy and make the unpublished destination readable by the hash
        // verifier, while refusing writers, renames, and deletion on both handles.
        source_options.share_mode(FILE_SHARE_READ);
        destination_options.share_mode(FILE_SHARE_READ);
    }
    let mut source_file =
        source_options.open(source).map_err(|error| format!("Open media source for immutable copy: {error}"))?;
    let mut cached_file =
        destination_options.open(cached).map_err(|error| format!("Create private media cache: {error}"))?;
    let copied = std::io::copy(&mut source_file, &mut cached_file)
        .map_err(|error| format!("Copy media into app cache: {error}"))?;
    if copied != source_bytes {
        return Err(format!("Copy media into app cache was incomplete: expected {source_bytes} bytes, wrote {copied}"));
    }
    // Flush on the write-capable destination handle. Windows FlushFileBuffers rejects a read-only
    // handle with ERROR_ACCESS_DENIED; the previous implementation therefore broke every verified
    // production grant even though the copy itself succeeded.
    cached_file.sync_all().map_err(|error| format!("Flush copied media cache: {error}"))?;
    Ok(cached_file)
}

/// Hold the cache image under a read-only Windows sharing contract for the complete grant lifetime.
/// `FILE_SHARE_READ` permits WebView playback but rejects writers, renames, and deletion. The product
/// supports Windows 11 only; the portable fallback remains a read handle for developer builds.
fn open_immutable_cache_guard(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        options.share_mode(FILE_SHARE_READ);
    }
    options.open(path).map_err(|error| format!("Seal media cache for read-only playback: {error}"))
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

pub(crate) fn cleanup_retired_media_artifacts(artifacts: Vec<RetiredMediaArtifact>, context: &str) {
    for artifact in artifacts {
        let cached_path = artifact.cached_path.clone();
        // Drop every immutable Windows handle before attempting removal. This function is called
        // only after the registry mutex guard has left scope.
        drop(artifact);
        remove_cached_media_file(&cached_path, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SpeechSegment;
    use std::sync::{mpsc, Barrier};
    use std::time::{Duration as StdDuration, Instant};
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
            signal_anomaly_score: None,
            ..SpeechSegment::default()
        }
    }

    fn write_test_wav(path: &Path, sample: i16) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..1_600 {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn write_stereo_48khz_test_wav(path: &Path) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..4_800_i16 {
            writer.write_sample(frame.saturating_mul(2)).unwrap();
            writer.write_sample(frame.saturating_neg()).unwrap();
        }
        writer.finalize().unwrap();
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
    fn ordinary_legacy_grant_resolves_but_cannot_mint_review_authority() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("legacy-null-identity.wav");
        std::fs::write(&audio, b"legacy audio bytes").unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();

        let mut registry = MediaRegistry::default();
        let grant = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();
        assert!(Path::new(&registry.resolve(&grant.id).unwrap()).exists());
        let error = registry
            .playback_binding(&grant.id)
            .expect_err("a membership-only Library grant must never become policy-4 authority");
        assert!(error.contains("no verified audio identity"), "{error}");
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
    fn recreates_cache_after_a_retired_grant_file_is_removed() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("sample.wav");
        std::fs::write(&audio, b"audio").unwrap();

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();

        let mut registry = MediaRegistry::default();
        let first = registry.register(&db, tmp.path(), &audio.to_string_lossy()).unwrap();
        // A live Windows grant deliberately denies deletion. Retire it first, exactly as expiry does,
        // then simulate an external cleanup of the now-unleased cache artifact.
        let retired = registry.grants.remove(&first.id).unwrap();
        drop(retired);
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
    fn materializes_an_independent_flushed_copy_even_on_the_same_volume() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("book.wav");
        std::fs::write(&src, b"original-audio").unwrap();
        let cache_dir = tmp.path().join("media-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cached = cache_dir.join("clip.wav");

        copy_into_cache(&src, &cached, 14, &cache_dir).unwrap();
        assert_eq!(std::fs::read(&cached).unwrap(), b"original-audio", "cache starts byte-exact");

        // The playback image must not follow a later in-place source edit after authority was issued.
        std::fs::write(&src, b"changed").unwrap();
        assert_eq!(
            std::fs::read(&cached).unwrap(),
            b"original-audio",
            "the cached entry must remain an independent immutable snapshot"
        );
    }

    #[test]
    fn removing_the_cached_copy_never_deletes_the_source() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("book.wav");
        std::fs::write(&src, b"keep me").unwrap();
        let cache_dir = tmp.path().join("media-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cached = cache_dir.join("clip.wav");
        copy_into_cache(&src, &cached, 7, &cache_dir).unwrap();

        remove_cached_media_file(&cached, "test");
        assert!(!cached.exists(), "cache entry removed");
        assert!(src.exists(), "the source audio must survive removal of its cache copy");
        assert_eq!(std::fs::read(&src).unwrap(), b"keep me", "source content intact");
    }

    #[cfg(windows)]
    #[test]
    fn production_grant_freezes_source_identity_until_the_verified_grant_is_retired() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("verified.wav");
        write_test_wav(&audio, 900);
        let expected = crate::export_bundle::current_canonical_pcm_blake3(&audio).unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params!["seg-1", expected],
            )
            .unwrap();

        let source = MediaRegistry::validate_playback_source(&db, &audio.to_string_lossy()).unwrap();
        let mut registry = MediaRegistry::default();
        let grant = registry.grant_verified_source(tmp.path(), source).unwrap();
        let cached_before = std::fs::read(&grant.path).unwrap();
        let source_before = std::fs::read(&audio).unwrap();
        assert!(
            std::fs::write(&audio, b"different source bytes").is_err(),
            "the imported source cannot drift while its verified review grant can authorize a decision",
        );

        assert_eq!(std::fs::read(&grant.path).unwrap(), cached_before, "source edits cannot alias into the grant");
        assert_eq!(std::fs::read(&audio).unwrap(), source_before, "the imported source bytes remain the heard bytes");
        assert_eq!(
            registry.playback_binding(&grant.id).unwrap().audio_content_hash,
            expected,
            "playback authority carries the decoded identity verified from the private copy",
        );
        let retired = registry.grants.remove(&grant.id).unwrap();
        drop(retired);
        write_test_wav(&audio, -900);
        assert_ne!(
            std::fs::read(&audio).unwrap(),
            source_before,
            "retiring the review grant releases the source for ordinary owner edits",
        );
    }

    #[test]
    fn production_grant_serves_the_same_canonical_pcm_timeline_the_backend_verified() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("stereo-48khz-source.wav");
        write_stereo_48khz_test_wav(&audio);
        let expected = crate::export_bundle::current_canonical_pcm_blake3(&audio).unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params!["seg-1", expected],
            )
            .unwrap();

        let source = MediaRegistry::validate_playback_source(&db, &audio.to_string_lossy()).unwrap();
        let mut registry = MediaRegistry::default();
        let grant = registry.grant_verified_source(tmp.path(), source).unwrap();
        let cached = Path::new(&grant.path);
        assert_eq!(cached.extension().and_then(|extension| extension.to_str()), Some("wav"));
        let reader = hound::WavReader::open(cached).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1, "the WebView receives the canonical mono track");
        assert_eq!(spec.sample_rate, 16_000, "the WebView receives the canonical decoder timebase");
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(
            canonical_review_wav_pcm_blake3(cached).unwrap(),
            expected,
            "the browser-facing WAV must hash to the same decoded PCM identity stored at import",
        );
        assert_ne!(
            std::fs::read(cached).unwrap(),
            std::fs::read(&audio).unwrap(),
            "review playback must not pass the original container through to the WebView",
        );
    }

    #[test]
    fn production_grant_refuses_a_source_that_drifted_from_its_imported_pcm_identity() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("drifted.wav");
        write_test_wav(&audio, 700);
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params!["seg-1", "f".repeat(64)],
            )
            .unwrap();

        let source = MediaRegistry::validate_playback_source(&db, &audio.to_string_lossy()).unwrap();
        let mut registry = MediaRegistry::default();
        let error = registry
            .grant_verified_source(tmp.path(), source)
            .expect_err("stale imported identity must not mint a playback grant");
        assert!(error.contains("no longer matches its imported identity"), "{error}");
        let cache = media_cache_dir(tmp.path());
        assert_eq!(std::fs::read_dir(cache).unwrap().count(), 0, "failed verification leaves no cache artifact");
    }

    #[cfg(windows)]
    #[test]
    fn live_production_grant_denies_cache_rewrite_until_it_is_retired() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("sealed.wav");
        write_test_wav(&audio, 500);
        let expected = crate::export_bundle::current_canonical_pcm_blake3(&audio).unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&segment(&audio)).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params!["seg-1", expected],
            )
            .unwrap();
        let source = MediaRegistry::validate_playback_source(&db, &audio.to_string_lossy()).unwrap();
        let mut registry = MediaRegistry::default();
        let grant = registry.grant_verified_source(tmp.path(), source).unwrap();

        assert!(
            std::fs::write(&grant.path, b"forged bytes").is_err(),
            "the live read-share-only handle must reject cache mutation",
        );
        assert!(
            std::fs::remove_file(&grant.path).is_err(),
            "the live read-share-only handle must reject cache deletion",
        );
    }

    #[test]
    fn startup_cleanup_removes_only_previous_process_cache_files() {
        let tmp = TempDir::new().unwrap();
        let cache_dir = tmp.path().join("media-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let stale = cache_dir.join("stale.wav");
        std::fs::write(&stale, b"stale").unwrap();
        let nested = cache_dir.join("kept-directory");
        std::fs::create_dir_all(&nested).unwrap();
        prune_media_cache_on_startup(&cache_dir);
        assert!(!stale.exists());
        assert!(nested.exists(), "the cache janitor never recursively removes unexpected directories");
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

    #[test]
    fn exact_duplicate_concurrent_requests_build_one_verified_artifact() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("single-flight.wav");
        write_test_wav(&audio, 1_234);
        let expected = crate::export_bundle::current_canonical_pcm_blake3(&audio).unwrap();
        let source_path = std::fs::canonicalize(&audio).unwrap();
        let data_dir = tmp.path().to_path_buf();
        let registry = Arc::new(Mutex::new(MediaRegistry::default()));
        let coordinator = Arc::new(MediaMaterializationCoordinator::default());
        let starts = Arc::new(AtomicUsize::new(0));
        let launch = Arc::new(Barrier::new(9));

        let mut workers = Vec::new();
        for _ in 0..8 {
            let registry = Arc::clone(&registry);
            let coordinator = Arc::clone(&coordinator);
            let starts = Arc::clone(&starts);
            let launch = Arc::clone(&launch);
            let source_path = source_path.clone();
            let expected = expected.clone();
            let data_dir = data_dir.clone();
            workers.push(std::thread::spawn(move || {
                launch.wait();
                coordinator.register_with(&registry, &data_dir, source_path, Some(expected), |plan| {
                    starts.fetch_add(1, Ordering::SeqCst);
                    // Keep the leader in-flight long enough that all seven followers converge
                    // through the flight instead of merely reusing a finished grant.
                    std::thread::sleep(StdDuration::from_millis(75));
                    materialize_reserved_source(plan)
                })
            }));
        }
        launch.wait();
        let grants: Vec<MediaGrant> = workers.into_iter().map(|worker| worker.join().unwrap().unwrap()).collect();

        assert_eq!(starts.load(Ordering::SeqCst), 1, "one exact identity must execute one decoder/copy build");
        assert!(grants.iter().all(|grant| grant.id == grants[0].id));
        assert!(grants.iter().all(|grant| grant.path == grants[0].path));
        assert_eq!(lock_recovering(&registry, "test registry").grants.len(), 1);
        assert_eq!(std::fs::read_dir(media_cache_dir(&data_dir)).unwrap().count(), 1);
    }

    #[test]
    fn slow_verified_build_does_not_hold_the_registry_lock() {
        let tmp = TempDir::new().unwrap();
        let ordinary = tmp.path().join("already-live.wav");
        std::fs::write(&ordinary, b"small ordinary audio").unwrap();
        let slow = tmp.path().join("slow-verified.wav");
        write_test_wav(&slow, 777);
        let expected = crate::export_bundle::current_canonical_pcm_blake3(&slow).unwrap();
        let slow = std::fs::canonicalize(slow).unwrap();
        let data_dir = tmp.path().to_path_buf();
        let registry = Arc::new(Mutex::new(MediaRegistry::default()));
        let existing = {
            let mut registry_guard = lock_recovering(&registry, "test registry");
            registry_guard.grant_source(&data_dir, std::fs::canonicalize(&ordinary).unwrap()).unwrap()
        };
        let coordinator = Arc::new(MediaMaterializationCoordinator::default());
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);

        let worker_registry = Arc::clone(&registry);
        let worker_coordinator = Arc::clone(&coordinator);
        let worker_data_dir = data_dir.clone();
        let worker = std::thread::spawn(move || {
            worker_coordinator.register_with(&worker_registry, &worker_data_dir, slow, Some(expected), |plan| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                materialize_reserved_source(plan)
            })
        });
        started_rx.recv_timeout(StdDuration::from_secs(5)).expect("slow builder reached off-lock work");

        let resolve_started = Instant::now();
        let resolved = lock_recovering(&registry, "test registry").resolve(&existing.id).unwrap();
        let resolve_elapsed = resolve_started.elapsed();
        assert_eq!(resolved, existing.path);
        assert!(
            resolve_elapsed < StdDuration::from_millis(100),
            "unrelated live-grant resolution waited {resolve_elapsed:?} while a decoder was blocked off-lock"
        );

        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn slow_failed_artifact_cleanup_does_not_hold_the_registry_lock() {
        let tmp = TempDir::new().unwrap();
        let ordinary = tmp.path().join("cleanup-unrelated.wav");
        std::fs::write(&ordinary, b"small ordinary audio").unwrap();
        let failing = tmp.path().join("cleanup-failing.wav");
        write_test_wav(&failing, 909);
        let expected = crate::export_bundle::current_canonical_pcm_blake3(&failing).unwrap();
        let failing = std::fs::canonicalize(failing).unwrap();
        let data_dir = tmp.path().to_path_buf();
        let registry = Arc::new(Mutex::new(MediaRegistry::default()));
        let existing = {
            let mut registry_guard = lock_recovering(&registry, "test registry");
            registry_guard.grant_source(&data_dir, std::fs::canonicalize(&ordinary).unwrap()).unwrap()
        };
        let coordinator = Arc::new(MediaMaterializationCoordinator::default());
        let (cleanup_started_tx, cleanup_started_rx) = mpsc::sync_channel(1);
        let (release_cleanup_tx, release_cleanup_rx) = mpsc::sync_channel(1);

        let worker_registry = Arc::clone(&registry);
        let worker_coordinator = Arc::clone(&coordinator);
        let worker_data_dir = data_dir.clone();
        let worker = std::thread::spawn(move || {
            worker_coordinator.register_with_cleanup(
                &worker_registry,
                &worker_data_dir,
                failing,
                Some(expected),
                |plan| {
                    std::fs::create_dir_all(&plan.cache_dir).unwrap();
                    std::fs::write(&plan.cached_path, b"injected partial").unwrap();
                    Err("injected failure before cleanup".to_string())
                },
                |paths| {
                    if paths.is_empty() {
                        return;
                    }
                    cleanup_started_tx.send(()).unwrap();
                    release_cleanup_rx.recv().unwrap();
                    cleanup_retired_media_artifacts(paths, "injected slow cleanup");
                },
            )
        });
        cleanup_started_rx.recv_timeout(StdDuration::from_secs(5)).expect("failure cleanup reached its off-lock phase");

        let resolve_started = Instant::now();
        let resolved = lock_recovering(&registry, "test registry").resolve(&existing.id).unwrap();
        let resolve_elapsed = resolve_started.elapsed();
        assert_eq!(resolved, existing.path);
        assert!(
            resolve_elapsed < StdDuration::from_millis(100),
            "unrelated resolution waited {resolve_elapsed:?} while file cleanup was deliberately blocked"
        );

        release_cleanup_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap().unwrap_err(), "injected failure before cleanup");
        assert_eq!(std::fs::read_dir(media_cache_dir(&data_dir)).unwrap().count(), 1);
    }

    #[test]
    fn third_distinct_materialization_fails_closed_while_two_are_active() {
        let tmp = TempDir::new().unwrap();
        let mut sources = Vec::new();
        for (index, sample) in [101_i16, 202_i16, 303_i16].into_iter().enumerate() {
            let path = tmp.path().join(format!("bounded-{index}.wav"));
            write_test_wav(&path, sample);
            sources.push((
                std::fs::canonicalize(&path).unwrap(),
                crate::export_bundle::current_canonical_pcm_blake3(&path).unwrap(),
            ));
        }
        let data_dir = tmp.path().to_path_buf();
        let registry = Arc::new(Mutex::new(MediaRegistry::default()));
        let coordinator = Arc::new(MediaMaterializationCoordinator::default());
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let mut workers = Vec::new();

        for (source_path, expected) in sources.iter().take(2).cloned() {
            let registry = Arc::clone(&registry);
            let coordinator = Arc::clone(&coordinator);
            let gate = Arc::clone(&gate);
            let started_tx = started_tx.clone();
            let data_dir = data_dir.clone();
            workers.push(std::thread::spawn(move || {
                coordinator.register_with(&registry, &data_dir, source_path, Some(expected), |plan| {
                    started_tx.send(()).unwrap();
                    let (open, ready) = &*gate;
                    let mut open = lock_recovering(open, "test build gate");
                    while !*open {
                        open = ready.wait(open).unwrap();
                    }
                    materialize_reserved_source(plan)
                })
            }));
        }
        for _ in 0..2 {
            started_rx.recv_timeout(StdDuration::from_secs(5)).expect("both allowed builders must start");
        }

        let rejected = coordinator
            .register_with(&registry, &data_dir, sources[2].0.clone(), Some(sources[2].1.clone()), |_| {
                panic!("an overloaded third identity must never execute materialization")
            })
            .expect_err("the third distinct build must fail closed");
        assert!(rejected.starts_with(MEDIA_MATERIALIZATION_BUSY_CODE), "{rejected}");
        {
            let registry = lock_recovering(&registry, "test registry");
            assert!(registry.grants.is_empty(), "in-flight files are not published early");
            assert_eq!(registry.materializing_paths.len(), 2, "only the two admitted builds own reserved paths");
        }

        let (open, ready) = &*gate;
        *lock_recovering(open, "test build gate") = true;
        ready.notify_all();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert_eq!(lock_recovering(&registry, "test registry").grants.len(), 2);
    }

    #[test]
    fn failed_or_panicked_build_never_publishes_or_leaves_a_partial_file() {
        let tmp = TempDir::new().unwrap();
        let audio = tmp.path().join("fail-before-publish.wav");
        write_test_wav(&audio, 404);
        let expected = crate::export_bundle::current_canonical_pcm_blake3(&audio).unwrap();
        let source_path = std::fs::canonicalize(audio).unwrap();
        let data_dir = tmp.path().to_path_buf();
        let registry = Arc::new(Mutex::new(MediaRegistry::default()));
        let coordinator = MediaMaterializationCoordinator::default();

        let failure = coordinator
            .register_with(&registry, &data_dir, source_path.clone(), Some(expected.clone()), |plan| {
                std::fs::create_dir_all(&plan.cache_dir).unwrap();
                std::fs::write(&plan.cached_path, b"partial unpublished bytes").unwrap();
                Err("injected materialization failure".to_string())
            })
            .expect_err("injected failure must surface");
        assert_eq!(failure, "injected materialization failure");
        {
            let registry = lock_recovering(&registry, "test registry");
            assert!(registry.grants.is_empty(), "no grant may name an incomplete file");
            assert!(registry.materializing_paths.is_empty(), "failed reservation must be retired");
        }
        assert_eq!(std::fs::read_dir(media_cache_dir(&data_dir)).unwrap().count(), 0);

        let panicked = coordinator
            .register_with(&registry, &data_dir, source_path.clone(), Some(expected.clone()), |plan| {
                std::fs::write(&plan.cached_path, b"partial bytes before panic").unwrap();
                panic!("injected builder panic before publication")
            })
            .expect_err("a builder panic must become a closed error");
        assert_eq!(panicked, "Media cache materialization failed unexpectedly before publication");
        {
            let registry = lock_recovering(&registry, "test registry");
            assert!(registry.grants.is_empty());
            assert!(registry.materializing_paths.is_empty());
        }
        assert_eq!(std::fs::read_dir(media_cache_dir(&data_dir)).unwrap().count(), 0);

        let recovered = coordinator
            .register_with(&registry, &data_dir, source_path, Some(expected), materialize_reserved_source)
            .expect("a clean retry must build and publish normally");
        assert!(Path::new(&recovered.path).exists());
    }

    #[test]
    fn contained_timeout_crash_and_flood_retire_partials_and_release_the_build_slot() {
        use crate::engine_runtime::{run_contained_command, ContainedCommandSpec};
        use std::process::Command;

        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let registry = Arc::new(Mutex::new(MediaRegistry::default()));
        let coordinator = MediaMaterializationCoordinator::default();

        for (index, role) in ["hang", "abnormal", "oversized"].into_iter().enumerate() {
            let audio = tmp.path().join(format!("contained-fault-{index}.wav"));
            write_test_wav(&audio, i16::try_from(index + 1).unwrap());
            let expected = crate::export_bundle::current_canonical_pcm_blake3(&audio).unwrap();
            let source_path = std::fs::canonicalize(audio).unwrap();
            let error = coordinator
                .register_with(&registry, &data_dir, source_path.clone(), Some(expected.clone()), |plan| {
                    std::fs::create_dir_all(&plan.cache_dir).unwrap();
                    std::fs::write(&plan.cached_path, b"unpublished worker partial").unwrap();
                    let mut command = Command::new(std::env::current_exe().unwrap());
                    command
                        .arg("engine_runtime::tests::contained_command_fault_helper")
                        .arg("--exact")
                        .arg("--nocapture")
                        .env("CORTEX_CONTAINED_COMMAND_DRILL", role);
                    let timeout =
                        if role == "hang" { StdDuration::from_millis(250) } else { StdDuration::from_secs(5) };
                    let boundary_error = run_contained_command(
                        command,
                        ContainedCommandSpec {
                            timeout,
                            stdin_body: Vec::new(),
                            max_stdin_bytes: 1_024,
                            max_stdout_bytes: 1_024,
                            max_stderr_bytes: 1_024,
                            process_memory_limit_bytes: Some(128 * 1024 * 1024),
                            active_process_limit: Some(1),
                        },
                    )
                    .expect_err("the injected worker fault must fail closed");
                    Err(format!("contained {role} drill: {boundary_error}"))
                })
                .expect_err("a contained worker fault must not publish a grant");
            assert!(error.contains(role), "{error}");
            {
                let registry = lock_recovering(&registry, "test registry");
                assert!(registry.materializing_paths.is_empty(), "{role} left a reserved path");
                assert!(
                    registry.grants.values().all(|grant| grant.source_path != source_path),
                    "{role} published an unverified grant"
                );
            }
            assert!(
                lock_recovering(&coordinator.state, "test coordinator").flights.is_empty(),
                "{role} permanently occupied a materialization slot"
            );
            let cache_files_before = std::fs::read_dir(media_cache_dir(&data_dir)).unwrap().count();
            assert_eq!(cache_files_before, index, "{role} left its partial target behind");

            let recovered = coordinator
                .register_with(&registry, &data_dir, source_path, Some(expected), materialize_reserved_source)
                .expect("the exact slot and identity must admit a clean build after worker failure");
            assert!(Path::new(&recovered.path).is_file());
        }
    }
}
