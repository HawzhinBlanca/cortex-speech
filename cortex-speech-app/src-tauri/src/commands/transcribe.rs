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
use crate::ipc_contract::{CommandErrorV1, TranscribedSegmentV1, WordTimestampV1};
use crate::validation::input as validate;
use crate::{aligner, audio, AppState};
use std::sync::Arc;
use tauri::State;

fn canonical_alignment_inputs(
    requested_audio_path: String,
    requested_text: String,
    requested_alignment_json: Option<String>,
    stored_segment: Option<(crate::db::SpeechSegment, i64)>,
    segment_id: Option<&str>,
) -> Result<(String, String, Option<String>, Option<i64>), String> {
    let Some(id) = segment_id else {
        return Ok((requested_audio_path, requested_text, requested_alignment_json, None));
    };
    let (stored, revision) = stored_segment.ok_or_else(|| format!("Segment '{id}' no longer exists"))?;
    if stored.audio_path != requested_audio_path {
        return Err(format!("Segment '{id}' audio path changed; reload it before aligning"));
    }
    let authoritative_text = crate::quality::effective_transcript(&stored).to_string();
    if authoritative_text.trim().is_empty() {
        return Err(format!("Segment '{id}' has no authoritative transcript to align"));
    }
    // Alignment is durable evidence about a particular transcript. A draft, normalized/refined
    // machine string, or stale renderer row must never acquire timings under the segment id. Compare
    // the caller's visible text for an actionable refusal, then run inference on the DB-owned bytes.
    if requested_text.trim() != authoritative_text.trim() {
        return Err(format!("Segment '{id}' transcript changed; reload it before aligning"));
    }
    // A list-page row intentionally carries alignment_json = null. When an id is available, the DB
    // row is the authority: its JSON contains source_start/source_end, and accepting the lightweight
    // caller value would align the entire source file then persist words-only JSON over chunk identity.
    Ok((stored.audio_path, authoritative_text, stored.alignment_json, Some(revision)))
}

/// A historical whole-file segment may have `alignment_json = NULL`. Word timing JSON cannot stay
/// offset-less: the shared transcription slicer deliberately rejects such blobs as clobbered chunk
/// identity. Promote NULL to the exact explicit 0..duration source span before merging fresh words.
fn merge_bound_transcription_words(
    segment: &crate::db::SpeechSegment,
    words: &[aligner::WordTimestamp],
) -> Result<String, String> {
    let base = if let Some(existing) = segment.alignment_json.clone() {
        existing
    } else {
        if segment.duration_ms <= 0 {
            return Err(format!(
                "Segment '{}' has no positive stored duration; refusing to publish word timings without source identity",
                segment.id
            ));
        }
        crate::chunking::SegmentSourceMeta {
            source_start_ms: 0,
            source_end_ms: segment.duration_ms,
            chunk_index: 0,
            chunk_count: 1,
        }
        .to_alignment_json()
    };
    Ok(crate::chunking::merge_word_timestamps(Some(&base), words))
}

/// The durable text presented for review. Delegating to the one canonical projection prevents this
/// IPC response from promoting normalized/refined machine evidence over human/champion authority.
fn machine_review_text(segment: &crate::db::SpeechSegment) -> &str {
    crate::quality::effective_transcript(segment)
}

/// Review text that will be authoritative after a bound champion commit. The commit preserves an
/// existing human annotation; otherwise the new immutable champion raw transcript is the baseline.
/// Derived/normalized text is evidence only and must not drive reviewer word timings.
fn prospective_champion_review_text<'a>(segment: &'a crate::db::SpeechSegment, champion_raw: &'a str) -> &'a str {
    segment.annotated_transcript.as_deref().filter(|text| !text.trim().is_empty()).unwrap_or(champion_raw)
}

