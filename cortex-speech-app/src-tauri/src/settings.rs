use crate::atomic_file::{remove_file_on_error, replace_file};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub model_dir: PathBuf,
    pub output_dir: PathBuf,
    pub asr_provider: AsrProvider,
    pub asr_model_size: AsrModelSize,
    pub vad_threshold: f32,
    pub min_segment_duration_ms: u32,
    pub max_segment_duration_ms: u32,
    pub num_asr_threads: u32,
    pub enable_gpu: bool,
    pub language: String,
    pub export_format: ExportFormat,
    pub auto_normalize: bool,
    pub auto_align: bool,
    /// When importing multi-chunk audio from one file, assign speaker_id from filename stem.
    #[serde(default = "default_assign_speaker_from_filename")]
    pub assign_speaker_from_filename: bool,
    /// Acoustic speaker diarization during import (mel embedding + clustering).
    #[serde(default = "default_enable_diarization")]
    pub enable_diarization: bool,
    /// Maximum distinct speakers to detect per source file.
    #[serde(default = "default_max_speakers")]
    pub max_speakers: u32,
    /// AI audio denoising before transcription.
    #[serde(default = "default_enable_denoising")]
    pub enable_denoising: bool,
    /// Fail validation when annotated segments exceed this WER vs hypothesis.
    #[serde(default = "default_max_wer_threshold")]
    pub max_wer_threshold: f64,
    /// Fail validation when annotated segments exceed this CER vs hypothesis.
    #[serde(default = "default_max_cer_threshold")]
    pub max_cer_threshold: f64,
    /// When true, `validate_dataset` treats WER/CER threshold breaches as errors.
    #[serde(default = "default_enforce_quality_gates")]
    pub enforce_quality_gates: bool,
    #[serde(default = "default_autoplay_segments")]
    pub autoplay_segments: bool,
    #[serde(default = "default_verbalize_numbers")]
    pub verbalize_numbers: bool,
    pub theme: Theme,

    // HuggingFace Export Settings
    #[serde(default = "default_hf_train_ratio")]
    pub hf_train_ratio: f64,
    #[serde(default = "default_hf_val_ratio")]
    pub hf_val_ratio: f64,
    #[serde(default = "default_hf_test_ratio")]
    pub hf_test_ratio: f64,
    #[serde(default = "default_hf_split_seed")]
    pub hf_split_seed: u64,
    #[serde(default = "default_hf_speaker_disjoint")]
    pub hf_speaker_disjoint: bool,
    #[serde(default = "default_hf_license")]
    pub hf_license: String,

    // AI Post-Processing
    #[serde(default)]
    pub llm_mode: LlmMode,
    #[serde(default = "default_llm_endpoint")]
    pub llm_endpoint: String,
    #[serde(default)]
    pub llm_api_key: String,
    #[serde(default)]
    pub llm_api_key_configured: bool,
    #[serde(default)]
    pub cloud_llm_opt_in: bool,
    /// Gate for cloud speech-to-text (ElevenLabs Scribe). When on (and a Scribe key is configured),
    /// imports transcribe the whole file via Scribe instead of the local ASR. Audio is sent to
    /// ElevenLabs' API — off by default, like every other cloud gate.
    #[serde(default)]
    pub cloud_stt_opt_in: bool,
    #[serde(default = "default_llm_system_prompt")]
    pub llm_system_prompt: String,
    #[serde(default = "default_llm_model")]
    pub llm_model: String,
    #[serde(default)]
    pub external_asr_script_path: String,

    // Listening Jury settings
    /// Gate for T2 Gemini audio calls.  Audio is sent to Gemini cloud.
    #[serde(default)]
    pub jury_cloud_opt_in: bool,
    #[serde(default = "default_jury_model")]
    pub jury_model: String,
    /// Whole-file source transcript models used before chunking for reference-aware adjudication.
    #[serde(default = "default_source_reference_models")]
    pub source_reference_models: Vec<String>,
    #[serde(default = "default_jury_self_consistency_n")]
    pub jury_self_consistency_n: u32,
    #[serde(default)]
    pub jury_autonomy_level: AutonLevel,
    #[serde(default = "default_jury_t1_threshold")]
    pub jury_t1_threshold: f64,

    /// Opt-in: apply LOOP-0 correction memories to the transcript after decoding/refinement, so a
    /// previously corrected (and independently confirmed) confusion is fixed automatically. Default
    /// OFF — it rewrites ASR output, so it is a deliberate choice, never a silent surprise.
    #[serde(default)]
    pub loop0_firing_enabled: bool,

    /// Opt-in: prime LLM refinement with the diverse N-best hypotheses + relevant past corrections
    /// (generative error correction) instead of plain single-string refinement. Default OFF — it
    /// changes the refinement prompt, so it is a deliberate, validate-on-your-data choice.
    #[serde(default)]
    pub ger_refinement_enabled: bool,
}

