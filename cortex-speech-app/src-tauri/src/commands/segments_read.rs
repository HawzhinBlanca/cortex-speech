//! Segment + audio read/retrieval IPC commands — slice 8 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use
//! segments_read::*;`), so `lib.rs`'s invoke_handler still names `commands::get_segments` and the
//! frontend invokes are untouched. Same functions, only relocated.
//!
//! These are the whole-library reads / unbounded FTS search / audio-health scan / waveform + duration
//! probes — all `async` + `run_blocking` so a large library never freezes the UI thread.

use super::{run_blocking, send_audio_duration_probe_result, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::db::SpeechSegment;
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
pub async fn get_waveform(
    path: String,
    num_points: usize,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<f32>, String> {
    RATE_LIMITER.check("get_waveform")?;
    let validated = validate::validate_file_path(&path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    // Clone the pipeline out of the global lock before the (up to 30 s) decode so a waveform
    // render never starves other pipeline-lock users (matches import_audio_file / rediarize /
    // run_gold_eval_asr, which all clone for the same reason), then decode OFF the main thread.
    let pipeline = state.lock_pipeline().clone();
    run_blocking(move || {
        pipeline.get_waveform(&validated, num_points, alignment_json.as_deref()).map_err(|e| e.to_string())
    })
    .await
}

/// P3.3: report which distinct source audio files are missing on disk (moved/renamed/deleted).
#[tauri::command]
pub async fn get_audio_health(state: State<'_, AppState>) -> Result<crate::db::AudioHealth, String> {
    RATE_LIMITER.check("get_audio_health")?;
    let segment_queries = state.segment_queries();
    run_blocking(move || segment_queries.audio_health().map_err(|e| e.to_string())).await
}

/// P3.3: relink missing source audio by basename against a folder the owner picks.
#[tauri::command]
pub async fn relink_audio(search_dir: String, state: State<'_, AppState>) -> Result<crate::db::RelinkResult, String> {
    STRICT_RATE_LIMITER.check("relink_audio")?;
    // P1.1: reject a UNC search dir BEFORE db.relink_audio probes `search_dir.join(name).is_file()` — a
    // renderer-supplied `\\attacker\share` would otherwise drive the SMB redirector (NTLM forced-auth
    // leak) on the is_file() stat and then PERSIST the UNC path into the row. Syntactic guard, zero I/O
    // (no canonicalize: the picked dir is searched as-is); also subsumes the prior null-byte check.
    validate::reject_unc_path(&search_dir)?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        db.relink_audio(std::path::Path::new(&search_dir)).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn get_active_learning_queue(
    state: State<'_, AppState>,
    target_error: f64,
    confidence_level: f64,
    limit: usize,
) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_active_learning_queue")?;
    let segment_queries = state.segment_queries();
    run_blocking(move || {
        segment_queries.active_learning_queue(target_error, confidence_level, limit).map_err(Into::into)
    })
    .await
}
