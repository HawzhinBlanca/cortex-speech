//! Per-segment transcription / alignment / audio-check IPC commands — slice 6 of the Week-4
//! `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use
//! transcribe::*;`), so `lib.rs`'s invoke_handler still names `commands::transcribe_segment` and the
//! frontend invokes are untouched. Same functions, only relocated.
//!
//! Each decodes audio + runs ONNX/WSL inference (or re-diarizes), so the heavy body runs via
//! `run_blocking` to keep the UI thread free.

use super::{decode_finetuned_clip_16k, resolve_finetuned_paths, run_blocking, RATE_LIMITER, STRICT_RATE_LIMITER};
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

/// Opt-in: transcribe a segment with the CONSTRAINED Kurdish-token CTC decode (guarantees
/// Kurdish-script output) via the `ort` raw-logits path, instead of the default sherpa-onnx
/// decode. Additive — it does NOT touch the default `transcribe_segment` path. Loads a fresh ort
/// session per call (fine for a user-initiated action; session caching is a perf follow-up).
#[tauri::command]
pub async fn transcribe_segment_constrained(
    audio_path: String,
    alignment_json: Option<String>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("transcribe_segment_constrained")?;
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    // Decode + constrained CTC beam search — off the main thread.
    run_blocking(move || {
        let models = crate::models::active_models_dir();
        let model = models.join(crate::models::OMNIASR_CTC_300M_MODEL);
        let tokens = models.join(crate::models::OMNIASR_CTC_300M_TOKENS);
        if !model.exists() || !tokens.exists() {
            return Err("OmniASR model/tokens not found for constrained decode".to_string());
        }
        // Runtime integrity gate — the SAME SHA-256 pin the default ASR path enforces (asr.rs:288-311):
        // a tampered/swapped same-line-count tokens.txt (wrong id->grapheme map) or ONNX still LOADS but
        // decodes every clip to the WRONG graphemes, persisted as trustworthy. Verify BOTH the model and
        // the tokens vocab before the constrained decode runs; the default path refuses on mismatch and so
        // must this opt-in one. verify_model_path_runtime is a no-op for an unpinned file.
        crate::models::verify_model_path_runtime(&model, crate::models::OMNIASR_CTC_300M_MODEL)?;
        crate::models::verify_model_path_runtime(&tokens, crate::models::OMNIASR_CTC_300M_TOKENS)?;
        // decode_to_pcm returns 16 kHz mono PCM (the model's expected input rate).
        let (rate, pcm) = crate::audio::decode_to_pcm(&audio_path).map_err(|e| e.to_string())?;
        // Slice only THIS segment's clip — every VAD chunk shares the whole-source audio_path (the range is
        // in alignment_json), so decoding `pcm` directly would re-transcribe the ENTIRE recording into one
        // segment. None alignment (single-segment file) = whole file. Mirrors the finetuned/Scribe paths.
        let (clip, _suffix) = crate::chunking::slice_pcm_by_alignment(&pcm, rate, alignment_json.as_deref())
            .map_err(|e| e.to_string())?;
        let audio: Vec<f32> = clip.iter().map(|&s| s as f32 / 32768.0).collect();
        let text = crate::constrained_decode::run_constrained(&model, &tokens, &audio, true)?;
        // A blank decode is NOT a transcript. Returning it would let the frontend overwrite an existing
        // good transcript with "" (a real result destroyed + a blank persisted, violating the no-blank
        // honesty rule). Fail instead — the caller keeps the current transcript (opt-in path: no fallback).
        if text.trim().is_empty() {
            return Err(
                "Constrained transcription produced no text (silent clip or no Kurdish speech) — the existing transcript is unchanged."
                    .to_string(),
            );
        }
        Ok(serde_json::json!({ "text": text, "rawTranscript": text }))
    })
    .await
}

