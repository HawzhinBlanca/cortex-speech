use crate::atomic_file::{remove_file_on_error, replace_file};
use crate::settings::AsrModelSize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static USER_MODELS_DIR: OnceLock<PathBuf> = OnceLock::new();

// Model integrity pinning has two independent layers:
//   1. Archive pins (`*_ARCHIVE_SHA256`) gate the FIRST-RUN download:
//      `ensure_pinned_sha256` refuses to fetch an archive whose hash is unpinned,
//      and `verify_sha256` checks the downloaded `.tar.bz2` before extraction.
//      These stay empty until the canonical archive hash is recorded — the
//      archives are deleted after extraction, so the hash cannot be recovered
//      from the on-disk extracted files; it needs a download-and-hash pass or an
//      upstream-published checksum. While empty, `model_download_supported`
//      reports the model as not auto-downloadable instead of fetching it unverified.
//   2. Extracted-file pins (`MODELS[].sha256`) verify the unpacked `.onnx`/tokens
//      after extraction and CAN be computed directly from on-disk models. A
//      mismatch fails the install (`verify_extracted_against_pin`); an empty pin
//      is treated as "not yet pinned" and accepted.

/// Subdirectory under the models root for Meta OmniASR CTC 300M (sherpa-onnx bundle).
pub const OMNIASR_CTC_300M_DIR: &str = "omniasr-ctc-300m";
pub const OMNIASR_CTC_300M_MODEL: &str = "omniasr-ctc-300m/model.int8.onnx";
pub const OMNIASR_CTC_300M_TOKENS: &str = "omniasr-ctc-300m/tokens.txt";
pub const OMNIASR_CTC_300M_ARCHIVE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-int8-2025-11-12.tar.bz2";
pub const OMNIASR_CTC_300M_ARCHIVE_SHA256: &str = "";

/// Subdirectory under the models root for Meta OmniASR CTC 1B (sherpa-onnx bundle).
pub const OMNIASR_CTC_1B_DIR: &str = "omniasr-ctc-1b";
pub const OMNIASR_CTC_1B_MODEL: &str = "omniasr-ctc-1b/model.int8.onnx";
pub const OMNIASR_CTC_1B_TOKENS: &str = "omniasr-ctc-1b/tokens.txt";
pub const OMNIASR_CTC_1B_ARCHIVE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-omnilingual-asr-1600-languages-1B-ctc-int8-2025-11-12.tar.bz2";
// Pinned from the real 786 MB archive (downloaded + extracted; the extracted model.int8.onnx/tokens
// matched the pins below, authenticating the archive). Non-empty → the in-app CTC-1B download is
// unblocked (model_download_supported) and the archive is hash-verified before extraction.
pub const OMNIASR_CTC_1B_ARCHIVE_SHA256: &str = "27c270dfe9bc1abb35fef62c396b373577ffc55a272cb039d08487c27b0ecfaa";

/// CAM++ speaker embedding model. The sherpa-onnx speaker-recognition assets are single `.onnx` files
/// (NOT tar.bz2 bundles), so this is a direct-file download like Silero VAD — the 192-dim zh+en
/// "advanced" CAM++ voiceprint. Speaker embeddings are language-agnostic, so it serves Kurdish
/// diarization fine. (The previous tar.bz2 URL 404'd — dead link.)
pub const CAMPP_DIR: &str = "campp";
pub const CAMPP_MODEL: &str = "campp/model.onnx";
pub const CAMPP_MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx";
pub const CAMPP_MODEL_SHA256: &str = "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2";

/// AI Audio Denoiser — sherpa-onnx GTCRN (the architecture denoiser.rs loads via `gtcrn.model`). A
/// single ~0.5 MB `.onnx`, direct-file download. (The previous tar.bz2 URL 404'd — dead link.)
pub const DENOISER_DIR: &str = "denoiser";
pub const DENOISER_MODEL: &str = "denoiser/model.onnx";
pub const DENOISER_MODEL_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/gtcrn_simple.onnx";
pub const DENOISER_MODEL_SHA256: &str = "e77603ac0c23dac3227dd2d7135b3a585cbee2679048aecfa886657d3ae1b534";

/// Bundled models shipped with the app / used during development.
pub fn bundled_models_dir() -> PathBuf {
    select_bundled_models_dir(bundled_model_dir_candidates())
}

/// Register the user AppData models directory (call once at startup).
pub fn init_user_models_dir(dir: PathBuf) {
    let _ = USER_MODELS_DIR.set(dir);
}

/// Directory containing ONNX weights: user AppData first, then bundled fallback.
pub fn active_models_dir() -> PathBuf {
    USER_MODELS_DIR.get().map(|d| resolve_models_dir(d)).unwrap_or_else(bundled_models_dir)
}

fn bundled_model_dir_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            candidates.push(exe_dir.join("models"));
            candidates.push(exe_dir.join("resources").join("models"));
            if let Some(parent) = exe_dir.parent() {
                candidates.push(parent.join("models"));
                candidates.push(parent.join("resources").join("models"));
            }
        }
    }

    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join("models"));
        candidates.push(current_dir.join("src-tauri").join("models"));
    }

    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("models"));
    dedupe_paths(candidates)
}

fn select_bundled_models_dir(candidates: Vec<PathBuf>) -> PathBuf {
    let fallback = candidates.first().cloned().unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("models"));
    candidates
        .into_iter()
        .find(|candidate| omniasr_ctc_300m_present_in(candidate) || omniasr_ctc_1b_present_in(candidate))
        .unwrap_or(fallback)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.iter().any(|existing: &PathBuf| existing == &path) {
            unique.push(path);
        }
    }
    unique
}

/// Prefer `user_dir` when OmniASR model and tokens exist there; else bundled dev models.
pub fn resolve_models_dir(user_dir: &Path) -> PathBuf {
    if omniasr_ctc_300m_present_in(user_dir) || omniasr_ctc_1b_present_in(user_dir) {
        return user_dir.to_path_buf();
    }
    let bundled = bundled_models_dir();
    if omniasr_ctc_300m_present_in(&bundled) || omniasr_ctc_1b_present_in(&bundled) {
        return bundled;
    }
    user_dir.to_path_buf()
}

