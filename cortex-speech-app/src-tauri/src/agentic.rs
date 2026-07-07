use crate::chunking;
use crate::db::{SegmentHypothesis, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::{audio, gemini_api};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

const INLINE_AUDIO_LIMIT_BYTES: u64 = 20 * 1024 * 1024;
const FILES_API_MAX_AUDIO_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SourceTranscriptArtifact {
    pub audio_path: String,
    pub model_id: String,
    pub transcript_path: String,
    pub transcript_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateSelectionScore {
    pub model_id: String,
    pub transcript: String,
    pub final_score: f64,
    pub reference_window_overlap: f64,
    pub reference_global_overlap: f64,
    pub text_quality: f64,
    pub model_prior: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceAgreementReport {
    pub reference_model_id: Option<String>,
    pub selected_model_id: String,
    pub selected_transcript: String,
    pub selected_score: f64,
    pub confidence: f64,
    pub margin: f64,
    pub should_commit: bool,
    pub reference_window_overlap: f64,
    pub reference_global_overlap: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateSelectionReport {
    pub reference_model_id: Option<String>,
    pub selected_model_id: String,
    pub selected_transcript: String,
    pub selected_score: f64,
    pub confidence: f64,
    pub margin: f64,
    pub should_commit: bool,
    pub rationale: String,
    pub reference_window_preview: String,
    pub scores: Vec<CandidateSelectionScore>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_agreement: Vec<ReferenceAgreementReport>,
}

pub fn generate_whole_file_reference_transcript(
    audio_path: &Path,
    model: &str,
    api_key: &str,
    output_dir: &Path,
) -> Result<SourceTranscriptArtifact, String> {
    let transcript = transcribe_audio_with_gemini(audio_path, model, api_key)?;
    if !is_usable_source_reference_transcript(&transcript) {
        return Err("Gemini returned an empty or unusable whole-file reference transcript".to_string());
    }
    let transcript_path = write_reference_text_file(audio_path, model, &transcript, output_dir)?;
    Ok(SourceTranscriptArtifact {
        audio_path: audio_path.to_string_lossy().to_string(),
        model_id: model.to_string(),
        transcript_path: transcript_path.to_string_lossy().to_string(),
        transcript_text: transcript,
    })
}

pub fn is_usable_source_reference_transcript(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with("[ASR unavailable") || trimmed.starts_with("[Pending") {
        return false;
    }

    let lower = trimmed.to_lowercase();
    ![
        "cannot transcribe",
        "can't transcribe",
        "unable to transcribe",
        "i am unable",
        "i'm unable",
        "as an ai",
        "sorry,",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

pub fn select_best_candidate_against_reference(
    segment: &SpeechSegment,
    reference_text: &str,
    source_duration_ms: Option<i64>,
    candidates: &[SegmentHypothesis],
) -> Option<CandidateSelectionReport> {
    let reference_tokens = tokenize_for_reference(reference_text);
    if reference_tokens.is_empty() {
        return None;
    }

    let window_tokens = reference_window_tokens(segment, &reference_tokens, source_duration_ms);
    let scoring_window = if window_tokens.is_empty() { &reference_tokens } else { &window_tokens };

    let mut scores: Vec<CandidateSelectionScore> = candidates
        .iter()
        .filter(|candidate| !candidate.transcript.trim().is_empty())
        .map(|candidate| {
            let candidate_tokens = tokenize_for_reference(&candidate.transcript);
            let reference_window_overlap = token_overlap_ratio(&candidate_tokens, scoring_window);
            let reference_global_overlap = token_overlap_ratio(&candidate_tokens, &reference_tokens);
            let text_quality = transcript_text_quality(&candidate.transcript);
            let model_prior = model_prior(candidate);
            let exact_window_bonus = exact_normalized_match(&candidate_tokens, scoring_window);
            let final_score = (0.52 * reference_window_overlap
                + 0.18 * reference_global_overlap
                + 0.15 * text_quality
                + 0.15 * model_prior
                + exact_window_bonus)
                .clamp(0.0, 1.0);

            CandidateSelectionScore {
                model_id: candidate.model_id.clone(),
                transcript: candidate.transcript.trim().to_string(),
                final_score,
                reference_window_overlap,
                reference_global_overlap,
                text_quality,
                model_prior,
            }
        })
        .collect();

    if scores.is_empty() {
        return None;
    }

    scores.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.reference_window_overlap.partial_cmp(&a.reference_window_overlap).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.model_prior.partial_cmp(&a.model_prior).unwrap_or(std::cmp::Ordering::Equal))
    });

    let winner = scores[0].clone();
    let runner_up_score = scores.get(1).map(|score| score.final_score).unwrap_or(0.0);
    let margin = (winner.final_score - runner_up_score).max(0.0);
    let should_commit = winner.final_score >= 0.72 && margin >= 0.08 && winner.reference_window_overlap >= 0.45;
    let confidence =
        if should_commit { (winner.final_score + margin.min(0.2)).clamp(0.0, 1.0) } else { winner.final_score };

    Some(CandidateSelectionReport {
        reference_model_id: None,
        selected_model_id: winner.model_id.clone(),
        selected_transcript: winner.transcript.clone(),
        selected_score: winner.final_score,
        confidence,
        margin,
        should_commit,
        rationale: if should_commit {
            format!(
                "Reference-aware agent selected '{}' using source-window overlap {:.2}, global overlap {:.2}, and margin {:.2}.",
                winner.model_id, winner.reference_window_overlap, winner.reference_global_overlap, margin
            )
        } else {
            format!(
                "Reference-aware agent preferred '{}' but kept the segment in jury review because score {:.2}, overlap {:.2}, or margin {:.2} was below the commit gate.",
                winner.model_id, winner.final_score, winner.reference_window_overlap, margin
            )
        },
        reference_window_preview: preview_tokens(scoring_window, 36),
        scores,
        reference_agreement: Vec::new(),
    })
}

fn transcribe_audio_with_gemini(audio_path: &Path, model: &str, api_key: &str) -> Result<String, String> {
    if api_key.trim().is_empty() {
        return Err("Gemini API key is missing".to_string());
    }
    let bytes = std::fs::metadata(audio_path)
        .map_err(|e| format!("Cannot inspect audio file '{}': {e}", audio_path.display()))?
        .len();
    if bytes == 0 {
        return Err("Cannot transcribe an empty audio file".to_string());
    }
    if bytes > FILES_API_MAX_AUDIO_BYTES {
        return Err(format!(
            "Audio file is too large for Gemini Files API upload ({} MB > 2048 MB)",
            bytes / 1024 / 1024
        ));
    }

    let mime_type = guess_audio_mime_type(audio_path);
    let prompt = whole_file_reference_prompt();
    if bytes <= INLINE_AUDIO_LIMIT_BYTES {
        transcribe_inline_audio(audio_path, model, api_key, mime_type, prompt)
    } else {
        transcribe_uploaded_audio(audio_path, model, api_key, mime_type, prompt)
    }
}

fn transcribe_inline_audio(
    audio_path: &Path,
    model: &str,
    api_key: &str,
    mime_type: &str,
    prompt: &str,
) -> Result<String, String> {
    let bytes =
        std::fs::read(audio_path).map_err(|e| format!("Cannot read audio file '{}': {e}", audio_path.display()))?;
    let payload = json!({
        "systemInstruction": {
            "parts": [{ "text": whole_file_reference_system_prompt() }]
        },
        "contents": [{
            "parts": [
                {
                    "inlineData": {
                        "mimeType": mime_type,
                        "data": base64_encode(&bytes)
                    }
                },
                { "text": prompt }
            ]
        }],
        "generationConfig": {
            "temperature": 0.0
        }
    });

    let url = gemini_api::generate_content_url(model);
    let response = gemini_api::with_api_key(crate::http::API_AGENT.post(&url), api_key)
        .set("Content-Type", "application/json")
        .send_json(payload)
        .map_err(|e| format!("Gemini inline audio request failed: {}", redact_for_user(e, api_key)))?;
    let body: Value = crate::http::json_bounded(response)
        .map_err(|e| format!("Failed to parse Gemini inline audio response: {e}"))?;
    extract_gemini_text(&body)
}

fn transcribe_uploaded_audio(
    audio_path: &Path,
    model: &str,
    api_key: &str,
    mime_type: &str,
    prompt: &str,
) -> Result<String, String> {
    let uploaded = upload_gemini_file(audio_path, api_key, mime_type)?;
    let result = generate_from_uploaded_file(&uploaded.file_uri, mime_type, model, api_key, prompt);
    if let Err(error) = delete_gemini_file(&uploaded.name, api_key) {
        tracing::warn!("Failed to delete temporary Gemini file {}: {}", uploaded.name, error);
    }
    result
}

struct UploadedGeminiFile {
    name: String,
    file_uri: String,
}

fn upload_gemini_file(audio_path: &Path, api_key: &str, mime_type: &str) -> Result<UploadedGeminiFile, String> {
    let size = std::fs::metadata(audio_path)
        .map_err(|e| format!("Cannot inspect audio file '{}': {e}", audio_path.display()))?
        .len();
    let display_name = audio_path.file_name().and_then(|name| name.to_str()).unwrap_or("audio");
    let start_payload = json!({ "file": { "display_name": display_name } });

    let start_response = gemini_api::with_api_key(
        crate::http::API_AGENT.post("https://generativelanguage.googleapis.com/upload/v1beta/files"),
        api_key,
    )
    .set("X-Goog-Upload-Protocol", "resumable")
    .set("X-Goog-Upload-Command", "start")
    .set("X-Goog-Upload-Header-Content-Length", &size.to_string())
    .set("X-Goog-Upload-Header-Content-Type", mime_type)
    .set("Content-Type", "application/json")
    .send_json(start_payload)
    .map_err(|e| format!("Gemini file upload start failed: {}", redact_for_user(e, api_key)))?;

    let upload_url = start_response
        .header("x-goog-upload-url")
        .or_else(|| start_response.header("X-Goog-Upload-URL"))
        .ok_or_else(|| "Gemini file upload did not return an upload URL".to_string())?
        .to_string();

    let file = std::fs::File::open(audio_path)
        .map_err(|e| format!("Cannot open audio file '{}': {e}", audio_path.display()))?;
    let upload_response = crate::http::API_AGENT
        .post(&upload_url)
        .set("Content-Length", &size.to_string())
        .set("X-Goog-Upload-Offset", "0")
        .set("X-Goog-Upload-Command", "upload, finalize")
        .send(file)
        .map_err(|e| format!("Gemini file upload failed: {}", redact_for_user(e, api_key)))?;

    let body: Value = crate::http::json_bounded(upload_response)
        .map_err(|e| format!("Failed to parse Gemini file upload response: {e}"))?;
    let file = body.get("file").ok_or_else(|| "Gemini upload response missing file metadata".to_string())?;
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "Gemini upload response missing file name".to_string())?
        .to_string();
    let file_uri = file
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| "Gemini upload response missing file URI".to_string())?
        .to_string();

    Ok(UploadedGeminiFile { name, file_uri })
}

fn generate_from_uploaded_file(
    file_uri: &str,
    mime_type: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> Result<String, String> {
    let payload = json!({
        "systemInstruction": {
            "parts": [{ "text": whole_file_reference_system_prompt() }]
        },
        "contents": [{
            "parts": [
                {
                    "fileData": {
                        "mimeType": mime_type,
                        "fileUri": file_uri
                    }
                },
                { "text": prompt }
            ]
        }],
        "generationConfig": {
            "temperature": 0.0
        }
    });

    let url = gemini_api::generate_content_url(model);
    let response = gemini_api::with_api_key(crate::http::API_AGENT.post(&url), api_key)
        .set("Content-Type", "application/json")
        .send_json(payload)
        .map_err(|e| format!("Gemini uploaded audio request failed: {}", redact_for_user(e, api_key)))?;
    let body: Value = crate::http::json_bounded(response)
        .map_err(|e| format!("Failed to parse Gemini uploaded audio response: {e}"))?;
    extract_gemini_text(&body)
}

fn delete_gemini_file(name: &str, api_key: &str) -> Result<(), String> {
    let path = if name.starts_with("files/") { name.to_string() } else { format!("files/{name}") };
    let url = format!("https://generativelanguage.googleapis.com/v1beta/{path}");
    // Route through the shared bounded agent (connect/read/write timeouts), like every sibling call in
    // this file. The bare global `ureq::delete` has no read/write timeout, so a server that accepts the
    // connection then stalls byte-silently would block this worker thread forever (see http.rs).
    // API_AGENT bounds it to timeout_read/write — this was the only remaining bare `ureq::` call site in
    // the tree.
    gemini_api::with_api_key(crate::http::API_AGENT.delete(&url), api_key)
        .call()
        .map_err(|e| format!("Gemini file delete failed: {}", redact_for_user(e, api_key)))?;
    Ok(())
}

fn extract_gemini_text(body: &Value) -> Result<String, String> {
    // Concatenate ALL text parts, not just parts[0]. Gemini can split one transcript across
    // content.parts[0..N] (and a 2.5-class "thinking" model can emit a leading thought part), so
    // reading parts[0] alone silently truncates the reference transcript — which then mis-scores every
    // local candidate downstream. Join every part's text to reconstruct the full response.
    let joined = body["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|ps| ps.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        return Err("Gemini response did not contain transcript text".to_string());
    }
    Ok(trimmed.to_string())
}

fn whole_file_reference_system_prompt() -> &'static str {
    "You are an expert Sorani Kurdish speech transcription specialist. Transcribe speech faithfully. Do not summarize."
}

fn whole_file_reference_prompt() -> &'static str {
    "Transcribe the entire audio file as Sorani Kurdish text. Preserve spoken order, names, repetitions that matter, and sentence boundaries. Output only the transcript text with no commentary, no timestamps, and no markdown."
}