#[tauri::command]
#[specta::specta]
pub async fn transcribe_segment(
    segment_id: Option<String>,
    audio_path: String,
    alignment_json: Option<String>,
    state: State<'_, AppState>,
) -> Result<TranscribedSegmentV1, CommandErrorV1> {
    RATE_LIMITER
        .check("transcribe_segment")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("transcribe_segment"))?;
    if let Some(ref id) = segment_id {
        validate::validate_identifier(id).map_err(|_| crate::ipc_contract::invalid_segment_id_error())?;
    }
    validate::validate_file_path(&audio_path).map_err(|_| crate::ipc_contract::invalid_audio_path_error())?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj).map_err(|_| crate::ipc_contract::invalid_alignment_error())?;
    }
    // Clone the pipeline (Arc-wrapped internals) so the global pipeline mutex is released before the
    // possibly-long WSL/ONNX transcription, and run it OFF the main thread so the UI stays responsive.
    let pipeline = state.lock_pipeline().clone();
    let database = state.db_runtime();
    let history = Arc::clone(&state.history);
    let result = run_blocking(move || {
        let mutation = database.begin_mutation()?;
        let id = segment_id.as_deref().ok_or_else(|| {
            "E_TRANSCRIPTION_SOURCE_UNBOUND: an imported segment id is required for transcription".to_string()
        })?;
        let source = pipeline
            .bind_existing_transcription_source(id, Some(&audio_path), alignment_json.as_deref())
            .map_err(|error| error.to_string())?;
        // Inference is side-effect free here. The command owns the one later database transaction so
        // transcript/provenance/hypotheses and both exact Undo endpoints are captured atomically.
        let draft = pipeline.transcribe_bound_draft_only(&source, None).map_err(|e| e.to_string())?;
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
        let prepared = pipeline.prepare_bound_champion_draft(draft).map_err(|e| e.to_string())?;
        // Alignment is transcript-dependent machine truth. Compute configured auto-alignment before
        // publication so a failure leaves the old transcript/timings untouched; otherwise remove old
        // word timings while retaining the immutable source span. The database commits this alignment
        // endpoint with the transcript and exact Undo evidence below.
        let (replacement_alignment_json, replacement_alignment_quality) = if pipeline.settings_snapshot().auto_align {
            let alignment_text = prospective_champion_review_text(source.segment(), &prepared.raw_transcript);
            let (words, quality) = pipeline
                .align(
                    &source.segment().audio_path,
                    alignment_text,
                    source.segment().alignment_json.as_deref(),
                )
                .map_err(|error| format!("Automatic alignment failed before transcription commit: {error}"))?;
            if words.is_empty() {
                (
                    crate::chunking::without_word_timestamps(source.segment().alignment_json.as_deref()),
                    None,
                )
            } else {
                (
                    Some(merge_bound_transcription_words(source.segment(), &words)?),
                    Some(quality.as_db_str().to_string()),
                )
            }
        } else {
            (
                crate::chunking::without_word_timestamps(source.segment().alignment_json.as_deref()),
                None,
            )
        };
        let decoder_config_sha256 = pipeline.champion_transcription_config_sha256().map_err(|e| e.to_string())?;
        let champion = crate::db::SegmentHypothesis {
            segment_id: source.segment().id.clone(),
            model_id: prepared.model_version_id.clone(),
            transcript: prepared.raw_transcript.clone(),
            confidence: prepared.confidence,
        };
        {
            let db = database.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| {
                tracing::warn!("Recovering poisoned database lock during bound transcription commit");
                poisoned.into_inner()
            });
            let endpoints = db
                .commit_bound_champion_transcript_with_history(
                    &champion,
                    Some(&prepared.deployment_sha256),
                    prepared.normalized_transcript.as_deref(),
                    prepared.confidence_source.as_deref(),
                    prepared.cloud_call,
                    &decoder_config_sha256,
                    prepared.normalizer_version.as_deref(),
                    replacement_alignment_json.as_deref(),
                    replacement_alignment_quality.as_deref(),
                    source.snapshot(),
                )
                .map_err(|error| format!("Failed to commit champion transcript: {error}"))?;
            let Some((previous, current)) = endpoints else {
                return Err(format!(
                    "Segment {} gained a human decision while the champion was running; its reviewed transcript was not overwritten",
                    source.segment().id
                ));
            };
            // Build the response from the endpoint SQLite actually committed, not from the
            // pre-transaction inference object. The Verbatim-Law review projection and persisted
            // provenance are therefore byte-for-byte identical after a reload, including when the
            // IPC success response is delayed; normalized/refined text remains separate evidence.
            let response = TranscribedSegmentV1::from_committed_segment(
                &current.segment,
                machine_review_text(&current.segment).to_string(),
            );
            // Keep the serialized database lock until the exact history endpoint is appended. The
            // Undo/Redo path and every mutation that holds both locks use this DB -> history order;
            // releasing DB first would let a later mutation enter history before this earlier commit,
            // making the very next Undo stale or chronologically wrong.
            history
                .lock()
                .unwrap_or_else(|poisoned| {
                    tracing::warn!("Recovering poisoned history lock after bound transcription commit");
                    poisoned.into_inner()
                })
                .record_machine_transcription(previous, current)
                .map_err(|error| format!("Champion committed but exact Undo authority could not be recorded: {error}"))?;
            Ok(response)
        }
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner transcription command failed: {error}");
        crate::ipc_contract::public_transcription_error(&error)
    })
}

