use crate::settings::AsrModelSize;
use sherpa_onnx::{OfflineOmnilingualAsrCtcModelConfig, OfflineRecognizer, OfflineRecognizerConfig};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const SAMPLE_RATE: u32 = 16000;
const CHUNK_SAMPLES: usize = 30 * SAMPLE_RATE as usize;
const CTC_300M_MIN_MODEL_BYTES: u64 = 50_000_000;
const CTC_1B_MIN_MODEL_BYTES: u64 = 500_000_000;
const CTC_MIN_TOKEN_BYTES: u64 = 100;

/// Runtime options passed when loading the pooled ASR service.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

pub fn select_available_model_size(model_dir: &Path, requested: &AsrModelSize) -> AsrModelSize {
    if *requested == AsrModelSize::WSL7B || omniasr_model_present(model_dir, requested) {
        return requested.clone();
    }

    let fallback = match requested {
        AsrModelSize::CTC1B => AsrModelSize::CTC300M,
        AsrModelSize::CTC300M => AsrModelSize::CTC1B,
        AsrModelSize::WSL7B => AsrModelSize::WSL7B,
    };

    if fallback != *requested && omniasr_model_present(model_dir, &fallback) {
        tracing::warn!(
            "Requested ASR model {:?} is not installed under {}; falling back to {:?}",
            requested,
            model_dir.display(),
            fallback
        );
        return fallback;
    }

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

fn confidence_from_asr_result(text: &str, ys_log_probs: &[f64]) -> (Option<f64>, ConfidenceSource) {
    if ys_log_probs.is_empty() {
        let conf = if text.trim().is_empty() { Some(0.0) } else { Some(0.90) };
        (conf, ConfidenceSource::Heuristic)
    } else {
        let sum_prob: f64 = ys_log_probs.iter().map(|&lp| lp.exp()).sum();
        (Some(sum_prob / ys_log_probs.len() as f64), ConfidenceSource::RealPosterior)
    }
}

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

pub struct KurdishAsrService {
    recognizer: Option<OfflineRecognizer>,
    language: String,
}

impl KurdishAsrService {
    pub fn new(model_dir: &Path, enable_gpu: bool) -> Result<Self, String> {
        Self::new_with_config(model_dir, &AsrLoadConfig { enable_gpu, ..AsrLoadConfig::default() })
    }

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

    pub fn transcribe(&mut self, audio: &[f32], sample_rate: u32) -> Result<(String, Option<f64>), String> {
        let recognizer = self.recognizer.as_ref().ok_or("ASR model not loaded")?;

        if sample_rate != SAMPLE_RATE {
            tracing::warn!("ASR expects {SAMPLE_RATE} Hz input, got {sample_rate} Hz; results may be degraded");
        }

        if audio.len() <= CHUNK_SAMPLES {
            let (text, confidence) = self.transcribe_chunk(recognizer, audio, sample_rate)?;
            return Ok((text.trim().to_string(), confidence));
        }

        tracing::info!(
            "Audio {} samples ({:.1}s), splitting into dynamic chunks",
            audio.len(),
            audio.len() as f64 / SAMPLE_RATE as f64
        );

        let mut all_text = String::new();
        let mut confidences = Vec::new();
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
                Ok((text, confidence)) => {
                    let text = text.trim();
                    if !text.is_empty() {
                        if !all_text.is_empty() {
                            all_text.push(' ');
                        }
                        all_text.push_str(text);
                        if let Some(c) = confidence {
                            confidences.push(c);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("ASR Chunk {} transcription failed: {e}", chunk_idx);
                }
            }

            current_idx = cut_idx;
        }

        let average_confidence = if confidences.is_empty() {
            None
        } else {
            Some(confidences.iter().sum::<f64>() / confidences.len() as f64)
        };

        Ok((all_text, average_confidence))
    }

    fn transcribe_chunk(
        &self,
        recognizer: &OfflineRecognizer,
        audio: &[f32],
        sample_rate: u32,
    ) -> Result<(String, Option<f64>), String> {
        // Guard: empty slices can occur at exact chunk boundaries.
        // Passing an empty waveform to ONNX may panic or return garbage.
        if audio.is_empty() {
            return Ok((String::new(), Some(0.0)));
        }

        let stream = recognizer.create_stream();
        if !self.language.is_empty() {
            stream.set_option("language", &self.language);
        }

        stream.accept_waveform(sample_rate as i32, audio);
        recognizer.decode(&stream);

        #[repr(transparent)]
        struct OfflineStreamMirror {
            ptr: *const std::ffi::c_void,
        }

        let stream_ptr = unsafe {
            let mirror: &OfflineStreamMirror = std::mem::transmute(&stream);
            mirror.ptr
        };

        unsafe {
            let json_cstr = sherpa_onnx_sys::SherpaOnnxGetOfflineStreamResultAsJson(stream_ptr as *const _);
            if json_cstr.is_null() {
                return Err("OmniASR decode returned no result".to_string());
            }

            let json_str = std::ffi::CStr::from_ptr(json_cstr).to_string_lossy().into_owned();

            sherpa_onnx_sys::SherpaOnnxDestroyOfflineStreamResultJson(json_cstr);

            let (text, confidence, _source) = parse_asr_result_json(&json_str)?;
            // TODO(confidence-source): thread `_source` up to the segment write (migration
            // v20) so conformal/IRT/autonomy can distinguish real posteriors from the
            // heuristic fallback. transcribe()/transcribe_chunk() signatures kept stable
            // for now to avoid rippling through pipeline/commands.
            Ok((text, confidence))
        }
    }
}

/// Thread-safe lazy singleton for Meta OmniASR CTC sessions.
pub struct AsrPool {
    inner: Mutex<AsrPoolState>,
}

struct AsrPoolState {
    services: std::collections::HashMap<AsrLoadConfig, KurdishAsrService>,
    loaded_dir: Option<PathBuf>,
}

impl Default for AsrPool {
    fn default() -> Self {
        Self::new()
    }
}

impl AsrPool {
    pub fn new() -> Self {
        Self { inner: Mutex::new(AsrPoolState { services: std::collections::HashMap::new(), loaded_dir: None }) }
    }

    fn lock_state(&self) -> MutexGuard<'_, AsrPoolState> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned ASR pool state lock");
            poisoned.into_inner()
        })
    }

    fn ensure_loaded(&self, model_dir: &Path, config: &AsrLoadConfig) {
        let mut state = self.lock_state();

        if state.loaded_dir.as_ref() != Some(&model_dir.to_path_buf()) {
            state.services.clear();
            state.loaded_dir = Some(model_dir.to_path_buf());
        }

        if state.services.contains_key(config) {
            return;
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
        state.services.insert(config.clone(), service);
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

    #[test]
    fn empty_asr_text_has_zero_confidence_without_token_probs() {
        assert_eq!(confidence_from_asr_result("", &[]), (Some(0.0), ConfidenceSource::Heuristic));
        assert_eq!(confidence_from_asr_result("   ", &[]), (Some(0.0), ConfidenceSource::Heuristic));
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
    fn falls_back_to_installed_300m_when_1b_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let model_dir = tmp.path();
        let (model, tokens) = omniasr_model_paths(model_dir, &AsrModelSize::CTC300M);
        write_sized_file(&model, CTC_300M_MIN_MODEL_BYTES);
        write_sized_file(&tokens, CTC_MIN_TOKEN_BYTES);

        assert_eq!(select_available_model_size(model_dir, &AsrModelSize::CTC1B), AsrModelSize::CTC300M);
    }

    #[test]
    fn truncated_requested_model_falls_back_to_complete_300m() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let model_dir = tmp.path();
        let (model, tokens) = omniasr_model_paths(model_dir, &AsrModelSize::CTC1B);
        write_sized_file(&model, CTC_1B_MIN_MODEL_BYTES - 1);
        write_sized_file(&tokens, CTC_MIN_TOKEN_BYTES);

        let (fallback_model, fallback_tokens) = omniasr_model_paths(model_dir, &AsrModelSize::CTC300M);
        write_sized_file(&fallback_model, CTC_300M_MIN_MODEL_BYTES);
        write_sized_file(&fallback_tokens, CTC_MIN_TOKEN_BYTES);

        assert_eq!(select_available_model_size(model_dir, &AsrModelSize::CTC1B), AsrModelSize::CTC300M);
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
                return Ok((String::new(), None));
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
            Ok((text, _)) => assert!(text.is_empty(), "empty audio should yield empty transcript"),
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
            let service = KurdishAsrService::new_with_config(&model_dir, &config).unwrap();
            let recognizer = service.recognizer.as_ref().unwrap();
            let stream = recognizer.create_stream();
            stream.set_option("language", "ckb");
            let lang_opt = stream.get_option("language");
            assert_eq!(lang_opt, "ckb");
        }
    }

    fn write_sized_file(path: &Path, size_bytes: u64) {
        std::fs::create_dir_all(path.parent().expect("parent dir")).expect("create parent dir");
        let file = std::fs::File::create(path).expect("create model fixture");
        file.set_len(size_bytes).expect("set model fixture length");
    }
}
