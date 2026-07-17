//! Batch review-action IPC commands — slice 3 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES are unchanged: `commands.rs` re-exports this module
//! (`pub use batch::*;`), so `lib.rs`'s invoke_handler still names `commands::batch_verify` and the
//! frontend's `invoke('batch_verify')` is untouched. Same functions, only relocated.
//!
//! Each spawns a worker thread and returns immediately (the audit's OFFLOADED_HIGH set) so a
//! whole-library batch never blocks the UI; progress + per-item failures stream via `emit_or_log`.
//! (batch_transcribe stays in commands.rs for now — it is coupled to the jury `with_jury_db` helper.)

use super::{emit_or_log, STRICT_RATE_LIMITER};
use crate::db::SpeechSegment;
use crate::validation::input as validate;
use crate::AppState;
use std::sync::Arc;
use tauri::{Manager, State};

#[tauri::command]
pub fn batch_verify(
    ids: Vec<String>,
    verified: bool,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("batch_verify")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }

    let total = ids.len();
    state.try_start_batch()?;

    let cancel = state.ensure_cancel_token()?;
    let app_clone = app.clone();

    std::thread::spawn(move || {
        struct BatchGuard {
            app: tauri::AppHandle,
        }
        impl Drop for BatchGuard {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_batch();
                }
            }
        }
        let _guard = BatchGuard { app: app_clone.clone() };

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({ "type": "started", "total": total, "operation": "verify" }),
        );

        // One targeted UPDATE per segment — no read-modify-write cycle.
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut cancelled = false;

        for (i, id) in ids.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let update_ok = if let Some(app_state) = app_clone.try_state::<AppState>() {
                match app_state.lock_db().update_verified(id, verified) {
                    Ok(updated) => updated,
                    Err(error) => {
                        tracing::error!("Batch verify DB update failed for {id}: {error}");
                        false
                    }
                }
            } else {
                false
            };

            if update_ok {
                succeeded += 1;
            } else {
                failed += 1;
            }

            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": i + 1, "total": total,
                    "file": id,
                    "status": if verified { "verifying" } else { "unverifying" },
                    "operation": "verify"
                }),
            );
        }

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "completed", "total": total,
                "succeeded": succeeded, "failed": failed,
                "cancelled": cancelled, "operation": "verify"
            }),
        );
    });

    Ok(serde_json::json!({ "status": "started" }))
}

#[tauri::command]
pub fn batch_assign_speaker(
    ids: Vec<String>,
    speaker_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("batch_assign_speaker")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    if !speaker_id.is_empty() {
        validate::validate_text(&speaker_id, 256, "Speaker ID")?;
    }

    let total = ids.len();
    state.try_start_batch()?;

    let cancel = state.ensure_cancel_token()?;
    let app_clone = app.clone();
    let speaker_id_clone = speaker_id.clone();

    std::thread::spawn(move || {
        struct BatchGuard {
            app: tauri::AppHandle,
        }
        impl Drop for BatchGuard {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_batch();
                }
            }
        }
        let _guard = BatchGuard { app: app_clone.clone() };

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({ "type": "started", "total": total, "operation": "assign_speaker" }),
        );

        // One targeted UPDATE per segment — avoids full read-modify-write cycle.
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut cancelled = false;
        let spk: Option<&str> = if speaker_id_clone.is_empty() { None } else { Some(&speaker_id_clone) };

        for (i, id) in ids.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let update_ok = if let Some(app_state) = app_clone.try_state::<AppState>() {
                match app_state.lock_db().update_speaker_id(id, spk) {
                    Ok(updated) => updated,
                    Err(error) => {
                        tracing::error!("Batch speaker assignment DB update failed for {id}: {error}");
                        false
                    }
                }
            } else {
                false
            };

            if update_ok {
                succeeded += 1;
            } else {
                failed += 1;
            }

            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": i + 1, "total": total,
                    "file": id, "status": "assigning speaker",
                    "operation": "assign_speaker"
                }),
            );
        }

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "completed", "total": total,
                "succeeded": succeeded, "failed": failed,
                "cancelled": cancelled, "operation": "assign_speaker"
            }),
        );
    });

    Ok(serde_json::json!({ "status": "started" }))
}

