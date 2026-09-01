//! Segment + audio read/retrieval IPC commands — slice 8 of the Week-4 `commands.rs` decomposition.
//!
//! Public command names remain stable: `commands.rs` re-exports this module (`pub use
//! segments_read::*;`), so `lib.rs`'s invoke handler keeps the same registration surface. Library
//! page/id/anomaly reads additionally use generated typed contracts and renderer-safe errors.
//!
//! These are the whole-library reads / unbounded FTS search / audio-health scan / waveform + duration
//! probes — all `async` + `run_blocking` so a large library never freezes the UI thread.

use super::{run_blocking, send_audio_duration_probe_result, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::db::{SegmentsPage, SpeechSegment};
use crate::validation::input as validate;
use crate::{audio, AppState};
use std::path::Path;
use std::time::Duration;
use tauri::State;

fn public_review_read_error(error: &str) -> crate::ipc_contract::CommandErrorV1 {
    if error.to_ascii_lowercase().contains("database is locked")
        || error.to_ascii_lowercase().contains("database is busy")
    {
        crate::ipc_contract::CommandErrorV1::new(
            "DATABASE_BUSY",
            "The workspace is busy. Retry loading the review queue.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    } else {
        crate::ipc_contract::CommandErrorV1::new("REVIEW_PAGE_FAILED", "The review queue could not be loaded.", false)
            .suggested(crate::ipc_contract::SuggestedActionV1::OpenHealth)
    }
}

fn public_library_read_error(error: &str) -> crate::ipc_contract::CommandErrorV1 {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("database is locked") || normalized.contains("database is busy") {
        crate::ipc_contract::CommandErrorV1::new(
            "DATABASE_BUSY",
            "The workspace is busy. Retry loading the library.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    } else {
        crate::ipc_contract::CommandErrorV1::new(
            "LIBRARY_READ_FAILED",
            "The library could not be read. Open Health for recovery options.",
            false,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::OpenHealth)
    }
}

fn library_rate_limited_error() -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new("RATE_LIMITED", "Too many library requests. Retry in a moment.", true)
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
}

fn library_worker_error() -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(
        "LIBRARY_READ_FAILED",
        "The library worker stopped unexpectedly. Retry the request.",
        true,
    )
    .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
}

fn invalid_library_request(code: &str, message: &str) -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(code, message, false)
}

fn public_library_sort(sort: Option<String>) -> Result<String, crate::ipc_contract::CommandErrorV1> {
    let sort = sort.unwrap_or_else(|| "newest".to_string());
    validate::validate_text(&sort, 64, "Segment sort")
        .map_err(|_| invalid_library_request("INVALID_LIBRARY_SORT", "The selected library sort is invalid."))?;
    match sort.as_str() {
        "newest" | "oldest" | "duration" | "verified" | "confidence" | "activeLearning" | "active_learning"
        | "suspectFirst" | "suspect_first" => Ok(sort),
        _ => Err(invalid_library_request("INVALID_LIBRARY_SORT", "The selected library sort is invalid.")),
    }
}

fn validate_library_cursor(cursor: Option<&str>) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    let Some(cursor) = cursor else {
        return Ok(());
    };
    validate::validate_text(cursor, 2048, "Segment page cursor").map_err(|_| {
        invalid_library_request("INVALID_LIBRARY_CURSOR", "The library cursor is invalid. Reload the library.")
    })?;
    if cursor.chars().all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_') {
        Ok(())
    } else {
        Err(invalid_library_request("INVALID_LIBRARY_CURSOR", "The library cursor is invalid. Reload the library."))
    }
}

fn public_library_page_limit(limit: Option<i64>) -> usize {
    limit.unwrap_or(200).clamp(1, 500) as usize
}

fn public_anomaly_limit(limit: Option<i64>) -> usize {
    limit.unwrap_or(100).clamp(1, 500) as usize
}

fn stale_library_focus_error() -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(
        "STALE_VOICE_FOCUS",
        "The voice-focus policy changed while the page was loading. Reload the review workspace.",
        false,
    )
    .suggested(crate::ipc_contract::SuggestedActionV1::ReloadClip)
}

fn ensure_library_focus_unchanged(
    before: Option<&crate::voice_focus::VoiceFocusBinding>,
    current: Option<&crate::voice_focus::VoiceFocusBinding>,
) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    let before_id = before.map(|binding| binding.focus_id.as_str());
    let current_id = current.map(|binding| binding.focus_id.as_str());
    if current_id == before_id {
        Ok(())
    } else {
        Err(stale_library_focus_error())
    }
}

fn voice_focus_policy_error() -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(
        "VOICE_FOCUS_POLICY_INVALID",
        "The active voice-focus policy cannot be read. Open Health before loading focused review work.",
        false,
    )
    .suggested(crate::ipc_contract::SuggestedActionV1::OpenHealth)
}

fn require_active_voice_focus(
    data_dir: Option<&Path>,
    expected_focus_id: &str,
) -> Result<crate::voice_focus::VoiceFocusBinding, crate::ipc_contract::CommandErrorV1> {
    let binding = crate::voice_focus::resolve_binding(data_dir).map_err(|_| voice_focus_policy_error())?;
    let Some(binding) = binding else {
        return Err(crate::ipc_contract::CommandErrorV1::new(
            "VOICE_FOCUS_NOT_ACTIVE",
            "No voice-focus policy is active. Reload the review workspace.",
            false,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::ReloadClip));
    };
    if binding.focus_id != expected_focus_id {
        return Err(crate::ipc_contract::CommandErrorV1::new(
            "STALE_VOICE_FOCUS",
            "The voice-focus policy changed. Reload the review workspace before continuing.",
            false,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::ReloadClip));
    }
    Ok(binding)
}

