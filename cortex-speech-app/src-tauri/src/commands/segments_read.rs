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
use crate::{audio, quality, AppState};
use std::time::Duration;
use tauri::State;

#[tauri::command]
pub async fn get_segments(verified: Option<bool>, state: State<'_, AppState>) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_segments")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        db.get_segments(verified).map_err(|e| e.to_string())
    })
    .await
}

/// M2.5: Return segments ordered by suspect-first priority: escalated + low confidence first.
/// Priority: 1) Jury escalated, 2) Low agent confidence, 3) Chronological.
#[tauri::command]
pub async fn get_segments_suspect_first(
    verified: Option<bool>,
    state: State<'_, AppState>,
) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("get_segments_suspect_first")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        db.get_segments_suspect_first(verified).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn search_segments(query: String, state: State<'_, AppState>) -> Result<Vec<SpeechSegment>, String> {
    RATE_LIMITER.check("search_segments")?;
    // Bound the free-text query like every other text-accepting command (save_session caps its
    // search_query at 1000): an unbounded multi-MB string otherwise reaches the FTS5 MATCH parser.
    validate::validate_text(&query, 1000, "Search query")?;
    // Off the main thread: the FTS5 MATCH has no LIMIT, so a common token materializes + serializes a
    // large slice of the library. Run it on the blocking pool exactly like the get_segments siblings so
    // a keystroke in the search box can't freeze the UI. db_arc + lock INSIDE the task — never hold a
    // lock_db() guard across the await.
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        db.search_segments(&query).map_err(|e| e.to_string())
    })
    .await
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
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        db.audio_health().map_err(|e| e.to_string())
    })
    .await
}

/// P3.3: relink missing source audio by basename against a folder the owner picks.
#[tauri::command]
pub async fn relink_audio(search_dir: String, state: State<'_, AppState>) -> Result<crate::db::RelinkResult, String> {
    STRICT_RATE_LIMITER.check("relink_audio")?;
    if search_dir.contains('\0') {
        return Err("Search directory contains null bytes".to_string());
    }
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
    let db = state.db_arc();
    run_blocking(move || {
        let segments = {
            let db = db.lock().unwrap_or_else(|p| p.into_inner());
            db.get_segments(None).map_err(|e| e.to_string())?
        };

        let cert = quality::conformal::calibrate_and_certify(&segments, target_error, confidence_level);
        let q_hat = cert.threshold;

        let mut candidates: Vec<(SpeechSegment, f64)> = segments
            .into_iter()
            .filter(|s| !s.verified)
            .map(|s| {
                let score = quality::conformal::compute_nonconformity_score(&s);
                let uncertainty = -(score - q_hat).abs();
                (s, uncertainty)
            })
            .collect();

        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(candidates.into_iter().take(limit).map(|(s, _)| s).collect())
    })
    .await
}
