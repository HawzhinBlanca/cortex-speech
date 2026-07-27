//! Infrastructure / diagnostics IPC commands — slice 11 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use infra::*;`),
//! so `lib.rs`'s invoke_handler still names `commands::save_session` and the frontend invokes are
//! untouched. Same functions, only relocated.
//!
//! The small non-data-domain app plumbing: last-crash report, media-cache asset register/url, PCM/VAD
//! cache info + clear, fingerprint count, session save/restore, couch-review start/stop, transcript
//! diff, and the telemetry span/stat readers.

use super::{RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::diff::TextDiff;
use crate::validation::input as validate;
use crate::AppState;
use tauri::State;

/// Return a one-line summary of the most recent crash report (if the last session panicked), surfaced
/// exactly once. The frontend shows it as a notification on startup so a mid-review crash — after which
/// the app relaunches looking normal — is no longer silent. The full report stays in the rolling log.
#[tauri::command]
pub fn take_last_crash(state: State<'_, AppState>) -> Option<String> {
    let data_dir = state.lock_data_dir().clone()?;
    crate::crash::take_latest_crash_summary(&data_dir)
}

#[tauri::command]
// ponytail: `async fn` moves the whole body (incl. the multi-GB cache copy in grant_source) OFF the
// main/UI thread, which fixes the freeze. It runs on an async worker rather than a spawn_blocking
// pool because grant_source holds the media-registry MutexGuard across the copy (not Send-movable);
// a full spawn_blocking offload needs MediaRegistry restructured into check→copy→record phases (or an
// Arc handle) — deferred. There is no `.await` in the body, so the guard never crosses a suspend
// point and the future stays Send.
#[allow(clippy::unused_async)]
pub async fn register_media_asset(
    audio_path: String,
    state: State<'_, AppState>,
) -> Result<crate::media::MediaGrant, String> {
    RATE_LIMITER.check("register_media_asset")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let mut registry = state.lock_media_registry();
    // Round-25 #7: validate the source (membership check) under a SHORT-LIVED global db lock (fast),
    // then DROP that lock before the potentially multi-GB cache copy in grant_source — holding the
    // global db mutex across std::fs::copy froze every other DB-touching IPC (notably the UI's
    // get_segments) for the length of the copy the first time a large clip was played. The
    // media-registry lock is held throughout (only media commands take it), so this never deadlocks
    // with the db lock.
    let canonical = {
        let db = state.lock_db();
        registry.validate_source(&db, &audio_path)?
    };
    registry.grant_source(&data_dir, canonical)
}

#[tauri::command]
pub fn get_media_asset_url(id: String, state: State<'_, AppState>) -> Result<String, String> {
    RATE_LIMITER.check("get_media_asset_url")?;
    validate::validate_identifier(&id)?;
    let mut registry = state.lock_media_registry();
    registry.resolve(&id)
}

#[tauri::command]
pub fn get_cache_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("get_cache_info")?;
    Ok(serde_json::json!({ "entries": state.cache.size(), "maxEntries": 1000 }))
}

#[tauri::command]
pub fn clear_cache(state: State<'_, AppState>) -> Result<(), String> {
    STRICT_RATE_LIMITER.check("clear_cache")?;
    state.cache.clear();
    Ok(())
}

#[tauri::command]
pub fn get_fingerprint_count(state: State<'_, AppState>) -> Result<usize, String> {
    RATE_LIMITER.check("get_fingerprint_count")?;
    Ok(state.fingerprint.count())
}

#[tauri::command]
pub fn compute_diff(raw: String, annotated: String) -> Result<TextDiff, String> {
    RATE_LIMITER.check("compute_diff")?;
    validate::validate_text(&raw, 100000, "Raw text")?;
    validate::validate_text(&annotated, 100000, "Annotated text")?;
    let meta = crate::telemetry::Tracer::metadata(vec![
        ("raw_len", raw.len().to_string()),
        ("ann_len", annotated.len().to_string()),
    ]);
    Ok(crate::telemetry::TRACER.record("diff.compute", meta, || crate::diff::compute_diff(&raw, &annotated)))
}

#[tauri::command]
pub fn get_tracing_stats(_state: State<'_, AppState>) -> Result<crate::telemetry::TracingStats, String> {
    RATE_LIMITER.check("get_tracing_stats")?;
    Ok(crate::telemetry::TRACER.stats())
}

#[tauri::command]
pub fn get_recent_spans(count: Option<usize>) -> Result<Vec<crate::telemetry::Span>, String> {
    RATE_LIMITER.check("get_recent_spans")?;
    let spans = crate::telemetry::TRACER.get_recent();
    let count = count.unwrap_or(50).min(spans.len());
    Ok(spans.into_iter().rev().take(count).collect())
}

#[tauri::command]
pub fn clear_tracing_spans() -> Result<(), String> {
    STRICT_RATE_LIMITER.check("clear_tracing_spans")?;
    crate::telemetry::TRACER.clear();
    Ok(())
}

/// Persist the user's view-state (search query + sort order) so it survives a restart. The values
/// are held in the session manager so the periodic counts-only auto_save preserves them too.
#[tauri::command]
pub fn save_session(
    search_query: String,
    sort_order: String,
    filter_verified: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Throttle like every other infra.rs command: save_session is a webview-reachable DB write taken
    // under the GLOBAL db lock, and it was the lone one here without a limiter. The frontend debounces
    // it to ~1/800ms, so this never rejects a legitimate save — it only stops a webview loop that
    // bypasses the debounce from pinning the db lock and starving get_segments et al. (same class as
    // export_audio round-22 #5 / register_media_asset round-25 #7).
    RATE_LIMITER.check("save_session")?;
    validate::validate_text(&search_query, 1000, "search_query")?;
    validate::validate_text(&sort_order, 64, "sort_order")?;
    let db = state.lock_db();
    let mut session = state.lock_session();
    session.set_view_state(search_query, sort_order, filter_verified);
    session.save(&db).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn restore_session(state: State<'_, AppState>) -> Result<Option<crate::session::SessionState>, String> {
    RATE_LIMITER.check("restore_session")?;
    let mut session = state.lock_session();
    session.restore().map_err(|e| e.to_string())
}

/// Start Couch Review — the LAN-only, token-gated phone review server (see couch.rs for the privacy
/// stance). Explicit per-session start; returns one URL PER named reviewer, each carrying that
/// reviewer's own token. An empty `reviewers` list starts a single-reviewer session under the default
/// name, which is the previous behaviour exactly.
#[tauri::command]
pub fn start_couch_review(
    state: State<'_, AppState>,
    reviewers: Option<Vec<String>>,
) -> Result<crate::couch::CouchStatus, String> {
    STRICT_RATE_LIMITER.check("start_couch_review")?;
    let db_path = { state.lock_db().path().to_string() };
    crate::couch::start(db_path, reviewers.unwrap_or_default())
}

/// Stop Couch Review and invalidate every reviewer's session token.
#[tauri::command]
pub fn stop_couch_review() -> Result<crate::couch::CouchStatus, String> {
    STRICT_RATE_LIMITER.check("stop_couch_review")?;
    crate::couch::stop()
}
