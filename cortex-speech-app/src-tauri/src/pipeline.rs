use crate::aligner;
use crate::asr;
use crate::audio;
use crate::cache::TranscriptCache;
use crate::cancel::CancellationToken;
use crate::chunking::{self, MAX_PCM_SAMPLES};
use crate::db::{
    ChampionTranscriptionSourceSnapshot, Database, SegmentHypothesis, SourceTranscriptRecord, SpeechSegment,
};
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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

mod import_flow;
mod transcription;
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
    /// Exact immutable deployment digest returned by the out-of-process champion. Internal import
    /// publication uses it to bind every drafted row to the still-current registry identity.
    pub(crate) deployment_sha256: Option<String>,
    pub cloud_call: bool,
    /// TRUE when `transcribe` itself already committed this draft to the segment row (the WSL-7B
    /// champion branch commits transcript + sole hypothesis + provenance atomically). Callers must
    /// then NOT write again: the 2026-08-20 external review found batch_transcribe re-writing the
    /// same result, so a failed second write reported "failed" for a row the first commit had
    /// already changed — two owners for one commit. One inference, one commit, one owner.
    pub committed_by_pipeline: bool,
}

/// An imported segment bound to both its one-statement database snapshot and an immutable decoded-
/// PCM source lease. The lease is deliberately owned by this value so it cannot be dropped between
/// inference and the compare-and-swap commit.
#[derive(Debug)]
pub(crate) struct BoundTranscriptionSource {
    snapshot: ChampionTranscriptionSourceSnapshot,
    _source_lease: crate::media::VerifiedMediaSourceLease,
}

/// Batch-scoped single-flight cache for verified source leases. Segments cut from the same recording
/// share one decoded-PCM verification and one immutable OS handle, while different recordings may
/// still verify concurrently. The cache belongs to the batch and therefore outlives every worker.
pub(crate) type TranscriptionSourceLeaseCache =
    Mutex<HashMap<(String, String), Arc<OnceLock<Result<crate::media::VerifiedMediaSourceLease, String>>>>>;

