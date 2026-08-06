use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// Audio fingerprinting using spectral energy peaks.
/// Tracks which source file each fingerprint came from so re-importing the same
/// file is allowed while duplicate content from different paths is rejected.
///
/// SCOPE — this map is IN-MEMORY and lives exactly as long as the process. It is constructed empty
/// in `lib.rs` at startup and nothing rehydrates it; no migration has ever created a column to
/// rehydrate it FROM. So the rejection above holds WITHIN a single run only: restart the app,
/// re-import the same audio under a different path, and it is accepted silently.
///
/// `get_fingerprint_count` therefore reports what this session has imported, NOT corpus coverage —
/// the 2026-08-05 audit read its 0 next to a 144-clip corpus as evidence that legacy rows were never
/// backfilled, which is why the UI label now says "(this session)".
///
/// Making duplicate detection durable needs a persisted fingerprint per segment plus a backfill over
/// existing rows. Deliberately not done as a side effect of a labelling fix.
pub struct AudioFingerprint {
    known: Mutex<HashMap<u64, String>>,
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

    fn lock_known(&self) -> MutexGuard<'_, HashMap<u64, String>> {
        self.known.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned audio fingerprint cache");
            poisoned.into_inner()
        })
    }

    /// Generate a 64-bit fingerprint from PCM samples using spectral energy bands.
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

    /// Returns true if this fingerprint matches audio already imported from a different file.
    pub fn check_duplicate(&self, pcm: &[i16], sample_rate: u32, source: Option<&Path>) -> bool {
        let fp = Self::fingerprint(pcm, sample_rate);
        if fp == 0 {
            return false; // degenerate fingerprint (empty/silent) — not a real content key, never a dup
        }
        let source_key = Self::source_key(source);
        let map = self.lock_known();
        match map.get(&fp) {
            Some(existing) if !source_key.is_empty() && existing == &source_key => false,
            Some(_) => true,
            None => false,
        }
    }

    /// Check and register a fingerprint in a single lock acquire to avoid the
    /// check-then-act race where register() overwrites the entry check_duplicate() just tested.
    /// When source_key is empty (e.g., streaming/in-memory), it only checks for exact matches
    /// with other empty entries; otherwise it detects duplicates across different source files.
    pub fn check_and_register(
        &self,
        pcm: &[i16],
        sample_rate: u32,
        source: Option<&Path>,
    ) -> Result<u64, &'static str> {
        let fp = Self::fingerprint(pcm, sample_rate);
        // A fingerprint of 0 is the degenerate "no usable energy bands" case — digital silence, a fully
        // -silent decode window (every all-zero window hashes to 0), or a clip shorter than 8 frames
        // (<16 ms) — NOT a real content signature. Without this guard, the first such file registers 0 and
        // the next DISTINCT file that also hashes to 0 collides on it and is wrongly rejected as
        // "Duplicate audio content", silently dropping a legitimate clip. Such audio yields no speech
        // segments anyway, so skip both the conflict check and registration and never store it.
        if fp == 0 {
            return Ok(fp);
        }
        let source_key = Self::source_key(source);

        let mut map = self.lock_known();
        if let Some(existing) = map.get(&fp) {
            if source_key.is_empty() && existing.is_empty() {
                // Both are unknown source — treat as same
                return Ok(fp);
            }
            if source_key.is_empty() {
                // Empty source_key with existing non-empty → allow (different import context)
                return Ok(fp);
            }
            if !source_key.is_empty() {
                if existing == &source_key {
                    return Ok(fp);
                }
                // Non-empty source_key conflicts with existing different key
                return Err("Duplicate audio content");
            }
        }
        map.insert(fp, source_key);
        Ok(fp)
    }

    /// Register a fingerprint for a source file (re-registering the same path is allowed).
    pub fn register(&self, pcm: &[i16], sample_rate: u32, source: Option<&Path>) -> u64 {
        let fp = Self::fingerprint(pcm, sample_rate);
        // Never store a degenerate fingerprint (empty/silent window) as a content key — it would make
        // every later silent window collide with it. See check_and_register.
        if fp != 0 {
            self.lock_known().insert(fp, Self::source_key(source));
        }
        fp
    }

    pub fn register_hash(&self, hash: u64, source: Option<&Path>) {
        self.lock_known().insert(hash, Self::source_key(source));
    }

    pub fn clear(&self) {
        self.lock_known().clear();
    }

    pub fn count(&self) -> usize {
        self.lock_known().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Pins the SCOPE limit documented on the struct, so nobody reads the duplicate-rejection tests
    /// below as a durable guarantee. A fresh `AudioFingerprint` is what `lib.rs` builds at every app
    /// start, and it knows nothing — so the same content under a new path is accepted after a
    /// restart. This test exists to make that visible, not to bless it: when fingerprints are
    /// persisted it should FAIL and be replaced by its opposite.
    #[test]
    fn duplicate_detection_does_not_survive_a_restart() {
        let pcm: Vec<i16> = (0..16_000).map(|i| (i as i16).wrapping_mul(100)).collect();
        let first_run = AudioFingerprint::new();
        first_run.register(&pcm, 16000, Some(Path::new(r"C:\audio\original.wav")));
        assert!(
            first_run.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\copy.wav"))),
            "within one run, the same content under a different path IS a duplicate"
        );

        // What the next launch actually gets: lib.rs calls AudioFingerprint::new() and nothing
        // rehydrates it, because no migration ever created a column to rehydrate from.
        let after_restart = AudioFingerprint::new();
        assert_eq!(after_restart.count(), 0, "the map is in-memory only");
        assert!(
            !after_restart.check_duplicate(&pcm, 16000, Some(Path::new(r"C:\audio\copy.wav"))),
            "KNOWN GAP: cross-session duplicate detection does not exist — see the struct docs"
        );
    }

    #[test]
    fn reimport_same_source_is_not_duplicate() {
        let fp = AudioFingerprint::new();
        let pcm: Vec<i16> = (0..16_000).map(|i| (i as i16).wrapping_mul(100)).collect();
        let source = Path::new(r"C:\audio\audiobook.mp3");

        assert!(!fp.check_duplicate(&pcm, 16000, Some(source)));
        fp.register(&pcm, 16000, Some(source));
        assert!(!fp.check_duplicate(&pcm, 16000, Some(source)));
    }

    #[test]
    fn duplicate_from_different_source_is_rejected() {
        let fp = AudioFingerprint::new();
        let pcm: Vec<i16> = (0..16_000).map(|i| (i as i16).wrapping_mul(100)).collect();
        let a = Path::new(r"C:\audio\file_a.mp3");
        let b = Path::new(r"C:\audio\file_b.mp3");

        fp.register(&pcm, 16000, Some(a));
        assert!(fp.check_duplicate(&pcm, 16000, Some(b)));
    }

    #[test]
    fn silent_windows_do_not_collide_across_distinct_files() {
        // Round-20: a fully-silent (all-zero) window hashes to fingerprint 0 — the same as empty input.
        // Two DISTINCT files that each contain a silent decode window must NOT be rejected as duplicates
        // just because they share silence.
        let fp = AudioFingerprint::new();
        let silent = vec![0i16; 16_000];
        let a = Path::new(r"C:\audio\a.wav");
        let b = Path::new(r"C:\audio\b.wav");

        assert_eq!(fp.check_and_register(&silent, 16000, Some(a)).unwrap(), 0);
        assert_eq!(
            fp.check_and_register(&silent, 16000, Some(b)).unwrap(),
            0,
            "a distinct file's silent window must not be a 'Duplicate'"
        );
        assert!(!fp.check_duplicate(&silent, 16000, Some(b)), "silence is not a content duplicate");
        assert_eq!(fp.count(), 0, "degenerate fingerprint 0 is never stored as a content key");

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

        let pcm: Vec<i16> = (0..16_000).map(|i| (i as i16).wrapping_mul(100)).collect();
        let a = Path::new(r"C:\audio\file_a.mp3");
        let b = Path::new(r"C:\audio\file_b.mp3");

        fp.register(&pcm, 16000, Some(a));
        assert!(fp.check_duplicate(&pcm, 16000, Some(b)));
        assert_eq!(fp.count(), 1);
    }
}
