//! Per-segment transcription / alignment / audio-check IPC commands — slice 6 of the Week-4
//! `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use
//! transcribe::*;`), so `lib.rs`'s invoke_handler still names `commands::transcribe_segment` and the
//! frontend invokes are untouched. Same functions, only relocated.
//!
//! Each decodes audio + runs ONNX/WSL inference (or re-diarizes), so the heavy body runs via
//! `run_blocking` to keep the UI thread free.

use super::{run_blocking, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::validation::input as validate;
use crate::{aligner, audio, AppState};
use tauri::State;

fn canonical_alignment_inputs(
    requested_audio_path: String,
    requested_alignment_json: Option<String>,
    stored_segment: Option<crate::db::SpeechSegment>,
    segment_id: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let Some(id) = segment_id else {
        return Ok((requested_audio_path, requested_alignment_json));
    };
    let stored = stored_segment.ok_or_else(|| format!("Segment '{id}' no longer exists"))?;
    if stored.audio_path != requested_audio_path {
        return Err(format!("Segment '{id}' audio path changed; reload it before aligning"));
    }
    // A list-page row intentionally carries alignment_json = null. When an id is available, the DB
    // row is the authority: its JSON contains source_start/source_end, and accepting the lightweight
    // caller value would align the entire source file then persist words-only JSON over chunk identity.
    Ok((stored.audio_path, stored.alignment_json))
}

#[tauri::command]
pub async fn transcribe_segment(
    segment_id: Option<String>,
    audio_path: String,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("transcribe_segment")?;
    if let Some(ref id) = segment_id {
        validate::validate_identifier(id)?;
    }
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    // Clone the pipeline (Arc-wrapped internals) so the global pipeline mutex is released before the
    // possibly-long WSL/ONNX transcription, and run it OFF the main thread so the UI stays responsive.
    let pipeline = state.lock_pipeline().clone();
    let database = state.db_runtime();
    run_blocking(move || {
        let _mutation = database.begin_mutation()?;
        let id = segment_id.as_deref().ok_or_else(|| {
            "E_TRANSCRIPTION_SOURCE_UNBOUND: an imported segment id is required for transcription".to_string()
        })?;
        let source = pipeline
            .bind_existing_transcription_source(id, Some(&audio_path), alignment_json.as_deref())
            .map_err(|error| error.to_string())?;
        let draft = pipeline.transcribe_bound(&source, None).map_err(|e| e.to_string())?;
        // A blank draft is NOT a transcript. Returning Ok("") lets the frontend upsert "" over an
        // existing good transcript, destroying it and persisting a blank. The production command must
        // fail closed so the frontend keeps the current transcript. (Memory:
        // blank-transcript-never-overwrites-good; recurring data-loss class.)
        if draft.final_text.trim().is_empty() {
            return Err(
                "Transcription produced no text (silent clip or no speech) — the existing transcript is unchanged."
                    .to_string(),
            );
        }
        Ok(serde_json::json!({
            "text": draft.final_text,
            "rawTranscript": draft.raw_text,
            "confidence": draft.confidence,
            "confidenceSource": draft.confidence_source,
            "modelVersionId": draft.model_version_id,
            "cloudCall": draft.cloud_call
            ,"segmentId": source.segment().id
        }))
    })
    .await
}

#[tauri::command]
pub async fn align_segment(
    audio_path: String,
    text: String,
    alignment_json: Option<String>,
    segment_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<aligner::WordTimestamp>, String> {
    RATE_LIMITER.check("align_segment")?;
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    if let Some(ref id) = segment_id {
        validate::validate_identifier(id)?;
    }
    if text.trim().is_empty() {
        return Err("Alignment text cannot be empty".to_string());
    }
    validate::validate_text(&text, 100000, "Alignment text")?;
    // Clone the pipeline OUT of the lock so the global mutex is released before the slow decode +
    // ONNX forced alignment, and run the whole align + persist OFF the main thread — holding either
    // lock (or blocking the UI thread) serializes every other pipeline command (get_import_status
    // polling, transcribe, get_waveform) for the whole alignment. ProcessingPipeline is Clone; align
    // takes &self.
    let pipeline = state.lock_pipeline().clone();
    let database = state.db_runtime();
    run_blocking(move || {
        let mutation = database.begin_mutation()?;
        let stored_segment = if let Some(ref id) = segment_id {
            let db_guard = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
            db_guard
                .get_segment_by_id(id)
                .map_err(|error| format!("Failed to reload segment {id} before alignment: {error}"))?
        } else {
            None
        };
        let (audio_path, alignment_json) =
            canonical_alignment_inputs(audio_path, alignment_json, stored_segment, segment_id.as_deref())?;
        validate::validate_file_path(&audio_path)?;
        if let Some(ref aj) = alignment_json {
            validate::validate_alignment_json(aj)?;
        }
        let (timestamps, quality) =
            pipeline.align(&audio_path, &text, alignment_json.as_deref()).map_err(|e| e.to_string())?;
        // Persist the word timings INTO alignment_json (merged with existing chunk metadata) AND stamp
        // the honest alignment_quality in ONE atomic statement, so per-word review features survive a
        // reload and the timings can never land without their quality marker (quality.rs raises the
        // energy-heuristic review-risk reason only when the marker is present). The quality
        // distinguishes real CTC forced alignment from the linear/energy heuristic fallback.
        if let Some(ref id) = segment_id {
            if !timestamps.is_empty() {
                let merged = crate::chunking::merge_word_timestamps(alignment_json.as_deref(), &timestamps);
                let db = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
                let persisted = db
                    .update_segment_alignment_if_unchanged(id, alignment_json.as_deref(), &merged, quality.as_db_str())
                    .map_err(|error| format!("Failed to persist word timings + quality for {id}: {error}"))?;
                if !persisted {
                    return Err(format!("Segment {id} changed while alignment was running; reload it and try again"));
                }
            }
        }
        Ok(timestamps)
    })
    .await
}

#[tauri::command]
pub async fn rediarize_segments(ids: Vec<String>, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("rediarize_segments")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    // Clone the pipeline and let it open its own DB connection, so neither the global pipeline nor
    // db mutex is held across the per-file decode + diarization-inference loop, and run it OFF the
    // main thread so the UI stays responsive for the decode duration.
    let pipeline = state.lock_pipeline().clone();
    let database = state.db_runtime();
    run_blocking(move || {
        let _mutation = database.begin_mutation()?;
        pipeline.rediarize_segments(&ids).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn check_audio(path: String) -> Result<serde_json::Value, String> {
    run_blocking(move || {
        let validated = validate::validate_file_path(&path)?;
        let info = audio::check_audio_file(&validated).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "duration_ms": info.duration_ms,
            "sample_rate": info.sample_rate,
            "channels": info.channels,
            "format": info.format,
        }))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::canonical_alignment_inputs;
    use crate::db::SpeechSegment;

    #[test]
    fn alignment_with_segment_id_uses_stored_chunk_metadata() {
        let stored_json = r#"{"source_start_ms":12000,"source_end_ms":13000,"chunk_index":2,"chunk_count":4}"#;
        let stored = SpeechSegment {
            id: "seg-1".to_string(),
            audio_path: "C:\\audio\\book.wav".to_string(),
            alignment_json: Some(stored_json.to_string()),
            ..SpeechSegment::default()
        };

        let (_, alignment) = canonical_alignment_inputs(stored.audio_path.clone(), None, Some(stored), Some("seg-1"))
            .expect("stored segment should be canonical");

        assert_eq!(alignment.as_deref(), Some(stored_json));
    }

    #[test]
    fn alignment_rejects_a_stale_audio_path_for_a_segment_id() {
        let stored = SpeechSegment {
            id: "seg-1".to_string(),
            audio_path: "C:\\audio\\current.wav".to_string(),
            ..SpeechSegment::default()
        };

        let error = canonical_alignment_inputs("C:\\audio\\stale.wav".to_string(), None, Some(stored), Some("seg-1"))
            .expect_err("stale caller path must fail closed");

        assert!(error.contains("audio path changed"));
    }
}