/// Discover only the opaque identity and cardinality of the active focus. The private policy name,
/// individual segment ids and owner data-dir path never cross IPC.
#[tauri::command]
#[specta::specta]
pub async fn get_active_voice_focus_v1(
    state: State<'_, AppState>,
) -> Result<Option<crate::ipc_contract::ActiveVoiceFocusV1>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_active_voice_focus_v1").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "RATE_LIMITED",
            "Too many voice-focus requests. Retry in a moment.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?;
    let data_dir = state.lock_data_dir().clone();
    tokio::task::spawn_blocking(move || {
        let binding =
            crate::voice_focus::resolve_binding(data_dir.as_deref()).map_err(|_| voice_focus_policy_error())?;
        binding
            .map(|binding| {
                let segment_count = i64::try_from(binding.segment_ids.len()).map_err(|_| voice_focus_policy_error())?;
                Ok(crate::ipc_contract::ActiveVoiceFocusV1 { focus_id: binding.focus_id, segment_count })
            })
            .transpose()
    })
    .await
    .map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "VOICE_FOCUS_READ_FAILED",
            "The voice-focus worker stopped unexpectedly.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?
}

/// Versioned review queue read. Each rendered row and `baseRevision` originate in one SQLite result
/// row, so a concurrent writer cannot pair old text with a newer compare-and-swap token. The read and
/// DTO hydration stay off the desktop main thread even for a live-sized library.
#[tauri::command]
#[specta::specta]
pub async fn get_review_page_v1(
    scope: crate::ipc_contract::ReviewScope,
    limit: Option<usize>,
    cursor: Option<String>,
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::ReviewPageV1, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_review_page_v1").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "RATE_LIMITED",
            "Too many review queue requests. Retry in a moment.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?;
    let (query, scope_label, escalation_only, expected_focus_id) = match scope {
        crate::ipc_contract::ReviewScope::Pending => (None, "pending".to_string(), false, None),
        crate::ipc_contract::ReviewScope::Search { query } => {
            validate::validate_text(&query, 1000, "Search query").map_err(|_| {
                crate::ipc_contract::CommandErrorV1::new(
                    "INVALID_REVIEW_SCOPE",
                    "The review search is invalid or too long.",
                    false,
                )
            })?;
            (Some(query), "search".to_string(), false, None)
        }
        crate::ipc_contract::ReviewScope::Escalation => (None, "escalation".to_string(), true, None),
        crate::ipc_contract::ReviewScope::VoiceFocus { focus_id } => {
            if !crate::voice_focus::is_opaque_focus_id(&focus_id) {
                return Err(crate::ipc_contract::CommandErrorV1::new(
                    "INVALID_REVIEW_SCOPE",
                    "The voice-focus identity is invalid. Reload the review workspace.",
                    false,
                )
                .suggested(crate::ipc_contract::SuggestedActionV1::ReloadClip));
            }
            (None, "voiceFocus".to_string(), false, Some(focus_id))
        }
    };
    if let Some(cursor) = cursor.as_deref() {
        validate::validate_text(cursor, 2048, "Review page cursor").map_err(|_| {
            crate::ipc_contract::CommandErrorV1::new(
                "INVALID_REVIEW_CURSOR",
                "The review cursor is invalid. Reload the queue.",
                false,
            )
            .suggested(crate::ipc_contract::SuggestedActionV1::ReloadClip)
        })?;
        if !cursor.chars().all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_') {
            return Err(crate::ipc_contract::CommandErrorV1::new(
                "INVALID_REVIEW_CURSOR",
                "The review cursor is invalid. Reload the queue.",
                false,
            )
            .suggested(crate::ipc_contract::SuggestedActionV1::ReloadClip));
        }
    }
    let data_dir = state.lock_data_dir().clone();
    let focus = if let Some(expected_focus_id) = expected_focus_id.as_deref() {
        Some(require_active_voice_focus(data_dir.as_deref(), expected_focus_id)?.segment_ids)
    } else {
        crate::voice_focus::resolve(data_dir.as_deref()).map_err(|error| public_review_read_error(&error))?
    };
    let segment_queries = state.segment_queries();
    let limit = limit.unwrap_or(100).clamp(1, 200);
    tokio::task::spawn_blocking(move || {
        let page = if escalation_only {
            segment_queries.get_escalation_review_page(limit, cursor.as_deref(), focus.as_deref())
        } else {
            segment_queries.get_segments_page(
                Some(false),
                query.as_deref(),
                "oldest",
                limit,
                cursor.as_deref(),
                focus.as_deref(),
            )
        }
        .map_err(|error| public_review_read_error(&error.to_string()))?;
        // A bound page must still describe the active file policy when it leaves the worker. This
        // second read closes the material query window for atomic policy replacements; a stale id is
        // reported loudly instead of returning rows from a retired focus generation.
        if let Some(expected_focus_id) = expected_focus_id.as_deref() {
            require_active_voice_focus(data_dir.as_deref(), expected_focus_id)?;
        }
        let total = i64::try_from(page.total).map_err(|_| {
            crate::ipc_contract::CommandErrorV1::new(
                "REVIEW_PAGE_FAILED",
                "The review queue count is out of range.",
                false,
            )
        })?;
        let items = page
            .items
            .into_iter()
            .map(|segment| {
                let base_revision = page.revisions.get(&segment.id).copied().unwrap_or(-1);
                let eligible =
                    base_revision >= 0 && !crate::quality::is_placeholder_transcript(&segment.raw_transcript);
                crate::ipc_contract::ReviewItemV1 {
                    segment,
                    base_revision,
                    eligible,
                    disabled_reason: (!eligible).then(|| "TRANSCRIPT_NOT_READY".to_string()),
                }
            })
            .collect();
        Ok(crate::ipc_contract::ReviewPageV1 {
            items,
            total,
            next_cursor: page.next_cursor,
            scope_label,
            focus_narrowed: page.focus_narrowed,
        })
    })
    .await
    .map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "REVIEW_PAGE_FAILED",
            "The review queue worker stopped unexpectedly.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?
}