pub struct ModelInfo {
    pub name: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub min_size_bytes: u64,
    pub version: &'static str,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        // Round-24 #8: the download URL must point at an IMMUTABLE ref whose bytes BOTH match the pin
        // AND expose the ONNX interface the app's VAD code requires — a unified `state`/`stateN` tensor
        // (audio.rs `inputs!["input","sr","state"]`, `outputs["stateN"]`), NOT the separate-h/c LSTM
        // interface. The old `raw/master/...` URL was a mutable branch ref (the original reliability
        // bug). It currently still serves the correct model (sha 1a153a22…, 2.3 MB, unified-state), but
        // a branch can change under us at any time. Pin to the immutable COMMIT that serves that exact
        // file — same bytes as the pin and the bundled copy, so existing installs and the interface are
        // unaffected. (The official v4.0-release file uses the incompatible h/c interface, so it must
        // NOT be used here despite being labelled "v4".)
        name: "Silero VAD v4",
        filename: "silero_vad_v4.onnx",
        url: "https://github.com/snakers4/silero-vad/raw/bfdc0193023f121ea5b3cc7b176dbed570a68a59/src/silero_vad/data/silero_vad.onnx",
        sha256: "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3",
        min_size_bytes: 1_000_000,
        version: "4.0",
    },
    ModelInfo {
        name: "Meta OmniASR CTC 300M (model)",
        filename: OMNIASR_CTC_300M_MODEL,
        url: "",
        sha256: "e7c4e54ee4c4c47829cc6667d5d00ed8ea7bef1dcfeef0fce766f77752a2726c",
        min_size_bytes: 50_000_000,
        version: "2.0",
    },
    ModelInfo {
        name: "Meta OmniASR CTC 300M (tokens)",
        filename: OMNIASR_CTC_300M_TOKENS,
        url: "",
        sha256: "a7a044c52cb29cbe8b0dc1953e92cefd4ca16b0ed968177b6beab21f9a7d0b31",
        min_size_bytes: 100,
        version: "2.0",
    },
    ModelInfo {
        name: "Meta OmniASR CTC 1B (model)",
        filename: OMNIASR_CTC_1B_MODEL,
        url: "",
        sha256: "f7b74c964039162423b83e3fa950ce24810c9a635d9ff8468b5f4d142b7c1e8c",
        min_size_bytes: 500_000_000,
        version: "2.0",
    },
    ModelInfo {
        name: "Meta OmniASR CTC 1B (tokens)",
        filename: OMNIASR_CTC_1B_TOKENS,
        url: "",
        sha256: "a7a044c52cb29cbe8b0dc1953e92cefd4ca16b0ed968177b6beab21f9a7d0b31",
        min_size_bytes: 100,
        version: "2.0",
    },
    ModelInfo {
        name: "CAM++ Speaker Embedding",
        filename: CAMPP_MODEL,
        // Literal url/sha256 (matches CAMPP_MODEL_URL/CAMPP_MODEL_SHA256) — the provenance policy audits
        // the hex directly in the ModelInfo block, like Silero/OmniASR above.
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx",
        sha256: "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2",
        min_size_bytes: 10_000_000,
        version: "1.0",
    },
    ModelInfo {
        name: "AI Audio Denoiser",
        filename: DENOISER_MODEL,
        // Literal url/sha256 (matches DENOISER_MODEL_URL/DENOISER_MODEL_SHA256).
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/gtcrn_simple.onnx",
        sha256: "e77603ac0c23dac3227dd2d7135b3a585cbee2679048aecfa886657d3ae1b534",
        // GTCRN is ~0.5 MB — a 10 MB floor (the old value) rejected the correct model. Guard against a
        // truncated download without excluding the real file.
        min_size_bytes: 400_000,
        version: "1.0",
    },
    ModelInfo {
        name: "WavLM Speech OOD Detector",
        filename: "wavlm_ood.onnx",
        url: "",
        sha256: "",
        min_size_bytes: 5_000_000,
        version: "1.0",
    },
];

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    if expected.is_empty() {
        remove_model_temp_file(path, "unpinned model download");
        return Err(missing_pinned_sha256_error("model download"));
    }
    let mut file = std::fs::File::open(path).map_err(|e| format!("Open for hash: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("Hash read: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hash = hex_lower(&hasher.finalize());
    if hash != expected {
        remove_model_temp_file(path, "SHA256-mismatched model download");
        return Err(format!("SHA256 mismatch: expected {expected}, got {hash}"));
    }
    Ok(())
}

fn ensure_pinned_sha256(label: &str, expected: &str) -> Result<(), String> {
    if expected.is_empty() {
        return Err(missing_pinned_sha256_error(label));
    }
    Ok(())
}

fn missing_pinned_sha256_error(label: &str) -> String {
    format!("Missing pinned SHA256 for {label}; refusing to download unverifiable artifact")
}

/// Verifies a freshly-extracted/installed model file's computed SHA256 against its
/// pinned value (`MODELS[].sha256`). An empty pin means "not yet pinned" and is
/// accepted as a no-op so archive-sourced files without an on-disk-computed pin
/// still install; a non-empty pin that mismatches fails the install so a corrupted
/// or tampered extraction is caught at the point of install rather than at runtime.
fn verify_extracted_against_pin(filename: &str, computed: &str, pinned: &str) -> Result<(), String> {
    if pinned.is_empty() {
        return Ok(());
    }
    if computed != pinned {
        return Err(format!(
            "Installed model {filename} failed integrity check: expected SHA256 {pinned}, got {computed}"
        ));
    }
    Ok(())
}

/// Runtime integrity check for a model file already on disk. Unlike `verify_sha256` (the
/// download path, which DELETES a mismatched file), this is NON-destructive: a tampered or
/// wrong-version model at load time must be refused, not silently removed. The expected digest
/// is the pinned `MODELS[].sha256` for `pin_filename`; an empty pin means "not pinned" and is a
/// no-op (campp/denoiser/wavlm_ood are unpinned today). Returns Err with both digests on
/// mismatch so a swapped/corrupted `.onnx` is rejected at load (charter M2.3: "a tampered ONNX
/// fails the runtime manifest check").
pub fn verify_model_path_runtime(path: &Path, pin_filename: &str) -> Result<(), String> {
    let pinned = MODELS.iter().find(|m| m.filename == pin_filename).map(|m| m.sha256).unwrap_or("");
    if pinned.is_empty() {
        return Ok(()); // not pinned -> cannot verify, do not block load
    }
    let actual = compute_file_sha256(path)?;
    if actual != pinned {
        return Err(format!(
            "Model integrity check FAILED for {pin_filename}: expected SHA256 {pinned}, got {actual} (tampered or wrong version) - refusing to load"
        ));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct ModelMeta {
    pub filename: String,
    pub downloaded_at: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub version: String,
}

#[derive(Clone)]
pub struct ModelManager {
    pub models_dir: PathBuf,
}

impl ModelManager {
    pub fn new(models_dir: PathBuf) -> Self {
        Self { models_dir }
    }

    pub fn ensure_dir(&self) -> Result<(), String> {
        fs::create_dir_all(&self.models_dir).map_err(|e| format!("Failed to create models directory: {e}"))
    }

    pub fn missing_models(&self) -> Vec<&ModelInfo> {
        let model_dir = self.resolved_dir();
        MODELS
            .iter()
            .filter(|m| {
                let (_, size) = model_file_state(&model_dir, m.filename);
                size.unwrap_or(0) < m.min_size_bytes
            })
            .collect()
    }

    pub fn missing_required_model_names(&self) -> Vec<&'static str> {
        missing_required_model_names_in(&self.resolved_dir())
    }

    pub fn missing_optional_model_names(&self) -> Vec<&'static str> {
        let model_dir = self.resolved_dir();
        let has_any_asr = omniasr_ctc_300m_present_in(&model_dir) || omniasr_ctc_1b_present_in(&model_dir);
        self.missing_models()
            .into_iter()
            .filter(|model| {
                model.filename != "silero_vad_v4.onnx" && (has_any_asr || !model.filename.starts_with("omniasr-ctc-"))
            })
            .map(|model| model.name)
            .collect()
    }

    pub fn downloadable_missing_models(&self) -> Vec<&ModelInfo> {
        let model_dir = self.resolved_dir();
        MODELS
            .iter()
            .filter(|model| !model_available_in(&model_dir, model) && model_download_supported(model))
            .collect()
    }

    pub fn all_models_present(&self) -> bool {
        self.missing_required_model_names().is_empty()
    }

    pub fn model_path(&self, filename: &str) -> PathBuf {
        let path = self.models_dir.join(filename);
        ensure_model_parent_dir(&path);
        path
    }

    /// Resolved directory where inference loads ONNX files from.
    pub fn resolved_dir(&self) -> PathBuf {
        resolve_models_dir(&self.models_dir)
    }

    fn meta_path(&self) -> PathBuf {
        self.models_dir.join("models_meta.json")
    }

    pub fn save_meta(&self, entries: &[ModelMeta]) -> Result<(), String> {
        let json = serde_json::to_string_pretty(entries).map_err(|e| format!("Serialize meta: {e}"))?;
        let meta_path = self.meta_path();
        let tmp_path = meta_path.with_extension("json.tmp");
        remove_file_on_error(
            &tmp_path,
            (|| -> Result<(), String> {
                std::fs::write(&tmp_path, &json).map_err(|e| format!("Write meta tmp: {e}"))?;
                replace_file(&tmp_path, &meta_path).map_err(|e| format!("Replace meta: {e}"))?;
                Ok(())
            })(),
        )
    }

    pub fn load_meta(&self) -> Result<Vec<ModelMeta>, String> {
        let content = std::fs::read_to_string(self.meta_path()).map_err(|e| format!("Read meta: {e}"))?;
        serde_json::from_str(&content).map_err(|e| format!("Parse meta: {e}"))
    }

    fn load_meta_for_update(&self) -> Vec<ModelMeta> {
        match std::fs::read_to_string(self.meta_path()) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!("Failed to parse existing model metadata before update; starting fresh: {error}");
                    Vec::new()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                tracing::warn!("Failed to read existing model metadata before update; starting fresh: {error}");
                Vec::new()
            }
        }
    }

    pub fn omniasr_ctc_300m_present(&self) -> bool {
        omniasr_ctc_300m_present_in(&self.models_dir)
    }

    pub fn omniasr_ctc_1b_present(&self) -> bool {
        omniasr_ctc_1b_present_in(&self.models_dir)
    }

    /// Download and extract the official sherpa-onnx OmniASR CTC archive.
    pub fn download_omniasr(&self, size: AsrModelSize, progress_cb: impl Fn(f32)) -> Result<(), String> {
        let (dir_name, archive_url, archive_sha256, model_file, tokens_file) = match size {
            AsrModelSize::CTC300M => (
                OMNIASR_CTC_300M_DIR,
                OMNIASR_CTC_300M_ARCHIVE_URL,
                OMNIASR_CTC_300M_ARCHIVE_SHA256,
                OMNIASR_CTC_300M_MODEL,
                OMNIASR_CTC_300M_TOKENS,
            ),
            AsrModelSize::CTC1B => (
                OMNIASR_CTC_1B_DIR,
                OMNIASR_CTC_1B_ARCHIVE_URL,
                OMNIASR_CTC_1B_ARCHIVE_SHA256,
                OMNIASR_CTC_1B_MODEL,
                OMNIASR_CTC_1B_TOKENS,
            ),
            AsrModelSize::WSL7B => {
                return Err("The 7B model runs locally via WSL; no download is required from this panel.".to_string());
            }
        };

        if self.models_dir.join(model_file).exists() && self.models_dir.join(tokens_file).exists() {
            progress_cb(1.0);
            return Ok(());
        }

        ensure_pinned_sha256(&format!("Meta OmniASR {size:?} archive"), archive_sha256)?;

        self.ensure_dir()?;
        let dest_dir = self.models_dir.join(dir_name);
        fs::create_dir_all(&dest_dir).map_err(|e| format!("Create OmniASR dir: {e}"))?;

        let tmp_archive = self.models_dir.join(format!("{dir_name}.downloading.tar.bz2"));

        tracing::info!("Downloading Meta OmniASR {:?} from {}", size, archive_url);

        let response = crate::http::DOWNLOAD_AGENT
            .get(archive_url)
            .call()
            .map_err(|e| format!("OmniASR archive download failed: {e}"))?;

        let total_size = response.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

        write_reader_to_temp(
            response.into_reader(),
            &tmp_archive,
            total_size,
            0.9,
            &progress_cb,
            "Archive read error",
            "Archive write error",
        )?;

        let archive_size = fs::metadata(&tmp_archive).map(|m| m.len()).unwrap_or(0);
        if archive_size < 50_000_000 {
            remove_model_temp_file(&tmp_archive, "undersized OmniASR archive");
            return Err(format!("Downloaded OmniASR archive too small: {archive_size} bytes"));
        }

        verify_sha256(&tmp_archive, archive_sha256)?;
        progress_cb(0.92);
        self.extract_model_archive(&tmp_archive, &dest_dir, "model.int8.onnx", true)?;
        remove_model_temp_file(&tmp_archive, "completed OmniASR archive");
        progress_cb(1.0);

        if !self.model_path(model_file).exists() || !self.model_path(tokens_file).exists() {
            return Err(format!(
                "OmniASR archive extracted but model.int8.onnx or tokens.txt is missing in {}",
                dest_dir.display()
            ));
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut meta_entries = self.load_meta_for_update();
        for model in MODELS.iter().filter(|m| m.filename.starts_with(dir_name)) {
            let path = self.model_path(model.filename);
            if !path.exists() {
                continue;
            }
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let sha256 = compute_file_sha256(&path)?;
            verify_extracted_against_pin(model.filename, &sha256, model.sha256)?;
            let entry = ModelMeta {
                filename: model.filename.to_string(),
                downloaded_at: now.clone(),
                sha256,
                size_bytes: size,
                version: model.version.to_string(),
            };
            if let Some(pos) = meta_entries.iter().position(|m| m.filename == model.filename) {
                meta_entries[pos] = entry;
            } else {
                meta_entries.push(entry);
            }
        }
        if let Err(e) = self.save_meta(&meta_entries) {
            tracing::warn!("Failed to save OmniASR model metadata: {e}");
        }

        tracing::info!("Meta OmniASR {:?} installed under {}", size, dest_dir.display());
        Ok(())
    }

    fn extract_model_archive(
        &self,
        archive_path: &Path,
        dest_dir: &Path,
        model_output_name: &str,
        require_tokens: bool,
    ) -> Result<(), String> {
        let file = fs::File::open(archive_path).map_err(|e| format!("Open archive: {e}"))?;
        let decoder = bzip2::read::BzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);

        let mut model_written = false;
        let mut tokens_written = false;
        let mut staged_files: Vec<(PathBuf, PathBuf)> = Vec::new();

        for entry in archive.entries().map_err(|e| format!("Read archive entries: {e}"))? {
            let mut entry = entry.map_err(|e| format!("Archive entry: {e}"))?;
            let path = entry.path().map_err(|e| format!("Archive path: {e}"))?;
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            match name {
                "model.int8.onnx" | "model.onnx" => {
                    let dest = dest_dir.join(model_output_name);
                    let tmp = extraction_tmp_path(&dest);
                    remove_model_temp_file(&tmp, "stale model extraction temp");
                    if let Err(e) = entry.unpack(&tmp) {
                        remove_model_temp_file(&tmp, "failed model extraction temp");
                        cleanup_staged_files(&staged_files);
                        return Err(format!("Extract model: {e}"));
                    }
                    model_written = true;
                    staged_files.push((tmp, dest));
                }
                "tokens.txt" => {
                    let dest = dest_dir.join("tokens.txt");
                    let tmp = extraction_tmp_path(&dest);
                    remove_model_temp_file(&tmp, "stale token extraction temp");
                    if let Err(e) = entry.unpack(&tmp) {
                        remove_model_temp_file(&tmp, "failed token extraction temp");
                        cleanup_staged_files(&staged_files);
                        return Err(format!("Extract tokens: {e}"));
                    }
                    tokens_written = true;
                    staged_files.push((tmp, dest));
                }
                _ => {}
            }
        }

        if !model_written || (require_tokens && !tokens_written) {
            cleanup_staged_files(&staged_files);
            return Err(if require_tokens {
                "Archive did not contain model.int8.onnx (or model.onnx) and tokens.txt".to_string()
            } else {
                "Archive did not contain model.int8.onnx or model.onnx".to_string()
            });
        }

        for i in 0..staged_files.len() {
            let (tmp, dest) = &staged_files[i];
            if let Err(e) = replace_file(tmp, dest) {
                // Clean up the failing temp AND every still-unpromoted staged temp (entries 0..i were
                // already renamed away). Matches the other error paths; without it, a mid-loop failure
                // leaks the remaining .extracting-* files into the models dir.
                cleanup_staged_files(&staged_files[i..]);
                return Err(format!("Promote extracted model artifact: {e}"));
            }
        }
        Ok(())
    }

    pub fn campp_present(&self) -> bool {
        model_file_meets_min_size(&self.models_dir, CAMPP_MODEL, 10_000_000)
    }

    /// Download the CAM++ speaker-embedding ONNX (a single direct file, not an archive) and verify it
    /// against its pinned SHA-256 before placing it as `campp/model.onnx`.
    pub fn download_campp(&self, progress_cb: impl Fn(f32)) -> Result<(), String> {
        if self.models_dir.join(CAMPP_MODEL).exists() {
            progress_cb(1.0);
            return Ok(());
        }

        ensure_pinned_sha256("CAM++ model", CAMPP_MODEL_SHA256)?;

        self.ensure_dir()?;
        let dest_dir = self.models_dir.join(CAMPP_DIR);
        fs::create_dir_all(&dest_dir).map_err(|e| format!("Create CAM++ dir: {e}"))?;

        let dest = self.models_dir.join(CAMPP_MODEL);
        let tmp = dest.with_extension("downloading");
        tracing::info!("Downloading CAM++ from {}", CAMPP_MODEL_URL);

        let response = crate::http::DOWNLOAD_AGENT
            .get(CAMPP_MODEL_URL)
            .call()
            .map_err(|e| format!("CAM++ download failed: {e}"))?;

        let total_size = response.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

        write_reader_to_temp(
            response.into_reader(),
            &tmp,
            total_size,
            0.95,
            &progress_cb,
            "CAM++ read error",
            "CAM++ write error",
        )?;

        verify_sha256(&tmp, CAMPP_MODEL_SHA256)?;
        replace_file(&tmp, &dest).map_err(|e| format!("Replace CAM++ model: {e}"))?;
        progress_cb(1.0);

        if !self.campp_present() {
            return Err("CAM++ downloaded but model.onnx is missing or undersized".into());
        }

        tracing::info!("CAM++ installed under {}", dest_dir.display());
        Ok(())
    }

    pub fn denoiser_present(&self) -> bool {
        // Round-23 #3 (review): check the RESOLVED dir — the directory inference actually loads from
        // (resolve_models_dir falls back to the bundled dir when the user dir lacks OmniASR CTC). The
        // pipeline constructs DenoiserService from resolved_dir(), so the denoising-provenance flag must
        // be computed from the same place, or a denoiser present in the user dir but unreachable after
        // the bundled-dir fallback would still record denoising=true while audio passed through.
        // GTCRN is ~0.5 MB — see the MODELS entry: a 10 MB floor rejected the correct file.
        model_file_meets_min_size(&self.resolved_dir(), DENOISER_MODEL, 400_000)
    }

    /// Download the GTCRN denoiser ONNX (a single direct file, not an archive) and verify it against
    /// its pinned SHA-256 before placing it as `denoiser/model.onnx`.
    pub fn download_denoiser(&self, progress_cb: impl Fn(f32)) -> Result<(), String> {
        if self.models_dir.join(DENOISER_MODEL).exists() {
            progress_cb(1.0);
            return Ok(());
        }

        ensure_pinned_sha256("AI Denoiser model", DENOISER_MODEL_SHA256)?;

        self.ensure_dir()?;
        let dest_dir = self.models_dir.join(DENOISER_DIR);
        fs::create_dir_all(&dest_dir).map_err(|e| format!("Create Denoiser dir: {e}"))?;

        let dest = self.models_dir.join(DENOISER_MODEL);
        let tmp = dest.with_extension("downloading");
        tracing::info!("Downloading AI Denoiser from {}", DENOISER_MODEL_URL);

        let response = crate::http::DOWNLOAD_AGENT
            .get(DENOISER_MODEL_URL)
            .call()
            .map_err(|e| format!("Denoiser download failed: {e}"))?;

        let total_size = response.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

        write_reader_to_temp(
            response.into_reader(),
            &tmp,
            total_size,
            0.95,
            &progress_cb,
            "Denoiser read error",
            "Denoiser write error",
        )?;

        verify_sha256(&tmp, DENOISER_MODEL_SHA256)?;
        replace_file(&tmp, &dest).map_err(|e| format!("Replace Denoiser model: {e}"))?;
        progress_cb(1.0);

        if !self.denoiser_present() {
            return Err("Denoiser downloaded but model.onnx is missing or undersized".into());
        }

        tracing::info!("AI Denoiser installed under {}", dest_dir.display());
        Ok(())
    }

    pub fn download_model(&self, model: &ModelInfo, progress_cb: impl Fn(f32)) -> Result<(), String> {
        if model.filename.starts_with(OMNIASR_CTC_300M_DIR) {
            return self.download_omniasr(AsrModelSize::CTC300M, progress_cb);
        }
        if model.filename.starts_with(OMNIASR_CTC_1B_DIR) {
            return self.download_omniasr(AsrModelSize::CTC1B, progress_cb);
        }
        if model.filename.starts_with(CAMPP_DIR) {
            return self.download_campp(progress_cb);
        }
        if model.filename.starts_with(DENOISER_DIR) {
            return self.download_denoiser(progress_cb);
        }

        if model.url.is_empty() {
            return Err(format!("No download URL configured for {}; use Download All or manual install", model.name));
        }

        ensure_pinned_sha256(model.name, model.sha256)?;

        self.ensure_dir()?;

        let dest = self.model_path(model.filename);
        let tmp = dest.with_extension("downloading");

        let response = crate::http::DOWNLOAD_AGENT
            .get(model.url)
            .call()
            .map_err(|e| format!("Download failed for {}: {}", model.name, e))?;

        let total_size = response.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

        write_reader_to_temp(
            response.into_reader(),
            &tmp,
            total_size,
            1.0,
            &progress_cb,
            "Download read error",
            "Write error",
        )?;

        let size = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        if size < model.min_size_bytes {
            remove_model_temp_file(&tmp, "undersized model download");
            return Err(format!("Downloaded file too small: {} bytes (expected > {})", size, model.min_size_bytes));
        }

        verify_sha256(&tmp, model.sha256)?;

        replace_file(&tmp, &dest).map_err(|e| format!("Replace downloaded model: {e}"))?;

        let actual_sha256 = model.sha256.to_string();

        let now = chrono::Utc::now().to_rfc3339();
        let meta_entry = ModelMeta {
            filename: model.filename.to_string(),
            downloaded_at: now,
            sha256: actual_sha256,
            size_bytes: size,
            version: model.version.to_string(),
        };

        let mut meta_entries = self.load_meta_for_update();
        if let Some(pos) = meta_entries.iter().position(|m| m.filename == model.filename) {
            meta_entries[pos] = meta_entry;
        } else {
            meta_entries.push(meta_entry);
        }
        if let Err(e) = self.save_meta(&meta_entries) {
            tracing::warn!("Failed to save model metadata: {e}");
        }

        tracing::info!("Downloaded {} ({} bytes)", model.name, size);
        Ok(())
    }

    pub fn warmup(&self) -> Result<(), String> {
        let vad_path = self.resolved_dir().join("silero_vad_v4.onnx");
        if vad_path.exists() {
            match ort::session::Session::builder().and_then(|mut b| b.commit_from_file(&vad_path)) {
                Ok(_) => tracing::info!("Silero VAD warmed up successfully"),
                Err(e) => tracing::warn!("VAD warm-up failed: {e}"),
            }
        }
        Ok(())
    }

    pub fn status(&self) -> Vec<serde_json::Value> {
        let active_dir = self.resolved_dir();
        MODELS
            .iter()
            .map(|m| {
                let path = active_dir.join(m.filename);
                let (exists, size) = model_file_state(&active_dir, m.filename);
                let available = size.unwrap_or(0) >= m.min_size_bytes;
                let source = if !available {
                    "missing"
                } else if active_dir == self.models_dir {
                    "user"
                } else {
                    "bundled"
                };
                serde_json::json!({
                    "name": m.name,
                    "filename": m.filename,
                    "downloaded": available,
                    "exists": exists,
                    "size_bytes": size,
                    "min_size_bytes": m.min_size_bytes,
                    "version": m.version,
                    "source": source,
                    "downloadable": model_download_supported(m),
                    "path": path,
                })
            })
            .collect()
    }
}

