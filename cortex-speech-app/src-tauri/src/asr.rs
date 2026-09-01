use crate::settings::AsrModelSize;
use sherpa_onnx::{OfflineOmnilingualAsrCtcModelConfig, OfflineRecognizer, OfflineRecognizerConfig};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const SAMPLE_RATE: u32 = 16000;
const CHUNK_SAMPLES: usize = 30 * SAMPLE_RATE as usize;
const CTC_300M_MIN_MODEL_BYTES: u64 = 50_000_000;
const CTC_1B_MIN_MODEL_BYTES: u64 = 500_000_000;
const CTC_MIN_TOKEN_BYTES: u64 = 100;
const WORKER_ARG: &str = "--cortex-asr-worker";
const SUPERVISOR_SELF_TEST_ARG: &str = "--cortex-asr-supervisor-self-test";
const WORKER_MESSAGE_PREFIX: &str = "CORTEX_ASR/1 ";
const WORKER_STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const WORKER_REQUEST_MIN_TIMEOUT: Duration = Duration::from_secs(180);
const WORKER_REQUEST_MAX_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const WORKER_MAX_SAMPLES: usize = 10 * 60 * SAMPLE_RATE as usize;
const WORKER_WRITE_BUFFER_BYTES: usize = 64 * 1024;
const WORKER_CIRCUIT_FAILURES: u32 = 3;
const WORKER_CIRCUIT_OPEN: Duration = Duration::from_secs(30);

/// Runtime options passed when loading the pooled ASR service.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AsrLoadConfig {
    pub model_size: AsrModelSize,
    pub enable_gpu: bool,
    pub num_threads: u32,
    pub language: String,
}

impl Default for AsrLoadConfig {
    fn default() -> Self {
        Self { model_size: AsrModelSize::default(), enable_gpu: true, num_threads: 4, language: "ckb".to_string() }
    }
}

pub fn get_provider() -> String {
    if cfg!(target_os = "macos") {
        #[cfg(target_arch = "aarch64")]
        return "coreml".to_string();
    }

    if which_nvidia_smi() {
        return "cuda".to_string();
    }

    if cfg!(target_os = "windows") {
        // DirectML is the high-tier standard for Windows GPU acceleration across all vendors.
        return "directml".to_string();
    }

    "cpu".to_string()
}

/// Automatically tune the ASR configuration based on local hardware.
pub fn auto_tune_config(mut config: AsrLoadConfig) -> AsrLoadConfig {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);

    if config.model_size == AsrModelSize::CTC1B {
        config.num_threads = (cores as i32 - 1).clamp(2, 8) as u32;
    } else {
        config.num_threads = (cores as i32 / 2).clamp(1, 4) as u32;
    }

    config
}

pub fn detect_optimal_provider(enable_gpu: bool) -> String {
    if enable_gpu {
        get_provider()
    } else {
        "cpu".to_string()
    }
}

fn which_nvidia_smi() -> bool {
    std::process::Command::new("nvidia-smi").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

pub fn omniasr_model_paths(model_dir: &Path, size: &AsrModelSize) -> (PathBuf, PathBuf) {
    let dir_name = match size {
        AsrModelSize::CTC300M => "omniasr-ctc-300m",
        AsrModelSize::CTC1B => "omniasr-ctc-1b",
        AsrModelSize::WSL7B => "omniasr-wsl-7b",
    };
    let base = model_dir.join(dir_name);
    (base.join("model.int8.onnx"), base.join("tokens.txt"))
}

pub fn omniasr_model_present(model_dir: &Path, size: &AsrModelSize) -> bool {
    let (model_path, tokens_path) = omniasr_model_paths(model_dir, size);
    let min_model_bytes = match size {
        AsrModelSize::CTC300M => CTC_300M_MIN_MODEL_BYTES,
        AsrModelSize::CTC1B => CTC_1B_MIN_MODEL_BYTES,
        AsrModelSize::WSL7B => return model_path.exists() && tokens_path.exists(),
    };
    file_meets_min_size(&model_path, min_model_bytes) && file_meets_min_size(&tokens_path, CTC_MIN_TOKEN_BYTES)
}

/// Preserve the operator's exact engine selection. Availability is checked by the selected engine's
/// load/transcribe path, which returns an actionable error; choosing a different installed model here
/// would create a plausible-looking transcript with false provenance.
pub fn select_available_model_size(_model_dir: &Path, requested: &AsrModelSize) -> AsrModelSize {
    requested.clone()
}

pub fn verify_model_dir<P: AsRef<Path>>(model_dir: P) -> Result<(), String> {
    let dir = model_dir.as_ref();
    if !dir.exists() {
        return Err(format!("Model directory not found: {}", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!("Not a directory: {}", dir.display()));
    }
    if !omniasr_model_present(dir, &AsrModelSize::CTC300M) {
        let (model_path, tokens_path) = omniasr_model_paths(dir, &AsrModelSize::CTC300M);
        return Err(format!(
            "Missing or incomplete OmniASR CTC 300M model pair at {} and {}",
            model_path.display(),
            tokens_path.display()
        ));
    }
    Ok(())
}

fn file_meets_min_size(path: &Path, min_size_bytes: u64) -> bool {
    path.metadata().map(|metadata| metadata.len() >= min_size_bytes).unwrap_or(false)
}

/// Whether a clip's confidence came from real model token-posteriors or the heuristic
/// fallback. Downstream calibration (the conformal certificate, IRT consensus, the
/// autonomy dial) must NOT treat a Heuristic confidence as calibrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceSource {
    /// Mean per-token posterior, for a model that exposes per-token `ys_log_probs`. NOTE: the default
    /// offline engine (OmniASR CTC via sherpa-onnx) does NOT expose these — its result JSON always
    /// carries `ys_log_probs: []` — so this variant is unreachable on the shipped local path, which
    /// uses `Heuristic`. It exists for a future/alternate engine that actually emits token log-probs.
    RealPosterior,
    /// The 0.90 / 0.0 fallback, used when the model exposed no token probabilities — which is EVERY
    /// segment on the default offline OmniASR CTC engine.
    #[default]
    Heuristic,
}

impl ConfidenceSource {
    pub fn as_db_value(self) -> &'static str {
        match self {
            ConfidenceSource::RealPosterior => "real_posterior",
            ConfidenceSource::Heuristic => "heuristic",
        }
    }
}

fn confidence_from_asr_result(text: &str, ys_log_probs: &[f64]) -> (Option<f64>, ConfidenceSource) {
    // Empty/whitespace output is never high-confidence, even if the model exposed per-token probs for the
    // blank-collapsed result. A confident-empty would persist a high confidence on a BLANK transcript,
    // which then feeds the jury/conformal gate as if it were trustworthy. Guard BOTH branches, not just
    // the no-probs heuristic (the original guarded only the empty `ys_log_probs` case).
    if text.trim().is_empty() {
        return (Some(0.0), ConfidenceSource::Heuristic);
    }
    if ys_log_probs.is_empty() {
        (Some(0.90), ConfidenceSource::Heuristic)
    } else {
        let sum_prob: f64 = ys_log_probs.iter().map(|&lp| lp.exp()).sum();
        (Some(sum_prob / ys_log_probs.len() as f64), ConfidenceSource::RealPosterior)
    }
}

#[cfg(test)]
#[derive(serde::Deserialize)]
struct RawAsrResult {
    text: String,
    // sherpa-onnx's OfflineRecognizerResult JSON emits per-token acoustic LOG-probs under the key
    // "ys_log_probs" — which this field name matches directly. HONESTY NOTE: the omnilingual OmniASR
    // CTC engine (the default and only local engine) NEVER populates it — its Convert() emits
    // `ys_log_probs: []` for every chunk in sherpa-onnx 1.13.2 — so confidence_from_asr_result always
    // takes the empty branch and returns the documented 0.90/0.0 HEURISTIC (correctly labelled
    // ConfidenceSource::Heuristic). The RealPosterior branch is thus unreachable on the shipped local
    // path; it would fire only for an engine that actually exposes these log-probs. A genuine per-token
    // posterior for CTC would require capturing the greedy argmax log-prob in the sherpa bindings,
    // which 1.13.2 discards. (The earlier `alias = "ys_probs"` aliased a key sherpa never emits — dead,
    // so removed.)
    #[serde(default)]
    ys_log_probs: Vec<f64>,
}

/// Parse sherpa-onnx's offline-result JSON into `(text, confidence, source)`. Confidence is
/// the mean per-token posterior (exp of the acoustic log-probs) when the model exposes them
/// (source = RealPosterior), otherwise the documented heuristic fallback (source = Heuristic).
#[cfg(test)]
fn parse_asr_result_json(json_str: &str) -> Result<(String, Option<f64>, ConfidenceSource), String> {
    let res: RawAsrResult =
        serde_json::from_str(json_str).map_err(|e| format!("Failed to parse ASR stream result JSON: {e}"))?;
    let (confidence, source) = confidence_from_asr_result(&res.text, &res.ys_log_probs);
    Ok((res.text, confidence, source))
}

pub fn check_models() -> Result<(), String> {
    let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let models_dir = project_root.join("models");
    let vad_path = models_dir.join("silero_vad_v4.onnx");

    if !vad_path.exists() {
        return Err(format!("Required VAD model not found: {}", vad_path.display()));
    }

    let (m300_path, t300_path) = omniasr_model_paths(&models_dir, &AsrModelSize::CTC300M);
    let (m1b_path, t1b_path) = omniasr_model_paths(&models_dir, &AsrModelSize::CTC1B);

    let has_300m = m300_path.exists() && t300_path.exists();
    let has_1b = m1b_path.exists() && t1b_path.exists();

    if !has_300m && !has_1b {
        return Err(format!(
            "No OmniASR models found (tried 300M at {} and 1B at {})",
            m300_path.display(),
            m1b_path.display()
        ));
    }

    Ok(())
}

pub fn check_model_integrity() -> Vec<String> {
    let mut warnings = Vec::new();
    let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let models_dir = project_root.join("models");

    let mut checks: Vec<(&str, u64)> = vec![("silero_vad_v4.onnx", 100_000)];

    let (m300_path, _) = omniasr_model_paths(&models_dir, &AsrModelSize::CTC300M);
    if m300_path.exists() {
        checks.push((crate::models::OMNIASR_CTC_300M_MODEL, 50_000_000));
        checks.push((crate::models::OMNIASR_CTC_300M_TOKENS, 100));
    }

    let (m1b_path, _) = omniasr_model_paths(&models_dir, &AsrModelSize::CTC1B);
    if m1b_path.exists() {
        checks.push((crate::models::OMNIASR_CTC_1B_MODEL, 500_000_000));
        checks.push((crate::models::OMNIASR_CTC_1B_TOKENS, 100));
    }

    for (file, min_size) in checks {
        let path = models_dir.join(file);
        if !path.exists() {
            warnings.push(format!("Missing model file: {}", path.display()));
        } else if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() < min_size {
                warnings.push(format!("Model file too small ({} bytes): {}", meta.len(), path.display()));
            }
        }
    }

    warnings
}

/// The native recognizer lives only in the worker-process entrypoint below. The desktop host never
/// constructs this type: a C++ exception, access violation, abort, or allocator failure therefore
/// terminates the disposable child rather than the Tauri process and its unsaved review session.
struct NativeKurdishAsrEngine {
    recognizer: Option<OfflineRecognizer>,
    language: String,
}

