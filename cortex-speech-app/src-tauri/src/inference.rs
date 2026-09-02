use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Instant;

pub struct InferenceMetrics {
    pub vad_latencies: Mutex<Vec<f64>>,
    pub asr_latencies: Mutex<Vec<f64>>,
    pub align_latencies: Mutex<Vec<f64>>,
    pub vad_calls: std::sync::atomic::AtomicU64,
    pub asr_calls: std::sync::atomic::AtomicU64,
    pub align_calls: std::sync::atomic::AtomicU64,
    pub vad_failures: std::sync::atomic::AtomicU64,
    pub asr_failures: std::sync::atomic::AtomicU64,
    pub model_load_time_ms: Mutex<f64>,
}

impl InferenceMetrics {
    pub fn new() -> Self {
        Self {
            vad_latencies: Mutex::new(Vec::new()),
            asr_latencies: Mutex::new(Vec::new()),
            align_latencies: Mutex::new(Vec::new()),
            vad_calls: std::sync::atomic::AtomicU64::new(0),
            asr_calls: std::sync::atomic::AtomicU64::new(0),
            align_calls: std::sync::atomic::AtomicU64::new(0),
            vad_failures: std::sync::atomic::AtomicU64::new(0),
            asr_failures: std::sync::atomic::AtomicU64::new(0),
            model_load_time_ms: Mutex::new(0.0),
        }
    }

    fn lock_latencies<'a>(latencies: &'a Mutex<Vec<f64>>, label: &str) -> MutexGuard<'a, Vec<f64>> {
        latencies.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned {label} inference latency metrics");
            poisoned.into_inner()
        })
    }

    fn record_latency(latencies: &Mutex<Vec<f64>>, label: &str, elapsed_ms: f64) {
        let mut values = Self::lock_latencies(latencies, label);
        values.push(elapsed_ms);
        if values.len() > 1000 {
            values.drain(0..100);
        }
    }

    fn snapshot_latencies(latencies: &Mutex<Vec<f64>>, label: &str) -> Vec<f64> {
        Self::lock_latencies(latencies, label).clone()
    }

    fn lock_model_load_time_ms(&self) -> MutexGuard<'_, f64> {
        self.model_load_time_ms.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned model-load inference metric");
            poisoned.into_inner()
        })
    }

    pub fn set_model_load_time_ms(&self, value: f64) {
        *self.lock_model_load_time_ms() = value;
    }

    fn model_load_time_ms(&self) -> f64 {
        *self.lock_model_load_time_ms()
    }
}

impl Default for InferenceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

pub static INFERENCE_METRICS: LazyLock<InferenceMetrics> = LazyLock::new(InferenceMetrics::new);

#[derive(Debug, Clone, PartialEq)]
pub struct InferenceKindStats {
    pub calls: u64,
    pub failures: u64,
    pub p50_ms: f64,
    pub p99_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InferenceStatsSnapshot {
    pub vad: InferenceKindStats,
    pub asr: InferenceKindStats,
    pub model_load_ms: f64,
}

pub struct InferenceTimer {
    start: Instant,
    kind: &'static str,
}

impl InferenceTimer {
    pub fn start(kind: &'static str) -> Self {
        Self { start: Instant::now(), kind }
    }

    pub fn finish(self, success: bool) {
        let elapsed = self.start.elapsed().as_secs_f64() * 1000.0;
        match self.kind {
            "vad" => {
                INFERENCE_METRICS.vad_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                InferenceMetrics::record_latency(&INFERENCE_METRICS.vad_latencies, "vad", elapsed);
                if !success {
                    INFERENCE_METRICS.vad_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            "asr" => {
                INFERENCE_METRICS.asr_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                InferenceMetrics::record_latency(&INFERENCE_METRICS.asr_latencies, "asr", elapsed);
                if !success {
                    INFERENCE_METRICS.asr_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            "align" => {
                INFERENCE_METRICS.align_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                InferenceMetrics::record_latency(&INFERENCE_METRICS.align_latencies, "align", elapsed);
            }
            _ => {}
        }
    }
}

pub fn get_inference_stats() -> InferenceStatsSnapshot {
    let vad_latencies = InferenceMetrics::snapshot_latencies(&INFERENCE_METRICS.vad_latencies, "vad");
    let asr_latencies = InferenceMetrics::snapshot_latencies(&INFERENCE_METRICS.asr_latencies, "asr");

    let vad_p50 = percentile(&vad_latencies, 50.0);
    let vad_p99 = percentile(&vad_latencies, 99.0);
    let asr_p50 = percentile(&asr_latencies, 50.0);
    let asr_p99 = percentile(&asr_latencies, 99.0);
    let model_load = INFERENCE_METRICS.model_load_time_ms();

    InferenceStatsSnapshot {
        vad: InferenceKindStats {
            calls: INFERENCE_METRICS.vad_calls.load(std::sync::atomic::Ordering::Relaxed),
            failures: INFERENCE_METRICS.vad_failures.load(std::sync::atomic::Ordering::Relaxed),
            p50_ms: vad_p50,
            p99_ms: vad_p99,
        },
        asr: InferenceKindStats {
            calls: INFERENCE_METRICS.asr_calls.load(std::sync::atomic::Ordering::Relaxed),
            failures: INFERENCE_METRICS.asr_failures.load(std::sync::atomic::Ordering::Relaxed),
            p50_ms: asr_p50,
            p99_ms: asr_p99,
        },
        model_load_ms: model_load,
    }
}

fn percentile(data: &[f64], p: f64) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut sorted = data.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64) as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_metrics_recover_poisoned_latency_lock() {
        let metrics = InferenceMetrics::new();
        InferenceMetrics::record_latency(&metrics.vad_latencies, "vad", 12.0);

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = metrics.vad_latencies.lock().expect("lock vad metrics");
            panic!("poison vad metrics");
        }));

        InferenceMetrics::record_latency(&metrics.vad_latencies, "vad", 18.0);
        assert_eq!(InferenceMetrics::snapshot_latencies(&metrics.vad_latencies, "vad"), vec![12.0, 18.0]);
    }

    #[test]
    fn inference_metrics_recover_poisoned_model_load_lock() {
        let metrics = InferenceMetrics::new();
        metrics.set_model_load_time_ms(42.0);

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = metrics.model_load_time_ms.lock().expect("lock model-load metric");
            panic!("poison model-load metric");
        }));

        metrics.set_model_load_time_ms(64.0);
        assert_eq!(metrics.model_load_time_ms(), 64.0);
    }
}