fn model_file_state(model_dir: &Path, filename: &str) -> (bool, Option<u64>) {
    let path = model_dir.join(filename);
    let size = path.metadata().map(|m| m.len()).ok();
    (size.is_some(), size)
}

fn model_file_meets_min_size(model_dir: &Path, filename: &str, min_size_bytes: u64) -> bool {
    model_file_state(model_dir, filename).1.unwrap_or(0) >= min_size_bytes
}

fn model_available_in(model_dir: &Path, model: &ModelInfo) -> bool {
    model_file_meets_min_size(model_dir, model.filename, model.min_size_bytes)
}

fn model_download_supported(model: &ModelInfo) -> bool {
    if model.filename.starts_with(OMNIASR_CTC_300M_DIR) {
        return !OMNIASR_CTC_300M_ARCHIVE_SHA256.is_empty();
    }
    if model.filename.starts_with(OMNIASR_CTC_1B_DIR) {
        return !OMNIASR_CTC_1B_ARCHIVE_SHA256.is_empty();
    }
    if model.filename.starts_with(CAMPP_DIR) {
        return !CAMPP_MODEL_SHA256.is_empty();
    }
    if model.filename.starts_with(DENOISER_DIR) {
        return !DENOISER_MODEL_SHA256.is_empty();
    }
    !model.url.is_empty() && !model.sha256.is_empty()
}