#[tauri::command]
#[specta::specta]
pub async fn align_segment(
    audio_path: String,
    text: String,
    alignment_json: Option<String>,
    segment_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<WordTimestampV1>, CommandErrorV1> {
    RATE_LIMITER
        .check("align_segment")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("align_segment"))?;
    validate::validate_file_path(&audio_path).map_err(|_| crate::ipc_contract::invalid_audio_path_error())?;
    if let Some(ref aj) = alignment_json {
        validate::validate_alignment_json(aj).map_err(|_| crate::ipc_contract::invalid_alignment_error())?;
    }
    if let Some(ref id) = segment_id {
        validate::validate_identifier(id).map_err(|_| crate::ipc_contract::invalid_segment_id_error())?;
    }
    if text.trim().is_empty() {
        return Err(crate::ipc_contract::invalid_alignment_text_error());
    }
    validate::validate_text(&text, 100000, "Alignment text")
        .map_err(|_| crate::ipc_contract::invalid_alignment_text_error())?;
    // Clone the pipeline OUT of the lock so the global mutex is released before the slow decode +
    // ONNX forced alignment, and run the whole align + persist OFF the main thread — holding either
    // lock (or blocking the UI thread) serializes every other pipeline command (get_import_status
    // polling, transcribe, get_waveform) for the whole alignment. ProcessingPipeline is Clone; align
    // takes &self.
    let pipeline = state.lock_pipeline().clone();
    let database = state.db_runtime();
    let result = run_blocking(move || {
        let mutation = database.begin_mutation()?;
        let stored_segment = if let Some(ref id) = segment_id {
            let db_guard = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
            db_guard
                .get_segment_by_id_with_revision(id)
                .map_err(|error| format!("Failed to reload segment {id} before alignment: {error}"))?
        } else {
            None
        };
        let (audio_path, text, alignment_json, expected_revision) =
            canonical_alignment_inputs(audio_path, text, alignment_json, stored_segment, segment_id.as_deref())?;
        validate::validate_file_path(&audio_path)?;
        validate::validate_text(&text, 100000, "Alignment text")?;
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
                let expected_revision =
                    expected_revision.ok_or_else(|| format!("Segment {id} alignment lost its revision authority"))?;
                let merged = crate::chunking::merge_word_timestamps(alignment_json.as_deref(), &timestamps);
                let db = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
                let persisted = db
                    .update_segment_alignment_if_unchanged(
                        id,
                        expected_revision,
                        alignment_json.as_deref(),
                        &merged,
                        quality.as_db_str(),
                    )
                    .map_err(|error| format!("Failed to persist word timings + quality for {id}: {error}"))?;
                if !persisted {
                    return Err(format!("Segment {id} changed while alignment was running; reload it and try again"));
                }
            }
        }
        Ok(timestamps.into_iter().map(WordTimestampV1::from).collect())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner alignment command failed: {error}");
        crate::ipc_contract::public_alignment_error(&error)
    })
}

