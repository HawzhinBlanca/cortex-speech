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
    let db = state.db_arc();
    run_blocking(move || {
        // P1.3, last site. This read the WHOLE library as full records — transcripts, alignment JSON,
        // evidence JSON — to compute one threshold and then return at most `limit` clips.
        //
        // The audit filed this under "compute the conformal threshold in SQL", but that conflates two
        // separable problems. The SELECTION RULE here really is naive (rank by distance to one
        // threshold) and fixing it needs the frozen human-labelled calibration split that does not exist
        // until the Gold Marathon — that is P1.4 and it stays open. The MEMORY SHAPE is independent of
        // that and fixable now, so it is fixed now, and the ranking below is byte-for-byte what it was.
        //
        // ONE streaming pass does both jobs: the tally accumulates the certificate, and every unverified
        // row's nonconformity is captured as it goes by. `q_hat` is not known until the pass ends, but it
        // is a GLOBAL constant applied afterwards, so the per-segment score is all that must be carried.
        //
        // What survives the pass is `(id, score)` per unverified row — tens of bytes — instead of the
        // full record. Only the `limit` clips actually returned are hydrated, exactly as couch.rs does.
        let (q_hat, mut scored) = {
            let db = db.lock().unwrap_or_else(|p| p.into_inner());
            let mut tally = quality::conformal::ConformalTally::default();
            let mut scored: Vec<(String, f64)> = Vec::new();
            db.for_each_segment(None, |seg| {
                if !seg.verified {
                    scored.push((seg.id.clone(), quality::conformal::compute_nonconformity_score(&seg)));
                }
                tally.push(&seg);
            })
            .map_err(|e| e.to_string())?;
            (tally.finish(target_error, confidence_level).threshold, scored)
        };

        // Identical ordering to the Vec<(SpeechSegment, f64)> sort this replaces: uncertainty is
        // `-(score - q_hat).abs()` sorted DESCENDING, and `sort_by` is STABLE, so ties keep corpus
        // order. Both properties are preserved deliberately — this change is about memory, not ranking.
        scored.sort_by(|a, b| {
            let (ua, ub) = (-(a.1 - q_hat).abs(), -(b.1 - q_hat).abs());
            ub.partial_cmp(&ua).unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);

        // Hydrate only what is returned, then re-impose the ranked order: get_segments_by_ids applies its
        // own global ordering, and handing back a differently-ordered queue would silently change which
        // clip a reviewer is asked to judge first — the whole point of an active-learning queue.
        let ids: Vec<String> = scored.iter().map(|(id, _)| id.clone()).collect();
        let rows = {
            let db = db.lock().unwrap_or_else(|p| p.into_inner());
            db.get_segments_by_ids(&ids).map_err(|e| e.to_string())?
        };
        let by_id: std::collections::HashMap<&str, &SpeechSegment> = rows.iter().map(|s| (s.id.as_str(), s)).collect();
        // filter_map, not unwrap: a clip can be deleted between the scan and the fetch. Returning one
        // fewer is correct; panicking on a race in a read command is not.
        Ok(ids.iter().filter_map(|id| by_id.get(id.as_str()).map(|s| (*s).clone())).collect())
    })
    .await
}
