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
const MAX_PUBLIC_RECENT_SPANS: usize = 200;

fn diagnostics_rate_limited_error(action: &str) -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", action, true).suggested(SuggestedActionV1::Retry)
}

fn media_rate_limited_error() -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", "Audio preparation is busy. Retry in a moment.", true)
        .suggested(SuggestedActionV1::Retry)
}

fn media_unavailable_error(code: &str, retryable: bool) -> CommandErrorV1 {
    CommandErrorV1::new(code, "This audio clip is unavailable. Reload the clip and retry.", retryable)
        .suggested(SuggestedActionV1::ReloadClip)
}

fn public_media_error(error: String) -> CommandErrorV1 {
    if error.starts_with(crate::media::MEDIA_MATERIALIZATION_BUSY_CODE) {
        CommandErrorV1::new("MEDIA_PREPARATION_BUSY", "Other audio is being prepared. Wait a moment, then retry.", true)
            .suggested(SuggestedActionV1::Retry)
    } else {
        media_unavailable_error("MEDIA_ASSET_UNAVAILABLE", false)
    }
}

fn renderer_safe_spans(spans: Vec<crate::telemetry::Span>) -> Vec<crate::ipc_contract::TracingSpanV1> {
    spans.into_iter().map(Into::into).collect()
}

fn public_recent_span_limit(count: Option<usize>) -> usize {
    count.unwrap_or(50).min(MAX_PUBLIC_RECENT_SPANS)
}

fn diff_rate_limited_error() -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", "Too many transcript comparisons. Retry in a moment.", true)
        .suggested(SuggestedActionV1::Retry)
}

fn public_session_error(code: &str, message: &str, retryable: bool) -> CommandErrorV1 {
    let error = CommandErrorV1::new(code, message, retryable);
    if retryable {
        error.suggested(SuggestedActionV1::Retry)
    } else {
        error.suggested(SuggestedActionV1::OpenHealth)
    }
}

fn validate_public_session_state(search_query: &str, sort_order: &str) -> Result<(), CommandErrorV1> {
    validate::validate_text(search_query, 1000, "search_query")
        .and_then(|_| validate::validate_text(sort_order, 64, "sort_order"))
        .map_err(|_| CommandErrorV1::new("INVALID_SESSION_STATE", "The saved workspace view is invalid.", false))
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

/// Return a generic renderer-safe notice when the previous session left any crash report, surfaced
/// exactly once. The frontend shows it at startup so a mid-review crash is no longer silent, while
/// the panic message, location and full report remain in backend-owned diagnostics.
#[tauri::command]
#[specta::specta]
pub fn take_last_crash(state: State<'_, AppState>) -> Result<Option<String>, CommandErrorV1> {
    RATE_LIMITER
        .check("take_last_crash")
        .map_err(|_| diagnostics_rate_limited_error("The previous-crash check is busy. Retry in a moment."))?;
    let Some(data_dir) = state.lock_data_dir().clone() else {
        return Ok(None);
    };
    Ok(crate::crash::take_latest_crash_summary(&data_dir))
}

#[tauri::command]
#[specta::specta]
pub async fn register_media_asset(
    audio_path: String,
    state: State<'_, AppState>,
) -> Result<crate::media::MediaGrant, CommandErrorV1> {
    RATE_LIMITER.check("register_media_asset").map_err(|_| media_rate_limited_error())?;
    let data_dir =
        state.lock_data_dir().clone().ok_or_else(|| media_unavailable_error("MEDIA_STATE_UNAVAILABLE", false))?;
    // Ordinary Library/curation playback needs only imported-file membership. Legacy schema-65 rows
    // with a missing identity remain playable here because this grant can never authorize a review
    // decision: playback_binding rejects unverified grants. Keep the stronger proof path separate.
    let canonical = {
        let db = state.lock_db();
        crate::media::MediaRegistry::ensure_imported(&db, &audio_path).map_err(public_media_error)?
    };
    let registry = Arc::clone(&state.media_registry);
    let materializer = Arc::clone(&state.media_materializer);
    run_blocking(move || materializer.register_unverified(&registry, &data_dir, std::path::PathBuf::from(canonical)))
        .await
        .map_err(public_media_error)
}

/// Mint the immutable, decoded-PCM-verified grant required by the policy-4 review boundary. This is
/// intentionally separate from ordinary media playback so a legacy/null fingerprint cannot break
/// Library listening, while it still fails closed before any human-truth write is possible.
#[tauri::command]
#[specta::specta]
pub async fn register_review_media_asset(
    audio_path: String,
    state: State<'_, AppState>,
) -> Result<crate::media::MediaGrant, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("register_review_media_asset").map_err(|_| media_rate_limited_error())?;
    let data_dir =
        state.lock_data_dir().clone().ok_or_else(|| media_unavailable_error("MEDIA_STATE_UNAVAILABLE", false))?;
    // Capture authority under a short DB lock, then release it before the potentially multi-GB copy
    // and PCM decode. Never acquire registry -> DB: session commands deliberately lease registry
    // state first and release it before entering the database transaction.
    let source = {
        let db = state.lock_db();
        crate::media::MediaRegistry::validate_playback_source(&db, &audio_path).map_err(public_media_error)?
    };
    let registry = Arc::clone(&state.media_registry);
    let materializer = Arc::clone(&state.media_materializer);
    run_blocking(move || materializer.register_verified(&registry, &data_dir, source)).await.map_err(public_media_error)
}