/// Hydrate one selected list row with its full alignment/evidence payload. The database read runs
/// off the Tauri main thread and every refusal is a stable renderer-safe code.
#[tauri::command]
#[specta::specta]
pub async fn get_segment(
    segment_id: String,
    state: State<'_, AppState>,
) -> Result<SpeechSegment, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_segment").map_err(|_| library_rate_limited_error())?;
    validate::validate_identifier(&segment_id)
        .map_err(|_| invalid_library_request("INVALID_SEGMENT_ID", "The selected segment identity is invalid."))?;
    let segment_queries = state.segment_queries();
    tokio::task::spawn_blocking(move || {
        segment_queries
            .get_segment(&segment_id)
            .map_err(|error| public_library_read_error(&error.to_string()))?
            .ok_or_else(|| {
                crate::ipc_contract::CommandErrorV1::new(
                    "SEGMENT_NOT_FOUND",
                    "The selected segment no longer exists. Reload the library.",
                    false,
                )
                .suggested(crate::ipc_contract::SuggestedActionV1::ReloadClip)
            })
    })
    .await
    .map_err(|_| library_worker_error())?
}

/// Read one stable keyset page from the owner library. Voice-focus resolution and SQLite work stay
/// in the blocking worker; malformed policy files fail closed without exposing their private path.
#[tauri::command]
#[specta::specta]
pub async fn get_segments_page(
    verified: Option<bool>,
    query: Option<String>,
    sort: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
    focused: Option<bool>,
    state: State<'_, AppState>,
) -> Result<SegmentsPage, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_segments_page").map_err(|_| library_rate_limited_error())?;
    if let Some(query) = query.as_deref() {
        validate::validate_text(query, 1000, "Search query").map_err(|_| {
            invalid_library_request("INVALID_LIBRARY_QUERY", "The library search is invalid or too long.")
        })?;
    }
    let sort = public_library_sort(sort)?;
    validate_library_cursor(cursor.as_deref())?;
    let limit = public_library_page_limit(limit);
    let data_dir = state.lock_data_dir().clone();
    let segment_queries = state.segment_queries();
    let focused = focused.unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        // A missing focus file is unrestricted; a present-but-invalid file fails closed. Only the
        // review queue opts into this scope. Library/curation reads remain corpus-wide.
        let focus_binding = if focused {
            crate::voice_focus::resolve_binding(data_dir.as_deref()).map_err(|_| voice_focus_policy_error())?
        } else {
            None
        };
        let page = segment_queries
            .get_segments_page(
                verified,
                query.as_deref(),
                &sort,
                limit,
                cursor.as_deref(),
                focus_binding.as_ref().map(|binding| binding.segment_ids.as_ref()),
            )
            .map_err(|error| public_library_read_error(&error.to_string()))?;
        if focused {
            let current =
                crate::voice_focus::resolve_binding(data_dir.as_deref()).map_err(|_| voice_focus_policy_error())?;
            ensure_library_focus_unchanged(focus_binding.as_ref(), current.as_ref())?;
        }
        Ok(page)
    })
    .await
    .map_err(|_| library_worker_error())?
}

/// Read the exact id set for a contextual batch action without hydrating every segment row.
#[tauri::command]
#[specta::specta]
pub async fn get_segment_ids_for_view(
    verified: Option<bool>,
    query: Option<String>,
    transcript_state: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<String>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_segment_ids_for_view").map_err(|_| library_rate_limited_error())?;
    if let Some(query) = query.as_deref() {
        validate::validate_text(query, 1000, "Search query").map_err(|_| {
            invalid_library_request("INVALID_LIBRARY_QUERY", "The library search is invalid or too long.")
        })?;
    }
    let transcript_state = transcript_state.unwrap_or_else(|| "any".to_string());
    if !matches!(transcript_state.as_str(), "any" | "real" | "missing") {
        return Err(invalid_library_request("INVALID_TRANSCRIPT_STATE", "The selected transcript filter is invalid."));
    }
    let segment_queries = state.segment_queries();
    tokio::task::spawn_blocking(move || {
        segment_queries
            .get_segment_ids_for_view(verified, query.as_deref(), &transcript_state)
            .map_err(|error| public_library_read_error(&error.to_string()))
    })
    .await
    .map_err(|_| library_worker_error())?
}

