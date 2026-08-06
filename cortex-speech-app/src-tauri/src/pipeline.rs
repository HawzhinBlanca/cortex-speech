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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use uuid::Uuid;

const SUBPROCESS_ERROR_PREVIEW_CHARS: usize = 4096;
const SOURCE_AUDIO_HASH_BUFFER_BYTES: usize = 128 * 1024;
const NORMALIZER_VERSION: &str = "sorani-normalizer-v1";

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

fn kill_and_reap_wsl_child(child: &mut std::process::Child, context: &str) {
    if let Err(error) = child.kill() {
        tracing::warn!("Failed to kill {context}: {error}");
    }
    if let Err(error) = child.wait() {
        tracing::warn!("Failed to reap {context}: {error}");
    }
}

fn join_wsl_pipe_reader(thread: std::thread::JoinHandle<Vec<u8>>, stream: &str) -> Vec<u8> {
    match thread.join() {
        Ok(buffer) => buffer,
        Err(_) => {
            tracing::warn!("WSL subprocess {stream} reader panicked");
            Vec::new()
        }
    }
}

fn lock_decoded_windows(windows: &Mutex<Vec<audio::PcmWindow>>) -> MutexGuard<'_, Vec<audio::PcmWindow>> {
    windows.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned decoded PCM window accumulator");
        poisoned.into_inner()
    })
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

fn parse_wsl_segment_result(stdout: &str) -> AppResult<(String, Option<f64>)> {
    #[derive(serde::Deserialize)]
    struct WslResult {
        raw_transcript: String,
        confidence: Option<f64>,
    }

    let mut raw_transcript = String::new();
    let mut confidence: Option<f64> = None;
    let mut saw_result = false;
    for line in stdout.lines() {
        if let Some(stripped) = line.strip_prefix("__RESULT__=") {
            if let Ok(res) = serde_json::from_str::<WslResult>(stripped) {
                saw_result = true;
                raw_transcript = res.raw_transcript;
                // Sanitize the external script's confidence to a valid posterior: drop non-finite and
                // clamp into [0,1]. A homegrown script emitting a percentage (e.g. 92.0) must not flow
                // unbounded into the conformal certificate, where it would read as MAXIMAL certainty.
                confidence = res.confidence.filter(|c| c.is_finite()).map(|c| c.clamp(0.0, 1.0));
            }
        }
    }

    // A reachable server that emits a `__RESULT__` line with an EMPTY transcript is a LEGITIMATE
    // outcome (a silent/music/noise clip), NOT an infrastructure failure — the client's failure
    // contract exits non-zero (handled by the caller before we are reached) for a real infra fault and
    // exits 0 with a `__RESULT__` line otherwise. Returning Err on an empty-but-present result made ONE
    // silent chunk roll back the ENTIRE import and left the file permanently unimportable via the 7B.
    // So Err ONLY when no `__RESULT__` line was seen at all; an empty transcript returns Ok and the
    // caller escalates just that one segment.
    if !saw_result {
        return Err(AppError::Other("WSL 7B ASR process did not return a __RESULT__ line.".into()));
    }

    Ok((raw_transcript, confidence))
}

/// Serializes ALL WSL-7B client spawns process-wide. The champion server is a single-threaded accept
/// loop, so concurrent app-side callers (an import's per-segment pass, the batch refinement loop, a UI
/// re-transcribe) would queue on the socket and blow through their client/app timeouts CUMULATIVELY —
/// misread as "server not running" and rolling back a HEALTHY import. Serializing here means each
/// request waits its turn, then gets its FULL fresh timeout budget once it actually runs.
static WSL_7B_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The port the OmniASR-7B warm server listens on inside WSL. SINGLE source of truth — shared by the
/// preflight probe AND passed to the client via `CORTEX_7B_PORT`, so the app, client, and server can't
/// drift (a mismatch would even green-light the preflight against the wrong service).
pub(crate) const WSL_7B_SERVER_PORT: u16 = 8799; // must match cortex_7b_server.py

/// Stable marker in the Err that `transcribe()` returns for a legit-but-EMPTY 7B result — a REACHABLE
/// server that produced no words (a silent/music/noise clip), which parse_wsl_segment_result surfaces as
/// Ok(""). transcribe() converts that to an Err so the re-transcribe IPCs never blank-overwrite a stored
/// transcript; but the IMPORT pass also routes through transcribe(), and for it an empty result is NOT an
/// infra failure — it must escalate only that one segment, never roll back the whole file. The import pass
/// matches this marker to tell a benign empty apart from a real "server down / client exited non-zero" error.
pub(crate) const WSL_7B_EMPTY_RESULT_MARKER: &str = "WSL 7B returned an empty transcript";

/// Machine-readable sentinel embedded in every "the OmniASR-7B champion is the selected primary
/// engine but it is unavailable / failed" error. The frontend matches on this token to offer the
/// user an EXPLICIT choice — retry the champion once its server is up, or transcribe this one clip
/// with the offline model — instead of a dead-end error. The app NEVER silently substitutes a
/// smaller model on the primary path; a small-model transcript is produced only on a deliberate
/// user action. Keep the value in sync with `ASR_7B_UNAVAILABLE_TAG` in `src/lib/commands.ts`.
pub(crate) const ASR_7B_UNAVAILABLE_TAG: &str = "E_ASR_7B_UNAVAILABLE";

/// Wrap a primary-7B transcription failure so the UI can classify it (see [`ASR_7B_UNAVAILABLE_TAG`])
/// and present the retry-or-offline choice. Preserves the original actionable text.
pub(crate) fn tag_7b_unavailable(err: AppError) -> AppError {
    let msg = err.to_string();
    if msg.contains(ASR_7B_UNAVAILABLE_TAG) {
        return err; // already tagged upstream — don't double-prefix
    }
    AppError::Validation(format!("{ASR_7B_UNAVAILABLE_TAG}: {msg}"))
}

