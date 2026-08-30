use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Recording identity for duplicate detection, in two tiers.
///
/// TIER 1 — the 64-bit spectral value ([`AudioFingerprint::fingerprint`]) is a CANDIDATE signal. It is
/// eight per-band mean energies XOR-folded into a u64, and it was never capable of proving that two
/// recordings hold the same audio: band energy is a coarse loudness envelope, the shifts discard most
/// of each band's ~30 significant bits, and two unrelated clips of the same speaker at the same level
/// in the same room can land on the same value. It is kept because it is cheap and it buckets.
///
/// TIER 2 — the blake3 [`content_hash`](AudioFingerprint::content_hash) over canonical decoded PCM plus
/// its sample rate is the DEFINITIVE key. A rejection requires a tier-2 match. Tier 1 keeps legacy
/// storage and diagnostics compatible, but admission checks tier 2 across every bucket: streamed and
/// whole-buffer decoding deliberately compute tier 1 differently, so restricting the definitive check
/// to one bucket would let identical audio through when its processing route changed.
///
/// Why this shape (external review 2026-08-06, P1.1): until v51 the tier-1 value WAS the identity, so a
/// spectral collision returned `Err("Duplicate audio content")` and a legitimate recording was refused
/// at import. That is silent loss of speech to defend against a duplicate — the wrong trade for a
/// dataset tool. The rule now is: prefer keeping a duplicate over discarding legitimate audio, and only
/// a cryptographic match may reject.
///
/// SCOPE — the map is in memory, but it is not only in memory. Migration v50 stores the spectral value
/// on the segment rows and v51 adds the content hash; `lib.rs` calls [`rehydrate`](AudioFingerprint::rehydrate)
/// at startup, so duplicate detection survives a restart.
///
/// The remaining gap is honest and bounded, and is the same one v50 left: a row stored before v51 has a
/// spectral value but NO content hash, and a value that cannot distinguish content must never reject —
/// so those entries are loaded, are visible to `count()`, and NEVER cause a rejection until
/// `backfill_fingerprints` decodes their audio and writes the real hash. Uncovered, not wrong.
pub struct AudioFingerprint {
    /// Every index is guarded by the SAME mutex. Admission, reservation rollback and rehydration must
    /// update the spectral buckets, source lookup and definitive-content lookup atomically; separate
    /// locks would re-introduce a check-then-act window or require fragile lock ordering.
    state: Mutex<FingerprintState>,
    /// Monotonic identity for rollback-safe, in-flight import reservations. A reservation is never
    /// persisted; it only prevents two live imports from both passing duplicate admission before either
    /// database publication commits.
    next_reservation_token: AtomicU64,
}

/// In-memory acceleration for the durable database authority.
///
/// `by_spectral` preserves the legacy/candidate grouping, while `spectral_by_source` and
/// `content_refcounts` make the two production admission checks expected O(1). The old implementation
/// searched every bucket for every source and content check, making a large batch O(n^2) at the stated
/// 100,000-recording scale. Refcounts (rather than a set) preserve protection when historical duplicate
/// rows share the same content and one source is later forgotten.
#[derive(Default)]
struct FingerprintState {
    by_spectral: HashMap<u64, HashMap<String, KnownRecording>>,
    spectral_by_source: HashMap<String, u64>,
    content_refcounts: HashMap<String, usize>,
    source_by_reservation: HashMap<u64, String>,
}

impl FingerprintState {
    fn contains_source(&self, source: &str) -> bool {
        self.spectral_by_source.contains_key(source)
    }

    fn contains_content(&self, content: &str) -> bool {
        self.content_refcounts.contains_key(content)
    }

    fn increment_content(&mut self, content: &str) {
        *self.content_refcounts.entry(content.to_string()).or_insert(0) += 1;
    }

    fn decrement_content(&mut self, content: &str) {
        let remove = match self.content_refcounts.get_mut(content) {
            Some(count) if *count > 1 => {
                *count -= 1;
                false
            }
            Some(_) => true,
            None => {
                tracing::error!(content, "Audio fingerprint content index lost an entry");
                false
            }
        };
        if remove {
            self.content_refcounts.remove(content);
        }
    }

    /// Insert a source that is not already present. All secondary indexes change in one critical
    /// section, including the in-flight token index when this is a reservation.
    fn insert(&mut self, spectral: u64, recording: KnownRecording) -> bool {
        if spectral == 0 || recording.source.is_empty() || self.contains_source(&recording.source) {
            return false;
        }

        let source = recording.source.clone();
        if let Some(token) = recording.reservation_token {
            if self.source_by_reservation.contains_key(&token) {
                tracing::error!(token, "Audio fingerprint reservation token was reused");
                return false;
            }
        }
        if let Some(content) = recording.content.as_deref() {
            self.increment_content(content);
        }
        if let Some(token) = recording.reservation_token {
            self.source_by_reservation.insert(token, source.clone());
        }
        self.spectral_by_source.insert(source.clone(), spectral);
        self.by_spectral.entry(spectral).or_default().insert(source, recording);
        true
    }

    /// Remove exactly one globally unique source and every secondary-index reference to it.
    fn remove_source(&mut self, source: &str) -> Option<KnownRecording> {
        let spectral = self.spectral_by_source.remove(source)?;
        let (removed, empty_bucket) = match self.by_spectral.get_mut(&spectral) {
            Some(bucket) => {
                let removed = bucket.remove(source);
                (removed, bucket.is_empty())
            }
            None => {
                tracing::error!(spectral, source, "Audio fingerprint source index points to a missing bucket");
                return None;
            }
        };
        if empty_bucket {
            self.by_spectral.remove(&spectral);
        }
        if let Some(recording) = removed.as_ref() {
            if let Some(content) = recording.content.as_deref() {
                self.decrement_content(content);
            }
            if let Some(token) = recording.reservation_token {
                self.source_by_reservation.remove(&token);
            }
        } else {
            tracing::error!(spectral, source, "Audio fingerprint bucket lost an indexed source");
        }
        removed
    }

    fn commit_reservation(&mut self, token: u64) -> bool {
        let Some(source) = self.source_by_reservation.get(&token).cloned() else {
            return false;
        };
        let Some(&spectral) = self.spectral_by_source.get(&source) else {
            self.source_by_reservation.remove(&token);
            return false;
        };
        let committed = self
            .by_spectral
            .get_mut(&spectral)
            .and_then(|bucket| bucket.get_mut(&source))
            .filter(|recording| recording.reservation_token == Some(token))
            .map(|recording| recording.reservation_token = None)
            .is_some();
        self.source_by_reservation.remove(&token);
        committed
    }

