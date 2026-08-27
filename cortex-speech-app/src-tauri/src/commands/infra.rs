//! Infrastructure / diagnostics IPC commands — slice 11 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use infra::*;`),
//! so `lib.rs`'s invoke_handler still names `commands::save_session` and the frontend invokes are
//! untouched. Same functions, only relocated.
//!
//! The small non-data-domain app plumbing: last-crash report, media-cache asset register/url, PCM/VAD
//! cache info + clear, fingerprint count, session save/restore, couch-review start/stop, transcript
//! diff, and the telemetry span/stat readers.

use super::{run_blocking, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::diff::TextDiff;
use crate::ipc_contract::{CommandErrorV1, SuggestedActionV1};
use crate::validation::input as validate;
use crate::AppState;
use std::sync::Arc;
use tauri::State;

const MAX_PUBLIC_DIFF_WORDS: usize = 10_000;
const MAX_PUBLIC_DIFF_LCS_CELLS: usize = 12_500_000;

fn diff_rate_limited_error() -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", "Too many transcript comparisons. Retry in a moment.", true)
        .suggested(SuggestedActionV1::Retry)
}

fn validate_public_diff_input(raw: &str, annotated: &str) -> Result<(), CommandErrorV1> {
    validate::validate_text(raw, 100_000, "Raw text")
        .and_then(|_| validate::validate_text(annotated, 100_000, "Annotated text"))
        .map_err(|_| CommandErrorV1::new("INVALID_DIFF_INPUT", "The transcript comparison input is invalid.", false))?;

    let raw_words = raw.split_whitespace().count();
    let annotated_words = annotated.split_whitespace().count();
    if raw_words > MAX_PUBLIC_DIFF_WORDS || annotated_words > MAX_PUBLIC_DIFF_WORDS {
        return Err(CommandErrorV1::new(
            "DIFF_TOO_LARGE",
            "The transcript comparison is too large to process safely.",
            false,
        )
        .detail("rawWords", raw_words as i64)
        .detail("annotatedWords", annotated_words as i64)
        .detail("maxWords", MAX_PUBLIC_DIFF_WORDS as i64));
    }

    let requested_cells = raw_words.saturating_mul(annotated_words);
    if requested_cells > MAX_PUBLIC_DIFF_LCS_CELLS {
        return Err(CommandErrorV1::new(
            "DIFF_TOO_COMPLEX",
            "The transcript comparison would require too much memory.",
            false,
        )
        .detail("rawWords", raw_words as i64)
        .detail("annotatedWords", annotated_words as i64)
        .detail("maxCells", MAX_PUBLIC_DIFF_LCS_CELLS as i64));
    }
    Ok(())
}

/// Return a one-line summary of the most recent crash report (if the last session panicked), surfaced
/// exactly once. The frontend shows it as a notification on startup so a mid-review crash — after which
/// the app relaunches looking normal — is no longer silent. The full report stays in the rolling log.
#[tauri::command]
pub fn take_last_crash(state: State<'_, AppState>) -> Option<String> {
    let data_dir = state.lock_data_dir().clone()?;
    crate::crash::take_latest_crash_summary(&data_dir)
}

#[tauri::command]
pub async fn register_media_asset(
    audio_path: String,
    state: State<'_, AppState>,
) -> Result<crate::media::MediaGrant, String> {
    RATE_LIMITER.check("register_media_asset")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    // Ordinary Library/curation playback needs only imported-file membership. Legacy schema-65 rows
    // with a missing identity remain playable here because this grant can never authorize a review
    // decision: playback_binding rejects unverified grants. Keep the stronger proof path separate.
    let canonical = {
        let db = state.lock_db();
        crate::media::MediaRegistry::ensure_imported(&db, &audio_path)?
    };
    let registry = Arc::clone(&state.media_registry);
    let materializer = Arc::clone(&state.media_materializer);
    run_blocking(move || materializer.register_unverified(&registry, &data_dir, std::path::PathBuf::from(canonical)))
        .await
}

/// Mint the immutable, decoded-PCM-verified grant required by the policy-4 review boundary. This is
/// intentionally separate from ordinary media playback so a legacy/null fingerprint cannot break
/// Library listening, while it still fails closed before any human-truth write is possible.
#[tauri::command]
pub async fn register_review_media_asset(
    audio_path: String,
    state: State<'_, AppState>,
) -> Result<crate::media::MediaGrant, String> {
    STRICT_RATE_LIMITER.check("register_review_media_asset")?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    // Capture authority under a short DB lock, then release it before the potentially multi-GB copy
    // and PCM decode. Never acquire registry -> DB: session commands deliberately lease registry
    // state first and release it before entering the database transaction.
    let source = {
        let db = state.lock_db();
        crate::media::MediaRegistry::validate_playback_source(&db, &audio_path)?
    };
    let registry = Arc::clone(&state.media_registry);
    let materializer = Arc::clone(&state.media_materializer);
    run_blocking(move || materializer.register_verified(&registry, &data_dir, source)).await
}