/// Lean, side-effect-free health probe of the OmniASR-7B warm server: TCP-connect 127.0.0.1:PORT
/// inside WSL's network namespace (the loopback port is NOT reachable from Windows). Returns a bare
/// bool for the engine-status surface — unlike `wsl_7b_server_preflight`, which fails hard with a
/// user-facing message on the import path. `timeout_secs` bounds both the in-WSL probe and the child.
pub(crate) fn probe_wsl_7b_server(timeout_secs: u64) -> bool {
    let probe = format!("timeout {timeout_secs} bash -c 'exec 3<>/dev/tcp/127.0.0.1/{WSL_7B_SERVER_PORT}'");
    let mut cmd = std::process::Command::new("wsl");
    cmd.arg("bash").arg("-lc").arg(&probe);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false; // no WSL / launch failure → engine is not reachable
    };
    // Bound the wait a hair past the in-WSL timeout so a wedged WSL can't hang the status poll.
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs + 2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    kill_and_reap_wsl_child(&mut child, "engine-status probe");
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                // A wait-status error may leave the probe child running; reap it before bailing,
                // exactly like the sibling try_wait loops (run_wsl_segment_transcript, the 7B
                // preflight probe). std::process::Child does NOT kill/reap on drop, so a bare
                // `return false` here leaks a WSL process on every failed status poll — and this
                // probe is called on a poll.
                kill_and_reap_wsl_child(&mut child, "engine-status probe");
                return false;
            }
        }
    }
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
) -> AppResult<(String, Option<f64>)> {
    // Hold the process-wide gate for the whole spawn+wait so cross-path 7B calls never collide on the
    // single-threaded server. Poison-tolerant like every other lock in this crate.
    let _gate = WSL_7B_GATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut cmd = std::process::Command::new("wsl");
    // Pass the DB path + port to the client via `env` (WSL does not propagate Windows env into Linux),
    // so the client follows a MOVED data dir / non-default port instead of its hardcoded fallbacks — the
    // app is the single source of truth for both.
    cmd.arg("env")
        .arg(format!("CORTEX_7B_DB={}", win_path_to_wsl(db_path)))
        .arg(format!("CORTEX_7B_PORT={WSL_7B_SERVER_PORT}"))
        .arg("/root/cortex_env/bin/python3")
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

fn multi_model_hypothesis_stage(db: &Database, file: impl Into<String>, segments: &[SpeechSegment]) -> PipelineEvent {
    let file = file.into();
    let total = segments.len().max(1);
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
    cloud_stt: AtomicBool,
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
        self.cloud_stt.store(settings.cloud_stt_opt_in, Ordering::SeqCst);
        self.jury_cloud.store(settings.jury_cloud_opt_in, Ordering::SeqCst);
    }

    fn cloud_llm(&self) -> bool {
        self.cloud_llm.load(Ordering::SeqCst)
    }
    fn cloud_stt(&self) -> bool {
        self.cloud_stt.load(Ordering::SeqCst)
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
        if !next.cloud_stt_opt_in {
            self.consent.cloud_stt.store(false, Ordering::SeqCst);
        }
        if !next.jury_cloud_opt_in {
            self.consent.jury_cloud.store(false, Ordering::SeqCst);
        }
    }

    pub fn update_settings(&mut self, settings: AppSettings) {
        // Consent FIRST, and through the shared Arc so it reaches clones already running an import.
        // The snapshot swap below is visible only to this instance; that is fine for preferences and
        // was the bug for consent.
        self.consent.apply(&settings);
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
        if self.should_use_wsl_primary_asr() {
            tracing::info!("WSL 7B model selected: skipping local ONNX ASR pool warm-up.");
            return Ok(());
        }
        let model_dir = self.model_manager.resolved_dir();
        self.asr_pool.warmup(&model_dir, &self.asr_config())
    }

    fn asr_config(&self) -> asr::AsrLoadConfig {
        asr::AsrLoadConfig {
            model_size: self.active_local_asr_model_size(),
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        }
    }

    fn active_local_asr_model_size(&self) -> crate::settings::AsrModelSize {
        let model_dir = self.model_manager.resolved_dir();
        if self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
            if asr::omniasr_model_present(&model_dir, &crate::settings::AsrModelSize::CTC1B) {
                return crate::settings::AsrModelSize::CTC1B;
            }
            if asr::omniasr_model_present(&model_dir, &crate::settings::AsrModelSize::CTC300M) {
                return crate::settings::AsrModelSize::CTC300M;
            }
            return crate::settings::AsrModelSize::CTC300M;
        }
        asr::select_available_model_size(&model_dir, &self.settings.asr_model_size)
    }

    fn should_use_wsl_primary_asr(&self) -> bool {
        self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B
            && self.settings.external_asr_script_path().is_some()
            // ENGINE TRUTH (true-10 audit 2026-07-09): use_finetuned_asr is documented as
            // "overriding asr_model_size" — when the override is EFFECTIVE (flag on AND the model
            // present), the fine-tuned engine is the primary drafter, so the 7B pass must not run:
            // it re-attributed the fine-tuned text as an "omniasr-wsl-7b" hypothesis (wrong badge,
            // double vote for one engine in the jury) and hard-required a server that never
            // transcribes. Mirrors wsl7b_primary_unresolved below: with the flag on but the model
            // ABSENT, the 7B remains the primary and this stays true (never a silent stock
            // downgrade — the F2 contract).
            && !(self.settings.use_finetuned_asr && Self::finetuned_model_paths().is_some())
    }

    /// F2 — the no-silent-downgrade guard. True when the selected primary engine is WSL 7B but no
    /// client script is configured AND the fine-tuned engine is not available to serve as the
    /// primary instead. In that state the only thing left below is stock local CTC — which the owner
    /// never selected — so every primary-transcription entry point must FAIL LOUDLY here rather than
    /// silently downgrading (the fail-hard contract documented in settings.rs). Hypothesis
    /// generation is unaffected: it calls the ASR pool directly, not this primary path.
    fn wsl7b_primary_unresolved(&self) -> bool {
        self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B
            && self.settings.external_asr_script_path().is_none()
            && !(self.settings.use_finetuned_asr && Self::finetuned_model_paths().is_some())
    }

    /// F6 — fast preflight before an import that will drive the WSL 7B primary: confirm the warm
    /// server is actually accepting connections. The server binds 127.0.0.1:8799 INSIDE WSL's network
    /// namespace (not reachable from Windows directly), so probe from within WSL with a bash
    /// `/dev/tcp` open bounded by `timeout`. Without this, a down/hung server is only discovered
    /// per-segment after a 300 s transcription timeout — up to ~5 minutes of spinner before the
    /// fail-hard rollback. This turns that into a ~2-second, actionable failure at import start.
    fn wsl_7b_server_preflight(&self) -> AppResult<()> {
        if !self.should_use_wsl_primary_asr() {
            return Ok(());
        }
        // Uses the module-level WSL_7B_SERVER_PORT (same value handed to the client) — one source of truth.
        let probe = format!("timeout 3 bash -c 'exec 3<>/dev/tcp/127.0.0.1/{WSL_7B_SERVER_PORT}'");
        let mut cmd = std::process::Command::new("wsl");
        cmd.arg("bash").arg("-lc").arg(&probe);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        let mut child =
            cmd.spawn().map_err(|e| AppError::Other(format!("could not launch WSL to check the 7B server: {e}")))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => {
                    return Err(AppError::Validation(format!(
                        "{ASR_7B_UNAVAILABLE_TAG}: OmniASR-7B server is not responding on \
                         127.0.0.1:{WSL_7B_SERVER_PORT} (in WSL). Start it (e.g. wsl python \
                         cortex_7b_server.py from scripts/) and try again, or transcribe with the \
                         offline model. The import was not started, so nothing was left \
                         half-transcribed."
                    )));
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        kill_and_reap_wsl_child(&mut child, "7B preflight probe");
                        return Err(AppError::Validation(format!(
                            "{ASR_7B_UNAVAILABLE_TAG}: Timed out checking the OmniASR-7B server (WSL \
                             not responding). Ensure WSL and the 7B server are running and try \
                             again, or transcribe with the offline model."
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    kill_and_reap_wsl_child(&mut child, "7B preflight probe");
                    return Err(AppError::Other(format!("WSL 7B preflight wait failed: {e}")));
                }
            }
        }
    }

    /// The actionable, UI-classified error returned whenever [`Self::wsl7b_primary_unresolved`]
    /// holds. Carries [`ASR_7B_UNAVAILABLE_TAG`] so the frontend presents the two deliberate ways
    /// forward — start the 7B server (with the client script set) and retry the champion, or
    /// transcribe this one clip with the offline model — never a silent downgrade.
    fn primary_engine_unavailable_error() -> AppError {
        // Tagged so the UI offers the retry-or-offline choice rather than a dead-end (see
        // ASR_7B_UNAVAILABLE_TAG). The app never silently downgrades to a smaller model.
        AppError::Validation(format!(
            "{ASR_7B_UNAVAILABLE_TAG}: OmniASR-7B (the champion) is the selected engine but its WSL \
             client script is not configured (Settings → \"External ASR script path\" is empty). \
             Start the 7B server and set that path to transcribe with the champion, or choose the \
             offline model for this clip. Refusing to silently downgrade to a smaller model you did \
             not select."
        ))
    }

    fn local_asr_model_id(&self) -> &'static str {
        match self.active_local_asr_model_size() {
            crate::settings::AsrModelSize::CTC1B => "omniasr-ctc-1b",
            crate::settings::AsrModelSize::CTC300M | crate::settings::AsrModelSize::WSL7B => "omniasr-ctc-300m",
        }
    }

    fn with_asr<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut asr::KurdishAsrService) -> R,
    {
        let model_dir = self.model_manager.resolved_dir();
        self.asr_pool.with_service(&model_dir, &self.asr_config(), f)
    }

    /// Run the permanent gold-set eval end-to-end against the live local ASR engine.
    ///
    /// Unlike the model-agnostic [`crate::eval::run_gold_eval`] (which trusts caller
    /// hypotheses), this loads each gold clip, decodes + resamples it exactly like the
    /// import path, runs the pooled OmniASR CTC recognizer, and scores the *raw* ASR
    /// output against the gold reference — the only way a published WER/CER is
    /// reproducible from audio rather than asserted. Opens its own DB connection so no
    /// `AppState` lock is held across the (slow) ASR loop.
    pub fn run_gold_eval_asr(&self, model_id: Option<&str>) -> AppResult<crate::eval::EvalRunResult> {
        let db = self.open_db()?;
        let active_id = self.local_asr_model_id();
        // HONESTY GUARD (true-10 audit 2026-07-09): this entrypoint always transcribes with the
        // ACTIVE pooled local engine (transcribe_audio_file_raw → with_asr → asr_config), so a
        // caller-supplied label that names a DIFFERENT engine would persist a mislabeled WER/CER
        // row. Refuse the mismatch instead of recording it.
        if let Some(requested) = model_id {
            if requested != active_id {
                return Err(AppError::Validation(format!(
                    "run_gold_eval_asr transcribes with the active local engine '{active_id}', but \
                     the run was requested under the label '{requested}'. Eval rows are persisted \
                     under that label, so it must match the engine that actually runs — switch the \
                     active model or drop the label."
                )));
            }
        }
        let model_id = active_id.to_string();
        crate::eval::run_gold_eval_with_transcriber(&db, &model_id, |seg| {
            self.transcribe_audio_file_raw(&seg.audio_path)
        })
    }

    /// Decode an audio file and return the *raw* local-ASR transcript — no LLM
    /// refinement, no normalization — i.e. the exact hypothesis used for gold WER/CER.
    pub fn transcribe_audio_file_raw(&self, audio_path: &str) -> AppResult<String> {
        let (sample_rate, pcm) = audio::decode_to_pcm(audio_path)?;
        let (_sr, pcm16) = audio::ensure_pcm_16khz(sample_rate, pcm)?;
        let f32_pcm: Vec<f32> = pcm16.iter().map(|&s| s as f32 / 32768.0).collect();
        self.with_asr(|asr| {
            if !asr.is_available() {
                return Err(AppError::Other("ASR model not loaded".into()));
            }
            asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
                .map(|(text, _confidence, _source)| text)
                .map_err(AppError::Other)
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
        if self.settings.llm_api_key.trim().is_empty() {
            return Err(AppError::Other(
                "Gemini API key is required for whole-file reference transcript when jury cloud opt-in is enabled"
                    .to_string(),
            ));
        }

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

            match crate::agentic::generate_whole_file_reference_transcript(
                path,
                &model,
                &self.settings.llm_api_key,
                &output_dir,
            ) {
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
        // P3.2: open a resume journal for this import (best-effort — a journal failure never fails the
        // import). A crash leaves this job 'running'; the next launch can offer to resume it.
        let job_id: Option<String> = db.begin_import_job(&dir_path.to_string_lossy(), total).ok();
        callback(PipelineEvent::Started { total });
        self.reset_finetuned_counters();
        callback(PipelineEvent::Phase { phase: "importing".into() });
        self.set_import_status(0, total, "");
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
        let mut failed = 0;
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
            if resume_should_skip_file(resume_completed.is_some(), journaled, !resume_existing_ids.is_empty()) {
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
                    callback(multi_model_hypothesis_stage(&db, fname.clone(), &segments));
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
                    failed += 1;
                    callback(PipelineEvent::Error { file: fname, error: e.to_string() });
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

        let (chunk_ranges, vad_backend) = chunking::plan_speech_chunks(
            &pcm,
            sample_rate,
            self.settings.vad_threshold,
            self.settings.min_segment_duration_ms,
            self.settings.max_segment_duration_ms,
        )?;

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
        for (seg_id, f32_pcm) in pcm_cache {
            if let Err(error) = self.populate_hypotheses(db, &seg_id, &f32_pcm) {
                log_hypothesis_population_failure(&seg_id, &error);
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
        let windows: Arc<Mutex<Vec<audio::PcmWindow>>> = Arc::new(Mutex::new(Vec::new()));
        let acc = Arc::clone(&windows);
        let path_buf = path.to_path_buf();
        audio::decode_pcm_windows_with_timeout(path_buf, audio::DECODE_WINDOW_MS, decode_timeout, move |window| {
            lock_decoded_windows(&acc).push(window);
            Ok(())
        })?;

        let windows = {
            // MOVE the decoded windows out of the mutex instead of cloning them. The decode callback has
            // already finished (decode_pcm_windows_with_timeout returned), so nothing else touches the
            // Vec; cloning here held the ENTIRE file's PCM twice — exactly what the streaming path exists
            // to avoid. std::mem::take leaves an empty Vec behind and releases the lock without the copy.
            let mut guard = lock_decoded_windows(&windows);
            std::mem::take(&mut *guard)
        };

        if windows.is_empty() {
            return Err(AppError::Validation("Empty audio buffer".into()));
        }

        let estimated_total =
            ((duration_ms as f64 / self.settings.max_segment_duration_ms.max(1) as f64).ceil() as usize).max(1);
        let mut global_chunk = 0usize;

        let mut segments = Vec::new();
        let mut all_pcm_cache = Vec::new();
        let num_windows = windows.len();
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
        for (w_idx, window) in windows.into_iter().enumerate() {
            if let Some(token) = cancel {
                token.check()?;
            }
            let is_last = w_idx + 1 == num_windows;

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
                continue;
            }
            let pcm = effective_pcm;

            let (mut chunk_ranges, vad_backend) = chunking::plan_speech_chunks(
                &pcm,
                sample_rate,
                self.settings.vad_threshold,
                self.settings.min_segment_duration_ms,
                self.settings.max_segment_duration_ms,
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
                continue;
            }

            let global_ranges: Vec<(usize, usize)> =
                chunk_ranges.iter().map(|&(s, e)| (base_sample + s, base_sample + e.min(pcm.len()))).collect();

            let mut window_progress = |_: usize, _: usize| {
                global_chunk += 1;
                on_chunk(global_chunk, estimated_total.max(global_chunk));
            };

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
            )?;
            segments.extend(window_segs);
            all_pcm_cache.extend(window_pcm_cache);
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
        for (seg_id, f32_pcm) in all_pcm_cache {
            if let Err(error) = self.populate_hypotheses(db, &seg_id, &f32_pcm) {
                log_hypothesis_population_failure(&seg_id, &error);
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
    ) -> AppResult<(Vec<SpeechSegment>, Vec<(String, Vec<f32>)>)> {
        let chunk_count = chunk_ranges.len() as u32;
        let chunk_total = chunk_ranges.len().max(1);
        let active_asr_model_size = self.active_local_asr_model_size();
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

        // Round-23 #5: hash the audio file ONCE for the whole run (its content is invariant), then key
        // every per-chunk cache get/set on that hash — instead of re-reading + re-hashing the entire
        // file on each of the N chunks (O(N·filesize) of redundant I/O on long recordings). `None` when
        // the file is unhashable, which simply means "no cache for this run" (same effect as before).
        let file_hash = crate::cache::TranscriptCache::compute_hash(path).ok();

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
            let finetuned_text: Option<String> = if self.settings.use_finetuned_asr {
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
                file_hash.as_deref().and_then(|h| self.cache.get_chunk_by_hash(h, &model_id, Some(&chunk_suffix)))
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
                    if let Some(h) = file_hash.as_deref() {
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
            pcm_cache.push((seg_id.clone(), f32_pcm));

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
                agent_confidence: None,
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
            let text = crate::corrections::loop0_draft_text(
                seg.annotated_transcript.as_deref(),
                seg.normalized_transcript.as_deref(),
                &seg.raw_transcript,
            );
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
        // finalized text (annotated ▸ normalized ▸ raw — the real 7B transcript post-pass).
        let mut by_path: std::collections::HashMap<String, Vec<(String, Option<String>, String)>> =
            std::collections::HashMap::new();
        for s in segments {
            let text = crate::corrections::loop0_draft_text(
                s.annotated_transcript.as_deref(),
                s.normalized_transcript.as_deref(),
                &s.raw_transcript,
            )
            .to_string();
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

        // The warm 7B server can transiently fail or return an empty result for a clip (e.g. while
        // still under load right after launch), which would otherwise leave that segment stuck at its
        // "[Pending WSL 7B ASR]" placeholder for good (observed in stress testing: 1 of 3 segments).
        // Retry a few times before giving up so an import reliably transcribes every segment; only
        // escalate after the retries are exhausted, rather than silently shipping a pending segment.
        const MAX_ATTEMPTS: usize = 3;
        // FORCE-USE the Champion (fail-hard): if the 7B server is unreachable/hung/errored — an
        // INFRASTRUCTURE failure, i.e. the client process exits non-zero (its honest failure contract),
        // as opposed to a REACHABLE server legitimately returning an empty transcript for a silent clip —
        // this import is CANCELLED and every segment it just created is rolled back. The user then starts
        // the 7B server and re-imports cleanly, instead of being left with a library of
        // "[Pending WSL 7B ASR]" placeholders or silently-downgraded output the owner never asked for.
        let import_ids: Vec<String> = segments.iter().map(|s| s.id.clone()).collect();
        let mut updated = 0usize;
        for seg in segments.iter_mut() {
            // Honor cancellation between segments: each WSL transcription can ride a 300s timeout, so
            // without this a cancelled import of an N-segment file would keep running for up to N*300s.
            if let Some(token) = cancel {
                if let Err(cancel_err) = token.check() {
                    // Roll back THIS file's just-created segments (a mix of transcribed + still-placeholder
                    // rows) before propagating the cancel, so re-importing the same file cannot duplicate
                    // every segment. Matches the infra-failure rollback and the F6 "nothing half-imported"
                    // promise. Earlier files in a directory import are already committed and untouched.
                    tracing::info!("WSL 7B import cancelled; rolling back {} segment(s)", import_ids.len());
                    if let Err(e) = db.delete_segments_batch(&import_ids) {
                        tracing::error!("failed to roll back {} segment(s) after cancel: {e}", import_ids.len());
                    }
                    return Err(cancel_err);
                }
            }
            let mut last_problem: Option<String> = None;
            let mut infra_failure = false;
            for attempt in 1..=MAX_ATTEMPTS {
                match self.transcribe(
                    Some(seg.id.as_str()),
                    &seg.audio_path,
                    seg.alignment_json.as_deref(),
                    cancel.map(|t| t.as_atomic()),
                ) {
                    Ok(_draft) => {
                        if let Err(e) = self.refresh_segment_from_db(db, seg) {
                            // A DB hiccup mid-pass otherwise left a partial import (some transcribed, some
                            // still placeholder) with no rollback; a re-import would then duplicate it.
                            tracing::error!(
                                "WSL 7B import: DB error mid-pass ({e}); rolling back {} segment(s)",
                                import_ids.len()
                            );
                            // A failed rollback must be LOUD (matches the cancel/infra siblings above):
                            // the log just promised a rollback, and if the delete also fails the
                            // placeholders survive — re-importing the file then duplicates them.
                            if let Err(rollback_err) = db.delete_segments_batch(&import_ids) {
                                tracing::error!(
                                    "failed to roll back {} segment(s) after mid-pass DB error: {rollback_err}",
                                    import_ids.len()
                                );
                            }
                            return Err(e);
                        }
                        let usable = !seg.raw_transcript.trim().is_empty() && !seg.raw_transcript.contains("[Pending");
                        if usable {
                            // Record the Champion's output as its hypothesis so the review provenance badge
                            // can honestly name "OmniASR-7B Champion" as the producing engine — the primary
                            // raw_transcript otherwise carries no model id. Best-effort: a provenance-write
                            // failure must not fail the (successful) transcription.
                            if let Err(e) = insert_hypothesis_checked(
                                db,
                                &seg.id,
                                "omniasr-wsl-7b",
                                seg.raw_transcript.clone(),
                                None,
                            ) {
                                tracing::warn!("could not record omniasr-wsl-7b provenance for {}: {e}", seg.id);
                            }
                            updated += 1;
                            last_problem = None;
                            infra_failure = false;
                            break;
                        }
                        // Reachable server but no words back — could be a genuinely silent clip. NOT an
                        // infrastructure failure: escalate only this segment after retries, never cancel.
                        last_problem = Some("7B returned an empty transcript".to_string());
                        infra_failure = false;
                    }
                    Err(error) => {
                        let msg = error.to_string();
                        if msg.contains(WSL_7B_EMPTY_RESULT_MARKER) {
                            // A legit-but-EMPTY 7B result reaches here as an Err ONLY because transcribe()
                            // converts Ok("") -> Err to stop the re-transcribe IPCs from blank-overwriting a
                            // stored transcript. For the IMPORT pass that is NOT an infrastructure failure —
                            // the server is reachable and simply produced no words for a silent/music/noise
                            // chunk. Treat it EXACTLY like the Ok-arm usable=false path: escalate only THIS
                            // segment after retries, never roll the whole file back (which would discard the
                            // good transcripts already computed for the file's other chunks). Two in-code
                            // contracts (parse_wsl_segment_result + the Ok-arm comment) require this.
                            last_problem = Some("7B returned an empty transcript".to_string());
                            infra_failure = false;
                        } else {
                            // The client exited non-zero: server not running / unreachable / hung / errored.
                            // Fatal for a force-7B import. A 5-minute per-attempt timeout means the server is
                            // HUNG, not transiently flaky — another full-timeout attempt only triples the
                            // stall, so stop fast. Quick failures (connection refused) still retry briefly in
                            // case the server is mid-launch.
                            let hung = msg.contains("timed out");
                            last_problem = Some(msg);
                            infra_failure = true;
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
            if infra_failure {
                let reason = last_problem.unwrap_or_else(|| "7B server unreachable".to_string());
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
            if let Some(reason) = last_problem {
                tracing::warn!("WSL 7B import: segment {} failed after {MAX_ATTEMPTS} attempts: {reason}", seg.id);
                if let Err(e) = self.mark_wsl_primary_unavailable(db, seg, &reason) {
                    tracing::error!(
                        "WSL 7B import: DB error mid-pass ({e}); rolling back {} segment(s)",
                        import_ids.len()
                    );
                    // Same loud-rollback contract as the sibling sites: a swallowed delete failure
                    // here leaves placeholder rows the promised rollback never removed.
                    if let Err(rollback_err) = db.delete_segments_batch(&import_ids) {
                        tracing::error!(
                            "failed to roll back {} segment(s) after mid-pass DB error: {rollback_err}",
                            import_ids.len()
                        );
                    }
                    return Err(e);
                }
            }
        }
        Ok(updated)
    }

    fn mark_wsl_primary_unavailable(&self, db: &Database, seg: &mut SpeechSegment, reason: &str) -> AppResult<()> {
        let rationale = format!("WSL 7B primary ASR unavailable before jury: {reason}");
        tracing::warn!("{} ({})", rationale, seg.id);
        // Explicit lowest confidence (0.0), NOT None: a None here becomes COALESCE(agent_confidence, 0.5)
        // in the suspect-first queue, tying these unresolved-primary clips (empty/failed 7B, unknown
        // quality — exactly the ones most needing attention) at the 0.5 plateau to sort by id. 0.0 sorts
        // them to the very front.
        db.write_segment_verdict(&seg.id, "escalated", None, Some(&rationale), None, Some(0.0), true)?;
        self.refresh_segment_from_db(db, seg)?;
        Ok(())
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
        // Cloud STT (ElevenLabs Scribe) first when opted in and a key is configured: one API call,
        // segmented from word timestamps. On ANY error, fall through to the local ASR path below so
        // an import never fails just because the cloud is unavailable.
        let result = 'transcribe: {
            if let Some(scribe_key) = self.scribe_api_key_if_enabled() {
                if let Some(token) = cancel.as_ref() {
                    if let Err(e) = token.check() {
                        break 'transcribe Err(e);
                    }
                }
                on_event(PipelineEvent::Phase { phase: "transcribing".into() });
                on_event(agent_stage(
                    "audio_chunking",
                    "running",
                    fname.clone(),
                    "Transcribing whole file with ElevenLabs Scribe (cloud)",
                    0,
                    1,
                ));
                match self.import_single_file_via_scribe(path, &db, &scribe_key) {
                    Ok(segs) => {
                        chunks_done = segs.len();
                        break 'transcribe Ok(segs);
                    }
                    Err(e) => {
                        tracing::warn!("Scribe import failed ({e}); falling back to local ASR");
                        on_event(agent_stage(
                            "audio_chunking",
                            "running",
                            fname.clone(),
                            "Scribe unavailable — using local ASR",
                            0,
                            estimated_chunks,
                        ));
                    }
                }
            }
            self.process_single_file_with_progress(path, &db, cancel.as_ref(), |current, total| {
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
            })
        };

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
                on_event(multi_model_hypothesis_stage(&db, fname.clone(), segments));

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

    /// The ElevenLabs Scribe key to use for cloud STT, or `None` when cloud STT is not opted in or no
    /// key is configured. Mirrors the cloud-LLM opt-in gate; the key lives in `secrets.env` next to
    /// the database and is never read unless the user has explicitly opted in (privacy by default).
    fn scribe_api_key_if_enabled(&self) -> Option<String> {
        // Snapshot AND live consent — see LiveConsent. Returning None here means the key is never
        // even read from disk, so a mid-import withdrawal stops the upload at the source.
        if !self.settings.cloud_stt_opt_in || !self.consent.cloud_stt() {
            return None;
        }
        let data_dir = std::path::Path::new(&self.db_path).parent()?;
        crate::api_keys::ApiKeys::load(data_dir).elevenlabs
    }

    /// Import a file using ElevenLabs Scribe as the transcriber: ONE API call for the whole file,
    /// segmented from Scribe's word timestamps into source-file slices (no local ASR/VAD/diarization).
    /// Persists and returns the segments. The caller falls back to the local path on error, so this
    /// surfaces failures rather than masking them. Acoustic-quality/diarization fields are left unset.
    pub fn import_single_file_via_scribe(
        &self,
        path: &Path,
        db: &Database,
        api_key: &str,
    ) -> AppResult<Vec<SpeechSegment>> {
        let duration_ms = audio::get_duration_ms(path)?;
        if duration_ms == 0 {
            return Err(AppError::Validation("Empty audio file".into()));
        }
        let audio_path = path.to_string_lossy().to_string();
        let model_id = crate::scribe_api::DEFAULT_MODEL;
        let scribe_segs = crate::scribe_api::transcribe_segments(
            &audio_path,
            api_key,
            model_id,
            crate::scribe_api::SORANI_LANGUAGE_CODE,
        )?;
        let segments = Self::build_scribe_speech_segments(
            &scribe_segs,
            &audio_path,
            duration_ms,
            self.settings.auto_normalize,
            self.settings.verbalize_numbers,
            model_id,
        );
        if segments.is_empty() {
            return Err(AppError::Other("Scribe returned no segments".into()));
        }
        db.insert_segments_batch(&segments)?;
        Ok(segments)
    }

    /// Build persistable [`SpeechSegment`]s from Scribe segments. Each becomes a source-file slice
    /// (`audio_path` + `SegmentSourceMeta` time range) so it plays back the right region; text is
    /// stored in logical (reading) order and normalized when auto-normalize is on. A segment with an
    /// open end (0) extends to the file duration. Acoustic-quality fields stay `None` — Scribe gives
    /// text and timing, not waveform metrics.
    fn build_scribe_speech_segments(
        scribe_segs: &[crate::scribe_api::ScribeSegment],
        audio_path: &str,
        total_duration_ms: i64,
        auto_normalize: bool,
        verbalize_numbers: bool,
        model_id: &str,
    ) -> Vec<SpeechSegment> {
        let chunk_count = scribe_segs.len() as u32;
        scribe_segs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let start = s.source_start_ms.max(0);
                let end = if s.source_end_ms > start { s.source_end_ms } else { total_duration_ms.max(start) };
                let meta = crate::chunking::SegmentSourceMeta {
                    source_start_ms: start,
                    source_end_ms: end,
                    chunk_index: i as u32,
                    chunk_count,
                };
                let normalized = if auto_normalize && !s.text.trim().is_empty() {
                    let cfg = crate::normalizer::NormalizationConfig {
                        normalize_numbers: auto_normalize,
                        verbalize_numbers,
                        normalize_hamza: true,
                        remove_diacritics: false,
                    };
                    Some(crate::normalizer::SoraniNormalizer::with_config(cfg).normalize(&s.text))
                } else {
                    None
                };
                SpeechSegment {
                    id: Uuid::new_v4().to_string(),
                    audio_path: audio_path.to_string(),
                    raw_transcript: s.text.clone(),
                    normalized_transcript: normalized,
                    alignment_json: Some(meta.to_alignment_json()),
                    duration_ms: end.saturating_sub(start).max(0),
                    // PROVENANCE: Scribe is the one path that uploads raw audio to a cloud provider, so
                    // these rows must say so durably. `..Default::default()` here persisted
                    // cloud_call=false and model_version_id=NULL for exactly the segments whose audio
                    // left the machine. Scribe returns no per-segment confidence, so `confidence` stays
                    // None and `confidence_source` honestly stays None with it.
                    cloud_call: true,
                    model_version_id: Some(model_id.to_string()),
                    ..Default::default()
                }
            })
            .collect()
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
        if self.settings.use_finetuned_asr {
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
                                if let Err(error) = self.populate_hypotheses(&db, id, &f32_pcm) {
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

        if self.should_use_wsl_primary_asr() {
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

                // Tag a 7B failure (server down / timeout / empty) so the UI offers retry-or-offline
                // instead of a dead-end — and NEVER silently falls through to a smaller model here.
                let (raw_transcript, confidence) =
                    self.run_wsl_segment_transcript(&id, cancel).map_err(tag_7b_unavailable)?;

                // A TRANSIENT empty 7B result (server up but under load) comes back as Ok("") — NOT an Err
                // — so the map_err(tag_7b_unavailable) above does not catch it. Do not let it fall through
                // to the write below: update_asr_transcript_if_unreviewed would replace a good, unverified
                // stored transcript with "" (silent data loss). Both re-transcribe entry points route
                // through here (batch_transcribe + the per-segment transcribe IPC) with no retry, unlike
                // the import path which retries/escalates for exactly this transient. Surface it as the
                // retry-or-offline 7B failure the tag above promises, leaving the existing transcript intact.
                if raw_transcript.trim().is_empty() {
                    return Err(tag_7b_unavailable(AppError::Other(format!(
                        "{WSL_7B_EMPTY_RESULT_MARKER} (the server is likely under load); the existing transcript is left unchanged"
                    ))));
                }

                let db = crate::db::Database::open(&self.db_path).map_err(|e| AppError::Other(e.to_string()))?;

                let normalized_transcript = if self.settings.auto_normalize && !raw_transcript.is_empty() {
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

                // Use the safe update method: human decisions are never overwritten.
                let updated = db
                    .update_asr_transcript_if_unreviewed(
                        &id,
                        &raw_transcript,
                        normalized_transcript.as_deref(),
                        confidence,
                        Some("external_provider"),
                        Some("omniasr-wsl-7b"),
                        false,
                    )
                    .map_err(|e| AppError::Other(format!("Failed to update segment in database: {}", e)))?;
                if !updated {
                    tracing::info!("WSL 7B: segment {id} has a human decision — transcript not overwritten.");
                }

                // Insert WSL 7B hypothesis for downstream jury comparison.
                if let Err(error) =
                    insert_hypothesis_checked(&db, &id, "omniasr-wsl-7b", raw_transcript.clone(), confidence)
                {
                    tracing::error!("{error}");
                }

                // Populate local hypotheses for comparison
                let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();
                if let Err(error) = self.populate_hypotheses(&db, &id, &f32_pcm) {
                    log_hypothesis_population_failure(&id, &error);
                }

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
                        let hyps: Vec<String> = match db.get_hypotheses_for_segment(&id) {
                            Ok(hyps) => hyps.into_iter().map(|h| h.transcript).collect(),
                            Err(e) => {
                                tracing::warn!(
                                    "GER: could not load N-best hypotheses for {id}: {e}; refining unprimed"
                                );
                                Vec::new()
                            }
                        };
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
                            tracing::warn!("LLM Refinement failed: {}. Falling back to raw transcript.", e);
                            raw_transcript.clone()
                        }
                    }
                } else {
                    raw_transcript.clone()
                };

                // LOOP 0: when enabled, correct previously-learned confusions in the final text
                // before it is returned/stored (opt-in; default off; best-effort).
                let final_text = apply_loop0_firing(self.settings.loop0_firing_enabled, &db, &final_text);

                let cloud_call = self.llm_refinement_uses_cloud();
                return Ok(TranscriptionDraft {
                    raw_text: raw_transcript,
                    final_text,
                    confidence,
                    confidence_source: Some("external_provider".to_string()),
                    model_version_id: Some("omniasr-wsl-7b".to_string()),
                    cloud_call,
                });
            } else {
                return Err(AppError::Other(
                    "Segment not found in database. Please import the audio file first to generate speech segments."
                        .into(),
                ));
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
                    tracing::warn!("LLM Refinement failed: {}. Falling back to raw transcript.", e);
                    raw_text.clone()
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
                if let Err(error) = self.populate_hypotheses(&db, id, &f32_pcm) {
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
        // The search itself now lives in `models.rs`, because this reasoning had been applied HERE
        // and not to the identical lookup behind the `transcribe_segment_finetuned` IPC command —
        // which consequently could not find the model at all. One implementation, two callers.
        crate::models::finetuned_model_paths()
    }

    /// Transcribe one decoded chunk (16 kHz mono i16) with the fine-tuned engine. The fine-tuned
    /// model is trained on short utterances, so a single >~15 s pass can duplicate text — sub-split a
    /// long chunk into balanced ~15 s windows and join the per-window transcripts.
    // pub(crate): the transcribe_segment_finetuned IPC must share this windowing instead of calling
    // run_wav2vec2 directly — a single unbounded pass over >15 s audio duplicates text on this model
    // (true-10 audit 2026-07-09: same engine, two call paths, different quality).
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

        let model_dir = self.model_manager.resolved_dir();
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
        let model_dir = self.model_manager.resolved_dir();

        // 1. OmniASR 300M
        let model_id_300m = "omniasr-ctc-300m";
        let config_300m = asr::AsrLoadConfig {
            model_size: crate::settings::AsrModelSize::CTC300M,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        let res_300m = self.asr_pool.with_service(&model_dir, &config_300m, |asr| {
            if !asr.is_available() {
                return None;
            }
            Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE))
        });
        match res_300m {
            Some(Ok((text, conf, _source))) => insert_hypothesis_checked(db, segment_id, model_id_300m, text, conf)?,
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
        let res_1b = self.asr_pool.with_service(&model_dir, &config_1b, |asr| {
            if !asr.is_available() {
                return None;
            }
            Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE))
        });
        match res_1b {
            Some(Ok((text, conf, _source))) => insert_hypothesis_checked(db, segment_id, model_id_1b, text, conf)?,
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
        if let Some((onnx, vocab)) = Self::finetuned_model_paths() {
            let chunk_i16: Vec<i16> = f32_pcm.iter().map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16).collect();
            match Self::transcribe_chunk_finetuned(&onnx, &vocab, &chunk_i16) {
                Ok(text) if !text.trim().is_empty() => {
                    insert_hypothesis_checked(db, segment_id, "finetuned-mms-ckb", text, None)?;
                }
                Ok(_) => tracing::debug!("finetuned-mms-ckb hypothesis empty for {segment_id}"),
                Err(error) => {
                    tracing::warn!("finetuned-mms-ckb hypothesis transcription failed for {segment_id}: {error}");
                }
            }
        }

        self.populate_wsl_hypothesis_if_configured(db, segment_id)?;

        Ok(())
    }

    fn populate_wsl_hypothesis_if_configured(&self, db: &Database, segment_id: &str) -> AppResult<()> {
        if self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
            return Ok(());
        }
        if self.settings.external_asr_script_path().is_none() {
            return Ok(());
        }
        if db
            .get_hypotheses_for_segment(segment_id)?
            .iter()
            .any(|hyp| hyp.model_id == "omniasr-wsl-7b" && !hyp.transcript.trim().is_empty())
        {
            return Ok(());
        }

        match self.run_wsl_segment_transcript(segment_id, None) {
            Ok((raw_transcript, confidence)) => {
                insert_hypothesis_checked(db, segment_id, "omniasr-wsl-7b", raw_transcript, confidence)?;
            }
            Err(error) => {
                tracing::warn!("omniasr-wsl-7b hypothesis transcription failed for {segment_id}: {error}");
            }
        }
        Ok(())
    }

    fn run_wsl_segment_transcript(
        &self,
        segment_id: &str,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<(String, Option<f64>)> {
        let Some(external_script) = self.settings.external_asr_script_path() else {
            return Err(AppError::Validation(
                "External ASR provider is not configured. Set the WSL script path in Settings before using the 7B provider.".into(),
            ));
        };
        run_wsl_segment_transcript_with_script(&external_script, segment_id, &self.db_path, cancel)
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