#[tauri::command]
#[specta::specta]
pub async fn rediarize_segments(ids: Vec<String>, state: State<'_, AppState>) -> Result<usize, CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("rediarize_segments")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("rediarize_segments"))?;
    for id in &ids {
        validate::validate_identifier(id).map_err(|_| crate::ipc_contract::invalid_segment_id_error())?;
    }
    let mutation = super::begin_mutation().map_err(|error| {
        tracing::warn!("Owner rediarization admission failed: {error}");
        crate::ipc_contract::public_rediarization_error(&error)
    })?;
    // Clone the pipeline and let it open its own DB connection, so neither the global pipeline nor
    // db mutex is held across the per-file decode + diarization-inference loop, and run it OFF the
    // main thread so the UI stays responsive for the decode duration.
    let pipeline = state.lock_pipeline().clone();
    let result = run_blocking(move || {
        let _mutation = mutation;
        pipeline.rediarize_segments(&ids).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner rediarization command failed: {error}");
        crate::ipc_contract::public_rediarization_error(&error)
    })
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
    use super::{
        canonical_alignment_inputs, machine_review_text, merge_bound_transcription_words,
        prospective_champion_review_text,
    };
    use crate::aligner::WordTimestamp;
    use crate::db::SpeechSegment;

    #[test]
    fn alignment_with_segment_id_uses_stored_chunk_metadata() {
        let stored_json = r#"{"source_start_ms":12000,"source_end_ms":13000,"chunk_index":2,"chunk_count":4}"#;
        let stored = SpeechSegment {
            id: "seg-1".to_string(),
            audio_path: "C:\\audio\\book.wav".to_string(),
            raw_transcript: "authoritative words".to_string(),
            alignment_json: Some(stored_json.to_string()),
            ..SpeechSegment::default()
        };

        let (_, text, alignment, revision) = canonical_alignment_inputs(
            stored.audio_path.clone(),
            "authoritative words".into(),
            None,
            Some((stored, 7)),
            Some("seg-1"),
        )
        .expect("stored segment should be canonical");

        assert_eq!(text, "authoritative words");
        assert_eq!(alignment.as_deref(), Some(stored_json));
        assert_eq!(revision, Some(7));
    }

    #[test]
    fn alignment_rejects_a_stale_audio_path_for_a_segment_id() {
        let stored = SpeechSegment {
            id: "seg-1".to_string(),
            audio_path: "C:\\audio\\current.wav".to_string(),
            raw_transcript: "authoritative words".to_string(),
            ..SpeechSegment::default()
        };

        let error = canonical_alignment_inputs(
            "C:\\audio\\stale.wav".to_string(),
            "authoritative words".into(),
            None,
            Some((stored, 3)),
            Some("seg-1"),
        )
        .expect_err("stale caller path must fail closed");

        assert!(error.contains("audio path changed"));
    }

    #[test]
    fn alignment_rejects_non_authoritative_or_stale_caller_text() {
        let stored = SpeechSegment {
            id: "seg-1".to_string(),
            audio_path: "C:\\audio\\current.wav".to_string(),
            raw_transcript: "champion raw".to_string(),
            normalized_transcript: Some("normalized machine evidence".to_string()),
            ..SpeechSegment::default()
        };

        let error = canonical_alignment_inputs(
            stored.audio_path.clone(),
            "normalized machine evidence".into(),
            None,
            Some((stored, 4)),
            Some("seg-1"),
        )
        .expect_err("derived or stale text must never receive durable timings");

        assert!(error.contains("transcript changed"));
    }

    #[test]
    fn whole_file_word_alignment_gains_an_explicit_retriable_source_span() {
        let segment = SpeechSegment {
            id: "whole-file".into(),
            audio_path: "C:\\audio\\whole.wav".into(),
            duration_ms: 1_250,
            alignment_json: None,
            ..SpeechSegment::default()
        };
        let merged = merge_bound_transcription_words(
            &segment,
            &[WordTimestamp { word: "test".into(), start: 0.1, end: 0.4, confidence: 0.9 }],
        )
        .expect("whole-file source can be made explicit");
        let meta =
            crate::chunking::SegmentSourceMeta::from_alignment_json(&merged).expect("source span remains usable");
        assert_eq!(meta.source_start_ms, 0);
        assert_eq!(meta.source_end_ms, 1_250);
        assert_eq!(meta.chunk_index, 0);
        assert_eq!(meta.chunk_count, 1);
        assert_eq!(crate::chunking::word_timestamps_from_alignment(&merged).unwrap().len(), 1);
    }

    #[test]
    fn command_response_text_never_promotes_derived_machine_evidence() {
        let mut segment = SpeechSegment { raw_transcript: "champion raw".into(), ..SpeechSegment::default() };
        assert_eq!(machine_review_text(&segment), "champion raw");
        segment.normalized_transcript = Some("configured final".into());
        assert_eq!(machine_review_text(&segment), "champion raw");
        segment.annotated_transcript = Some("human draft".into());
        assert_eq!(machine_review_text(&segment), "human draft");
    }

    #[test]
    fn automatic_alignment_uses_the_same_verbatim_review_baseline() {
        let mut segment = SpeechSegment {
            raw_transcript: "old raw".into(),
            normalized_transcript: Some("machine paraphrase".into()),
            ..SpeechSegment::default()
        };
        assert_eq!(prospective_champion_review_text(&segment, "new champion raw"), "new champion raw");
        segment.annotated_transcript = Some("human draft".into());
        assert_eq!(prospective_champion_review_text(&segment, "new champion raw"), "human draft");
    }

    #[test]
    fn whitespace_only_human_draft_never_becomes_the_alignment_baseline() {
        let segment = SpeechSegment {
            raw_transcript: "old raw".into(),
            annotated_transcript: Some("   \n".into()),
            ..SpeechSegment::default()
        };
        assert_eq!(prospective_champion_review_text(&segment, "new champion raw"), "new champion raw");
    }

    #[test]
    fn alignment_without_a_segment_id_passes_caller_inputs_through() {
        let (audio, text, alignment, revision) = canonical_alignment_inputs(
            "C:\\audio\\adhoc.wav".to_string(),
            "ad hoc words".to_string(),
            Some(r#"{"words":[]}"#.to_string()),
            None,
            None,
        )
        .expect("id-less alignment stays a caller-scoped operation");
        assert_eq!(audio, "C:\\audio\\adhoc.wav");
        assert_eq!(text, "ad hoc words");
        assert_eq!(alignment.as_deref(), Some(r#"{"words":[]}"#));
        assert_eq!(revision, None, "no segment id means no revision authority to enforce");
    }

    #[test]
    fn alignment_rejects_a_vanished_segment_id() {
        let error = canonical_alignment_inputs(
            "C:\\audio\\gone.wav".to_string(),
            "any words".to_string(),
            None,
            None,
            Some("seg-gone"),
        )
        .expect_err("a deleted segment must not silently align as an ad-hoc file");
        assert!(error.contains("no longer exists"), "unexpected error: {error}");
        assert!(error.contains("seg-gone"), "refusal must name the segment: {error}");
    }

    #[test]
    fn alignment_rejects_a_segment_with_no_authoritative_transcript() {
        let stored = SpeechSegment {
            id: "seg-blank".to_string(),
            audio_path: "C:\\audio\\blank.wav".to_string(),
            raw_transcript: "   ".to_string(),
            ..SpeechSegment::default()
        };
        let error = canonical_alignment_inputs(
            stored.audio_path.clone(),
            "   ".to_string(),
            None,
            Some((stored, 1)),
            Some("seg-blank"),
        )
        .expect_err("a blank authority must never acquire durable word timings");
        assert!(error.contains("no authoritative transcript"), "unexpected error: {error}");
    }

    #[test]
    fn word_timings_refuse_a_whole_file_segment_without_a_positive_duration() {
        let segment = SpeechSegment {
            id: "no-duration".into(),
            audio_path: "C:\\audio\\short.wav".into(),
            duration_ms: 0,
            alignment_json: None,
            ..SpeechSegment::default()
        };
        let error = merge_bound_transcription_words(
            &segment,
            &[WordTimestamp { word: "x".into(), start: 0.0, end: 0.2, confidence: 0.5 }],
        )
        .expect_err("offset-less word timings would clobber chunk identity");
        assert!(error.contains("no positive stored duration"), "unexpected error: {error}");
        assert!(error.contains("no-duration"), "refusal must name the segment: {error}");
    }

    #[test]
    fn word_timings_merge_into_the_stored_chunk_identity_when_present() {
        let stored_json = r#"{"source_start_ms":12000,"source_end_ms":13000,"chunk_index":2,"chunk_count":4}"#;
        let segment = SpeechSegment {
            id: "chunked".into(),
            audio_path: "C:\\audio\\book.wav".into(),
            duration_ms: 1_000,
            alignment_json: Some(stored_json.to_string()),
            ..SpeechSegment::default()
        };
        let merged = merge_bound_transcription_words(
            &segment,
            &[WordTimestamp { word: "wusha".into(), start: 0.2, end: 0.6, confidence: 0.8 }],
        )
        .expect("stored chunk identity is the merge base");
        let meta =
            crate::chunking::SegmentSourceMeta::from_alignment_json(&merged).expect("chunk identity survives merging");
        assert_eq!(meta.source_start_ms, 12_000);
        assert_eq!(meta.source_end_ms, 13_000);
        assert_eq!(meta.chunk_index, 2);
        assert_eq!(meta.chunk_count, 4);
        let words = crate::chunking::word_timestamps_from_alignment(&merged).expect("fresh words persisted");
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].word, "wusha");
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().expect("build test runtime").block_on(future)
    }

    #[test]
    fn check_audio_fails_closed_on_a_missing_file() {
        let missing = std::env::temp_dir().join("cortex-check-audio-definitely-missing.wav");
        let error = block_on(super::check_audio(missing.to_string_lossy().into_owned()))
            .expect_err("a nonexistent path must never report audio info");
        assert!(error.contains("Invalid path"), "unexpected error: {error}");
    }

    #[test]
    fn check_audio_reports_the_real_identity_of_a_wav() {
        let dir = tempfile::tempdir().expect("temp dir");
        let wav = dir.path().join("probe.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).expect("create wav");
        for n in 0..800u32 {
            // A ramp, not silence: a wrong byte offset shows up as a wrong value downstream.
            writer.write_sample(((n % 320) as i16).wrapping_mul(90)).expect("write sample");
        }
        writer.finalize().expect("finalize wav");
        // Settle loop: write-then-immediately-read flakes on this Windows box (memory:
        // windows-fs-test-write-then-read-flaky).
        for _ in 0..50 {
            if std::fs::metadata(&wav).map(|m| m.len() > 44).unwrap_or(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let info = block_on(super::check_audio(wav.to_string_lossy().into_owned())).expect("valid wav reports info");
        assert_eq!(info["sample_rate"], 16_000);
        assert_eq!(info["channels"], 1);
        assert_eq!(info["format"], "wav");
        // 800 mono frames at 16 kHz are exactly 50 ms.
        assert_eq!(info["duration_ms"], 50);
    }
}

/// Wave-4 state-boundary coverage for the transcription/alignment `#[tauri::command]` wrappers,
/// invoked through a genuine managed `State<'_, AppState>`. Deliberately only the arms that refuse
/// BEFORE any ONNX/WSL inference: a unit test must never depend on a downloaded model, and the
/// champion is a hard stop by canon, not something a test may stub.
#[cfg(test)]
mod state_command_surface_tests {
    use super::*;
    use crate::test_support::managed_app_state;
    use tauri::Manager;

    type MockApp = tauri::App<tauri::test::MockRuntime>;

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().expect("build test runtime").block_on(future)
    }

    /// A real 50 ms mono 16 kHz WAV, so `validate_file_path` passes and any decode is trivial.
    fn probe_wav(dir: &std::path::Path, name: &str) -> String {
        let path = dir.join(name);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("create wav");
        for n in 0..800u32 {
            writer.write_sample(((n % 320) as i16).wrapping_mul(90)).expect("write sample");
        }
        writer.finalize().expect("finalize wav");
        crate::test_support::await_stable_fixture(&path);
        path.to_string_lossy().into_owned()
    }

    fn seed_segment(app: &MockApp, id: &str, audio_path: &str, transcript: &str) {
        app.state::<AppState>()
            .lock_db()
            .insert_segment(&crate::db::SpeechSegment {
                id: id.into(),
                audio_path: audio_path.into(),
                raw_transcript: transcript.into(),
                duration_ms: 50,
                ..crate::db::SpeechSegment::default()
            })
            .unwrap();
    }

    type Action = crate::ipc_contract::SuggestedActionV1;

    fn expect_error<T>(result: Result<T, CommandErrorV1>, code: &str, action: Option<Action>) {
        let error = match result {
            Ok(_) => panic!("expected {code}, got a success"),
            Err(error) => error,
        };
        assert_eq!(error.code, code);
        assert!(!error.retryable, "{code} is a caller-repairable input, never a retry");
        assert_eq!(error.suggested_action, action, "{code} suggested action");
    }

    /// Transcription is bound to an imported clip. Every argument is validated at the boundary, and
    /// an id-less request is refused with the reload affordance instead of transcribing a loose file.
    #[test]
    fn transcribe_segment_validates_its_arguments_and_refuses_an_unbound_clip() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let wav = probe_wav(tmp.path(), "bound.wav");

        expect_error(
            block_on(transcribe_segment(Some("../evil".into()), wav.clone(), None, app.state())),
            "INVALID_SEGMENT_ID",
            None,
        );
        expect_error(
            block_on(transcribe_segment(None, "Z:\\nope\\missing.wav".into(), None, app.state())),
            "INVALID_AUDIO_PATH",
            None,
        );
        expect_error(
            block_on(transcribe_segment(None, wav.clone(), Some("{not json".into()), app.state())),
            "INVALID_ALIGNMENT",
            Some(Action::ReloadClip),
        );
        // A valid file with no segment id is the E_TRANSCRIPTION_SOURCE_UNBOUND arm: the champion
        // never drafts a clip that has no durable identity to commit the provenance against.
        expect_error(
            block_on(transcribe_segment(None, wav, None, app.state())),
            "TRANSCRIPTION_SOURCE_UNBOUND",
            Some(Action::ReloadClip),
        );
    }

    /// Alignment validates audio path, timing JSON, id and text before any decode. The size limit is
    /// 100 000 chars; one over must be refused as text, not truncated.
    #[test]
    fn align_segment_validates_every_argument_before_decoding_audio() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let wav = probe_wav(tmp.path(), "align.wav");

        expect_error(
            block_on(align_segment("Z:\\nope\\missing.wav".into(), "words".into(), None, None, app.state())),
            "INVALID_AUDIO_PATH",
            None,
        );
        expect_error(
            block_on(align_segment(wav.clone(), "words".into(), Some("{not json".into()), None, app.state())),
            "INVALID_ALIGNMENT",
            Some(Action::ReloadClip),
        );
        expect_error(
            block_on(align_segment(wav.clone(), "words".into(), None, Some("../evil".into()), app.state())),
            "INVALID_SEGMENT_ID",
            None,
        );
        expect_error(
            block_on(align_segment(wav.clone(), "   \n\t".into(), None, None, app.state())),
            "INVALID_ALIGNMENT_TEXT",
            None,
        );
        expect_error(
            block_on(align_segment(wav, "x".repeat(100_001), None, None, app.state())),
            "INVALID_ALIGNMENT_TEXT",
            None,
        );
    }

    /// Word timings are durable evidence about ONE transcript. When the caller names a segment, the
    /// database row is the authority: a vanished clip, a changed source path and a stale transcript
    /// each fail closed, and nothing is written.
    #[test]
    fn align_segment_fails_closed_on_a_vanished_or_edited_clip_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let wav = probe_wav(tmp.path(), "stored.wav");
        let other_wav = probe_wav(tmp.path(), "elsewhere.wav");
        seed_segment(&app, "seg-align", &wav, "authoritative words");

        expect_error(
            block_on(align_segment(wav.clone(), "words".into(), None, Some("seg-gone".into()), app.state())),
            "SEGMENT_NOT_FOUND",
            Some(Action::ReloadClip),
        );
        expect_error(
            block_on(align_segment(
                other_wav,
                "authoritative words".into(),
                None,
                Some("seg-align".into()),
                app.state(),
            )),
            "ALIGNMENT_SOURCE_CHANGED",
            Some(Action::ReloadClip),
        );
        expect_error(
            block_on(align_segment(wav, "stale caller text".into(), None, Some("seg-align".into()), app.state())),
            "ALIGNMENT_SOURCE_CHANGED",
            Some(Action::ReloadClip),
        );

        let stored = app.state::<AppState>().lock_db().get_segment_by_id("seg-align").unwrap().expect("clip survives");
        assert_eq!(stored.alignment_json, None, "a refused alignment must never persist timings");
        assert_eq!(stored.raw_transcript, "authoritative words", "and must never touch the transcript");
    }

    #[test]
    fn rediarize_segments_validates_ids_and_accepts_an_empty_request() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let refused = match block_on(rediarize_segments(vec!["ok-id".into(), "../evil".into()], app.state())) {
            Ok(_) => panic!("one bad id must refuse the whole batch"),
            Err(error) => error,
        };
        assert_eq!(refused.code, "INVALID_SEGMENT_ID");
        assert!(!refused.retryable);

        let none = block_on(rediarize_segments(vec![], app.state())).expect("an empty request is a no-op");
        assert_eq!(none, 0, "no ids means no clips re-diarized");
    }
}