#[tauri::command]
#[specta::specta]
pub fn get_media_asset_url(id: String, state: State<'_, AppState>) -> Result<String, CommandErrorV1> {
    RATE_LIMITER.check("get_media_asset_url").map_err(|_| media_rate_limited_error())?;
    validate::validate_identifier(&id).map_err(|_| media_unavailable_error("INVALID_MEDIA_GRANT", false))?;
    let (result, retired) = {
        let mut registry = state.lock_media_registry();
        let result = registry.refresh_grant(&id);
        (result, registry.take_retired_artifacts())
    };
    crate::media::cleanup_retired_media_artifacts(retired, "expired media grant");
    result.map_err(public_media_error)?;
    crate::media::media_grant_url(&id).map_err(public_media_error)
}

#[cfg(test)]
mod typed_media_ipc_tests {
    use super::*;

    #[test]
    fn media_failures_are_stable_and_scrub_private_backend_details() {
        let hostile =
            public_media_error(r"sqlite D:\private\cortex.db token=secret SELECT * FROM speech_segments".to_string());
        let wire = serde_json::to_string(&hostile).expect("serialize public media error");
        assert!(wire.contains("MEDIA_ASSET_UNAVAILABLE"));
        assert!(wire.contains("reloadClip"));
        for forbidden in ["D:\\", "private", "token", "secret", "SELECT", "speech_segments"] {
            assert!(!wire.contains(forbidden));
        }

        let busy = public_media_error(format!(
            "{}: private internal queue details",
            crate::media::MEDIA_MATERIALIZATION_BUSY_CODE
        ));
        let busy = serde_json::to_value(busy).expect("serialize busy media error");
        assert_eq!(busy["code"], "MEDIA_PREPARATION_BUSY");
        assert_eq!(busy["retryable"], true);
        assert_eq!(busy["suggestedAction"], "retry");
        assert!(!busy.to_string().contains("private internal"));
    }
}

fn fingerprint_count_rate_limited_error() -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", "The duplicate-audio summary is busy. Retry in a moment.", true)
        .suggested(SuggestedActionV1::Retry)
}