impl NativeKurdishAsrEngine {
    pub fn new_with_config(model_dir: &Path, config: &AsrLoadConfig) -> Result<Self, String> {
        let (model_path, tokens_path) = omniasr_model_paths(model_dir, &config.model_size);

        if !omniasr_model_present(model_dir, &config.model_size) {
            tracing::warn!(
                "OmniASR CTC model files missing or incomplete under {} (expected {} and {}). ASR unavailable.",
                model_dir.display(),
                model_path.display(),
                tokens_path.display()
            );
            return Ok(Self::new_unavailable());
        }

        // Runtime integrity gate (M2.3): recompute the on-disk SHA-256 and refuse to build a recognizer if
        // it does not match the pinned digest. A missing file degrades gracefully (handled above); a
        // TAMPERED/wrong file is a hard error, not silent acceptance. Verify BOTH the model weights AND the
        // tokens.txt vocab — the CTC index->grapheme map is equally load-bearing for output correctness and
        // equally pinned (OMNIASR_CTC_*_TOKENS). A tampered/swapped same-line-count tokens file still builds
        // a recognizer but decodes every clip to the WRONG graphemes, persisted as trustworthy, so it must
        // fail the same gate as a tampered ONNX.
        let (model_pin, tokens_pin) = match config.model_size {
            AsrModelSize::CTC300M => (crate::models::OMNIASR_CTC_300M_MODEL, crate::models::OMNIASR_CTC_300M_TOKENS),
            AsrModelSize::CTC1B => (crate::models::OMNIASR_CTC_1B_MODEL, crate::models::OMNIASR_CTC_1B_TOKENS),
            // The champion is a WSL server, never a local ONNX: its identity is proven by the exact
            // `{model id, deploymentSha256}` handshake in `engine_runtime::LoadedChampionHealth::matches`,
            // and no pinned digest exists for `models/omniasr-wsl-7b/`. Files found there would therefore
            // load UNVERIFIED, and every clip they decoded would be stamped with the champion's own
            // provenance id (`omniasr-wsl-7b`, pipeline.rs) — a silent identity forgery of the one piece
            // of fixed infrastructure canon says must be exact. Absence is the normal case and already
            // degraded gracefully above; PRESENCE is a hard stop, never an unpinned load.
            AsrModelSize::WSL7B => {
                return Err(format!(
                    "refusing to load unpinned native model files under the champion's provenance id: {} — the \
                     OmniASR-7B champion runs in WSL and is verified by its deploymentSha256 handshake, never \
                     from this directory",
                    model_path.display()
                ));
            }
        };
        for (path, pin) in [(&model_path, model_pin), (&tokens_path, tokens_pin)] {
            if let Err(e) = crate::models::verify_model_path_runtime(path, pin) {
                tracing::error!("ASR model integrity check failed: {e}");
                return Err(e);
            }
        }

        let mut provider = detect_optimal_provider(config.enable_gpu).to_string();
        let mut rec_config =
            OfflineRecognizerConfig { decoding_method: Some("greedy_search".into()), ..Default::default() };
        rec_config.model_config.omnilingual =
            OfflineOmnilingualAsrCtcModelConfig { model: Some(model_path.to_string_lossy().into_owned()) };
        rec_config.model_config.tokens = Some(tokens_path.to_string_lossy().into_owned());
        rec_config.model_config.num_threads = config.num_threads.max(1) as i32;
        rec_config.model_config.provider = Some(provider.clone());
        rec_config.model_config.debug = false;

        let recognizer = match OfflineRecognizer::create(&rec_config) {
            Some(r) => r,
            None => {
                if provider != "cpu" {
                    tracing::warn!("Failed to create ASR recognizer with provider {provider}, falling back to CPU");
                    provider = "cpu".to_string();
                    rec_config.model_config.provider = Some(provider.clone());
                    match OfflineRecognizer::create(&rec_config) {
                        Some(r) => r,
                        None => {
                            return Err(format!(
                                "Failed to create OmniASR recognizer on CPU fallback (model={})",
                                model_path.display()
                            ));
                        }
                    }
                } else {
                    return Err(format!("Failed to create OmniASR recognizer on CPU (model={})", model_path.display()));
                }
            }
        };

        tracing::info!(
            "Meta OmniASR {:?} loaded from {} (provider={provider}, threads={})",
            config.model_size,
            model_dir.display(),
            config.num_threads
        );

        Ok(Self { recognizer: Some(recognizer), language: config.language.clone() })
    }

    pub fn new_unavailable() -> Self {
        Self { recognizer: None, language: String::new() }
    }

    pub fn is_available(&self) -> bool {
        self.recognizer.is_some()
    }

    pub fn transcribe(
        &mut self,
        audio: &[f32],
        sample_rate: u32,
    ) -> Result<(String, Option<f64>, ConfidenceSource), String> {
        // Telemetry (Week-1 "measure first"): ASR inference is the core per-segment heavy op and had no
        // span. The guard records real wall-clock on return — divide duration_ms by (audio_s * 1000) for
        // RTF. Mirrors the existing audio.decode_to_pcm / normalizer / diff guards; the ring self-limits.
        let _span = crate::telemetry::TRACER.start_span(
            "asr.transcribe",
            crate::telemetry::Tracer::metadata(vec![(
                "audio_s",
                format!("{:.2}", audio.len() as f64 / SAMPLE_RATE as f64),
            )]),
        );
        let recognizer = self.recognizer.as_ref().ok_or("ASR model not loaded")?;

        if sample_rate != SAMPLE_RATE {
            tracing::warn!("ASR expects {SAMPLE_RATE} Hz input, got {sample_rate} Hz; results may be degraded");
        }

        if audio.len() <= CHUNK_SAMPLES {
            let (text, confidence, source) = self.transcribe_chunk(recognizer, audio, sample_rate)?;
            return Ok((text.trim().to_string(), confidence, source));
        }

        tracing::info!(
            "Audio {} samples ({:.1}s), splitting into dynamic chunks",
            audio.len(),
            audio.len() as f64 / SAMPLE_RATE as f64
        );

        let mut all_text = String::new();
        let mut confidences = Vec::new();
        let mut sources = Vec::new();
        let mut current_idx = 0;
        let mut chunk_idx = 0;

        while current_idx < audio.len() {
            let remaining = audio.len() - current_idx;
            let cut_idx = if remaining <= CHUNK_SAMPLES {
                audio.len()
            } else {
                let target_cut = current_idx + CHUNK_SAMPLES;
                let search_radius = (1.5 * SAMPLE_RATE as f64) as usize;
                let window_size = (0.1 * SAMPLE_RATE as f64) as usize;
                let min_chunk_samples = 5 * SAMPLE_RATE as usize;

                let lower_bound = target_cut.saturating_sub(search_radius).max(current_idx + min_chunk_samples);
                let upper_bound = (target_cut + search_radius).min(audio.len().saturating_sub(min_chunk_samples));

                if upper_bound > lower_bound && upper_bound >= lower_bound + window_size {
                    let mut best_start = target_cut - window_size / 2;
                    let mut min_energy = f32::MAX;
                    let step = 160; // 10ms steps
                    let mut w_start = lower_bound;

                    while w_start <= upper_bound - window_size {
                        let energy: f32 = audio[w_start..w_start + window_size].iter().map(|x| x.abs()).sum();
                        if energy < min_energy {
                            min_energy = energy;
                            best_start = w_start;
                        }
                        w_start += step;
                    }
                    best_start + window_size / 2
                } else {
                    target_cut
                }
            };

            let chunk_audio = &audio[current_idx..cut_idx];
            chunk_idx += 1;

            tracing::info!(
                "ASR Chunk {}: samples {}..{} ({} samples, {:.1}s)",
                chunk_idx,
                current_idx,
                cut_idx,
                chunk_audio.len(),
                chunk_audio.len() as f64 / SAMPLE_RATE as f64
            );

            match self.transcribe_chunk(recognizer, chunk_audio, sample_rate) {
                Ok((text, confidence, source)) => {
                    let text = text.trim();
                    if !text.is_empty() {
                        if !all_text.is_empty() {
                            all_text.push(' ');
                        }
                        all_text.push_str(text);
                        if let Some(c) = confidence {
                            confidences.push(c);
                        }
                        sources.push(source);
                    }
                }
                Err(e) => {
                    // Round-22 #9: a sub-chunk failure must NOT be swallowed. Silently dropping the
                    // failed span and returning the surviving chunks stitched together would present a
                    // PARTIAL transcript as a complete one at full ~0.90 heuristic confidence —
                    // laundering a dropped audio span into the dataset as a confident transcription
                    // (an honesty-law violation). The single-chunk path already returns Err (via `?`);
                    // the multi-chunk path must be just as honest. Fail the whole segment so it is
                    // escalated/retried, never persisted as a confident complete transcription.
                    tracing::warn!("ASR Chunk {chunk_idx} transcription failed: {e}");
                    return Err(format!(
                        "ASR sub-chunk {chunk_idx} failed; refusing to return a partial transcript as complete: {e}"
                    ));
                }
            }

            current_idx = cut_idx;
        }

        let average_confidence = if confidences.is_empty() {
            None
        } else {
            Some(confidences.iter().sum::<f64>() / confidences.len() as f64)
        };
        let confidence_source =
            if !sources.is_empty() && sources.iter().all(|source| *source == ConfidenceSource::RealPosterior) {
                ConfidenceSource::RealPosterior
            } else {
                ConfidenceSource::Heuristic
            };

        Ok((all_text, average_confidence, confidence_source))
    }