#[tauri::command]
pub fn batch_normalize(
    ids: Vec<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("batch_normalize")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }

    let total = ids.len();
    state.try_start_batch()?;

    let cancel = state.ensure_cancel_token()?;
    let settings = state.lock_settings().clone();
    let config = crate::normalizer::NormalizationConfig {
        normalize_numbers: settings.auto_normalize,
        verbalize_numbers: settings.verbalize_numbers,
        normalize_hamza: true,
        remove_diacritics: false,
    };
    let normalizer = Arc::new(crate::normalizer::SoraniNormalizer::with_config(config));
    let app_clone = app.clone();

    std::thread::spawn(move || {
        struct BatchGuard {
            app: tauri::AppHandle,
        }
        impl Drop for BatchGuard {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_batch();
                }
            }
        }
        let _guard = BatchGuard { app: app_clone.clone() };

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "started", "total": total, "operation": "normalize"
            }),
        );

        let mut prefetch_failed_ids: Vec<String> = Vec::new();
        let segments: Vec<SpeechSegment> = if let Some(app_state) = app_clone.try_state::<AppState>() {
            let db = app_state.lock_db();
            let mut found = Vec::new();
            for id in &ids {
                match db.get_segment_by_id(id) {
                    Ok(Some(seg)) => found.push(seg),
                    Ok(None) => {
                        tracing::warn!("Batch normalize segment not found during prefetch: {id}");
                        prefetch_failed_ids.push(id.clone());
                    }
                    Err(error) => {
                        tracing::error!("Batch normalize DB prefetch failed for {id}: {error}");
                        prefetch_failed_ids.push(id.clone());
                    }
                }
            }
            found
        } else {
            tracing::error!("Batch normalize app state unavailable during prefetch");
            prefetch_failed_ids.extend(ids.iter().cloned());
            Vec::new()
        };

        // Fold the result-affecting config flags into the cache key. NORMALIZER_CACHE is a
        // never-cleared process-global static, so keying on raw text alone replayed the FIRST
        // config's normalization for the same text after the user toggled auto_normalize /
        // verbalize_numbers (digit handling differs), persisting the wrong normalized_transcript.
        let (auto_norm, verbalize) = (settings.auto_normalize, settings.verbalize_numbers);
        let results = crate::perf::parallel_batch(&segments, |seg| {
            let cache_key = format!("{}|{}|{}", auto_norm as u8, verbalize as u8, seg.raw_transcript);
            let normalized =
                crate::perf::NORMALIZER_CACHE.memoize(&cache_key, |_| normalizer.normalize(&seg.raw_transcript));
            (seg.id.clone(), normalized)
        });

        let mut succeeded = 0u32;
        let mut failed = prefetch_failed_ids.len() as u32;
        let mut cancelled = false;

        for (i, id) in prefetch_failed_ids.iter().enumerate() {
            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": i + 1, "total": total,
                    "file": id, "status": "failed", "operation": "normalize"
                }),
            );
        }

        for (i, (id, normalized)) in results.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }

            let update_ok = if let Some(app_state) = app_clone.try_state::<AppState>() {
                let db = app_state.lock_db();
                match db.get_segment_by_id(id) {
                    Ok(Some(mut seg)) => {
                        seg.normalized_transcript = Some(normalized.clone());
                        // CRITICAL: Do NOT overwrite annotated_transcript here.
                        // annotated_transcript is the human-corrected or LLM-refined
                        // ground truth. Normalization only affects the normalized field.
                        match db.insert_segment(&seg) {
                            Ok(()) => true,
                            Err(error) => {
                                tracing::error!("Batch normalize DB update failed for {id}: {error}");
                                false
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::warn!("Batch normalize segment disappeared before update: {id}");
                        false
                    }
                    Err(error) => {
                        tracing::error!("Batch normalize DB lookup failed before update for {id}: {error}");
                        false
                    }
                }
            } else {
                tracing::error!("Batch normalize app state unavailable before update for {id}");
                false
            };

            if update_ok {
                succeeded += 1;
            } else {
                failed += 1;
            }

            emit_or_log(
                &app_clone,
                "batch-progress",
                serde_json::json!({
                    "type": "progress", "current": prefetch_failed_ids.len() + i + 1, "total": total,
                    "file": id, "status": "normalizing", "operation": "normalize"
                }),
            );
        }

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "completed", "total": total,
                "succeeded": succeeded, "failed": failed,
                "cancelled": cancelled, "operation": "normalize"
            }),
        );
    });

    Ok(serde_json::json!({ "status": "started" }))
}
