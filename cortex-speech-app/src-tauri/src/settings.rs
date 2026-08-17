use crate::atomic_file::{remove_file_on_error, replace_file};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub model_dir: PathBuf,
    pub output_dir: PathBuf,
    pub asr_model_size: AsrModelSize,
    pub vad_threshold: f32,
    pub min_segment_duration_ms: u32,
    pub max_segment_duration_ms: u32,
    pub num_asr_threads: u32,
    pub enable_gpu: bool,
    /// Run the three EXTRA local engines (CTC-300M, CTC-1B, fine-tuned MMS) on every imported clip
    /// to store their hypotheses beside the champion's.
    ///
    /// Their only live consumer is the jury's auto-accept scoring. The consensus card that also read
    /// them was removed 2026-08-13 (measured: an ability-weighted blend of engines at 19-40% CER
    /// against a champion at 10.6%, and the same fusion changed 0 of 135 clips on the owner's audio),
    /// and the accept-provenance gate needs only the champion's own hypothesis.
    ///
    /// What they cost, MEASURED on the 2026-08-13 import: this sherpa-onnx build has no GPU support
    /// ("Fallback to cpu!"), so TWO billion-parameter models decode every clip on the CPU — 156,000
    /// CPU-seconds and ~13.6 cores pinned for 2.28 hours of audio, sustaining 2.5 clips/minute while
    /// the champion sat on the GPU at 5% utilisation. That is 84 minutes of wall clock per audio-hour,
    /// against roughly 6 for the champion alone. On a 34-hour corpus the difference is 47 hours of
    /// import versus about 6, with the app closed and every reviewer locked out for the duration.
    ///
    /// Defaults to FALSE: the fine-tuned OmniASR-7B champion is the sole normal transcription path.
    /// Smaller engines are an explicit experiment, never background work that silently changes the
    /// production workload or evidence mix.
    #[serde(default = "default_multi_engine_hypotheses")]
    pub multi_engine_hypotheses: bool,
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
    /// Consent gate for explicit per-segment ElevenLabs Scribe tools. Imports never consult this
    /// setting and always use the configured primary ASR. An explicit Scribe action sends only the
    /// selected clip to ElevenLabs' API. Off by default.
    #[serde(default)]
    pub cloud_stt_opt_in: bool,
    /// App-owned supervision of the champion 7B WSL server (engine_runtime): the app holds the server
    /// as an owned child and auto-restarts it per engine_supervisor's backoff/breaker policy, killing
    /// it on app exit. OFF by default — enabling auto-loads a ~30 GB model server (owner decision).
    #[serde(default)]
    pub champion_supervision_enabled: bool,
    /// Second-directory backup (Week-2 storage durability): when set to an absolute directory (ideally
    /// on ANOTHER drive), the periodic snapshot thread also rotates snapshots into
    /// `<backup_second_dir>/snapshots/` — so losing the data dir/disk no longer loses the backups too.
    /// Empty (default) = off. Re-read every snapshot interval; no restart needed.
    #[serde(default)]
    pub backup_second_dir: String,
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
    /// Transport for the T2 audio judge: "gemini" (default — Google's direct generativelanguage REST)
    /// or "openrouter" (OpenAI-compatible endpoint reaching the same model as
    /// `google/gemini-2.5-pro`, sidestepping the direct-Gemini 429 quota). Uses the OpenRouter key
    /// from secrets.env; still gated by `jury_cloud_opt_in`; falls back to direct Gemini if no
    /// OpenRouter key is present.
    ///
    /// POLICY (owner decision 2026-07-14): the ONLY approved cloud ASR judge for Central Kurdish is
    /// **Gemini 2.5 Pro** (ElevenLabs Scribe is the only other approved cloud STT). Qwen-family ASR is
    /// measured-bad on Sorani (no ckb support — see PROGRESS_LEDGER 2026 sweep) — do NOT point
    /// `jury_model` at it. Any future judge model needs a measured ckb CER on the frozen gold set first.
    #[serde(default = "default_jury_provider")]
    pub jury_provider: String,
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
    /// previously corrected (and independently confirmed) confusion is fixed automatically. Default OFF.
    /// CRITICAL (M2): This setting MUST remain disabled until LOOP-0 shadow mode is implemented.
    /// Currently, enabling it would apply corrections without logging would-fire events — violating
    /// the requirement that LOOP-0 only activates AFTER measured over-trigger = 0 (C5). Do not enable
    /// this in production until M2's shadow-mode instrumentation lands.
    #[serde(default)]
    pub loop0_firing_enabled: bool,

    /// Optional embedded MMS-CTC engine (measured 21.0% CER [19.93, 22.04], N=900). It may act as the
    /// primary only when a non-champion local engine is selected; it can never override WSL7B. Default
    /// OFF. This is a separate MMS model, not the fine-tuned OmniASR-7B + LoRA champion.
    #[serde(default)]
    pub use_finetuned_asr: bool,

    /// Opt-in: prime LLM refinement with the diverse N-best hypotheses + relevant past corrections
    /// (generative error correction) instead of plain single-string refinement. Default OFF — it
    /// changes the refinement prompt, so it is a deliberate, validate-on-your-data choice.
    #[serde(default)]
    pub ger_refinement_enabled: bool,

    /// Opt-in: persist the EM-fitted per-model IRT abilities and warm-start the next T0 consensus from
    /// them, so the jury learns each engine's real strength over time instead of restarting from the
    /// hardcoded priors every run. Default OFF — when off, T0 behavior is byte-identical to the
    /// hardcoded-prior path (no persistence, no warm-start).
    #[serde(default)]
    pub irt_ability_learning_enabled: bool,
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

