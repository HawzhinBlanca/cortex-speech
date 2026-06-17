//! AI-powered audio denoising for high-tier podcast cleanup.
//!
//! Uses an ONNX-based denoiser model to remove background noise, hiss, and hum.

use sherpa_onnx::{OfflineSpeechDenoiser, OfflineSpeechDenoiserConfig, OfflineSpeechDenoiserModelConfig};
use std::path::Path;

pub struct DenoiserService {
    denoiser: Option<OfflineSpeechDenoiser>,
}

impl DenoiserService {
    pub fn new(model_dir: &Path) -> Self {
        let model_path = model_dir.join(crate::models::DENOISER_MODEL);
        if !model_path.exists() {
            tracing::warn!("Denoiser model not found at {}; AI cleanup disabled", model_path.display());
            return Self { denoiser: None };
        }

        let mut provider = crate::asr::get_provider();
        let mut model_config = OfflineSpeechDenoiserModelConfig::default();
        model_config.gtcrn.model = Some(model_path.to_string_lossy().to_string());
        model_config.num_threads = 2;
        model_config.provider = Some(provider.clone());

        let mut config = OfflineSpeechDenoiserConfig { model: model_config };

        match OfflineSpeechDenoiser::create(&config) {
            Some(denoiser) => Self { denoiser: Some(denoiser) },
            None => {
                if provider != "cpu" {
                    tracing::warn!(
                        "Failed to load Denoiser model with GPU provider '{}'; falling back to CPU",
                        provider
                    );
                    provider = "cpu".to_string();
                    config.model.provider = Some(provider);
                    match OfflineSpeechDenoiser::create(&config) {
                        Some(denoiser) => Self { denoiser: Some(denoiser) },
                        None => {
                            tracing::error!("Failed to load Denoiser model even on CPU");
                            Self { denoiser: None }
                        }
                    }
                } else {
                    tracing::error!("Failed to load Denoiser model");
                    Self { denoiser: None }
                }
            }
        }
    }

    pub fn process(&self, pcm: &[f32], sample_rate: u32) -> Vec<f32> {
        if let Some(ref denoiser) = self.denoiser {
            let res = denoiser.run(pcm, sample_rate as i32);
            res.samples
        } else {
            pcm.to_vec()
        }
    }
}
