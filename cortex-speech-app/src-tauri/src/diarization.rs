//! Lightweight acoustic speaker diarization for podcast / audiobook imports.
//!
//! Uses mel-filterbank embeddings per VAD chunk and online cosine clustering — no extra ONNX model.

use crate::features::FbankExtractor;
use ndarray::Array2;
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use std::path::Path;

/// Minimum cosine similarity to assign a chunk to an existing speaker centroid.
const MIN_SPEAKER_SIMILARITY: f32 = 0.85;

/// High-tier speaker embedding service using CAM++ ONNX model.
pub struct SpeakerEmbeddingService {
    manager: Option<SpeakerEmbeddingExtractor>,
}

impl SpeakerEmbeddingService {
    pub fn new(model_dir: &Path) -> Self {
        let model_path = model_dir.join(crate::models::CAMPP_MODEL);
        if !model_path.exists() {
            tracing::warn!("CAM++ model not found at {}; high-tier diarization disabled", model_path.display());
            return Self { manager: None };
        }

        let config = SpeakerEmbeddingExtractorConfig {
            model: Some(model_path.to_string_lossy().to_string()),
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_string()),
        };

        match SpeakerEmbeddingExtractor::create(&config) {
            Some(manager) => Self { manager: Some(manager) },
            None => {
                tracing::error!("Failed to load Speaker Embedding model");
                Self { manager: None }
            }
        }
    }

    pub fn compute_embedding(&self, samples: &[f32], sample_rate: u32) -> Vec<f32> {
        if let Some(ref manager) = self.manager {
            if let Some(stream) = manager.create_stream() {
                stream.accept_waveform(sample_rate as i32, samples);
                manager.compute(&stream).unwrap_or_default()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }
}

/// Assign a speaker label per VAD chunk (`SPEAKER_00`, `SPEAKER_01`, …).
pub fn label_chunk_speakers(
    pcm: &[i16],
    sample_rate: u32,
    chunk_ranges: &[(usize, usize)],
    max_speakers: u32,
    embedding_service: &SpeakerEmbeddingService,
) -> Vec<Option<String>> {
    if chunk_ranges.is_empty() {
        return Vec::new();
    }

    let max_speakers = max_speakers.clamp(1, 32) as usize;
    let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

    let embeddings: Vec<Vec<f32>> = chunk_ranges
        .iter()
        .map(|&(start, end)| {
            let start = start.min(f32_pcm.len());
            let end = end.min(f32_pcm.len());
            if end <= start {
                return Vec::new();
            }
            let chunk = &f32_pcm[start..end];

            // Try high-tier embedding first
            let emb = embedding_service.compute_embedding(chunk, sample_rate);
            if !emb.is_empty() {
                emb
            } else {
                // Fallback to acoustic clustering if service is unavailable
                let fbank = FbankExtractor::new(sample_rate);
                chunk_embedding(&fbank, chunk)
            }
        })
        .collect();

    online_cluster(&embeddings, max_speakers)
}

fn chunk_embedding(fbank: &FbankExtractor, pcm: &[f32]) -> Vec<f32> {
    if pcm.len() < 160 {
        return Vec::new();
    }
    let features: Array2<f32> = fbank.compute(pcm);
    if features.nrows() == 0 {
        return Vec::new();
    }
    let bins = features.ncols();
    let frames = features.nrows();
    let mut mean = vec![0.0f32; bins];
    for row in features.rows() {
        for (i, &v) in row.iter().enumerate() {
            mean[i] += v;
        }
    }
    for v in &mut mean {
        *v /= frames as f32;
    }

    let mut std = vec![0.0f32; bins];
    if frames > 1 {
        for b in 0..bins {
            let m = mean[b];
            let var: f32 = features.column(b).iter().map(|v| (v - m).powi(2)).sum::<f32>() / frames as f32;
            std[b] = var.sqrt();
        }
    }

    let energy: f32 = pcm.iter().map(|x| x * x).sum();
    let log_energy = (energy / pcm.len().max(1) as f32 + 1e-8).ln();
    mean.push(log_energy);
    mean.extend(std);
    l2_normalize(&mut mean);
    mean
}

fn online_cluster(embeddings: &[Vec<f32>], max_speakers: usize) -> Vec<Option<String>> {
    let mut centroids: Vec<Vec<f32>> = Vec::new();
    let mut labels = Vec::with_capacity(embeddings.len());

    for emb in embeddings {
        if emb.is_empty() {
            labels.push(None);
            continue;
        }

        let (best_idx, best_sim) = best_centroid(emb, &centroids);

        let speaker_idx = if best_sim >= MIN_SPEAKER_SIMILARITY {
            if let Some(i) = best_idx {
                update_centroid(&mut centroids[i], emb);
                i
            } else {
                0
            }
        } else if centroids.len() < max_speakers {
            centroids.push(emb.clone());
            centroids.len() - 1
        } else {
            best_idx.unwrap_or(0)
        };

        labels.push(Some(format!("SPEAKER_{speaker_idx:02}")));
    }

    labels
}

fn best_centroid(emb: &[f32], centroids: &[Vec<f32>]) -> (Option<usize>, f32) {
    let mut best_idx = None;
    let mut best_sim = -1.0f32;
    for (i, cen) in centroids.iter().enumerate() {
        let sim = cosine_similarity(emb, cen);
        if sim > best_sim {
            best_sim = sim;
            best_idx = Some(i);
        }
    }
    (best_idx, best_sim)
}

fn update_centroid(centroid: &mut [f32], sample: &[f32]) {
    if centroid.len() != sample.len() {
        return;
    }
    for (c, &s) in centroid.iter_mut().zip(sample.iter()) {
        *c = (*c + s) * 0.5;
    }
    l2_normalize(centroid);
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return -1.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 1e-12 || nb <= 1e-12 {
        return -1.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in v {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_pcm(freq: f32, sample_rate: u32, duration_ms: u32) -> Vec<i16> {
        let n = (sample_rate as f32 * duration_ms as f32 / 1000.0) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                (f32::sin(2.0 * std::f32::consts::PI * freq * t) * 16000.0) as i16
            })
            .collect()
    }

    #[test]
    fn single_chunk_gets_speaker_label() {
        let pcm = tone_pcm(440.0, 16000, 500);
        let ranges = vec![(0, pcm.len())];
        let service = SpeakerEmbeddingService { manager: None };
        let labels = label_chunk_speakers(&pcm, 16000, &ranges, 4, &service);
        assert_eq!(labels.len(), 1);
        assert!(labels[0].as_deref().unwrap().starts_with("SPEAKER_"));
    }

    #[test]
    fn multi_chunk_assigns_speaker_labels() {
        let pcm_a: Vec<i16> = (0..128_000).map(|i| (((i * 17) % 500) as i16).saturating_mul(60)).collect();
        let pcm_b: Vec<i16> = (0..128_000).map(|i| (((i * 43) % 900) as i16).saturating_mul(35)).collect();
        let split = pcm_a.len();
        let mut pcm = pcm_a;
        pcm.extend_from_slice(&pcm_b);
        let ranges = vec![(0, split), (split, pcm.len())];
        let service = SpeakerEmbeddingService { manager: None };
        let labels = label_chunk_speakers(&pcm, 16000, &ranges, 4, &service);
        assert_eq!(labels.len(), 2);
        for label in &labels {
            assert!(label.as_deref().unwrap_or("").starts_with("SPEAKER_"));
        }
    }

    #[test]
    fn empty_chunk_range_is_none() {
        let pcm = tone_pcm(440.0, 16000, 100);
        let ranges = vec![(0, 0)];
        let service = SpeakerEmbeddingService { manager: None };
        let labels = label_chunk_speakers(&pcm, 16000, &ranges, 4, &service);
        assert_eq!(labels[0], None);
    }
}