/// Read the highest anomaly scores using a bounded public page. An untrusted renderer cannot turn
/// this diagnostics view into an unbounded library hydration.
#[tauri::command]
#[specta::specta]
pub async fn get_signal_anomaly_segments(
    limit: Option<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<SpeechSegment>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_signal_anomaly_segments").map_err(|_| library_rate_limited_error())?;
    let limit = public_anomaly_limit(limit);
    let segment_queries = state.segment_queries();
    tokio::task::spawn_blocking(move || {
        segment_queries
            .get_signal_anomaly_segments(limit)
            .map_err(|error| public_library_read_error(&error.to_string()))
    })
    .await
    .map_err(|_| library_worker_error())?
}

#[cfg(test)]
mod voice_focus_scope_tests {
    use super::*;

    fn write_focus(dir: &Path, ids: &[&str]) {
        std::fs::write(
            dir.join(crate::voice_focus::VOICE_FOCUS_FILE),
            serde_json::to_vec(&serde_json::json!({
                "name": "private owner label",
                "segment_ids": ids,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn exact_active_focus_identity_is_accepted_without_exposing_private_policy_data() {
        let dir = tempfile::tempdir().unwrap();
        write_focus(dir.path(), &["segment-b", "segment-a"]);
        let discovered = crate::voice_focus::resolve_binding(Some(dir.path())).unwrap().unwrap();
        let accepted = require_active_voice_focus(Some(dir.path()), &discovered.focus_id).unwrap();

        assert_eq!(accepted.segment_ids.len(), 2);
        assert!(accepted.segment_ids.contains("segment-a"));
        assert!(!accepted.focus_id.contains("private owner label"));
        assert!(!accepted.focus_id.contains(dir.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn changed_missing_and_broken_focus_policies_fail_closed_with_public_codes() {
        let dir = tempfile::tempdir().unwrap();
        write_focus(dir.path(), &["segment-old"]);
        let stale = crate::voice_focus::resolve_binding(Some(dir.path())).unwrap().unwrap().focus_id;

        write_focus(dir.path(), &["segment-new"]);
        assert_eq!(require_active_voice_focus(Some(dir.path()), &stale).unwrap_err().code, "STALE_VOICE_FOCUS");

        std::fs::remove_file(dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE)).unwrap();
        assert_eq!(require_active_voice_focus(Some(dir.path()), &stale).unwrap_err().code, "VOICE_FOCUS_NOT_ACTIVE");

        std::fs::write(dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE), b"{ broken").unwrap();
        let error = require_active_voice_focus(Some(dir.path()), &stale).unwrap_err();
        assert_eq!(error.code, "VOICE_FOCUS_POLICY_INVALID");
        assert!(!error.message.contains(dir.path().to_string_lossy().as_ref()));
        assert!(!error.message.contains(crate::voice_focus::VOICE_FOCUS_FILE));
    }
}

#[cfg(test)]
mod library_read_contract_tests {
    use super::*;

    #[test]
    fn library_errors_are_typed_retry_aware_and_scrub_internal_text() {
        let busy = public_library_read_error("database is locked at X:\\private\\owner.db");
        assert_eq!(busy.code, "DATABASE_BUSY");
        assert!(busy.retryable);

        let failed = public_library_read_error("token=secret SQL SELECT annotated_transcript FROM speech_segments");
        assert_eq!(failed.code, "LIBRARY_READ_FAILED");
        assert!(!failed.retryable);
        let wire = serde_json::to_string(&failed).expect("serialize public library error");
        assert!(!wire.contains("secret"));
        assert!(!wire.contains("SELECT"));
        assert!(!wire.contains("annotated_transcript"));
        assert!(!wire.contains("speech_segments"));
    }

    #[test]
    fn public_library_bounds_and_filters_fail_closed() {
        assert_eq!(public_library_page_limit(None), 200);
        assert_eq!(public_library_page_limit(Some(0)), 1);
        assert_eq!(public_library_page_limit(Some(-10)), 1);
        assert_eq!(public_library_page_limit(Some(i64::MAX)), 500);
        assert_eq!(public_anomaly_limit(None), 100);
        assert_eq!(public_anomaly_limit(Some(-10)), 1);
        assert_eq!(public_anomaly_limit(Some(i64::MAX)), 500);

        assert_eq!(public_library_sort(Some("oldest".to_string())).unwrap(), "oldest");
        assert_eq!(public_library_sort(Some("DROP TABLE".to_string())).unwrap_err().code, "INVALID_LIBRARY_SORT");
        assert!(validate_library_cursor(Some("opaque_ABC-123")).is_ok());
        assert_eq!(validate_library_cursor(Some("../private.db")).unwrap_err().code, "INVALID_LIBRARY_CURSOR");
    }

    #[test]
    fn a_replaced_or_newly_activated_focus_invalidates_the_page_generation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE),
            br#"{"name":"private one","segment_ids":["segment-one"]}"#,
        )
        .unwrap();
        let before = crate::voice_focus::resolve_binding(Some(dir.path())).unwrap().unwrap();

        std::fs::write(
            dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE),
            br#"{"name":"private two","segment_ids":["segment-two"]}"#,
        )
        .unwrap();
        let current = crate::voice_focus::resolve_binding(Some(dir.path())).unwrap().unwrap();
        assert_eq!(
            ensure_library_focus_unchanged(Some(&before), Some(&current)).unwrap_err().code,
            "STALE_VOICE_FOCUS"
        );
        assert_eq!(ensure_library_focus_unchanged(None, Some(&current)).unwrap_err().code, "STALE_VOICE_FOCUS");
    }
}

#[tauri::command]
pub async fn get_segments(verified: Option<bool>, state: State<'_, AppState>) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_segments")?;
    let segment_queries = state.segment_queries();
    run_blocking(move || segment_queries.get_segments(verified).map_err(|e| e.to_string())).await
}

/// M2.5: Return segments ordered by suspect-first priority: escalated + low confidence first.
/// Priority: 1) Jury escalated, 2) Low agent confidence, 3) Chronological.
#[tauri::command]
pub async fn get_segments_suspect_first(
    verified: Option<bool>,
    state: State<'_, AppState>,
) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_segments_suspect_first")?;
    let segment_queries = state.segment_queries();
    run_blocking(move || segment_queries.get_segments_suspect_first(verified).map_err(|e| e.to_string())).await
}

#[tauri::command]
pub async fn search_segments(query: String, state: State<'_, AppState>) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("search_segments")?;
    // Bound the free-text query like every other text-accepting command (save_session caps its
    // search_query at 1000): an unbounded multi-MB string otherwise reaches the FTS5 MATCH parser.
    validate::validate_text(&query, 1000, "Search query")?;
    // Off the main thread: the FTS5 MATCH has no LIMIT, so a common token materializes + serializes a
    // large slice of the library. Run it on the blocking pool exactly like the get_segments siblings so
    // a keystroke in the search box can't freeze the UI. The bounded query store is moved into the task;
    // no database guard is held across the await.
    let segment_queries = state.segment_queries();
    run_blocking(move || segment_queries.search_segments(&query).map_err(|e| e.to_string())).await
}

#[tauri::command]
pub async fn get_audio_duration(path: String) -> Result<i64, String> {
    RATE_LIMITER.check("get_audio_duration")?;
    let validated = validate::validate_file_path(&path)?;
    // The whole watchdog (probe thread + 30s recv_timeout that bounds a pathological decode) runs on
    // the blocking pool, so the `recv_timeout` blocks a spawn_blocking thread instead of the UI thread.
    // Behavior-preserving: same probe thread, same 30s bound, same four outcomes — only the thread the
    // caller waits on changes (main -> blocking pool).
    run_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = audio::get_duration_ms(&validated);
            send_audio_duration_probe_result(tx, result);
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(dur)) => Ok(dur),
            Ok(Err(e)) => Err(e.to_string()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err("Audio duration probe timed out after 30s".to_string())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("Audio duration probe thread disconnected".to_string())
            }
        }
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_waveform(
    path: String,
    num_points: usize,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<f32>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_waveform").map_err(|_| crate::ipc_contract::owner_critical_rate_limited("get_waveform"))?;
    let validated = validate::validate_file_path(&path).map_err(|_| crate::ipc_contract::invalid_audio_path_error())?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj).map_err(|_| crate::ipc_contract::invalid_alignment_error())?;
    }
    // Clone the pipeline out of the global lock before the (up to 30 s) decode so a waveform
    // render never starves other pipeline-lock users (matches import_audio_file / rediarize /
    // run_gold_eval_asr, which all clone for the same reason), then decode OFF the main thread.
    let pipeline = state.lock_pipeline().clone();
    let result = run_blocking(move || {
        pipeline.get_waveform(&validated, num_points, alignment_json.as_deref()).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner waveform command failed: {error}");
        crate::ipc_contract::public_waveform_error(&error)
    })
}