fn write_reference_text_file(
    audio_path: &Path,
    model: &str,
    transcript: &str,
    output_dir: &Path,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create source transcript directory '{}': {e}", output_dir.display()))?;
    let stem = audio_path.file_stem().and_then(|name| name.to_str()).unwrap_or("audio");
    // Disambiguate by the FULL path: two audio files with the same stem in different directories
    // (e.g. seriesA/01.wav and seriesB/01.wav) must NOT share one reference-transcript file, or
    // importing one silently overwrites the other's reference — and a later re-import then rewrites
    // the first file's DB row to the second's text. A short hash of the full path keys them apart.
    let path_key = {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(audio_path.to_string_lossy().as_bytes());
        let mut s = String::with_capacity(12);
        for b in digest.iter().take(6) {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    };
    let filename =
        format!("{}.{}.{}.whole_file_reference.txt", sanitize_filename(stem), sanitize_filename(model), path_key);
    let path = output_dir.join(filename);
    std::fs::write(&path, transcript.trim())
        .map_err(|e| format!("Failed to write source transcript '{}': {e}", path.display()))?;
    Ok(path)
}

/// Decode the source file, extract ONLY this segment's audio window (via its alignment), and return
/// it as in-memory WAV bytes. This is the single source of truth for "the audio of one segment" —
/// any cloud egress (Gemini T2, ElevenLabs Scribe vote) must send this slice, NEVER the whole source
/// file, or it both leaks/processes unrelated audio and (for Scribe) stores a whole-file transcript
/// against a single segment. `decode_to_pcm` caches by content, so slicing many segments from one
/// source file decodes it once.
pub fn segment_audio_as_wav_bytes(segment: &SpeechSegment) -> AppResult<Vec<u8>> {
    let path = Path::new(&segment.audio_path);
    let duration_ms = audio::get_duration_ms(path)?;
    if duration_ms == 0 {
        return Err(AppError::Validation("Empty audio file".into()));
    }
    let timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);
    let (sample_rate, pcm) = audio::decode_to_pcm_with_timeout(path, timeout)?;
    let (sample_rate, pcm) = audio::ensure_pcm_16khz(sample_rate, pcm)?;
    if pcm.is_empty() {
        return Err(AppError::Audio(crate::error::AudioError::EmptyBuffer));
    }
    let (chunk_pcm, _) = chunking::slice_pcm_by_alignment(&pcm, sample_rate, segment.alignment_json.as_deref())?;
    pcm_i16_to_wav_bytes(&chunk_pcm, audio::TARGET_SAMPLE_RATE)
}