fn default_hf_train_ratio() -> f64 {
    0.8
}
fn default_hf_val_ratio() -> f64 {
    0.1
}
fn default_hf_test_ratio() -> f64 {
    0.1
}
fn default_hf_split_seed() -> u64 {
    42
}
fn default_hf_speaker_disjoint() -> bool {
    true
}
fn default_hf_license() -> String {
    "mit".to_string()
}

fn default_verbalize_numbers() -> bool {
    true
}

fn default_autoplay_segments() -> bool {
    false
}

fn default_llm_endpoint() -> String {
    "http://127.0.0.1:11434/v1/chat/completions".to_string()
}

fn default_llm_system_prompt() -> String {
    "You are an expert Kurdish linguist. Fix the phonetic transcription errors in the following text, preserving the exact meaning. Output ONLY the corrected text, no explanations.".to_string()
}

fn default_llm_model() -> String {
    "heretic-final:latest".to_string()
}

fn default_enable_diarization() -> bool {
    true
}

fn default_max_speakers() -> u32 {
    8
}

fn default_enable_denoising() -> bool {
    false
}

fn default_max_wer_threshold() -> f64 {
    crate::wer::DEFAULT_MAX_WER
}

fn default_max_cer_threshold() -> f64 {
    crate::wer::DEFAULT_MAX_CER
}

fn default_enforce_quality_gates() -> bool {
    false
}

fn default_assign_speaker_from_filename() -> bool {
    true
}

/// ASR backend identifier (OmniASR CTC via sherpa-onnx is the only engine).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum AsrProvider {
    #[default]
    #[serde(alias = "SherpaOnnxCtc")]
    SherpaOnnxCtc,
    /// Legacy settings value; deserialized for compatibility, not used for routing.
    #[serde(alias = "SherpaOnnxWhisper")]
    SherpaOnnxWhisper,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum LlmMode {
    None,
    #[default]
    Local,
    Gemini,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq, Hash)]
pub enum AsrModelSize {
    #[default]
    CTC300M,
    CTC1B,
    WSL7B,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum ExportFormat {
    #[default]
    Json,
    Csv,
    Jsonl,
    Parquet,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
    System,
}

/// Autonomy level for the Listening Jury (the "Autonomy Dial").
///
/// Serialized as snake_case to match the frontend, which sends/expects
/// `observe|propose|act_confirm|act_auto` (settingsStore.ts / SettingsPanel.svelte). Without this,
/// an Autonomy Dial click sent e.g. `"act_confirm"`, which failed to deserialize the WHOLE
/// `update_settings` payload (`unknown variant`), silently dropping every settings save in that
/// session. PascalCase aliases keep any pre-existing settings.json files loadable.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutonLevel {
    /// Agent only annotates cards without committing any verdict.
    #[serde(alias = "Observe")]
    Observe,
    /// Agent stages verdicts; human confirms each one (default).
    #[default]
    #[serde(alias = "Propose")]
    Propose,
    /// Agent auto-commits agreements; asks for human confirmation only on edits/rejects.
    #[serde(alias = "ActConfirm")]
    ActConfirm,
    /// Fully unattended — agent commits all verdicts without pausing.
    #[serde(alias = "ActAuto")]
    ActAuto,
}