/// P3.3: report which distinct source audio files are missing on disk (moved/renamed/deleted).
#[tauri::command]
#[specta::specta]
pub async fn get_audio_health(
    state: State<'_, AppState>,
) -> Result<crate::db::AudioHealth, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER
        .check("get_audio_health")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("get_audio_health"))?;
    let segment_queries = state.segment_queries();
    let result = run_blocking(move || segment_queries.audio_health().map_err(|e| e.to_string())).await;
    result.map_err(|error| {
        tracing::warn!("Owner audio-health command failed: {error}");
        crate::ipc_contract::public_owner_data_error(crate::ipc_contract::OwnerDataOperationV1::AudioHealth, &error)
    })
}

/// P3.3: relink missing source audio by basename against a folder the owner picks.
#[tauri::command]
#[specta::specta]
pub async fn relink_audio(
    search_dir: String,
    state: State<'_, AppState>,
) -> Result<crate::db::RelinkResult, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("relink_audio")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("relink_audio"))?;
    // P1.1: reject a UNC search dir BEFORE db.relink_audio probes `search_dir.join(name).is_file()` — a
    // renderer-supplied `\\attacker\share` would otherwise drive the SMB redirector (NTLM forced-auth
    // leak) on the is_file() stat and then PERSIST the UNC path into the row. Syntactic guard, zero I/O
    // (no canonicalize: the picked dir is searched as-is); also subsumes the prior null-byte check.
    validate::reject_unc_path(&search_dir).map_err(|_| crate::ipc_contract::invalid_relink_path_error())?;
    let database = state.db_runtime();
    let result = run_blocking(move || {
        let mutation = database.begin_mutation().map_err(|error| error.to_string())?;
        let db = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
        db.relink_audio(std::path::Path::new(&search_dir)).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner audio-relink command failed: {error}");
        crate::ipc_contract::public_owner_data_error(crate::ipc_contract::OwnerDataOperationV1::RelinkAudio, &error)
    })
}

