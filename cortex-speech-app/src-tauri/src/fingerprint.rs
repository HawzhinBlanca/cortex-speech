use std::collections::HashMap;
use std::path::Path;
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
/// its sample rate is the DEFINITIVE key. A rejection requires a tier-2 match; tier 1 only decides
/// which entries are worth comparing.
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
    /// Bucketed by the tier-1 spectral value; a bucket may legitimately hold several DISTINCT
    /// recordings, which is the whole point of demoting it from an identity to a candidate index.
    known: Mutex<HashMap<u64, Vec<KnownRecording>>>,
}

/// One recording as the dedup map knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRecording {
    /// Hex blake3 of canonical PCM. `None` for rows written before v51 — unknown content, and unknown
    /// content can never prove a duplicate.
    pub content: Option<String>,
    /// Canonicalised source path, or empty for in-memory/streaming imports with no file behind them.
    pub source: String,
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
/// therefore land in different buckets and miss each other — that direction keeps a duplicate rather
/// than discarding audio, which is the trade this whole module now makes deliberately. In practice the
/// path is chosen by duration, so the same recording takes the same one every time.
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
        for sample in pcm {
            self.hasher.update(&sample.to_le_bytes());
        }
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
        Self { known: Mutex::new(HashMap::new()) }
    }

    fn lock_known(&self) -> MutexGuard<'_, HashMap<u64, Vec<KnownRecording>>> {
        self.known.lock().unwrap_or_else(|poisoned| {
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
        // Chunked rather than one big allocated Vec: a 90-second 16 kHz window is 2.9 MB, and a
        // whole-file non-streaming import can be far larger. Hashing in place costs no copy of the PCM.
        for sample in pcm {
            hasher.update(&sample.to_le_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    /// Both tiers for one decoded buffer.
    pub fn identify(pcm: &[i16], sample_rate: u32) -> AudioIdentity {
        AudioIdentity { spectral: Self::fingerprint(pcm, sample_rate), content: Self::content_hash(pcm, sample_rate) }
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
    /// Reject only when a KNOWN recording, from a DIFFERENT source file, has a content hash that is
    /// present and equal. Every other case keeps the audio:
    ///   - same source path: a re-import of the same file, never a duplicate (unchanged);
    ///   - empty source key: an in-memory/streaming import with nothing to compare paths on;
    ///   - `content: None`: a pre-v51 row whose audio was never hashed. It CANNOT prove identity, so it
    ///     does not get to reject one.
    fn is_proven_duplicate(bucket: &[KnownRecording], content: &str, source_key: &str) -> bool {
        if source_key.is_empty() {
            return false;
        }
        bucket.iter().any(|known| known.source != source_key && known.content.as_deref() == Some(content))
    }

    /// Returns true if this audio is byte-identical to audio already imported from a different file.
    pub fn check_duplicate(&self, pcm: &[i16], sample_rate: u32, source: Option<&Path>) -> bool {
        let id = Self::identify(pcm, sample_rate);
        if id.spectral == 0 {
            return false; // degenerate bucket (empty/silent) — never a content key, never a dup
        }
        let source_key = Self::source_key(source);
        let map = self.lock_known();
        map.get(&id.spectral).is_some_and(|bucket| Self::is_proven_duplicate(bucket, &id.content, &source_key))
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
        // A spectral value of 0 is the degenerate "no usable energy bands" case — digital silence, a
        // fully-silent decode window, or a clip shorter than 8 frames (<16 ms). Such audio yields no
        // speech segments anyway, so it is neither compared nor stored; the bucket would otherwise
        // collect every silent window in the library. Tier 2 alone would in fact be safe here (all-zero
        // PCM really IS identical content), but refusing to reject on silence is the kinder default and
        // matches what v50 shipped.
        if id.spectral == 0 {
            return Ok(id);
        }
        let source_key = Self::source_key(source);

        let mut map = self.lock_known();
        let bucket = map.entry(id.spectral).or_default();
        if Self::is_proven_duplicate(bucket, &id.content, &source_key) {
            return Err("Duplicate audio content");
        }
        // Same path re-imported, or a pre-v51 entry we can now upgrade: replace it rather than growing
        // the bucket, so re-importing one file a hundred times does not grow the map a hundred entries.
        if let Some(existing) = bucket.iter_mut().find(|k| k.source == source_key) {
            existing.content = Some(id.content.clone());
        } else {
            bucket.push(KnownRecording { content: Some(id.content.clone()), source: source_key });
        }
        Ok(id)
    }

    /// Register a recording (re-registering the same path is allowed).
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
        // Never store a degenerate bucket key — see check_and_register.
        if id.spectral == 0 {
            return Ok(());
        }
        let source_key = Self::source_key(source);
        let mut map = self.lock_known();
        let bucket = map.entry(id.spectral).or_default();
        if Self::is_proven_duplicate(bucket, &id.content, &source_key) {
            return Err("Duplicate audio content");
        }
        match bucket.iter_mut().find(|k| k.source == source_key) {
            Some(existing) => existing.content = Some(id.content.clone()),
            None => bucket.push(KnownRecording { content: Some(id.content.clone()), source: source_key }),
        }
        Ok(())
    }

    /// PRIVATE on purpose. There is deliberately NO public "register without checking" entry point for a
    /// precomputed identity: the streaming path briefly had one, and using it instead of
    /// [`check_and_register_identity`](Self::check_and_register_identity) made cross-session dedup for
    /// long files look implemented while doing nothing. An API that cannot be called wrongly beats a
    /// comment asking callers not to.
    fn register_identity(&self, id: &AudioIdentity, source: Option<&Path>) {
        let _ = self.check_and_register_identity(id, source);
    }

    /// Load stored recording identities into the map at startup (v50 spectral + v51 content hash).
    ///
    /// This is what makes duplicate detection survive a restart. Returns the number of entries ADDED.
    ///
    /// Additive, never clearing: a caller that rehydrates mid-session must not drop what this run has
    /// already registered. A stored row that upgrades a known path from `None` to a real hash is
    /// applied in place and does not count as an addition.
    pub fn rehydrate(&self, known: impl IntoIterator<Item = StoredAudioIdentity>) -> usize {
        let mut map = self.lock_known();
        let mut added = 0usize;
        for row in known {
            // Skip the degenerate value `register` refuses to store, so a legacy 0 cannot become a
            // bucket that every silent window then lands in.
            if row.spectral == 0 {
                continue;
            }
            let bucket = map.entry(row.spectral).or_default();
            match bucket.iter_mut().find(|k| k.source == row.audio_path) {
                // A live registration always wins over a stored row; only fill a genuine gap.
                Some(existing) => {
                    if existing.content.is_none() {
                        existing.content = row.content;
                    }
                }
                None => {
                    bucket.push(KnownRecording { content: row.content, source: row.audio_path });
                    added += 1;
                }
            }
        }
        added
    }

    pub fn clear(&self) {
        self.lock_known().clear();
    }

    /// Number of RECORDINGS known, not buckets — a bucket may hold several distinct recordings.
    pub fn count(&self) -> usize {
        self.lock_known().values().map(Vec::len).sum()
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
        assert!(
            !AudioFingerprint::is_proven_duplicate(&map.lock_known()[&bucket], &b, r"C:\audio\b.wav"),
            "distinct content sharing a spectral bucket must NOT be rejected"
        );
        // ...and the real duplicate in the same bucket still is.
        assert!(AudioFingerprint::is_proven_duplicate(&map.lock_known()[&bucket], &a, r"C:\audio\copy_of_a.wav"));
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
            !after_restart.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\original.wav"))),
            "re-importing the SAME file is still allowed — that is not a duplicate"
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

        // And what this run registered is still there and still its own path.
        assert!(!map.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\this_run.wav"))));
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
        assert_eq!(
            map.lock_known()[&live.spectral][0].content.as_deref(),
            Some(live.content.as_str()),
            "the freshly computed hash wins over the stored one"
        );
    }

    #[test]
    fn reimporting_one_file_repeatedly_does_not_grow_the_bucket() {
        let map = AudioFingerprint::new();
        let pcm = pcm_scaled(100);
        let source = Path::new(r"C:\audio\audiobook.mp3");
        for _ in 0..50 {
            map.check_and_register(&pcm, 16000, Some(source)).expect("re-import is never a duplicate");
        }
        assert_eq!(map.count(), 1, "one file is one recording however many times it is imported");
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
    fn reimport_same_source_is_not_duplicate() {
        let fp = AudioFingerprint::new();
        let pcm = pcm_scaled(100);
        let source = Path::new(r"C:\audio\audiobook.mp3");

        assert!(!fp.check_duplicate(&pcm, 16000, Some(source)));
        fp.register(&pcm, 16000, Some(source));
        assert!(!fp.check_duplicate(&pcm, 16000, Some(source)));
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
            let _guard = fp.known.lock().expect("lock fingerprint cache");
            panic!("poison fingerprint cache");
        });

        let pcm = pcm_scaled(100);
        let a = Path::new(r"C:\audio\file_a.mp3");
        let b = Path::new(r"C:\audio\file_b.mp3");

        fp.register(&pcm, 16000, Some(a));
        assert!(fp.check_duplicate(&pcm, 16000, Some(b)));
        assert_eq!(fp.count(), 1);
    }
}
