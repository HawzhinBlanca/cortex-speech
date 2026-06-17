use crate::db::Database;
use crate::error::AppResult;
use crate::models::ModelManager;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Instant;
use sysinfo::System;

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

pub fn check_memory_pressure() -> bool {
    current_memory_mb() > 2000
}

pub fn health_check(db: &Database, model_mgr: &ModelManager) -> AppResult<serde_json::Value> {
    let info = db.info()?;
    let segment_count = info["segmentCount"].as_i64().unwrap_or(0);
    let db_size = info["sizeBytes"].as_i64().unwrap_or(0);
    let uptime = INSTANT.elapsed().as_secs();
    let missing_required = model_mgr.missing_required_model_names();
    let missing_optional = model_mgr.missing_optional_model_names();
    Ok(serde_json::json!({
        "status": if missing_required.is_empty() { "ok" } else { "models_needed" },
        "db_size": db_size,
        "uptime": uptime,
        "segment_count": segment_count,
        "memory_mb": current_memory_mb(),
        "missing_models": missing_required,
        "missing_optional_models": missing_optional,
    }))
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