#[tauri::command]
#[specta::specta]
pub async fn get_active_learning_queue(
    state: State<'_, AppState>,
    target_error: f64,
    confidence_level: f64,
    limit: usize,
) -> Result<Vec<SpeechSegment>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER
        .check("get_active_learning_queue")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("get_active_learning_queue"))?;
    let segment_queries = state.segment_queries();
    let result = run_blocking(move || {
        segment_queries.active_learning_queue(target_error, confidence_level, limit).map_err(Into::into)
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner active-learning queue command failed: {error}");
        crate::ipc_contract::public_owner_analysis_error(
            crate::ipc_contract::OwnerAnalysisOperationV1::ActiveLearningQueue,
            &error,
        )
    })
}

#[cfg(test)]
mod read_command_surface_tests {
    use super::*;

    #[test]
    fn review_read_errors_are_typed_retry_aware_and_scrub_internal_text() {
        let busy = public_review_read_error("Database Is LOCKED at X:\\private\\owner.db");
        assert_eq!(busy.code, "DATABASE_BUSY");
        assert!(busy.retryable);
        assert_eq!(busy.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::Retry));
        assert_eq!(public_review_read_error("database is busy").code, "DATABASE_BUSY");

        let failed = public_review_read_error("SQL SELECT annotated_transcript FROM speech_segments");
        assert_eq!(failed.code, "REVIEW_PAGE_FAILED");
        assert!(!failed.retryable);
        assert_eq!(failed.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::OpenHealth));
        let wire = serde_json::to_string(&failed).expect("serialize public review error");
        assert!(!wire.contains("SELECT"));
        assert!(!wire.contains("speech_segments"));
        assert!(!wire.contains("annotated_transcript"));
    }

    #[test]
    fn library_worker_and_rate_limit_refusals_are_retryable_typed_codes() {
        let limited = library_rate_limited_error();
        assert_eq!(limited.code, "RATE_LIMITED");
        assert!(limited.retryable);
        assert_eq!(limited.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::Retry));

        let worker = library_worker_error();
        assert_eq!(worker.code, "LIBRARY_READ_FAILED");
        assert!(worker.retryable);
        assert_eq!(worker.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::Retry));
    }

    #[test]
    fn every_supported_library_sort_is_accepted_and_the_default_is_newest() {
        assert_eq!(public_library_sort(None).unwrap(), "newest");
        for sort in [
            "newest",
            "oldest",
            "duration",
            "verified",
            "confidence",
            "activeLearning",
            "active_learning",
            "suspectFirst",
            "suspect_first",
        ] {
            assert_eq!(public_library_sort(Some(sort.to_string())).unwrap(), sort);
        }
        // An oversize value takes the length-validation arm, not the unknown-word arm.
        assert_eq!(public_library_sort(Some("n".repeat(65))).unwrap_err().code, "INVALID_LIBRARY_SORT");
    }

    #[test]
    fn absent_empty_and_oversized_cursors_take_their_own_validation_arms() {
        assert!(validate_library_cursor(None).is_ok());
        assert!(validate_library_cursor(Some("")).is_ok());
        let oversized = "a".repeat(2049);
        assert_eq!(validate_library_cursor(Some(&oversized)).unwrap_err().code, "INVALID_LIBRARY_CURSOR");
    }

    #[test]
    fn an_unchanged_focus_generation_is_accepted_and_deactivation_is_stale() {
        assert!(ensure_library_focus_unchanged(None, None).is_ok());

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE),
            br#"{"name":"private label","segment_ids":["segment-a"]}"#,
        )
        .unwrap();
        let before = crate::voice_focus::resolve_binding(Some(dir.path())).unwrap().unwrap();
        let current = crate::voice_focus::resolve_binding(Some(dir.path())).unwrap().unwrap();
        assert!(ensure_library_focus_unchanged(Some(&before), Some(&current)).is_ok());
        // A focus that disappears mid-read must invalidate the page, exactly like a replaced one.
        assert_eq!(ensure_library_focus_unchanged(Some(&before), None).unwrap_err().code, "STALE_VOICE_FOCUS");
    }

    #[test]
    fn audio_duration_probe_measures_real_audio_and_refuses_bad_paths() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("one-second.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        for sample in 0..16_000 {
            writer.write_sample(((sample % 127) as i16) - 63).unwrap();
        }
        writer.finalize().unwrap();

        let garbage = dir.path().join("not-audio.wav");
        std::fs::write(&garbage, b"this is not a RIFF container").unwrap();

        tauri::async_runtime::block_on(async {
            // 16 000 frames at 16 kHz is exactly one second.
            assert_eq!(get_audio_duration(wav.to_string_lossy().into_owned()).await.unwrap(), 1_000);

            let missing = get_audio_duration(dir.path().join("missing.wav").to_string_lossy().into_owned()).await;
            assert!(missing.unwrap_err().contains("Invalid path"));

            // An existing non-audio file passes path validation and must fail in the decode probe.
            assert!(get_audio_duration(garbage.to_string_lossy().into_owned()).await.is_err());
        });
    }
}

/// Wave-4 state-boundary coverage: every read command above invoked through a genuine managed
/// `State<'_, AppState>` (`crate::test_support::managed_app_state`), driving the same
/// limiter/validation/worker closures production IPC runs.
#[cfg(test)]
mod state_read_command_surface_tests {
    use super::*;
    use crate::ipc_contract::ReviewScope;
    use crate::test_support::managed_app_state;
    use tauri::Manager;