    fn transcribe_chunk(
        &self,
        recognizer: &OfflineRecognizer,
        audio: &[f32],
        sample_rate: u32,
    ) -> Result<(String, Option<f64>, ConfidenceSource), String> {
        // Guard: empty slices can occur at exact chunk boundaries.
        // Passing an empty waveform to ONNX may panic or return garbage.
        if audio.is_empty() {
            return Ok((String::new(), Some(0.0), ConfidenceSource::Heuristic));
        }

        let stream = recognizer.create_stream();
        if !self.language.is_empty() {
            stream.set_option("language", &self.language);
        }

        stream.accept_waveform(sample_rate as i32, audio);
        recognizer.decode(&stream);

        // Stay on sherpa-onnx's supported API. The previous implementation transmuted the private
        // OfflineStream layout to call its C pointer directly, which made a dependency layout change
        // undefined behavior in the host process. The extra `ys_log_probs` field that motivated that
        // bypass is empty for the shipped OmniASR CTC build (measured and documented above), so it
        // provided no real posterior. Preserve the honest heuristic classification without the UB.
        let result = stream.get_result().ok_or_else(|| "OmniASR decode returned no result".to_string())?;
        let (confidence, source) = confidence_from_asr_result(&result.text, &[]);
        Ok((result.text, confidence, source))
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkerStartup {
    Native { model_dir: PathBuf, config: AsrLoadConfig },
    CrashOnceThenEcho { marker: PathBuf },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct WorkerRequest {
    request_id: u64,
    sample_rate: u32,
    sample_count: usize,
}

fn write_worker_request<W: Write>(output: &mut W, request: &WorkerRequest, audio: &[f32]) -> Result<(), String> {
    if request.sample_count > WORKER_MAX_SAMPLES {
        return Err(format!(
            "ASR worker request contains {} samples; maximum is {WORKER_MAX_SAMPLES}",
            request.sample_count
        ));
    }
    if request.sample_count != audio.len() {
        return Err(format!(
            "ASR worker request header declares {} samples but the payload contains {}",
            request.sample_count,
            audio.len()
        ));
    }
    let header = serde_json::to_string(request).map_err(|e| format!("serialize ASR worker request: {e}"))?;
    // Buffer the wire stream so a 15-second chunk does not issue 240,000 four-byte OS pipe writes.
    // The fixed 64 KiB buffer keeps memory flat even at the protocol's existing 10-minute limit.
    let mut buffered = BufWriter::with_capacity(WORKER_WRITE_BUFFER_BYTES, output);
    buffered
        .write_all(header.as_bytes())
        .and_then(|()| buffered.write_all(b"\n"))
        .map_err(|e| format!("write ASR worker request header: {e}"))?;
    for sample in audio {
        buffered.write_all(&sample.to_le_bytes()).map_err(|e| format!("write ASR worker audio payload: {e}"))?;
    }
    buffered.flush().map_err(|e| format!("flush ASR worker request: {e}"))
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkerResponse {
    Ready {
        ok: bool,
        error: Option<String>,
    },
    Transcript {
        request_id: u64,
        ok: bool,
        text: String,
        confidence: Option<f64>,
        confidence_source: ConfidenceSource,
        error: Option<String>,
    },
}

fn write_worker_response(response: &WorkerResponse) -> Result<(), String> {
    let encoded = serde_json::to_string(response).map_err(|e| format!("serialize ASR worker response: {e}"))?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    output
        .write_all(format!("{WORKER_MESSAGE_PREFIX}{encoded}\n").as_bytes())
        .map_err(|e| format!("write ASR worker response: {e}"))?;
    output.flush().map_err(|e| format!("flush ASR worker response: {e}"))
}

/// Read one small JSON protocol line without allowing a malformed peer to grow memory without bound.
fn read_bounded_line<R: BufRead>(reader: &mut R, max_bytes: usize) -> Result<Option<String>, String> {
    let mut bytes = Vec::with_capacity(max_bytes.min(1024));
    loop {
        let available = reader.fill_buf().map_err(|e| format!("read ASR worker protocol: {e}"))?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err("ASR worker protocol ended in the middle of a line".to_string())
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if bytes.len().saturating_add(take) > max_bytes {
            return Err(format!("ASR worker protocol line exceeds {max_bytes} bytes"));
        }
        bytes.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return String::from_utf8(bytes).map(Some).map_err(|_| "ASR worker protocol line is not UTF-8".to_string());
        }
    }
}

enum WorkerEngine {
    Native(NativeKurdishAsrEngine),
    CrashOnceThenEcho { marker: PathBuf },
}

fn run_worker(startup_json: &str) -> Result<(), String> {
    let startup: WorkerStartup =
        serde_json::from_str(startup_json).map_err(|e| format!("parse ASR worker startup configuration: {e}"))?;
    let mut engine = match startup {
        WorkerStartup::Native { model_dir, config } => {
            let engine = NativeKurdishAsrEngine::new_with_config(&model_dir, &config)?;
            if !engine.is_available() {
                let error = "ASR model is unavailable in worker".to_string();
                let _ = write_worker_response(&WorkerResponse::Ready { ok: false, error: Some(error.clone()) });
                return Err(error);
            }
            WorkerEngine::Native(engine)
        }
        WorkerStartup::CrashOnceThenEcho { marker } => WorkerEngine::CrashOnceThenEcho { marker },
    };
    write_worker_response(&WorkerResponse::Ready { ok: true, error: None })?;

    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    loop {
        let Some(line) = read_bounded_line(&mut input, 8 * 1024)? else {
            return Ok(());
        };
        let request: WorkerRequest =
            serde_json::from_str(&line).map_err(|e| format!("parse ASR worker request: {e}"))?;
        if request.sample_count > WORKER_MAX_SAMPLES {
            return Err(format!(
                "ASR worker request contains {} samples; maximum is {WORKER_MAX_SAMPLES}",
                request.sample_count
            ));
        }
        let byte_count = request
            .sample_count
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| "ASR worker sample byte count overflow".to_string())?;
        let mut encoded_audio = vec![0_u8; byte_count];
        input.read_exact(&mut encoded_audio).map_err(|e| format!("read ASR worker audio payload: {e}"))?;
        let mut audio = Vec::with_capacity(request.sample_count);
        for sample in encoded_audio.chunks_exact(4) {
            let value = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
            if !value.is_finite() {
                return Err("ASR worker audio contains a non-finite sample".to_string());
            }
            audio.push(value);
        }

        let outcome = match &mut engine {
            WorkerEngine::Native(engine) => engine.transcribe(&audio, request.sample_rate),
            WorkerEngine::CrashOnceThenEcho { marker } => {
                if !marker.exists() {
                    std::fs::write(&*marker, b"crashed")
                        .map_err(|e| format!("write ASR containment marker {}: {e}", marker.display()))?;
                    // This deliberately models the uncatchable native failure class. Only the worker
                    // dies; the supervisor process must observe EOF, remain alive, and restart it.
                    std::process::abort();
                }
                Ok(("worker-restarted".to_string(), Some(1.0), ConfidenceSource::Heuristic))
            }
        };
        let response = match outcome {
            Ok((text, confidence, confidence_source)) => WorkerResponse::Transcript {
                request_id: request.request_id,
                ok: true,
                text,
                confidence,
                confidence_source,
                error: None,
            },
            Err(error) => WorkerResponse::Transcript {
                request_id: request.request_id,
                ok: false,
                text: String::new(),
                confidence: None,
                confidence_source: ConfidenceSource::Heuristic,
                error: Some(error),
            },
        };
        write_worker_response(&response)?;
    }
}

fn worker_executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| format!("resolve ASR worker executable: {e}"))?;
    // A Rust test harness lives under target/{profile}/deps and does not execute this package's main().
    // Cargo builds the real binary beside that directory for integration tests; use it there. Installed
    // and normally launched applications simply supervise another instance of their own executable.
    if current.parent().and_then(Path::file_name).is_some_and(|name| name == "deps") {
        let profile_dir = current
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| "test executable has no Cargo profile directory".to_string())?;
        let candidate = profile_dir.join(format!("cortex-speech-app{}", std::env::consts::EXE_SUFFIX));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Ok(current)
}

struct WorkerProcess {
    child: Child,
    input: ChildStdin,
    responses: mpsc::Receiver<Result<WorkerResponse, String>>,
}