fn default_jury_model() -> String {
    "gemini-2.5-pro".to_string()
}
fn default_source_reference_models() -> Vec<String> {
    vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()]
}
fn default_jury_self_consistency_n() -> u32 {
    3
}
fn default_jury_t1_threshold() -> f64 {
    0.75
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            model_dir: PathBuf::from("models"),
            output_dir: PathBuf::from("exports"),
            asr_provider: AsrProvider::default(),
            asr_model_size: AsrModelSize::CTC300M,
            vad_threshold: 0.5,
            min_segment_duration_ms: 3000,
            max_segment_duration_ms: 15000,
            num_asr_threads: 4,
            enable_gpu: true,
            language: "ckb".to_string(),
            export_format: ExportFormat::default(),
            auto_normalize: true,
            auto_align: false,
            assign_speaker_from_filename: true,
            enable_diarization: true,
            max_speakers: 8,
            enable_denoising: false,
            max_wer_threshold: crate::wer::DEFAULT_MAX_WER,
            max_cer_threshold: crate::wer::DEFAULT_MAX_CER,
            enforce_quality_gates: false,
            autoplay_segments: false,
            verbalize_numbers: true,
            theme: Theme::default(),
            hf_train_ratio: 0.8,
            hf_val_ratio: 0.1,
            hf_test_ratio: 0.1,
            hf_split_seed: 42,
            hf_speaker_disjoint: true,
            hf_license: "mit".to_string(),
            llm_mode: LlmMode::default(),
            llm_endpoint: default_llm_endpoint(),
            llm_api_key: "".to_string(),
            llm_api_key_configured: false,
            cloud_llm_opt_in: false,
            cloud_stt_opt_in: false,
            llm_system_prompt: default_llm_system_prompt(),
            llm_model: default_llm_model(),
            external_asr_script_path: "".to_string(),
            jury_cloud_opt_in: false,
            jury_model: default_jury_model(),
            source_reference_models: default_source_reference_models(),
            jury_self_consistency_n: default_jury_self_consistency_n(),
            jury_autonomy_level: AutonLevel::default(),
            jury_t1_threshold: default_jury_t1_threshold(),
            loop0_firing_enabled: false,
            ger_refinement_enabled: false,
        }
    }
}

/// Validate an outbound HTTP endpoint against the app's allow-list: at most 2048 chars and either an
/// `https://` URL or a localhost `http://` address; empty is rejected (an outbound POST needs a real
/// URL). Shared so every outbound channel — the LLM endpoint and the DPO export — enforces the same
/// rule. The Rust IPC layer is the trust boundary, so a malicious/XSS-planted argument cannot repoint
/// a request (and its payload) at an attacker-controlled host.
/// Whether an endpoint targets the LOCAL device (loopback) and is therefore NOT off-device egress —
/// so it needs no cloud-egress consent. Strict host match (not a loose `starts_with`): a hostname
/// like `localhost.attacker.example` must NOT be treated as local. Covers http/https + an optional
/// port and the IPv6 loopback `[::1]`.
pub(crate) fn endpoint_is_localhost(endpoint: &str) -> bool {
    let lower = endpoint.trim().to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("http://").or_else(|| lower.strip_prefix("https://")) else {
        return false;
    };
    let host_port = rest.split('/').next().unwrap_or("");
    if host_port.starts_with("[::1]") {
        return true; // IPv6 loopback, with or without a trailing :port
    }
    let host = host_port.split(':').next().unwrap_or("");
    host == "localhost" || host == "127.0.0.1"
}