    type MockApp = tauri::App<tauri::test::MockRuntime>;

    fn seed_segment(app: &MockApp, id: &str, transcript: &str) {
        app.state::<AppState>()
            .lock_db()
            .insert_segment(&SpeechSegment {
                id: id.into(),
                audio_path: format!("C:/fixtures/{id}.wav"),
                raw_transcript: transcript.into(),
                duration_ms: 1_000,
                ..SpeechSegment::default()
            })
            .unwrap();
    }

    #[test]
    fn review_page_scopes_and_voice_focus_serve_and_refuse_through_state() {
        let dir = tempfile::tempdir().unwrap();
        let app = managed_app_state(dir.path());
        seed_segment(&app, "review-a", "deng yek du");
        seed_segment(&app, "review-b", "wusha ciya jida");

        tauri::async_runtime::block_on(async {
            assert!(
                get_active_voice_focus_v1(app.state()).await.expect("no focus file yet").is_none(),
                "a missing focus policy is honestly no focus, not an error"
            );

            let pending =
                get_review_page_v1(ReviewScope::Pending, None, None, app.state()).await.expect("pending review page");
            assert_eq!(pending.total, 2);
            assert_eq!(pending.items.len(), 2);
            assert_eq!(pending.scope_label, "pending");
            assert!(!pending.focus_narrowed);
            assert!(pending.items.iter().all(|item| item.eligible && item.base_revision == 0));

            let oversized =
                get_review_page_v1(ReviewScope::Search { query: "q".repeat(1001) }, None, None, app.state())
                    .await
                    .expect_err("an oversized search scope must be refused");
            assert_eq!(oversized.code, "INVALID_REVIEW_SCOPE");

            let searched = get_review_page_v1(ReviewScope::Search { query: "deng".into() }, None, None, app.state())
                .await
                .expect("search scope");
            assert_eq!(searched.scope_label, "search");
            assert_eq!(searched.items.len(), 1);
            assert_eq!(searched.items[0].segment.id, "review-a");

            for bad_cursor in ["../evil".to_string(), "c".repeat(2049)] {
                let refused = get_review_page_v1(ReviewScope::Pending, None, Some(bad_cursor), app.state())
                    .await
                    .expect_err("cursor validation");
                assert_eq!(refused.code, "INVALID_REVIEW_CURSOR");
            }

            let escalation =
                get_review_page_v1(ReviewScope::Escalation, None, None, app.state()).await.expect("escalation scope");
            assert_eq!(escalation.scope_label, "escalation");
            assert_eq!(escalation.total, 0);

            let opaque = get_review_page_v1(
                ReviewScope::VoiceFocus { focus_id: "not-an-opaque-digest".into() },
                None,
                None,
                app.state(),
            )
            .await
            .expect_err("a non-opaque focus identity must be refused");
            assert_eq!(opaque.code, "INVALID_REVIEW_SCOPE");

            std::fs::write(
                dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "name": "private owner label",
                    "segment_ids": ["review-a"],
                }))
                .unwrap(),
            )
            .unwrap();
            let active =
                get_active_voice_focus_v1(app.state()).await.expect("focus discovery").expect("focus is active");
            assert_eq!(active.segment_count, 1);
            assert!(!active.focus_id.contains("private owner label"), "the private policy name never crosses IPC");

            let focused = get_review_page_v1(
                ReviewScope::VoiceFocus { focus_id: active.focus_id.clone() },
                None,
                None,
                app.state(),
            )
            .await
            .expect("focused review page");
            assert_eq!(focused.scope_label, "voiceFocus");
            assert!(focused.focus_narrowed, "a focus page must say its total counts a subset");
            assert_eq!(focused.total, 1);
            assert_eq!(focused.items[0].segment.id, "review-a");