fn missing_required_model_names_in(model_dir: &Path) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !model_file_meets_min_size(model_dir, "silero_vad_v4.onnx", 1_000_000) {
        missing.push("Silero VAD v4");
    }
    if !omniasr_ctc_300m_present_in(model_dir) && !omniasr_ctc_1b_present_in(model_dir) {
        missing.push("Meta OmniASR CTC model and tokens");
    }
    missing
}

fn omniasr_ctc_300m_present_in(model_dir: &Path) -> bool {
    model_file_meets_min_size(model_dir, OMNIASR_CTC_300M_MODEL, 50_000_000)
        && model_file_meets_min_size(model_dir, OMNIASR_CTC_300M_TOKENS, 100)
}

fn omniasr_ctc_1b_present_in(model_dir: &Path) -> bool {
    model_file_meets_min_size(model_dir, OMNIASR_CTC_1B_MODEL, 500_000_000)
        && model_file_meets_min_size(model_dir, OMNIASR_CTC_1B_TOKENS, 100)
}

/// Lowercase hex of a byte slice. sha2 0.11's `finalize()` output (a `hybrid_array::Array`) no longer
/// implements `LowerHex`, so the old `format!("{:x}", ...)` form no longer compiles — encode the bytes
/// explicitly here (no extra dependency).
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Compute the SHA-256 of a file as a lowercase hex string — the content-address shared by archive
/// verification and registry import (a model checkpoint is identified by exactly this hash).
pub fn compute_file_sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("Open for hash: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("Hash read: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Hard ceiling on any single model/archive download. The SHA-256 verify runs only AFTER the full
/// write, so without this a compromised/on-path host (or the mutable GitHub `raw/<commit>` origin)
/// could trickle a multi-GB body within the read timeout and fill the disk BEFORE the hash rejects it
/// — wedging unrelated writers (the SQLite WAL, other temps). No legitimate pinned artifact is anywhere
/// near this (the largest is the ~365 MB OmniASR archive); the exact bytes are still enforced by
/// verify_sha256. Backstop only — mirrors http::MAX_JSON_RESPONSE_BYTES on the JSON path. We do NOT cap
/// against the server-supplied Content-Length, which a malicious host controls.
const MAX_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

fn write_reader_to_temp<R: Read>(
    mut reader: R,
    tmp_path: &Path,
    total_size: u64,
    progress_scale: f32,
    progress_cb: &impl Fn(f32),
    read_context: &str,
    write_context: &str,
) -> Result<u64, String> {
    let result = (|| -> Result<u64, String> {
        let mut file = fs::File::create(tmp_path).map_err(|e| format!("Create temp file: {e}"))?;
        let mut downloaded: u64 = 0;
        let mut buffer = [0u8; 8192];
        loop {
            let n = reader.read(&mut buffer).map_err(|e| format!("{read_context}: {e}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buffer[..n]).map_err(|e| format!("{write_context}: {e}"))?;
            downloaded += n as u64;
            if downloaded > MAX_DOWNLOAD_BYTES {
                // Abort mid-stream so an oversized body can't fill the disk; the temp is removed below.
                return Err(format!("{read_context}: download exceeded the {MAX_DOWNLOAD_BYTES}-byte safety cap"));
            }
            if total_size > 0 {
                progress_cb((downloaded as f32 / total_size as f32) * progress_scale);
            }
        }
        file.flush().map_err(|e| format!("{write_context}: {e}"))?;
        Ok(downloaded)
    })();
    if result.is_err() {
        remove_model_temp_file(tmp_path, "partial model download");
    }
    result
}

fn extraction_tmp_path(dest: &Path) -> PathBuf {
    let file_name = dest.file_name().and_then(|name| name.to_str()).unwrap_or("model-artifact");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    dest.with_file_name(format!("{file_name}.extracting-{}-{nonce}", std::process::id()))
}

fn cleanup_staged_files(staged_files: &[(PathBuf, PathBuf)]) {
    for (tmp, _) in staged_files {
        remove_model_temp_file(tmp, "staged model extraction temp");
    }
}

fn ensure_model_parent_dir(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(error) = fs::create_dir_all(parent) {
        tracing::warn!("Failed to create model artifact parent directory {}: {error}", parent.display());
    }
}

fn remove_model_temp_file(path: &Path, context: &str) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!("Failed to remove {context} file {}: {error}", path.display()),
    }
}

/// The platform-specific ONNX Runtime shared-library filename that the `ort`
/// crate's `load-dynamic` backend dlopen's at runtime (sherpa-onnx links its own
/// copy; this is the standalone library used for the Silero VAD path).
pub(crate) fn ort_dylib_filename() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "onnxruntime.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "libonnxruntime.dylib"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "libonnxruntime.so"
    }
}