pub fn segment_audio_as_wav_base64(segment: &SpeechSegment) -> AppResult<String> {
    Ok(base64_encode(&segment_audio_as_wav_bytes(segment)?))
}

fn pcm_i16_to_wav_bytes(pcm: &[i16], sample_rate: u32) -> AppResult<Vec<u8>> {
    let spec =
        hound::WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| AppError::Other(format!("Failed to create in-memory WAV: {e}")))?;
        for &sample in pcm {
            writer
                .write_sample(sample)
                .map_err(|e| AppError::Other(format!("Failed to write in-memory WAV sample: {e}")))?;
        }
        writer.finalize().map_err(|e| AppError::Other(format!("Failed to finalize in-memory WAV: {e}")))?;
    }
    Ok(cursor.into_inner())
}

pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn guess_audio_mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_ascii_lowercase().as_str() {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "webm" => "audio/webm",
        "wav" | "wave" => "audio/wav",
        _ => "audio/wav",
    }
}

fn sanitize_filename(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        // Unicode `is_alphanumeric`, NOT `is_ascii_alphanumeric` — a Sorani stem (e.g. "کوردی") must
        // stay meaningful in the reference-file name, matching the export sanitizer's P3.7 behavior
        // (validation::input::sanitize_filename). The old ASCII-only form folded every Kurdish letter to
        // '_', producing "____"-style names for Sorani sources — inconsistent with the exported clips.
        if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "artifact".to_string()
    } else {
        trimmed.to_string()
    }
}