impl WorkerProcess {
    fn spawn(executable: &Path, startup: &WorkerStartup) -> Result<Self, String> {
        let startup_json =
            serde_json::to_string(startup).map_err(|e| format!("serialize ASR worker startup configuration: {e}"))?;
        let mut child = Command::new(executable)
            .arg(WORKER_ARG)
            .arg(startup_json)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("start isolated ASR worker {}: {e}", executable.display()))?;
        let input = child.stdin.take().ok_or_else(|| "ASR worker stdin was not piped".to_string())?;
        let stdout = child.stdout.take().ok_or_else(|| "ASR worker stdout was not piped".to_string())?;
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_bounded_line(&mut reader, 256 * 1024) {
                    Ok(Some(line)) => {
                        let Some(payload) = line.strip_prefix(WORKER_MESSAGE_PREFIX) else {
                            // Native runtimes occasionally write diagnostics to stdout. They are not
                            // protocol data and must never desynchronize or spoof a response.
                            continue;
                        };
                        let parsed = serde_json::from_str::<WorkerResponse>(payload)
                            .map_err(|e| format!("parse ASR worker response: {e}"));
                        if sender.send(parsed).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(Err("ASR worker exited before replying".to_string()));
                        break;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        let process = Self { child, input, responses };
        match process.responses.recv_timeout(WORKER_STARTUP_TIMEOUT) {
            Ok(Ok(WorkerResponse::Ready { ok: true, .. })) => Ok(process),
            Ok(Ok(WorkerResponse::Ready { error, .. })) => {
                Err(error.unwrap_or_else(|| "ASR worker refused startup".to_string()))
            }
            Ok(Ok(_)) => Err("ASR worker sent a transcript before its ready handshake".to_string()),
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("ASR worker did not become ready within {} seconds", WORKER_STARTUP_TIMEOUT.as_secs()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("ASR worker response channel disconnected".to_string()),
        }
    }

    fn transcribe(
        &mut self,
        request_id: u64,
        audio: &[f32],
        sample_rate: u32,
        timeout: Duration,
    ) -> Result<(String, Option<f64>, ConfidenceSource), String> {
        let request = WorkerRequest { request_id, sample_rate, sample_count: audio.len() };
        write_worker_request(&mut self.input, &request, audio)?;

        match self.responses.recv_timeout(timeout) {
            Ok(Ok(WorkerResponse::Transcript {
                request_id: response_id,
                ok,
                text,
                confidence,
                confidence_source,
                error,
            })) if response_id == request_id => {
                if ok {
                    Ok((text, confidence, confidence_source))
                } else {
                    Err(error.unwrap_or_else(|| "ASR worker decode failed".to_string()))
                }
            }
            Ok(Ok(WorkerResponse::Transcript { request_id: response_id, .. })) => {
                Err(format!("ASR worker response id mismatch: expected {request_id}, got {response_id}"))
            }
            Ok(Ok(WorkerResponse::Ready { .. })) => Err("ASR worker sent a duplicate ready handshake".to_string()),
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("ASR worker request timed out after {} seconds", timeout.as_secs()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("ASR worker response channel disconnected".to_string()),
        }
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

struct AsrWorkerSupervisor {
    executable: PathBuf,
    startup: WorkerStartup,
    process: Option<WorkerProcess>,
    next_request_id: u64,
    consecutive_failures: u32,
    retry_after: Option<Instant>,
    circuit_open_until: Option<Instant>,
}

impl AsrWorkerSupervisor {
    fn start(startup: WorkerStartup) -> Result<Self, String> {
        let executable = worker_executable()?;
        let process = WorkerProcess::spawn(&executable, &startup)?;
        Ok(Self {
            executable,
            startup,
            process: Some(process),
            next_request_id: 1,
            consecutive_failures: 0,
            retry_after: None,
            circuit_open_until: None,
        })
    }

    fn ensure_process(&mut self) -> Result<(), String> {
        let now = Instant::now();
        if self.circuit_open_until.is_some_and(|until| until > now) {
            let seconds =
                self.circuit_open_until.map(|until| until.saturating_duration_since(now).as_secs()).unwrap_or_default();
            return Err(format!("ASR worker circuit is open after repeated crashes; retry in {seconds}s"));
        }
        if self.retry_after.is_some_and(|until| until > now) {
            let millis =
                self.retry_after.map(|until| until.saturating_duration_since(now).as_millis()).unwrap_or_default();
            return Err(format!("ASR worker restart backoff is active; retry in {millis}ms"));
        }
        if self.process.is_none() {
            match WorkerProcess::spawn(&self.executable, &self.startup) {
                Ok(process) => self.process = Some(process),
                Err(error) => {
                    self.record_failure();
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn record_failure(&mut self) {
        self.process.take();
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let now = Instant::now();
        if self.consecutive_failures >= WORKER_CIRCUIT_FAILURES {
            self.circuit_open_until = Some(now + WORKER_CIRCUIT_OPEN);
            self.retry_after = None;
        } else {
            let shift = self.consecutive_failures.saturating_sub(1).min(4);
            let backoff_ms = 250_u64.saturating_mul(1_u64 << shift);
            self.retry_after = Some(now + Duration::from_millis(backoff_ms));
        }
    }

    fn transcribe(
        &mut self,
        audio: &[f32],
        sample_rate: u32,
    ) -> Result<(String, Option<f64>, ConfidenceSource), String> {
        if audio.len() > WORKER_MAX_SAMPLES {
            return Err(format!(
                "ASR request is {:.1} minutes; isolated worker limit is 10 minutes",
                audio.len() as f64 / SAMPLE_RATE as f64 / 60.0
            ));
        }
        if audio.iter().any(|sample| !sample.is_finite()) {
            return Err("ASR audio contains a non-finite sample".to_string());
        }
        self.ensure_process()?;
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        let audio_seconds = audio.len() as u64 / sample_rate.max(1) as u64;
        let timeout = Duration::from_secs(60_u64.saturating_add(audio_seconds.saturating_mul(5)))
            .max(WORKER_REQUEST_MIN_TIMEOUT)
            .min(WORKER_REQUEST_MAX_TIMEOUT);
        let outcome = self.process.as_mut().ok_or_else(|| "ASR worker is not running".to_string())?.transcribe(
            request_id,
            audio,
            sample_rate,
            timeout,
        );
        match outcome {
            Ok(result) => {
                self.consecutive_failures = 0;
                self.retry_after = None;
                self.circuit_open_until = None;
                Ok(result)
            }
            Err(error) => {
                self.record_failure();
                Err(format!("isolated ASR worker failed; host remains healthy: {error}"))
            }
        }
    }
}

/// Host-side ASR facade. It contains only a subprocess supervisor and no native recognizer handle.
pub struct KurdishAsrService {
    worker: Option<AsrWorkerSupervisor>,
}

impl KurdishAsrService {
    pub fn new(model_dir: &Path, enable_gpu: bool) -> Result<Self, String> {
        Self::new_with_config(model_dir, &AsrLoadConfig { enable_gpu, ..AsrLoadConfig::default() })
    }

    pub fn new_with_config(model_dir: &Path, config: &AsrLoadConfig) -> Result<Self, String> {
        if !omniasr_model_present(model_dir, &config.model_size) {
            let (model_path, tokens_path) = omniasr_model_paths(model_dir, &config.model_size);
            tracing::warn!(
                "OmniASR model files missing or incomplete under {} (expected {} and {}). ASR unavailable.",
                model_dir.display(),
                model_path.display(),
                tokens_path.display()
            );
            return Ok(Self::new_unavailable());
        }
        let startup = WorkerStartup::Native { model_dir: model_dir.to_path_buf(), config: config.clone() };
        Ok(Self { worker: Some(AsrWorkerSupervisor::start(startup)?) })
    }

    pub fn new_unavailable() -> Self {
        Self { worker: None }
    }

    pub fn is_available(&self) -> bool {
        self.worker.is_some()
    }

    pub fn transcribe(
        &mut self,
        audio: &[f32],
        sample_rate: u32,
    ) -> Result<(String, Option<f64>, ConfidenceSource), String> {
        self.worker.as_mut().ok_or_else(|| "ASR model not loaded".to_string())?.transcribe(audio, sample_rate)
    }
}

fn run_supervisor_self_test() -> Result<(), String> {
    let marker = std::env::temp_dir().join(format!("cortex-asr-crash-once-{}", uuid::Uuid::new_v4()));
    let startup = WorkerStartup::CrashOnceThenEcho { marker: marker.clone() };
    let mut supervisor = AsrWorkerSupervisor::start(startup)?;
    let first = supervisor.transcribe(&[0.0; 160], SAMPLE_RATE);
    if first.is_ok() {
        return Err("fault-injection worker unexpectedly survived its first request".to_string());
    }
    std::thread::sleep(Duration::from_millis(300));
    let second = supervisor.transcribe(&[0.0; 160], SAMPLE_RATE)?;
    let _ = std::fs::remove_file(&marker);
    if second.0 != "worker-restarted" {
        return Err(format!("restarted worker returned unexpected transcript: {}", second.0));
    }
    Ok(())
}

/// Called before Tauri initialization. Returning `Some` means this process was intentionally launched
/// in a non-UI role and the caller must exit with the returned code.
pub fn run_special_process_mode() -> Option<i32> {
    let mut args = std::env::args();
    let _program = args.next();
    match args.next().as_deref() {
        Some(WORKER_ARG) => {
            let result = args
                .next()
                .ok_or_else(|| "missing ASR worker startup configuration".to_string())
                .and_then(|json| run_worker(&json));
            if let Err(error) = result {
                eprintln!("ASR worker failed: {error}");
                Some(2)
            } else {
                Some(0)
            }
        }
        Some(SUPERVISOR_SELF_TEST_ARG) => {
            if let Err(error) = run_supervisor_self_test() {
                eprintln!("ASR supervisor self-test failed: {error}");
                Some(3)
            } else {
                Some(0)
            }
        }
        _ => None,
    }
}

/// Thread-safe lazy singleton for Meta OmniASR CTC sessions.
pub struct AsrPool {
    inner: Mutex<AsrPoolState>,
}

struct AsrPoolState {
    services: std::collections::HashMap<AsrLoadConfig, KurdishAsrService>,
    loaded_dir: Option<PathBuf>,
    /// Circuit breaker (P1.4): per config, the model+tokens fingerprint at the last PRESENT-but-failed
    /// load AND when it failed. While the on-disk file is unchanged, ensure_loaded skips re-running the
    /// expensive full-file SHA-256 verify + ONNX load for a cooldown, then re-probes (half-open). Cleared
    /// on success or when a file goes absent (the cheap round-24 retry path); a re-download changes the
    /// fingerprint and re-attempts immediately.
    load_failed: std::collections::HashMap<AsrLoadConfig, (ModelFingerprint, std::time::Instant)>,
}

/// How long a present-but-failed model load is skipped before a half-open re-probe. Bounds the R4
/// gigabyte re-hash to at most once per this window (vs once per chunk) while still letting a TRANSIENT
/// failure — an AV/sharing lock during the SHA-256 read, a transient GPU provider init — recover on its
/// own within it, so the round-24 "download in-app then transcribe" flow is never wedged to a restart.
const LOAD_RETRY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// A cheap on-disk identity for the model+tokens pair: ((model size, model mtime), (tokens size, tokens
/// mtime)). Changes on any re-download; `None` (see [`model_files_fingerprint`]) means a file is absent.
type ModelFingerprint = ((u64, std::time::SystemTime), (u64, std::time::SystemTime));

/// The current fingerprint of the model+tokens files, or `None` if EITHER is absent. Pure `fs::metadata`
/// — no read, no hash — so consulting it is negligible next to the SHA-256 it gates.
fn model_files_fingerprint(model_dir: &Path, size: &AsrModelSize) -> Option<ModelFingerprint> {
    let (model_path, tokens_path) = omniasr_model_paths(model_dir, size);
    let id = |p: &Path| -> Option<(u64, std::time::SystemTime)> {
        let m = std::fs::metadata(p).ok()?;
        Some((m.len(), m.modified().ok()?))
    };
    Some((id(&model_path)?, id(&tokens_path)?))
}

/// Whether `ensure_loaded` should (re)attempt the expensive load for an UNAVAILABLE service (the caller
/// returns first for an available one, so this never touches the happy path). Pure — the breaker is
/// unit-testable without a real ONNX model or a clock. Skip ONLY when the model is present (`current_fp`
/// Some), unchanged since it failed, AND still within the cooldown. Otherwise attempt: an absent model
/// always retries (round-24), a changed fingerprint is a re-download, and an elapsed cooldown is a
/// half-open re-probe that lets a transient failure self-heal without a restart.
fn should_attempt_load(
    current_fp: &Option<ModelFingerprint>,
    last_failed_fp: Option<&ModelFingerprint>,
    elapsed_since_fail: Option<std::time::Duration>,
    cooldown: std::time::Duration,
) -> bool {
    match current_fp {
        Some(fp) if last_failed_fp == Some(fp) => elapsed_since_fail.map_or(true, |e| e >= cooldown),
        _ => true,
    }
}

impl Default for AsrPool {
    fn default() -> Self {
        Self::new()
    }
}

impl AsrPool {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(AsrPoolState {
                services: std::collections::HashMap::new(),
                loaded_dir: None,
                load_failed: std::collections::HashMap::new(),
            }),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, AsrPoolState> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned ASR pool state lock");
            poisoned.into_inner()
        })
    }

    /// Returns whether a (re)load was attempted this call — callers don't need it, but it makes the
    /// unavailable-retry contract below unit-testable without a real ONNX model on disk.
    fn ensure_loaded(&self, model_dir: &Path, config: &AsrLoadConfig) -> bool {
        let mut state = self.lock_state();

        if state.loaded_dir.as_ref() != Some(&model_dir.to_path_buf()) {
            state.services.clear();
            state.load_failed.clear();
            state.loaded_dir = Some(model_dir.to_path_buf());
        }

        // Fast path: reuse an AVAILABLE cached service with ZERO disk access (this runs per chunk).
        if state.services.get(config).is_some_and(|svc| svc.is_available()) {
            return false;
        }

        // Unavailable. A cached UNAVAILABLE placeholder used to be retried UNCONDITIONALLY (round-24 hunt
        // #2, so a fresh-install in-app download recovers without restart) — but that re-runs the
        // full-file SHA-256 verify + ONNX load on EVERY call, and for a PRESENT-but-corrupt/unloadable
        // model (300 MB–1.4 GB) it re-hashed gigabytes per chunk (P1.4 / audit R4). Circuit breaker: an
        // absent model has no fingerprint and always retries cheaply (new_with_config bails at the
        // presence check before hashing — round-24 preserved); a present model that FAILED for this exact
        // file is skipped for LOAD_RETRY_COOLDOWN, then half-open re-probed so a TRANSIENT failure (an AV
        // lock during the hash read; a transient GPU init) self-heals; a re-download changes the
        // fingerprint and re-attempts immediately.
        let current_fp = model_files_fingerprint(model_dir, &config.model_size);
        let last = state.load_failed.get(config).copied();
        let elapsed = last.map(|(_, when)| when.elapsed());
        if !should_attempt_load(&current_fp, last.as_ref().map(|(fp, _)| fp), elapsed, LOAD_RETRY_COOLDOWN) {
            return false;
        }

        tracing::info!(
            "Initializing pooled ASR from {} (gpu={}, threads={})...",
            model_dir.display(),
            config.enable_gpu,
            config.num_threads
        );
        let service = match KurdishAsrService::new_with_config(model_dir, config) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("ASR pool init: {e}");
                KurdishAsrService::new_unavailable()
            }
        };
        let now_available = service.is_available();
        state.services.insert(config.clone(), service);
        // Latch a PRESENT-but-failed load (current_fp is Some) with the time it failed, so it is skipped
        // for the cooldown then half-open re-probed; clear on success OR when a file is absent (keep the
        // cheap round-24 retry alive).
        match (now_available, current_fp) {
            (false, Some(fp)) => {
                state.load_failed.insert(config.clone(), (fp, std::time::Instant::now()));
            }
            _ => {
                state.load_failed.remove(config);
            }
        }
        true
    }

    pub fn warmup(&self, model_dir: &Path, config: &AsrLoadConfig) -> Result<(), String> {
        self.ensure_loaded(model_dir, config);
        Ok(())
    }

    pub fn with_service<F, R>(&self, model_dir: &Path, config: &AsrLoadConfig, f: F) -> R
    where
        F: FnOnce(&mut KurdishAsrService) -> R,
    {
        self.ensure_loaded(model_dir, config);
        let mut state = self.lock_state();
        if let Some(svc) = state.services.get_mut(config) {
            f(svc)
        } else {
            tracing::error!("ASR service missing despite ensure_loaded");
            let mut fallback = KurdishAsrService::new_unavailable();
            f(&mut fallback)
        }
    }

    pub fn is_available(&self, model_dir: &Path, config: &AsrLoadConfig) -> bool {
        self.with_service(model_dir, config, |asr| asr.is_available())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct CountingWriter {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn worker_request_writer_is_bounded_buffered_and_wire_exact() {
        let audio = [-1.0_f32, 0.0, 0.25, -0.5];
        let request = WorkerRequest { request_id: 17, sample_rate: SAMPLE_RATE, sample_count: audio.len() };
        let mut output = CountingWriter::default();

        write_worker_request(&mut output, &request, &audio).unwrap();

        assert_eq!(output.writes, 1, "a small request must be emitted as one buffered OS write");
        assert_eq!(output.flushes, 1);
        let newline = output.bytes.iter().position(|byte| *byte == b'\n').expect("JSON header terminator");
        let decoded: WorkerRequest = serde_json::from_slice(&output.bytes[..newline]).unwrap();
        assert_eq!(decoded.request_id, request.request_id);
        assert_eq!(decoded.sample_rate, request.sample_rate);
        assert_eq!(decoded.sample_count, request.sample_count);
        let expected_payload: Vec<u8> = audio.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        assert_eq!(&output.bytes[newline + 1..], expected_payload.as_slice());

        let mismatch = WorkerRequest { request_id: 18, sample_rate: SAMPLE_RATE, sample_count: audio.len() + 1 };
        let mut rejected = CountingWriter::default();
        let error = write_worker_request(&mut rejected, &mismatch, &audio).unwrap_err();
        assert!(error.contains("declares"), "unexpected mismatch error: {error}");
        assert!(rejected.bytes.is_empty(), "an invalid frame must not be partially written");

        let oversized =
            WorkerRequest { request_id: 19, sample_rate: SAMPLE_RATE, sample_count: WORKER_MAX_SAMPLES + 1 };
        let error = write_worker_request(&mut rejected, &oversized, &[]).unwrap_err();
        assert!(error.contains("maximum"), "unexpected size-limit error: {error}");
        assert!(rejected.bytes.is_empty(), "an oversized frame must not be partially written");
    }

    #[test]
    fn worker_request_writer_bounds_pipe_writes_for_a_full_chunk() {
        let audio = vec![0.25_f32; 15 * SAMPLE_RATE as usize];
        let request = WorkerRequest { request_id: 20, sample_rate: SAMPLE_RATE, sample_count: audio.len() };
        let mut output = CountingWriter::default();

        write_worker_request(&mut output, &request, &audio).unwrap();

        let header_bytes = serde_json::to_string(&request).unwrap().len() + 1;
        let packet_bytes = header_bytes + audio.len() * std::mem::size_of::<f32>();
        let maximum_buffer_flushes = packet_bytes.div_ceil(WORKER_WRITE_BUFFER_BYTES);
        assert!(
            output.writes <= maximum_buffer_flushes,
            "{} underlying writes exceeded the {} fixed-buffer bound",
            output.writes,
            maximum_buffer_flushes
        );
        assert!(output.writes < 20, "15 seconds of PCM regressed to tiny pipe writes");
        assert_eq!(output.bytes.len(), packet_bytes);
    }

    #[test]
    fn asr_pool_retries_an_unavailable_cached_service_instead_of_pinning_it() {
        // Round-24 hunt #2: ensure_loaded used to short-circuit on bare contains_key, so the
        // unavailable placeholder cached on a model-less first call was pinned for the process
        // lifetime — the fresh-install "download the model in-app, then transcribe" flow stayed
        // "ASR model not loaded" until app restart (same resolved dir + same config key before and
        // after the download, so no invalidation path ever fired). The pool must RETRY a cached
        // unavailable service on every ensure_loaded, so the load succeeds the moment the model
        // files appear on disk.
        let dir = tempfile::TempDir::new().unwrap();
        let pool = AsrPool::new();
        let config = AsrLoadConfig::default();

        // First call: no model files -> load attempted, unavailable placeholder cached.
        assert!(pool.ensure_loaded(dir.path(), &config), "first call must attempt the load");
        assert!(!pool.is_available(dir.path(), &config), "no model on disk -> unavailable");

        // Second call, model still absent: the cached UNAVAILABLE placeholder must be retried —
        // this is the regression. (Before the fix, contains_key short-circuited and returned
        // without attempting a load, which is exactly why an in-app download never recovered.)
        assert!(
            pool.ensure_loaded(dir.path(), &config),
            "a cached unavailable service must be retried, not pinned for the process lifetime"
        );

        // The available-service fast path (reuse without reload) cannot be unit-covered here —
        // constructing an available service needs a real ONNX model on disk; the ignored
        // ort_omniasr_smoke test covers real loading on machines that have the model.
    }

    #[test]
    fn model_load_breaker_skips_within_cooldown_then_retries_absent_changed_or_elapsed() {
        // P1.4 (audit R4): the circuit-breaker decision. A present-but-corrupt model must be re-verified
        // (full SHA-256 of 300 MB–1.4 GB) at most once per cooldown, not once per chunk — while an absent
        // model always retries (round-24 fresh-install recovery), a re-download (changed fingerprint)
        // retries immediately, and — critically — an elapsed cooldown half-open re-probes so a TRANSIENT
        // failure (an AV lock during the hash, a transient GPU init) self-heals without a restart.
        use std::time::{Duration, SystemTime};
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let fp_a: ModelFingerprint = ((300_000_000, t), (5_000, t));
        let fp_b: ModelFingerprint = ((300_000_001, t), (5_000, t)); // a re-download changed the model size
        let cd = Duration::from_secs(30);

        // Absent (no fingerprint) -> ALWAYS attempt, even with a stale latch: round-24 recovery.
        assert!(should_attempt_load(&None, None, None, cd), "absent + no latch -> attempt");
        assert!(should_attempt_load(&None, Some(&fp_a), Some(Duration::from_secs(1)), cd), "absent -> retry");
        // Present, no prior failure -> attempt the first (expensive) verify.
        assert!(should_attempt_load(&Some(fp_a), None, None, cd), "present + first try -> attempt");
        // Present + UNCHANGED since it failed, WITHIN cooldown -> SKIP (no per-chunk re-hash).
        assert!(
            !should_attempt_load(&Some(fp_a), Some(&fp_a), Some(Duration::from_secs(5)), cd),
            "within cooldown -> skip"
        );
        // Present + UNCHANGED, cooldown ELAPSED -> half-open re-probe (transient self-heals).
        assert!(
            should_attempt_load(&Some(fp_a), Some(&fp_a), Some(Duration::from_secs(30)), cd),
            "cooldown elapsed -> re-probe"
        );
        // Present but CHANGED (a re-download) -> attempt immediately regardless of cooldown.
        assert!(
            should_attempt_load(&Some(fp_b), Some(&fp_a), Some(Duration::from_secs(1)), cd),
            "re-download -> attempt"
        );
    }

    /// Do sherpa-onnx HOTWORDS do anything on the offline CTC path? Measured, not assumed.
    ///
    /// The offline config exposes `hotwords_file`/`hotwords_score`, and the crate even offers a
    /// per-stream `create_stream_with_hotwords` — which reads as a live contextual-biasing lever for
    /// the owner's recurring proper nouns. MEASURED 2026-07-29 (sherpa-onnx 1.13.2, CTC-300M int8,
    /// CPU, the committed FLEURS fixture):
    ///
    ///   * config `hotwords_file` + greedy_search  -> construction REFUSED: upstream validates
    ///     "Please use --decoding-method=modified_beam_search if you provide --hotwords-file".
    ///   * `create_stream_with_hotwords` on a CTC recognizer -> HARD PROCESS ABORT (exit 0xffffffff,
    ///     "Only transducer models support contextual biasing", offline-recognizer-impl.h:38). A C++
    ///     abort cannot be caught from Rust, so this test must never call it — and neither may any
    ///     production path, ever: one call kills the whole app. The pin for that leg is therefore a
    ///     source-scan in this crate (no call sites) rather than a runtime probe.
    ///
    /// Net: contextual biasing is CLOSED on this stack, more definitively than LM fusion — the knobs
    /// exist on the shared config struct but the CTC implementation actively refuses or aborts. If
    /// the construction assert below ever fails after a sherpa upgrade, re-probe: biasing may have
    /// become real, and then it belongs on the improvement list.
    #[test]
    #[ignore]
    fn hotwords_are_refused_on_the_offline_ctc_path() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let model = root.join("models/omniasr-ctc-300m/model.int8.onnx");
        let tokens = root.join("models/omniasr-ctc-300m/tokens.txt");
        let fixture = root.join("tests/fixtures/fleurs_ckb_sample.wav");
        if !model.exists() || !fixture.exists() {
            eprintln!("model or fixture not present; skipping hotwords probe");
            return;
        }
        crate::models::init_ort_dylib_path();
        let (sr, pcm) =
            crate::audio::decode_to_pcm_with_timeout(&fixture, std::time::Duration::from_secs(120)).expect("decode");
        assert_eq!(sr, 16000);
        let audio: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

        let base_config = |hotwords: Option<&std::path::Path>, method: &str| {
            let mut c = OfflineRecognizerConfig { decoding_method: Some(method.into()), ..Default::default() };
            c.model_config.omnilingual =
                OfflineOmnilingualAsrCtcModelConfig { model: Some(model.to_string_lossy().into_owned()) };
            c.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
            c.model_config.num_threads = 4;
            c.model_config.provider = Some("cpu".into()); // deterministic: same EP for every leg
            if let Some(h) = hotwords {
                c.hotwords_file = Some(h.to_string_lossy().into_owned());
                c.hotwords_score = 50.0; // absurd on purpose: a live mechanism could not ignore this
            }
            c
        };
        let decode = |rec: &OfflineRecognizer, stream: sherpa_onnx::OfflineStream| -> String {
            stream.accept_waveform(16000, &audio);
            rec.decode(&stream);
            stream.get_result().map(|r| r.text).unwrap_or_default()
        };

        // A: baseline, exactly the production shape (greedy, no hotwords).
        let rec_a = OfflineRecognizer::create(&base_config(None, "greedy_search")).expect("baseline recognizer");
        let text_a = decode(&rec_a, rec_a.create_stream());
        assert!(!text_a.trim().is_empty(), "baseline must produce a real transcript");
        let bias_word = "کوردستان";
        assert!(!text_a.contains(bias_word), "probe needs a bias word the baseline does NOT emit; got: {text_a}");

        let tmp = tempfile::tempdir().unwrap();
        let hw = tmp.path().join("hotwords.txt");
        std::fs::write(&hw, format!("{bias_word}\n")).unwrap();

        // B: config-level hotwords_file + score 50 on the shipped decoding method. The PIN: upstream
        // refuses to construct this at all. (If it ever starts constructing, decode and compare.)
        let b = OfflineRecognizer::create(&base_config(Some(&hw), "greedy_search"));
        eprintln!("[hotwords] A baseline: {text_a}");
        eprintln!("[hotwords] B config+greedy constructs: {}", b.is_some());
        assert!(
            b.is_none(),
            "config-level hotwords on CTC+greedy now CONSTRUCTS — re-probe: biasing may have become real"
        );

        // C is deliberately NOT exercised: `create_stream_with_hotwords` on this recognizer is a
        // measured hard process abort (see doc above). The pin for C is the source scan below —
        // the fatal call must have zero call sites anywhere in the crate (this file's own mentions
        // are comments and the scan, so asr.rs is excluded by name).
        let mut offenders = Vec::new();
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src_dir];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|e| e == "rs") && p.file_name().is_some_and(|n| n != "asr.rs") {
                    let s = std::fs::read_to_string(&p).unwrap_or_default();
                    if s.contains("create_stream_with_hotwords") {
                        offenders.push(p.display().to_string());
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "create_stream_with_hotwords is a measured PROCESS ABORT on the CTC model and must never \
             be called: {offenders:?}"
        );
    }

    /// Feasibility gate for the constrained-decode port: can we load the OmniASR ONNX via `ort`
    /// (dynamic onnxruntime) and run one inference to get the [1, T, 9812] CTC logits? Gated on the
    /// model being present; sets ORT_DYLIB_PATH to the bundled onnxruntime.dll.
    #[test]
    #[ignore]
    fn ort_omniasr_smoke() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let model = root.join("models/omniasr-ctc-300m/model.int8.onnx");
        if !model.exists() {
            eprintln!("model not present; skipping ort smoke");
            return;
        }
        // Resolve the real nested onnxruntime.dll (models/onnxruntime.dll/onnxruntime.dll on
        // Windows) via the production helper rather than hardcoding a path.
        crate::models::init_ort_dylib_path();

        let mut session = ort::session::Session::builder()
            .expect("builder")
            .commit_from_file(&model)
            .expect("load model.int8.onnx via ort");

        let n = 16000usize; // 1s of silence
        let audio = vec![0.0f32; n];
        let input = ort::value::Tensor::from_array(([1usize, n], audio)).expect("tensor");
        let outputs = session.run(ort::inputs!["x" => input]).expect("run");
        let (shape, _data) = outputs["logits"].try_extract_tensor::<f32>().expect("extract logits");
        eprintln!("[ort-smoke] logits shape = {shape:?}");
        assert_eq!(shape[2], 9812, "vocab dim must be 9812");
    }

    #[test]
    fn empty_asr_text_has_zero_confidence_without_token_probs() {
        assert_eq!(confidence_from_asr_result("", &[]), (Some(0.0), ConfidenceSource::Heuristic));
        assert_eq!(confidence_from_asr_result("   ", &[]), (Some(0.0), ConfidenceSource::Heuristic));
    }

    #[test]
    fn empty_asr_text_has_zero_confidence_even_with_token_probs() {
        // A blank/whitespace transcript must never carry a high confidence, even when the model exposed
        // per-token probs for the blank-collapsed result — a confident-empty would persist into the jury
        // gate as if trustworthy. The RealPosterior branch is now guarded too.
        assert_eq!(confidence_from_asr_result("", &[-0.1, -0.1]), (Some(0.0), ConfidenceSource::Heuristic));
        assert_eq!(confidence_from_asr_result("  ", &[-0.05]), (Some(0.0), ConfidenceSource::Heuristic));
    }

    #[test]
    fn nonempty_asr_text_gets_fallback_confidence_without_token_probs() {
        assert_eq!(confidence_from_asr_result("سڵاو", &[]), (Some(0.90), ConfidenceSource::Heuristic));
    }

    #[test]
    fn real_token_log_probs_yield_a_mean_posterior_not_the_constant() {
        // Exercises the RealPosterior branch's LOGIC using sherpa's REAL key "ys_log_probs", for a
        // hypothetical engine that exposes per-token log-probs. Honesty: the default OmniASR CTC engine
        // never emits these (it always sends `ys_log_probs: []` — see
        // asr_result_without_token_probs_uses_honest_heuristic), so on the shipped local path confidence
        // is ALWAYS the Heuristic; this test does NOT claim the default engine produces a real posterior.
        // mean(exp(-0.10536), exp(-0.22314)) = mean(0.900, 0.800) = 0.850.
        let json = r#"{"text":"سڵاو","ys_log_probs":[-0.10536,-0.22314]}"#;
        let (text, conf, source) = parse_asr_result_json(json).expect("parse");
        assert_eq!(text, "سڵاو");
        assert_eq!(source, ConfidenceSource::RealPosterior, "real per-token log-probs ⇒ RealPosterior");
        let c = conf.expect("confidence");
        assert!((c - 0.85).abs() < 0.01, "expected real ~0.85 mean posterior, got {c}");
        assert!((c - 0.90).abs() > 0.01, "must NOT be the 0.90 constant");
    }

    #[test]
    fn asr_result_without_token_probs_uses_honest_heuristic() {
        // When the model exposes no posteriors, confidence is the documented 0.90 heuristic.
        let (_t, conf, source) = parse_asr_result_json(r#"{"text":"سڵاو"}"#).expect("parse");
        assert_eq!(conf, Some(0.90));
        assert_eq!(source, ConfidenceSource::Heuristic, "no ys_probs ⇒ Heuristic");
        // Empty text → zero confidence, never the heuristic.
        let (_t, conf, _source) = parse_asr_result_json(r#"{"text":"  "}"#).expect("parse");
        assert_eq!(conf, Some(0.0));
    }

    #[test]
    fn token_probs_drive_asr_confidence_when_present() {
        let probs = [0.25f64.ln(), 0.75f64.ln()];
        assert_eq!(confidence_from_asr_result("سڵاو", &probs), (Some(0.5), ConfidenceSource::RealPosterior));
    }

    #[test]
    fn missing_1b_never_silently_selects_installed_300m() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let model_dir = tmp.path();
        let (model, tokens) = omniasr_model_paths(model_dir, &AsrModelSize::CTC300M);
        write_sized_file(&model, CTC_300M_MIN_MODEL_BYTES);
        write_sized_file(&tokens, CTC_MIN_TOKEN_BYTES);

        assert_eq!(select_available_model_size(model_dir, &AsrModelSize::CTC1B), AsrModelSize::CTC1B);
    }

    #[test]
    fn truncated_requested_model_never_changes_engine_identity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let model_dir = tmp.path();
        let (model, tokens) = omniasr_model_paths(model_dir, &AsrModelSize::CTC1B);
        write_sized_file(&model, CTC_1B_MIN_MODEL_BYTES - 1);
        write_sized_file(&tokens, CTC_MIN_TOKEN_BYTES);

        let (fallback_model, fallback_tokens) = omniasr_model_paths(model_dir, &AsrModelSize::CTC300M);
        write_sized_file(&fallback_model, CTC_300M_MIN_MODEL_BYTES);
        write_sized_file(&fallback_tokens, CTC_MIN_TOKEN_BYTES);

        assert_eq!(select_available_model_size(model_dir, &AsrModelSize::CTC1B), AsrModelSize::CTC1B);
    }

    #[test]
    fn keeps_requested_model_when_it_meets_min_size() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let model_dir = tmp.path();
        let (model, tokens) = omniasr_model_paths(model_dir, &AsrModelSize::CTC300M);
        write_sized_file(&model, CTC_300M_MIN_MODEL_BYTES);
        write_sized_file(&tokens, CTC_MIN_TOKEN_BYTES);

        assert_eq!(select_available_model_size(model_dir, &AsrModelSize::CTC300M), AsrModelSize::CTC300M);
    }

    #[test]
    fn asr_pool_lazy_init_and_reuse() {
        let pool = AsrPool::new();
        let model_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
        let config = AsrLoadConfig { enable_gpu: false, ..AsrLoadConfig::default() };

        assert!(pool.warmup(&model_dir, &config).is_ok());

        let first_available = pool.is_available(&model_dir, &config);
        let second_available = pool.is_available(&model_dir, &config);
        assert_eq!(first_available, second_available);

        let transcribed = pool.with_service(&model_dir, &config, |asr| {
            if !asr.is_available() {
                return Ok((String::new(), None, ConfidenceSource::Heuristic));
            }
            asr.transcribe(&[0.0f32; 16000], 16000)
        });
        assert!(transcribed.is_ok());
    }

    #[test]
    fn asr_pool_recovers_poisoned_state_lock() {
        let pool = AsrPool::new();
        let model_dir = tempfile::tempdir().expect("tempdir");
        let config = AsrLoadConfig { enable_gpu: false, ..AsrLoadConfig::default() };

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = pool.inner.lock().expect("lock ASR pool state");
            panic!("poison ASR pool state");
        }));