    fn rollback_reservation(&mut self, token: u64) -> bool {
        let Some(source) = self.source_by_reservation.get(&token).cloned() else {
            return false;
        };
        let is_owner = self
            .spectral_by_source
            .get(&source)
            .and_then(|spectral| self.by_spectral.get(spectral))
            .and_then(|bucket| bucket.get(&source))
            .is_some_and(|recording| recording.reservation_token == Some(token));
        if !is_owner {
            self.source_by_reservation.remove(&token);
            return false;
        }
        self.remove_source(&source).is_some()
    }

    fn recording(&self, source: &str) -> Option<&KnownRecording> {
        let spectral = self.spectral_by_source.get(source)?;
        self.by_spectral.get(spectral)?.get(source)
    }

    fn count(&self) -> usize {
        self.spectral_by_source.len()
    }

    #[cfg(test)]
    fn assert_consistent(&self) {
        let mut expected_content = HashMap::<String, usize>::new();
        let mut expected_reservations = HashMap::<u64, String>::new();
        let mut recordings = 0usize;
        for (&spectral, bucket) in &self.by_spectral {
            assert_ne!(spectral, 0, "the degenerate spectral key must never be indexed");
            for (source, recording) in bucket {
                recordings += 1;
                assert_eq!(source, &recording.source, "bucket key and recording source diverged");
                assert_eq!(self.spectral_by_source.get(source), Some(&spectral));
                if let Some(content) = recording.content.as_ref() {
                    *expected_content.entry(content.clone()).or_insert(0) += 1;
                }
                if let Some(token) = recording.reservation_token {
                    assert!(expected_reservations.insert(token, source.clone()).is_none());
                }
            }
        }
        assert_eq!(recordings, self.spectral_by_source.len());
        assert_eq!(self.content_refcounts, expected_content);
        assert_eq!(self.source_by_reservation, expected_reservations);
    }
}

/// One recording as the dedup map knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRecording {
    /// Hex blake3 of canonical PCM. `None` for rows written before v51 — unknown content, and unknown
    /// content can never prove a duplicate.
    pub content: Option<String>,
    /// Canonicalised source path, or empty for in-memory/streaming imports with no file behind them.
    pub source: String,
    /// `Some` only while an import owns this cache entry but has not yet committed its database
    /// publication. Drop removes exactly the matching token, so a failed import cannot poison retries
    /// and cannot erase a later registration.
    reservation_token: Option<u64>,
}

/// A duplicate-admission reservation tied to one database publication attempt.
///
/// The fingerprint cache is only an index; the database is authority. The reservation is therefore
/// committed immediately after the source rows and their identity commit together. Every earlier exit
/// path drops it and removes the in-flight entry. This closes the old check/register-before-ASR gap,
/// where a champion failure left the source "already imported" in memory despite publishing no rows.
pub struct AudioImportReservation<'a> {
    owner: &'a AudioFingerprint,
    spectral: u64,
    token: Option<u64>,
    committed: bool,
}

impl AudioImportReservation<'_> {
    pub fn commit(mut self) {
        let Some(token) = self.token else {
            self.committed = true;
            return;
        };
        let mut state = self.owner.lock_state();
        if !state.commit_reservation(token) {
            // This is an internal invariant breach, not a recoverable duplicate. The database has
            // already committed, so never panic or report the import as failed after durable success.
            // Startup rehydration repairs the cache; logging keeps the breach visible meanwhile.
            tracing::error!(
                spectral = self.spectral,
                token,
                "Committed audio reservation disappeared from the fingerprint cache"
            );
        }
        self.committed = true;
    }
}

impl Drop for AudioImportReservation<'_> {
    fn drop(&mut self) {
        let Some(token) = self.token else {
            return;
        };
        if self.committed {
            return;
        }
        let mut state = self.owner.lock_state();
        if !state.rollback_reservation(token) {
            tracing::error!(
                spectral = self.spectral,
                token,
                "Rolled-back audio reservation disappeared from the fingerprint cache"
            );
        }
    }
}

/// The identity of one decoded recording: both tiers, computed together so a caller cannot persist one
/// without the other.
///
/// A named struct rather than a `(u64, String)` tuple so adding a tier later does not break every
/// destructuring site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioIdentity {
    /// Tier 1: cheap spectral bucket key. 0 means degenerate (silent / shorter than 8 frames).
    pub spectral: u64,
    /// Tier 2: hex blake3 over canonical PCM + sample rate. Definitive.
    pub content: String,
}

/// One `(spectral, content, path)` row as the database stores it, for [`AudioFingerprint::rehydrate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAudioIdentity {
    pub spectral: u64,
    /// `None` for rows written before migration v51.
    pub content: Option<String>,
    pub audio_path: String,
}

/// Feed PCM to a blake3 hasher as little-endian i16, a block at a time.
///
/// The obvious `for s in pcm { hasher.update(&s.to_le_bytes()) }` produces the identical digest and is
/// 44x slower: MEASURED 2026-08-06 on a 90-second 16 kHz window (1,440,000 samples), 195.4 ms per-sample
/// vs 4.5 ms blocked. That is ~8 seconds of pure hashing added to importing a one-hour recording, on the
/// path a user waits on, for nothing. blake3 vectorises over a block; handing it two bytes at a time
/// pays the call overhead 1.4 million times and defeats that.
///
/// Explicit `to_le_bytes` rather than a transmute of the slice: the digest is PERSISTED and compared
/// across runs, so it must not silently depend on host endianness.
fn hash_samples_le(hasher: &mut blake3::Hasher, pcm: &[i16]) {
    // 2 KiB of samples per block — comfortably inside blake3's internal buffer and a trivial stack cost.
    let mut buf = [0u8; 4096];
    for block in pcm.chunks(buf.len() / 2) {
        for (slot, sample) in buf.chunks_exact_mut(2).zip(block) {
            slot.copy_from_slice(&sample.to_le_bytes());
        }
        hasher.update(&buf[..block.len() * 2]);
    }
}