fn default_multi_engine_hypotheses() -> bool {
    false
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
fn default_jury_provider() -> String {
    "gemini".to_string()
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
            // Accuracy is the product contract: the fine-tuned OmniASR-7B + LoRA champion is the sole
            // main/default drafter. If its WSL client is not configured or the server is unavailable,
            // transcription fails closed with an actionable error; it never silently substitutes 300M,
            // 1B, MMS, or a cloud engine.
            asr_model_size: AsrModelSize::WSL7B,
            vad_threshold: 0.5,
            min_segment_duration_ms: 3000,
            max_segment_duration_ms: 15000,
            num_asr_threads: 4,
            enable_gpu: true,
            multi_engine_hypotheses: default_multi_engine_hypotheses(),
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
            champion_supervision_enabled: false,
            backup_second_dir: String::new(),
            llm_system_prompt: default_llm_system_prompt(),
            llm_model: default_llm_model(),
            external_asr_script_path: "".to_string(),
            jury_cloud_opt_in: false,
            jury_model: default_jury_model(),
            jury_provider: default_jury_provider(),
            source_reference_models: default_source_reference_models(),
            jury_self_consistency_n: default_jury_self_consistency_n(),
            jury_autonomy_level: AutonLevel::default(),
            jury_t1_threshold: default_jury_t1_threshold(),
            loop0_firing_enabled: false,
            use_finetuned_asr: false,
            ger_refinement_enabled: false,
            irt_ability_learning_enabled: false,
        }
    }
}