pub fn validate_outbound_endpoint(endpoint: &str) -> Result<(), crate::error::AppError> {
    use crate::error::AppError;
    const MAX_ENDPOINT_LEN: usize = 2048;
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation("Endpoint URL must not be empty".into()));
    }
    if trimmed.len() > MAX_ENDPOINT_LEN {
        return Err(AppError::Validation("Endpoint URL is too long".into()));
    }
    let lower = trimmed.to_ascii_lowercase();
    let is_https = lower.starts_with("https://");
    let is_localhost = lower.starts_with("http://localhost")
        || lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://[::1]");
    if is_https || is_localhost {
        Ok(())
    } else {
        Err(AppError::Validation("Endpoint must be an https:// URL or a localhost http:// address".into()))
    }
}

impl AppSettings {
    pub fn load(path: &std::path::Path) -> Self {
        // If a previous save was interrupted (a hard crash between replace_file's two renames on
        // Windows, or a rename+restore double-failure), the canonical file can be missing while a
        // valid `.replace-bak-*` copy survives next to it. Promote it BEFORE reading so we never
        // silently revert persisted state — output dir, the cloud consent opt-ins, the configured-key
        // flag, jury settings — to defaults while the real values sit recoverable on disk. No-op when
        // the file is present (the common case).
        match crate::atomic_file::recover_interrupted_replace(path) {
            Ok(true) => {
                tracing::warn!("Recovered settings from an interrupted save at {}", path.display())
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!("Could not check for an interrupted settings save at {}: {e}", path.display())
            }
        }
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str::<AppSettings>(&s) {
                Ok(mut settings) => {
                    if !settings.llm_api_key.is_empty() {
                        settings.llm_api_key_configured = true;
                        settings.llm_api_key.clear();
                        if let Err(e) = settings.save(path) {
                            tracing::warn!(
                                "Failed to scrub plaintext LLM key from settings file at {}: {e}",
                                path.display()
                            );
                        }
                    }
                    settings
                }
                Err(e) => {
                    tracing::warn!("Failed to parse settings file at {}: {}; using defaults", path.display(), e);
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!("Failed to read settings file at {}: {}; using defaults", path.display(), e);
                Self::default()
            }
        }
    }

