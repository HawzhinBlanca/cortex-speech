use crate::aligner;
use crate::asr;
use crate::audio;
use crate::cache::TranscriptCache;
use crate::cancel::CancellationToken;
use crate::chunking::{self, MAX_PCM_SAMPLES};
use crate::db::{Database, SegmentHypothesis, SourceTranscriptRecord, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::fingerprint::AudioFingerprint;
use crate::models::ModelManager;
use crate::normalizer::SoraniNormalizer;
use crate::settings::AppSettings;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use uuid::Uuid;

const SUBPROCESS_ERROR_PREVIEW_CHARS: usize = 4096;
const SOURCE_AUDIO_HASH_BUFFER_BYTES: usize = 128 * 1024;
const NORMALIZER_VERSION: &str = "sorani-normalizer-v1";
/// Longest we will wait for ONE 90 s decode window in the streaming import.
///
/// The old whole-file budget (`duration × 2`, up to an hour) cannot be reused as-is: decode and ASR
/// now interleave, so a file-length budget would be timing the transcription too. 90 s of audio
/// decodes in well under a second on any supported machine, so a two-minute ceiling is generous for
/// a stalled disk while still being an actual bound.
const MAX_WINDOW_DECODE_WAIT: Duration = Duration::from_secs(120);

/// P1.4b: whether the streaming loop should (re)build a cached denoiser/diarization service this window.
/// Build if the service is UNSET (`!present`); otherwise re-attempt a present-but-INACTIVE service only
/// if we have NOT already tried this file (`!already_tried`) — so an unloadable model is not reloaded
/// (a full GPU-then-CPU ONNX attempt) on every 90 s window, at most once per file. Pure, so the breaker
/// decision is unit-testable without a real ONNX model.
fn should_rebuild_streaming_service(present: bool, active: bool, already_tried: bool) -> bool {
    !present || (!already_tried && !active)
}

#[derive(Debug, Clone)]
pub struct TranscriptionDraft {
    pub raw_text: String,
    pub final_text: String,
    pub confidence: Option<f64>,
    pub confidence_source: Option<String>,
    pub model_version_id: Option<String>,
    pub cloud_call: bool,
    /// TRUE when `transcribe` itself already committed this draft to the segment row (the WSL-7B
    /// champion branch commits transcript + sole hypothesis + provenance atomically). Callers must
    /// then NOT write again: the 2026-08-20 external review found batch_transcribe re-writing the
    /// same result, so a failed second write reported "failed" for a row the first commit had
    /// already changed — two owners for one commit. One inference, one commit, one owner.
    pub committed_by_pipeline: bool,
}

/// A primary transcript that was already computed for this exact PCM during the current operation.
/// Multi-engine hypothesis population may reuse it only for the identical model id; every independent
/// voter still runs normally.
#[derive(Clone, Copy)]
struct PrimaryHypothesis<'a> {
    model_id: &'a str,
    transcript: &'a str,
    confidence: Option<f64>,
}

impl<'a> PrimaryHypothesis<'a> {
    fn from_segment(segment: &'a SpeechSegment) -> Option<Self> {
        let model_id = segment.model_version_id.as_deref()?;
        let transcript = segment.raw_transcript.as_str();
        // An empty transcript can be a genuine completed decode (for example, non-speech). Keep it
        // as completion provenance so the same deterministic model is not run again, but do not
        // insert it as jury evidence below. Explicit unavailable/not-run results remain retryable.
        if crate::quality::is_placeholder_transcript(transcript)
            || matches!(segment.confidence_source.as_deref(), Some("not_available" | "not_run"))
        {
            return None;
        }
        Some(Self { model_id, transcript, confidence: segment.confidence })
    }
}

fn reuse_primary_or_infer<E>(
    primary: Option<PrimaryHypothesis<'_>>,
    candidate_model_id: &str,
    infer: impl FnOnce() -> Option<Result<(String, Option<f64>), E>>,
) -> Option<Result<(String, Option<f64>), E>> {
    if let Some(primary) = primary.filter(|primary| primary.model_id == candidate_model_id) {
        return if primary.transcript.trim().is_empty() {
            None
        } else {
            Some(Ok((primary.transcript.to_string(), primary.confidence)))
        };
    }
    infer()
}

fn auxiliary_hypotheses_enabled(settings: &AppSettings) -> bool {
    settings.multi_engine_hypotheses && settings.asr_model_size != crate::settings::AsrModelSize::WSL7B
}

#[derive(Debug, Clone)]
pub(crate) struct SourceAudioIdentity {
    pub(crate) content_hash: String,
    pub(crate) size_bytes: i64,
}

pub(crate) fn source_audio_identity(path: &Path) -> AppResult<SourceAudioIdentity> {
    let metadata = std::fs::metadata(path)?;
    let size_bytes = i64::try_from(metadata.len())
        .map_err(|_| AppError::Validation(format!("Audio file is too large to index safely: {}", path.display())))?;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; SOURCE_AUDIO_HASH_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(SourceAudioIdentity { content_hash: hasher.finalize().to_hex().to_string(), size_bytes })
}

/// Apply LOOP-0 correction memories to a finalized transcript when the opt-in is enabled. Returns
/// the transcript unchanged when disabled, empty, or when nothing fires. Best-effort: a memory-load
/// failure logs and returns the original transcript rather than failing the transcription.
pub(crate) fn apply_loop0_firing(enabled: bool, db: &crate::db::Database, transcript: &str) -> String {
    if !enabled || transcript.trim().is_empty() {
        return transcript.to_string();
    }
    match db.load_correction_memories() {
        Ok(memories) if !memories.is_empty() => {
            let cfg = crate::corrections::FiringConfig::default();
            // Provenance: a LOOP-0 rewrite is otherwise indistinguishable from raw ASR. Record which
            // memories fired (to the persistent log) via the shared chokepoint, so ALL firing paths
            // (import + batch re-transcribe) attribute rewrites consistently.
            let fired = crate::corrections::fired_memories_summary(transcript, &memories, &cfg);
            if !fired.is_empty() {
                tracing::info!(
                    "LOOP-0 firing rewrote a transcript using {} memory/memories: {}",
                    fired.len(),
                    fired.join(", ")
                );
            }
            crate::corrections::apply_memories(transcript, &memories, &cfg)
        }
        Ok(_) => transcript.to_string(),
        Err(error) => {
            tracing::warn!("LOOP-0 firing skipped: failed to load correction memories: {error}");
            transcript.to_string()
        }
    }
}

/// Map the configured LLM model name to an OpenRouter model id for the consent-gated Gemini/OpenRouter
/// refine path. An already-namespaced id (`vendor/model`) passes through; a bare `gemini-*` gets the
/// `google/` prefix; a local-only name (e.g. `heretic-final:latest`) has no OpenRouter equivalent, so we
/// default to the Gemini-class model the "Gemini" mode implies — never silently to `openai/gpt-4o-mini`.
fn openrouter_model_id(configured: &str) -> String {
    let m = configured.trim();
    if m.is_empty() {
        return "google/gemini-2.5-pro".to_string();
    }
    if m.contains('/') {
        return m.to_string();
    }
    if m.to_ascii_lowercase().starts_with("gemini") {
        return format!("google/{m}");
    }
    "google/gemini-2.5-pro".to_string()
}

/// M2.3 / P1.3: true when LOOP-0 WOULD change this transcript (a confirmed correction memory matches).
/// This is the pure shadow signal — it never mutates and is independent of the firing opt-in, so the C5
/// over-trigger decision can be measured while firing stays default-off.
pub(crate) fn loop0_would_fire(memories: &[crate::corrections::MemoryEntry], text: &str) -> bool {
    !memories.is_empty()
        && !text.trim().is_empty()
        // Detect an ACTUAL memory firing (a winner-take-all slot replacement), NOT whitespace normalization.
        // apply_memories rebuilds the text via split_whitespace()+join(" "), so it differs from a
        // non-whitespace-canonical input (double space, leading/trailing, tab, newline) even when ZERO
        // memories match — using `apply_memories(...) != text` here counted pure whitespace edits as
        // firings and inflated the C5 over-trigger honesty metric (wouldFire / firedButHumanAcceptedOriginal,
        // which must be 0 to let firing go live). fired_memories_summary applies the SAME eligibility gates +
        // winner-take-all as apply_memories (it is the real firing path's provenance), minus that artifact.
        && !crate::corrections::fired_memories_summary(text, memories, &crate::corrections::FiringConfig::default())
            .is_empty()
}

/// F7 — the LLM-refinement diff guard. Max CER an LLM refinement may sit from the raw ASR text
/// before it is rejected. A refiner is a light post-editor (fix phonetic/orthographic slips); an LLM
/// can instead hallucinate a fluent-but-wrong rewrite far from what was actually heard. Without this
/// bound that rewrite would silently overwrite good, audio-grounded ASR. Mirrors the T2 listener's
/// `GEMINI_MAX_EDIT_FROM_HYP`.
const MAX_REFINE_CER_FROM_RAW: f64 = 0.6;

/// Accept an LLM refinement only when it is non-empty AND stays within [`MAX_REFINE_CER_FROM_RAW`]
/// of the raw ASR text; otherwise keep the raw transcript. This is the single chokepoint every
/// refine call site routes through so a hallucinated rewrite can never overwrite good ASR.
pub(crate) fn accept_refinement(raw: &str, refined: &str) -> String {
    let trimmed = refined.trim();
    if trimmed.is_empty() {
        return raw.to_string();
    }
    let cer = crate::wer::compute_cer(raw, trimmed);
    if cer > MAX_REFINE_CER_FROM_RAW {
        tracing::warn!(
            "LLM refinement rejected: CER {cer:.2} from raw exceeds {MAX_REFINE_CER_FROM_RAW}; keeping raw ASR text"
        );
        return raw.to_string();
    }
    trimmed.to_string()
}

fn subprocess_error_preview(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "(no stderr output)".to_string();
    }

    let mut chars = trimmed.chars();
    let mut preview: String = chars.by_ref().take(SUBPROCESS_ERROR_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        preview.push_str("\n[truncated subprocess stderr]");
    }
    preview
}

pub(crate) fn kill_and_reap_wsl_child(child: &mut std::process::Child, context: &str) {
    if let Err(error) = child.kill() {
        tracing::warn!("Failed to kill {context}: {error}");
    }
    if let Err(error) = child.wait() {
        tracing::warn!("Failed to reap {context}: {error}");
    }
}

pub(crate) fn join_wsl_pipe_reader(thread: std::thread::JoinHandle<Vec<u8>>, stream: &str) -> Vec<u8> {
    match thread.join() {
        Ok(buffer) => buffer,
        Err(_) => {
            tracing::warn!("WSL subprocess {stream} reader panicked");
            Vec::new()
        }
    }
}

fn log_hypothesis_population_failure(segment_id: &str, error: &AppError) {
    tracing::error!("Failed to populate ASR hypotheses for {segment_id}: {error}");
}

/// Resume decision: should this file be SKIPPED (its already-persisted segments adopted into the
/// jury batch rather than re-processed)? A file is already done when EITHER the resume journal
/// recorded it (`journaled`) OR — while resuming — its segments are already in the DB
/// (`has_persisted_segments`). The second case closes the crash window between `persist_segments`
/// (an atomic batch commit) and `mark_import_file_done`: the rows exist but the journal never saw
/// them, so without it resume would re-persist and DUPLICATE every segment of the in-flight file.
/// Only fires while resuming — a fresh import (`resuming == false`) must never skip.
fn resume_should_skip_file(resuming: bool, journaled: bool, has_persisted_segments: bool) -> bool {
    journaled || (resuming && has_persisted_segments)
}

/// Resolve the segment id for a `(audio_path, alignment_json)` pair when `transcribe` was called
/// without an explicit `segment_id`. A missing row is a legitimate `Ok(None)` — the caller falls
/// through to the "segment not found, import first" error. A REAL DB error (locked / IO / corrupt /
/// no-such-table) PROPAGATES as `Err` instead of masquerading as "no such row": the old `.ok()`
/// collapsed both into `None`, so a transient read failure wrongly told the user to re-import an
/// already-imported file and hid the real fault. Matches the sibling bare-`audio_path` branch, which
/// already propagates DB errors.
fn resolve_segment_id_by_alignment(
    conn: &rusqlite::Connection,
    audio_path: &str,
    alignment_json: &str,
) -> AppResult<Option<String>> {
    match conn.query_row(
        "SELECT id FROM speech_segments WHERE audio_path = ? AND alignment_json = ?",
        [audio_path, alignment_json],
        |row| row.get::<_, String>(0),
    ) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(AppError::Other(format!("transcribe: segment lookup by alignment failed: {e}"))),
    }
}

fn insert_hypothesis_checked(
    db: &Database,
    segment_id: &str,
    model_id: &str,
    transcript: String,
    confidence: Option<f64>,
) -> AppResult<()> {
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment_id.to_string(),
        model_id: model_id.to_string(),
        transcript,
        confidence,
    })
    .map_err(|error| AppError::Other(format!("Failed to insert {model_id} hypothesis for {segment_id}: {error}")))
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Wsl7bResult {
    pub raw_transcript: String,
    pub confidence: Option<f64>,
    pub model_version_id: String,
    pub deployment_sha256: String,
}

fn require_exact_champion_result(
    result: &Wsl7bResult,
    expected: &crate::registry::DeploymentIdentity,
) -> AppResult<()> {
    if result.model_version_id != expected.model_version_id || result.deployment_sha256 != expected.deployment_sha256 {
        return Err(AppError::Validation(format!(
            "{ASR_7B_UNAVAILABLE_TAG}: transcription reply identity {}/{} does not match registry champion {}/{}",
            result.model_version_id, result.deployment_sha256, expected.model_version_id, expected.deployment_sha256
        )));
    }
    Ok(())
}

/// The clip's source window from alignment metadata, mirroring the WSL client's rule exactly:
/// ABSENT alignment = whole file (the file IS the clip); PRESENT but offsetless or inverted = a
/// clobbered chunk and a hard error — transcribing the whole source as if it were the clip would
/// store a transcript of the wrong audio.
fn wsl7b_source_range(alignment_json: Option<&str>) -> AppResult<Option<(i64, i64)>> {
    let Some(raw) = alignment_json.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        AppError::Validation(format!(
            "segment alignment metadata is present but is not valid JSON ({e}) — re-import the file"
        ))
    })?;
    match (
        parsed.get("source_start_ms").and_then(serde_json::Value::as_i64),
        parsed.get("source_end_ms").and_then(serde_json::Value::as_i64),
    ) {
        (Some(start), Some(end)) if end > start && start >= 0 => Ok(Some((start, end))),
        _ => Err(AppError::Validation(
            "segment alignment metadata has no usable source_start_ms/source_end_ms offsets (clobbered chunk) — re-import the file"
                .into(),
        )),
    }
}

/// One bounded newline-terminated JSON request to the champion server over TCP — the DIRECT
/// transport (2026-08-20 external review). The per-segment WSL client subprocess re-derived
/// (audio_path, start, end) by snapshot-copying the LIVE DB + WAL (~108 MB on this library) into
/// WSL and spawning a Python interpreter for EVERY clip; Rust already holds every one of those
/// values. Same wire protocol, same BUSY backpressure retry, same bounded single-reply read, no
/// process spawn, no DB transport. Cancel is polled every 500 ms while waiting on the socket.
fn wsl7b_request_direct(
    request: &serde_json::Value,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> AppResult<serde_json::Value> {
    use std::io::{Read, Write};
    const MAX_REPLY_BYTES: usize = 1024 * 1024; // matches the server's MAX_RESPONSE_BYTES
    let port = wsl_7b_port();
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut payload = serde_json::to_vec(request).map_err(|e| AppError::Other(e.to_string()))?;
    payload.push(b'\n');
    let deadline = std::time::Instant::now() + timeout;
    let cancelled = || cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed));
    loop {
        if cancelled() {
            return Err(AppError::Other("7B call cancelled".into()));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(AppError::Other(format!(
                "7B server stayed busy for {:.0}s on 127.0.0.1:{port}",
                timeout.as_secs_f64()
            )));
        }
        let mut stream =
            std::net::TcpStream::connect_timeout(&addr, remaining.min(Duration::from_secs(5))).map_err(|e| {
                // A connect that failed because OUR budget ran out is a TIMEOUT, not a dead engine.
                // After a long BUSY retry loop `remaining` can be a couple of milliseconds, and
                // reporting that as "7B engine not running" sends the operator to restart a WSL
                // server that was up the whole time (review 2026-08-20).
                if std::time::Instant::now() >= deadline {
                    AppError::Other(format!(
                        "7B server reachable but timed out after {:.0}s on 127.0.0.1:{port}",
                        timeout.as_secs_f64()
                    ))
                } else {
                    AppError::Other(format!(
                        "7B engine not running: cannot reach the OmniASR-7B server on 127.0.0.1:{port} ({e})"
                    ))
                }
            })?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_write_timeout(Some(remaining.min(Duration::from_secs(10))));
        stream
            .write_all(&payload)
            .map_err(|e| AppError::Other(format!("7B engine request could not be sent on 127.0.0.1:{port} ({e})")))?;
        // Read ONE newline-terminated bounded reply, polling cancel between short read timeouts.
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            if cancelled() {
                return Err(AppError::Other("7B call cancelled".into()));
            }
            if std::time::Instant::now() >= deadline {
                return Err(AppError::Other(format!(
                    "7B server reachable but timed out after {:.0}s on 127.0.0.1:{port}",
                    timeout.as_secs_f64()
                )));
            }
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > MAX_REPLY_BYTES {
                        return Err(AppError::Other(format!("7B engine reply exceeded {MAX_REPLY_BYTES} bytes")));
                    }
                    if buf.contains(&b'\n') {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                    continue;
                }
                Err(e) => return Err(AppError::Other(format!("7B engine connection failed mid-reply ({e})"))),
            }
        }
        if buf.is_empty() {
            return Err(AppError::Other(format!("7B engine returned no data from 127.0.0.1:{port}")));
        }
        let Some(newline) = buf.iter().position(|&b| b == b'\n') else {
            return Err(AppError::Other("7B engine reply was not newline-terminated".into()));
        };
        if buf[newline + 1..].iter().any(|&b| !b.is_ascii_whitespace()) {
            return Err(AppError::Other("7B engine sent more than one reply".into()));
        }
        let reply: serde_json::Value = serde_json::from_slice(&buf[..newline])
            .map_err(|e| AppError::Other(format!("7B engine sent an unparseable reply: {e}")))?;
        if !reply.is_object() {
            return Err(AppError::Other("7B engine reply is not a JSON object".into()));
        }
        if reply.get("code").and_then(serde_json::Value::as_str) == Some("BUSY") {
            std::thread::sleep(Duration::from_millis(50));
            continue; // every replica mid-decode: reconnect so the kernel can pick a freed worker
        }
        if let Some(error) = reply.get("error") {
            let message = error.as_str().filter(|m| !m.is_empty()).unwrap_or("unknown server error");
            return Err(AppError::Other(format!("7B engine error: {message}")));
        }
        return Ok(reply);
    }
}

/// Refuse a reachable but untrustworthy champion: byte-for-byte the WSL client's identity rules.
fn wsl7b_validate_identity(reply: &serde_json::Value) -> AppResult<(String, String)> {
    fn is_sha(value: Option<&serde_json::Value>) -> bool {
        value
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    }
    let fail = |message: &str| AppError::Other(format!("7B engine identity error: {message}"));
    if reply.get("protocol").and_then(serde_json::Value::as_str) != Some("cortex-omniasr-adapter")
        || reply.get("protocolVersion").and_then(serde_json::Value::as_i64) != Some(1)
    {
        return Err(fail("reply is not from the compatible Cortex OmniASR deployment protocol"));
    }
    if reply.get("family").and_then(serde_json::Value::as_str) != Some("omniasr-7b") {
        return Err(fail("reply has an unexpected model family"));
    }
    let Some(model_id) =
        reply.get("modelVersionId").and_then(serde_json::Value::as_str).filter(|m| !m.trim().is_empty())
    else {
        return Err(fail("reply has no modelVersionId"));
    };
    if !is_sha(reply.get("deploymentSha256")) {
        return Err(fail("reply has no canonical deploymentSha256"));
    }
    let Some(components) = reply.get("componentSha256").and_then(serde_json::Value::as_object) else {
        return Err(fail("reply has no complete componentSha256 identity"));
    };
    let expected = ["adapter", "adapterConfig", "base", "tokenizer"];
    let mut keys: Vec<&str> = components.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != expected || expected.iter().any(|k| !is_sha(components.get(*k))) {
        return Err(fail("reply has no complete componentSha256 identity"));
    }
    if reply.get("language").and_then(serde_json::Value::as_str) != Some("ckb_Arab") {
        return Err(fail("reply has an unexpected language"));
    }
    if !is_sha(reply.get("manifestSha256")) {
        return Err(fail("reply has no canonical manifestSha256"));
    }
    if !matches!(reply.get("provenanceKind").and_then(serde_json::Value::as_str), Some("flywheel" | "legacy_bootstrap"))
    {
        return Err(fail("reply has an unexpected provenanceKind"));
    }
    if !reply.get("worker").and_then(serde_json::Value::as_str).is_some_and(|w| !w.trim().is_empty()) {
        return Err(fail("reply has no worker identity"));
    }
    let sha = reply.get("deploymentSha256").and_then(serde_json::Value::as_str).unwrap_or_default();
    Ok((model_id.to_string(), sha.to_string()))
}