/// Validate an outbound HTTP endpoint against the app's allow-list: at most 2048 chars and either an
/// `https://` URL or a localhost `http://` address; empty is rejected (an outbound POST needs a real
/// URL). Shared so every outbound channel — the LLM endpoint and the DPO export — enforces the same
/// rule. The Rust IPC layer is the trust boundary, so a malicious/XSS-planted argument cannot repoint
/// a request (and its payload) at an attacker-controlled host.
/// Whether an http(s) endpoint's HOST is a loopback address. Parses the host EXACTLY (authority,
/// minus any userinfo / port / IPv6 brackets) rather than a string prefix, so an attacker-controlled
/// name like `localhost.evil.com`, `127.0.0.1.evil.com`, or the `http://localhost@evil.com` userinfo
/// trick is NOT mistaken for the local machine (which would bypass the cloud consent gate / SSRF
/// allow-list). Returns false for a non-http(s) or unparseable endpoint (fail closed).
pub fn endpoint_host_is_loopback(endpoint: &str) -> bool {
    let lower = endpoint.trim().to_ascii_lowercase();
    let Some(after_scheme) = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) else {
        return false;
    };
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or(authority); // drop any userinfo
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        // Bracketed IPv6: the ']' must be followed by nothing or ':port'. Reject junk after it (e.g.
        // `[::1].evil.com`) — otherwise the bracket trick smuggles an attacker host past the loopback
        // check. `rest` is "::1].evil.com" / "::1]:8080" / "::1]".
        let Some(close) = rest.find(']') else {
            return false; // no closing bracket -> malformed, fail closed
        };
        let after = &rest[close + 1..];
        if !after.is_empty() && !after.starts_with(':') {
            return false;
        }
        &rest[..close] // inner host, e.g. ::1
    } else {
        hostport.split(':').next().unwrap_or(hostport) // host:port -> host
    };
    host == "localhost" || host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
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
    // A plain-http endpoint is only allowed if its host is genuinely loopback (exact parse).
    let is_localhost = lower.starts_with("http://") && endpoint_host_is_loopback(trimmed);
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
            Ok(s) => {
                // Tolerate a leading UTF-8 BOM: a settings.json saved by e.g. PowerShell's
                // `Out-File -Encoding utf8` (the owner's primary shell) carries a U+FEFF that serde_json
                // does NOT skip, which would otherwise silently discard EVERY persisted setting — engine
                // selection, WSL script path, thresholds, consent flags — and fall back to defaults.
                let parseable = s.strip_prefix('\u{feff}').unwrap_or(&s);
                match serde_json::from_str::<AppSettings>(parseable) {
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
                        // A hand-edited file bypasses update_settings' validate() gate, so degenerate
                        // numeric knobs (min=max=0 → one chunk per PCM SAMPLE; num_asr_threads=0 → ONNX
                        // config) must be repaired HERE. Repair only the bad knobs — hard-failing load
                        // would brick startup, and falling back to full defaults would silently drop
                        // consent flags and the output dir over one bad number.
                        settings.repair_out_of_range_numeric_knobs();
                        settings
                    }
                    Err(e) => {
                        // The file EXISTS but is unparseable (bad hand-edit, UTF-16, truncation). Preserve
                        // it BEFORE returning defaults so the next update_settings save cannot overwrite —
                        // and permanently destroy — the still-recoverable original.
                        tracing::warn!("Failed to parse settings file at {}: {}; using defaults", path.display(), e);
                        Self::preserve_unparseable_settings(path);
                        Self::default()
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!("Failed to read settings file at {}: {}; using defaults", path.display(), e);
                Self::default()
            }
        }
    }

    /// Reset any out-of-range segment-duration / thread knob to its default, warning per field.
    /// Load-path counterpart of the same bounds validate() enforces at the update_settings trust
    /// boundary — a hand-edited settings.json never passes through validate(), and a degenerate
    /// value here explodes the chunk planner (min=max=0 → one chunk per PCM sample) or feeds 0
    /// threads to the ONNX session config.
    fn repair_out_of_range_numeric_knobs(&mut self) {
        // 1000 ms floor = the UI's whole-second unit; see the matching bound in validate(). A
        // sub-second value repaired-to-kept here would map to 0 in the seconds-based settings UI
        // and brick every subsequent save.
        const MIN_SEGMENT_MS: u32 = 1000;
        const MAX_SEGMENT_MS: u32 = 600_000;
        let defaults = Self::default();
        if self.min_segment_duration_ms < MIN_SEGMENT_MS || self.min_segment_duration_ms > MAX_SEGMENT_MS {
            tracing::warn!(
                "settings: min_segment_duration_ms {} out of [{MIN_SEGMENT_MS}, {MAX_SEGMENT_MS}]; reset to {}",
                self.min_segment_duration_ms,
                defaults.min_segment_duration_ms
            );
            self.min_segment_duration_ms = defaults.min_segment_duration_ms;
        }
        if self.max_segment_duration_ms < self.min_segment_duration_ms || self.max_segment_duration_ms > MAX_SEGMENT_MS
        {
            tracing::warn!(
                "settings: max_segment_duration_ms {} out of [min, {MAX_SEGMENT_MS}]; reset to {}",
                self.max_segment_duration_ms,
                defaults.max_segment_duration_ms.max(self.min_segment_duration_ms)
            );
            self.max_segment_duration_ms = defaults.max_segment_duration_ms.max(self.min_segment_duration_ms);
        }
        if self.num_asr_threads == 0 || self.num_asr_threads > 128 {
            tracing::warn!(
                "settings: num_asr_threads {} out of [1, 128]; reset to {}",
                self.num_asr_threads,
                defaults.num_asr_threads
            );
            self.num_asr_threads = defaults.num_asr_threads;
        }

        // The [0,1] gate/threshold knobs get the SAME load-path repair. validate() enforces these bounds
        // only on the update_settings trust boundary, so a hand-edited/legacy settings.json could carry a
        // threshold as a percentage ("max_wer_threshold": 30), a NaN, or a negative — and `wer > 30.0` is
        // then always false, SILENTLY disabling the export quality gate (exactly the honesty-rule
        // violation validate()'s own comment warns about). Reset any non-finite / out-of-[0,1] value to
        // its default; a valid threshold is left untouched.
        let repair_rate = |name: &str, cur: &mut f64, def: f64| {
            if !cur.is_finite() || !(0.0..=1.0).contains(&*cur) {
                tracing::warn!("settings: {name} {} out of [0, 1]; reset to {def}", *cur);
                *cur = def;
            }
        };
        repair_rate("max_wer_threshold", &mut self.max_wer_threshold, defaults.max_wer_threshold);
        repair_rate("max_cer_threshold", &mut self.max_cer_threshold, defaults.max_cer_threshold);
        repair_rate("jury_t1_threshold", &mut self.jury_t1_threshold, defaults.jury_t1_threshold);
        // vad_threshold carries the same [0, 1] contract but is an f32, so it gets its own check.
        if !self.vad_threshold.is_finite() || !(0.0..=1.0).contains(&self.vad_threshold) {
            tracing::warn!(
                "settings: vad_threshold {} out of [0, 1]; reset to {}",
                self.vad_threshold,
                defaults.vad_threshold
            );
            self.vad_threshold = defaults.vad_threshold;
        }
    }

    /// Rename an unparseable settings file to `settings.json.corrupt-<epoch>` so a subsequent save of
    /// the defaults can never clobber the owner's recoverable original. Best-effort.
    fn preserve_unparseable_settings(path: &std::path::Path) {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut backup = path.as_os_str().to_owned();
        backup.push(format!(".corrupt-{ts}"));
        let backup = std::path::PathBuf::from(backup);
        match std::fs::rename(path, &backup) {
            Ok(()) => tracing::warn!("Preserved unparseable settings as {}", backup.display()),
            Err(e) => tracing::warn!("Could not preserve unparseable settings at {}: {e}", path.display()),
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
            // Match the host EXACTLY, not by string prefix — `starts_with("http://localhost")` let
            // `http://localhost.evil.com` / `http://127.0.0.1.evil.com` pass this entry-point validator
            // (update_settings is the webview trust boundary) and exfiltrate transcripts in cleartext.
            // Use the same authority-parsing helper the egress validator uses so both stay identical.
            let is_localhost = lower.starts_with("http://") && endpoint_host_is_loopback(endpoint);
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

        // Server-side bounds on the numeric gate/threshold knobs. Without this a webview payload
        // (XSS-planted or a hand-edited settings.json) could set e.g. `max_wer_threshold = NaN`,
        // and every `wer > threshold` comparison is then `false` — so ALL segments silently pass the
        // export quality gate that should have failed them. A non-finite / out-of-range threshold is
        // never a legitimate value, so reject it at the trust boundary rather than let it quietly
        // defeat the load-bearing gate (the project's honesty rule: a gate must never be silently
        // disabled). Rates that are compared as `metric > threshold` must be finite in [0, 1].
        for (name, value) in [
            ("max_wer_threshold", self.max_wer_threshold),
            ("max_cer_threshold", self.max_cer_threshold),
            ("jury_t1_threshold", self.jury_t1_threshold),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(AppError::Validation(format!("{name} must be a finite value in [0, 1]")));
            }
        }
        if !self.vad_threshold.is_finite() || !(0.0..=1.0).contains(&self.vad_threshold) {
            return Err(AppError::Validation("vad_threshold must be a finite value in [0, 1]".into()));
        }
        // Split ratios are relative weights (normalized downstream), so only require finite and
        // non-negative — a NaN/negative weight would corrupt the train/val/test partitioning.
        for (name, value) in [
            ("hf_train_ratio", self.hf_train_ratio),
            ("hf_val_ratio", self.hf_val_ratio),
            ("hf_test_ratio", self.hf_test_ratio),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(AppError::Validation(format!("{name} must be a finite value >= 0")));
            }
        }
        // Segment-duration and thread knobs feed the chunk planner and sherpa config directly.
        // With min=max=0 the planner's `.max(1)` floors both to ONE PCM SAMPLE, so every import
        // explodes into 16,000 segments per second of audio; num_asr_threads=0 flows unchecked
        // into the ONNX session config. Same trust boundary as the thresholds above: a webview
        // payload or hand-edited settings.json must not be able to plant these.
        // The 1000 ms floor is the UI's unit: settingsAdapter round-trips these as WHOLE SECONDS
        // (Math.round(ms/1000) → sec*1000), so a stored sub-second value maps to 0 on the next UI
        // save and would then be rejected here — bricking every settings save (adversarial review
        // 2026-07-11). Accept only values the UI round-trip preserves.
        const MIN_SEGMENT_MS: u32 = 1000;
        const MAX_SEGMENT_MS: u32 = 600_000; // 10 minutes — far beyond any sane review segment
        if self.min_segment_duration_ms < MIN_SEGMENT_MS || self.min_segment_duration_ms > MAX_SEGMENT_MS {
            return Err(AppError::Validation(format!(
                "min_segment_duration_ms must be in [{MIN_SEGMENT_MS}, {MAX_SEGMENT_MS}]"
            )));
        }
        if self.max_segment_duration_ms < self.min_segment_duration_ms || self.max_segment_duration_ms > MAX_SEGMENT_MS
        {
            return Err(AppError::Validation(format!(
                "max_segment_duration_ms must be in [min_segment_duration_ms, {MAX_SEGMENT_MS}]"
            )));
        }
        if self.num_asr_threads == 0 || self.num_asr_threads > 128 {
            return Err(AppError::Validation("num_asr_threads must be in [1, 128]".into()));
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

    /// True when the configured LLM endpoint targets the local machine (loopback) — a transcript
    /// sent there never leaves the device. An empty endpoint is treated as local (no outbound).
    pub fn llm_endpoint_is_local(&self) -> bool {
        let e = self.llm_endpoint.trim();
        // Empty = no outbound. Otherwise require the host to be EXACTLY loopback (parsed, not a string
        // prefix) so localhost.evil.com / 127.0.0.1.evil.com / localhost@evil.com can't bypass consent.
        e.is_empty() || endpoint_host_is_loopback(e)
    }

    pub fn effective_llm_mode(&self) -> LlmMode {
        // Cloud (Gemini) requires the explicit cloud-LLM opt-in.
        if self.llm_mode == LlmMode::Gemini && !self.cloud_llm_opt_in {
            return LlmMode::None;
        }
        // Round-22 #6: "Local" mode pointed at a REMOTE (non-loopback) endpoint sends the transcript
        // text (and the bearer API key) off the device, so it is effectively cloud and likewise
        // requires the cloud-LLM opt-in. A genuine on-device endpoint (e.g. Ollama at localhost) stays
        // on the machine and is always allowed. Without this, Local + an https remote would POST every
        // transcript with no consent gate.
        if self.llm_mode == LlmMode::Local && !self.cloud_llm_opt_in && !self.llm_endpoint_is_local() {
            return LlmMode::None;
        }
        self.llm_mode.clone()
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
    fn factory_default_is_the_finetuned_7b_champion_only() {
        let defaults = AppSettings::default();
        assert_eq!(defaults.asr_model_size, AsrModelSize::WSL7B);
        assert!(!defaults.multi_engine_hypotheses, "smaller-model hypotheses must be explicit opt-in");
    }

    #[test]
    fn load_tolerates_utf8_bom_instead_of_silently_resetting() {
        // A settings.json saved with a UTF-8 BOM (PowerShell Out-File default) must still parse — not
        // fall back to Default and silently swap the engine + every preference.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        let base = AppSettings { asr_model_size: AsrModelSize::CTC1B, ..AppSettings::default() };
        let json = format!("\u{feff}{}", serde_json::to_string(&base).unwrap());
        std::fs::write(&path, json).unwrap();
        let loaded = AppSettings::load(&path);
        assert_eq!(
            loaded.asr_model_size,
            AsrModelSize::CTC1B,
            "a BOM-prefixed settings.json must parse, not silently reset to defaults"
        );
    }

    #[test]
    fn unparseable_settings_are_preserved_not_destroyed() {
        // An unparseable file must be moved aside so a later save of the defaults cannot overwrite the
        // owner's still-recoverable original.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ not valid json ").unwrap();
        let _ = AppSettings::load(&path); // returns defaults
        assert!(!path.exists(), "the unparseable file is moved aside, not left in place");
        let preserved = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(preserved, 1, "the unparseable settings file is preserved as .corrupt-<ts>");
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
    fn validate_rejects_loopback_prefix_trick_over_plaintext_http() {
        // The entry-point validator (update_settings trust boundary) must match the host EXACTLY, not by
        // string prefix — otherwise http://localhost.evil.com / http://127.0.0.1.evil.com pass as
        // "localhost" over cleartext HTTP and exfiltrate transcripts. Mirrors validate_outbound_endpoint.
        for bad in [
            "http://localhost.evil.com/collect",
            "http://127.0.0.1.evil.com/collect",
            "http://localhost@evil.com/collect",
            "http://[::1].evil.com/x",
        ] {
            let s = AppSettings { llm_endpoint: bad.to_string(), ..AppSettings::default() };
            assert!(s.validate().is_err(), "loopback-prefix trick must be rejected by validate(): {bad:?}");
        }
    }

    #[test]
    fn validate_outbound_endpoint_enforces_https_or_localhost() {
        for ok in ["https://example.com/x", "http://localhost:8080", "http://127.0.0.1/y", "http://[::1]:9/z"] {
            assert!(super::validate_outbound_endpoint(ok).is_ok(), "{ok} should pass");
        }
        // Empty, non-https remote, non-http schemes, AND loopback-prefix/userinfo tricks are rejected.
        for bad in [
            "",
            "   ",
            "http://attacker.example.com/collect",
            "ftp://x",
            "file:///etc/passwd",
            "http://localhost.evil.com/collect",
            "http://127.0.0.1.evil.com/collect",
            "http://localhost@evil.com/collect",
        ] {
            assert!(super::validate_outbound_endpoint(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn endpoint_host_is_loopback_uses_exact_host_not_prefix() {
        for local in [
            "http://localhost:11434/v1",
            "https://localhost",
            "http://127.0.0.1/x",
            "http://[::1]:9/z",
            "https://127.0.0.5",
        ] {
            assert!(super::endpoint_host_is_loopback(local), "{local} should be loopback");
        }
        for remote in [
            "https://localhost.evil.com/v1",
            "http://localhost.evil.com/v1",
            "http://127.0.0.1.evil.com/v1",
            "http://localhost@evil.com/v1",
            "https://evil.com/localhost",
            "ftp://localhost",
            "",
        ] {
            assert!(!super::endpoint_host_is_loopback(remote), "{remote} must NOT be loopback");
        }
    }

    #[test]
    fn local_mode_to_remote_endpoint_requires_cloud_opt_in() {
        // Loopback "Local" never leaves the device — always allowed.
        let local = AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "http://localhost:11434/v1".into(),
            cloud_llm_opt_in: false,
            ..AppSettings::default()
        };
        assert_eq!(local.effective_llm_mode(), LlmMode::Local);

        // "Local" pointed at a REMOTE https host sends the transcript off-device: without the
        // cloud-LLM opt-in it must downgrade to None (no outbound call constructed).
        let remote_no_optin = AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "https://gateway.example.com/v1/chat/completions".into(),
            cloud_llm_opt_in: false,
            ..AppSettings::default()
        };
        assert_eq!(remote_no_optin.effective_llm_mode(), LlmMode::None);

        // A loopback-PREFIX attacker host must NOT be mistaken for local (the consent-gate bypass).
        let prefix_trick = AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "https://localhost.evil.com/v1/chat/completions".into(),
            cloud_llm_opt_in: false,
            ..AppSettings::default()
        };
        assert_eq!(prefix_trick.effective_llm_mode(), LlmMode::None);

        // With the opt-in, the same remote Local endpoint is permitted.
        let remote_opted = AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "https://gateway.example.com/v1/chat/completions".into(),
            cloud_llm_opt_in: true,
            ..AppSettings::default()
        };
        assert_eq!(remote_opted.effective_llm_mode(), LlmMode::Local);
    }

    #[test]
    fn endpoint_host_is_loopback_is_strict_about_egress() {
        for local in [
            "http://localhost:11434/v1/chat/completions",
            "http://127.0.0.1:8080/x",
            "https://localhost/x",
            "http://[::1]:9/z",
        ] {
            assert!(super::endpoint_host_is_loopback(local), "{local} is local");
        }
        for remote in [
            "https://api.openai.com/v1/chat/completions",
            "http://localhost.attacker.example/exfil", // must NOT be treated as local
            "https://127.0.0.1.attacker.example/x",
            "ftp://localhost",
        ] {
            assert!(!super::endpoint_host_is_loopback(remote), "{remote} is NOT local");
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
    fn validate_rejects_non_finite_or_out_of_range_gate_thresholds() {
        // The default settings must pass — the guard must not reject legitimate values.
        assert!(AppSettings::default().validate().is_ok(), "default thresholds must validate");

        // A NaN quality-gate threshold makes every `wer > threshold` comparison false, so ALL segments
        // silently pass the export gate. It must be rejected at the trust boundary, not accepted.
        for bad in [f64::NAN, f64::INFINITY, -0.1, 1.5] {
            let s = AppSettings { max_wer_threshold: bad, ..AppSettings::default() };
            assert!(
                matches!(s.validate(), Err(crate::error::AppError::Validation(_))),
                "max_wer_threshold {bad} must be rejected (out of [0,1] / non-finite)"
            );
            let s = AppSettings { max_cer_threshold: bad, ..AppSettings::default() };
            assert!(matches!(s.validate(), Err(crate::error::AppError::Validation(_))), "max_cer_threshold {bad}");
            let s = AppSettings { jury_t1_threshold: bad, ..AppSettings::default() };
            assert!(matches!(s.validate(), Err(crate::error::AppError::Validation(_))), "jury_t1_threshold {bad}");
        }

        // vad_threshold (f32) same contract.
        for bad in [f32::NAN, f32::INFINITY, -1.0, 2.0] {
            let s = AppSettings { vad_threshold: bad, ..AppSettings::default() };
            assert!(matches!(s.validate(), Err(crate::error::AppError::Validation(_))), "vad_threshold {bad}");
        }

        // Split ratios: finite and non-negative (they are normalized downstream, so >1 is fine).
        for bad in [f64::NAN, f64::INFINITY, -0.5] {
            let s = AppSettings { hf_train_ratio: bad, ..AppSettings::default() };
            assert!(matches!(s.validate(), Err(crate::error::AppError::Validation(_))), "hf_train_ratio {bad}");
        }
    }

    #[test]
    fn validate_rejects_degenerate_segment_durations_and_thread_count() {
        // min=max=0 floors the chunk planner to ONE PCM SAMPLE per chunk (chunking.rs `.max(1)`),
        // exploding every import into 16k segments/second; num_asr_threads=0 flows into the ONNX
        // session config. All three arrive from the same webview/hand-edited-json trust boundary
        // as the thresholds above and must be bounded there.
        assert!(AppSettings::default().validate().is_ok(), "defaults must validate");

        // 999 covers the sub-second band the UI's whole-second round-trip turns into 0 (accepting
        // it on write would brick every later save — adversarial review 2026-07-11).
        for (min_ms, max_ms) in [(0, 0), (0, 15_000), (999, 15_000), (3_000, 0), (3_000, 2_999), (700_000, 700_000)] {
            let s = AppSettings {
                min_segment_duration_ms: min_ms,
                max_segment_duration_ms: max_ms,
                ..AppSettings::default()
            };
            assert!(
                matches!(s.validate(), Err(crate::error::AppError::Validation(_))),
                "segment durations min={min_ms} max={max_ms} must be rejected"
            );
        }

        for bad_threads in [0u32, 129] {
            let s = AppSettings { num_asr_threads: bad_threads, ..AppSettings::default() };
            assert!(
                matches!(s.validate(), Err(crate::error::AppError::Validation(_))),
                "num_asr_threads {bad_threads} must be rejected"
            );
        }
    }

    #[test]
    fn load_repairs_degenerate_numeric_knobs_from_hand_edited_file() {
        // A hand-edited settings.json bypasses update_settings' validate() gate entirely, so the
        // load path must repair degenerate knobs itself — while PRESERVING every other persisted
        // value (repairing must not become a stealth reset of consent flags or the output dir).
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let hand_edited = AppSettings {
            min_segment_duration_ms: 0,
            max_segment_duration_ms: 0,
            num_asr_threads: 0,
            language: "ckb".to_string(),
            ..AppSettings::default()
        };
        // save() persists as-is (it doesn't validate) — exactly the hand-edit analogue.
        hand_edited.save(&path).expect("seed hand-edited settings");

        let loaded = AppSettings::load(&path);
        let defaults = AppSettings::default();
        assert_eq!(loaded.min_segment_duration_ms, defaults.min_segment_duration_ms, "min repaired to default");
        assert_eq!(loaded.max_segment_duration_ms, defaults.max_segment_duration_ms, "max repaired to default");
        assert_eq!(loaded.num_asr_threads, defaults.num_asr_threads, "threads repaired to default");
        assert_eq!(loaded.language, "ckb", "unrelated persisted values must survive the repair");
        assert!(loaded.validate().is_ok(), "repaired settings must pass the update-path validator");
    }

    #[test]
    fn repair_resets_out_of_range_quality_gate_thresholds() {
        // The [0,1] gate/threshold knobs get the same load-path repair as the segment/thread knobs above.
        // A hand-edited/legacy settings.json bypasses validate(), so a threshold written as a percentage
        // (30), a NaN, or a negative would otherwise survive load and make `wer > threshold` always false
        // — silently disabling the export quality gate (the honesty rule). Tested on repair() directly
        // because NaN does not round-trip through JSON (serde emits null).
        let mut s = AppSettings {
            max_wer_threshold: 30.0,     // a percentage, not the [0,1] fraction the gate needs
            max_cer_threshold: f64::NAN, // non-finite
            jury_t1_threshold: -1.0,     // below range
            vad_threshold: 2.0,          // above range
            language: "ckb".to_string(),
            ..AppSettings::default()
        };
        s.repair_out_of_range_numeric_knobs();
        let d = AppSettings::default();
        assert_eq!(s.max_wer_threshold, d.max_wer_threshold, "percentage wer threshold repaired");
        assert_eq!(s.max_cer_threshold, d.max_cer_threshold, "NaN cer threshold repaired");
        assert_eq!(s.jury_t1_threshold, d.jury_t1_threshold, "negative jury threshold repaired");
        assert_eq!(s.vad_threshold, d.vad_threshold, "above-range vad threshold repaired");
        assert_eq!(s.language, "ckb", "unrelated values untouched");
        assert!(s.validate().is_ok(), "repaired settings pass the update-path validator");
        // A valid in-range threshold must NOT be touched.
        let mut ok = AppSettings { max_wer_threshold: 0.15, ..AppSettings::default() };
        ok.repair_out_of_range_numeric_knobs();
        assert_eq!(ok.max_wer_threshold, 0.15, "a valid threshold is left as-is");
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
        assert_eq!(loaded.asr_model_size, AsrModelSize::WSL7B);
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