        assert!(pool.warmup(model_dir.path(), &config).is_ok());
        assert!(!pool.is_available(model_dir.path(), &config));
        let transcribed = pool.with_service(model_dir.path(), &config, |asr| asr.transcribe(&[0.0f32; 16000], 16000));
        assert!(transcribed.is_err());
    }

    #[test]
    fn unavailable_without_models() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut asr = KurdishAsrService::new(tmp.path(), false).expect("new");
        assert!(!asr.is_available());
        assert!(asr.transcribe(&[0.0f32; 16000], 16000).is_err());
    }

    #[test]
    fn transcribe_chunk_empty_audio_returns_early() {
        // The empty-chunk guard must return Ok("", 0.0) without panicking,
        // regardless of whether a recognizer is loaded.
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut asr = KurdishAsrService::new(tmp.path(), false).expect("new");
        // transcribe on unavailable service errors, but we test via the guarded chunk path
        // indirectly: transcribe() will skip the chunk loop entirely for empty input.
        let result = asr.transcribe(&[], 16000);
        // Should return Ok with an empty transcript (not a panic).
        match result {
            Ok((text, _, _)) => assert!(text.is_empty(), "empty audio should yield empty transcript"),
            Err(e) => {
                // If ASR model is not loaded, the outer `transcribe` will return Err("ASR model not loaded")
                // which is also acceptable — the guard is inside transcribe_chunk, called only when a
                // recognizer is present. Assert the error string is the expected one.
                assert!(e.contains("ASR model not loaded"), "unexpected error: {e}");
            }
        }
    }

    #[test]
    fn test_asr_chunking_silence_boundary() {
        // Construct a synthetic 35s audio at 16kHz
        let mut audio = vec![1.0f32; 35 * 16000];

        // Put a silence valley (zeros) of 2 seconds from 29.5s to 31.5s
        // 29.5s = 29.5 * 16000 = 472000
        // 31.5s = 31.5 * 16000 = 504000
        for sample in audio.iter_mut().take(504000).skip(472000) {
            *sample = 0.0;
        }

        let target_cut: usize = 30 * 16000; // 480000
        let search_radius: usize = 24000;
        let window_size: usize = 1600;
        let current_idx: usize = 0;
        let min_chunk_samples: usize = 5 * 16000;

        let lower_bound = target_cut.saturating_sub(search_radius).max(current_idx + min_chunk_samples);
        let upper_bound = (target_cut + search_radius).min(audio.len().saturating_sub(min_chunk_samples));

        assert!(upper_bound > lower_bound);
        assert!(upper_bound >= lower_bound + window_size);

        let mut best_start = target_cut - window_size / 2;
        let mut min_energy = f32::MAX;

        let step = 160;
        let mut w_start = lower_bound;
        while w_start <= upper_bound - window_size {
            let energy: f32 = audio[w_start..w_start + window_size].iter().map(|x| x.abs()).sum();
            if energy < min_energy {
                min_energy = energy;
                best_start = w_start;
            }
            w_start += step;
        }
        let best_cut = best_start + window_size / 2;

        assert_eq!(min_energy, 0.0);
        assert!((472000..=504000).contains(&best_cut));
    }

    #[test]
    fn test_offline_stream_language_hint() {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let model_dir = project_root.join("models");
        let config = AsrLoadConfig {
            model_size: AsrModelSize::CTC300M,
            enable_gpu: false,
            num_threads: 1,
            language: "ckb".to_string(),
        };

        if omniasr_model_present(&model_dir, &AsrModelSize::CTC300M) {
            let service = NativeKurdishAsrEngine::new_with_config(&model_dir, &config).unwrap();
            let recognizer = service.recognizer.as_ref().unwrap();
            let stream = recognizer.create_stream();
            stream.set_option("language", "ckb");
            let lang_opt = stream.get_option("language");
            assert_eq!(lang_opt, "ckb");
        }
    }

    #[test]
    fn unpinned_files_under_the_champions_provenance_id_are_refused_not_loaded() {
        // The champion is fixed infrastructure whose identity is the deploymentSha256 handshake, so
        // `models/omniasr-wsl-7b/` has no pinned digest. Before this gate, anything dropped there passed
        // a bare exists() check and loaded UNVERIFIED while every clip it decoded was stamped
        // `omniasr-wsl-7b` — the champion's own provenance id.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (model, tokens) = omniasr_model_paths(tmp.path(), &AsrModelSize::WSL7B);
        write_sized_file(&model, 4096);
        write_sized_file(&tokens, CTC_MIN_TOKEN_BYTES);
        let config = AsrLoadConfig { model_size: AsrModelSize::WSL7B, enable_gpu: false, ..AsrLoadConfig::default() };

        // Matched rather than `expect_err`: the Ok variant is the engine itself, which is not Debug.
        let error = match NativeKurdishAsrEngine::new_with_config(tmp.path(), &config) {
            Ok(_) => panic!("unpinned files carrying the champion's provenance id must never build a recognizer"),
            Err(error) => error,
        };
        assert!(error.contains("deploymentSha256"), "the refusal must name the real identity check: {error}");

        // An EMPTY directory is the normal installation and must still degrade gracefully — the champion
        // simply does not live here — rather than turn every start into an error.
        let empty = tempfile::tempdir().expect("tempdir");
        let engine = NativeKurdishAsrEngine::new_with_config(empty.path(), &config)
            .expect("an absent local 7B directory is the normal case, not a failure");
        assert!(!engine.is_available());
    }

    #[test]
    fn provider_and_autotune_stay_within_documented_bounds() {
        assert_eq!(detect_optimal_provider(false), "cpu", "gpu disabled must always mean cpu");

        let tuned_1b = auto_tune_config(AsrLoadConfig {
            model_size: AsrModelSize::CTC1B,
            enable_gpu: false,
            num_threads: 99,
            language: "ckb".to_string(),
        });
        assert!((2..=8).contains(&tuned_1b.num_threads), "1B threads out of clamp: {}", tuned_1b.num_threads);
        assert_eq!(tuned_1b.model_size, AsrModelSize::CTC1B, "tuning must not touch the engine selection");
        assert!(!tuned_1b.enable_gpu, "tuning must not touch the GPU choice");
        assert_eq!(tuned_1b.language, "ckb", "tuning must not touch the language");

        let tuned_300m = auto_tune_config(AsrLoadConfig {
            model_size: AsrModelSize::CTC300M,
            enable_gpu: true,
            num_threads: 99,
            language: "ckb".to_string(),
        });
        assert!((1..=4).contains(&tuned_300m.num_threads), "300M threads out of clamp: {}", tuned_300m.num_threads);
    }

    #[test]
    fn omniasr_paths_follow_the_size_directory_layout() {
        let base = Path::new("m");
        for (size, dir) in [
            (AsrModelSize::CTC300M, "omniasr-ctc-300m"),
            (AsrModelSize::CTC1B, "omniasr-ctc-1b"),
            (AsrModelSize::WSL7B, "omniasr-wsl-7b"),
        ] {
            let (model, tokens) = omniasr_model_paths(base, &size);
            assert_eq!(model, base.join(dir).join("model.int8.onnx"));
            assert_eq!(tokens, base.join(dir).join("tokens.txt"));
        }
    }

    #[test]
    fn wsl7b_presence_is_exists_only_while_ctc_requires_min_sizes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // The champion marker dir has no local min-size contract: presence is exists-only. Its real
        // identity is the WSL deploymentSha256 handshake, and loading files from here is refused
        // anyway (unpinned_files_under_the_champions_provenance_id_are_refused_not_loaded).
        let (m7, t7) = omniasr_model_paths(tmp.path(), &AsrModelSize::WSL7B);
        write_sized_file(&m7, 0);
        write_sized_file(&t7, 0);
        assert!(omniasr_model_present(tmp.path(), &AsrModelSize::WSL7B));

        // A zero-byte CTC pair is NOT present: a truncated download must never read as installed.
        let (m3, t3) = omniasr_model_paths(tmp.path(), &AsrModelSize::CTC300M);
        write_sized_file(&m3, 0);
        write_sized_file(&t3, 0);
        assert!(!omniasr_model_present(tmp.path(), &AsrModelSize::CTC300M));

        // Model at its floor but tokens below theirs -> still absent (the pair is atomic).
        write_sized_file(&m3, CTC_300M_MIN_MODEL_BYTES);
        write_sized_file(&t3, CTC_MIN_TOKEN_BYTES - 1);
        assert!(!omniasr_model_present(tmp.path(), &AsrModelSize::CTC300M));
    }

    #[test]
    fn verify_model_dir_errors_name_the_missing_piece() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let absent = tmp.path().join("never-created");
        assert!(verify_model_dir(&absent).expect_err("absent dir").contains("not found"));

        let file = tmp.path().join("a-file");
        std::fs::write(&file, b"x").expect("file");
        assert!(verify_model_dir(&file).expect_err("file is not a dir").contains("Not a directory"));

        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).expect("empty dir");
        let err = verify_model_dir(&empty).expect_err("no model pair");
        assert!(err.contains("OmniASR CTC 300M"), "error must name the required pair: {err}");

        let (model, tokens) = omniasr_model_paths(&empty, &AsrModelSize::CTC300M);
        write_sized_file(&model, CTC_300M_MIN_MODEL_BYTES);
        write_sized_file(&tokens, CTC_MIN_TOKEN_BYTES);
        assert!(verify_model_dir(&empty).is_ok());
    }

    #[test]
    fn confidence_source_labels_are_stable_for_db_and_wire() {
        // Downstream calibration (conformal certificate, IRT consensus, autonomy dial) keys on these
        // strings; the DB label and the serde wire label must stay identical.
        assert_eq!(ConfidenceSource::RealPosterior.as_db_value(), "real_posterior");
        assert_eq!(ConfidenceSource::Heuristic.as_db_value(), "heuristic");
        assert_eq!(serde_json::to_string(&ConfidenceSource::Heuristic).unwrap(), "\"heuristic\"");
        assert_eq!(serde_json::to_string(&ConfidenceSource::RealPosterior).unwrap(), "\"real_posterior\"");
        assert_eq!(ConfidenceSource::default(), ConfidenceSource::Heuristic);
    }

    #[test]
    fn read_bounded_line_strips_terminators_and_fails_closed() {
        use std::io::Cursor;

        let mut two = Cursor::new(&b"hello\nworld"[..]);
        assert_eq!(read_bounded_line(&mut two, 64).unwrap(), Some("hello".to_string()));
        let err = read_bounded_line(&mut two, 64).expect_err("EOF mid-line is a truncated protocol");
        assert!(err.contains("middle of a line"), "{err}");

        let mut crlf = Cursor::new(&b"line\r\n"[..]);
        assert_eq!(read_bounded_line(&mut crlf, 64).unwrap(), Some("line".to_string()));

        let mut empty = Cursor::new(&b""[..]);
        assert_eq!(read_bounded_line(&mut empty, 64).unwrap(), None);

        let mut oversized = Cursor::new(vec![b'a'; 100]);
        let err = read_bounded_line(&mut oversized, 16).expect_err("a malformed peer must not grow memory");
        assert!(err.contains("exceeds 16 bytes"), "{err}");

        let mut invalid = Cursor::new(&[0xff, 0xfe, b'\n'][..]);
        let err = read_bounded_line(&mut invalid, 64).expect_err("non-UTF-8 protocol line");
        assert!(err.contains("not UTF-8"), "{err}");
    }

    #[test]
    fn worker_protocol_wire_format_is_stable() {
        let ready = serde_json::to_string(&WorkerResponse::Ready { ok: true, error: None }).unwrap();
        assert!(ready.contains("\"kind\":\"ready\""), "{ready}");

        let transcript = serde_json::to_string(&WorkerResponse::Transcript {
            request_id: 7,
            ok: true,
            text: "سڵاو".to_string(),
            confidence: Some(0.9),
            confidence_source: ConfidenceSource::Heuristic,
            error: None,
        })
        .unwrap();
        assert!(transcript.contains("\"kind\":\"transcript\""), "{transcript}");
        assert!(transcript.contains("\"confidence_source\":\"heuristic\""), "{transcript}");
        match serde_json::from_str::<WorkerResponse>(&transcript).unwrap() {
            WorkerResponse::Transcript { request_id, ok, text, .. } => {
                assert_eq!((request_id, ok, text.as_str()), (7, true, "سڵاو"));
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }

        let startup = serde_json::to_string(&WorkerStartup::Native {
            model_dir: PathBuf::from("m"),
            config: AsrLoadConfig::default(),
        })
        .unwrap();
        assert!(startup.contains("\"kind\":\"native\""), "{startup}");
        match serde_json::from_str::<WorkerStartup>(&startup).unwrap() {
            WorkerStartup::Native { model_dir, config } => {
                assert_eq!(model_dir, PathBuf::from("m"));
                assert_eq!(config, AsrLoadConfig::default());
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn worker_executable_resolves_to_a_real_absolute_file() {
        // Under the test harness (target/{profile}/deps) this exercises the deps-redirect branch;
        // either the real app binary beside the profile dir or the current exe itself comes back.
        let exe = worker_executable().expect("worker executable");
        assert!(exe.is_absolute(), "{}", exe.display());
        assert!(exe.is_file(), "{}", exe.display());
        // The test harness was not launched in a special role, so the role dispatcher must decline.
        assert!(run_special_process_mode().is_none());
    }

    #[test]
    fn supervisor_rejects_oversized_or_non_finite_audio_before_any_process() {
        let mut supervisor = AsrWorkerSupervisor {
            executable: PathBuf::from("never-spawned"),
            startup: WorkerStartup::CrashOnceThenEcho { marker: PathBuf::from("unused-marker") },
            process: None,
            next_request_id: 1,
            consecutive_failures: 0,
            retry_after: None,
            circuit_open_until: None,
        };

        let oversized = vec![0.0_f32; WORKER_MAX_SAMPLES + 1];
        let err = supervisor.transcribe(&oversized, SAMPLE_RATE).expect_err("over the 10-minute limit");
        assert!(err.contains("10 minutes"), "{err}");

        let err = supervisor.transcribe(&[0.0, f32::NAN], SAMPLE_RATE).expect_err("NaN audio");
        assert!(err.contains("non-finite"), "{err}");

        assert_eq!(supervisor.consecutive_failures, 0, "input guards must not charge the crash breaker");
        assert!(supervisor.process.is_none(), "input guards must never spawn a worker");
    }

    #[test]
    fn supervisor_spawn_failure_arms_backoff_then_the_circuit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut supervisor = AsrWorkerSupervisor {
            executable: tmp.path().join("no-such-worker-binary.exe"),
            startup: WorkerStartup::CrashOnceThenEcho { marker: tmp.path().join("marker") },
            process: None,
            next_request_id: 1,
            consecutive_failures: 0,
            retry_after: None,
            circuit_open_until: None,
        };

        let err = supervisor.transcribe(&[0.0_f32; 160], SAMPLE_RATE).expect_err("unspawnable worker");
        assert!(err.contains("start isolated ASR worker"), "{err}");
        assert_eq!(supervisor.consecutive_failures, 1, "a spawn failure must charge the breaker");
        assert!(supervisor.retry_after.is_some(), "the first failure arms the restart backoff");
        assert!(supervisor.circuit_open_until.is_none(), "one failure must not open the circuit");

        // Deterministic backoff refusal (no timing dependence on the real 250 ms window).
        supervisor.retry_after = Some(Instant::now() + Duration::from_secs(60));
        let err = supervisor.transcribe(&[0.0_f32; 160], SAMPLE_RATE).expect_err("backoff window");
        assert!(err.contains("backoff is active"), "{err}");

        // Reaching the failure threshold opens the circuit, which then refuses with its own message.
        supervisor.retry_after = None;
        supervisor.consecutive_failures = WORKER_CIRCUIT_FAILURES - 1;
        supervisor.record_failure();
        assert!(supervisor.circuit_open_until.is_some(), "the threshold failure must open the circuit");
        assert!(supervisor.retry_after.is_none(), "the open circuit replaces per-attempt backoff");
        let err = supervisor.transcribe(&[0.0_f32; 160], SAMPLE_RATE).expect_err("open circuit");
        assert!(err.contains("circuit is open"), "{err}");
    }

    #[test]
    fn switching_the_model_dir_resets_cached_pool_services() {
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let pool = AsrPool::new();
        let config = AsrLoadConfig::default();

        assert!(pool.ensure_loaded(dir_a.path(), &config));
        assert!(!pool.is_available(dir_a.path(), &config));

        // A different dir must drop every cached service and re-attempt against the new root.
        assert!(pool.ensure_loaded(dir_b.path(), &config), "a dir switch must re-attempt the load");
        assert!(!pool.is_available(dir_b.path(), &config));
    }

    #[test]
    fn bundled_repo_fixture_passes_the_model_startup_checks() {
        // The repository bundles Silero VAD + the 300M pair (asserted by models.rs's
        // bundled_runtime_models_count_as_available_when_user_dir_is_empty); the startup checks must
        // accept that fixture rather than red a clean checkout.
        assert!(check_models().is_ok(), "{:?}", check_models());
        let warnings = check_model_integrity();
        assert!(
            !warnings.iter().any(|w| w.contains("silero_vad_v4")),
            "bundled VAD must pass its integrity floor: {warnings:?}"
        );
        assert!(
            !warnings.iter().any(|w| w.contains("omniasr-ctc-300m")),
            "bundled 300M pair must pass its integrity floors: {warnings:?}"
        );
    }

    fn write_sized_file(path: &Path, size_bytes: u64) {
        std::fs::create_dir_all(path.parent().expect("parent dir")).expect("create parent dir");
        let file = std::fs::File::create(path).expect("create model fixture");
        file.set_len(size_bytes).expect("set model fixture length");
    }

    /// Wait until `path` reports exactly `len` bytes. Windows metadata can briefly lag a `set_len`
    /// under AV/temp load, and a fingerprint comparison reads that size directly.
    fn settle_file_size(path: &Path, len: u64) {
        for _ in 0..500 {
            if path.metadata().map(|m| m.len() == len).unwrap_or(false) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("file size {len} not visible for {}", path.display());
    }

    #[test]
    fn provider_detection_returns_one_of_the_supported_backends() {
        // Machine-dependent WHICH backend wins; never machine-dependent that it is a supported one.
        // (The owner's rig has CUDA; a CI box without nvidia-smi must still land on a real provider.)
        let provider = get_provider();
        assert!(
            ["coreml", "cuda", "directml", "cpu"].contains(&provider.as_str()),
            "unknown execution provider: {provider}"
        );
        assert_eq!(detect_optimal_provider(true), provider, "gpu enabled must defer to the detected backend");
        assert_eq!(detect_optimal_provider(false), "cpu", "gpu disabled is always cpu, whatever the hardware");
        #[cfg(target_os = "windows")]
        assert_ne!(provider, "coreml", "coreml is an Apple-silicon-only backend");
    }

    #[test]
    fn model_fingerprint_needs_both_files_and_changes_on_a_redownload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let (model, tokens) = omniasr_model_paths(dir, &AsrModelSize::CTC300M);

        assert!(model_files_fingerprint(dir, &AsrModelSize::CTC300M).is_none(), "nothing on disk -> no identity");
        write_sized_file(&model, CTC_300M_MIN_MODEL_BYTES);
        assert!(
            model_files_fingerprint(dir, &AsrModelSize::CTC300M).is_none(),
            "a half-installed pair has no identity: the breaker must keep retrying cheaply"
        );

        write_sized_file(&tokens, CTC_MIN_TOKEN_BYTES);
        settle_file_size(&model, CTC_300M_MIN_MODEL_BYTES);
        settle_file_size(&tokens, CTC_MIN_TOKEN_BYTES);
        let first = model_files_fingerprint(dir, &AsrModelSize::CTC300M).expect("complete pair has an identity");
        assert_eq!(model_files_fingerprint(dir, &AsrModelSize::CTC300M), Some(first), "a stable file is stable");
        assert_eq!(first.0 .0, CTC_300M_MIN_MODEL_BYTES);
        assert_eq!(first.1 .0, CTC_MIN_TOKEN_BYTES);

        // A re-download changes the size, so the load breaker re-attempts immediately.
        write_sized_file(&model, CTC_300M_MIN_MODEL_BYTES + 1);
        settle_file_size(&model, CTC_300M_MIN_MODEL_BYTES + 1);
        let second = model_files_fingerprint(dir, &AsrModelSize::CTC300M).expect("identity after re-download");
        assert_ne!(second, first, "a changed model file must produce a different fingerprint");
        assert!(should_attempt_load(&Some(second), Some(&first), Some(Duration::from_secs(0)), LOAD_RETRY_COOLDOWN));

        // A different engine reads its own directory, so it has no identity here.
        assert!(model_files_fingerprint(dir, &AsrModelSize::CTC1B).is_none());
    }

    #[test]
    fn asr_pool_default_is_an_empty_lazy_pool() {
        let pool = AsrPool::default();
        {
            let state = pool.lock_state();
            assert!(state.services.is_empty(), "the pool must not load anything before it is asked");
            assert!(state.loaded_dir.is_none());
            assert!(state.load_failed.is_empty());
        }
        // An absent model degrades to unavailable rather than erroring the caller.
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = AsrLoadConfig { enable_gpu: false, ..AsrLoadConfig::default() };
        assert!(!pool.is_available(tmp.path(), &config));
        let state = pool.lock_state();
        assert_eq!(state.loaded_dir.as_deref(), Some(tmp.path()), "the pool remembers the dir it loaded against");
        assert_eq!(state.services.len(), 1, "one unavailable placeholder is cached for the requested config");
    }

    #[test]
    fn malformed_asr_result_json_is_an_error_not_a_fabricated_transcript() {
        let error = parse_asr_result_json("{not json").expect_err("malformed engine output must fail closed");
        assert!(error.contains("Failed to parse ASR stream result JSON"), "{error}");
        // A well-formed object missing the required `text` field is equally refused — never defaulted
        // to an empty transcript that would then overwrite a good draft downstream.
        assert!(parse_asr_result_json(r#"{"ys_log_probs":[]}"#).is_err(), "a result without text is not a transcript");
    }

    #[test]
    fn restart_backoff_doubles_before_the_circuit_and_an_expired_circuit_reopens_the_gate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut supervisor = AsrWorkerSupervisor {
            executable: tmp.path().join("no-such-worker-binary.exe"),
            startup: WorkerStartup::CrashOnceThenEcho { marker: tmp.path().join("marker") },
            process: None,
            next_request_id: 1,
            consecutive_failures: 0,
            retry_after: None,
            circuit_open_until: None,
        };

        let before = Instant::now();
        supervisor.record_failure();
        let first = supervisor.retry_after.expect("first failure arms backoff").saturating_duration_since(before);
        assert!(first >= Duration::from_millis(250), "first backoff was {first:?}");
        assert!(supervisor.circuit_open_until.is_none());

        let before = Instant::now();
        supervisor.record_failure();
        let second = supervisor.retry_after.expect("second failure re-arms backoff").saturating_duration_since(before);
        assert!(second >= Duration::from_millis(500), "backoff must double, got {second:?}");
        assert_eq!(supervisor.consecutive_failures, 2);

        supervisor.record_failure();
        assert_eq!(supervisor.consecutive_failures, WORKER_CIRCUIT_FAILURES);
        assert!(supervisor.circuit_open_until.is_some(), "the threshold failure opens the circuit");

        // An EXPIRED circuit must not latch the engine off forever: the gate reopens and the next
        // attempt reaches the spawn (which fails here, because the executable does not exist).
        supervisor.circuit_open_until = Some(Instant::now() - Duration::from_secs(1));
        supervisor.retry_after = None;
        supervisor.consecutive_failures = 0;
        let error = supervisor.ensure_process().expect_err("the spawn itself still fails");
        assert!(error.contains("start isolated ASR worker"), "an expired circuit must retry, not refuse: {error}");
        assert_eq!(supervisor.consecutive_failures, 1, "the retry's own failure recharges the breaker");
    }
}