#[tauri::command]
pub fn get_media_asset_url(id: String, state: State<'_, AppState>) -> Result<String, String> {
    RATE_LIMITER.check("get_media_asset_url")?;
    validate::validate_identifier(&id)?;
    let (result, retired) = {
        let mut registry = state.lock_media_registry();
        let result = registry.resolve(&id);
        (result, registry.take_retired_artifacts())
    };
    crate::media::cleanup_retired_media_artifacts(retired, "expired media grant");
    result
}

#[tauri::command]
pub fn get_fingerprint_count(state: State<'_, AppState>) -> Result<usize, String> {
    RATE_LIMITER.check("get_fingerprint_count")?;
    Ok(state.fingerprint.count())
}

#[tauri::command]
#[specta::specta]
pub fn compute_diff(raw: String, annotated: String) -> Result<TextDiff, CommandErrorV1> {
    RATE_LIMITER.check("compute_diff").map_err(|_| diff_rate_limited_error())?;
    validate_public_diff_input(&raw, &annotated)?;
    let meta = crate::telemetry::Tracer::metadata(vec![
        ("raw_len", raw.len().to_string()),
        ("ann_len", annotated.len().to_string()),
    ]);
    Ok(crate::telemetry::TRACER.record("diff.compute", meta, || crate::diff::compute_diff(&raw, &annotated)))
}

#[cfg(test)]
mod typed_diff_ipc_tests {
    use super::*;

    #[test]
    fn public_diff_refuses_misleading_or_memory_unsafe_work() {
        let oversized = "w ".repeat(MAX_PUBLIC_DIFF_WORDS + 1);
        let error = validate_public_diff_input(&oversized, "small").expect_err("oversized diff must refuse");
        assert_eq!(error.code, "DIFF_TOO_LARGE");
        assert_eq!(error.details.get("maxWords"), Some(&crate::ipc_contract::CommandErrorDetailV1::Number(10_000.0)));

        let expensive = "w ".repeat(4_000);
        let error = validate_public_diff_input(&expensive, &expensive).expect_err("memory-heavy diff must refuse");
        assert_eq!(error.code, "DIFF_TOO_COMPLEX");
        assert_eq!(
            error.details.get("maxCells"),
            Some(&crate::ipc_contract::CommandErrorDetailV1::Number(12_500_000.0))
        );

        let acceptable = "w ".repeat(3_500);
        validate_public_diff_input(&acceptable, &acceptable)
            .expect("comparison below the cell ceiling must pass admission");
    }

    #[test]
    fn public_diff_validation_error_is_typed_and_contains_no_input() {
        let hostile = format!("token=secret {}", "x".repeat(100_000));
        let error = validate_public_diff_input(&hostile, "ok").expect_err("invalid input must refuse");
        let wire = serde_json::to_string(&error).expect("serialize public diff error");
        assert!(wire.contains("INVALID_DIFF_INPUT"));
        assert!(!wire.contains("secret"));
        assert!(!wire.contains("token"));
    }
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
    // The data dir is where the session is remembered, so a link survives closing the app. None (no
    // data dir registered) simply means nothing is remembered — the previous per-session behaviour.
    let data_dir = state.lock_data_dir().clone();
    crate::couch::start(db_path, reviewers.unwrap_or_default(), data_dir)
}

/// Revoke ONE reviewer's Couch Review link, leaving everyone else's working
/// (docs/REMOTE_REVIEW_PLAN.md §3.7). Their completed work, scores and audit trail are untouched —
/// those are a record of what happened, not a permission being withdrawn.
#[tauri::command]
pub fn revoke_couch_reviewer(reviewer: String) -> Result<crate::couch::CouchStatus, String> {
    STRICT_RATE_LIMITER.check("revoke_couch_reviewer")?;
    crate::couch::revoke(&reviewer)
}

/// Stop Couch Review and invalidate every reviewer's session token.
#[tauri::command]
pub fn stop_couch_review(state: State<'_, AppState>) -> Result<crate::couch::CouchStatus, String> {
    STRICT_RATE_LIMITER.check("stop_couch_review")?;
    let data_dir = state.lock_data_dir().clone();
    crate::couch::stop_with_data_dir(data_dir.as_deref())
}
