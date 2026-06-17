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
        let store = self.lock_store();
        store.get(&Self::cache_key(&hash, model_id, chunk_suffix)).cloned()
    }

    pub fn set(&self, audio_path: &Path, entry: CacheEntry) {
        self.set_chunk(audio_path, None, entry);
    }

    pub fn set_chunk(&self, audio_path: &Path, chunk_suffix: Option<&str>, entry: CacheEntry) {
        if let Ok(hash) = Self::compute_hash(audio_path) {
            let mut store = self.lock_store();
            let key = Self::cache_key(&hash, &entry.model_id, chunk_suffix);
            while store.len() >= self.max_entries {
                if let Some(key) = store.keys().next().cloned() {
                    store.remove(&key);
                } else {
                    break;
                }
            }
            store.insert(key, entry);
        }
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
