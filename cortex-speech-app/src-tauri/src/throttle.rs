use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Maximum number of distinct keys tracked simultaneously.
/// On overflow the oldest key (by insertion order) is evicted so the
/// rate-limiter never becomes an unbounded memory sink in long-running processes.
const MAX_BUCKET_KEYS: usize = 256;

struct Bucket {
    tokens: f64,
    last_check: Instant,
}

pub struct RateLimiter {
    rate: f64,
    burst: f64,
    buckets: HashMap<String, Bucket>,
    /// Insertion-ordered key list used for O(1) oldest-key eviction.
    insertion_order: Vec<String>,
}

impl RateLimiter {
    pub fn new(rate: u64) -> Self {
        Self { rate: rate as f64, burst: rate as f64, buckets: HashMap::new(), insertion_order: Vec::new() }
    }

    pub fn new_with_burst(rate: u64, burst: u64) -> Self {
        Self { rate: rate as f64, burst: burst as f64, buckets: HashMap::new(), insertion_order: Vec::new() }
    }

    pub fn check(&mut self, key: &str) -> Result<(), String> {
        let now = Instant::now();

        // Evict oldest entry if we're at capacity and this is a new key.
        if !self.buckets.contains_key(key) && self.buckets.len() >= MAX_BUCKET_KEYS {
            if let Some(oldest) = self.insertion_order.first().cloned() {
                self.buckets.remove(&oldest);
                self.insertion_order.retain(|k| k != &oldest);
            }
        }

        // Insert new bucket if key is not yet tracked.
        if !self.buckets.contains_key(key) {
            self.buckets.insert(key.to_string(), Bucket { tokens: self.burst, last_check: now });
            self.insertion_order.push(key.to_string());
        }

        let Some(entry) = self.buckets.get_mut(key) else {
            return Err(format!("Rate limiter internal bucket state inconsistent for '{key}'"));
        };
        let elapsed = now.duration_since(entry.last_check).as_secs_f64();
        entry.last_check = now;
        entry.tokens = (entry.tokens + elapsed * self.rate).min(self.burst);

        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            Ok(())
        } else {
            Err(format!("Rate limit exceeded for '{key}'. Please slow down."))
        }
    }
}

pub struct GlobalRateLimiter {
    inner: std::sync::OnceLock<Mutex<RateLimiter>>,
    rate: u64,
    burst: u64,
}

impl GlobalRateLimiter {
    pub const fn new(rate: u64) -> Self {
        Self { inner: std::sync::OnceLock::new(), rate, burst: rate }
    }

    pub const fn new_with_burst(rate: u64, burst: u64) -> Self {
        Self { inner: std::sync::OnceLock::new(), rate, burst }
    }

    fn get(&self) -> &Mutex<RateLimiter> {
        self.inner.get_or_init(|| Mutex::new(RateLimiter::new_with_burst(self.rate, self.burst)))
    }

    pub fn check(&self, key: &str) -> Result<(), String> {
        let mut limiter = self.get().lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned rate limiter lock");
            poisoned.into_inner()
        });
        limiter.check(key)
    }
}

pub static RATE_LIMITER: GlobalRateLimiter = GlobalRateLimiter::new(60);
pub static STRICT_RATE_LIMITER: GlobalRateLimiter = GlobalRateLimiter::new_with_burst(10, 5);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_up_to_burst() {
        // rate = 0 (no refill) makes this test DETERMINISTIC regardless of CPU scheduling. With a
        // positive rate, more than 1/rate seconds of descheduling between the burst-exhausting call and
        // the next one refills a token and flakes the `is_err()` assertion — at rate=10 that window is
        // only 0.1s, reachable under load. A frozen bucket isolates the pure burst-exhaustion semantics.
        let mut rl = RateLimiter::new_with_burst(0, 3);
        assert!(rl.check("a").is_ok());
        assert!(rl.check("a").is_ok());
        assert!(rl.check("a").is_ok());
        assert!(rl.check("a").is_err(), "should be rate-limited after burst");
    }

    #[test]
    fn rate_limiter_bounded_bucket_count() {
        let mut rl = RateLimiter::new(100);
        // Register MAX_BUCKET_KEYS + 10 distinct keys; map must not exceed the cap.
        for i in 0..MAX_BUCKET_KEYS + 10 {
            let _ = rl.check(&format!("key-{i}"));
        }
        assert!(
            rl.buckets.len() <= MAX_BUCKET_KEYS,
            "bucket count {} exceeded cap {}",
            rl.buckets.len(),
            MAX_BUCKET_KEYS
        );
        assert_eq!(rl.insertion_order.len(), rl.buckets.len(), "order vec out of sync");
    }

    #[test]
    fn rate_limiter_independent_keys() {
        let mut rl = RateLimiter::new_with_burst(1, 1);
        assert!(rl.check("x").is_ok());
        assert!(rl.check("x").is_err());
        // Different key should get its own fresh bucket.
        assert!(rl.check("y").is_ok());
    }

    #[test]
    fn global_rate_limiter_recovers_poisoned_lock() {
        let limiter = GlobalRateLimiter::new_with_burst(10, 1);
        let _ = std::panic::catch_unwind(|| {
            let _guard = limiter.get().lock().expect("lock rate limiter");
            panic!("poison rate limiter");
        });

        assert!(limiter.check("after-poison").is_ok());
    }
}