#[tauri::command]
#[specta::specta]
pub fn get_fingerprint_count(state: State<'_, AppState>) -> Result<usize, CommandErrorV1> {
    RATE_LIMITER.check("get_fingerprint_count").map_err(|_| fingerprint_count_rate_limited_error())?;
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

#[cfg(test)]
mod typed_fingerprint_ipc_tests {
    use super::*;

    #[test]
    fn fingerprint_rate_limit_is_stable_typed_and_renderer_safe() {
        let error = fingerprint_count_rate_limited_error();
        let wire = serde_json::to_value(error).expect("serialize fingerprint error");
        assert_eq!(wire["schema"], 1);
        assert_eq!(wire["code"], "RATE_LIMITED");
        assert_eq!(wire["retryable"], true);
        assert_eq!(wire["suggestedAction"], "retry");
        assert!(wire.get("sql").is_none());
        assert!(!wire.to_string().contains("C:\\"));
    }
}

#[cfg(test)]
mod typed_session_ipc_tests {
    use super::*;

    #[test]
    fn session_errors_are_stable_bounded_and_renderer_safe() {
        let hostile = format!("token=secret {}", "x".repeat(1_001));
        let error = validate_public_session_state(&hostile, "newest").expect_err("oversized state must refuse");
        let wire = serde_json::to_string(&error).expect("serialize session validation error");
        assert!(wire.contains("INVALID_SESSION_STATE"));
        assert!(!wire.contains("secret"));
        assert!(!wire.contains("token"));

        let storage = public_session_error(
            "SESSION_SAVE_FAILED",
            "The workspace view could not be saved. Open Health for recovery options.",
            false,
        );
        let storage_wire = serde_json::to_string(&storage).unwrap();
        assert!(!storage_wire.contains("C:\\"));
        assert!(!storage_wire.contains("SQL"));
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_tracing_stats(_state: State<'_, AppState>) -> Result<crate::ipc_contract::TracingStatsV1, CommandErrorV1> {
    RATE_LIMITER
        .check("get_tracing_stats")
        .map_err(|_| diagnostics_rate_limited_error("The diagnostics summary is busy. Retry in a moment."))?;
    Ok(crate::telemetry::TRACER.stats().into())
}

#[tauri::command]
#[specta::specta]
pub fn get_recent_spans(count: Option<usize>) -> Result<Vec<crate::ipc_contract::TracingSpanV1>, CommandErrorV1> {
    RATE_LIMITER
        .check("get_recent_spans")
        .map_err(|_| diagnostics_rate_limited_error("The diagnostics history is busy. Retry in a moment."))?;
    let count = public_recent_span_limit(count);
    Ok(renderer_safe_spans(crate::telemetry::TRACER.get_recent_limited(count)))
}

#[tauri::command]
#[specta::specta]
pub fn clear_tracing_spans() -> Result<(), CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("clear_tracing_spans")
        .map_err(|_| diagnostics_rate_limited_error("The diagnostics clear action is busy. Retry in a moment."))?;
    crate::telemetry::TRACER.clear();
    Ok(())
}

#[cfg(test)]
mod typed_diagnostics_ipc_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn public_spans_drop_raw_error_metadata_and_enforce_the_count_ceiling() {
        let hostile = crate::telemetry::Span {
            operation: "diagnostic.test",
            start: "2026-08-27T00:00:00Z".to_string(),
            duration_ms: 12.5,
            metadata: HashMap::from([("path".to_string(), r"X:\private\owner.wav".to_string())]),
            success: false,
            error: Some("token=secret SQL SELECT transcript".to_string()),
        };
        let spans = renderer_safe_spans(vec![hostile; public_recent_span_limit(Some(usize::MAX))]);
        assert_eq!(spans.len(), MAX_PUBLIC_RECENT_SPANS);
        let wire = serde_json::to_string(&spans).expect("serialize public diagnostics");
        assert!(wire.contains("diagnostic.test"));
        assert!(!wire.contains("private"));
        assert!(!wire.contains("secret"));
        assert!(!wire.contains("SELECT"));
        assert!(!wire.contains("metadata"));
        assert!(!wire.contains("error"));
    }
}

/// Persist the user's view-state (search query + sort order) so it survives a restart. The values
/// are held in the session manager so the periodic counts-only auto_save preserves them too.
#[tauri::command]
#[specta::specta]
pub fn save_session(
    search_query: String,
    sort_order: String,
    filter_verified: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), CommandErrorV1> {
    // Throttle like every other infra.rs command: save_session is a webview-reachable DB write taken
    // under the GLOBAL db lock, and it was the lone one here without a limiter. The frontend debounces
    // it to ~1/800ms, so this never rejects a legitimate save — it only stops a webview loop that
    // bypasses the debounce from pinning the db lock and starving get_segments et al. (same class as
    // export_audio round-22 #5 / register_media_asset round-25 #7).
    RATE_LIMITER
        .check("save_session")
        .map_err(|_| public_session_error("RATE_LIMITED", "Session saving is busy. Retry in a moment.", true))?;
    validate_public_session_state(&search_query, &sort_order)?;
    state.save_session_view_state(search_query, sort_order, filter_verified).map_err(|error| {
        if matches!(
            &error,
            crate::error::AppError::Other(message)
                if message == crate::database_runtime::RESTORE_IN_PROGRESS_MSG
        ) {
            return public_session_error(
                "RESTORE_IN_PROGRESS",
                "Session saving is paused while database recovery is active. Retry after recovery finishes.",
                true,
            );
        }
        public_session_error(
            "SESSION_SAVE_FAILED",
            "The workspace view could not be saved. Open Health for recovery options.",
            false,
        )
    })
}

#[tauri::command]
#[specta::specta]
pub fn restore_session(
    state: State<'_, AppState>,
) -> Result<Option<crate::ipc_contract::SessionStateV1>, CommandErrorV1> {
    RATE_LIMITER
        .check("restore_session")
        .map_err(|_| public_session_error("RATE_LIMITED", "Session recovery is busy. Retry in a moment.", true))?;
    let mut session = state.lock_session();
    session.restore().map(|state| state.map(Into::into)).map_err(|_| {
        public_session_error(
            "SESSION_RESTORE_FAILED",
            "The previous workspace view could not be restored. Open Health for recovery options.",
            false,
        )
    })
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