/// The champion call over the direct transport: one gate permit, one request, validated identity.
pub(crate) fn run_wsl_segment_transcript_direct(
    audio_path: &str,
    alignment_json: Option<&str>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> AppResult<Wsl7bResult> {
    let Some(_gate) = WSL_7B_GATE.acquire(cancel) else {
        return Err(AppError::Other("7B call cancelled while waiting for a server slot".into()));
    };
    let range = wsl7b_source_range(alignment_json)?;
    let wsl_path = if audio_path.starts_with('/') { audio_path.to_string() } else { win_path_to_wsl(audio_path) };
    let mut request = serde_json::json!({ "audio_path": wsl_path });
    if let Some((start, end)) = range {
        request["start_ms"] = start.into();
        request["end_ms"] = end.into();
    }
    let timeout = Duration::from_secs_f64(
        std::env::var("CORTEX_7B_TRANSCRIPTION_TIMEOUT_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(280.0),
    );
    let reply = wsl7b_request_direct(&request, timeout, cancel)?;
    let Some(transcript) = reply.get("transcript").and_then(serde_json::Value::as_str) else {
        return Err(AppError::Other("7B engine identity error: transcription reply has no string transcript".into()));
    };
    let (model_version_id, deployment_sha256) = wsl7b_validate_identity(&reply)?;
    Ok(Wsl7bResult { raw_transcript: transcript.to_string(), confidence: None, model_version_id, deployment_sha256 })
}

fn parse_wsl_segment_result(stdout: &str) -> AppResult<Wsl7bResult> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WireResult {
        raw_transcript: String,
        confidence: Option<f64>,
        model_version_id: String,
        deployment_sha256: String,
    }

    let mut parsed: Option<WireResult> = None;
    for line in stdout.lines() {
        if let Some(stripped) = line.strip_prefix("__RESULT__=") {
            if parsed.is_some() {
                return Err(AppError::Validation(
                    "WSL 7B ASR process returned multiple __RESULT__ lines; model identity is ambiguous".into(),
                ));
            }
            parsed = Some(serde_json::from_str::<WireResult>(stripped).map_err(|error| {
                AppError::Validation(format!("WSL 7B ASR returned an invalid identity-bound result: {error}"))
            })?);
        }
    }

    // A reachable server that emits a `__RESULT__` line with an EMPTY transcript is a LEGITIMATE
    // outcome (a silent/music/noise clip), NOT an infrastructure failure — the client's failure
    // contract exits non-zero (handled by the caller before we are reached) for a real infra fault and
    // exits 0 with a `__RESULT__` line otherwise. Returning Err on an empty-but-present result made ONE
    // silent chunk roll back the ENTIRE import and left the file permanently unimportable via the 7B.
    // So Err ONLY when no `__RESULT__` line was seen at all; an empty transcript returns Ok and the
    // caller escalates just that one segment.
    let result =
        parsed.ok_or_else(|| AppError::Other("WSL 7B ASR process did not return a __RESULT__ line.".into()))?;
    crate::validation::input::validate_identifier(&result.model_version_id).map_err(AppError::Validation)?;
    if result.deployment_sha256.len() != 64
        || !result.deployment_sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(AppError::Validation("WSL 7B ASR result has no canonical deployment SHA-256 identity".into()));
    }
    if result.raw_transcript.len() > 100_000 {
        return Err(AppError::Validation(
            "WSL 7B ASR result transcript exceeds the 100,000-byte persistence bound".into(),
        ));
    }
    Ok(Wsl7bResult {
        raw_transcript: result.raw_transcript,
        confidence: result.confidence.filter(|value| value.is_finite()).map(|value| value.clamp(0.0, 1.0)),
        model_version_id: result.model_version_id,
        deployment_sha256: result.deployment_sha256,
    })
}

/// Bounds concurrent WSL-7B client spawns process-wide.
///
/// WHY A BOUND AT ALL: concurrent app-side callers (an import's per-segment pass, the batch loop, a
/// UI re-transcribe) queue on the server socket and blow through their client/app timeouts
/// CUMULATIVELY — misread as "server not running" and rolling back a HEALTHY import. Each admitted
/// request gets its FULL fresh timeout budget once it actually runs.
///
/// WHY NOT ONE: this was a plain Mutex, admitting exactly one request, on the premise that "the
/// champion server is a single-threaded accept loop". That stopped being true — `cortex_7b_server.py`
/// PRE-FORKS one full replica per GPU, all accept()ing on a shared socket, precisely so two requests
/// can run at once (it chose processes over threads because two replica THREADS measured only 1.10x
/// on 2 GPUs, GIL-bound). A one-permit gate meant the second GPU could never receive work: measured
/// 2026-08-11, a batch with 8 workers still ran 23.1 s/clip — the 7B's own serial time — with both
/// cards at 19% and 12%.
///
/// Default 1 keeps the previous behaviour exactly; set CORTEX_7B_CONCURRENCY to the server's replica
/// count. Never set it ABOVE that: extra admitted requests just queue on the socket, which is the
/// cumulative-timeout failure this gate exists to prevent.
static WSL_7B_GATE: Wsl7bGate = Wsl7bGate::new();

/// What one segment's champion attempts concluded — decided WITHOUT the shared `Database`, so a
/// whole wave of them can run at once. `transcribe` opens its OWN connection per call, which is
/// what makes this safe: each thread writes through its own SQLite handle rather than sharing the
/// caller's.
enum ChampionAttempt {
    /// A usable transcript came back and `transcribe` stored it.
    Drafted,
    /// Reachable server, no words after every retry — a silent/music chunk. Escalate THIS segment
    /// only; never roll the file back, or the good transcripts for its other chunks are discarded.
    Empty(String),
    /// The client exited non-zero: server down, hung, or errored. Fatal for a force-7B import.
    Infra(String),
}

/// Permitted concurrent 7B requests. Clamped to 1..=8; anything unusable falls back to 1, the safe
/// end — an over-admitting gate reintroduces the timeout blowout, an under-admitting one is merely slow.
fn wsl_7b_concurrency() -> usize {
    parse_wsl_7b_concurrency(std::env::var("CORTEX_7B_CONCURRENCY").ok().as_deref())
}

fn parse_wsl_7b_concurrency(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok()).filter(|n| (1..=8).contains(n)).unwrap_or(1)
}

/// A counting semaphore over `wsl_7b_concurrency()` permits (std has none; Mutex + Condvar is the
/// stdlib spelling). The limit is read on each acquire so the env var takes effect without a restart.
struct Wsl7bGate {
    in_flight: std::sync::Mutex<usize>,
    released: std::sync::Condvar,
}

impl Wsl7bGate {
    const fn new() -> Self {
        Self { in_flight: std::sync::Mutex::new(0), released: std::sync::Condvar::new() }
    }

    /// Blocks until a permit is free — or until `cancel` flips, checked every 100 ms
    /// (2026-08-20 external review: an uncancellable condvar wait meant a Cancel pressed while a
    /// clip was QUEUED behind the gate did nothing until the clip's turn came). `None` = the caller
    /// gave up because it was cancelled; no permit was taken.
    ///
    /// Poison-tolerant like every other lock in this crate.
    fn acquire(&self, cancel: Option<&std::sync::atomic::AtomicBool>) -> Option<Wsl7bPermit<'_>> {
        let limit = wsl_7b_concurrency();
        let mut in_flight = self.in_flight.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while *in_flight >= limit {
            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                return None;
            }
            let (guard, _timeout) = self
                .released
                .wait_timeout(in_flight, Duration::from_millis(100))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            in_flight = guard;
        }
        *in_flight += 1;
        Some(Wsl7bPermit { gate: self })
    }
}

/// Releases its permit on drop, so an early return or a panic mid-request cannot leak one and
/// permanently shrink the gate.
struct Wsl7bPermit<'a> {
    gate: &'a Wsl7bGate,
}

impl Drop for Wsl7bPermit<'_> {
    fn drop(&mut self) {
        let mut in_flight = self.gate.in_flight.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *in_flight = in_flight.saturating_sub(1);
        self.gate.released.notify_one();
    }
}

/// The port the OmniASR-7B warm server listens on inside WSL. SINGLE source of truth — shared by the
/// preflight probe AND passed to the client via `CORTEX_7B_PORT`, so the app, client, and server can't
/// drift (a mismatch would even green-light the preflight against the wrong service).
pub(crate) const WSL_7B_SERVER_PORT: u16 = 8799; // must match cortex_7b_server.py

/// THE one port every 7B consumer uses. Health/start already honored `CORTEX_7B_PORT`; transcription
/// and the agentic preflight used the raw const, so a configured non-default port passed the health
/// check and then transcribed against a dead socket (2026-08-20 external review). One accessor, no
/// drift.
pub(crate) fn wsl_7b_port() -> u16 {
    std::env::var("CORTEX_7B_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(WSL_7B_SERVER_PORT)
}
/// Stable provenance id for the owner's sole production ASR. Keep selection, persisted hypotheses,
/// review filtering, and health/readiness checks tied to one identifier so an auxiliary model can
/// never be mistaken for the champion by string drift between subsystems.
#[cfg(test)]
pub(crate) const CHAMPION_MODEL_ID: &str = "omniasr-wsl-7b";

/// Stable marker in the Err that `transcribe()` returns for a legit-but-EMPTY 7B result — a REACHABLE
/// server that produced no words (a silent/music/noise clip), which parse_wsl_segment_result surfaces as
/// Ok(""). transcribe() converts that to an Err so the re-transcribe IPCs never blank-overwrite a stored
/// transcript; but the IMPORT pass also routes through transcribe(), and for it an empty result is NOT an
/// infra failure — it must escalate only that one segment, never roll back the whole file. The import pass
/// matches this marker to tell a benign empty apart from a real "server down / client exited non-zero" error.
pub(crate) const WSL_7B_EMPTY_RESULT_MARKER: &str = "WSL 7B returned an empty transcript";

/// Machine-readable sentinel embedded in every "the OmniASR-7B champion is the selected primary
/// engine but it is unavailable / failed" error. The frontend matches on this token to offer one
/// safe recovery: restore the champion service and retry. The app NEVER silently substitutes a
/// smaller model on the primary path. Keep the value in sync with `ASR_7B_UNAVAILABLE_TAG` in
/// `src/lib/commands.ts`.
pub(crate) const ASR_7B_UNAVAILABLE_TAG: &str = "E_ASR_7B_UNAVAILABLE";

/// Wrap a primary-7B transcription failure so the UI can classify it (see [`ASR_7B_UNAVAILABLE_TAG`])
/// and present the champion-retry recovery. Preserves the original actionable text.
pub(crate) fn tag_7b_unavailable(err: AppError) -> AppError {
    let msg = err.to_string();
    if msg.contains(ASR_7B_UNAVAILABLE_TAG) {
        return err; // already tagged upstream — don't double-prefix
    }
    AppError::Validation(format!("{ASR_7B_UNAVAILABLE_TAG}: {msg}"))
}

/// Translate a Windows path to its WSL `/mnt` view (mirrors `cortex_7b_client.py`'s `win_to_wsl`), so
/// the app can hand the client a `CORTEX_7B_DB` that follows a moved data dir instead of the client's
/// hardcoded default. `C:\a\b` -> `/mnt/c/a/b`; a `\\?\` extended-length prefix is stripped; a
/// non-drive path is returned with backslashes normalised.
pub(crate) fn win_path_to_wsl(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    let bytes = s.as_bytes();
    if bytes.len() > 2 && bytes[1] == b':' {
        let drive = s[..1].to_ascii_lowercase();
        return format!("/mnt/{drive}{}", &s[2..]);
    }
    s
}

fn resolve_wsl_7b_client(configured: Option<String>) -> Option<String> {
    for candidate in [configured, std::env::var("CORTEX_7B_CLIENT_SCRIPT").ok()].into_iter().flatten() {
        let value = candidate.trim();
        if value.starts_with('/') || std::path::Path::new(value).is_file() {
            return Some(value.to_string());
        }
    }
    let exe_dir = std::env::current_exe().ok().and_then(|path| path.parent().map(std::path::Path::to_path_buf))?;
    [
        "cortex_7b_client.py",
        "scripts/cortex_7b_client.py",
        "_up_/scripts/cortex_7b_client.py",
        "../../../../scripts/cortex_7b_client.py",
        "../../../scripts/cortex_7b_client.py",
        "../../scripts/cortex_7b_client.py",
    ]
    .into_iter()
    .find_map(|relative| {
        exe_dir
            .join(relative)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
    })
}

/// Spawn the configured external WSL ASR client for ONE segment and return its parsed transcript.
///
/// Shared by the per-segment pipeline path (`cancel = None`) and the batch refinement command. The
/// configured script is the per-segment warm-server client (`--segment-id <id> --stdout-only`); the
/// batch command drives THIS function in a loop rather than spawning the script once with batch
/// flags (`--limit-files`/`--dry-run`/…) the per-segment client does not understand. `cancel`, when
/// supplied, is polled while waiting for the child so a long-running clip is killed promptly (within
/// ~50 ms) instead of blocking the whole batch for the full 5-minute timeout.
pub(crate) fn run_wsl_segment_transcript_with_script(
    external_script: &str,
    segment_id: &str,
    db_path: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> AppResult<Wsl7bResult> {
    // Hold a permit for the whole spawn+wait so no more 7B calls are in flight than the server has
    // replicas to serve them. Released on drop, including on an early return. A cancel that lands
    // while QUEUED here returns immediately — no permit, no spawn.
    let Some(_gate) = WSL_7B_GATE.acquire(cancel) else {
        return Err(AppError::Other("7B call cancelled while waiting for a server slot".into()));
    };

    let external_script =
        if external_script.starts_with('/') { external_script.to_string() } else { win_path_to_wsl(external_script) };
    let python =
        std::env::var("CORTEX_7B_PYTHON").unwrap_or_else(|_| "/home/ai/.venv-wsl-whisper/bin/python".to_string());
    let mut cmd = std::process::Command::new("wsl");
    // Pass the DB path + port to the client via `env` (WSL does not propagate Windows env into Linux),
    // so the client follows a MOVED data dir / non-default port instead of its hardcoded fallbacks — the
    // app is the single source of truth for both.
    cmd.arg("env")
        .arg(format!("CORTEX_7B_DB={}", win_path_to_wsl(db_path)))
        .arg(format!("CORTEX_7B_PORT={}", wsl_7b_port()))
        .arg(python)
        .arg(external_script)
        .arg("--segment-id")
        .arg(segment_id)
        .arg("--stdout-only");

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    // Spawn with explicit pipes so we keep a KILLABLE Child handle. The previous
    // cmd.output()-on-a-thread approach gave no handle: on timeout the reader thread stayed blocked
    // in output() and the wsl subprocess kept running, leaking one thread + one zombie process per
    // timed-out segment. Now we poll for exit and, on timeout OR cancel, kill + reap the child.
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| AppError::Other(format!("WSL subprocess launch failed: {e}")))?;

    // Drain both pipes on threads so a chatty child can't deadlock on a full pipe buffer; the
    // readers finish when the pipes close (child exit or kill). Bound how much we read so a
    // buggy/hostile script that streams unbounded output can't OOM the host. The real protocol is
    // one small `__RESULT__=` line; 8 MiB is far more than enough headroom while capping a runaway.
    const MAX_WSL_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut s) = child_stdout {
            use std::io::Read;
            let _ = s.take(MAX_WSL_OUTPUT_BYTES).read_to_end(&mut buf);
        }
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut s) = child_stderr {
            use std::io::Read;
            let _ = s.take(MAX_WSL_OUTPUT_BYTES).read_to_end(&mut buf);
        }
        buf
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let mut was_cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
                    kill_and_reap_wsl_child(&mut child, "cancelled WSL subprocess");
                    was_cancelled = true;
                    break None;
                }
                if std::time::Instant::now() >= deadline {
                    kill_and_reap_wsl_child(&mut child, "timed-out WSL subprocess");
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                kill_and_reap_wsl_child(&mut child, "failed WSL subprocess");
                return Err(AppError::Other(format!("WSL subprocess wait failed: {e}")));
            }
        }
    };

    let stdout = join_wsl_pipe_reader(stdout_reader, "stdout");
    let stderr = join_wsl_pipe_reader(stderr_reader, "stderr");
    let output = match status {
        Some(status) => std::process::Output { status, stdout, stderr },
        None if was_cancelled => return Err(AppError::Other("WSL 7B ASR was cancelled.".into())),
        None => return Err(AppError::Other("WSL 7B ASR process timed out after 5 minutes. Check WSL health.".into())),
    };

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    if !stdout_str.is_empty() {
        tracing::debug!("WSL 7B ASR stdout captured ({} bytes).", output.stdout.len());
    }
    if !stderr_str.is_empty() {
        tracing::debug!("WSL 7B ASR stderr captured ({} bytes).", output.stderr.len());
    }

    if !output.status.success() {
        let err_msg = subprocess_error_preview(&stderr_str);
        return Err(AppError::Other(format!("WSL 7B ASR process failed: {}", err_msg)));
    }

    parse_wsl_segment_result(&stdout_str)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportStatus {
    pub running: bool,
    pub current: usize,
    pub total: usize,
    pub file: String,
}

#[derive(Debug, Clone)]
pub enum PipelineEvent {
    Started { total: usize },
    Phase { phase: String },
    AgentStage { stage: String, status: String, file: String, detail: String, current: usize, total: usize },
    Progress { current: usize, total: usize, file: String, status: String },
    Completed { total: usize, succeeded: usize, failed: usize },
    Error { file: String, error: String },
}

fn agent_stage(
    stage: &str,
    status: &str,
    file: impl Into<String>,
    detail: impl Into<String>,
    current: usize,
    total: usize,
) -> PipelineEvent {
    PipelineEvent::AgentStage {
        stage: stage.to_string(),
        status: status.to_string(),
        file: file.into(),
        detail: detail.into(),
        current,
        total,
    }
}

fn multi_model_hypothesis_stage(
    db: &Database,
    settings: &crate::settings::AppSettings,
    file: impl Into<String>,
    segments: &[SpeechSegment],
) -> PipelineEvent {
    let file = file.into();
    let total = segments.len().max(1);

    // The production champion is intentionally single-engine. Requiring two auxiliary ASRs here made
    // a healthy 7B import look blocked and encouraged silent 300M/1B/MMS execution solely to satisfy a
    // metric. Training-grade multi-model proof remains enforced in its dedicated export gates; it is
    // simply not a requirement of the champion transcription/review flow.
    if settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
        return agent_stage(
            "multi_model_hypotheses",
            "not_required",
            file,
            "Champion-only mode: the fine-tuned OmniASR 7B transcript is sent to human review; auxiliary ASR coverage is not required or executed",
            segments.len(),
            total,
        );
    }
    if segments.is_empty() {
        return agent_stage(
            "multi_model_hypotheses",
            "blocked",
            file,
            "No speech segments were persisted, so multi-model hypothesis coverage could not be verified",
            0,
            total,
        );
    }

    let mut covered = 0usize;
    let mut blocked_ids = Vec::new();
    let mut observed_models = std::collections::BTreeSet::new();
    for segment in segments {
        match db.get_hypotheses_for_segment(&segment.id) {
            Ok(hypotheses) => {
                let coverage = crate::quality::hypothesis_coverage_for_model_outputs(&hypotheses);
                observed_models.extend(coverage.non_empty_models.iter().cloned());
                if coverage.passes_minimum {
                    covered += 1;
                } else {
                    blocked_ids.push(segment.id.clone());
                }
            }
            Err(error) => {
                return agent_stage(
                    "multi_model_hypotheses",
                    "blocked",
                    file,
                    format!("Failed to verify multi-model hypothesis coverage from the database: {error}"),
                    covered,
                    total,
                );
            }
        }
    }

    if blocked_ids.is_empty() {
        let models = if observed_models.is_empty() {
            "none".to_string()
        } else {
            observed_models.into_iter().collect::<Vec<_>>().join(", ")
        };
        return agent_stage(
            "multi_model_hypotheses",
            "completed",
            file,
            format!("Verified multi-model hypothesis coverage for {covered}/{total} segment(s): {models}"),
            covered,
            total,
        );
    }

    let preview = blocked_ids.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    let suffix = if blocked_ids.len() > 5 { format!(" and {} more", blocked_ids.len() - 5) } else { String::new() };
    agent_stage(
        "multi_model_hypotheses",
        "blocked",
        file,
        format!(
            "Only {covered}/{total} segment(s) have the required non-empty multi-model hypothesis coverage; blocked segment(s): {preview}{suffix}"
        ),
        covered,
        total,
    )
}

/// The three cloud opt-ins, held so that REVOKING one reaches work already in flight.
///
/// Why this is separate from `settings` (audit 2026-08-06). Every long-running entry point does
/// `state.lock_pipeline().clone()` and then runs on that clone (commands.rs:482, 580, 644, 1132),
/// while `update_settings` REPLACES `self.settings` on the stored instance. An `Arc` swap is not
/// visible through a clone taken earlier, so a directory import started before the user switched
/// cloud OFF kept uploading audio and transcripts for every remaining file — the save succeeded,
/// `get_settings` said off, the toggle rendered off, and the egress continued. That directly
/// contradicts the fail-safe consent doctrine this crate states at commands.rs:1877.
///
/// Only CONSENT lives here, deliberately. A model choice or VAD threshold changed mid-import should
/// NOT retroactively apply — the import is one coherent unit and the snapshot is correct for it.
/// Withdrawing consent is not a preference, it is a stop instruction, and it has to be obeyed at the
/// moment of the call rather than at the moment the run began.
///
/// Read with the snapshot, never instead of it: each gate is `snapshot_allows && still_consented`.
/// So revoking mid-run halts egress immediately, while GRANTING mid-run does not retroactively
/// enable a run the user started under a no-cloud understanding. Fail-safe in both directions.
#[derive(Debug, Default)]
pub struct LiveConsent {
    cloud_llm: AtomicBool,
    jury_cloud: AtomicBool,
}

impl LiveConsent {
    fn from_settings(settings: &AppSettings) -> Self {
        let consent = Self::default();
        consent.apply(settings);
        consent
    }

    fn apply(&self, settings: &AppSettings) {
        // SeqCst, not Relaxed: this is a safety stop, and the cost is irrelevant next to a network
        // call. A worker must never read a stale `true` after the user has switched cloud off.
        self.cloud_llm.store(settings.cloud_llm_opt_in, Ordering::SeqCst);
        self.jury_cloud.store(settings.jury_cloud_opt_in, Ordering::SeqCst);
    }