            std::fs::write(
                dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "name": "replacement label",
                    "segment_ids": ["review-b"],
                }))
                .unwrap(),
            )
            .unwrap();
            let stale = get_review_page_v1(
                ReviewScope::VoiceFocus { focus_id: active.focus_id.clone() },
                None,
                None,
                app.state(),
            )
            .await
            .expect_err("a replaced focus generation must invalidate the request");
            assert_eq!(stale.code, "STALE_VOICE_FOCUS");

            std::fs::remove_file(dir.path().join(crate::voice_focus::VOICE_FOCUS_FILE)).unwrap();
            let inactive =
                get_review_page_v1(ReviewScope::VoiceFocus { focus_id: active.focus_id }, None, None, app.state())
                    .await
                    .expect_err("a deactivated focus must not serve rows");
            assert_eq!(inactive.code, "VOICE_FOCUS_NOT_ACTIVE");
        });
    }

    #[test]
    fn library_reads_serve_pages_ids_and_rows_through_state() {
        let dir = tempfile::tempdir().unwrap();
        let app = managed_app_state(dir.path());
        seed_segment(&app, "lib-a", "deng yek du");
        seed_segment(&app, "lib-b", "wusha ciya jida");
        seed_segment(&app, "lib-c", "uniquetoken body");

        tauri::async_runtime::block_on(async {
            let invalid = get_segment("bad id!".into(), app.state()).await.expect_err("identifier gate");
            assert_eq!(invalid.code, "INVALID_SEGMENT_ID");
            let missing = get_segment("lib-missing".into(), app.state()).await.expect_err("unknown row");
            assert_eq!(missing.code, "SEGMENT_NOT_FOUND");
            assert_eq!(get_segment("lib-a".into(), app.state()).await.expect("hydrate one row").id, "lib-a");

            let page = get_segments_page(None, None, None, None, None, None, app.state()).await.expect("default page");
            assert_eq!(page.total, 3);
            assert_eq!(page.items.len(), 3);
            assert!(!page.focus_narrowed);

            let verified_only = get_segments_page(Some(true), None, None, None, None, None, app.state())
                .await
                .expect("verified filter");
            assert_eq!(verified_only.total, 0, "nothing is verified on a fresh library");

            assert_eq!(
                get_segments_page(None, None, Some("DROP TABLE".into()), None, None, None, app.state())
                    .await
                    .unwrap_err()
                    .code,
                "INVALID_LIBRARY_SORT"
            );
            assert_eq!(
                get_segments_page(None, None, None, None, Some("../evil".into()), None, app.state())
                    .await
                    .unwrap_err()
                    .code,
                "INVALID_LIBRARY_CURSOR"
            );
            assert_eq!(
                get_segments_page(None, Some("q".repeat(1001)), None, None, None, None, app.state())
                    .await
                    .unwrap_err()
                    .code,
                "INVALID_LIBRARY_QUERY"
            );
            let unfocused = get_segments_page(None, None, None, None, None, Some(true), app.state())
                .await
                .expect("a focused read with no policy file stays corpus-wide");
            assert_eq!(unfocused.total, 3);
            assert!(!unfocused.focus_narrowed);

            assert_eq!(
                get_segment_ids_for_view(None, None, Some("everything".into()), app.state()).await.unwrap_err().code,
                "INVALID_TRANSCRIPT_STATE"
            );
            assert_eq!(
                get_segment_ids_for_view(None, Some("q".repeat(1001)), None, app.state()).await.unwrap_err().code,
                "INVALID_LIBRARY_QUERY"
            );
            assert_eq!(get_segment_ids_for_view(None, None, None, app.state()).await.expect("all ids").len(), 3);

            assert!(
                get_signal_anomaly_segments(None, app.state()).await.expect("anomaly page").is_empty(),
                "no segment carries an anomaly score yet"
            );

            assert_eq!(get_segments(None, app.state()).await.expect("whole library").len(), 3);
            assert_eq!(get_segments(Some(true), app.state()).await.expect("verified subset").len(), 0);
            assert_eq!(get_segments_suspect_first(None, app.state()).await.expect("suspect-first order").len(), 3);

            let bounded = search_segments("q".repeat(1001), app.state()).await.expect_err("bounded query");
            assert!(bounded.contains("Search query"), "{bounded}");
            let hits = search_segments("uniquetoken".into(), app.state()).await.expect("FTS search");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].id, "lib-c");
        });
    }

    #[test]
    fn audio_probes_health_relink_and_learning_queue_through_state() {
        let dir = tempfile::tempdir().unwrap();
        let app = managed_app_state(dir.path());
        let wav = dir.path().join("waveform.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        for sample in 0..16_000 {
            writer.write_sample(((sample % 127) as i16) - 63).unwrap();
        }
        writer.finalize().unwrap();
        let garbage = dir.path().join("garbage.wav");
        std::fs::write(&garbage, b"this is not a RIFF container").unwrap();

        tauri::async_runtime::block_on(async {
            let points = get_waveform(wav.to_string_lossy().into_owned(), 16, None, app.state())
                .await
                .expect("waveform for real audio");
            assert_eq!(points.len(), 16, "one second at 16 kHz bins into exactly the requested points");
            assert!(points.iter().all(|point| point.is_finite()));

            assert_eq!(
                get_waveform(dir.path().join("missing.wav").to_string_lossy().into_owned(), 16, None, app.state())
                    .await
                    .unwrap_err()
                    .code,
                "INVALID_AUDIO_PATH"
            );
            assert_eq!(
                get_waveform(wav.to_string_lossy().into_owned(), 16, Some("{ broken".into()), app.state())
                    .await
                    .unwrap_err()
                    .code,
                "INVALID_ALIGNMENT"
            );
            let undecodable = get_waveform(garbage.to_string_lossy().into_owned(), 16, None, app.state())
                .await
                .expect_err("garbage bytes cannot decode");
            assert_eq!(undecodable.code, "WAVEFORM_FAILED");
            assert!(undecodable.retryable);

            let health = get_audio_health(app.state()).await.expect("audio health scan");
            assert_eq!(health.total_files, 0);
            assert_eq!(health.missing_files, 0);
            assert!(health.missing_paths.is_empty());

            assert_eq!(
                relink_audio(r"\\attacker\share".into(), app.state()).await.unwrap_err().code,
                "INVALID_RELINK_FOLDER",
                "a renderer-supplied UNC search dir must be refused before any filesystem probe"
            );
            let relinked = relink_audio(dir.path().to_string_lossy().into_owned(), app.state())
                .await
                .expect("relink over an empty library");
            assert_eq!(relinked.relinked, 0);
            assert_eq!(relinked.still_missing, 0);

            assert!(
                get_active_learning_queue(app.state(), 0.05, 0.95, 10).await.expect("empty queue").is_empty(),
                "an empty library has nothing to prioritize"
            );
        });
    }
}