fn redact_for_user(error: ureq::Error, api_key: &str) -> String {
    crate::secret_redaction::redact_api_key(&error.to_string(), api_key)
}

fn tokenize_for_reference(text: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() || is_kurdish_arabic_char(ch) {
            normalized.extend(ch.to_lowercase());
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().map(ToOwned::to_owned).collect()
}

fn reference_window_tokens(
    segment: &SpeechSegment,
    reference_tokens: &[String],
    source_duration_ms: Option<i64>,
) -> Vec<String> {
    let Some(duration_ms) = source_duration_ms.filter(|duration| *duration > 0) else {
        return reference_tokens.to_vec();
    };
    let Some(meta) = segment.alignment_json.as_deref().and_then(chunking::SegmentSourceMeta::from_alignment_json)
    else {
        return reference_tokens.to_vec();
    };

    let token_count = reference_tokens.len();
    if token_count == 0 {
        return Vec::new();
    }

    let start_ratio = (meta.source_start_ms.max(0) as f64 / duration_ms as f64).clamp(0.0, 1.0);
    let end_ratio = (meta.source_end_ms.max(meta.source_start_ms) as f64 / duration_ms as f64).clamp(start_ratio, 1.0);
    let mut start = (start_ratio * token_count as f64).floor() as usize;
    let mut end = (end_ratio * token_count as f64).ceil() as usize;
    if end <= start {
        end = (start + 1).min(token_count);
    }

    let expected_len = end.saturating_sub(start).max(1);
    let pad = expected_len.max(8);
    start = start.saturating_sub(pad);
    end = (end + pad).min(token_count);
    reference_tokens[start..end].to_vec()
}

fn token_overlap_ratio(candidate_tokens: &[String], reference_tokens: &[String]) -> f64 {
    if candidate_tokens.is_empty() || reference_tokens.is_empty() {
        return 0.0;
    }
    let mut reference_counts: HashMap<&str, usize> = HashMap::new();
    for token in reference_tokens {
        *reference_counts.entry(token.as_str()).or_default() += 1;
    }

    let mut overlap = 0usize;
    for token in candidate_tokens {
        if let Some(count) = reference_counts.get_mut(token.as_str()) {
            if *count > 0 {
                *count -= 1;
                overlap += 1;
            }
        }
    }
    overlap as f64 / candidate_tokens.len() as f64
}

fn exact_normalized_match(candidate_tokens: &[String], reference_tokens: &[String]) -> f64 {
    if candidate_tokens.is_empty() || reference_tokens.is_empty() || candidate_tokens.len() > reference_tokens.len() {
        return 0.0;
    }
    if reference_tokens.windows(candidate_tokens.len()).any(|window| window == candidate_tokens) {
        0.08
    } else {
        0.0
    }
}

fn transcript_text_quality(text: &str) -> f64 {
    let trimmed = text.trim();
    if !is_usable_source_reference_transcript(trimmed) {
        return 0.0;
    }
    let total = trimmed.chars().filter(|ch| !ch.is_whitespace()).count();
    if total == 0 {
        return 0.0;
    }
    let kurdish_chars = trimmed.chars().filter(|ch| is_kurdish_arabic_char(*ch)).count();
    let script_ratio = kurdish_chars as f64 / total as f64;
    let length_score = (trimmed.split_whitespace().count() as f64 / 4.0).clamp(0.25, 1.0);
    (0.75 * script_ratio + 0.25 * length_score).clamp(0.0, 1.0)
}

fn is_kurdish_arabic_char(ch: char) -> bool {
    let value = ch as u32;
    (0x0600..=0x06FF).contains(&value) || (0x0750..=0x077F).contains(&value) || (0x08A0..=0x08FF).contains(&value)
}

fn model_prior(candidate: &SegmentHypothesis) -> f64 {
    let confidence = candidate.confidence.unwrap_or_else(|| {
        let model = candidate.model_id.to_lowercase();
        if model.contains("gemini") && model.contains("pro") {
            0.88
        } else if model.contains("gemini") {
            0.82
        } else if model.contains("7b") {
            0.80
        } else if model.contains("1b") {
            0.68
        } else if model.contains("300m") {
            0.55
        } else {
            0.50
        }
    });
    confidence.clamp(0.0, 1.0)
}

fn preview_tokens(tokens: &[String], max_tokens: usize) -> String {
    let mut preview = tokens.iter().take(max_tokens).cloned().collect::<Vec<_>>().join(" ");
    if tokens.len() > max_tokens {
        preview.push_str(" ...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sanitize_filename_keeps_sorani_letters_like_the_exporter() {
        // Aligns with validation::input::sanitize_filename (P3.7): Sorani/Arabic-script letters are
        // Unicode-alphanumeric and must be KEPT so a reference-file name stays meaningful; only unsafe
        // characters (spaces, path/reserved) become '_'. The old ASCII-only form mangled کوردی -> "____".
        assert_eq!(sanitize_filename("کوردی"), "کوردی", "Sorani letters preserved");
        assert_eq!(sanitize_filename("clip 01/bad:name"), "clip_01_bad_name", "unsafe chars -> _");
        assert_eq!(sanitize_filename("gesht-01.wav"), "gesht-01.wav", "safe punctuation kept");
        assert_eq!(sanitize_filename("!!!"), "artifact", "an all-unsafe stem still yields a usable name");
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn reference_file_name_is_stable_and_safe() {
        let tmp = TempDir::new().expect("tempdir");
        let audio = tmp.path().join("My Long Audio.wav");
        std::fs::write(&audio, b"fixture").expect("write fixture");

        let path = write_reference_text_file(&audio, "gemini-2.5/pro", "hello", tmp.path()).expect("write ref");

        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // Stem + sanitized model preserved; a 12-hex path key now disambiguates same-stem files.
        assert!(name.starts_with("My_Long_Audio.gemini-2.5_pro."), "{name}");
        assert!(name.ends_with(".whole_file_reference.txt"), "{name}");
        let key =
            name.trim_start_matches("My_Long_Audio.gemini-2.5_pro.").trim_end_matches(".whole_file_reference.txt");
        assert_eq!(key.len(), 12, "path key is 12 hex chars: {key}");
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()), "{key}");
        assert_eq!(std::fs::read_to_string(path).expect("read ref"), "hello");
    }

    #[test]
    fn reference_file_name_disambiguates_same_stem_different_dirs() {
        // Round-2 audit HIGH (cross-file corruption): same-stem audio in different dirs must produce
        // DISTINCT reference files (write_reference_text_file does not read the audio, only its path).
        let tmp = TempDir::new().expect("tempdir");
        let out = tmp.path();
        let a = tmp.path().join("seriesA").join("interview.wav");
        let b = tmp.path().join("seriesB").join("interview.wav");

        let pa = write_reference_text_file(&a, "gemini-2.5-pro", "TEXT_A", out).expect("write a");
        let pb = write_reference_text_file(&b, "gemini-2.5-pro", "TEXT_B", out).expect("write b");

        assert_ne!(pa, pb, "same-stem files in different dirs must not collide");
        assert_eq!(std::fs::read_to_string(&pa).unwrap(), "TEXT_A");
        assert_eq!(std::fs::read_to_string(&pb).unwrap(), "TEXT_B", "b must not have overwritten a");
    }

    #[test]
    fn source_reference_transcript_validator_rejects_empty_placeholders_and_refusals() {
        assert!(!is_usable_source_reference_transcript(""));
        assert!(!is_usable_source_reference_transcript("[ASR unavailable: model missing]"));
        assert!(!is_usable_source_reference_transcript("[Pending transcription]"));
        assert!(!is_usable_source_reference_transcript("Sorry, I am unable to transcribe this audio file."));
        assert!(is_usable_source_reference_transcript("دەقی دروستی سەرچاوەی دەنگەکە"));
    }

    #[test]
    fn segment_audio_base64_uses_alignment_window() {
        let tmp = TempDir::new().expect("tempdir");
        let wav = tmp.path().join("source.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).expect("create wav");
        for _ in 0..32000 {
            writer.write_sample(1000i16).expect("write sample");
        }
        writer.finalize().expect("finalize wav");

        let meta =
            chunking::SegmentSourceMeta { source_start_ms: 500, source_end_ms: 1500, chunk_index: 0, chunk_count: 1 };
        let segment = SpeechSegment {
            id: "seg".to_string(),
            audio_path: wav.to_string_lossy().to_string(),
            alignment_json: Some(meta.to_alignment_json()),
            ..SpeechSegment::default()
        };

        let encoded = segment_audio_as_wav_base64(&segment).expect("encode segment");
        assert!(encoded.starts_with("UklGR"), "WAV base64 should start with RIFF header");
        let decoded_len_estimate = encoded.len() * 3 / 4;
        assert!(
            (32_000..=33_000).contains(&decoded_len_estimate),
            "one second 16-bit PCM WAV should be about 32 KB, got {decoded_len_estimate}"
        );

        // Round-21 #1: the Scribe vote path must send the sliced SEGMENT window (1 s ≈ 32 KB), never
        // the whole 2 s source file (≈ 64 KB). Assert the raw WAV bytes are the slice, not the source.
        let bytes = segment_audio_as_wav_bytes(&segment).expect("encode segment bytes");
        assert!(bytes.starts_with(b"RIFF"), "raw WAV bytes start with the RIFF header");
        assert!(
            (32_000..=33_200).contains(&bytes.len()),
            "sliced 1 s segment WAV should be ~32 KB (NOT the ~64 KB whole 2 s file), got {}",
            bytes.len()
        );
    }

    fn hyp(model: &str, text: &str, confidence: f64) -> SegmentHypothesis {
        SegmentHypothesis {
            segment_id: "seg".to_string(),
            model_id: model.to_string(),
            transcript: text.to_string(),
            confidence: Some(confidence),
        }
    }

    #[test]
    fn reference_selection_prefers_candidate_inside_chunk_window() {
        let reference = "w01 w02 w03 w04 w05 w06 w07 w08 w09 w10 w11 w12 w13 w14 w15 \
            navenda sarata peyam rast drust w21 w22 w23 w24 w25 w26 w27 w28 w29 w30 \
            w31 w32 w33 w34 w35 w36 w37 w38 w39 w40";
        let meta =
            chunking::SegmentSourceMeta { source_start_ms: 3000, source_end_ms: 5000, chunk_index: 1, chunk_count: 4 };
        let segment = SpeechSegment {
            id: "seg".to_string(),
            audio_path: "source.wav".to_string(),
            alignment_json: Some(meta.to_alignment_json()),
            ..SpeechSegment::default()
        };
        let candidates = vec![hyp("weak", "w01 w02 w03", 0.95), hyp("omniasr-ctc-1b", "navenda sarata peyam", 0.70)];

        let report = select_best_candidate_against_reference(&segment, reference, Some(8000), &candidates)
            .expect("selection report");

        assert_eq!(report.selected_model_id, "omniasr-ctc-1b");
        assert!(report.should_commit);
    }

    #[test]
    fn reference_selection_does_not_commit_when_margin_is_weak() {
        let segment =
            SpeechSegment { id: "seg".to_string(), audio_path: "source.wav".to_string(), ..Default::default() };
        let candidates = vec![hyp("a", "navenda peyam", 0.70), hyp("b", "navenda peyam", 0.69)];

        let report =
            select_best_candidate_against_reference(&segment, "navenda peyam", None, &candidates).expect("report");

        assert!(!report.should_commit);
    }
}