/// Builds one whole-recording [`AudioIdentity`] from audio that arrives in pieces.
///
/// The streaming import path (`process_single_file_streaming`) decodes a long file in 90-second
/// windows precisely so the whole PCM is never resident, so it cannot call
/// [`AudioFingerprint::identify`] on the file. Before v51 it therefore fingerprinted each window
/// separately and persisted NOTHING, which meant a long recording never participated in cross-session
/// duplicate detection at all.
///
/// blake3 is a streaming hash, so tier 2 costs no extra memory here: feeding the windows in order
/// produces exactly the digest [`AudioFingerprint::content_hash`] would have returned for the whole
/// file, so a recording gets the SAME content hash whichever import path handled it.
///
/// Tier 1 is the first non-degenerate window's bucket, because band energies over the whole file cannot
/// be accumulated without the whole file. A streamed and a non-streamed copy of identical audio can
/// therefore land in different tier-1 buckets. Duplicate admission consequently searches the
/// definitive tier-2 hash across all buckets; route or threshold changes cannot mint a second recording.
pub struct StreamingIdentity {
    hasher: blake3::Hasher,
    rate_mixed: bool,
    spectral: u64,
}

impl Default for StreamingIdentity {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingIdentity {
    pub fn new() -> Self {
        Self { hasher: blake3::Hasher::new(), rate_mixed: false, spectral: 0 }
    }

    /// Feed one freshly decoded window, in order. Carried-over tails must NOT be pushed twice.
    pub fn push(&mut self, pcm: &[i16], sample_rate: u32) {
        if pcm.is_empty() {
            return;
        }
        // Mixed exactly ONCE, matching content_hash's `rate || samples` layout, so a whole-file hash and
        // a windowed hash of the same canonical PCM agree.
        if !self.rate_mixed {
            self.hasher.update(&sample_rate.to_le_bytes());
            self.rate_mixed = true;
        }
        hash_samples_le(&mut self.hasher, pcm);
        if self.spectral == 0 {
            self.spectral = AudioFingerprint::fingerprint(pcm, sample_rate);
        }
    }

    pub fn finish(self) -> AudioIdentity {
        AudioIdentity { spectral: self.spectral, content: self.hasher.finalize().to_hex().to_string() }
    }
}

impl Default for AudioFingerprint {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioFingerprint {
    pub fn new() -> Self {
        Self { state: Mutex::new(FingerprintState::default()), next_reservation_token: AtomicU64::new(1) }
    }