    fn cloud_llm(&self) -> bool {
        self.cloud_llm.load(Ordering::SeqCst)
    }
    fn jury_cloud(&self) -> bool {
        self.jury_cloud.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
pub struct ProcessingPipeline {
    db_path: String,
    _normalizer: Arc<SoraniNormalizer>,
    cache: Arc<TranscriptCache>,
    fingerprint: Arc<AudioFingerprint>,
    settings: Arc<AppSettings>,
    /// Shared by every clone (Arc, never replaced) so a withdrawal reaches in-flight work.
    consent: Arc<LiveConsent>,
    model_manager: Arc<ModelManager>,
    asr_pool: Arc<asr::AsrPool>,
    import_status: Arc<Mutex<ImportStatus>>,
    diarization_service: Arc<Mutex<Option<crate::diarization::SpeakerEmbeddingService>>>,
    denoiser_service: Arc<Mutex<Option<crate::denoiser::DenoiserService>>>,
    /// F2 no-silent-downgrade: per-import counters for chunks where the selected fine-tuned engine
    /// was attempted vs where it silently fell back to the stock engine (absent model / error /
    /// empty output). Shared across pipeline clones; reset at import start; a non-zero fallback
    /// count is surfaced as a LOUD completion-time error event — never a log-only downgrade.
    finetuned_attempts: Arc<std::sync::atomic::AtomicUsize>,
    finetuned_fallbacks: Arc<std::sync::atomic::AtomicUsize>,
}

impl ProcessingPipeline {
    pub fn new(
        db_path: String,
        normalizer: Arc<SoraniNormalizer>,
        cache: Arc<TranscriptCache>,
        fingerprint: Arc<AudioFingerprint>,
        settings: Arc<AppSettings>,
        model_manager: Arc<ModelManager>,
    ) -> Self {
        Self {
            db_path,
            _normalizer: normalizer,
            consent: Arc::new(LiveConsent::from_settings(&settings)),
            cache,
            fingerprint,
            settings,
            model_manager,
            asr_pool: Arc::new(asr::AsrPool::new()),
            import_status: Arc::new(Mutex::new(ImportStatus::default())),
            diarization_service: Arc::new(Mutex::new(None)),
            denoiser_service: Arc::new(Mutex::new(None)),
            finetuned_attempts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            finetuned_fallbacks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Apply only the WITHDRAWALS in `next` to the shared live consent, right now.
    ///
    /// Never grants: turning an opt-in ON still goes through `update_settings`, which commits only
    /// after a successful persist, so consent can never be enabled by a change that will not survive
    /// a restart.
    ///
    /// This exists because persistence and safety want opposite orderings (external review
    /// 2026-08-06). `commands::update_settings` deliberately saves BEFORE committing, so a failed
    /// save leaves memory, pipeline and disk consistent at the old value. Correct for a preference —
    /// and wrong for a withdrawal: on a full or read-only disk the user's revocation would never
    /// reach the running import and audio would keep going to the cloud, with only a save error to
    /// show for it. A stop instruction must not be contingent on free disk space.
    pub fn revoke_consent_now(&self, next: &AppSettings) {
        if !next.cloud_llm_opt_in {
            self.consent.cloud_llm.store(false, Ordering::SeqCst);
        }
        if !next.jury_cloud_opt_in {
            self.consent.jury_cloud.store(false, Ordering::SeqCst);
        }
    }

    pub fn update_settings(&mut self, settings: AppSettings) {
        // Fail-safe in BOTH directions, which needs two different mechanisms:
        //
        // WITHDRAWAL is a stop instruction, so it is applied to the SHARED handle and reaches every
        // clone already running an import or an upload batch. That is the 2026-08-06 audit fix.
        //
        // GRANTING must NOT reach those clones: a run the user began understanding nothing would
        // leave the machine must stay offline for its whole length. So instead of raising the shared
        // flags, this instance is handed a FRESH handle. Clones keep the one they started with —
        // which `revoke_consent_now` can still lower, but nothing can raise.
        self.revoke_consent_now(&settings);
        self.consent = Arc::new(LiveConsent::from_settings(&settings));
        self.settings = Arc::new(settings);
    }

    /// F2: reset the per-import fine-tuned downgrade counters (call at every import entry point).
    fn reset_finetuned_counters(&self) {
        self.finetuned_attempts.store(0, std::sync::atomic::Ordering::Relaxed);
        self.finetuned_fallbacks.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// F2: the LOUD completion-time downgrade message. `None` when no chunk fell back (including
    /// when the fine-tuned engine was never selected). Pure and unit-tested — the no-silent-downgrade
    /// contract hangs on this being emitted, not logged.
    pub(crate) fn finetuned_downgrade_message(attempts: usize, fallbacks: usize) -> Option<String> {
        if fallbacks == 0 {
            return None;
        }
        Some(if fallbacks >= attempts {
            format!(
                "Fine-tuned engine is selected but ALL {attempts} chunk(s) were drafted by the STOCK engine \
                 instead (model missing or failing) — this import's accuracy is stock-grade, not the selected \
                 engine's. Run Stats → Verify model integrity."
            )
        } else {
            format!(
                "Fine-tuned engine fell back to the STOCK engine on {fallbacks} of {attempts} chunk(s) — those \
                 drafts are stock-grade. Run Stats → Verify model integrity."
            )
        })
    }

    /// Path to the SQLite database this pipeline writes to. The batch 7B refinement command reads it
    /// (briefly, under the pipeline lock) so its detached worker thread can open its OWN connection
    /// instead of holding an AppState lock across a long per-segment transcription loop.
    pub(crate) fn db_path(&self) -> &str {
        &self.db_path
    }

    pub fn settings_snapshot(&self) -> AppSettings {
        self.settings.as_ref().clone()
    }

    /// Pre-load the pooled Meta OmniASR CTC recognizer.
    pub fn warmup_asr(&self) -> Result<(), String> {
        if self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
            tracing::info!("WSL 7B model selected: skipping local ONNX ASR pool warm-up.");
            return Ok(());
        }
        let model_dir = self.asr_model_root();
        self.asr_pool.warmup(&model_dir, &self.asr_config())
    }

    /// The models ROOT that actually contains the SELECTED OmniASR engine.
    ///
    /// Per-file, like the CAMPP/denoiser sites below and for the identical reason (round-26):
    /// `resolved_dir()` flips all-or-nothing between the user dir and the bundled dir, so a user dir
    /// holding SOME models orphans every model that lives only in the other root. MEASURED
    /// 2026-08-13: `%APPDATA%/cortex-speech/models/omniasr-ctc-1b/` existed but was EMPTY while the
    /// 1 GB bundled copy sat beside the exe — selecting CTC-1B failed with "ASR service unavailable
    /// (models missing?)" even though the model was present on disk. The engine was offered in the
    /// UI and could not load.
    fn asr_model_root(&self) -> std::path::PathBuf {
        self.root_for_size(&self.selected_asr_model_size())
    }

    /// The models root that actually contains THIS engine's weights.
    fn root_for_size(&self, size: &crate::settings::AsrModelSize) -> std::path::PathBuf {
        // A recognizer needs BOTH its weights and tokens from one root. Resolving only the model
        // filename can select a partial user download and orphan a complete bundled pair, so prefer
        // the first root where the whole engine passes the same size-aware presence gate as loading.
        for root in std::iter::once(self.model_manager.models_dir.clone()).chain(crate::models::model_root_candidates())
        {
            if asr::omniasr_model_present(&root, size) {
                return root;
            }
        }

        let (model_path, _) = asr::omniasr_model_paths(std::path::Path::new(""), size);
        let relative = model_path.to_string_lossy().replace('\\', "/");
        self.model_manager.resolve_root_for(relative.trim_start_matches('/'))
    }

    /// Probe this exact engine in the root selected for its complete weights+tokens pair.
    fn size_present(&self, size: &crate::settings::AsrModelSize) -> bool {
        asr::omniasr_model_present(&self.root_for_size(size), size)
    }

    fn asr_config(&self) -> asr::AsrLoadConfig {
        asr::AsrLoadConfig {
            model_size: self.selected_asr_model_size(),
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        }
    }

    /// Return exactly the configured engine. Missing assets fail at the selected engine's load path;
    /// substituting another installed engine would silently trade accuracy and falsify provenance.
    fn selected_asr_model_size(&self) -> crate::settings::AsrModelSize {
        self.settings.asr_model_size.clone()
    }

    /// CHAMPION SUPREMACY (owner rule, 2026-08-11 — see AGENT_CHARTER "The champion is not optional").
    ///
    /// When the owner selects WSL7B, the OmniASR-7B champion drafts EVERY clip. `use_finetuned_asr`
    /// no longer diverts it to the smaller embedded model, because that is a silent downgrade of the
    /// one thing accuracy depends on.
    ///
    /// Measured 2026-08-10, which is why this changed: a 494-clip review queue was drafted 494/494 by
    /// `finetuned-mms-ckb` while `asr_model_size` said WSL7B and the champion sat up and idle on both
    /// GPUs. Nothing in the UI, the DB or any gate said so — the owner found it by reading the
    /// transcripts. Measured gap on identical FLEURS ckb clips: 7.03% CER vs 9.32% (and the app runs
    /// the int8 build, whose own baseline is 21.00%).
    ///
    /// The desktop trust boundary clamps production to WSL7B. Smaller engines remain available only
    /// to explicit offline diagnostic/evaluation code, never as an interactive substitute.
    fn should_use_wsl_primary_asr(&self) -> bool {
        self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B
            && resolve_wsl_7b_client(self.settings.external_asr_script_path()).is_some()
    }

    /// True when the fine-tuned engine may act as the PRIMARY drafter: the flag is on and the owner
    /// has not selected the champion. Selecting WSL7B outranks the flag (champion supremacy).
    fn finetuned_override_active(&self) -> bool {
        self.settings.use_finetuned_asr && self.settings.asr_model_size != crate::settings::AsrModelSize::WSL7B
    }

    /// F2 — the no-silent-downgrade guard. True when the selected primary engine is WSL 7B but no
    /// client script is configured. In that state every primary-transcription entry point must FAIL
    /// LOUDLY here rather than silently downgrading (the fail-hard contract documented in
    /// settings.rs). Auxiliary hypothesis generation is also disabled whenever WSL7B is selected.
    fn wsl7b_primary_unresolved(&self) -> bool {
        self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B
            && resolve_wsl_7b_client(self.settings.external_asr_script_path()).is_none()
    }

    /// F6 — exact preflight before an import that will drive the WSL 7B primary. A TCP-open probe is
    /// insufficient: an old deployment or unrelated process can own the port. The bundled client asks
    /// for the loaded content identity and this method requires it to equal the registry champion's
    /// `{model id, deployment SHA}` before any decode or transcript write begins.
    /// Public preflight for any caller about to drive the PRIMARY engine over a batch.
    ///
    /// `batch_transcribe` accepted a 487-clip job and hard-stopped on clip 1 because the champion
    /// server was down (measured 2026-08-11). The stop was correct, but the caller had already been
    /// told "started". Checking here makes an unreachable champion an immediate, actionable refusal
    /// instead of a halt after the first write. A no-op when the champion is not the primary.
    pub fn preflight_primary_engine(&self) -> AppResult<()> {
        if self.wsl7b_primary_unresolved() {
            return Err(Self::primary_engine_unavailable_error());
        }
        self.wsl_7b_server_preflight()
    }

    fn wsl_7b_server_preflight(&self) -> AppResult<()> {
        if !self.should_use_wsl_primary_asr() {
            return Ok(());
        }
        let client = resolve_wsl_7b_client(self.settings.external_asr_script_path())
            .ok_or_else(Self::primary_engine_unavailable_error)?;
        let db = Database::open(&self.db_path)?;
        let expected = crate::registry::champion_identity(&db, crate::deployment::OMNIASR_7B_FAMILY)
            .map_err(|error| {
                AppError::Validation(format!(
                    "{ASR_7B_UNAVAILABLE_TAG}: champion registry identity could not be read: {error}"
                ))
            })?
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{ASR_7B_UNAVAILABLE_TAG}: no content-addressed OmniASR-7B champion is registered; refusing an identity-free server"
                ))
            })?;
        let loaded = crate::engine_runtime::query_loaded_champion_with_client(&client, Duration::from_secs(10))
            .map_err(|error| {
                AppError::Validation(format!(
                    "{ASR_7B_UNAVAILABLE_TAG}: exact champion health check failed: {error}. The import was not started."
                ))
            })?;
        if !loaded.matches(&expected) {
            return Err(AppError::Validation(format!(
                "{ASR_7B_UNAVAILABLE_TAG}: server identity {}/{} does not match registry champion {}/{}; refusing to write transcripts",
                loaded.model_version_id,
                loaded.deployment_sha256,
                expected.model_version_id,
                expected.deployment_sha256
            )));
        }
        Ok(())
    }

    /// The actionable, UI-classified error returned whenever [`Self::wsl7b_primary_unresolved`]
    /// holds. Carries [`ASR_7B_UNAVAILABLE_TAG`] so the frontend presents the safe recovery path:
    /// configure/start the 7B service and retry the champion. No smaller-engine substitution is
    /// offered from the production flow.
    fn primary_engine_unavailable_error() -> AppError {
        // Tagged so the UI can offer a champion retry rather than a dead-end (see
        // ASR_7B_UNAVAILABLE_TAG). The app never silently downgrades to a smaller model.
        AppError::Validation(format!(
            "{ASR_7B_UNAVAILABLE_TAG}: OmniASR-7B is selected but the bundled WSL client could not be \
             resolved. Repair/reinstall the app resources (or configure an explicit verified client \
             path), then retry. Refusing to silently downgrade to a smaller model."
        ))
    }

