use blake3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub audio_hash: String,
    pub raw_transcript: String,
    pub normalized_transcript: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub model_id: String,
}

pub struct TranscriptCache {
    store: Mutex<HashMap<String, CacheEntry>>,
    max_entries: usize,
}

impl TranscriptCache {
    pub fn new(max_entries: usize) -> Self {
        let max_entries = max_entries.max(1);
        Self { store: Mutex::new(HashMap::with_capacity(max_entries)), max_entries }
    }

    fn lock_store(&self) -> MutexGuard<'_, HashMap<String, CacheEntry>> {
        self.store.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned transcript cache");
            poisoned.into_inner()
        })
    }

    pub fn compute_hash(path: &Path) -> Result<String, crate::error::AppError> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher.finalize().to_hex().to_string())
    }

    fn cache_key(hash: &str, model_id: &str, chunk_suffix: Option<&str>) -> String {
        match chunk_suffix {
            Some(suffix) => format!("{}:{}:{}", hash, model_id, suffix),
            None => format!("{}:{}", hash, model_id),
        }
    }

    pub fn get(&self, audio_path: &Path, model_id: &str) -> Option<CacheEntry> {
        self.get_chunk(audio_path, model_id, None)
    }

    pub fn get_chunk(&self, audio_path: &Path, model_id: &str, chunk_suffix: Option<&str>) -> Option<CacheEntry> {
        let hash = Self::compute_hash(audio_path).ok()?;
        self.get_chunk_by_hash(&hash, model_id, chunk_suffix)
    }

    /// Lookup using a PRECOMPUTED whole-file content hash. Round-23 #5: the per-chunk transcription
    /// loop must hash the audio file ONCE per run (the content is invariant for the run) and reuse the
    /// hash here, instead of re-reading + re-hashing the entire file on every chunk get/set.
    pub fn get_chunk_by_hash(&self, hash: &str, model_id: &str, chunk_suffix: Option<&str>) -> Option<CacheEntry> {
        let store = self.lock_store();
        store.get(&Self::cache_key(hash, model_id, chunk_suffix)).cloned()
    }

    pub fn set(&self, audio_path: &Path, entry: CacheEntry) {
        self.set_chunk(audio_path, None, entry);
    }

    pub fn set_chunk(&self, audio_path: &Path, chunk_suffix: Option<&str>, entry: CacheEntry) {
        if let Ok(hash) = Self::compute_hash(audio_path) {
            self.set_chunk_by_hash(&hash, chunk_suffix, entry);
        }
    }

    /// Insert using a PRECOMPUTED whole-file content hash (see [`get_chunk_by_hash`]).
    pub fn set_chunk_by_hash(&self, hash: &str, chunk_suffix: Option<&str>, entry: CacheEntry) {
        let mut store = self.lock_store();
        let key = Self::cache_key(hash, &entry.model_id, chunk_suffix);
        // Round-23 #6: only evict when inserting a genuinely NEW key — an overwrite of an existing key
        // is net-zero in size, so evicting an unrelated entry for it needlessly drops a good transcript
        // and shrinks the cache below max_entries. And when we DO evict, drop the OLDEST entry (by
        // created_at) deterministically, not `keys().next()` (random HashMap order, which can evict a
        // hot entry while keeping cold ones).
        if !store.contains_key(&key) {
            while store.len() >= self.max_entries {
                let oldest = store.iter().min_by_key(|(_, e)| e.created_at).map(|(k, _)| k.clone());
                match oldest {
                    Some(k) => {
                        store.remove(&k);
                    }
                    None => break,
                }
            }
        }
        store.insert(key, entry);
    }

    pub fn invalidate(&self, audio_path: &Path) {
        if let Ok(hash) = Self::compute_hash(audio_path) {
            self.lock_store().retain(|k, _| !k.starts_with(&hash));
        }
    }

    pub fn clear(&self) {
        self.lock_store().clear();
    }

    pub fn size(&self) -> usize {
        self.lock_store().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn cache_entry(model_id: &str, text: &str) -> CacheEntry {
        CacheEntry {
            audio_hash: "unused".into(),
            raw_transcript: text.into(),
            normalized_transcript: None,
            created_at: chrono::Utc::now(),
            model_id: model_id.into(),
        }
    }

    fn audio_file(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("temp audio file");
        file.write_all(bytes).expect("write temp audio");
        file
    }

    #[test]
    fn cache_recovers_poisoned_store() {
        let cache = TranscriptCache::new(2);
        let audio = audio_file(b"audio");

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cache.store.lock().expect("lock transcript cache");
            panic!("poison transcript cache");
        }));

        cache.set(audio.path(), cache_entry("model-a", "text"));
        let cached = cache.get(audio.path(), "model-a").expect("cache hit after poison recovery");

        assert_eq!(cached.raw_transcript, "text");
        assert_eq!(cache.size(), 1);
    }

    #[test]
    fn overwrite_does_not_evict_an_unrelated_entry() {
        // Round-23 #6: re-caching an EXISTING key (an overwrite) must not evict a different valid entry
        // or shrink the cache below capacity.
        let cache = TranscriptCache::new(2);
        let a = audio_file(b"aaa");
        let b = audio_file(b"bbb");
        cache.set(a.path(), cache_entry("m", "a-text"));
        cache.set(b.path(), cache_entry("m", "b-text")); // cache now full at 2
        cache.set(a.path(), cache_entry("m", "a-text-2")); // overwrite a's key
        assert_eq!(cache.size(), 2, "overwrite must not shrink the cache");
        assert!(cache.get(b.path(), "m").is_some(), "the unrelated entry must survive an overwrite");
        assert_eq!(cache.get(a.path(), "m").expect("a present").raw_transcript, "a-text-2");
    }

    #[test]
    fn eviction_drops_the_oldest_entry_deterministically() {
        // Round-23 #6: when full, eviction drops the OLDEST entry (by created_at), not a random one.
        let cache = TranscriptCache::new(2);
        let mk = |path: &Path, text: &str, secs: i64| {
            cache.set_chunk(
                path,
                None,
                CacheEntry {
                    audio_hash: "x".into(),
                    raw_transcript: text.into(),
                    normalized_transcript: None,
                    created_at: chrono::DateTime::from_timestamp(secs, 0).expect("valid ts"),
                    model_id: "m".into(),
                },
            );
        };
        let a = audio_file(b"a1");
        let b = audio_file(b"b1");
        let c = audio_file(b"c1");
        mk(a.path(), "a", 100); // oldest
        mk(b.path(), "b", 200);
        mk(c.path(), "c", 300); // full -> evict the oldest (a)
        assert_eq!(cache.size(), 2);
        assert!(cache.get(a.path(), "m").is_none(), "the oldest entry (by created_at) is evicted");
        assert!(cache.get(b.path(), "m").is_some());
        assert!(cache.get(c.path(), "m").is_some());
    }

    #[test]
    fn invalidate_removes_every_entry_for_one_audio_and_keeps_others() {
        // Coverage gap (iter-81 hand-audit): invalidate() prefix-matches the audio content hash to
        // drop EVERY cache key for that file — all models AND all chunk suffixes — while leaving
        // unrelated audio untouched. Pin that contract (a stale cache entry surviving a re-import of
        // edited-in-place audio would replay the wrong transcript).
        let cache = TranscriptCache::new(8);
        let a = audio_file(b"aaa");
        let b = audio_file(b"bbb");
        cache.set(a.path(), cache_entry("m1", "a-m1"));
        cache.set(a.path(), cache_entry("m2", "a-m2"));
        cache.set_chunk(a.path(), Some("chunk_0_1000"), cache_entry("m1", "a-chunk"));
        cache.set(b.path(), cache_entry("m1", "b-m1"));
        assert_eq!(cache.size(), 4);

        cache.invalidate(a.path());

        assert!(cache.get(a.path(), "m1").is_none(), "audio A / model m1 must be invalidated");
        assert!(cache.get(a.path(), "m2").is_none(), "audio A / model m2 must be invalidated");
        assert!(cache.get_chunk(a.path(), "m1", Some("chunk_0_1000")).is_none(), "audio A chunk must be invalidated");
        assert!(cache.get(b.path(), "m1").is_some(), "an unrelated audio must survive invalidate");
        assert_eq!(cache.size(), 1);
    }

    #[test]
    fn zero_capacity_cache_keeps_one_entry() {
        let cache = TranscriptCache::new(0);
        let first = audio_file(b"first");
        let second = audio_file(b"second");

        cache.set(first.path(), cache_entry("model-a", "first"));
        cache.set(second.path(), cache_entry("model-a", "second"));

        assert_eq!(cache.size(), 1);
        assert!(cache.get(second.path(), "model-a").is_some());
    }
}
