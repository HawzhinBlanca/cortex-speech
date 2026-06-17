use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// A tracing span for measuring operation duration and recording metadata.
#[derive(Clone, Serialize)]
pub struct Span {
    pub operation: &'static str,
    pub start: String,
    pub duration_ms: f64,
    pub metadata: HashMap<String, String>,
    pub success: bool,
    pub error: Option<String>,
}

/// Tracer for collecting operation spans.
pub struct Tracer {
    spans: Mutex<Vec<Span>>,
    max_spans: usize,
}

impl Tracer {
    pub fn new(max_spans: usize) -> Self {
        Self { spans: Mutex::new(Vec::with_capacity(max_spans)), max_spans }
    }

    fn lock_spans(&self) -> MutexGuard<'_, Vec<Span>> {
        self.spans.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned telemetry span buffer");
            poisoned.into_inner()
        })
    }

    /// Record an operation with timing.
    pub fn record<F, T>(&self, operation: &'static str, metadata: HashMap<String, String>, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let start = Instant::now();
        let start_str = chrono::Local::now().to_rfc3339();
        let result = f();
        let duration = start.elapsed();

        let mut spans = self.lock_spans();
        let span = Span {
            operation,
            start: start_str,
            duration_ms: duration.as_secs_f64() * 1000.0,
            metadata,
            success: true,
            error: None,
        };
        spans.push(span);
        if spans.len() > self.max_spans {
            spans.remove(0);
        }

        result
    }

    /// Record an operation with timing, capturing success/failure.
    pub fn record_result<F, T, E>(
        &self,
        operation: &'static str,
        metadata: HashMap<String, String>,
        f: F,
    ) -> Result<T, E>
    where
        F: FnOnce() -> Result<T, E>,
        E: std::fmt::Display,
    {
        let start = Instant::now();
        let start_str = chrono::Local::now().to_rfc3339();
        let result = f();
        let duration = start.elapsed();

        let mut spans = self.lock_spans();
        let (success, error) = match &result {
            Ok(_) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        };
        spans.push(Span {
            operation,
            start: start_str,
            duration_ms: duration.as_secs_f64() * 1000.0,
            metadata,
            success,
            error,
        });
        if spans.len() > self.max_spans {
            spans.remove(0);
        }

        result
    }

    pub fn get_recent(&self) -> Vec<Span> {
        self.lock_spans().clone()
    }

    pub fn clear(&self) {
        self.lock_spans().clear();
    }

    /// Start a performance span, returning a guard that records on drop.
    pub fn start_span(&self, operation: &'static str, metadata: HashMap<String, String>) -> SpanGuard<'_> {
        SpanGuard { tracer: self, operation, metadata, start: Instant::now() }
    }

    /// Get aggregate statistics about recent operations.
    pub fn stats(&self) -> TracingStats {
        let spans = self.lock_spans();
        let total = spans.len();
        let failures = spans.iter().filter(|s| !s.success).count();
        let total_duration: f64 = spans.iter().map(|s| s.duration_ms).sum();
        let avg_duration = if total > 0 { total_duration / total as f64 } else { 0.0 };
        TracingStats { total_spans: total, failures, total_duration_ms: total_duration, avg_duration_ms: avg_duration }
    }

    /// Helper: create a metadata map from key-value pairs.
    pub fn metadata(pairs: Vec<(&str, String)>) -> HashMap<String, String> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }
}

#[derive(Clone, Serialize)]
pub struct TracingStats {
    pub total_spans: usize,
    pub failures: usize,
    pub total_duration_ms: f64,
    pub avg_duration_ms: f64,
}

/// A guard that records a span on drop.
pub struct SpanGuard<'a> {
    tracer: &'a Tracer,
    operation: &'static str,
    metadata: HashMap<String, String>,
    start: Instant,
}

impl<'a> SpanGuard<'a> {
    pub fn end_span(self) {
        drop(self);
    }
}

impl<'a> Drop for SpanGuard<'a> {
    fn drop(&mut self) {
        let duration = self.start.elapsed();
        let start_str = chrono::Local::now().to_rfc3339();
        let mut spans = self.tracer.lock_spans();
        spans.push(Span {
            operation: self.operation,
            start: start_str,
            duration_ms: duration.as_secs_f64() * 1000.0,
            metadata: std::mem::take(&mut self.metadata),
            success: true,
            error: None,
        });
        if spans.len() > self.tracer.max_spans {
            spans.remove(0);
        }
    }
}

// Global tracer instance
pub static TRACER: std::sync::LazyLock<Tracer> = std::sync::LazyLock::new(|| Tracer::new(10_000));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracer_records_span() {
        let tracer = Tracer::new(100);
        let meta = Tracer::metadata(vec![("file", "test.wav".to_string())]);

        let result = tracer.record("test_op", meta, || 42);
        assert_eq!(result, 42);

        let spans = tracer.get_recent();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].operation, "test_op");
        assert!(spans[0].duration_ms > 0.0);
        assert!(spans[0].success);
    }

    #[test]
    fn test_tracer_records_failure() {
        let tracer = Tracer::new(100);
        let meta = Tracer::metadata(vec![]);

        let result: Result<i32, String> = tracer.record_result("fail_op", meta, || Err("oops".to_string()));
        assert!(result.is_err());

        let spans = tracer.get_recent();
        assert!(!spans[0].success);
        assert_eq!(spans[0].error, Some("oops".to_string()));
    }

    #[test]
    fn test_tracer_max_spans() {
        let tracer = Tracer::new(3);
        for i in 0..5 {
            tracer.record("op", Tracer::metadata(vec![("i", i.to_string())]), || {});
        }
        assert_eq!(tracer.get_recent().len(), 3);
        // Oldest should be evicted (i=0,1 should be gone)
        assert_eq!(tracer.get_recent()[0].metadata.get("i").unwrap(), "2");
    }

    #[test]
    fn test_tracer_stats() {
        let tracer = Tracer::new(100);
        tracer.record("ok", Tracer::metadata(vec![]), || {});
        tracer.record("ok", Tracer::metadata(vec![]), || {});
        let meta = Tracer::metadata(vec![]);
        let _: Result<i32, String> = tracer.record_result("fail", meta, || Err("error".to_string()));

        let stats = tracer.stats();
        assert_eq!(stats.total_spans, 3);
        assert_eq!(stats.failures, 1);
        assert!(stats.avg_duration_ms > 0.0);
    }

    #[test]
    fn test_tracer_clear() {
        let tracer = Tracer::new(100);
        tracer.record("op", Tracer::metadata(vec![]), || {});
        assert_eq!(tracer.get_recent().len(), 1);
        tracer.clear();
        assert_eq!(tracer.get_recent().len(), 0);
    }

    #[test]
    fn tracer_recovers_poisoned_span_buffer() {
        let tracer = Tracer::new(10);

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = tracer.spans.lock().expect("lock telemetry spans");
            panic!("poison telemetry span buffer");
        }));

        tracer.record("recovered_record", Tracer::metadata(vec![]), || {});
        let _: Result<(), String> =
            tracer.record_result("recovered_failure", Tracer::metadata(vec![]), || Err("failed".to_string()));
        {
            let _span = tracer.start_span("recovered_guard", Tracer::metadata(vec![]));
        }

        let spans = tracer.get_recent();
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].operation, "recovered_record");
        assert!(!spans[1].success);
        assert_eq!(spans[2].operation, "recovered_guard");

        let stats = tracer.stats();
        assert_eq!(stats.total_spans, 3);
        assert_eq!(stats.failures, 1);

        tracer.clear();
        assert!(tracer.get_recent().is_empty());
    }
}