impl BoundTranscriptionSource {
    pub(crate) fn segment(&self) -> &SpeechSegment {
        &self.snapshot.segment
    }
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

/// A durable row is adoptable on resume only when its transcript authority is complete. The journal
/// is intentionally absent from this predicate: it is crash-recovery bookkeeping, not evidence that
/// the champion ever replaced a placeholder, that a draft came from the current champion, or that no
/// cloud path touched it. A real human accept/edit/reject outranks machine provenance; otherwise the
/// exact current local champion must own a non-empty, non-placeholder draft.
fn resume_segment_has_authoritative_transcript(seg: &SpeechSegment, champion_model_id: &str) -> bool {
    let human_decision = seg.human_decision.as_deref().or(seg.verdict.as_deref());
    let human_rejected = human_decision.is_some_and(|decision| {
        ["reject", "human_reject"].iter().any(|candidate| decision.eq_ignore_ascii_case(candidate))
    });
    if human_rejected {
        return true;
    }
    if let Some(text) = crate::quality::human_verified_text(seg) {
        return !text.trim().is_empty() && !crate::quality::is_placeholder_transcript(text);
    }
    seg.model_version_id.as_deref() == Some(champion_model_id)
        && !seg.cloud_call
        && !seg.raw_transcript.trim().is_empty()
        && !crate::quality::is_placeholder_transcript(&seg.raw_transcript)
}

/// Resume decision: a file is skipped only when the current database rows themselves prove complete
/// authority. A journal entry with no rows, or rows with stale/placeholder/wrong-model/cloud drafts,
/// must never mint completion merely because a previous process wrote "done" before crashing.
fn resume_should_skip_file(resuming: bool, has_authoritative_segments: bool) -> bool {
    resuming && has_authoritative_segments
}

fn resume_path_key(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

fn insert_hypothesis_checked(
    import_writes: &crate::stores::ImportWriteStore,
    segment_id: &str,
    model_id: &str,
    transcript: String,
    confidence: Option<f64>,
) -> AppResult<()> {
    import_writes
        .insert_hypothesis(&SegmentHypothesis {
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

/// What one segment's champion attempts concluded. Inference and refinement complete in memory;
/// canonical segment rows are published only after every segment in the file has a usable draft.
enum ChampionAttempt {
    /// A usable, fully refined transcript came back but has not been published.
    Drafted(TranscriptionDraft),
    /// Reachable server, no words after every retry. The whole file remains unpublished.
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
        self.acquire_with_limit(cancel, limit)
    }

    /// Explicit-limit core used by the environment-routed production entrypoint and deterministic
    /// unit tests. Keeping environment mutation out of parallel tests prevents an unrelated 7B test
    /// from temporarily changing the process-wide production limit observed by another test.
    fn acquire_with_limit(
        &self,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        limit: usize,
    ) -> Option<Wsl7bPermit<'_>> {
        debug_assert!((1..=8).contains(&limit));
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
    let installed = [
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
    });
    if installed.is_some() {
        return installed;
    }

    // An isolated Cargo target directory deliberately has no stable relationship to the checkout.
    // Unit tests still need to exercise the same tracked client that Tauri packages as a resource;
    // production binaries must only resolve their installed resource or an explicit override, never
    // retain a build-machine source path as an undocumented fallback.
    #[cfg(test)]
    {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scripts/cortex_7b_client.py")
            .canonicalize()
            .ok()
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
    }
    #[cfg(not(test))]
    None
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
    /// Desktop production injects the exact `AppState` runtime so migrated import writes have one
    /// serialized writer and the same restore admission as every other durable domain. Standalone
    /// library consumers retain the public path-based constructor during the strangler migration.
    database_runtime: Arc<Mutex<Option<crate::database_runtime::DatabaseRuntime>>>,
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
        Self::build(db_path, normalizer, cache, fingerprint, settings, model_manager, None)
    }

    pub(crate) fn new_with_runtime(
        db_path: String,
        normalizer: Arc<SoraniNormalizer>,
        cache: Arc<TranscriptCache>,
        fingerprint: Arc<AudioFingerprint>,
        settings: Arc<AppSettings>,
        model_manager: Arc<ModelManager>,
        runtime: crate::database_runtime::DatabaseRuntime,
    ) -> Self {
        Self::build(db_path, normalizer, cache, fingerprint, settings, model_manager, Some(runtime))
    }

    fn build(
        db_path: String,
        normalizer: Arc<SoraniNormalizer>,
        cache: Arc<TranscriptCache>,
        fingerprint: Arc<AudioFingerprint>,
        settings: Arc<AppSettings>,
        model_manager: Arc<ModelManager>,
        database_runtime: Option<crate::database_runtime::DatabaseRuntime>,
    ) -> Self {
        Self {
            db_path,
            database_runtime: Arc::new(Mutex::new(database_runtime)),
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

    fn shared_database_runtime(&self, database_path: &str) -> AppResult<crate::database_runtime::DatabaseRuntime> {
        if database_path != self.db_path {
            return Err(AppError::Other(
                "pipeline database capability does not match the supplied database".to_string(),
            ));
        }
        let mut slot = self.database_runtime.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned pipeline runtime slot");
            poisoned.into_inner()
        });
        if let Some(runtime) = slot.as_ref() {
            return Ok(runtime.clone());
        }
        // Compatibility path for public library consumers and standalone binaries. The slot is Arc
        // backed, so every cloned worker pipeline converges on this one runtime. Desktop production
        // never opens here: lib.rs injects the exact AppState runtime before the pipeline is cloned.
        let database = Database::open(database_path)?;
        let runtime = crate::database_runtime::DatabaseRuntime::new(database);
        *slot = Some(runtime.clone());
        Ok(runtime)
    }

    fn import_job_store(&self) -> AppResult<crate::stores::JobStore> {
        Ok(crate::stores::JobStore::new(self.shared_database_runtime(&self.db_path)?))
    }

    fn import_write_store(&self, database_path: &str) -> AppResult<crate::stores::ImportWriteStore> {
        Ok(crate::stores::ImportWriteStore::new(self.shared_database_runtime(database_path)?))
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
    /// transcripts. Historical duplication-weighted experiments showed that the engines were
    /// materially different, but those figures are not current model evidence. The operational
    /// lesson does not depend on an accuracy claim: silent substitution destroys provenance.
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

    fn source_reference_enabled(&self) -> bool {
        // Both the persisted choice and the revocable live consent must agree. Keep status reporting
        // on the exact same predicate as the upload gate so the UI never claims an external reference
        // is running when this import is champion-only.
        self.settings.jury_cloud_opt_in && self.consent.jury_cloud()
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
    fn jury_cloud_api_key(&self) -> AppResult<Option<String>> {
        let from_store = match Path::new(&self.db_path).parent() {
            Some(data_dir) => {
                crate::api_keys::ApiKeys::load(data_dir)
                    .map_err(|error| AppError::Other(format!("Could not load the encrypted API-key store: {error}")))?
                    .gemini
            }
            None => None,
        };
        Ok(from_store.or_else(|| {
            let typed = self.settings.llm_api_key.trim();
            (!typed.is_empty()).then(|| typed.to_string())
        }))
    }

    fn reusable_source_reference_record(
        &self,
        import_writes: &crate::stores::ImportWriteStore,
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
        import_writes.upsert_source_transcript(&synced)?;
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
        if !self.source_reference_enabled() {
            return Ok(Vec::new());
        }
        let import_writes = self.import_write_store(db.path())?;
        let Some(api_key) = self.jury_cloud_api_key()? else {
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
                    self.reusable_source_reference_record(&import_writes, &existing, current_identity.as_ref())?
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
                    import_writes.upsert_source_transcript(&record)?;
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
        let import_writes = self.import_write_store(db.path())?;
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
                match import_writes.update_machine_speaker(seg_id, label.as_str()) {
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
