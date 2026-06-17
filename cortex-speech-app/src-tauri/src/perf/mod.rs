use lru::LruCache;
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;

/// Thread-safe memoization cache for expensive computations.
pub struct Memoizer<K, V> {
    cache: Mutex<LruCache<K, V>>,
}

impl<K: Eq + Hash + Clone, V: Clone> Memoizer<K, V> {
    pub fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        Self { cache: Mutex::new(LruCache::new(capacity)) }
    }

    fn lock_cache(&self) -> MutexGuard<'_, LruCache<K, V>> {
        self.cache.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned memoizer cache");
            poisoned.into_inner()
        })
    }

    /// Get or compute a value.
    pub fn memoize<F: FnOnce(&K) -> V>(&self, key: &K, compute: F) -> V {
        let mut cache = self.lock_cache();
        if let Some(value) = cache.get(key) {
            return value.clone();
        }
        let value = compute(key);
        cache.put(key.clone(), value.clone());
        value
    }

    pub fn clear(&self) {
        self.lock_cache().clear();
    }

    pub fn len(&self) -> usize {
        self.lock_cache().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// Global memoizers
pub static NORMALIZER_CACHE: LazyLock<Memoizer<String, String>> = LazyLock::new(|| Memoizer::new(10_000));
pub static DIFF_CACHE: LazyLock<Memoizer<(String, String), crate::diff::TextDiff>> =
    LazyLock::new(|| Memoizer::new(500));

/// Parallel batch processor using rayon.
/// Processes items in parallel and collects results.
pub fn parallel_batch<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Send + Sync,
    R: Send,
    F: Fn(&T) -> R + Send + Sync,
{
    use rayon::prelude::*;
    items.par_iter().map(f).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memoizer_basic() {
        let memo = Memoizer::new(10);
        let mut call_count = 0;

        let result1 = memo.memoize(&"key1", |_| {
            call_count += 1;
            "computed".to_string()
        });
        assert_eq!(result1, "computed");
        assert_eq!(call_count, 1);

        // Second call should use cache
        let result2 = memo.memoize(&"key1", |_| {
            call_count += 1;
            "recomputed".to_string()
        });
        assert_eq!(result2, "computed");
        assert_eq!(call_count, 1);
    }

    #[test]
    fn test_memoizer_different_keys() {
        let memo = Memoizer::new(10);
        let mut call_count = 0;

        memo.memoize(&"a", |_| {
            call_count += 1;
            1
        });
        memo.memoize(&"b", |_| {
            call_count += 1;
            2
        });
        assert_eq!(call_count, 2);

        // Both should be cached now
        memo.memoize(&"a", |_| {
            call_count += 1;
            99
        });
        memo.memoize(&"b", |_| {
            call_count += 1;
            99
        });
        assert_eq!(call_count, 2);
    }

    #[test]
    fn test_memoizer_eviction() {
        let memo = Memoizer::new(2);
        memo.memoize(&"a", |_| 1);
        memo.memoize(&"b", |_| 2);
        memo.memoize(&"c", |_| 3); // Evicts "a"

        // "a" should be evicted
        let result = memo.memoize(&"a", |_| 99);
        assert_eq!(result, 99);
    }

    #[test]
    fn test_memoizer_clear() {
        let memo = Memoizer::new(10);
        memo.memoize(&"key", |_| 42);
        assert_eq!(memo.len(), 1);
        memo.clear();
        assert_eq!(memo.len(), 0);
    }

    #[test]
    fn test_memoizer_recovers_poisoned_cache() {
        let memo = Memoizer::new(10);
        memo.memoize(&"key", |_| 42);

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = memo.cache.lock().expect("lock memoizer cache");
            panic!("poison memoizer cache");
        }));

        assert_eq!(memo.memoize(&"key", |_| 99), 42);
        assert_eq!(memo.len(), 1);
        memo.clear();
        assert!(memo.is_empty());
    }

    #[test]
    fn test_memoizer_zero_capacity_keeps_one_entry() {
        let memo = Memoizer::new(0);
        memo.memoize(&"a", |_| 1);
        memo.memoize(&"b", |_| 2);

        assert_eq!(memo.len(), 1);
        assert_eq!(memo.memoize(&"b", |_| 99), 2);
    }

    #[test]
    fn test_parallel_batch() {
        let items = vec![1, 2, 3, 4, 5];
        let results = parallel_batch(&items, |x| x * 2);
        assert_eq!(results, vec![2, 4, 6, 8, 10]);
    }
}
