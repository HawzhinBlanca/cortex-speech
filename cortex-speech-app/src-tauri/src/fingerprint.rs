use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// Audio fingerprinting using spectral energy peaks.
/// Tracks which source file each fingerprint came from so re-importing the same
/// file is allowed while duplicate content from different paths is still rejected.
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
        self.lock_known().insert(fp, Self::source_key(source));
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