    fn lock_state(&self) -> MutexGuard<'_, FingerprintState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned audio fingerprint cache");
            poisoned.into_inner()
        })
    }

    /// TIER 1. A 64-bit spectral-energy bucket key — a CANDIDATE signal, never an identity.
    ///
    /// Two distinct recordings CAN share this value; that is expected and handled by comparing
    /// [`content_hash`](Self::content_hash) before anything is rejected.
    pub fn fingerprint(pcm: &[i16], sample_rate: u32) -> u64 {
        if pcm.is_empty() {
            return 0;
        }

        let frame_size = (sample_rate as usize / 1000).max(32); // 1ms frames
        let num_frames = pcm.len() / frame_size;
        if num_frames == 0 {
            return 0;
        }

        let mut bands = [0u64; 8];
        let band_size = num_frames / 8;

        for (band_idx, band) in bands.iter_mut().enumerate() {
            let start = band_idx * band_size * frame_size;
            let end = ((band_idx + 1) * band_size * frame_size).min(pcm.len());
            if start >= end {
                continue;
            }

            let mut sum: i128 = 0;
            let mut count = 0;
            for &sample in &pcm[start..end] {
                let val = sample as i128;
                sum += val * val;
                count += 1;
            }

            if count > 0 {
                let energy = (sum / count as i128) as u64;
                *band = energy;
            }
        }

        let mut hash: u64 = 0;
        for (i, &band) in bands.iter().enumerate() {
            hash ^= band.wrapping_shl(i as u32 * 8);
            hash = hash.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }

        hash
    }

    /// TIER 2. The definitive content key: blake3 over the canonical decoded PCM and its sample rate.
    ///
    /// The sample rate is mixed in (as a length-prefix-free LE u32 followed by the samples, which is
    /// unambiguous because the rate is fixed-width) so the SAME sample values decoded at a different
    /// rate are a different recording rather than a false duplicate. Samples are little-endian i16, the
    /// canonical in-memory form the whole pipeline already uses, so the hash is reproducible on any
    /// machine that decodes the file the same way.
    ///
    /// blake3 rather than SHA-256 because the crate is already a dependency (`cache.rs`, `audio.rs`) and
    /// runs at GB/s — hashing a 90-second window is well under a millisecond, cheap enough to sit in the
    /// import path.
    pub fn content_hash(pcm: &[i16], sample_rate: u32) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&sample_rate.to_le_bytes());
        hash_samples_le(&mut hasher, pcm);
        hasher.finalize().to_hex().to_string()
    }

    /// Both tiers for one decoded buffer.
    pub fn identify(pcm: &[i16], sample_rate: u32) -> AudioIdentity {
        AudioIdentity { spectral: Self::fingerprint(pcm, sample_rate), content: Self::content_hash(pcm, sample_rate) }
    }

    /// Decode one source through the immutable persisted-identity protocol: fixed 90-second windows,
    /// mono 16 kHz PCM, concatenated in source order. Every database writer and verifier must use this
    /// protocol rather than whole-buffer resampling; FIR/sinc boundary state makes those byte streams
    /// differ for long 44.1/48 kHz recordings.
    pub fn identify_canonical_file(path: &Path) -> crate::error::AppResult<AudioIdentity> {
        let mut identity = StreamingIdentity::new();
        let mut saw_audio = false;
        crate::audio::decode_pcm_windows(path, crate::audio::DECODE_WINDOW_MS, |window| {
            if !window.pcm.is_empty() {
                saw_audio = true;
                identity.push(&window.pcm, window.sample_rate);
            }
            Ok(())
        })?;
        if !saw_audio {
            return Err(crate::error::AppError::Validation(format!(
                "Source audio decoded to no canonical samples: {}",
                path.display()
            )));
        }
        Ok(identity.finish())
    }

    fn source_key(source: Option<&Path>) -> String {
        source
            .map(|p| {
                std::fs::canonicalize(p)
                    .ok()
                    .map(|c| c.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.to_string_lossy().into_owned())
            })
            .unwrap_or_default()
    }

    /// The one rejection rule, shared by [`check_duplicate`](Self::check_duplicate) and
    /// [`check_and_register`](Self::check_and_register) so the two can never drift apart.
    ///
    /// Reject only when a KNOWN recording has a content hash that is present and equal. Source path does
    /// not weaken content identity: retrying the exact same source is an idempotency case for the import
    /// pipeline to adopt from durable rows, not permission to mint a second set of segment IDs.
    /// Every other case keeps the audio:
    ///   - empty source key: an in-memory/streaming import with nothing to compare paths on;
    ///   - `content: None`: a pre-v51 row whose audio was never hashed. It CANNOT prove identity, so it
    ///     does not get to reject one.
    #[cfg(test)]
    fn is_proven_duplicate(bucket: &[KnownRecording], content: &str, source_key: &str) -> bool {
        if source_key.is_empty() {
            return false;
        }
        bucket.iter().any(|known| known.content.as_deref() == Some(content))
    }

    /// Returns true if this audio is byte-identical to audio already admitted from any source.
    pub fn check_duplicate(&self, pcm: &[i16], sample_rate: u32, source: Option<&Path>) -> bool {
        let id = Self::identify(pcm, sample_rate);
        if id.spectral == 0 {
            return false; // degenerate bucket (empty/silent) — never a content key, never a dup
        }
        let source_key = Self::source_key(source);
        !source_key.is_empty() && self.lock_state().contains_content(&id.content)
    }

    /// Check and register in a single lock acquire, so the check-then-act race where a concurrent
    /// `register` overwrites the entry `check_duplicate` just tested cannot happen.
    ///
    /// Returns the recording's [`AudioIdentity`] so the caller can persist BOTH tiers — a caller that
    /// stored only the spectral value would silently re-create the pre-v51 collision bug on the next
    /// restart, because a rehydrated entry with no content hash can never reject.
    pub fn check_and_register(
        &self,
        pcm: &[i16],
        sample_rate: u32,
        source: Option<&Path>,
    ) -> Result<AudioIdentity, &'static str> {
        let id = Self::identify(pcm, sample_rate);
        let reservation = self.reserve_import_identity(&id, source)?;
        reservation.commit();
        Ok(id)
    }

    /// Reserve duplicate admission for a whole-buffer import. The caller must retain the returned guard
    /// until its database publication succeeds, then call [`AudioImportReservation::commit`].
    pub fn reserve_import(
        &self,
        pcm: &[i16],
        sample_rate: u32,
        source: Option<&Path>,
    ) -> Result<(AudioIdentity, AudioImportReservation<'_>), &'static str> {
        let id = Self::identify(pcm, sample_rate);
        let reservation = self.reserve_import_identity(&id, source)?;
        Ok((id, reservation))
    }

    /// Reserve a precomputed whole-recording identity (the bounded-memory streaming path).
    pub fn reserve_import_identity(
        &self,
        id: &AudioIdentity,
        source: Option<&Path>,
    ) -> Result<AudioImportReservation<'_>, &'static str> {
        // A spectral value of 0 is the degenerate "no usable energy bands" case — digital silence, a
        // fully-silent decode window, or a clip shorter than 8 frames (<16 ms). Such audio yields no
        // speech segments anyway, so it is neither compared nor stored.
        if id.spectral == 0 {
            return Ok(AudioImportReservation { owner: self, spectral: 0, token: None, committed: false });
        }
        let source_key = Self::source_key(source);
        if source_key.is_empty() {
            return Ok(AudioImportReservation { owner: self, spectral: id.spectral, token: None, committed: false });
        }

        let mut state = self.lock_state();
        if state.contains_source(&source_key) {
            return Err("Source audio path is already imported");
        }
        if state.contains_content(&id.content) {
            return Err("Duplicate audio content");
        }

        // Avoid the sentinel and refuse to reuse a still-live token even after the theoretical u64
        // wrap. The loop runs once in every realistic lifetime, but keeps token identity total.
        let token = loop {
            let candidate = self.next_reservation_token.fetch_add(1, Ordering::Relaxed);
            if candidate != 0 && !state.source_by_reservation.contains_key(&candidate) {
                break candidate;
            }
        };
        let inserted = state.insert(
            id.spectral,
            KnownRecording { content: Some(id.content.clone()), source: source_key, reservation_token: Some(token) },
        );
        if !inserted {
            return Err("Audio fingerprint reservation index conflict");
        }
        Ok(AudioImportReservation { owner: self, spectral: id.spectral, token: Some(token), committed: false })
    }

    /// Test-only registration helper for constructing committed cache state without a database.
    /// Production callers must retain a reservation through durable publication.
    #[cfg(test)]
    pub fn register(&self, pcm: &[i16], sample_rate: u32, source: Option<&Path>) -> AudioIdentity {
        let id = Self::identify(pcm, sample_rate);
        self.register_identity(&id, source);
        id
    }

    /// Check and register an identity that was computed elsewhere — the streaming path's
    /// [`StreamingIdentity::finish`], which never holds the whole file to hand to
    /// [`check_and_register`](Self::check_and_register).
    ///
    /// Same rejection rule as its whole-buffer sibling, and it must be CHECKED rather than merely
    /// registered: a streamed recording's per-window checks compare window hashes, which cannot match
    /// the whole-file hash that gets persisted, so registering this without testing it would persist an
    /// identity nothing ever consults — cross-session dedup for long files would look implemented and
    /// silently do nothing.
    pub fn check_and_register_identity(&self, id: &AudioIdentity, source: Option<&Path>) -> Result<(), &'static str> {
        let reservation = self.reserve_import_identity(id, source)?;
        reservation.commit();
        Ok(())
    }

    /// PRIVATE on purpose. There is deliberately NO public "register without checking" entry point for a
    /// precomputed identity: the streaming path briefly had one, and using it instead of
    /// [`check_and_register_identity`](Self::check_and_register_identity) made cross-session dedup for
    /// long files look implemented while doing nothing. An API that cannot be called wrongly beats a
    /// comment asking callers not to.
    #[cfg(test)]
    fn register_identity(&self, id: &AudioIdentity, source: Option<&Path>) {
        if id.spectral == 0 {
            return;
        }
        let source_key = Self::source_key(source);
        if source_key.is_empty() {
            return;
        }
        let mut state = self.lock_state();
        if let Some(existing) = state.recording(&source_key) {
            if existing.reservation_token.is_some() {
                tracing::error!(source = source_key, "Refusing to overwrite an in-flight audio reservation");
                return;
            }
            state.remove_source(&source_key);
        }
        let inserted = state.insert(
            id.spectral,
            KnownRecording { content: Some(id.content.clone()), source: source_key, reservation_token: None },
        );
        debug_assert!(inserted, "committed fingerprint registration must be unique by source");
    }

    /// Evict the cache entry for a source only after every database row for that source has been
    /// durably rolled back. This is intentionally source-scoped and never content-scoped: a historical
    /// duplicate under another path must remain protected.
    pub fn forget_source(&self, source: &Path) -> usize {
        let source_key = Self::source_key(Some(source));
        if source_key.is_empty() {
            return 0;
        }
        usize::from(self.lock_state().remove_source(&source_key).is_some())
    }

    /// Load stored recording identities into the map at startup (v50 spectral + v51 content hash).
    ///
    /// This is what makes duplicate detection survive a restart. Returns the number of entries ADDED.
    ///
    /// Additive, never clearing: a caller that rehydrates mid-session must not drop what this run has
    /// already registered. A stored row that upgrades a known path from `None` to a real hash is
    /// applied in place and does not count as an addition.
    pub fn rehydrate(&self, known: impl IntoIterator<Item = StoredAudioIdentity>) -> usize {
        let mut state = self.lock_state();
        let mut added = 0usize;
        for row in known {
            // Skip the degenerate value `register` refuses to store, so a legacy 0 cannot become a
            // bucket that every silent window then lands in.
            if row.spectral == 0 {
                continue;
            }
            let source_key = Self::source_key(Some(Path::new(&row.audio_path)));
            if source_key.is_empty() {
                continue;
            }
            if state.contains_source(&source_key) {
                // A live registration always wins over a stored row; only fill a genuine hash gap.
                let needs_content = state.recording(&source_key).is_some_and(|entry| entry.content.is_none());
                if needs_content {
                    if let Some(content) = row.content {
                        state.increment_content(&content);
                        let spectral = state.spectral_by_source[&source_key];
                        if let Some(existing) =
                            state.by_spectral.get_mut(&spectral).and_then(|bucket| bucket.get_mut(&source_key))
                        {
                            existing.content = Some(content);
                        } else {
                            tracing::error!(
                                spectral,
                                source = source_key,
                                "Audio fingerprint rehydrate index diverged"
                            );
                        }
                    }
                }
                continue;
            }
            if state.insert(
                row.spectral,
                KnownRecording { content: row.content, source: source_key, reservation_token: None },
            ) {
                added += 1;
            }
        }
        added
    }

    pub fn clear(&self) {
        *self.lock_state() = FingerprintState::default();
    }

    /// Number of RECORDINGS known, not buckets — a bucket may hold several distinct recordings.
    pub fn count(&self) -> usize {
        self.lock_state().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn pcm_scaled(mul: i16) -> Vec<i16> {
        (0..16_000).map(|i| (i as i16).wrapping_mul(mul)).collect()
    }

    fn stored(spectral: u64, content: Option<&str>, path: &str) -> StoredAudioIdentity {
        StoredAudioIdentity { spectral, content: content.map(str::to_string), audio_path: path.to_string() }
    }

    /// THE P1.1 REGRESSION TEST. A tier-1 collision between DISTINCT recordings must keep both.
    ///
    /// Before v51 the spectral value was the identity, so this returned Err and the second recording was
    /// refused at import — legitimate speech discarded to defend against a duplicate that was not one.
    /// The collision is forced rather than searched for: real 64-bit collisions are rare enough that
    /// hunting one would make a slow, flaky test prove the same thing.
    #[test]
    fn a_forced_spectral_collision_between_distinct_audio_keeps_both() {
        let map = AudioFingerprint::new();
        let bucket = 0xDEAD_BEEF_u64;

        // Two DIFFERENT recordings that the cheap tier-1 index happens to file together.
        let a = AudioFingerprint::content_hash(&pcm_scaled(100), 16000);
        let b = AudioFingerprint::content_hash(&pcm_scaled(77), 16000);
        assert_ne!(a, b, "the two clips really are different content");

        map.rehydrate([stored(bucket, Some(&a), r"C:\audio\a.wav")]);
        let entries: Vec<KnownRecording> = map.lock_state().by_spectral[&bucket].values().cloned().collect();
        assert!(
            !AudioFingerprint::is_proven_duplicate(&entries, &b, r"C:\audio\b.wav"),
            "distinct content sharing a spectral bucket must NOT be rejected"
        );
        // ...and the real duplicate in the same bucket still is.
        assert!(AudioFingerprint::is_proven_duplicate(&entries, &a, r"C:\audio\copy_of_a.wav"));
    }

    /// A pre-v51 row has no content hash. It must never reject, because it cannot prove identity.
    #[test]
    fn a_legacy_row_without_a_content_hash_can_never_reject() {
        let map = AudioFingerprint::new();
        let pcm = pcm_scaled(100);
        let spectral = AudioFingerprint::fingerprint(&pcm, 16000);

        // Exactly what load_audio_identities returns for a v50-era row: spectral present, content NULL.
        assert_eq!(map.rehydrate([stored(spectral, None, r"C:\audio\legacy.wav")]), 1);
        assert!(
            !map.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\new.wav"))),
            "an un-hashed legacy row must not reject a fresh import — uncovered, never wrong"
        );

        // The backfill writes the hash; from then on it protects.
        let hash = AudioFingerprint::content_hash(&pcm, 16000);
        let upgraded = AudioFingerprint::new();
        upgraded.rehydrate([stored(spectral, Some(&hash), r"C:\audio\legacy.wav")]);
        assert!(upgraded.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\new.wav"))));
    }

    #[test]
    fn content_hash_separates_what_the_spectral_value_cannot() {
        // Same energy envelope, different waveform: the tier-1 value is deliberately blind to sign, so
        // an inverted signal buckets identically while being different audio.
        let pcm: Vec<i16> = pcm_scaled(100);
        let inverted: Vec<i16> = pcm.iter().map(|s| s.wrapping_neg()).collect();
        assert_eq!(
            AudioFingerprint::fingerprint(&pcm, 16000),
            AudioFingerprint::fingerprint(&inverted, 16000),
            "tier 1 cannot tell these apart — that is the collision class this fix exists for"
        );
        assert_ne!(
            AudioFingerprint::content_hash(&pcm, 16000),
            AudioFingerprint::content_hash(&inverted, 16000),
            "tier 2 must"
        );
    }

    /// The blocked hasher must agree with the obvious per-sample reference, byte for byte.
    ///
    /// This digest is PERSISTED. If the two ever diverge, every content hash already in a user's library
    /// silently stops matching freshly computed ones — duplicate detection would quietly go dead rather
    /// than fail. Lengths straddle the 2048-sample block so a partial final block is covered, and an odd
    /// length proves the tail is not over- or under-fed.
    #[test]
    fn blocked_hashing_matches_the_per_sample_reference() {
        fn reference(pcm: &[i16], sample_rate: u32) -> String {
            let mut h = blake3::Hasher::new();
            h.update(&sample_rate.to_le_bytes());
            for s in pcm {
                h.update(&s.to_le_bytes());
            }
            h.finalize().to_hex().to_string()
        }
        for len in [0usize, 1, 2047, 2048, 2049, 4096, 5000] {
            // Full-range samples, computed in i32 so the fixture itself cannot overflow i16.
            let pcm: Vec<i16> = (0..len).map(|i| (((i * 7919) % 40_000) as i32 - 20_000) as i16).collect();
            assert_eq!(
                AudioFingerprint::content_hash(&pcm, 16000),
                reference(&pcm, 16000),
                "blocked and per-sample hashing disagree at len {len}"
            );
        }
    }

    #[test]
    fn content_hash_includes_the_sample_rate() {
        let pcm = pcm_scaled(100);
        assert_ne!(
            AudioFingerprint::content_hash(&pcm, 16000),
            AudioFingerprint::content_hash(&pcm, 48000),
            "the same samples at a different rate are different audio, not a duplicate"
        );
    }

    #[test]
    fn duplicate_detection_survives_a_restart_once_identities_are_rehydrated() {
        let pcm = pcm_scaled(100);
        let first_run = AudioFingerprint::new();
        let id = first_run.register(&pcm, 16000, Some(Path::new(r"C:\audio\original.wav")));

        // What the next launch gets: a fresh map (lib.rs builds one) PLUS what the library stored.
        let after_restart = AudioFingerprint::new();
        assert_eq!(after_restart.count(), 0, "a fresh map still starts empty");
        let loaded = after_restart.rehydrate([stored(id.spectral, Some(&id.content), r"C:\audio\original.wav")]);
        assert_eq!(loaded, 1, "the stored recording is loaded");

        assert!(
            after_restart.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\copy.wav"))),
            "the same content under a NEW path must still be caught across sessions"
        );
        assert!(
            after_restart.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\original.wav"))),
            "re-importing the same file must enter durable idempotency adoption, not mint new rows"
        );
    }

    #[test]
    fn rehydrate_is_additive_and_refuses_the_degenerate_zero_key() {
        let map = AudioFingerprint::new();
        let pcm = pcm_scaled(77);
        let live = map.register(&pcm, 16000, Some(Path::new(r"C:\audio\this_run.wav")));

        // A 0 spectral value is the silent/degenerate bucket register REFUSES to store; loading one from
        // an older row must not turn it into a bucket every silent window then lands in.
        let loaded = map.rehydrate([
            stored(0, Some("deadbeef"), r"C:\audio\silence.wav"),
            stored(12345, Some("cafe"), r"C:\audio\old.wav"),
        ]);
        assert_eq!(loaded, 1, "only the real entry is taken; the 0 is dropped");
        assert_eq!(map.count(), 2, "rehydrate must ADD to what this run registered, never replace it");

        // And what this run registered is still there and still blocks a second publication. The
        // pipeline may adopt its durable rows, but the cache itself must never authorize another set.
        assert!(map.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\this_run.wav"))));
        assert_ne!(live.spectral, 0);
    }

    /// A live registration is ground truth; a stored row must not overwrite it with a stale hash.
    #[test]
    fn rehydrate_never_downgrades_a_live_registration() {
        let map = AudioFingerprint::new();
        let pcm = pcm_scaled(100);
        let live = map.register(&pcm, 16000, Some(Path::new(r"C:\audio\rec.wav")));

        map.rehydrate([stored(live.spectral, Some("0000stale0000"), r"C:\audio\rec.wav")]);
        assert_eq!(map.count(), 1, "same path, same bucket — one recording, not two");
        let source = AudioFingerprint::source_key(Some(Path::new(r"C:\audio\rec.wav")));
        assert_eq!(
            map.lock_state().recording(&source).and_then(|recording| recording.content.as_deref()),
            Some(live.content.as_str()),
            "the freshly computed hash wins over the stored one"
        );
    }

    #[test]
    fn exact_same_source_reimport_is_rejected_without_growing_the_bucket() {
        let map = AudioFingerprint::new();
        let pcm = pcm_scaled(100);
        let source = Path::new(r"C:\audio\audiobook.mp3");
        map.check_and_register(&pcm, 16000, Some(source)).expect("first import must be admitted");
        for _ in 0..50 {
            assert!(map.check_and_register(&pcm, 16000, Some(source)).is_err());
        }
        assert_eq!(map.count(), 1, "retries must never mint another in-memory recording");
    }

    /// A windowed import must produce the SAME content hash as a whole-buffer one.
    ///
    /// If these ever diverge, a recording's identity would depend on which import path handled it, and
    /// the same audio would be two different recordings.
    #[test]
    fn a_windowed_identity_equals_the_whole_buffer_identity() {
        let whole: Vec<i16> = (0..48_000).map(|i| ((i * 13) % 9000) as i16).collect();
        let mut streaming = StreamingIdentity::new();
        for window in whole.chunks(7_000) {
            streaming.push(window, 16000);
        }
        assert_eq!(
            streaming.finish().content,
            AudioFingerprint::content_hash(&whole, 16000),
            "the same canonical PCM must hash the same whether it arrived in one piece or many"
        );
    }

    /// A streamed recording re-imported in a LATER session must still be caught.
    ///
    /// The bug this pins: the streaming path's per-window checks compare WINDOW hashes, which can never
    /// equal the whole-file hash that is persisted. Persisting the whole-file identity without also
    /// CHECKING it left cross-session dedup looking implemented while doing nothing at all for long
    /// files — the only kind that reach that path.
    #[test]
    fn a_streamed_recording_is_caught_on_reimport_after_a_restart() {
        let whole: Vec<i16> = (0..48_000).map(|i| ((i * 13) % 9000) as i16).collect();
        let mut first_run = StreamingIdentity::new();
        for window in whole.chunks(7_000) {
            first_run.push(window, 16000);
        }
        let id = first_run.finish();

        // Next launch: a fresh map plus what the library stored for that recording.
        let after_restart = AudioFingerprint::new();
        after_restart.rehydrate([stored(id.spectral, Some(&id.content), r"C:\audio\long.wav")]);

        // The same audio arrives again under a different path, windowed exactly as before.
        let mut second_run = StreamingIdentity::new();
        for window in whole.chunks(7_000) {
            second_run.push(window, 16000);
        }
        assert!(
            after_restart
                .check_and_register_identity(&second_run.finish(), Some(Path::new(r"C:\audio\copy.wav")))
                .is_err(),
            "a streamed duplicate must be rejected across sessions, not merely re-registered"
        );
    }

    #[test]
    fn definitive_content_duplicate_is_rejected_across_different_spectral_buckets() {
        let fp = AudioFingerprint::new();
        let content = AudioFingerprint::content_hash(&pcm_scaled(100), 16_000);
        let streamed = AudioIdentity { spectral: 0x1111, content: content.clone() };
        let whole_buffer = AudioIdentity { spectral: 0x2222, content };

        fp.reserve_import_identity(&streamed, Some(Path::new(r"C:\audio\streamed.wav")))
            .expect("first recording is admitted")
            .commit();

        assert!(
            fp.reserve_import_identity(&whole_buffer, Some(Path::new(r"C:\audio\whole-buffer-copy.wav"))).is_err(),
            "tier-2 identity is definitive even when a route change produces another tier-1 bucket"
        );
        assert_eq!(fp.count(), 1, "the rejected route-change duplicate must not grow the index");
    }

    #[test]
    fn persisted_identity_writers_use_the_fixed_window_canonical_decoder() {
        // Architecture regression: both standalone writers once decoded the whole file and then
        // persisted that hash. For >1 decode window at 44.1/48 kHz it disagrees with playback/export
        // verification and with streaming import. Keep the writer sources on the one public protocol.
        for (name, source) in [
            ("backfill_fingerprints", include_str!("bin/backfill_fingerprints.rs")),
            ("batch_importer", include_str!("bin/batch_importer.rs")),
        ] {
            let production = source.split("\n#[cfg(test)]\nmod tests").next().unwrap_or(source);
            assert!(
                production.contains("AudioFingerprint::identify_canonical_file"),
                "{name} must call the fixed-window canonical identity protocol"
            );
            assert!(
                !production.contains("decode_to_pcm("),
                "{name} must not derive a persisted identity from whole-buffer resampling"
            );
            assert!(
                !production.contains("AudioFingerprint::identify("),
                "{name} must not bypass canonical file decoding at a persisted writer"
            );
        }

        let evaluation_source = include_str!("eval.rs");
        let evaluation_production =
            evaluation_source.split("\n#[cfg(test)]\nmod tests").next().unwrap_or(evaluation_source);
        let identity_decodes = evaluation_production.matches("decode_pcm_windows(").count();
        let fixed_windows = evaluation_production.matches("crate::audio::DECODE_WINDOW_MS").count();
        assert_eq!(identity_decodes, 3, "evaluation has three source-identity/materialization decoders");
        assert_eq!(
            fixed_windows, identity_decodes,
            "every evaluation identity/materialization decoder must use the same fixed 90-second protocol"
        );
    }

    #[test]
    fn reimport_same_source_is_an_idempotency_conflict() {
        let fp = AudioFingerprint::new();
        let pcm = pcm_scaled(100);
        let source = Path::new(r"C:\audio\audiobook.mp3");

        assert!(!fp.check_duplicate(&pcm, 16000, Some(source)));
        fp.register(&pcm, 16000, Some(source));
        assert!(fp.check_duplicate(&pcm, 16000, Some(source)));
        assert!(fp.check_and_register(&pcm, 16000, Some(source)).is_err());
    }

    #[test]
    fn failed_import_reservation_rolls_back_and_allows_exact_retry() {
        let fp = AudioFingerprint::new();
        let pcm = pcm_scaled(100);
        let source = Path::new(r"C:\audio\retry.wav");

        let (_, reservation) = fp.reserve_import(&pcm, 16000, Some(source)).expect("admit first attempt");
        assert_eq!(fp.count(), 1, "the live reservation blocks concurrent duplicate admission");
        assert!(fp.reserve_import(&pcm, 16000, Some(source)).is_err());
        drop(reservation);

        assert_eq!(fp.count(), 0, "a failed publication must leave no phantom fingerprint");
        let (_, retry) = fp.reserve_import(&pcm, 16000, Some(source)).expect("retry after rollback");
        retry.commit();
        assert_eq!(fp.count(), 1);
        assert!(fp.reserve_import(&pcm, 16000, Some(source)).is_err());
    }

    #[test]
    fn dropping_one_reservation_cannot_remove_an_unrelated_recording() {
        let fp = AudioFingerprint::new();
        let first = pcm_scaled(100);
        let second = pcm_scaled(77);
        let (_, pending) =
            fp.reserve_import(&first, 16000, Some(Path::new(r"C:\audio\pending.wav"))).expect("reserve pending source");
        let (_, committed) = fp
            .reserve_import(&second, 16000, Some(Path::new(r"C:\audio\committed.wav")))
            .expect("reserve unrelated source");
        committed.commit();

        drop(pending);
        assert_eq!(fp.count(), 1);
        assert!(fp.check_duplicate(&second, 16000, Some(Path::new(r"C:\audio\copy.wav"))));
        assert!(!fp.check_duplicate(&first, 16000, Some(Path::new(r"C:\audio\retry.wav"))));
    }

    #[test]
    fn forget_source_removes_only_the_durably_rolled_back_source() {
        let fp = AudioFingerprint::new();
        let first = pcm_scaled(100);
        let second = pcm_scaled(77);
        let first_path = Path::new(r"C:\audio\rollback.wav");
        fp.register(&first, 16000, Some(first_path));
        fp.register(&second, 16000, Some(Path::new(r"C:\audio\keep.wav")));

        assert_eq!(fp.forget_source(first_path), 1);
        assert_eq!(fp.count(), 1);
        assert!(!fp.check_duplicate(&first, 16000, Some(Path::new(r"C:\audio\retry.wav"))));
        assert!(fp.check_duplicate(&second, 16000, Some(Path::new(r"C:\audio\copy.wav"))));
    }

    #[test]
    fn duplicate_from_different_source_is_rejected() {
        let fp = AudioFingerprint::new();
        let pcm = pcm_scaled(100);
        let a = Path::new(r"C:\audio\file_a.mp3");
        let b = Path::new(r"C:\audio\file_b.mp3");

        fp.register(&pcm, 16000, Some(a));
        assert!(fp.check_duplicate(&pcm, 16000, Some(b)));
        assert!(fp.check_and_register(&pcm, 16000, Some(b)).is_err());
    }

    #[test]
    fn silent_windows_do_not_collide_across_distinct_files() {
        // A fully-silent (all-zero) window hashes to spectral 0 — the same as empty input. Two DISTINCT
        // files that each contain a silent decode window must NOT be rejected as duplicates just because
        // they share silence.
        let fp = AudioFingerprint::new();
        let silent = vec![0i16; 16_000];
        let a = Path::new(r"C:\audio\a.wav");
        let b = Path::new(r"C:\audio\b.wav");

        assert_eq!(fp.check_and_register(&silent, 16000, Some(a)).unwrap().spectral, 0);
        assert_eq!(
            fp.check_and_register(&silent, 16000, Some(b)).unwrap().spectral,
            0,
            "a distinct file's silent window must not be a 'Duplicate'"
        );
        assert!(!fp.check_duplicate(&silent, 16000, Some(b)), "silence is not a content duplicate");
        assert_eq!(fp.count(), 0, "the degenerate bucket 0 is never stored");

        // Sanity: a REAL non-silent duplicate across distinct files is STILL rejected.
        let real: Vec<i16> = (0..16_000).map(|i| ((i * 7) % 5000) as i16).collect();
        assert!(fp.check_and_register(&real, 16000, Some(a)).is_ok());
        assert!(
            fp.check_and_register(&real, 16000, Some(b)).is_err(),
            "real shared content across distinct files is still rejected"
        );
    }

    #[test]
    fn duplicate_detection_recovers_poisoned_cache() {
        let fp = AudioFingerprint::new();
        let _ = std::panic::catch_unwind(|| {
            let _guard = fp.state.lock().expect("lock fingerprint cache");
            panic!("poison fingerprint cache");
        });

        let pcm = pcm_scaled(100);
        let a = Path::new(r"C:\audio\file_a.mp3");
        let b = Path::new(r"C:\audio\file_b.mp3");

        fp.register(&pcm, 16000, Some(a));
        assert!(fp.check_duplicate(&pcm, 16000, Some(b)));
        assert_eq!(fp.count(), 1);
    }

    #[test]
    fn hundred_thousand_recording_indexes_remain_exact_across_admission_and_rollback() {
        let fp = AudioFingerprint::new();
        {
            let mut state = fp.lock_state();
            for index in 0..100_000u64 {
                let source = format!(r"C:\library\clip-{index:06}.wav");
                let inserted = state.insert(
                    (index % 4096) + 1,
                    KnownRecording { content: Some(format!("{index:064x}")), source, reservation_token: None },
                );
                assert!(inserted, "fixture source {index} must be unique");
            }
            state.assert_consistent();
        }
        assert_eq!(fp.count(), 100_000);

        let duplicate = AudioIdentity { spectral: 9_999, content: format!("{:064x}", 54_321u64) };
        assert!(
            fp.reserve_import_identity(&duplicate, Some(Path::new(r"C:\library\copy.wav"))).is_err(),
            "definitive content lookup must still span every spectral bucket"
        );

        let fresh = AudioIdentity { spectral: 10_000, content: "f".repeat(64) };
        let pending = fp
            .reserve_import_identity(&fresh, Some(Path::new(r"C:\library\fresh.wav")))
            .expect("new source and content must reserve");
        assert_eq!(fp.count(), 100_001);
        fp.lock_state().assert_consistent();
        drop(pending);
        assert_eq!(fp.count(), 100_000, "rollback must remove every secondary-index reference");
        fp.lock_state().assert_consistent();
    }

    #[test]
    fn concurrent_content_reservations_admit_exactly_one_publication() {
        use std::sync::{Arc, Barrier};

        let fp = Arc::new(AudioFingerprint::new());
        let start = Arc::new(Barrier::new(16));
        let content = "a".repeat(64);
        let workers: Vec<_> = (0..16u64)
            .map(|index| {
                let fp = Arc::clone(&fp);
                let start = Arc::clone(&start);
                let content = content.clone();
                std::thread::spawn(move || {
                    let source = format!(r"C:\concurrent\copy-{index}.wav");
                    let identity = AudioIdentity { spectral: index + 1, content };
                    start.wait();
                    match fp.reserve_import_identity(&identity, Some(Path::new(&source))) {
                        Ok(reservation) => {
                            reservation.commit();
                            true
                        }
                        Err(_) => false,
                    }
                })
            })
            .collect();

        let admitted = workers
            .into_iter()
            .map(|worker| worker.join().expect("reservation worker must not panic"))
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(admitted, 1, "one content identity may own exactly one durable publication");
        assert_eq!(fp.count(), 1);
        fp.lock_state().assert_consistent();
    }

    #[test]
    fn forgetting_one_historical_duplicate_keeps_the_other_content_protection() {
        let fp = AudioFingerprint::new();
        let hash = "b".repeat(64);
        fp.rehydrate([stored(1, Some(&hash), r"C:\legacy\first.wav"), stored(2, Some(&hash), r"C:\legacy\second.wav")]);
        assert_eq!(fp.count(), 2);

        assert_eq!(fp.forget_source(Path::new(r"C:\legacy\first.wav")), 1);
        let candidate = AudioIdentity { spectral: 3, content: hash.clone() };
        assert!(
            fp.reserve_import_identity(&candidate, Some(Path::new(r"C:\legacy\third.wav"))).is_err(),
            "the remaining historical row must retain definitive-content protection"
        );

        assert_eq!(fp.forget_source(Path::new(r"C:\legacy\second.wav")), 1);
        let admitted = fp
            .reserve_import_identity(&candidate, Some(Path::new(r"C:\legacy\third.wav")))
            .expect("content becomes admissible only after its final durable source is gone");
        drop(admitted);
        fp.lock_state().assert_consistent();
    }
}