    fn local_asr_model_id(&self) -> &'static str {
        match self.selected_asr_model_size() {
            crate::settings::AsrModelSize::CTC1B => "omniasr-ctc-1b",
            crate::settings::AsrModelSize::CTC300M => "omniasr-ctc-300m",
            crate::settings::AsrModelSize::WSL7B => "omniasr-wsl-7b",
        }
    }

    fn with_asr<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut asr::KurdishAsrService) -> R,
    {
        let model_dir = self.asr_model_root();
        self.asr_pool.with_service(&model_dir, &self.asr_config(), f)
    }

    /// Run the shipped gold-set eval against the exact registered OmniASR-7B champion. The renderer
    /// supplies no model label, and every reply is checked against the durable registry identity
    /// before its text can enter an eval row.
    pub fn run_gold_eval_asr(&self) -> AppResult<crate::eval::EvalRunResult> {
        if self.selected_asr_model_size() != crate::settings::AsrModelSize::WSL7B {
            return Err(AppError::Validation(
                "Production gold evaluation requires the pinned OmniASR-7B champion; use an explicit offline diagnostic tool for auxiliary engines."
                    .into(),
            ));
        }
        self.preflight_primary_engine()?;
        let db = self.open_db()?;
        let expected =
            crate::registry::champion_identity(&db, crate::deployment::OMNIASR_7B_FAMILY)?.ok_or_else(|| {
                AppError::Validation("No registered OmniASR-7B champion is available for evaluation".into())
            })?;
        let model_id = expected.model_version_id.clone();
        crate::eval::run_gold_eval_with_transcriber(&db, &model_id, |seg| {
            let result = self.run_wsl_segment_transcript(&seg.audio_path, None, None)?;
            require_exact_champion_result(&result, &expected)?;
            Ok(result.raw_transcript)
        })
    }

    pub fn import_status_handle(&self) -> Arc<Mutex<ImportStatus>> {
        Arc::clone(&self.import_status)
    }

    fn lock_import_status(&self) -> MutexGuard<'_, ImportStatus> {
        self.import_status.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned import status lock");
            poisoned.into_inner()
        })
    }

    fn lock_diarization_service(&self) -> MutexGuard<'_, Option<crate::diarization::SpeakerEmbeddingService>> {
        self.diarization_service.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned diarization service lock");
            poisoned.into_inner()
        })
    }

    fn lock_denoiser_service(&self) -> MutexGuard<'_, Option<crate::denoiser::DenoiserService>> {
        self.denoiser_service.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned denoiser service lock");
            poisoned.into_inner()
        })
    }

    pub fn import_status(&self) -> ImportStatus {
        self.lock_import_status().clone()
    }

    fn set_import_status(&self, current: usize, total: usize, file: &str) {
        let mut status = self.lock_import_status();
        status.running = true;
        status.current = current;
        status.total = total;
        status.file = file.to_string();
    }

    fn finish_import_status(&self) {
        self.lock_import_status().running = false;
    }

    fn open_db(&self) -> AppResult<Database> {
        Database::open(&self.db_path)
    }

    fn source_transcript_dir(&self) -> Option<PathBuf> {
        Path::new(&self.db_path).parent().map(|dir| dir.join("source_transcripts"))
    }

    /// The Gemini key for the whole-file reference transcript, read from the ENCRYPTED store.
    ///
    /// NOT `settings.llm_api_key`, which is where this used to look. `AppSettings::load` deliberately
    /// CLEARS that field and rewrites settings.json, so a plaintext key never survives on disk (P0.3).
    /// The consequence was that the field is empty on every run after the one where it was typed, so
    /// `ensure_source_reference_transcripts` failed with "Gemini API key is required" no matter what the
    /// owner did — and `llm_api_key_configured` stayed true, so the UI reported a key that was gone.
    /// Measured 2026-08-10: an import of three files failed 3/3 on exactly that error while
    /// secrets.env held a working OpenRouter key and an EMPTY GEMINI_API_KEY.
    ///
    /// `secrets.env` via ApiKeys is where the jury and OpenRouter paths already look, so this makes the reference transcript agree with the rest of the
    /// crate instead of being the one caller reading a field that is guaranteed empty.
    ///
    /// The in-memory settings field is still honoured as a fallback: within the single session where
    /// the owner has just typed a key, it holds the value before any reload scrubs it, and refusing it
    /// there would be a surprising "I just entered it" failure.
    fn jury_cloud_api_key(&self) -> Option<String> {
        let from_store =
            Path::new(&self.db_path).parent().and_then(|data_dir| crate::api_keys::ApiKeys::load(data_dir).gemini);
        from_store.or_else(|| {
            let typed = self.settings.llm_api_key.trim();
            (!typed.is_empty()).then(|| typed.to_string())
        })
    }

    fn reusable_source_reference_record(
        &self,
        db: &Database,
        existing: &SourceTranscriptRecord,
        current_identity: Option<&SourceAudioIdentity>,
    ) -> AppResult<Option<SourceTranscriptRecord>> {
        let Some(current_identity) = current_identity else {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the current audio file identity could not be verified",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        };
        let identity_matches = existing.audio_content_hash.as_deref() == Some(current_identity.content_hash.as_str())
            && existing.audio_size_bytes == Some(current_identity.size_bytes);
        if !identity_matches {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the stored audio identity does not match the current file",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        }

        if !crate::agentic::is_usable_source_reference_transcript(&existing.transcript_text) {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the stored DB text is empty or unusable",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        }

        let transcript_path = Path::new(&existing.transcript_path);
        let saved_text = match std::fs::read_to_string(transcript_path) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(
                    "Ignoring cached whole-file reference transcript for {} with {} because '{}' could not be read: {}",
                    existing.audio_path,
                    existing.model_id,
                    existing.transcript_path,
                    error
                );
                return Ok(None);
            }
        };
        if !crate::agentic::is_usable_source_reference_transcript(&saved_text) {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because '{}' is empty or unusable",
                existing.audio_path,
                existing.model_id,
                existing.transcript_path
            );
            return Ok(None);
        }

        let saved_text = saved_text.trim().to_string();
        if saved_text == existing.transcript_text.trim() {
            return Ok(Some(existing.clone()));
        }

        let synced = SourceTranscriptRecord {
            transcript_text: saved_text,
            created_at: existing.created_at.clone(),
            ..existing.clone()
        };
        db.upsert_source_transcript(&synced)?;
        tracing::info!(
            "Synced cached whole-file reference transcript for {} with {} from edited text file '{}'",
            existing.audio_path,
            existing.model_id,
            existing.transcript_path
        );
        Ok(Some(synced))
    }

    fn ensure_source_reference_transcripts(
        &self,
        path: &Path,
        db: &Database,
    ) -> AppResult<Vec<SourceTranscriptRecord>> {
        // Snapshot AND live consent: a withdrawal after this import began must stop the upload.
        if !self.settings.jury_cloud_opt_in || !self.consent.jury_cloud() {
            return Ok(Vec::new());
        }
        let Some(api_key) = self.jury_cloud_api_key() else {
            return Err(AppError::Other(
                "Gemini API key is required for whole-file reference transcript when jury cloud opt-in \
                 is enabled. Save it from Settings (it goes to secrets.env, DPAPI-encrypted); note that \
                 settings.json is NOT a place a key can live - AppSettings::load scrubs it by design."
                    .to_string(),
            ));
        };

        let audio_path = path.to_string_lossy().to_string();
        let output_dir = self
            .source_transcript_dir()
            .ok_or_else(|| AppError::Other("Cannot resolve app data directory for source transcripts".into()))?;
        let current_identity = match source_audio_identity(path) {
            Ok(identity) => Some(identity),
            Err(error) => {
                tracing::warn!(
                    "Cannot verify current audio identity for whole-file source transcript cache at {}: {}",
                    path.display(),
                    error
                );
                None
            }
        };
        let mut records = Vec::new();
        let mut errors = Vec::new();

        for model in self.settings.source_reference_models() {
            if let Some(existing) = db.get_source_transcript(&audio_path, &model)? {
                if let Some(existing) =
                    self.reusable_source_reference_record(db, &existing, current_identity.as_ref())?
                {
                    tracing::info!(
                        "Reusing whole-file reference transcript for {} from {}",
                        path.display(),
                        existing.transcript_path
                    );
                    records.push(existing);
                    continue;
                }
            }

            match crate::agentic::generate_whole_file_reference_transcript(path, &model, &api_key, &output_dir) {
                Ok(artifact) => {
                    let identity =
                        current_identity.as_ref().cloned().or_else(|| source_audio_identity(path).ok()).ok_or_else(
                            || {
                                AppError::Other(format!(
                                "Cannot verify audio identity after generating whole-file reference transcript for {}",
                                path.display()
                            ))
                            },
                        )?;
                    let record = SourceTranscriptRecord {
                        audio_path: artifact.audio_path,
                        model_id: artifact.model_id,
                        audio_content_hash: Some(identity.content_hash),
                        audio_size_bytes: Some(identity.size_bytes),
                        transcript_path: artifact.transcript_path,
                        transcript_text: artifact.transcript_text,
                        created_at: None,
                    };
                    db.upsert_source_transcript(&record)?;
                    records.push(record);
                }
                Err(error) => {
                    tracing::warn!(
                        "Whole-file reference transcript failed for {} with {}: {}",
                        path.display(),
                        model,
                        error
                    );
                    errors.push(format!("{model}: {error}"));
                }
            }
        }

        if !errors.is_empty() {
            let scope = if records.is_empty() { "All" } else { "Some" };
            return Err(AppError::Other(format!(
                "{scope} whole-file reference transcript models failed before chunking; refusing to continue with incomplete source-reference evidence: {}",
                errors.join("; ")
            )));
        }
        Ok(records)
    }

    pub fn import_directory(
        &self,
        dir_path: &Path,
        cancel: Option<CancellationToken>,
        callback: impl Fn(PipelineEvent),
    ) -> AppResult<()> {
        self.import_directory_with_agent_run_id(dir_path, cancel, None, None, callback)
    }

    /// `resume_completed`: when resuming a crashed import, the set of file paths already imported in the
    /// interrupted run — they are skipped (their segments already persisted, per-file). `None` for a
    /// normal import, so the default path's behavior is unchanged.
    pub fn import_directory_with_agent_run_id(
        &self,
        dir_path: &Path,
        cancel: Option<CancellationToken>,
        agent_run_id: Option<&str>,
        resume_completed: Option<&std::collections::HashSet<String>>,
        callback: impl Fn(PipelineEvent),
    ) -> AppResult<()> {
        let db = self.open_db()?;
        let audio_exts = ["wav", "mp3", "flac", "m4a", "ogg", "aac", "opus", "mp4", "mov", "wma", "webm"];
        let mut files = Vec::new();

        fn collect_audio_files(
            dir: &Path,
            exts: &[&str],
            files: &mut Vec<std::path::PathBuf>,
            depth: usize,
        ) -> std::io::Result<()> {
            if depth > 32 {
                return Ok(());
            }
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    collect_audio_files(&path, exts, files, depth + 1)?;
                } else if path.is_file() {
                    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).unwrap_or_default();
                    if exts.contains(&ext.as_str()) {
                        files.push(path);
                    }
                }
            }
            Ok(())
        }

        collect_audio_files(dir_path, &audio_exts, &mut files, 0)?;

        let source_paths: Vec<String> = files.iter().map(|path| path.to_string_lossy().to_string()).collect();
        let total = files.len();
        callback(PipelineEvent::Started { total });
        self.reset_finetuned_counters();
        callback(PipelineEvent::Phase { phase: "importing".into() });
        self.set_import_status(0, total, "");
        // An empty selection is a successful no-op, not an import generation.  Recording a completed
        // zero-file job pollutes the recovery journal and makes an accidental folder pick look like
        // durable work.  Preserve the public event contract while leaving both journal tables clean.
        if total == 0 {
            callback(PipelineEvent::Completed { total: 0, succeeded: 0, failed: 0 });
            self.finish_import_status();
            return Ok(());
        }
        // P3.2: open a resume journal for this import (best-effort — a journal failure never fails the
        // import). A crash leaves this job 'running'; the next launch can offer to resume it.
        let job_id: Option<String> = db.begin_import_job(&dir_path.to_string_lossy(), total).ok();
        // RAII: clear import_status.running on EVERY exit path. The per-file `token.check()?` cancel
        // below early-returns before the manual finish_import_status() calls, which used to leave
        // get_import_status() reporting running:true forever after a cancelled directory import.
        struct ImportStatusGuard<'a>(&'a ProcessingPipeline);
        impl Drop for ImportStatusGuard<'_> {
            fn drop(&mut self) {
                self.0.finish_import_status();
            }
        }
        let _status_guard = ImportStatusGuard(self);
        let mut succeeded = 0;
        let failed = 0; // halt-on-first-failure (2026-08-20): a COMPLETED import has zero failures by definition
        let mut imported_ids = Vec::new();

        for (idx, file) in files.iter().enumerate() {
            if let Some(ref token) = cancel {
                token.check()?;
            }

            let fname = file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
            let file_path_str = file.to_string_lossy().to_string();

            // P3.2 + resume-journal-gap fix: on resume, skip (adopt, never re-persist) any file whose
            // segments are already in the DB. A file is "already done" two ways: (1) the resume journal
            // recorded it, or (2) it is NOT journaled yet its rows exist — the crash landed in the window
            // between persist_segments (an atomic batch commit) and mark_import_file_done, so the journal
            // never saw it. Without case (2), resume reprocesses the in-flight file and DUPLICATES every
            // segment. Query the existing ids once (resume only), then let resume_should_skip_file decide.
            let resume_existing_ids: Vec<String> = if resume_completed.is_some() {
                db.segment_ids_for_audio_path(&file_path_str).unwrap_or_else(|e| {
                    tracing::warn!("resume: could not fetch segment ids for {file_path_str}: {e}");
                    Vec::new()
                })
            } else {
                Vec::new()
            };
            let journaled = resume_completed.is_some_and(|done| done.contains(&file_path_str));
            // An UN-journaled file whose rows still hold placeholders/empty drafts is an interrupted
            // STAGE, not a completed file (2026-08-20 external review; the 2026-08-14 incident's 36
            // rows are exactly this state). Adopting it would publish a file the champion never
            // finished. Discard the stage and re-import it from scratch — the per-file pipeline is
            // the unit of atomicity, and the resume journal marks only files that truly finished.
            // An unreadable placeholder check counts as incomplete: fail toward re-doing work, never
            // toward publishing an unfinished stage.
            let staged_incomplete = resume_completed.is_some()
                && !journaled
                && !resume_existing_ids.is_empty()
                && db.audio_path_has_placeholder_rows(&file_path_str).unwrap_or(true);
            if staged_incomplete {
                tracing::warn!(
                    "resume: {} left {} staged row(s) from an interrupted import — discarding the stage and re-importing",
                    file_path_str,
                    resume_existing_ids.len()
                );
                db.delete_segments_batch(&resume_existing_ids)?;
            }
            if resume_should_skip_file(
                resume_completed.is_some(),
                journaled,
                !resume_existing_ids.is_empty() && !staged_incomplete,
            ) {
                succeeded += 1;
                if let Some(ref jid) = job_id {
                    let _ = db.mark_import_file_done(jid, &file_path_str);
                }
                // Fold the already-imported file's segments back into the jury batch. The post-import
                // jury (below) runs once at the end keyed on `imported_ids`; a crash interrupts BEFORE
                // that jury ever runs, so segments persisted pre-crash were never adjudicated. Skipping
                // them silently would leave them persisted-but-un-adjudicated (no reference commit, no
                // review routing). Adopt the ids fetched above so the end-of-run jury covers the whole
                // resumed import. Non-destructive: existing rows are adopted, never deleted (a reviewed
                // earlier import may legitimately share this audio_path).
                imported_ids.extend(resume_existing_ids);
                callback(PipelineEvent::Progress {
                    current: idx + 1,
                    total,
                    file: fname.clone(),
                    status: "Already imported — re-adjudicating (resume)".into(),
                });
                continue;
            }

            callback(PipelineEvent::Progress {
                current: idx + 1,
                total,
                file: fname.clone(),
                status: "Processing...".into(),
            });
            self.set_import_status(idx + 1, total, &fname);
            callback(PipelineEvent::Phase { phase: "reference_transcribing".into() });
            callback(agent_stage(
                "source_reference",
                "running",
                fname.clone(),
                "Building whole-file reference transcript",
                idx + 1,
                total,
            ));
            callback(PipelineEvent::Progress {
                current: idx + 1,
                total,
                file: fname.clone(),
                status: "Building whole-file reference transcript".into(),
            });

            let meta = crate::telemetry::Tracer::metadata(vec![
                ("file", fname.clone()),
                ("path", file.to_string_lossy().to_string()),
                ("index", (idx + 1).to_string()),
                ("total", total.to_string()),
            ]);
            // Thread the directory-import cancel token into per-file processing so Cancel interrupts the
            // CURRENT file (its VAD/ASR/per-segment 7B loop), not only the gap between files — a long
            // audiobook file could otherwise keep running for minutes after Cancel.
            let mut result = crate::telemetry::TRACER.record_result("pipeline.import_file", meta, || {
                self.process_single_file_with_progress(file, &db, cancel.as_ref(), |_, _| {})
            });

            if let Err(ref e) = result {
                if audio::is_transient_decode_error(e) {
                    tracing::warn!("Transient decode error for {}, retrying once: {e}", file.display());
                    std::thread::sleep(Duration::from_millis(500));
                    result = self.process_single_file_with_progress(file, &db, cancel.as_ref(), |_, _| {});
                }
            }

            match result {
                Ok(segments) => {
                    callback(PipelineEvent::Phase { phase: "transcribing".into() });
                    let segment_count = segments.len();
                    callback(agent_stage(
                        "source_reference",
                        "completed",
                        fname.clone(),
                        "Whole-file source reference stage completed or reused",
                        idx + 1,
                        total,
                    ));
                    callback(agent_stage(
                        "audio_chunking",
                        "completed",
                        fname.clone(),
                        format!("{segment_count} speech chunk(s) persisted"),
                        segment_count,
                        segment_count.max(1),
                    ));
                    callback(multi_model_hypothesis_stage(&db, &self.settings, fname.clone(), &segments));
                    succeeded += 1;
                    // P3.2: record this file as done in the resume journal (best-effort).
                    if let Some(ref jid) = job_id {
                        let _ = db.mark_import_file_done(jid, &file_path_str);
                    }
                    imported_ids.extend(segments.iter().map(|s| s.id.clone()));
                    if segments.len() > 1 {
                        tracing::info!("Imported {} annotatable segments from {}", segments.len(), file.display());
                    }
                }
                Err(e) => {
                    // HALT ON THE FIRST REAL FAILURE (owner rule 2026-08-11; wired here 2026-08-20).
                    // This arm used to count the failure and continue to the next file, ending with
                    // `Completed { failed: n }` and Ok(()) — the exact "partly-drafted dataset that
                    // looks finished" the champion law forbids, one directory level up from where
                    // batch_transcribe already halts. The resume journal makes halting cheap: every
                    // finished file is journaled, so re-running the import picks up exactly here.
                    callback(PipelineEvent::Error { file: fname.clone(), error: e.to_string() });
                    // _status_guard's Drop clears the running flag on this early return.
                    return Err(AppError::Other(format!(
                        "import HALTED at {fname} ({succeeded} file(s) completed before it): {e}. \
                         Nothing after it was attempted — fix the cause and re-import; completed files resume as done."
                    )));
                }
            }
        }

        if !imported_ids.is_empty() {
            callback(PipelineEvent::Phase { phase: "adjudicating".into() });
            callback(agent_stage(
                "jury_adjudication",
                "running",
                "post-import jury",
                format!("Adjudicating {} imported segment(s)", imported_ids.len()),
                0,
                imported_ids.len(),
            ));
            let mut report_options = crate::runs::AgentImportReportOptions::from_settings(&self.settings);
            report_options.agent_run_id = agent_run_id.map(str::to_string);
            let model_status = self.model_manager.status();
            let external_provider = crate::commands::external_provider_status(&self.settings);
            report_options.agentic_readiness = Some(crate::commands::build_agentic_readiness_snapshot(
                &self.settings,
                &model_status,
                &external_provider,
            ));
            match crate::commands::run_jury_pipeline_core(&db, &self.settings, imported_ids.clone()) {
                Ok(jury_report) => {
                    callback(agent_stage(
                        "jury_adjudication",
                        "completed",
                        "post-import jury",
                        format!(
                            "Reference commits: {}; review queue: {}",
                            jury_report["referenceCommitted"].as_u64().unwrap_or(0),
                            jury_report["humanInbox"].as_u64().unwrap_or(0)
                        ),
                        imported_ids.len(),
                        imported_ids.len(),
                    ));
                    if let Err(error) = crate::runs::record_agent_import_report_with_options(
                        &db,
                        "directory",
                        &source_paths,
                        &imported_ids,
                        Some(&jury_report),
                        None,
                        report_options,
                    ) {
                        let message = format!("Agent import report persistence failed after directory import: {error}");
                        tracing::error!("{message}");
                        callback(PipelineEvent::Error { file: "agent import report".into(), error: message.clone() });
                        self.finish_import_status();
                        return Err(AppError::Other(message));
                    }
                    callback(agent_stage(
                        "agent_report",
                        "completed",
                        "agent import report",
                        "Persisted auditable multi-agent import report",
                        imported_ids.len(),
                        imported_ids.len(),
                    ));
                }
                Err(error) => {
                    let mut message = format!("Post-import jury adjudication failed after directory import: {error}");
                    if let Err(report_error) = crate::runs::record_agent_import_report_with_options(
                        &db,
                        "directory",
                        &source_paths,
                        &imported_ids,
                        None,
                        Some(&error),
                        report_options,
                    ) {
                        message
                            .push_str(&format!("; additionally failed to persist agent import report: {report_error}"));
                    }
                    tracing::error!("{message}");
                    callback(agent_stage(
                        "jury_adjudication",
                        "blocked",
                        "post-import jury",
                        message.clone(),
                        0,
                        imported_ids.len(),
                    ));
                    callback(PipelineEvent::Error { file: "post-import jury".into(), error: message.clone() });
                    self.finish_import_status();
                    return Err(AppError::Other(message));
                }
            }
        }

        // P3.2: a clean finish — the job is no longer an interruption to resume (best-effort).
        if let Some(ref jid) = job_id {
            let _ = db.complete_import_job(jid);
        }
        // F2: a fine-tuned→stock downgrade during this import must end LOUD, not log-only.
        {
            let attempts = self.finetuned_attempts.load(std::sync::atomic::Ordering::Relaxed);
            let fallbacks = self.finetuned_fallbacks.load(std::sync::atomic::Ordering::Relaxed);
            if let Some(error) = Self::finetuned_downgrade_message(attempts, fallbacks) {
                tracing::error!("finetuned downgrade on import: {error}");
                callback(PipelineEvent::Error { file: "fine-tuned engine".into(), error });
            }
        }
        callback(PipelineEvent::Completed { total, succeeded, failed });
        self.finish_import_status();
        Ok(())
    }

    /// Decode one source file and persist one or more `SpeechSegment` rows (VAD chunking for long audio).
    pub fn process_single_file(&self, path: &Path, db: &Database) -> AppResult<Vec<SpeechSegment>> {
        self.process_single_file_with_progress(path, db, None, |_, _| {})
    }

    fn process_single_file_with_progress(
        &self,
        path: &Path,
        db: &Database,
        cancel: Option<&CancellationToken>,
        mut on_chunk: impl FnMut(usize, usize),
    ) -> AppResult<Vec<SpeechSegment>> {
        if let Some(token) = cancel {
            token.check()?;
        }

        let duration_ms = audio::get_duration_ms(path)?;
        if duration_ms == 0 {
            return Err(AppError::Validation("Empty audio file".into()));
        }

        // Before anything is decoded: if this recording was PROCESSED before it reached the app (the
        // pre-import cleaner separates voice from music, cuts the non-speech out, and normalises the
        // level), record that now, keyed by source path. Its clips are about to become
        // indistinguishable from raw field recordings in every export unless the library says
        // otherwise. Best-effort and never fatal — a failed stamp must not fail an import whose
        // audio is fine — but it WARNs, because what is lost is a provenance claim, not audio.
        if let Some(provenance) = crate::source_provenance::detect(path) {
            tracing::info!("source audio declared as processed before import: {}", provenance.processing);
            if let Err(e) = db.upsert_source_audio_provenance(&provenance) {
                tracing::warn!("could not record source audio provenance for {}: {e}", path.display());
            }
        }

        // F2: fail fast BEFORE any decode/VAD/diarization work if the selected primary engine can't
        // actually run — never silently transcribe the whole import with the stock model.
        if self.wsl7b_primary_unresolved() {
            return Err(Self::primary_engine_unavailable_error());
        }
        // F6: when the WSL 7B is primary, confirm its warm server is up before doing any work, so a
        // down server fails in ~2 s with an actionable message instead of a ~5-minute per-segment hang.
        self.wsl_7b_server_preflight()?;

        self.ensure_source_reference_transcripts(path, db).map_err(|error| {
            AppError::Other(format!(
                "Whole-file reference transcript failed before chunking {}: {error}",
                path.display()
            ))
        })?;
        if let Some(token) = cancel {
            token.check()?;
        }

        let decode_timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);

        if chunking::should_stream_decode(duration_ms, self.settings.max_segment_duration_ms) {
            return self.process_single_file_streaming(path, db, decode_timeout, duration_ms, cancel, on_chunk);
        }

        let (sample_rate, pcm) = audio::decode_to_pcm_with_timeout(path, decode_timeout)?;

        if pcm.is_empty() {
            return Err(AppError::Validation("Empty audio buffer".into()));
        }

        let (sample_rate, pcm) = audio::ensure_pcm_16khz(sample_rate, pcm)?;

        if pcm.len() > MAX_PCM_SAMPLES {
            tracing::warn!(
                "Decoded audio exceeds memory cap ({} samples, ~{} min); chunking will bound each segment",
                pcm.len(),
                pcm.len() / sample_rate as usize / 60
            );
        }

        let identity = self
            .fingerprint
            .check_and_register(&pcm, sample_rate, Some(path))
            .map_err(|e| AppError::Validation(e.into()))?;
        // v50: the value used to be computed here and thrown away as `_fp`, which is why duplicate
        // detection could not survive a restart. Stamped onto the rows AFTER persist_segments below,
        // once they exist — see the set_audio_identity call there. v51: BOTH tiers travel together, so
        // the rejection rule after a restart is the same cryptographic one it is during this run.

        // The embedding service is acquired BEFORE chunk planning (it used to come after), because the
        // planner now asks it who is speaking at every candidate merge: boundaries were planned by
        // silence alone and labels attached to whole chunks afterwards, so a two-host podcast glued
        // both voices into one chunk under one confident SPEAKER_0x (owner hit this twice reviewing,
        // 2026-08-17). The judge can only REFUSE a merge — with CAM++ absent it returns None and the
        // plan is exactly the historical silence-only one.
        let mut diarization_guard = self.lock_diarization_service();
        // Rebuild when unset OR cached-INACTIVE (see the denoiser site below): caching an inactive
        // service ignored a CAM++ model downloaded mid-session until an app restart. Cheap while absent.
        if diarization_guard.as_ref().map_or(true, |s| !s.is_available()) {
            // Per-file (round-26): resolve_root_for avoids resolved_dir()'s all-or-nothing orphan of the
            // bundled-only campp speaker model once the user downloads OmniASR into the user dir.
            let model_dir = self.model_manager.resolve_root_for(crate::models::CAMPP_MODEL);
            *diarization_guard = Some(crate::diarization::SpeakerEmbeddingService::new(&model_dir));
        }
        let embedding_service = diarization_guard
            .as_ref()
            .ok_or_else(|| AppError::Other("Failed to initialize diarization service".into()))?;

        let judge = crate::diarization::speaker_turn_judge(embedding_service, sample_rate);
        let (chunk_ranges, vad_backend) = chunking::plan_speech_chunks_with_judge(
            &pcm,
            sample_rate,
            self.settings.vad_threshold,
            self.settings.min_segment_duration_ms,
            self.settings.max_segment_duration_ms,
            Some(&judge),
        )?;

        let mut denoiser_guard = self.lock_denoiser_service();
        // Rebuild when unset OR cached-INACTIVE: an inactive service means the model was absent when it
        // was first built, so caching that pass-through for the whole session ignored a denoiser
        // downloaded mid-session until an app restart (hunt-10 #3) — and the export's fresh-service
        // denoising flag then read `true` over un-denoised audio. The absent-path rebuild is a cheap
        // path.exists() stat; once the model appears the load runs once and is_active() latches true.
        if denoiser_guard.as_ref().map_or(true, |s| !s.is_active()) {
            // Per-file (round-26): resolved_dir() is all-or-nothing, so a bundled-only or user-downloaded
            // denoiser is orphaned once OmniASR flips the root. resolve_root_for loads it from wherever it is.
            let model_dir = self.model_manager.resolve_root_for(crate::models::DENOISER_MODEL);
            *denoiser_guard = Some(crate::denoiser::DenoiserService::new(&model_dir));
        }
        let denoiser_service =
            denoiser_guard.as_ref().ok_or_else(|| AppError::Other("Failed to initialize denoiser service".into()))?;

        // Once per file — see the parameter's doc on build_segments_from_pcm.
        let file_hash = crate::cache::TranscriptCache::compute_hash(path).ok();
        let (segments, pcm_cache) = self.build_segments_from_pcm(
            path,
            &pcm,
            sample_rate,
            0,
            &chunk_ranges,
            vad_backend,
            cancel,
            embedding_service,
            denoiser_service,
            &mut on_chunk,
            None, // non-streaming: the whole file is one call, so diarization clusters in-place
            file_hash.as_deref(),
        )?;
        let mut persisted = self.persist_segments(db, segments)?;
        // v50: persist the fingerprint now that the rows exist, so the NEXT session rehydrates it and
        // re-importing this recording under a different path is rejected then too. Best-effort: a failed
        // stamp must not fail an import whose audio and transcripts are already committed — it only costs
        // this recording its place in cross-session dedup, and a WARN says so.
        if identity.spectral != 0 {
            if let Err(e) = db.set_audio_identity(&path.to_string_lossy(), &identity) {
                tracing::warn!("audio identity not persisted for {}: {e}", path.display());
            }
        }
        self.run_primary_wsl_pass_for_import(db, &mut persisted, cancel)?;
        // Deferred to AFTER the 7B pass so both evaluate the real transcript, not the placeholder, and
        // so alignment does not clobber the slice offsets the pass depends on. See persist_segments.
        self.shadow_log_loop0(db, &persisted);
        self.enqueue_background_alignments(&persisted);
        {
            let primary_by_segment: HashMap<&str, PrimaryHypothesis<'_>> = persisted
                .iter()
                .filter_map(|segment| {
                    PrimaryHypothesis::from_segment(segment).map(|primary| (segment.id.as_str(), primary))
                })
                .collect();
            for (seg_id, f32_pcm) in pcm_cache {
                let primary = primary_by_segment.get(seg_id.as_str()).copied();
                if let Err(error) = self.populate_hypotheses_reusing_primary(db, &seg_id, &f32_pcm, primary) {
                    log_hypothesis_population_failure(&seg_id, &error);
                }
            }
        }
        Ok(persisted)
    }

    fn process_single_file_streaming(
        &self,
        path: &Path,
        db: &Database,
        decode_timeout: Duration,
        duration_ms: i64,
        cancel: Option<&CancellationToken>,
        mut on_chunk: impl FnMut(usize, usize),
    ) -> AppResult<Vec<SpeechSegment>> {
        let estimated_total =
            ((duration_ms as f64 / self.settings.max_segment_duration_ms.max(1) as f64).ceil() as usize).max(1);
        let mut global_chunk = 0usize;

        let mut segments = Vec::new();
        let mut all_pcm_cache = Vec::new();
        let mut windows_seen = 0usize;
        // Carry the final chunk of each 90 s decode window into the next one. That chunk touches the
        // hard window edge, so re-chunking it together with the following audio lets the silence-aware
        // splitter cut on a pause instead of guillotining a word across the boundary (which made the 7B
        // re-emit the straddling word — e.g. "پێداویستە سەرەتایەکانی" was duplicated across a 180 s seam).
        let mut carry_pcm: Vec<i16> = Vec::new();
        let mut carry_base: usize = 0;
        let mut sample_rate_seen: u32 = 16_000;
        // Accumulate one speaker embedding per retained segment across ALL decode windows, so speakers
        // are clustered over the WHOLE file once (below) rather than re-clustered per 90s window.
        let mut all_embeddings: Vec<Vec<f32>> = Vec::new();
        // P1.4b (audit R4): the per-window rebuild-when-inactive below (fix #132, for a model that appears
        // mid-session) re-attempted a full GPU-then-CPU ONNX load on EVERY 90 s window for a PRESENT-but-
        // unloadable model. These flags bound the (re)build to at most ONCE per FILE — matching the
        // non-streaming sibling that builds once per file — so a corrupt/unloadable denoiser/CAM++ is not
        // reloaded per window. A NEW file (new streaming call) resets them, so a between-file download
        // still recovers (#132's intent, at file granularity).
        let mut diarization_rebuild_tried = false;
        let mut denoiser_rebuild_tried = false;
        // v51: accumulate ONE whole-recording identity across the windows. Before this, the streaming
        // path fingerprinted each window, discarded every value, and persisted nothing — so a long file
        // (the only kind that reaches this path) never participated in cross-session duplicate detection
        // at all. blake3 streams, so this costs no extra memory and yields exactly the digest the
        // non-streaming path would have computed for the same canonical PCM.
        let mut recording_identity = crate::fingerprint::StreamingIdentity::new();
        // Round-23 #5, corrected 2026-08-17: hash the source ONCE PER FILE. This used to live inside
        // build_segments_from_pcm, which the streaming path calls once per 90 s window — so a long
        // import re-read and re-hashed the entire source file for every window. Measured on the
        // library's longest source (KBHP-EP12.wav, 5,315 s / 162 MB): 60 full-file hashes instead of
        // one. The old comment claiming "once for the whole run" was only ever true of the
        // non-streaming sibling. `None` means "no cache for this run", exactly as before.
        let file_hash = crate::cache::TranscriptCache::compute_hash(path).ok();

        // Consume each decode window as it arrives instead of collecting them all first. Peak PCM
        // held is now bounded by a handful of windows (≤ 4 × 90 s ≈ 11.5 MB) instead of the whole
        // recording — 170 MB for that same KBHP-EP12, and unbounded in the file's length.
        let window_timeout = decode_timeout.min(MAX_WINDOW_DECODE_WAIT);
        let process_window = |window: audio::PcmWindow, is_last: bool| -> AppResult<()> {
            windows_seen += 1;
            if let Some(token) = cancel {
                token.check()?;
            }

            let (sample_rate, win_pcm) = if window.pcm.is_empty() {
                (sample_rate_seen, Vec::new())
            } else {
                let (sr, p) = audio::ensure_pcm_16khz(window.sample_rate, window.pcm)?;
                sample_rate_seen = sr;
                // Fingerprint only freshly-decoded audio, never the carried-over tail — pushing a carry
                // twice would change the whole-file digest and break the equality with the
                // non-streaming path.
                //
                // The per-window check stays: it fails fast, before the ASR cost, when this session has
                // already seen the same audio. The accumulated whole-file identity below is what gets
                // PERSISTED, so the next session catches it too.
                self.fingerprint.check_and_register(&p, sr, Some(path)).map_err(|e| AppError::Validation(e.into()))?;
                recording_identity.push(&p, sr);
                (sr, p)
            };

            // Prepend the previous window's carried-over tail (contiguous audio) before chunking.
            let (effective_pcm, base_sample) = if carry_pcm.is_empty() {
                let base = chunking::ms_to_samples(window.offset_ms.max(0) as u32, sample_rate);
                (win_pcm, base)
            } else {
                let mut v = std::mem::take(&mut carry_pcm);
                let base = carry_base;
                v.extend_from_slice(&win_pcm);
                (v, base)
            };
            if effective_pcm.is_empty() {
                return Ok(());
            }
            let pcm = effective_pcm;

            // Service before planning, same reorder and same reason as the non-streaming sibling: the
            // planner asks who is speaking before agreeing to a silence-approved merge. The rebuild
            // policy (at most one attempt per file) is unchanged — the block simply moved up.
            let mut diarization_guard = self.lock_diarization_service();
            // Rebuild when unset, OR when cached-inactive AND we have not yet tried this file (P1.4b:
            // don't re-attempt an unloadable CAM++ every window — at most once per file). See the
            // non-streaming sibling site.
            if should_rebuild_streaming_service(
                diarization_guard.is_some(),
                diarization_guard.as_ref().is_some_and(|s| s.is_available()),
                diarization_rebuild_tried,
            ) {
                diarization_rebuild_tried = true;
                // Per-file (round-26): see the sibling site — resolve_root_for avoids the all-or-nothing orphan.
                let model_dir = self.model_manager.resolve_root_for(crate::models::CAMPP_MODEL);
                *diarization_guard = Some(crate::diarization::SpeakerEmbeddingService::new(&model_dir));
            }
            let embedding_service = diarization_guard
                .as_ref()
                .ok_or_else(|| AppError::Other("Failed to initialize diarization service".into()))?;

            let judge = crate::diarization::speaker_turn_judge(embedding_service, sample_rate);
            let (mut chunk_ranges, vad_backend) = chunking::plan_speech_chunks_with_judge(
                &pcm,
                sample_rate,
                self.settings.vad_threshold,
                self.settings.min_segment_duration_ms,
                self.settings.max_segment_duration_ms,
                Some(&judge),
            )?;

            // Hold back the boundary-touching tail of every non-final window for the next round so the
            // splitter can later cut it on a pause. Carry from the last chunk's START all the way to the
            // window END (`pcm[ls..]`), NOT just to its VAD end `le`: the samples after `le` (trailing
            // silence up to the true window boundary) are real audio, and dropping them shifted the next
            // window's base earlier by `pcm.len() - le`, drifting every later segment's source_start_ms/
            // _end_ms cumulatively (offset_ms is only consulted while the carry is empty). Carrying the
            // whole tail keeps the concatenated timeline globally contiguous. Final window emits all.
            if !is_last {
                if let Some(&(ls, _le)) = chunk_ranges.last() {
                    carry_pcm = pcm[ls..].to_vec();
                    carry_base = base_sample + ls;
                    chunk_ranges.pop();
                }
            }
            if chunk_ranges.is_empty() {
                return Ok(());
            }

            let global_ranges: Vec<(usize, usize)> =
                chunk_ranges.iter().map(|&(s, e)| (base_sample + s, base_sample + e.min(pcm.len()))).collect();

            let mut window_progress = |_: usize, _: usize| {
                global_chunk += 1;
                on_chunk(global_chunk, estimated_total.max(global_chunk));
            };

            let mut denoiser_guard = self.lock_denoiser_service();
            // Rebuild when unset, OR when cached-inactive AND we have not yet tried this file (P1.4b:
            // don't re-attempt an unloadable GTCRN every window — at most once per file). See the
            // non-streaming sibling site.
            if should_rebuild_streaming_service(
                denoiser_guard.is_some(),
                denoiser_guard.as_ref().is_some_and(|s| s.is_active()),
                denoiser_rebuild_tried,
            ) {
                denoiser_rebuild_tried = true;
                // Per-file (round-26): see the sibling site — resolve_root_for avoids the all-or-nothing orphan.
                let model_dir = self.model_manager.resolve_root_for(crate::models::DENOISER_MODEL);
                *denoiser_guard = Some(crate::denoiser::DenoiserService::new(&model_dir));
            }
            let denoiser_service = denoiser_guard
                .as_ref()
                .ok_or_else(|| AppError::Other("Failed to initialize denoiser service".into()))?;

            let (window_segs, window_pcm_cache) = self.build_segments_from_pcm(
                path,
                &pcm,
                sample_rate,
                base_sample,
                &global_ranges,
                vad_backend,
                cancel,
                embedding_service,
                denoiser_service,
                &mut window_progress,
                Some(&mut all_embeddings), // streaming: defer clustering to the whole-file pass below
                file_hash.as_deref(),
            )?;
            segments.extend(window_segs);
            all_pcm_cache.extend(window_pcm_cache);
            Ok(())
        };

        audio::decode_pcm_windows_streaming(
            path.to_path_buf(),
            audio::DECODE_WINDOW_MS,
            window_timeout,
            process_window,
        )?;

        if windows_seen == 0 {
            return Err(AppError::Validation("Empty audio buffer".into()));
        }
        if segments.is_empty() {
            return Err(AppError::Validation("No speech chunks produced".into()));
        }

        // The whole-recording duplicate check, BEFORE anything is persisted. It has to happen here and
        // not next to the stamp below: the per-window checks inside the loop compare WINDOW hashes,
        // which by construction never equal the whole-file hash that gets persisted, so a streamed
        // recording re-imported in a LATER session would sail past every one of them. Registering the
        // identity without testing it would leave cross-session dedup looking implemented while doing
        // nothing for exactly the long files this path exists to handle.
        //
        // Late is not free — the ASR work for this file is already spent — but the harm being prevented
        // is duplicate ROWS in the library, and those have not been written yet.
        let identity = recording_identity.finish();
        self.fingerprint
            .check_and_register_identity(&identity, Some(path))
            .map_err(|e| AppError::Validation(e.into()))?;

        let chunk_count = segments.len() as u32;
        for (idx, seg) in segments.iter_mut().enumerate() {
            if let Some(meta) = seg.alignment_json.as_deref().and_then(chunking::SegmentSourceMeta::from_alignment_json)
            {
                let mut meta = meta;
                meta.chunk_index = idx as u32;
                meta.chunk_count = chunk_count;
                seg.alignment_json = Some(meta.to_alignment_json());
            }
        }

        // Whole-file speaker clustering: cluster every retained segment's embedding TOGETHER so a
        // physical speaker keeps ONE SPEAKER_xx label across decode-window boundaries (per-window
        // clustering relabels the first speaker of each window as SPEAKER_00). all_embeddings is in
        // lockstep with `segments`, so labels back-fill by index; a None label keeps any
        // filename-derived speaker hint, and it is a no-op when diarization is off.
        if self.settings.enable_diarization && all_embeddings.len() == segments.len() {
            let labels = crate::diarization::cluster_embeddings(&all_embeddings, self.settings.max_speakers);
            for (seg, label) in segments.iter_mut().zip(labels) {
                if let Some(spk) = label {
                    seg.speaker_id = Some(spk);
                }
            }
        }

        let mut persisted = self.persist_segments(db, segments)?;
        // v51: stamp the whole-recording identity now that the rows exist, exactly as the non-streaming
        // sibling does. Already checked and registered in memory above. Best-effort — a failed stamp
        // must not fail an import whose audio and transcripts are already committed; it only costs this
        // recording its place in cross-session dedup.
        if identity.spectral != 0 {
            if let Err(e) = db.set_audio_identity(&path.to_string_lossy(), &identity) {
                tracing::warn!("audio identity not persisted for {}: {e}", path.display());
            }
        }
        self.run_primary_wsl_pass_for_import(db, &mut persisted, cancel)?;
        // Deferred to here so both see the real transcript and alignment doesn't clobber offsets.
        self.shadow_log_loop0(db, &persisted);
        self.enqueue_background_alignments(&persisted);
        {
            let primary_by_segment: HashMap<&str, PrimaryHypothesis<'_>> = persisted
                .iter()
                .filter_map(|segment| {
                    PrimaryHypothesis::from_segment(segment).map(|primary| (segment.id.as_str(), primary))
                })
                .collect();
            for (seg_id, f32_pcm) in all_pcm_cache {
                let primary = primary_by_segment.get(seg_id.as_str()).copied();
                if let Err(error) = self.populate_hypotheses_reusing_primary(db, &seg_id, &f32_pcm, primary) {
                    log_hypothesis_population_failure(&seg_id, &error);
                }
            }
        }
        Ok(persisted)
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn build_segments_from_pcm(
        &self,
        path: &Path,
        pcm: &[i16],
        sample_rate: u32,
        global_base_sample: usize,
        chunk_ranges: &[(usize, usize)],
        vad_backend: crate::audio::VadBackend,
        cancel: Option<&CancellationToken>,
        embedding_service: &crate::diarization::SpeakerEmbeddingService,
        denoiser_service: &crate::denoiser::DenoiserService,
        on_chunk: &mut impl FnMut(usize, usize),
        // When `Some`, this is the STREAMING path: diarization clustering is DEFERRED — one embedding
        // per retained segment is appended here (in segment order) so the caller can cluster the WHOLE
        // file once. When `None`, clustering happens per-call (the non-streaming whole-file path).
        mut embedding_sink: Option<&mut Vec<Vec<f32>>>,
        // The source file's content hash, computed ONCE PER FILE by the caller and used to key the
        // per-chunk transcript cache. It used to be computed here, which the streaming path re-ran for
        // every 90 s window — O(windows × filesize) of redundant reads on exactly the long recordings
        // that path exists for. `None` = unhashable file = no cache for this run.
        file_hash: Option<&str>,
    ) -> AppResult<(Vec<SpeechSegment>, Vec<(String, Vec<f32>)>)> {
        let chunk_count = chunk_ranges.len() as u32;
        let chunk_total = chunk_ranges.len().max(1);
        let active_asr_model_size = self.selected_asr_model_size();
        let model_id = match active_asr_model_size {
            crate::settings::AsrModelSize::CTC300M => "omniasr-ctc-300m".to_string(),
            crate::settings::AsrModelSize::CTC1B => "omniasr-ctc-1b".to_string(),
            crate::settings::AsrModelSize::WSL7B => "omniasr-wsl-7b".to_string(),
        };
        let audio_path = path.to_string_lossy().to_string();
        let speaker_hint = if chunk_count > 1 && self.settings.assign_speaker_from_filename {
            path.file_stem().map(|s| s.to_string_lossy().into_owned())
        } else {
            None
        };

        let (diarization_labels, chunk_embeddings) = if self.settings.enable_diarization {
            // chunk_ranges are in GLOBAL sample coordinates, but `pcm` is the window-local buffer in
            // the streaming path (global_base_sample > 0). Embeddings slice `pcm` directly, so rebase
            // the ranges to local coords first — exactly like the transcription slice below. Without
            // this, every chunk past the first 90s window indexes beyond pcm.len(), clamps to an empty
            // slice, and silently gets NO speaker label. No-op when global_base_sample == 0.
            let local_ranges: Vec<(usize, usize)> = chunk_ranges
                .iter()
                .map(|&(gs, ge)| {
                    (gs.saturating_sub(global_base_sample), ge.saturating_sub(global_base_sample).min(pcm.len()))
                })
                .collect();
            let embeddings =
                crate::diarization::compute_chunk_embeddings(pcm, sample_rate, &local_ranges, embedding_service);
            if embedding_sink.is_some() {
                // Streaming: defer clustering to the caller's whole-file pass; no per-window labels.
                (vec![None; chunk_ranges.len()], Some(embeddings))
            } else {
                // Non-streaming: the whole file is this one call, so cluster in place and drop the
                // embeddings (the deferred sink is unused here).
                (crate::diarization::cluster_embeddings(&embeddings, self.settings.max_speakers), None)
            }
        } else {
            (vec![None; chunk_ranges.len()], None)
        };

        let mut segments = Vec::with_capacity(chunk_ranges.len());
        let mut pcm_cache = Vec::new();

        // Round-23 #3: if the user enabled denoising but the (optional) denoiser model is absent,
        // process() is a silent pass-through — warn loudly so the un-denoised reality is visible. The
        // run config separately records denoising=false (see runs::config_from_settings) so provenance
        // is honest; this log surfaces it to the operator.
        if self.settings.enable_denoising && !denoiser_service.is_active() {
            tracing::warn!(
                "Denoising is enabled in settings but the denoiser model is not loaded — audio is NOT being denoised (download the denoiser model to enable AI cleanup)"
            );
        }

        // The retained chunk PCM below is consumed by exactly one thing: the auxiliary-hypothesis pass,
        // which this same gate turns off. Under the champion (WSL7B) config it is always off, so a long
        // import used to accumulate every chunk's f32 audio for the whole file — ~270 MB for 1.5 h —
        // and then drop all of it untouched. Keep it only when something will actually read it.
        let retain_chunk_pcm = auxiliary_hypotheses_enabled(&self.settings);

        for (chunk_index, &(global_start, global_end)) in chunk_ranges.iter().enumerate() {
            if let Some(token) = cancel {
                token.check()?;
            }
            on_chunk(chunk_index + 1, chunk_total);

            let local_start = global_start.saturating_sub(global_base_sample);
            let local_end = global_end.saturating_sub(global_base_sample).min(pcm.len());
            if local_end <= local_start {
                continue;
            }
            let chunk_pcm = &pcm[local_start..local_end];
            if audio::is_silent(chunk_pcm) {
                continue;
            }
            let quality = crate::audio_quality::analyze_audio_quality(chunk_pcm);
            let chunk_duration_ms = chunking::samples_to_ms(local_end.saturating_sub(local_start), sample_rate);
            let source_meta =
                chunking::build_source_meta(global_start, global_end, sample_rate, chunk_index as u32, chunk_count);
            // Round-22 #12: key the per-chunk cache on the SAME stored ms range the re-transcribe read
            // path uses (slice_pcm_by_alignment), NOT raw sample indices. The read side round-trips
            // sample -> ms -> sample, so a raw-sample key never matched and the cache missed every time.
            let chunk_suffix = format!("chunk_{}_{}", source_meta.source_start_ms, source_meta.source_end_ms);

            let mut f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();

            // P1-1: Normalize PCM gain to -20 dBFS RMS before denoising and ASR.
            // Prevents low-energy audio (phone calls, distant mics) from producing
            // empty or junk transcripts due to near-zero token activations.
            audio::normalize_pcm_rms(&mut f32_pcm, -20.0);

            if self.settings.enable_denoising {
                let timer = crate::inference::InferenceTimer::start("denoiser");
                f32_pcm = denoiser_service.process(&f32_pcm, audio::TARGET_SAMPLE_RATE);
                timer.finish(true);
            }

            // Primary-engine override (matches transcribe()): when use_finetuned_asr is set, the
            // embedded fine-tuned MMS-CTC engine (the measured-best local Sorani engine, ~half the
            // CER of stock) is the import primary too — otherwise the flag silently did nothing on
            // import and every clip was transcribed with stock CTC. Any failure/empty output falls
            // through to the configured engine so import never breaks. Uses the raw chunk PCM, exactly
            // like transcribe()'s fine-tuned path (no extra RMS/denoise, so the two paths agree).
            // Evaluate the WSL-7B-primary routing ONCE per chunk: it decides both the placeholder branch
            // below AND whether a fine-tuned miss is a genuine STOCK downgrade (it is NOT when the 7B is the
            // primary drafter — the miss falls to the 7B champion, not stock local CTC).
            let wsl_primary = self.should_use_wsl_primary_asr();
            let finetuned_text: Option<String> = if self.finetuned_override_active() {
                // F2: every attempted chunk is counted; a fall-through TO STOCK increments the fallback
                // counter so the import completion can report the downgrade LOUDLY (a log-only
                // warn here left a whole import drafted at stock ~29.4% CER instead of the selected
                // 21.0% engine with nothing visible in the UI).
                self.finetuned_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let drafted = match Self::finetuned_model_paths() {
                    Some((onnx, vocab)) => match Self::transcribe_chunk_finetuned(&onnx, &vocab, chunk_pcm) {
                        Ok(t) if !t.trim().is_empty() => Some(t),
                        Ok(_) => {
                            tracing::warn!("fine-tuned ASR empty on import chunk; using the configured engine");
                            None
                        }
                        Err(e) => {
                            tracing::warn!("fine-tuned ASR failed on import chunk ({e}); using the configured engine");
                            None
                        }
                    },
                    None => {
                        tracing::warn!(
                            "use_finetuned_asr set but the fine-tuned model is absent; using configured engine"
                        );
                        None
                    }
                };
                // Count a fine-tuned MISS as a stock downgrade ONLY when the chunk actually falls back to
                // stock local CTC. Under the WSL-7B primary the miss falls to the 7B champion (the
                // placeholder branch below), which is NOT a stock downgrade — counting it would raise a
                // FALSE "ALL N chunk(s) were drafted by the STOCK engine … stock-grade" completion error on
                // an import the 7B actually drafted (the owner's WSL7B+use_finetuned config when the
                // fine-tuned checkpoint is absent — a direct honesty-law violation).
                if drafted.is_none() && !wsl_primary {
                    self.finetuned_fallbacks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                drafted
            } else {
                None
            };

            let (raw_transcript, confidence, confidence_source, model_version_id) = if let Some(text) = finetuned_text {
                (text, None, Some("fine_tuned_no_posterior".to_string()), Some("finetuned-mms-ckb".to_string()))
            } else if wsl_primary {
                (
                    "[Pending WSL 7B ASR]".to_string(),
                    None,
                    Some("not_run".to_string()),
                    Some("omniasr-wsl-7b".to_string()),
                )
            } else if let Some(cached) =
                file_hash.and_then(|h| self.cache.get_chunk_by_hash(h, &model_id, Some(&chunk_suffix)))
            {
                (cached.raw_transcript, None, Some("cache_replay".to_string()), Some(model_id.clone()))
            } else {
                let (text, conf, source) = self.with_asr(|asr| {
                    if asr.is_available() {
                        let timer = crate::inference::InferenceTimer::start("asr");
                        let result = asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE);
                        timer.finish(result.is_ok());
                        match result {
                            Ok((t, c, source)) => (t, c, Some(source.as_db_value().to_string())),
                            Err(e) => {
                                tracing::warn!(
                                    "ASR transcription failed for {} chunk {}: {e}",
                                    path.display(),
                                    chunk_index
                                );
                                (format!("[ASR unavailable: {e}]"), None, Some("not_available".to_string()))
                            }
                        }
                    } else {
                        tracing::warn!("ASR model not available for {} chunk {}", path.display(), chunk_index);
                        (String::new(), Some(0.0), Some("not_available".to_string()))
                    }
                });

                // Only cache a genuine transcription. Never bake a transient failure (model
                // unavailable → empty, or a transcribe error → "[ASR unavailable: …]") into the
                // cache, or every later retry would just replay the failure forever.
                if !text.trim().is_empty() && !crate::quality::is_placeholder_transcript(&text) {
                    if let Some(h) = file_hash {
                        let entry = crate::cache::CacheEntry {
                            audio_hash: String::new(),
                            raw_transcript: text.clone(),
                            normalized_transcript: None,
                            created_at: chrono::Utc::now(),
                            model_id: model_id.clone(),
                        };
                        self.cache.set_chunk_by_hash(h, Some(&chunk_suffix), entry);
                    }
                }
                (text, conf, source, Some(model_id.clone()))
            };

            let normalized = if self.settings.auto_normalize && !raw_transcript.is_empty() {
                let norm_config = crate::normalizer::NormalizationConfig {
                    normalize_numbers: self.settings.auto_normalize,
                    verbalize_numbers: self.settings.verbalize_numbers,
                    normalize_hamza: true,
                    remove_diacritics: false,
                };
                let norm = SoraniNormalizer::with_config(norm_config);
                Some(norm.normalize(&raw_transcript))
            } else {
                None
            };
            let normalizer_version = normalized.as_ref().map(|_| NORMALIZER_VERSION.to_string());

            let speaker_id = diarization_labels.get(chunk_index).and_then(|l| l.clone()).or(speaker_hint.clone());

            let seg_id = Uuid::new_v4().to_string();
            if retain_chunk_pcm {
                pcm_cache.push((seg_id.clone(), f32_pcm));
            }

            // Streaming defer: accumulate this segment's embedding (in segment order) so the caller can
            // cluster the whole file once. The push stays in lockstep with `segments` — both happen only
            // for a RETAINED chunk — so the back-filled labels align by index.
            if let Some(sink) = embedding_sink.as_mut() {
                if let Some(embs) = chunk_embeddings.as_ref() {
                    sink.push(embs.get(chunk_index).cloned().unwrap_or_default());
                }
            }

            segments.push(SpeechSegment {
                id: seg_id,
                created_at: None,
                audio_path: audio_path.clone(),
                raw_transcript,
                normalized_transcript: normalized,
                annotated_transcript: None,
                alignment_json: Some(source_meta.to_alignment_json()),
                duration_ms: chunk_duration_ms,
                speaker_id,
                verified: false,
                confidence,
                ctc_score: None,
                clipping_ratio: Some(quality.clipping_ratio),
                rms_db: Some(quality.rms_db),
                // Already Option: None (unmeasurable, e.g. a short clip) persists as NULL so the
                // quality/jury gates skip the SNR check instead of reading 0.0 as the worst SNR.
                snr_db: quality.snr_db,
                split: None,
                signal_anomaly_score: None,
                verdict: None,
                verdict_transcript: None,
                rationale: None,
                evidence_json: None,
                agreement_score: None,
                escalated: false,
                human_decision: None,
                corrected_at: None,
                is_gold: false,
                alignment_quality: None, // set to 'ctc_forced' or 'energy_heuristic' after align()
                model_version_id,
                confidence_source,
                cloud_call: false,
                decoder_config_hash: None,
                normalizer_version,
                // P0.4 per-segment processing provenance: record whether denoising / diarization
                // ACTUALLY ran for this clip (the setting enabled AND the model was loadable), not the
                // bare setting. This is per-FILE truth duplicated across the file's rows (honest), read
                // at export instead of recomputing from export-day model state (H3). For diarization,
                // `is_available()` reflects whether the CAM++ pass ran, independent of whether THIS
                // segment received a label (streaming defers labeling; single-speaker files get one id).
                denoised: Some(self.settings.enable_denoising && denoiser_service.is_active()),
                diarized: Some(self.settings.enable_diarization && embedding_service.is_available()),
                // P0.4: the VAD backend that ACTUALLY produced this file/window's regions (silero / energy
                // fallback / none for the short whole-buffer path) — surfaced from the detector, not a
                // path-exists probe (a corrupt Silero falls back to energy at runtime).
                vad_backend: Some(vad_backend.as_str().to_string()),
                // v43: a freshly imported clip has had no human decision, so it has no reviewer to
                // attribute. `record_human_decision_by` fills this in when someone actually decides it.
                reviewed_by: None,
                // v47: NOT measured at import, and None says exactly that. Answering it here costs two
                // extra CAM++ embeddings per chunk on top of the diarization pass, and the calibration
                // that reads it (0.59) was derived on ~14 s clips — applying it to whatever length the
                // planner emits would be a threshold used outside the range it was measured on.
                // `src/bin/speaker_change_probe.rs --persist` fills it for the whole library at once.
                speaker_change_score: None,
            });
        }

        // Round-22 #11: renumber the RETAINED segments to contiguous chunk_index / chunk_count. The loop
        // `continue`s past empty/silent chunks, so chunk_index (the enumerate index over ALL chunk_ranges)
        // has gaps and chunk_count over-counts the segments actually produced. The streaming caller
        // re-applies a whole-file renumber across decode windows; doing it here makes the non-streaming
        // whole-file path emit the same contiguous numbering instead of gappy provenance metadata.
        let retained = segments.len() as u32;
        for (idx, seg) in segments.iter_mut().enumerate() {
            if let Some(meta) = seg.alignment_json.as_deref().and_then(chunking::SegmentSourceMeta::from_alignment_json)
            {
                let mut meta = meta;
                meta.chunk_index = idx as u32;
                meta.chunk_count = retained;
                seg.alignment_json = Some(meta.to_alignment_json());
            }
        }

        Ok((segments, pcm_cache))
    }

    fn persist_segments(&self, db: &Database, segments: Vec<SpeechSegment>) -> AppResult<Vec<SpeechSegment>> {
        if segments.is_empty() {
            return Err(AppError::Validation("No speech chunks produced".into()));
        }

        // insert_segments_batch wraps inserts in its own transaction; do not nest SAVEPOINTs.
        db.insert_segments_batch(&segments)?;

        // NOTE: neither LOOP-0 shadow logging NOR background word-alignment runs here. Both must see
        // the REAL transcript, which under the forced WSL-7B engine does not exist yet (segments carry
        // the "[Pending WSL 7B ASR]" placeholder until run_primary_wsl_pass_for_import fills them in).
        // Shadowing the placeholder made the C5 over-trigger gate vacuous (always would_fire=false);
        // aligning before the 7B pass clobbered the slice offsets the 7B client needs. So the caller
        // runs BOTH only after the primary pass — see the shadow_log_loop0 + enqueue_background_alignments
        // calls right after run_primary_wsl_pass_for_import.

        Ok(segments)
    }

    /// M2.3 / P1.3: for each freshly persisted segment, record whether LOOP-0 WOULD have fired on its
    /// finalized transcript (annotated ▸ normalized ▸ raw), WITHOUT mutating anything. Memories are
    /// loaded once. Best-effort: a load or write failure logs and never fails the import.
    fn shadow_log_loop0(&self, db: &Database, segments: &[SpeechSegment]) {
        let memories = match db.load_correction_memories() {
            Ok(memories) => memories,
            Err(error) => {
                tracing::warn!("LOOP-0 shadow logging skipped: failed to load correction memories: {error}");
                return;
            }
        };
        for seg in segments {
            let text = crate::corrections::loop0_draft_text(seg.annotated_transcript.as_deref(), &seg.raw_transcript);
            let would_fire = loop0_would_fire(&memories, text);
            if let Err(error) = db.record_loop0_shadow(&seg.id, would_fire) {
                tracing::warn!("LOOP-0 shadow log write failed for {}: {error}", seg.id);
            }
        }
    }

    /// M2.4: Enqueue background word-alignment for segments. Non-blocking, best-effort, opt-in via
    /// `auto_align`.
    ///
    /// CRITICAL invariant (the whole-file-vs-clip bug class): each segment's `alignment_json` holds
    /// its `{source_start_ms, source_end_ms}` slice offsets, which every LATER reader depends on — the
    /// WSL-7B re-transcribe client, dataset audio export, clip playback, jury acoustic scoring. This
    /// alignment therefore MUST (1) slice the clip out of the source by those offsets before aligning
    /// (word timings clip-local, not smeared across the whole recording) and (2) MERGE its word array
    /// back under a `words` key via `merge_word_timestamps` — NEVER flat-overwrite `alignment_json`
    /// with a bare word array, which would destroy the offsets and silently degrade every reader to
    /// the whole file. (This ran inside `persist_segments` and clobbered offsets; it is now deferred to
    /// after the 7B pass and repaired to slice+merge.)
    fn enqueue_background_alignments(&self, segments: &[SpeechSegment]) {
        if !self.settings.auto_align {
            return;
        }
        // Group by source file so each recording is decoded ONCE (a VAD-chunked file yields many
        // segments sharing one audio_path). Carry each segment's source-offset alignment_json + its
        // finalized text (annotated ▸ raw — the VERBATIM 7B transcript; aligning the LLM-refined
        // paraphrase would time words the speaker never said).
        let mut by_path: std::collections::HashMap<String, Vec<(String, Option<String>, String)>> =
            std::collections::HashMap::new();
        for s in segments {
            let text =
                crate::corrections::loop0_draft_text(s.annotated_transcript.as_deref(), &s.raw_transcript).to_string();
            by_path.entry(s.audio_path.clone()).or_default().push((s.id.clone(), s.alignment_json.clone(), text));
        }
        let db_path = self.db_path.clone();
        // Resolved HERE because the thread below is `move` and never captures `self`. That is exactly how
        // this path ended up on `aligner::align` — the free fallback-only stub — instead of the real
        // aligner the foreground path uses: the model root simply was not reachable inside the closure.
        // Per-file resolve for the same reason `Pipeline::align` uses it: `resolve_root_for` finds
        // mms_aligner.onnx in the user dir OR bundled, where the all-or-nothing `resolved_dir()` orphans
        // a bundled aligner as soon as OmniASR is downloaded into the user dir.
        let aligner_root = self.model_manager.resolve_root_for("mms_aligner.onnx");
        let enable_gpu = self.settings.enable_gpu;

        // R3: this DETACHED thread is spawned during import (ImportState::Running) but OUTLIVES the
        // ImportGuard, then writes segment alignments (update_segment_alignment) on its OWN connection —
        // so in the post-import window it escapes the import fence AND the db-Mutex serialization the
        // restore relies on. Register it as a background DB writer for its whole lifetime. Acquire the
        // guard HERE (still on the import worker thread, so there is no unfenced gap between enqueue and
        // the thread starting) and move it into the closure; it drops when the thread ends, incl. panic.
        let align_writer_guard = crate::commands::BgDbWriterGuard::new();
        std::thread::spawn(move || {
            let _align_writer_guard = align_writer_guard; // held for the whole alignment thread
            let db = match crate::db::Database::open(&db_path) {
                Ok(db) => db,
                Err(error) => {
                    tracing::warn!("background alignment skipped: could not open db: {error}");
                    return;
                }
            };
            // ONCE for the whole import, not per segment: `ForcedAligner::new` loads a ~365 MB ONNX
            // session. `Pipeline::align` can afford to build one per call because it aligns a single
            // clip; doing that here — across every segment of every file in an import — would not be.
            // A missing model is NOT an error: `new` succeeds with no session and `align` then reports
            // EnergyHeuristic honestly, which is the old behaviour and the correct one when there is
            // genuinely nothing better available.
            let aligner = match aligner::ForcedAligner::new(&aligner_root, enable_gpu) {
                Ok(aligner) => aligner,
                Err(error) => {
                    tracing::warn!("background alignment skipped: aligner unavailable: {error}");
                    return;
                }
            };
            let (mut aligned, mut failed) = (0usize, 0usize);
            for (audio_path, jobs) in by_path {
                let pcm16 =
                    match audio::decode_to_pcm(&audio_path).and_then(|(sr, pcm)| audio::ensure_pcm_16khz(sr, pcm)) {
                        Ok((_, pcm)) => pcm,
                        Err(error) => {
                            tracing::warn!("background alignment: decode failed for {audio_path}: {error}");
                            failed += jobs.len();
                            continue;
                        }
                    };
                for (seg_id, source_alignment, text) in jobs {
                    if text.trim().is_empty() {
                        continue;
                    }
                    // Slice the clip out of the source by its stored offsets BEFORE aligning.
                    let sliced = match chunking::slice_pcm_by_alignment(&pcm16, 16000, source_alignment.as_deref()) {
                        Ok((clip, _)) => clip,
                        Err(error) => {
                            tracing::warn!("background alignment: slice failed for {seg_id}: {error}");
                            failed += 1;
                            continue;
                        }
                    };
                    match aligner.align(&sliced, 16000, &text) {
                        Ok((words, quality)) if !words.is_empty() => {
                            // MERGE under `words`, preserving source_start_ms/source_end_ms. One
                            // atomic write for timings + quality marker: persisting the timings while
                            // the quality stamp failed (the old swallowed `let _ =`) left heuristic
                            // word timings unmarked, and quality.rs only raises the review-risk
                            // reason when the marker is present.
                            let merged = crate::chunking::merge_word_timestamps(source_alignment.as_deref(), &words);
                            if let Err(error) = db.update_segment_alignment(
                                &seg_id,
                                &merged,
                                // The quality the aligner ACTUALLY achieved. This was hardcoded to
                                // EnergyHeuristic, which was true of the stub but is a provenance lie the
                                // moment a real alignment happens — and `quality.rs` raises a review-risk
                                // reason on exactly this value, so the lie cost every background-aligned
                                // clip a false risk flag.
                                quality.as_db_str(),
                            ) {
                                tracing::warn!("background alignment: persist failed for {seg_id}: {error}");
                                failed += 1;
                                continue;
                            }
                            aligned += 1;
                        }
                        // Empty word list or error: leave the source offsets INTACT (never overwrite).
                        Ok(_) => failed += 1,
                        Err(error) => {
                            tracing::warn!("background alignment failed for {seg_id}: {error}");
                            failed += 1;
                        }
                    }
                }
            }
            if failed > 0 {
                tracing::warn!(
                    "background alignment: {aligned} aligned, {failed} failed/empty (source offsets preserved)"
                );
            } else {
                tracing::debug!("background alignment: {aligned} segment(s) aligned");
            }
        });
    }

    fn run_primary_wsl_pass_for_import(
        &self,
        db: &Database,
        segments: &mut [SpeechSegment],
        cancel: Option<&CancellationToken>,
    ) -> AppResult<usize> {
        if !self.should_use_wsl_primary_asr() || segments.is_empty() {
            return Ok(0);
        }

        // FORCE-USE the Champion (fail-hard): if the 7B server is unreachable/hung/errored — an
        // INFRASTRUCTURE failure, i.e. the client process exits non-zero (its honest failure contract),
        // as opposed to a REACHABLE server legitimately returning an empty transcript for a silent clip —
        // this import is CANCELLED and every segment it just created is rolled back. The user then starts
        // the 7B server and re-imports cleanly, instead of being left with a library of
        // "[Pending WSL 7B ASR]" placeholders or silently-downgraded output the owner never asked for.
        let import_ids: Vec<String> = segments.iter().map(|s| s.id.clone()).collect();
        let mut updated = 0usize;

        // Run the champion calls a WAVE at a time instead of one clip at a time.
        //
        // MEASURED 2026-08-14: one round trip is 4.62 s on a ~9 s clip and the whole import sustained
        // 8.5 clips/min with both GPUs near idle — the cost is latency, not compute. `WSL_7B_GATE` was
        // built to admit several calls at once and the server pre-forks one replica per GPU, but until
        // now NOTHING ever issued two calls concurrently, so the gate limited a concurrency that never
        // existed and the second card never received work. Setting CORTEX_7B_CONCURRENCY=2 changed the
        // rate by nothing, which is what proved the loop was the bottleneck.
        //
        // Only the CALL is parallel. Every database touch — refresh, provenance, escalation, rollback —
        // stays on this thread, in the original order, so the rollback contract and the "escalate this
        // segment only" rule behave exactly as before. `transcribe` opens its own connection per call,
        // so the concurrent phase never shares `db`.
        let wave_size = wsl_7b_concurrency().max(1);
        let mut start = 0usize;
        while start < segments.len() {
            let end = (start + wave_size).min(segments.len());

            // Cancellation is checked once per WAVE rather than per segment; `transcribe` also polls
            // the flag inside each in-flight call, so a cancel still lands within ~50 ms.
            if let Some(token) = cancel {
                if let Err(cancel_err) = token.check() {
                    tracing::info!("WSL 7B import cancelled; rolling back {} segment(s)", import_ids.len());
                    if let Err(e) = db.delete_segments_batch(&import_ids) {
                        tracing::error!("failed to roll back {} segment(s) after cancel: {e}", import_ids.len());
                    }
                    return Err(cancel_err);
                }
            }

            // PHASE A — concurrent, no shared DB.
            let flag = cancel.map(|t| t.as_atomic());
            let jobs: Vec<(String, String, Option<String>)> = segments[start..end]
                .iter()
                .map(|s| (s.id.clone(), s.audio_path.clone(), s.alignment_json.clone()))
                .collect();
            let outcomes: Vec<ChampionAttempt> = std::thread::scope(|scope| {
                let handles: Vec<_> = jobs
                    .iter()
                    .map(|(id, path, aj)| scope.spawn(move || self.attempt_champion(id, path, aj.as_deref(), flag)))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        // A panicked worker must not be read as success. Treat it as an infrastructure
                        // failure so the import halts and rolls back, per the force-champion contract.
                        h.join().unwrap_or_else(|_| ChampionAttempt::Infra("champion worker panicked".to_string()))
                    })
                    .collect()
            });

            // PHASE B — sequential, in the original segment order.
            for (offset, outcome) in outcomes.into_iter().enumerate() {
                let seg = &mut segments[start + offset];
                let problem: Option<String> = match outcome {
                    ChampionAttempt::Infra(reason) => {
                        tracing::error!(
                            "WSL 7B import cancelled (server unavailable: {reason}); rolling back {} segment(s)",
                            import_ids.len()
                        );
                        if let Err(e) = db.delete_segments_batch(&import_ids) {
                            tracing::error!(
                                "failed to roll back {} placeholder segment(s) after 7B cancel: {e}",
                                import_ids.len()
                            );
                        }
                        return Err(AppError::Validation(format!(
                            "OmniASR 7B server is not running — start it (e.g. wsl python cortex_7b_server.py from scripts/) and re-import. \
                             The import was cancelled and its {} segment(s) were rolled back. ({reason})",
                            import_ids.len()
                        )));
                    }
                    ChampionAttempt::Empty(reason) => Some(reason),
                    ChampionAttempt::Drafted => {
                        if let Err(e) = self.refresh_segment_from_db(db, seg) {
                            tracing::error!(
                                "WSL 7B import: DB error mid-pass ({e}); rolling back {} segment(s)",
                                import_ids.len()
                            );
                            if let Err(rollback_err) = db.delete_segments_batch(&import_ids) {
                                tracing::error!(
                                    "failed to roll back {} segment(s) after mid-pass DB error: {rollback_err}",
                                    import_ids.len()
                                );
                            }
                            return Err(e);
                        }
                        // Re-verify against the STORED row. The concurrent phase judged usability from
                        // the returned draft (it cannot read the DB); if the row disagrees, believe the
                        // row and escalate rather than count a clip that has no text.
                        let stored_ok =
                            !seg.raw_transcript.trim().is_empty() && !seg.raw_transcript.contains("[Pending");
                        if stored_ok {
                            // `attempt_champion` routes through `transcribe`, whose DB helper commits
                            // transcript + sole champion hypothesis atomically. Replacing hypotheses
                            // again here used to create a second, fallible write after success.
                            updated += 1;
                            None
                        } else {
                            Some("7B reported a draft but the stored row is still empty".to_string())
                        }
                    }
                };

                if let Some(reason) = problem {
                    // HALT, do not escalate-and-continue (owner rule 2026-08-11, wired 2026-08-20).
                    // These are VAD speech chunks — silence was already filtered out — so a champion
                    // that returns nothing for one after three retries is an anomaly, not a
                    // classification. The old path wrote an "escalated" verdict and let the import
                    // finish "successfully" with unresolved rows inside it: exactly the tally the
                    // law forbids. Roll the whole file back and stop; re-import resumes cleanly.
                    tracing::error!(
                        "WSL 7B primary ASR unavailable before jury: segment {} failed after retries ({reason}); \
                         rolling back {} segment(s) and HALTING the import",
                        seg.id,
                        import_ids.len()
                    );
                    if let Err(rollback_err) = db.delete_segments_batch(&import_ids) {
                        tracing::error!(
                            "failed to roll back {} segment(s) after champion halt: {rollback_err}",
                            import_ids.len()
                        );
                    }
                    return Err(AppError::Other(format!(
                        "champion produced no usable draft for segment {} after retries ({reason}). \
                         The import was HALTED and this file's {} segment(s) rolled back — nothing was stored unresolved. \
                         Check the 7B server load and re-import.",
                        seg.id,
                        import_ids.len()
                    )));
                }
            }

            start = end;
        }
        Ok(updated)
    }

    /// The retry loop for ONE segment, with no shared-DB access anywhere in it.
    ///
    /// Usability is judged from the returned draft rather than by re-reading the row, because the
    /// re-read needs the shared connection. The caller re-verifies against the DB in its sequential
    /// phase and downgrades to `Empty` if the stored row disagrees — so this cannot report success
    /// for a row that did not actually get text.
    fn attempt_champion(
        &self,
        segment_id: &str,
        audio_path: &str,
        alignment_json: Option<&str>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> ChampionAttempt {
        // The warm 7B server can transiently fail or return an empty result for a clip (e.g. while
        // still under load right after launch), which would otherwise leave that segment stuck at its
        // "[Pending WSL 7B ASR]" placeholder for good (observed in stress testing: 1 of 3 segments).
        // Retry a few times before giving up so an import reliably transcribes every segment; only
        // escalate after the retries are exhausted, rather than silently shipping a pending segment.
        const MAX_ATTEMPTS: usize = 3;
        let mut last_problem = String::from("7B produced no result");
        let mut infra = false;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.transcribe(Some(segment_id), audio_path, alignment_json, cancel) {
                Ok(draft) => {
                    let usable = !draft.raw_text.trim().is_empty() && !draft.raw_text.contains("[Pending");
                    if usable {
                        return ChampionAttempt::Drafted;
                    }
                    last_problem = "7B returned an empty transcript".to_string();
                    infra = false;
                }
                Err(error) => {
                    let msg = error.to_string();
                    if msg.contains(WSL_7B_EMPTY_RESULT_MARKER) {
                        // transcribe() turns Ok("") into Err so the re-transcribe IPCs cannot
                        // blank-overwrite a stored transcript. For an IMPORT that is not an
                        // infrastructure failure — the server answered, the clip simply had no words.
                        last_problem = "7B returned an empty transcript".to_string();
                        infra = false;
                    } else {
                        // A 5-minute per-attempt timeout means the server is HUNG, not flaky: another
                        // full-timeout attempt only triples the stall. Quick failures (connection
                        // refused) still retry briefly in case the server is mid-launch.
                        let hung = msg.contains("timed out");
                        last_problem = msg;
                        infra = true;
                        if hung {
                            break;
                        }
                    }
                }
            }
            if attempt < MAX_ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(1000));
            }
        }
        if infra {
            ChampionAttempt::Infra(last_problem)
        } else {
            ChampionAttempt::Empty(last_problem)
        }
    }

    fn refresh_segment_from_db(&self, db: &Database, seg: &mut SpeechSegment) -> AppResult<bool> {
        let ids = vec![seg.id.clone()];
        if let Some(fresh) = db.get_segments_by_ids(&ids)?.into_iter().next() {
            *seg = fresh;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Import one audio file through the same VAD chunking + ASR path as directory import.
    pub fn import_single_file(&self, path: &Path) -> AppResult<Vec<SpeechSegment>> {
        self.import_single_file_with_events(path, None, None, |_| {})
    }

    /// Import one file with optional cancellation and progress events (for Ctrl+O / long audiobooks).
    pub fn import_single_file_with_events(
        &self,
        path: &Path,
        cancel: Option<CancellationToken>,
        // Retained for call-site symmetry with the directory path; the jury (which consumed this for
        // report correlation) now runs in the import command's background thread, not inline here.
        _agent_run_id: Option<&str>,
        on_event: impl Fn(PipelineEvent),
    ) -> AppResult<Vec<SpeechSegment>> {
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
        let duration_ms = audio::get_duration_ms(path)?;
        let estimated_chunks =
            ((duration_ms as f64 / self.settings.max_segment_duration_ms.max(1) as f64).ceil() as usize).max(1);

        on_event(PipelineEvent::Started { total: 1 });
        self.reset_finetuned_counters();
        on_event(PipelineEvent::Phase { phase: "importing".into() });
        self.set_import_status(0, estimated_chunks, &fname);
        // RAII: clear `running` on EVERY exit path, including an early `?` from the open_db below, so a
        // failed single-file import can't leave a phantom in-progress status (mirrors import_directory).
        struct ImportStatusGuard<'a>(&'a ProcessingPipeline);
        impl Drop for ImportStatusGuard<'_> {
            fn drop(&mut self) {
                self.0.finish_import_status();
            }
        }
        let _status_guard = ImportStatusGuard(self);

        let db = self.open_db()?;
        on_event(PipelineEvent::Phase { phase: "reference_transcribing".into() });
        on_event(agent_stage(
            "source_reference",
            "running",
            fname.clone(),
            "Building whole-file reference transcript",
            0,
            estimated_chunks,
        ));
        on_event(PipelineEvent::Progress {
            current: 0,
            total: estimated_chunks,
            file: fname.clone(),
            status: "Building whole-file reference transcript".into(),
        });
        let mut chunks_done = 0usize;
        // Imports always use the configured primary engine. Optional cloud tools may be invoked only
        // through their explicit per-segment actions; a consent toggle can never replace the 7B
        // champion for an entire import or create a mixed-engine dataset after a cloud failure.
        let result = self.process_single_file_with_progress(path, &db, cancel.as_ref(), |current, total| {
            chunks_done = current;
            let total = total.max(estimated_chunks);
            self.set_import_status(current, total, &fname);
            on_event(PipelineEvent::Phase { phase: "transcribing".into() });
            on_event(agent_stage(
                "audio_chunking",
                "running",
                fname.clone(),
                format!("Preparing chunk {current}/{total}"),
                current,
                total,
            ));
            on_event(PipelineEvent::Progress {
                current,
                total,
                file: fname.clone(),
                status: format!("Transcribing chunk {current}/{total}"),
            });
        });

        match &result {
            Ok(segments) => {
                self.set_import_status(segments.len(), segments.len(), &fname);
                let segment_count = segments.len();
                on_event(agent_stage(
                    "source_reference",
                    "completed",
                    fname.clone(),
                    "Whole-file source reference stage completed or reused",
                    1,
                    1,
                ));
                on_event(agent_stage(
                    "audio_chunking",
                    "completed",
                    fname.clone(),
                    format!("{segment_count} speech chunk(s) persisted"),
                    segment_count,
                    segment_count.max(1),
                ));
                on_event(multi_model_hypothesis_stage(&db, &self.settings, fname.clone(), segments));

                // Post-import jury adjudication is intentionally NOT run here. The import COMMAND
                // (commands.rs `import_audio_file`) runs it on a background thread with its OWN WAL
                // database connection, so the heavy ASR-bearing jury never holds the shared DB lock
                // and starves the UI's get_segments. Running it here too made single-file import
                // adjudicate — and make any opted-in cloud LLM calls — TWICE and persist two agent
                // import reports for one import. The directory path keeps its own inline jury because
                // it batches every file's segments into a single adjudication.
            }
            Err(_) => {
                self.set_import_status(chunks_done, estimated_chunks, &fname);
            }
        }
        // F2: a fine-tuned→stock downgrade during this single-file import must end LOUD too.
        {
            let attempts = self.finetuned_attempts.load(std::sync::atomic::Ordering::Relaxed);
            let fallbacks = self.finetuned_fallbacks.load(std::sync::atomic::Ordering::Relaxed);
            if let Some(error) = Self::finetuned_downgrade_message(attempts, fallbacks) {
                tracing::error!("finetuned downgrade on import: {error}");
                on_event(PipelineEvent::Error { file: "fine-tuned engine".into(), error });
            }
        }
        on_event(PipelineEvent::Completed {
            total: 1,
            succeeded: if result.is_ok() { 1 } else { 0 },
            failed: if result.is_err() { 1 } else { 0 },
        });
        // `running` is cleared by `_status_guard` on scope exit (covers early-return error paths too).
        result
    }

    /// Transcribe an audio file, optionally limited to a source-time range from chunk metadata.
    pub fn transcribe(
        &self,
        segment_id: Option<&str>,
        audio_path: &str,
        alignment_json: Option<&str>,
        // Optional cancel flag threaded down to the WSL-7B subprocess poller so an in-flight 7B call is
        // killed within ~50 ms of Cancel, not only between segments (import/batch callers pass their
        // token; one-off callers pass None).
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<TranscriptionDraft> {
        let path = Path::new(audio_path);
        let duration_ms = audio::get_duration_ms(path)?;
        if duration_ms == 0 {
            return Err(AppError::Validation("Empty audio file".into()));
        }

        // CHAMPION FIRST, BEFORE ANY DECODE (2026-08-20 external review). The WSL-7B branch sends
        // the server a path + source window and never touches PCM — yet this function fully decoded
        // the source before selecting an engine, so every champion call on a 9 s clip cut from a
        // 77-minute episode decoded the whole episode in Rust and threw it away. Work must scale
        // with the CLIP, not the source. Only the fine-tuned override still needs PCM before the
        // engine choice, and it keeps its precedence below.
        if self.should_use_wsl_primary_asr() && !self.finetuned_override_active() {
            let db = crate::db::Database::open(&self.db_path).map_err(|e| AppError::Other(e.to_string()))?;
            let audio_path_str = path.to_string_lossy().to_string();

            let segment_id: Option<String> = if let Some(id) = segment_id {
                Some(id.to_string())
            } else if let Some(aj) = alignment_json {
                resolve_segment_id_by_alignment(db.connection(), &audio_path_str, aj)?
            } else {
                // Round-22 #10: with neither an explicit segment_id NOR an alignment_json to
                // disambiguate, a bare `WHERE audio_path = ?` returns an ARBITRARY row when a file was
                // chunked into multiple segments (every chunk shares the source audio_path) — writing
                // the WSL ASR and its hypothesis to the WRONG segment. Only accept the bare lookup when
                // EXACTLY ONE segment matches; otherwise refuse and require an explicit segment_id.
                let conn = db.connection();
                let mut stmt = conn
                    .prepare("SELECT id FROM speech_segments WHERE audio_path = ?")
                    .map_err(|e| AppError::Other(e.to_string()))?;
                let ids: Vec<String> = stmt
                    .query_map([&audio_path_str], |row| row.get::<_, String>(0))
                    .map_err(|e| AppError::Other(e.to_string()))?
                    .filter_map(Result::ok)
                    .collect();
                if ids.len() > 1 {
                    return Err(AppError::Validation(format!(
                        "transcribe: {} segments share this audio file; pass an explicit segment_id (or alignment_json) to choose which one to transcribe",
                        ids.len()
                    )));
                }
                ids.into_iter().next()
            };

            if let Some(id) = segment_id {
                tracing::info!("Running WSL 7B ASR for segment ID: {}", id);

                drop(db);

                // Tag a 7B failure (server down / timeout / empty) so the UI can offer a champion retry
                // — and NEVER silently fall through to a smaller model here.
                let wsl_result =
                    self.run_wsl_segment_transcript(audio_path, alignment_json, cancel).map_err(tag_7b_unavailable)?;
                let raw_transcript = wsl_result.raw_transcript.clone();
                let confidence = wsl_result.confidence;

                // A TRANSIENT empty 7B result (server up but under load) comes back as Ok("") — NOT an Err
                // — so the map_err(tag_7b_unavailable) above does not catch it. Do not let it fall through
                // to the write below: update_asr_transcript_if_unreviewed would replace a good, unverified
                // stored transcript with "" (silent data loss). Both re-transcribe entry points route
                // through here (batch_transcribe + the per-segment transcribe IPC) with no retry, unlike
                // the import path which retries/escalates for exactly this transient. Surface it as the
                // retryable 7B failure the tag above promises, leaving the existing transcript intact.
                if raw_transcript.trim().is_empty() {
                    return Err(tag_7b_unavailable(AppError::Other(format!(
                        "{WSL_7B_EMPTY_RESULT_MARKER} (the server is likely under load); the existing transcript is left unchanged"
                    ))));
                }

                let db = crate::db::Database::open(&self.db_path).map_err(|e| AppError::Other(e.to_string()))?;

                // Stage 2: Dual-Pass LLM Refinement (OpenRouter when configured + key present)
                let final_text = if let Some(refiner) = self.build_refiner() {
                    tracing::info!("Running LLM refinement on {} bytes...", raw_transcript.len());
                    let refine_result = if self.settings.ger_refinement_enabled {
                        // Generative error correction: prime the refiner with the N-best (populated
                        // just above) + relevant past corrections (relevance-ranked few-shot).
                        // Context loads are best-effort — refinement legitimately proceeds unprimed
                        // (no hypotheses recorded is a normal state) — but a DB READ FAILURE must be
                        // logged, not folded into "no context": the old unwrap_or_default() made a
                        // persistent DB problem silently produce unprimed GER forever with no trace.
                        // Champion mode has exactly one ASR input. Use the in-memory 7B draft rather
                        // than rereading historical DB hypotheses: an older 300M/1B/MMS/Scribe row can
                        // never leak into refinement, and no partial provenance write is needed before
                        // the whole transcription/refinement result is ready to commit.
                        let hyps = vec![raw_transcript.clone()];
                        let few_shot: Vec<(String, String)> = match crate::jury::get_few_shot_examples(&db, &id, 3) {
                            Ok(examples) => examples.into_iter().map(|e| (e.wrong_transcript, e.human_fix)).collect(),
                            Err(e) => {
                                tracing::warn!(
                                    "GER: could not load few-shot corrections for {id}: {e}; refining unprimed"
                                );
                                Vec::new()
                            }
                        };
                        refiner.refine_with_context(&raw_transcript, &hyps, &few_shot)
                    } else {
                        refiner.refine_text(&raw_transcript)
                    };
                    match refine_result {
                        Ok(refined) => {
                            tracing::info!("LLM Refinement successful.");
                            accept_refinement(&raw_transcript, &refined)
                        }
                        Err(e) => {
                            // HARD STOP (owner rule 2026-08-11): a configured refiner that FAILS is a
                            // failure, not an invitation to ship the unrefined draft. Measured
                            // 2026-08-10: 59 of 487 clips silently kept raw text this way, so the
                            // dataset was part refined and part not with nothing recording which.
                            return Err(AppError::Other(format!(
                                "LLM refinement failed for segment {id}: {e}. Refinement is enabled, so this                                  clip is NOT complete — the run is stopped rather than storing an unrefined                                  draft as if it were finished."
                            )));
                        }
                    }
                } else {
                    raw_transcript.clone()
                };

                // LOOP 0: when enabled, correct previously-learned confusions in the final text
                // before it is returned/stored (opt-in; default off; best-effort).
                let final_text = apply_loop0_firing(self.settings.loop0_firing_enabled, &db, &final_text);

                // Commit ONCE, after every enabled refinement succeeds. The former early write stored
                // raw 7B text before an enabled refiner could fail, then the command returned an error
                // with a partially changed row. The backend is the sole writer; the frontend reloads
                // this authoritative row and never whole-row-upserts a stale pre-inference snapshot.
                let normalized_transcript = if self.settings.auto_normalize && !final_text.is_empty() {
                    let norm_config = crate::normalizer::NormalizationConfig {
                        normalize_numbers: self.settings.auto_normalize,
                        verbalize_numbers: self.settings.verbalize_numbers,
                        normalize_hamza: true,
                        remove_diacritics: false,
                    };
                    let norm = SoraniNormalizer::with_config(norm_config);
                    Some(norm.normalize(&final_text))
                } else {
                    None
                };
                let cloud_call = self.llm_refinement_uses_cloud();
                let champion = SegmentHypothesis {
                    segment_id: id.clone(),
                    model_id: wsl_result.model_version_id.clone(),
                    transcript: raw_transcript.clone(),
                    confidence,
                };
                let updated = db
                    .commit_champion_transcript_if_unreviewed(
                        &champion,
                        Some(&wsl_result.deployment_sha256),
                        normalized_transcript.as_deref(),
                        Some("external_provider"),
                        cloud_call,
                    )
                    .map_err(|e| AppError::Other(format!("Failed to commit champion transcript: {e}")))?;
                if !updated {
                    return Err(AppError::Validation(format!(
                        "Segment {id} gained a human decision while the champion was running; its reviewed transcript was not overwritten"
                    )));
                }

                return Ok(TranscriptionDraft {
                    raw_text: raw_transcript,
                    final_text,
                    confidence,
                    confidence_source: Some("external_provider".to_string()),
                    model_version_id: Some(wsl_result.model_version_id),
                    cloud_call,
                    committed_by_pipeline: true, // commit_champion_transcript_if_unreviewed above was THE write
                });
            } else {
                return Err(AppError::Other(
                    "Segment not found in database. Please import the audio file first to generate speech segments."
                        .into(),
                ));
            }
        }

        let decode_timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);
        let (sample_rate, pcm) = audio::decode_to_pcm_with_timeout(path, decode_timeout)?;
        let (sample_rate, pcm) = audio::ensure_pcm_16khz(sample_rate, pcm)?;
        if pcm.is_empty() {
            return Err(AppError::Audio(crate::error::AudioError::EmptyBuffer));
        }

        let (chunk_pcm, chunk_suffix) = chunking::slice_pcm_by_alignment(&pcm, sample_rate, alignment_json)?;

        // Primary-engine override: when use_finetuned_asr is set, transcribe with the embedded
        // fine-tuned MMS-CTC engine (best local Sorani quality) regardless of asr_model_size. Any
        // failure (model absent / inference error / empty output) falls through to the configured
        // engine below, so transcription never breaks.
        if self.finetuned_override_active() {
            if let Some((onnx, vocab)) = Self::finetuned_model_paths() {
                match Self::transcribe_chunk_finetuned(&onnx, &vocab, &chunk_pcm) {
                    Ok(raw_text) if !raw_text.trim().is_empty() => {
                        let final_text = match self.build_refiner() {
                            Some(refiner) => match refiner.refine_text(&raw_text) {
                                Ok(refined) => accept_refinement(&raw_text, &refined),
                                Err(_) => raw_text.clone(),
                            },
                            None => raw_text.clone(),
                        };
                        let final_text = self.fire_loop0_if_enabled(&final_text);
                        if let Some(id) = segment_id {
                            if let Ok(db) = self.open_db() {
                                let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();
                                let primary = PrimaryHypothesis {
                                    model_id: "finetuned-mms-ckb",
                                    transcript: &raw_text,
                                    confidence: None,
                                };
                                if let Err(error) =
                                    self.populate_hypotheses_reusing_primary(&db, id, &f32_pcm, Some(primary))
                                {
                                    log_hypothesis_population_failure(id, &error);
                                }
                            }
                        }
                        let cloud_call = self.llm_refinement_uses_cloud();
                        return Ok(TranscriptionDraft {
                            raw_text,
                            final_text,
                            confidence: None,
                            confidence_source: Some("fine_tuned_no_posterior".to_string()),
                            model_version_id: Some("finetuned-mms-ckb".to_string()),
                            cloud_call,
                            committed_by_pipeline: false,
                        });
                    }
                    Ok(_) => {
                        tracing::warn!("fine-tuned ASR returned empty output; falling back to the configured engine")
                    }
                    Err(e) => {
                        tracing::warn!("fine-tuned ASR failed ({e}); falling back to the configured engine")
                    }
                }
            } else {
                tracing::warn!(
                    "use_finetuned_asr is set but the fine-tuned model is absent; using the configured engine"
                );
            }
        }

        // F2: the fine-tuned override (above) and the WSL primary pass both declined; if WSL 7B is
        // the selected engine but unresolvable, fall-through to local CTC here would be the silent
        // downgrade. Refuse instead (covers manual per-segment re-transcribe, not just import).
        if self.wsl7b_primary_unresolved() {
            return Err(Self::primary_engine_unavailable_error());
        }

        let model_id = self.local_asr_model_id().to_string();
        if let Some(cached) = self.cache.get_chunk(path, &model_id, chunk_suffix.as_deref()) {
            // The cache stores the RAW ASR text (the key omits the refiner config), so re-run LLM
            // refinement + LOOP-0 with CURRENT settings — otherwise a refiner/settings change would be
            // ignored and the raw element would be contaminated with refined text.
            let raw = cached.raw_transcript.clone();
            let refined = match self.build_refiner() {
                Some(refiner) => match refiner.refine_text(&raw) {
                    Ok(refined) => accept_refinement(&raw, &refined),
                    Err(_) => raw.clone(),
                },
                None => raw.clone(),
            };
            let fired = self.fire_loop0_if_enabled(&refined);
            return Ok(TranscriptionDraft {
                raw_text: raw,
                final_text: fired,
                confidence: None,
                confidence_source: Some("cache_replay".to_string()),
                model_version_id: Some(model_id.clone()),
                cloud_call: self.llm_refinement_uses_cloud(),
                committed_by_pipeline: false,
            });
        }

        let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let (raw_text, confidence, confidence_source) = self.with_asr(|asr| {
            if !asr.is_available() {
                return Err(AppError::Other("ASR model not loaded".into()));
            }
            let timer = crate::inference::InferenceTimer::start("asr");
            let result = asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE);
            timer.finish(result.is_ok());
            result.map_err(AppError::Other)
        })?;

        // Stage 2: Dual-Pass LLM Refinement (OpenRouter when configured + key present)
        let final_text = if let Some(refiner) = self.build_refiner() {
            tracing::info!("Running LLM refinement on {} bytes...", raw_text.len());
            match refiner.refine_text(&raw_text) {
                Ok(refined) => {
                    tracing::info!("LLM Refinement successful.");
                    accept_refinement(&raw_text, &refined)
                }
                Err(e) => {
                    // HARD STOP (owner rule 2026-08-11), same contract as the champion path above.
                    return Err(AppError::Other(format!(
                        "LLM refinement failed: {e}. Refinement is enabled, so this clip is NOT complete —                          the run is stopped rather than storing an unrefined draft as if it were finished."
                    )));
                }
            }
        } else {
            raw_text.clone()
        };

        // Only cache a GENUINE transcription — never an empty or placeholder result. ASR can legitimately
        // return Ok("") for a quiet-but-real chunk (and this path applies no RMS-normalize/denoise), so
        // without this guard an empty result is baked into the in-memory chunk cache and every later
        // "Re-run ASR" / batch_transcribe just replays the empty no-op instead of re-invoking the model.
        // Mirrors the same guard in build_segments_from_pcm.
        if !raw_text.trim().is_empty() && !crate::quality::is_placeholder_transcript(&raw_text) {
            let entry = crate::cache::CacheEntry {
                audio_hash: String::new(),
                // Cache the RAW ASR text, NOT the refined output: the cache key omits the refiner config,
                // so storing refined text would replay a stale refiner result (and contaminate the raw
                // element) on a later hit. Refinement is re-run per call from the cached raw text.
                raw_transcript: raw_text.clone(),
                normalized_transcript: None,
                created_at: chrono::Utc::now(),
                model_id: model_id.clone(),
            };
            self.cache.set_chunk(path, chunk_suffix.as_deref(), entry);
        }

        if let Some(id) = segment_id {
            if let Ok(db) = self.open_db() {
                let primary = PrimaryHypothesis { model_id: &model_id, transcript: &raw_text, confidence };
                if let Err(error) = self.populate_hypotheses_reusing_primary(&db, id, &f32_pcm, Some(primary)) {
                    log_hypothesis_population_failure(id, &error);
                }
            }
        }

        let final_text = self.fire_loop0_if_enabled(&final_text);
        Ok(TranscriptionDraft {
            raw_text,
            final_text,
            confidence,
            confidence_source: Some(confidence_source.as_db_value().to_string()),
            model_version_id: Some(model_id),
            cloud_call: self.llm_refinement_uses_cloud(),
            committed_by_pipeline: false,
        })
    }

    /// Apply LOOP-0 firing to a finalized transcript, opening a short-lived DB connection only when
    /// the opt-in is enabled (so the default-off path pays nothing). Best-effort — a db-open failure
    /// logs and returns the original text rather than failing transcription.
    fn fire_loop0_if_enabled(&self, transcript: &str) -> String {
        if !self.settings.loop0_firing_enabled {
            return transcript.to_string();
        }
        match self.open_db() {
            Ok(db) => apply_loop0_firing(true, &db, transcript),
            Err(error) => {
                tracing::warn!("LOOP-0 firing skipped (could not open db): {error}");
                transcript.to_string()
            }
        }
    }

    /// Resolve the embedded fine-tuned MMS-CTC model (`finetuned-mms-ckb/{model.onnx,vocab.json}`)
    /// from the active (user) models dir, then the bundled one. `None` if it is not present.
    fn finetuned_model_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        // The search itself lives in `models.rs` so the offline diagnostic/evaluation callers share
        // one coherent root-selection rule.
        crate::models::finetuned_model_paths()
    }

    /// Transcribe one decoded chunk (16 kHz mono i16) with the fine-tuned engine. The fine-tuned
    /// model is trained on short utterances, so a single >~15 s pass can duplicate text — sub-split a
    /// long chunk into balanced ~15 s windows and join the per-window transcripts.
    // Shared by offline diagnostic/evaluation paths: a single unbounded pass over >15 s audio
    // duplicates text on this model, so every such path uses the same windowing.
    pub(crate) fn transcribe_chunk_finetuned(onnx: &Path, vocab: &Path, chunk_pcm: &[i16]) -> Result<String, String> {
        const MAX_WIN: usize = 15 * 16000;
        let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let n = f32_pcm.len();
        if n == 0 {
            return Ok(String::new());
        }
        let n_win = n.div_ceil(MAX_WIN);
        let step = n.div_ceil(n_win);
        let mut out = String::new();
        let mut a = 0;
        while a < n {
            let b = (a + step).min(n);
            let part = crate::wav2vec2_asr::run_wav2vec2(onnx, vocab, "ckb", &f32_pcm[a..b])?;
            let part = part.trim();
            if !part.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(part);
            }
            a = b;
        }
        Ok(out)
    }

    /// Whether LLM refinement may run under the CURRENT consent-gated settings. Consults the
    /// same `effective_llm_mode()` gate as `build_refiner` so every refinement decision point
    /// enforces the cloud (Gemini) opt-in (defense in depth): if a future path attempts
    /// refinement without going through `build_refiner`, this guard still blocks cloud use
    /// when the user has not opted in.
    fn llm_refinement_permitted(&self) -> bool {
        if self.settings.effective_llm_mode() == crate::settings::LlmMode::None {
            return false;
        }
        // The live check applies ONLY to a path that actually leaves the machine. Local refinement
        // sends nothing anywhere, so gating it on cloud consent would break offline work for no
        // privacy gain — the withdrawal is about egress, not about refinement.
        !self.llm_refinement_uses_cloud() || self.consent.cloud_llm()
    }

    fn llm_refinement_uses_cloud(&self) -> bool {
        match self.settings.effective_llm_mode() {
            crate::settings::LlmMode::Gemini => true,
            crate::settings::LlmMode::Local => !self.settings.llm_endpoint_is_local(),
            crate::settings::LlmMode::None => false,
        }
    }

    /// Build the LLM refiner. When the configured mode is the cloud (Gemini) and an OPENROUTER_API_KEY
    /// is present in secrets.env, route through OpenRouter instead — it is verified working and
    /// reaches Gemini-class models, whereas direct Gemini is commonly 429 quota-blocked. Respects
    /// `None` (refinement disabled) and `Local` (the user's own endpoint).
    fn build_refiner(&self) -> Option<crate::llm_refiner::LlmRefiner> {
        use crate::settings::LlmMode;
        // When the user has not opted into cloud LLM, `effective_llm_mode()` downgrades
        // Gemini -> None, so no refiner (and therefore no outbound cloud call) is ever
        // constructed. Mirrors the gate in `llm_refinement_permitted`.
        if !self.llm_refinement_permitted() {
            return None;
        }
        let refiner_from_settings = |mode: &LlmMode| {
            crate::llm_refiner::LlmRefiner::new(
                mode,
                self.settings.llm_endpoint.clone(),
                self.settings.llm_api_key.clone(),
                self.settings.llm_system_prompt.clone(),
                self.settings.llm_model.clone(),
            )
        };
        match self.settings.effective_llm_mode() {
            LlmMode::None => None,
            LlmMode::Local => refiner_from_settings(&LlmMode::Local),
            LlmMode::Gemini => {
                // secrets.env lives in the app data dir, next to the database.
                if let Some(data_dir) = std::path::Path::new(&self.db_path).parent() {
                    if let Some(openrouter_key) = crate::api_keys::ApiKeys::load(data_dir).openrouter {
                        return crate::llm_refiner::LlmRefiner::for_openrouter(
                            openrouter_key,
                            // Pass the CONFIGURED model, not an empty string (which silently defaulted to
                            // openai/gpt-4o-mini — a different family than the "Gemini" mode the owner chose,
                            // with no provenance). Map it to an OpenRouter id; a local-only name falls back
                            // to the Gemini-class model the user expects.
                            openrouter_model_id(&self.settings.llm_model),
                            self.settings.llm_system_prompt.clone(),
                        );
                    }
                }
                refiner_from_settings(&LlmMode::Gemini)
            }
        }
    }

    /// Explicit offline diagnostic evaluation. This is intentionally not registered as shipped IPC.
    pub fn run_gold_eval_local(&self, model_id: &str) -> AppResult<crate::eval::EvalRunResult> {
        // HONESTY GUARD (true-10 audit 2026-07-09): the eval row is persisted under `model_id`, so
        // the engine that transcribes MUST be derived from that id. Previously this always ran the
        // ACTIVE local engine and labeled the run with whatever the caller typed — a row labeled
        // "finetuned-mms-ckb" or "omniasr-wsl-7b" could be pure stock CTC output, a mislabeled
        // metric in the app's own honest-CER entrypoint. Only the locally runnable CTC engines are
        // accepted; anything else is an explicit error, never a silently mislabeled number.
        let model_size = match model_id {
            "omniasr-ctc-300m" => crate::settings::AsrModelSize::CTC300M,
            "omniasr-ctc-1b" => crate::settings::AsrModelSize::CTC1B,
            other => {
                return Err(AppError::Validation(format!(
                    "run_gold_eval_local can only run the local CTC engines it can label honestly \
                     (omniasr-ctc-300m, omniasr-ctc-1b); got '{other}'. Eval rows are persisted \
                     under this id, so the transcribing engine must match it exactly."
                )));
            }
        };
        // Open our own DB connection so no AppState lock is held across the (slow) decode+ASR loop —
        // mirrors run_gold_eval_asr. Holding the global db/pipeline mutexes here froze the whole UI.
        let db = self.open_db()?;
        let gold_segments = crate::eval::list_gold_segments(&db)?;
        let mut hypotheses = Vec::new();

        let model_dir = self.root_for_size(&model_size);
        let config = asr::AsrLoadConfig {
            model_size,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        self.asr_pool.warmup(&model_dir, &config)?;

        for gold in &gold_segments {
            let path = std::path::Path::new(&gold.audio_path);
            if !path.exists() {
                tracing::warn!("Gold segment audio path does not exist: {}", gold.audio_path);
                continue;
            }

            let (_sr, full_pcm) = match audio::decode_to_pcm(path) {
                Ok(pcm) => pcm,
                Err(e) => {
                    tracing::warn!("Failed to decode gold segment {}: {}", gold.id, e);
                    continue;
                }
            };

            let f32_pcm: Vec<f32> = full_pcm.iter().map(|&s| s as f32 / 32768.0).collect();

            let res = self.asr_pool.with_service(&model_dir, &config, |asr| {
                if !asr.is_available() {
                    return Err("ASR service unavailable".to_string());
                }
                asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
            });

            match res {
                Ok((text, _conf, _source)) => {
                    hypotheses.push((gold.id.clone(), text));
                }
                Err(e) => {
                    tracing::warn!("ASR failed for gold segment {}: {}", gold.id, e);
                }
            }
        }

        crate::eval::run_gold_eval(&db, model_id, hypotheses)
    }

    pub fn populate_hypotheses(&self, db: &Database, segment_id: &str, f32_pcm: &[f32]) -> AppResult<()> {
        self.populate_hypotheses_reusing_primary(db, segment_id, f32_pcm, None)
    }

    fn populate_hypotheses_reusing_primary(
        &self,
        db: &Database,
        segment_id: &str,
        f32_pcm: &[f32],
        primary: Option<PrimaryHypothesis<'_>>,
    ) -> AppResult<()> {
        // Guarded HERE rather than at the five call sites: one shared gate cannot be forgotten by a
        // sixth caller, and every caller wants the same answer. See `multi_engine_hypotheses` for
        // what these three engines cost when sherpa-onnx has no GPU (measured: 2.5 clips/minute).
        // The champion's own hypothesis is written by the transcribe path, not here, so turning this
        // off never leaves a clip without the transcript a reviewer is served.
        // The champion path stays single-engine even if a legacy settings file still carries the old
        // `multi_engine_hypotheses=true` default. The user's accuracy contract is explicit: when WSL7B
        // is selected, 300M/1B/MMS may not run automatically or influence the evidence mix. They remain
        // available only after selecting a non-champion engine and explicitly enabling this experiment.
        if !auxiliary_hypotheses_enabled(&self.settings) {
            return Ok(());
        }
        // 1. OmniASR 300M
        let model_id_300m = "omniasr-ctc-300m";
        let config_300m = asr::AsrLoadConfig {
            model_size: crate::settings::AsrModelSize::CTC300M,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        let model_dir_300m = self.root_for_size(&config_300m.model_size);
        let res_300m = reuse_primary_or_infer(primary, model_id_300m, || {
            if !self.size_present(&config_300m.model_size) {
                return None;
            }
            self.asr_pool.with_service(&model_dir_300m, &config_300m, |asr| {
                if !asr.is_available() {
                    return None;
                }
                Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE).map(|(text, conf, _source)| (text, conf)))
            })
        });
        match res_300m {
            Some(Ok((text, conf))) => insert_hypothesis_checked(db, segment_id, model_id_300m, text, conf)?,
            Some(Err(error)) => {
                tracing::warn!("{model_id_300m} hypothesis transcription failed for {segment_id}: {error}");
            }
            None => tracing::debug!("{model_id_300m} hypothesis model unavailable for {segment_id}"),
        }

        // 2. OmniASR 1B
        let model_id_1b = "omniasr-ctc-1b";
        let config_1b = asr::AsrLoadConfig {
            model_size: crate::settings::AsrModelSize::CTC1B,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        let model_dir_1b = self.root_for_size(&config_1b.model_size);
        let res_1b = reuse_primary_or_infer(primary, model_id_1b, || {
            if !self.size_present(&config_1b.model_size) {
                return None;
            }
            self.asr_pool.with_service(&model_dir_1b, &config_1b, |asr| {
                if !asr.is_available() {
                    return None;
                }
                Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE).map(|(text, conf, _source)| (text, conf)))
            })
        });
        match res_1b {
            Some(Ok((text, conf))) => insert_hypothesis_checked(db, segment_id, model_id_1b, text, conf)?,
            Some(Err(error)) => {
                tracing::warn!("{model_id_1b} hypothesis transcription failed for {segment_id}: {error}");
            }
            None => tracing::debug!("{model_id_1b} hypothesis model unavailable for {segment_id}"),
        }

        // 3. Fine-tuned MMS-CTC (ckb) — the machine's strongest INDEPENDENT local voter (wav2vec2 family,
        // ~21% CER), architecturally distinct from the correlated 300M/1B stock CTC pair. Its absence was
        // a root cause of "the jury escalates ~everything": two weak kin models rarely agree with the 7B,
        // so IRT confidence stays low and T0 almost never auto-accepts. Only runs when the fine-tuned
        // model is installed (a no-op otherwise); a failure is best-effort and never fails population.
        let model_id_finetuned = "finetuned-mms-ckb";
        let res_finetuned = reuse_primary_or_infer(primary, model_id_finetuned, || {
            let (onnx, vocab) = Self::finetuned_model_paths()?;
            let chunk_i16: Vec<i16> = f32_pcm.iter().map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16).collect();
            Some(Self::transcribe_chunk_finetuned(&onnx, &vocab, &chunk_i16).map(|text| (text, None)))
        });
        match res_finetuned {
            Some(Ok((text, _))) if !text.trim().is_empty() => {
                insert_hypothesis_checked(db, segment_id, model_id_finetuned, text, None)?;
            }
            Some(Ok(_)) => tracing::debug!("{model_id_finetuned} hypothesis empty for {segment_id}"),
            Some(Err(error)) => {
                tracing::warn!("{model_id_finetuned} hypothesis transcription failed for {segment_id}: {error}");
            }
            None => tracing::debug!("{model_id_finetuned} hypothesis model unavailable for {segment_id}"),
        }

        self.populate_wsl_hypothesis_if_configured(db, segment_id)?;

        Ok(())
    }

    fn populate_wsl_hypothesis_if_configured(&self, db: &Database, segment_id: &str) -> AppResult<()> {
        if self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
            return Ok(());
        }
        if resolve_wsl_7b_client(self.settings.external_asr_script_path()).is_none() {
            return Ok(());
        }
        let Some(expected) = crate::registry::champion_identity(db, crate::deployment::OMNIASR_7B_FAMILY)? else {
            tracing::warn!("WSL 7B auxiliary hypothesis skipped: no registry champion identity is available");
            return Ok(());
        };
        if db
            .get_hypotheses_for_segment(segment_id)?
            .iter()
            .any(|hyp| hyp.model_id == expected.model_version_id && !hyp.transcript.trim().is_empty())
        {
            return Ok(());
        }

        let Some(seg) = db.get_segment_by_id(segment_id)? else {
            return Ok(()); // the row vanished between selection and this auxiliary pass
        };
        match self.run_wsl_segment_transcript(&seg.audio_path, seg.alignment_json.as_deref(), None) {
            Ok(result) => {
                if result.model_version_id != expected.model_version_id
                    || result.deployment_sha256 != expected.deployment_sha256
                {
                    return Err(AppError::Validation(
                        "MODEL_IDENTITY_CHANGED: WSL 7B auxiliary reply does not match the registry champion".into(),
                    ));
                }
                insert_hypothesis_checked(
                    db,
                    segment_id,
                    &result.model_version_id,
                    result.raw_transcript,
                    result.confidence,
                )?;
            }
            Err(error) => {
                tracing::warn!("omniasr-wsl-7b hypothesis transcription failed for {segment_id}: {error}");
            }
        }
        Ok(())
    }

    fn run_wsl_segment_transcript(
        &self,
        audio_path: &str,
        alignment_json: Option<&str>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<Wsl7bResult> {
        // The resolvable client script stays the "champion is configured" signal (it gates
        // should_use_wsl_primary_asr and the whole fail-hard contract) — but the TRANSPORT no
        // longer spawns it (2026-08-20 external review): Rust already holds the path and the source
        // offsets the client re-derived by snapshot-copying the live DB into WSL per clip. The
        // script remains the manual/CLI transport (scorecards, the WSL console runner).
        if resolve_wsl_7b_client(self.settings.external_asr_script_path()).is_none() {
            return Err(AppError::Validation(
                "External ASR provider is not configured. Set the WSL script path in Settings before using the 7B provider.".into(),
            ));
        }
        run_wsl_segment_transcript_direct(audio_path, alignment_json, cancel)
    }

    pub fn align(
        &self,
        audio_path: &str,
        text: &str,
        alignment_json: Option<&str>,
    ) -> AppResult<(Vec<aligner::WordTimestamp>, aligner::AlignmentQuality)> {
        let (sample_rate, pcm) = audio::decode_to_pcm_with_timeout(audio_path, Duration::from_secs(120))?;
        let (sample_rate, pcm) = audio::ensure_pcm_16khz(sample_rate, pcm)?;
        if pcm.is_empty() {
            return Err(AppError::Audio(crate::error::AudioError::EmptyBuffer));
        }

        let pcm = chunking::slice_pcm_by_alignment(&pcm, sample_rate, alignment_json)?.0;

        let timer = crate::inference::InferenceTimer::start("align");
        // Prefer REAL CTC forced alignment from the fine-tuned MMS-CTC (Wav2Vec2 char-head) model when
        // it is installed — exact per-word boundaries from the same model family that transcribes, vs
        // the bundled aligner or the energy heuristic.
        if let Some(words) = Self::align_via_finetuned_mms(&pcm, text) {
            timer.finish(true);
            return Ok((words, aligner::AlignmentQuality::CtcForced));
        }
        // Per-file resolve (round-26): resolve_root_for finds the mms_aligner.onnx wherever it lives (user
        // dir OR bundled), NOT via resolved_dir() — which is all-or-nothing and orphans a bundled aligner
        // once the user downloads OmniASR into the user dir (the same class as the VAD/denoiser orphans).
        let aligner = aligner::ForcedAligner::new(
            &self.model_manager.resolve_root_for("mms_aligner.onnx"),
            self.settings.enable_gpu,
        )
        .map_err(AppError::Other)?;
        let result = aligner.align(&pcm, audio::TARGET_SAMPLE_RATE, text);
        timer.finish(result.is_ok());
        Ok(result?)
    }

    /// Real CTC forced alignment of a known transcript against the fine-tuned MMS-CTC (Wav2Vec2)
    /// model's emissions, when that model is installed. Returns None (caller falls back to the bundled
    /// aligner / energy heuristic) if the model is absent or the alignment is degenerate.
    fn align_via_finetuned_mms(pcm: &[i16], text: &str) -> Option<Vec<aligner::WordTimestamp>> {
        if text.trim().is_empty() || pcm.is_empty() {
            return None;
        }
        // The fine-tuned model is trained on short utterances; a single >~15 s forward pass degrades
        // (the same reason transcribe_chunk_finetuned windows at 15 s) and the Viterbi DP grows with
        // frames×chars. Bound the forced-alignment clip; longer clips fall back to the bundled aligner.
        const MAX_ALIGN_SAMPLES: usize = 15 * 16_000;
        if pcm.len() > MAX_ALIGN_SAMPLES {
            return None;
        }
        let (onnx, vocab) = Self::finetuned_model_paths()?;
        let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let (logits, frames, vocab_size, tokens) =
            crate::wav2vec2_asr::wav2vec2_logits(&onnx, &vocab, "ckb", &f32_pcm).ok()?;
        if frames == 0 {
            return None;
        }
        // Derive the per-frame stride from the model's own downsampling (≈0.02 s at 16 kHz) so the
        // frame→time mapping is exact regardless of the export's frame rate.
        let frame_sec = (pcm.len() as f64 / audio::TARGET_SAMPLE_RATE as f64) / frames as f64;
        let blank_idx = tokens.iter().position(|t| t == "<pad>").unwrap_or(0);
        aligner::ctc_logits_to_word_timestamps(&logits, frames, vocab_size, &tokens, blank_idx, text, frame_sec)
    }

    pub fn get_waveform(
        &self,
        audio_path: &str,
        num_points: usize,
        alignment_json: Option<&str>,
    ) -> AppResult<Vec<f32>> {
        let (_sample_rate, pcm) = audio::decode_to_pcm_with_timeout(audio_path, Duration::from_secs(30))?;
        let (sample_rate, pcm) = audio::ensure_pcm_16khz(_sample_rate, pcm)?;
        let pcm = chunking::slice_pcm_by_alignment(&pcm, sample_rate, alignment_json)?.0;
        Ok(audio::compute_waveform(&pcm, num_points))
    }

    /// Clear the audio PCM cache.
    pub fn clear_audio_cache(&self) {
        audio::clear_pcm_cache();
    }

    /// Re-run acoustic diarization on existing segments (grouped by source audio file).
    pub fn rediarize_segments(&self, ids: &[String]) -> AppResult<usize> {
        if !self.settings.enable_diarization {
            return Err(AppError::Validation("Speaker diarization is disabled in settings".into()));
        }

        // Own DB connection so no AppState lock is held across the per-file decode + ONNX diarization
        // loop (which previously froze every other db-touching command for the decode duration).
        let db = self.open_db()?;
        let all = db.get_segments_by_ids(ids)?;
        let targets: Vec<_> = all.into_iter().collect();
        if targets.is_empty() {
            return Ok(0);
        }

        let mut by_audio: std::collections::HashMap<String, Vec<SpeechSegment>> = std::collections::HashMap::new();
        for seg in targets {
            by_audio.entry(seg.audio_path.clone()).or_default().push(seg);
        }

        let mut updated = 0usize;
        for (audio_path, segs) in by_audio {
            let path = Path::new(&audio_path);
            if !path.exists() {
                continue;
            }
            let duration_ms = match audio::get_duration_ms(path) {
                Ok(duration_ms) => duration_ms,
                Err(error) => {
                    tracing::warn!("Rediarize duration probe failed for {audio_path}: {error}");
                    continue;
                }
            };
            let decode_timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);
            let (sample_rate, pcm) = match audio::decode_to_pcm_with_timeout(path, decode_timeout) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Rediarize decode failed for {audio_path}: {e}");
                    continue;
                }
            };
            let (sample_rate, pcm) = match audio::ensure_pcm_16khz(sample_rate, pcm) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Rediarize resample failed for {audio_path}: {e}");
                    continue;
                }
            };

            let mut chunk_ranges = Vec::new();
            let mut seg_order: Vec<String> = Vec::new();
            for seg in &segs {
                let (start, end) = if let Some(meta) =
                    seg.alignment_json.as_deref().and_then(chunking::SegmentSourceMeta::from_alignment_json)
                {
                    let (start_ms, end_ms) = (meta.source_start_ms.max(0), meta.source_end_ms.max(0));
                    // Same u32-wrap guard as chunking::slice_pcm_by_alignment / export::slice_for_export: a
                    // malformed offset > u32::MAX would wrap mod 2^32 to an in-range index and diarize an
                    // UNRELATED window, mislabeling this segment's speaker. Skip it rather than fall through
                    // to (0, pcm.len()) — whole-file diarization of a clip segment is the wrong answer too.
                    if start_ms > u32::MAX as i64 || end_ms > u32::MAX as i64 {
                        continue;
                    }
                    let s = chunking::ms_to_samples(start_ms as u32, sample_rate);
                    let e = chunking::ms_to_samples(end_ms as u32, sample_rate);
                    (s, e.min(pcm.len()))
                } else {
                    (0, pcm.len())
                };
                if end > start {
                    chunk_ranges.push((start, end));
                    seg_order.push(seg.id.clone());
                }
            }

            if chunk_ranges.is_empty() {
                continue;
            }

            let embedding_service = crate::diarization::SpeakerEmbeddingService::new(
                &self.model_manager.resolve_root_for(crate::models::CAMPP_MODEL),
            );
            let labels = crate::diarization::label_chunk_speakers(
                &pcm,
                sample_rate,
                &chunk_ranges,
                self.settings.max_speakers,
                &embedding_service,
            );

            for (idx, seg_id) in seg_order.iter().enumerate() {
                let Some(label) = labels.get(idx).and_then(|l| l.clone()) else {
                    continue;
                };
                // TARGETED single-column write, never a whole-row insert_segment upsert. `segs` is a
                // snapshot taken before the per-file decode + ONNX embedding pass above, and this method
                // deliberately holds no AppState lock across that work (see the comment at the top) so
                // other db-touching commands keep running — concurrent edits are expected BY DESIGN.
                // Upserting the stale snapshot silently reverted every column a human changed during a
                // multi-minute rediarize. This is the same anti-clobber discipline the batch speaker
                // command already follows via update_speaker_id. It also stops a segment DELETED during
                // the pass from being resurrected by the upsert: that is now a no-op, not a revival.
                match db.update_speaker_id(seg_id, Some(label.as_str())) {
                    Ok(true) => updated += 1,
                    Ok(false) => {
                        tracing::warn!("Rediarize speaker update skipped: segment {seg_id} no longer exists");
                    }
                    Err(error) => {
                        tracing::error!("Rediarize speaker update failed for {seg_id}: {error}");
                    }
                }
            }
        }

        Ok(updated)
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