    /// Validate frontend-supplied settings SERVER-SIDE before they take effect — the Rust
    /// IPC layer, not the webview, is the trust boundary. A malicious or XSS-planted
    /// settings payload could otherwise repoint every LLM/refiner request (and the bearer
    /// API key) at an attacker-controlled server, or carry unbounded strings.
    pub fn validate(&self) -> Result<(), crate::error::AppError> {
        use crate::error::AppError;
        const MAX_ENDPOINT_LEN: usize = 2048;
        const MAX_MODEL_LEN: usize = 256;
        const MAX_PROMPT_LEN: usize = 16_384;

        if self.llm_endpoint.len() > MAX_ENDPOINT_LEN {
            return Err(AppError::Validation("LLM endpoint URL is too long".into()));
        }
        let endpoint = self.llm_endpoint.trim();
        if !endpoint.is_empty() {
            let lower = endpoint.to_ascii_lowercase();
            let is_https = lower.starts_with("https://");
            let is_localhost = lower.starts_with("http://localhost")
                || lower.starts_with("http://127.0.0.1")
                || lower.starts_with("http://[::1]");
            if !is_https && !is_localhost {
                return Err(AppError::Validation(
                    "LLM endpoint must be an https:// URL or a localhost http:// address".into(),
                ));
            }
        }
        if self.llm_model.len() > MAX_MODEL_LEN {
            return Err(AppError::Validation("LLM model name is too long".into()));
        }
        if self.llm_system_prompt.len() > MAX_PROMPT_LEN {
            return Err(AppError::Validation("LLM system prompt is too long".into()));
        }
        Ok(())
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), crate::error::AppError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut persisted = self.clone();
        if !persisted.llm_api_key.is_empty() {
            persisted.llm_api_key_configured = true;
        }
        persisted.llm_api_key.clear();
        let json = serde_json::to_string_pretty(&persisted)?;
        // Atomic write: write to .tmp file first, then rename so a crash mid-write
        // cannot leave a truncated/corrupt settings.json on disk.
        let tmp_path = path.with_extension("json.tmp");
        remove_file_on_error(
            &tmp_path,
            (|| -> Result<(), crate::error::AppError> {
                fs::write(&tmp_path, &json)?;
                replace_file(&tmp_path, path)?;
                Ok(())
            })(),
        )
    }

    pub fn for_client_response(&self) -> Self {
        let mut settings = self.clone();
        if !settings.llm_api_key.is_empty() {
            settings.llm_api_key_configured = true;
            settings.llm_api_key.clear();
        }
        settings
    }

    pub fn merge_session_secret_from(&mut self, current: &Self) {
        if self.llm_api_key.is_empty() && self.llm_api_key_configured && !current.llm_api_key.is_empty() {
            self.llm_api_key = current.llm_api_key.clone();
            self.llm_api_key_configured = true;
        }
    }

    pub fn effective_llm_mode(&self) -> LlmMode {
        match self.llm_mode {
            // Cloud Gemini requires explicit consent.
            LlmMode::Gemini if !self.cloud_llm_opt_in => LlmMode::None,
            // Round-22 #6: "Local" mode pointed at a NON-localhost endpoint is off-device egress of
            // transcript text (and the API key) just like Gemini — it must require the same
            // cloud_llm_opt_in consent. Without opt-in, downgrade to None so no transcript leaves the
            // device. A genuine on-device endpoint (Ollama at localhost) needs no consent.
            LlmMode::Local if !self.cloud_llm_opt_in && !endpoint_is_localhost(&self.llm_endpoint) => LlmMode::None,
            _ => self.llm_mode.clone(),
        }
    }

    pub fn external_asr_script_path(&self) -> Option<String> {
        let trimmed = self.external_asr_script_path.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    pub fn source_reference_models(&self) -> Vec<String> {
        let mut models = Vec::new();
        for model in &self.source_reference_models {
            let trimmed = model.trim();
            if !trimmed.is_empty() && !models.iter().any(|existing| existing == trimmed) {
                models.push(trimmed.to_string());
            }
        }
        if models.is_empty() {
            let fallback = self.jury_model.trim();
            if fallback.is_empty() {
                models.push(default_jury_model());
            } else {
                models.push(fallback.to_string());
            }
        }
        models
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_asr_model_matches_bundled_runtime_model() {
        assert_eq!(AppSettings::default().asr_model_size, AsrModelSize::CTC300M);
    }

    #[test]
    fn loop0_firing_defaults_off_and_is_backward_compatible() {
        // Opt-in: rewriting ASR output / changing the refine prompt must never be a surprise -> OFF.
        assert!(!AppSettings::default().loop0_firing_enabled);
        assert!(!AppSettings::default().ger_refinement_enabled);
        // A settings.json from before this field existed still loads (missing -> false), not an error.
        let mut v = serde_json::to_value(AppSettings::default()).unwrap();
        v.as_object_mut().unwrap().remove("loop0_firing_enabled");
        let legacy: AppSettings = serde_json::from_value(v).expect("legacy settings (no field) must load");
        assert!(!legacy.loop0_firing_enabled, "a missing field must default to OFF");
        // It round-trips when explicitly enabled.
        let on = AppSettings { loop0_firing_enabled: true, ..AppSettings::default() };
        let json = serde_json::to_string(&on).unwrap();
        assert!(serde_json::from_str::<AppSettings>(&json).unwrap().loop0_firing_enabled);
    }

    #[test]
    fn cloud_stt_opt_in_persists_through_save_load_and_is_backward_compatible() {
        // Backward-compat: a settings.json written before this field existed must still load, with
        // the field defaulting to OFF (no surprise cloud STT calls for existing users).
        let mut v = serde_json::to_value(AppSettings::default()).unwrap();
        v.as_object_mut().unwrap().remove("cloud_stt_opt_in");
        let legacy: AppSettings = serde_json::from_value(v).expect("legacy settings (no field) must load");
        assert!(!legacy.cloud_stt_opt_in, "a missing field defaults to OFF");

        // Restart-safety: the toggle must survive the REAL persistence path (atomic write + key
        // scrub), not just a raw serde round-trip — otherwise it would silently reset every launch.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        AppSettings { cloud_stt_opt_in: true, ..AppSettings::default() }.save(&path).expect("save");
        assert!(AppSettings::load(&path).cloud_stt_opt_in, "the Scribe toggle must survive save -> load");
    }

    #[test]
    fn load_recovers_settings_from_an_interrupted_save_backup() {
        // Post-crash state the round-15 atomic_file fix addresses: the canonical settings.json is
        // MISSING (the durable rename never completed), but a valid `.replace-bak-*` sibling still
        // holds the user's real settings (cloud STT opted in). load() must promote that backup instead
        // of silently returning defaults — which would flip the consent opt-in OFF and drop the
        // configured key, with the recoverable data left orphaned on disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let backup = dir.path().join("settings.json.replace-bak-9999");
        let real = AppSettings { cloud_stt_opt_in: true, ..AppSettings::default() };
        std::fs::write(&backup, serde_json::to_string(&real).unwrap()).unwrap();
        assert!(!path.exists(), "the canonical file is missing (interrupted save)");

        let loaded = AppSettings::load(&path);

        assert!(loaded.cloud_stt_opt_in, "consent opt-in must be recovered, not reverted to default OFF");
        assert!(path.exists(), "the backup must have been promoted to the canonical path");
    }

    #[test]
    fn validate_accepts_https_and_localhost_endpoints() {
        // The default (Ollama localhost) must pass.
        assert!(AppSettings::default().validate().is_ok());
        for ep in [
            "https://generativelanguage.googleapis.com/v1beta",
            "http://localhost:11434/v1/chat/completions",
            "http://127.0.0.1:8080/x",
            "", // unset is fine
        ] {
            let s = AppSettings { llm_endpoint: ep.to_string(), ..AppSettings::default() };
            assert!(s.validate().is_ok(), "endpoint should be accepted: {ep:?}");
        }
    }

    #[test]
    fn validate_outbound_endpoint_enforces_https_or_localhost() {
        for ok in ["https://example.com/x", "http://localhost:8080", "http://127.0.0.1/y", "http://[::1]:9/z"] {
            assert!(super::validate_outbound_endpoint(ok).is_ok(), "{ok} should pass");
        }
        // Empty, non-https remote, and non-http schemes are all rejected (exfil / SSRF surface).
        for bad in ["", "   ", "http://attacker.example.com/collect", "ftp://x", "file:///etc/passwd"] {
            assert!(super::validate_outbound_endpoint(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn endpoint_is_localhost_is_strict() {
        for local in [
            "http://localhost:11434/v1/chat/completions",
            "http://127.0.0.1:8080/x",
            "https://localhost/x",
            "http://[::1]:9/z",
        ] {
            assert!(super::endpoint_is_localhost(local), "{local} is local");
        }
        for remote in [
            "https://api.openai.com/v1/chat/completions",
            "http://localhost.attacker.example/exfil", // must NOT be treated as local
            "https://127.0.0.1.attacker.example/x",
            "ftp://localhost",
        ] {
            assert!(!super::endpoint_is_localhost(remote), "{remote} is NOT local");
        }
    }

    #[test]
    fn local_mode_remote_endpoint_requires_cloud_consent() {
        // Round-22 #6: Local mode pointed at a REMOTE endpoint is off-device egress and must be gated by
        // cloud_llm_opt_in, exactly like Gemini — otherwise transcripts leak with no consent.
        let remote = |opt_in: bool| AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "https://remote-llm.example/v1/chat/completions".to_string(),
            cloud_llm_opt_in: opt_in,
            ..AppSettings::default()
        };
        assert_eq!(remote(false).effective_llm_mode(), LlmMode::None, "remote Local without opt-in must downgrade");
        assert_eq!(remote(true).effective_llm_mode(), LlmMode::Local, "remote Local WITH opt-in is allowed");

        // A genuine on-device (localhost) endpoint needs no consent and is unaffected.
        let local = AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "http://127.0.0.1:11434/v1/chat/completions".to_string(),
            cloud_llm_opt_in: false,
            ..AppSettings::default()
        };
        assert_eq!(local.effective_llm_mode(), LlmMode::Local, "localhost Local needs no consent");
    }

    #[test]
    fn autonlevel_roundtrips_frontend_snake_case() {
        // Round-2 audit HIGH: the Autonomy Dial sends snake_case; without rename_all it failed to
        // deserialize and silently broke every settings save.
        for (json, variant) in [
            ("\"observe\"", AutonLevel::Observe),
            ("\"propose\"", AutonLevel::Propose),
            ("\"act_confirm\"", AutonLevel::ActConfirm),
            ("\"act_auto\"", AutonLevel::ActAuto),
        ] {
            assert_eq!(serde_json::from_str::<AutonLevel>(json).unwrap(), variant, "{json} must parse");
        }
        // Serializes BACK as snake_case so the dropdown's `=== val` comparison matches.
        assert_eq!(serde_json::to_string(&AutonLevel::ActConfirm).unwrap(), "\"act_confirm\"");
        // Pre-existing PascalCase settings.json files still load via the aliases.
        assert_eq!(serde_json::from_str::<AutonLevel>("\"ActConfirm\"").unwrap(), AutonLevel::ActConfirm);
        assert_eq!(serde_json::from_str::<AutonLevel>("\"Propose\"").unwrap(), AutonLevel::Propose);
    }

    #[test]
    fn update_settings_payload_with_snake_case_autonomy_deserializes() {
        // Mirrors the real update_settings IPC arg the adapter produces.
        let mut json = serde_json::to_value(AppSettings::default()).unwrap();
        json["jury_autonomy_level"] = serde_json::json!("act_confirm");
        let parsed: AppSettings = serde_json::from_value(json).expect("settings payload must deserialize");
        assert_eq!(parsed.jury_autonomy_level, AutonLevel::ActConfirm);
    }

    #[test]
    fn validate_rejects_non_https_remote_endpoint_and_oversized_fields() {
        let remote_http =
            AppSettings { llm_endpoint: "http://evil.example.com/exfil".to_string(), ..AppSettings::default() };
        assert!(
            matches!(remote_http.validate(), Err(crate::error::AppError::Validation(_))),
            "a non-https remote endpoint must be rejected (exfil risk)"
        );

        let huge_prompt = AppSettings { llm_system_prompt: "x".repeat(20_000), ..AppSettings::default() };
        assert!(matches!(huge_prompt.validate(), Err(crate::error::AppError::Validation(_))));

        let huge_endpoint =
            AppSettings { llm_endpoint: format!("https://{}", "a".repeat(3000)), ..AppSettings::default() };
        assert!(matches!(huge_endpoint.validate(), Err(crate::error::AppError::Validation(_))));
    }

    #[test]
    fn save_replaces_existing_settings_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        fs::write(&path, "{\"language\":\"old\"}").expect("seed settings");

        let settings = AppSettings { language: "ckb".to_string(), ..AppSettings::default() };

        settings.save(&path).expect("settings save should replace existing file");

        let loaded = AppSettings::load(&path);
        assert_eq!(loaded.language, "ckb");
        assert_eq!(loaded.asr_model_size, AsrModelSize::CTC300M);
        assert!(!path.with_extension("json.tmp").exists());
        assert!(!backup_files_left(tmp.path()));
    }

    #[test]
    fn save_clears_secret_material_but_marks_key_configured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let settings = AppSettings { llm_api_key: "secret-token".to_string(), ..AppSettings::default() };

        settings.save(&path).expect("settings save");

        let saved = fs::read_to_string(&path).expect("read saved settings");
        assert!(!saved.contains("secret-token"));

        let loaded = AppSettings::load(&path);
        assert!(loaded.llm_api_key.is_empty());
        assert!(loaded.llm_api_key_configured);
    }

    #[test]
    fn load_scrubs_legacy_plaintext_secret_from_settings_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let legacy = AppSettings {
            language: "ckb".to_string(),
            llm_api_key: "legacy-secret-token".to_string(),
            llm_api_key_configured: false,
            ..AppSettings::default()
        };
        fs::write(&path, serde_json::to_string_pretty(&legacy).expect("serialize legacy settings"))
            .expect("seed legacy settings");

        let loaded = AppSettings::load(&path);

        assert_eq!(loaded.language, "ckb");
        assert!(loaded.llm_api_key.is_empty());
        assert!(loaded.llm_api_key_configured);

        let saved = fs::read_to_string(&path).expect("read scrubbed settings");
        assert!(!saved.contains("legacy-secret-token"));
        assert!(saved.contains("\"llm_api_key_configured\": true"));
        assert!(!path.with_extension("json.tmp").exists());
        assert!(!backup_files_left(tmp.path()));
    }

    #[test]
    fn for_client_response_clears_session_secret_but_preserves_configured_flag() {
        let settings = AppSettings {
            llm_api_key: "session-secret-token".to_string(),
            llm_api_key_configured: false,
            ..AppSettings::default()
        };

        let client = settings.for_client_response();

        assert_eq!(settings.llm_api_key, "session-secret-token");
        assert!(client.llm_api_key.is_empty());
        assert!(client.llm_api_key_configured);
    }

    #[test]
    fn merge_session_secret_keeps_existing_key_for_sanitized_client_update() {
        let current = AppSettings {
            llm_api_key: "session-secret-token".to_string(),
            llm_api_key_configured: true,
            ..AppSettings::default()
        };
        let mut incoming = AppSettings {
            language: "ckb".to_string(),
            llm_api_key: String::new(),
            llm_api_key_configured: true,
            ..AppSettings::default()
        };

        incoming.merge_session_secret_from(&current);

        assert_eq!(incoming.llm_api_key, "session-secret-token");
        assert!(incoming.llm_api_key_configured);
    }

    #[test]
    fn merge_session_secret_allows_explicit_unconfigured_clear() {
        let current = AppSettings {
            llm_api_key: "session-secret-token".to_string(),
            llm_api_key_configured: true,
            ..AppSettings::default()
        };
        let mut incoming =
            AppSettings { llm_api_key: String::new(), llm_api_key_configured: false, ..AppSettings::default() };

        incoming.merge_session_secret_from(&current);

        assert!(incoming.llm_api_key.is_empty());
        assert!(!incoming.llm_api_key_configured);
    }

    #[test]
    fn source_reference_models_are_deduped_and_fall_back_to_jury_model() {
        let settings = AppSettings {
            jury_model: "gemini-custom".to_string(),
            source_reference_models: vec![
                " gemini-2.5-pro ".to_string(),
                "".to_string(),
                "gemini-2.5-pro".to_string(),
                "gemini-2.5-flash".to_string(),
            ],
            ..AppSettings::default()
        };

        assert_eq!(
            settings.source_reference_models(),
            vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()]
        );

        let fallback = AppSettings {
            jury_model: "gemini-custom".to_string(),
            source_reference_models: Vec::new(),
            ..AppSettings::default()
        };
        assert_eq!(fallback.source_reference_models(), vec!["gemini-custom".to_string()]);
    }

    fn backup_files_left(dir: &Path) -> bool {
        fs::read_dir(dir)
            .expect("read dir")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".replace-bak-"))
    }
}