/// Opt-in: transcribe a segment with the fine-tuned Kurdish Wav2Vec2-CTC model (ONNX via `ort`),
/// which roughly halves CER vs the stock OmniASR path (see docs/EVAL.md). Additive — the default
/// `transcribe_segment` path is unchanged. Resolves the model from `CORTEX_FINETUNED_ONNX` +
/// `CORTEX_FINETUNED_VOCAB` (dev/testing) or `<models>/finetuned-mms-ckb/{model.onnx,vocab.json}`.
#[tauri::command]
pub async fn transcribe_segment_finetuned(
    audio_path: String,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    RATE_LIMITER.check("transcribe_segment_finetuned")?;
    validate::validate_file_path(&audio_path)?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj)?;
    }
    let db = state.db_arc();
    // Windowed decode + fine-tuned ONNX inference — off the main thread.
    run_blocking(move || {
        let (onnx, vocab) = resolve_finetuned_paths()?;
        // Offset-less alignment is only whole-file-safe when this source has NO sibling chunks (see
        // decode_finetuned_clip_16k): on a multi-segment source it would silently re-transcribe the
        // entire recording into one segment (true-10 audit 2026-07-09).
        let sibling_count: i64 = {
            let db = db.lock().unwrap_or_else(|p| p.into_inner());
            db.connection()
                .query_row(
                    "SELECT COUNT(*) FROM speech_segments WHERE audio_path = ?1",
                    rusqlite::params![audio_path],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?
        };
        // Decode ONLY this segment's clip window (16 kHz). Every VAD chunk shares the whole-source
        // audio_path with its range in alignment_json; decoding the WHOLE source first (the old path) failed
        // on long recordings ("audio too large to decode in one pass") even though the clip is a few
        // seconds. The windowed decode reads just the clip, so re-transcribe works on any source length.
        let clip = decode_finetuned_clip_16k(&audio_path, alignment_json.as_deref(), sibling_count <= 1)?;
        // Route through the pipeline's shared windowed wrapper, NOT run_wav2vec2 directly: the model is
        // trained on short utterances and a single unbounded pass over >15 s (a raised max-segment
        // setting, or a whole-file/no-meta clip) duplicates text — the pipeline path sub-splits into
        // ~15 s windows for exactly that reason (true-10 audit 2026-07-09: one engine, two call paths,
        // two different qualities).
        let text = crate::pipeline::ProcessingPipeline::transcribe_chunk_finetuned(&onnx, &vocab, &clip)?;
        // transcribe_chunk_finetuned returns Ok("") on a silent/blank-decoding clip. Returning it would let
        // the frontend overwrite an existing good transcript with "" (data loss + a blank persisted as a
        // real result). Fail instead — the in-pipeline finetuned path guards this exact case (pipeline.rs);
        // this standalone opt-in command must too (no fallback engine here).
        if text.trim().is_empty() {
            return Err(
                "Fine-tuned transcription produced no text (the clip may be silent) — the existing transcript is unchanged."
                    .to_string(),
            );
        }
        Ok(serde_json::json!({ "text": text, "rawTranscript": text }))
    })
    .await
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
    run_blocking(move || {
        let draft = pipeline
            .transcribe(segment_id.as_deref(), &audio_path, alignment_json.as_deref(), None)
            .map_err(|e| e.to_string())?;
        // A blank draft is NOT a transcript. Returning Ok("") lets the frontend upsert "" over an
        // existing good transcript (App.svelte handleTranscribe: annotatedTranscript = result.text, then
        // updateSegment), destroying it and persisting a blank. The two opt-in siblings
        // (transcribe_segment_constrained/_finetuned) already refuse this exact case; the DEFAULT path
        // must too — the frontend's try/catch then keeps the current transcript. (Memory:
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
    let db = state.db_arc();
    run_blocking(move || {
        let stored_segment = if let Some(ref id) = segment_id {
            let db_guard = db.lock().unwrap_or_else(|p| p.into_inner());
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
                let db = db.lock().unwrap_or_else(|p| p.into_inner());
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
    run_blocking(move || pipeline.rediarize_segments(&ids).map_err(|e| e.to_string())).await
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