/// Locate the ONNX Runtime shared library next to the executable, in the active
/// models directory, or in the working directory, and set `ORT_DYLIB_PATH` if it
/// is not already set. Runs on every platform using the per-OS library name; if
/// nothing is found it leaves the variable unset so `ort` falls back to the
/// system loader's default search (this is purely additive — it never overrides
/// an existing `ORT_DYLIB_PATH`).
pub fn init_ort_dylib_path() {
    if std::env::var("ORT_DYLIB_PATH").is_ok() {
        return;
    }

    let dylib = ort_dylib_filename();
    let mut resolved_path = None;

    // 1. Next to the current exe, or in its parent directory.
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let p1 = exe_dir.join(dylib);
            if p1.exists() {
                resolved_path = Some(p1);
            } else if let Some(parent_dir) = exe_dir.parent() {
                let p2 = parent_dir.join(dylib);
                if p2.exists() {
                    resolved_path = Some(p2);
                }
            }
        }
    }

    // 2. Bundled under the active models directory. On Windows the library is
    //    packaged inside a directory literally named `onnxruntime.dll`; all
    //    platforms also accept it placed flat in the models directory.
    if resolved_path.is_none() {
        let active_dir = active_models_dir();
        #[cfg(target_os = "windows")]
        {
            let nested = active_dir.join("onnxruntime.dll").join(dylib);
            if nested.exists() {
                resolved_path = Some(nested);
            }
        }
        if resolved_path.is_none() {
            let flat = active_dir.join(dylib);
            if flat.exists() {
                resolved_path = Some(flat);
            }
        }
    }

    // 3. Current working directory.
    if resolved_path.is_none() {
        let p4 = Path::new(dylib);
        if p4.exists() {
            if let Ok(abs) = p4.canonicalize() {
                resolved_path = Some(abs);
            }
        }
    }

    if let Some(path) = resolved_path {
        tracing::info!("Setting ORT_DYLIB_PATH programmatically to {:?}", path);
        std::env::set_var("ORT_DYLIB_PATH", path);
    } else {
        tracing::warn!(
            "{dylib} not found next to exe, in models dir, or cwd; ORT will fall back to the system loader search"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs::File;
    use std::io;

    #[test]
    fn runtime_integrity_rejects_tampered_model() {
        // A file under a PINNED model filename whose content does not match the pin is rejected
        // at runtime (charter M2.3: a tampered ONNX fails the runtime manifest check).
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tampered.onnx");
        std::fs::write(&path, b"this is not the real model").unwrap();
        let result = verify_model_path_runtime(&path, OMNIASR_CTC_300M_MODEL);
        assert!(result.is_err(), "a tampered model must be rejected, got {result:?}");
        assert!(result.unwrap_err().contains("integrity check FAILED"));
    }

    #[test]
    fn runtime_integrity_noop_for_unpinned_model() {
        // The WavLM OOD detector has an empty pin today -> verification is a documented no-op (cannot
        // verify). (CAM++ and the denoiser are now pinned, so they are NOT no-ops any more — see
        // extra_model_pins_are_populated_and_urls_are_direct_files.)
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("whatever.onnx");
        std::fs::write(&path, b"unpinned content").unwrap();
        assert!(verify_model_path_runtime(&path, "wavlm_ood.onnx").is_ok());
    }

    #[test]
    fn extra_model_pins_are_populated_and_urls_are_direct_files() {
        // Regression for the 2026-07-13 fetch: CAM++ and the GTCRN denoiser were installed and their
        // real bytes pinned; the previous tar.bz2 URLs were dead (404) and the extracted-file pins were
        // blank (integrity check silently disabled). CTC-1B's archive pin was blank too, which blocked
        // the in-app download. These invariants must not regress.
        assert_eq!(OMNIASR_CTC_1B_ARCHIVE_SHA256.len(), 64, "CTC-1B archive pin must be populated");

        for (dir, url, sha) in [
            (CAMPP_DIR, CAMPP_MODEL_URL, CAMPP_MODEL_SHA256),
            (DENOISER_DIR, DENOISER_MODEL_URL, DENOISER_MODEL_SHA256),
        ] {
            assert_eq!(sha.len(), 64, "{dir}: 64-hex SHA pin");
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{dir}: pin must be hex");
            // Direct .onnx file, never a tar.bz2 (the old dead URLs) — the download path no longer extracts.
            assert!(url.ends_with(".onnx"), "{dir}: URL must be a direct .onnx, got {url}");
            assert!(!url.ends_with(".tar.bz2"), "{dir}: must not be an archive URL");
            // MODELS entry mirrors the constants so status()/model_available_in agree with the downloader.
            let m = MODELS.iter().find(|m| m.filename.starts_with(dir)).expect("MODELS entry present");
            assert_eq!(m.sha256, sha, "{dir}: MODELS pin must match the constant");
            assert_eq!(m.url, url, "{dir}: MODELS url must match the constant");
            assert!(model_download_supported(m), "{dir}: must be auto-downloadable now");
        }

        // The GTCRN denoiser is ~535 KB; its floor must admit the real file (the old 10 MB floor rejected it).
        let den = MODELS.iter().find(|m| m.filename.starts_with(DENOISER_DIR)).unwrap();
        assert!(den.min_size_bytes < 535_638, "denoiser floor must be below the real GTCRN size");
    }

    #[test]
    fn runtime_integrity_accepts_real_300m_model_when_present() {
        // Positive proof on a machine where the real model is downloaded; skipped in CI.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("models").join(OMNIASR_CTC_300M_MODEL);
        if !path.exists() {
            return;
        }
        assert!(
            verify_model_path_runtime(&path, OMNIASR_CTC_300M_MODEL).is_ok(),
            "the real on-disk 300M model must pass its pinned integrity check"
        );
    }

    #[test]
    fn omniasr_download_refuses_unpinned_archive_before_side_effects() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let models_dir = tmp.path().join("models");
        let manager = ModelManager::new(models_dir.clone());
        let progress_calls = Cell::new(0);

        let err = manager
            .download_omniasr(AsrModelSize::CTC300M, |_| progress_calls.set(progress_calls.get() + 1))
            .expect_err("unpinned archive must fail before download");

        assert!(err.contains("Missing pinned SHA256 for Meta OmniASR CTC300M archive"));
        assert_eq!(progress_calls.get(), 0, "preflight failure must not emit progress");
        assert!(!models_dir.exists(), "preflight failure must not create model directories");
    }

    #[test]
    fn routed_archive_download_refuses_unpinned_archive_before_side_effects() {
        // Routed through download_model(): a model whose archive SHA is still blank (OmniASR CTC-300M —
        // its archive was never hash-recorded) must refuse BEFORE any network/dir side effects. (CAM++
        // and the denoiser are pinned now, so they no longer exercise this path.)
        let tmp = tempfile::tempdir().expect("tempdir");
        let models_dir = tmp.path().join("models");
        let manager = ModelManager::new(models_dir.clone());
        let progress_calls = Cell::new(0);
        let model = MODELS.iter().find(|model| model.filename == OMNIASR_CTC_300M_MODEL).expect("300M model entry");

        let err = manager
            .download_model(model, |_| progress_calls.set(progress_calls.get() + 1))
            .expect_err("unpinned archive must fail before download");

        assert!(err.contains("Missing pinned SHA256 for Meta OmniASR CTC300M archive"), "got: {err}");
        assert_eq!(progress_calls.get(), 0, "preflight failure must not emit progress");
        assert!(!models_dir.exists(), "preflight failure must not create model directories");
    }

    #[test]
    fn verify_extracted_against_pin_semantics() {
        // Empty pin is "not yet pinned" → accepted as a no-op so archive-sourced
        // files without an on-disk-computed pin still install.
        assert!(verify_extracted_against_pin("f.onnx", "abc123", "").is_ok());
        // Matching hash → accepted.
        assert!(verify_extracted_against_pin("f.onnx", "abc123", "abc123").is_ok());
        // Mismatch → rejected with a descriptive, both-sided error.
        let err = verify_extracted_against_pin("f.onnx", "deadbeef", "abc123")
            .expect_err("mismatched hash must fail the install");
        assert!(err.contains("f.onnx"));
        assert!(err.contains("integrity check"));
        assert!(err.contains("abc123"));
        assert!(err.contains("deadbeef"));
    }

    #[test]
    fn omniasr_extracted_files_are_sha256_pinned() {
        // The OmniASR model/token files are extracted from the archive bundle, so
        // their extracted-file pins are computed directly from the on-disk models
        // and must stay populated — an empty pin would silently disable the
        // post-extract integrity check in `verify_extracted_against_pin`.
        for filename in [OMNIASR_CTC_300M_MODEL, OMNIASR_CTC_300M_TOKENS, OMNIASR_CTC_1B_MODEL, OMNIASR_CTC_1B_TOKENS] {
            let model = MODELS.iter().find(|m| m.filename == filename).expect("model entry present");
            assert_eq!(model.sha256.len(), 64, "{filename} must carry a 64-hex-char SHA256 pin");
            assert!(model.sha256.chars().all(|c| c.is_ascii_hexdigit()), "{filename} pin must be hex");
        }
    }

    #[test]
    fn ort_dylib_filename_is_platform_appropriate() {
        let name = ort_dylib_filename();
        assert!(name.contains("onnxruntime"), "name should reference onnxruntime: {name}");
        #[cfg(target_os = "windows")]
        assert_eq!(name, "onnxruntime.dll");
        #[cfg(target_os = "macos")]
        assert_eq!(name, "libonnxruntime.dylib");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        assert_eq!(name, "libonnxruntime.so");
    }

    #[test]
    fn model_status_checks_are_read_only_for_empty_user_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let models_dir = tmp.path().join("models");
        let manager = ModelManager::new(models_dir.clone());

        let _ = manager.status();
        let _ = manager.missing_models();
        let _ = manager.all_models_present();

        assert!(!models_dir.exists(), "read-only model checks must not create user model directories");
    }

    #[test]
    fn save_meta_replaces_existing_metadata_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(tmp.path().join("models"));
        manager.ensure_dir().expect("ensure dir");
        std::fs::write(manager.meta_path(), "[{\"filename\":\"old\"}]").expect("seed meta");
        let entries = vec![ModelMeta {
            filename: "new-model.onnx".to_string(),
            downloaded_at: "2026-06-16T00:00:00Z".to_string(),
            sha256: "abc123".to_string(),
            size_bytes: 42,
            version: "1.0".to_string(),
        }];

        manager.save_meta(&entries).expect("save meta");

        let loaded = manager.load_meta().expect("load meta");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].filename, "new-model.onnx");
        assert!(!manager.meta_path().with_extension("json.tmp").exists());
    }

    #[test]
    fn load_meta_for_update_treats_missing_as_empty_but_surfaces_corrupt_to_strict_loader() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(tmp.path().join("models"));

        assert!(manager.load_meta_for_update().is_empty());

        manager.ensure_dir().expect("ensure dir");
        std::fs::write(manager.meta_path(), "{not valid json").expect("seed corrupt meta");

        assert!(manager.load_meta_for_update().is_empty());
        match manager.load_meta() {
            Ok(_) => panic!("strict meta loader should reject corrupt JSON"),
            Err(error) => assert!(error.contains("Parse meta")),
        }
    }

    #[test]
    fn write_reader_to_temp_writes_file_and_reports_progress() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("model.downloading");
        let progress_calls = Cell::new(0);

        let written = write_reader_to_temp(
            std::io::Cursor::new(b"model-bytes".to_vec()),
            &path,
            11,
            1.0,
            &|_| progress_calls.set(progress_calls.get() + 1),
            "read",
            "write",
        )
        .expect("write temp");

        assert_eq!(written, 11);
        assert_eq!(std::fs::read(&path).expect("read temp"), b"model-bytes");
        assert!(progress_calls.get() > 0);
    }

    #[test]
    fn write_reader_to_temp_removes_partial_file_on_read_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("model.downloading");

        let err = write_reader_to_temp(
            FailingReader { first_read_done: false },
            &path,
            100,
            1.0,
            &|_| {},
            "download read",
            "download write",
        )
        .expect_err("reader failure should error");

        assert!(err.contains("download read"));
        assert!(!path.exists(), "partial download temp file should be removed on read failure");
    }

    #[test]
    fn extract_model_archive_missing_tokens_leaves_no_final_or_staged_artifact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(tmp.path().join("models"));
        let archive_path = tmp.path().join("missing-tokens.tar.bz2");
        write_bzip2_tar(&archive_path, &[("nested/model.int8.onnx", b"model bytes")]);
        let dest_dir = tmp.path().join("extract");
        std::fs::create_dir_all(&dest_dir).expect("dest dir");

        let err = manager
            .extract_model_archive(&archive_path, &dest_dir, "model.int8.onnx", true)
            .expect_err("missing tokens should fail");

        assert!(err.contains("tokens.txt"));
        assert!(!dest_dir.join("model.int8.onnx").exists());
        assert_no_extracting_files(&dest_dir);
    }

    #[test]
    fn extract_model_archive_promotes_complete_required_pair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(tmp.path().join("models"));
        let archive_path = tmp.path().join("complete.tar.bz2");
        write_bzip2_tar(&archive_path, &[("nested/model.int8.onnx", b"model bytes"), ("nested/tokens.txt", b"tokens")]);
        let dest_dir = tmp.path().join("extract");
        std::fs::create_dir_all(&dest_dir).expect("dest dir");

        manager.extract_model_archive(&archive_path, &dest_dir, "model.int8.onnx", true).expect("extract");

        assert_eq!(std::fs::read(dest_dir.join("model.int8.onnx")).expect("model"), b"model bytes");
        assert_eq!(std::fs::read(dest_dir.join("tokens.txt")).expect("tokens"), b"tokens");
        assert_no_extracting_files(&dest_dir);
    }

    #[test]
    fn extract_model_archive_promotion_failure_leaves_no_orphan_temps() {
        // Hardening-audit LOW: if an earlier replace_file in the promotion loop fails, the remaining
        // staged extraction temps must still be cleaned up (matching the other error paths) rather
        // than leaking into the models dir.
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(tmp.path().join("models"));
        let archive_path = tmp.path().join("pair.tar.bz2");
        // model first -> staged_files[0], promoted first; tokens second -> staged_files[1].
        write_bzip2_tar(&archive_path, &[("nested/model.int8.onnx", b"model bytes"), ("nested/tokens.txt", b"tokens")]);
        let dest_dir = tmp.path().join("extract");
        std::fs::create_dir_all(&dest_dir).expect("dest dir");
        // Force the FIRST promotion (model.int8.onnx) to fail by occupying its destination with a
        // NON-EMPTY directory — replace_file errors on this on both Unix and Windows.
        let blocking = dest_dir.join("model.int8.onnx");
        std::fs::create_dir_all(&blocking).expect("blocking dir");
        std::fs::write(blocking.join("sentinel"), b"x").expect("sentinel");

        let err = manager
            .extract_model_archive(&archive_path, &dest_dir, "model.int8.onnx", true)
            .expect_err("promotion should fail on the first artifact");
        assert!(err.contains("Promote extracted model artifact"), "got: {err}");
        // BEFORE the fix this fails: tokens.txt.extracting-* is orphaned. AFTER: it is cleaned up.
        assert_no_extracting_files(&dest_dir);
    }

    #[test]
    fn bundled_runtime_models_count_as_available_when_user_dir_is_empty() {
        assert!(
            omniasr_ctc_300m_present_in(&bundled_models_dir())
                && model_file_meets_min_size(&bundled_models_dir(), "silero_vad_v4.onnx", 1_000_000),
            "repository fixture must include bundled VAD and OmniASR 300M models"
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(tmp.path().join("models"));

        assert!(manager.all_models_present(), "runtime should see bundled essential models");

        let missing_names = manager.missing_models().into_iter().map(|model| model.name).collect::<Vec<_>>();
        assert!(!missing_names.contains(&"Silero VAD v4"));
        assert!(!missing_names.contains(&"Meta OmniASR CTC 300M (model)"));
        assert!(!missing_names.contains(&"Meta OmniASR CTC 300M (tokens)"));
        let missing_optional_names = manager.missing_optional_model_names();
        assert!(!missing_optional_names.contains(&"Silero VAD v4"));
        if !omniasr_ctc_1b_present_in(&bundled_models_dir()) {
            assert!(missing_optional_names.contains(&"Meta OmniASR CTC 1B (model)"));
        }

        let status = manager.status();
        let ctc_300m =
            status.iter().find(|model| model["filename"] == OMNIASR_CTC_300M_MODEL).expect("300M model status");
        assert_eq!(ctc_300m["downloaded"], true);
        assert_eq!(ctc_300m["source"], "bundled");
    }

    struct FailingReader {
        first_read_done: bool,
    }

    impl Read for FailingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.first_read_done {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "synthetic read failure"));
            }
            self.first_read_done = true;
            buf[..7].copy_from_slice(b"partial");
            Ok(7)
        }
    }

    #[test]
    fn unpinned_optional_models_are_not_bulk_download_candidates() {
        // Bulk-download candidacy follows PINNING, not presence (presence now varies with local install
        // state — CAM++/denoiser may be in the bundled dir). CAM++ and the GTCRN denoiser became pinned
        // direct-file downloads on 2026-07-13; WavLM OOD is still unpinned and must always be skipped.
        let campp = MODELS.iter().find(|m| m.filename == CAMPP_MODEL).expect("CAM++ entry");
        let denoiser = MODELS.iter().find(|m| m.filename == DENOISER_MODEL).expect("denoiser entry");
        let wavlm = MODELS.iter().find(|m| m.filename == "wavlm_ood.onnx").expect("wavlm entry");
        assert!(model_download_supported(campp), "CAM++ is pinned + has a URL -> a candidate");
        assert!(model_download_supported(denoiser), "denoiser is pinned + has a URL -> a candidate");
        assert!(!model_download_supported(wavlm), "WavLM OOD is unpinned -> never a candidate");

        // The unpinned model must never appear in the bulk-download set regardless of what is installed.
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(tmp.path().join("models"));
        let downloadable: Vec<&str> = manager.downloadable_missing_models().into_iter().map(|m| m.name).collect();
        assert!(
            !downloadable.contains(&"WavLM Speech OOD Detector"),
            "an unpinned model must never be a bulk-download candidate, got {downloadable:?}"
        );
    }

    #[test]
    fn bundled_model_selector_prefers_first_candidate_with_runtime_asr_pair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing_candidate = tmp.path().join("exe").join("models");
        let resource_candidate = tmp.path().join("exe").join("resources").join("models");
        create_minimal_300m_model_pair(&resource_candidate);

        let selected = select_bundled_models_dir(vec![missing_candidate.clone(), resource_candidate.clone()]);

        assert_eq!(selected, resource_candidate);
        assert!(!selected.starts_with(missing_candidate));
    }

    #[test]
    fn resolve_models_dir_falls_back_when_user_dir_has_truncated_model_pair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_dir = tmp.path().join("user-models");
        std::fs::create_dir_all(user_dir.join(OMNIASR_CTC_300M_DIR)).expect("user model dir");
        std::fs::write(user_dir.join(OMNIASR_CTC_300M_MODEL), b"too small").expect("truncated model");
        std::fs::write(user_dir.join(OMNIASR_CTC_300M_TOKENS), b"tokens").expect("truncated tokens");

        assert_ne!(resolve_models_dir(&user_dir), user_dir);
    }

    fn create_minimal_300m_model_pair(model_dir: &Path) {
        std::fs::create_dir_all(model_dir.join(OMNIASR_CTC_300M_DIR)).expect("model dir");
        File::create(model_dir.join(OMNIASR_CTC_300M_MODEL))
            .expect("model file")
            .set_len(50_000_000)
            .expect("model size");
        File::create(model_dir.join(OMNIASR_CTC_300M_TOKENS)).expect("tokens file").set_len(100).expect("tokens size");
    }

    fn write_bzip2_tar(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("archive file");
        let encoder = bzip2::write::BzEncoder::new(file, bzip2::Compression::best());
        let mut builder = tar::Builder::new(encoder);
        for (entry_path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, *entry_path, &mut std::io::Cursor::new(*bytes))
                .expect("append archive entry");
        }
        let encoder = builder.into_inner().expect("finish tar");
        encoder.finish().expect("finish bzip2");
    }

    fn assert_no_extracting_files(dir: &Path) {
        let extracting_left = std::fs::read_dir(dir)
            .expect("read extract dir")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".extracting-"));
        assert!(!extracting_left, "staged extraction files should be promoted or removed");
    }
}
