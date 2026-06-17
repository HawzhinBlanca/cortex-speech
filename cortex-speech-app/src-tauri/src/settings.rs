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
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum AutonLevel {
    /// Agent only annotates cards without committing any verdict.
    Observe,
    /// Agent stages verdicts; human confirms each one (default).
    #[default]
    Propose,
    /// Agent auto-commits agreements; asks for human confirmation only on edits/rejects.
    ActConfirm,
    /// Fully unattended — agent commits all verdicts without pausing.
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
            llm_system_prompt: default_llm_system_prompt(),
            llm_model: default_llm_model(),
            external_asr_script_path: "".to_string(),
            jury_cloud_opt_in: false,
            jury_model: default_jury_model(),
            source_reference_models: default_source_reference_models(),
            jury_self_consistency_n: default_jury_self_consistency_n(),
            jury_autonomy_level: AutonLevel::default(),
            jury_t1_threshold: default_jury_t1_threshold(),
        }
    }
}

impl AppSettings {
    pub fn load(path: &std::path::Path) -> Self {
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
        if self.llm_mode == LlmMode::Gemini && !self.cloud_llm_opt_in {
            LlmMode::None
        } else {
            self.llm_mode.clone()
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
