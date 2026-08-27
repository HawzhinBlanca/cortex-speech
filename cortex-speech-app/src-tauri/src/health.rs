use crate::db::Database;
use crate::error::AppResult;
use crate::models::ModelManager;
use crate::settings::AppSettings;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Instant;
use sysinfo::System;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub status: String,
    pub db_size: i64,
    pub uptime: u64,
    pub segment_count: i64,
    pub memory_mb: u64,
    pub primary_asr_model: String,
    pub missing_models: Vec<String>,
    pub missing_optional_models: Vec<String>,
    pub snapshot_last_success_epoch_secs: Option<u64>,
    pub snapshot_consecutive_failures: usize,
    pub free_disk_bytes: Option<u64>,
}

pub static INSTANT: LazyLock<Instant> = LazyLock::new(Instant::now);
static SYS: LazyLock<Mutex<System>> = LazyLock::new(|| Mutex::new(System::new()));

fn lock_system() -> MutexGuard<'static, System> {
    SYS.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned system health lock");
        poisoned.into_inner()
    })
}

pub fn current_memory_mb() -> u64 {
    let mut sys = lock_system();
    sys.refresh_memory();
    let total = sys.total_memory();
    let available = sys.available_memory();
    (total - available) / 1_048_576
}

/// System memory actually AVAILABLE for new allocations, in MiB.
pub fn available_memory_mb() -> u64 {
    let mut sys = lock_system();
    sys.refresh_memory();
    sys.available_memory() / 1_048_576
}

/// True when the system is genuinely low on memory: under 1 GiB AVAILABLE. The previous definition
/// (system-wide USED > 2000 MB) was always true on any modern machine, and its result was discarded
/// at the only call site — dead code masquerading as backpressure (true-10 audit). Callers must act
/// on this (slow down / warn), not merely call it.
pub fn check_memory_pressure() -> bool {
    available_memory_mb() < 1024
}

/// Free bytes on the volume holding `path` (the data dir) — the snapshot/export safety margin.
/// `None` when the volume cannot be resolved.
pub fn free_disk_bytes_for(path: &std::path::Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        // The longest matching mount point is the volume actually holding the path.
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space())
}

pub fn health_check(
    db: &Database,
    model_mgr: &ModelManager,
    settings: &AppSettings,
    data_dir: Option<&std::path::Path>,
) -> AppResult<HealthSnapshot> {
    let info = db.info()?;
    let segment_count = info["segmentCount"].as_i64().unwrap_or(0);
    let db_size = info["sizeBytes"].as_i64().unwrap_or(0);
    let uptime = INSTANT.elapsed().as_secs();
    let missing_required = model_mgr.missing_required_model_names_for(&settings.asr_model_size);
    let health_status = if missing_required.is_empty() { "ok" } else { "models_needed" }.to_string();
    let missing_required = missing_required.into_iter().map(str::to_string).collect();
    // The shipped health response must not advertise diagnostic ASR artifacts. Report only missing
    // production support models not already covered by the required Silero list.
    let missing_optional = model_mgr
        .missing_production_models()
        .into_iter()
        .filter(|model| model.filename != "silero_vad_v4.onnx")
        .map(|model| model.name.to_string())
        .collect::<Vec<_>>();
    // True-10 audit: the auto-snapshot safety net could silently stop for months (warn-only in a
    // detached thread) and disk exhaustion was checked nowhere. Both are now health signals.
    let snapshot_health = crate::snapshot::snapshot_health();
    let free_disk_bytes = data_dir.and_then(free_disk_bytes_for);
    Ok(HealthSnapshot {
        status: health_status,
        db_size,
        uptime,
        segment_count,
        memory_mb: current_memory_mb(),
        primary_asr_model: format!("{:?}", settings.asr_model_size),
        missing_models: missing_required,
        missing_optional_models: missing_optional,
        snapshot_last_success_epoch_secs: snapshot_health.last_success_epoch_secs,
        snapshot_consecutive_failures: snapshot_health.consecutive_failures,
        free_disk_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poison_system_lock_for_test() {
        let _guard = SYS.lock().expect("lock system health");
        panic!("poison system health");
    }

    #[test]
    fn system_health_recovers_poisoned_lock() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            poison_system_lock_for_test();
        }));

        let _ = current_memory_mb();
        let guard = lock_system();
        assert!(guard.total_memory() >= guard.available_memory());
    }
}
