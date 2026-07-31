//! Lightweight acoustic speaker diarization for podcast / audiobook imports.
//!
//! Uses mel-filterbank embeddings per VAD chunk and online cosine clustering â€” no extra ONNX model.

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

    /// Whether the high-tier CAM++ embedding model is loaded. Used to pick ONE embedding backend per
    /// file so 192-dim CAM++ and 161-dim fbank vectors are never mixed within a single clustering pass.
    pub fn is_available(&self) -> bool {
        self.manager.is_some()
    }

    pub fn compute_embedding(&self, samples: &[f32], sample_rate: u32) -> Vec<f32> {
        if let Some(ref manager) = self.manager {
            if let Some(stream) = manager.create_stream() {
                // try_from, not `as i32`: a sample_rate above i32::MAX would wrap to a negative rate
                // into the sherpa FFI. Realistic decoded rates are ~16 kHz, so the fallback is unreached.
                stream.accept_waveform(i32::try_from(sample_rate).unwrap_or(i32::MAX), samples);
                manager.compute(&stream).unwrap_or_default()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }
}

/// Assign a speaker label per VAD chunk (`SPEAKER_00`, `SPEAKER_01`, â€¦) for a single PCM buffer.
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
    let embeddings = compute_chunk_embeddings(pcm, sample_rate, chunk_ranges, embedding_service);
    cluster_embeddings(&embeddings, max_speakers)
}

/// Compute one speaker embedding per chunk, WITHOUT clustering. Split out of `label_chunk_speakers`
/// so the streaming import path can accumulate embeddings across decode windows and cluster the WHOLE
/// file in a single pass â€” without this, each 90s window clusters from scratch and the same physical
/// speaker gets a different `SPEAKER_xx` label in different windows.
pub fn compute_chunk_embeddings(
    pcm: &[i16],
    sample_rate: u32,
    chunk_ranges: &[(usize, usize)],
    embedding_service: &SpeakerEmbeddingService,
) -> Vec<Vec<f32>> {
    // Speaker diarization requires the high-tier CAM++ embedding model. The previous fbank fallback
    // (log-mel mean/std statistics) is NOT speaker-discriminative â€” its pairwise cosine for clearly
    // distinct voices is ~0.97â€“0.999, far above the 0.85 clustering threshold â€” so EVERY chunk merged
    // into SPEAKER_00 and a multi-speaker recording exported with one bogus, authoritative-looking
    // speaker (worse than no diarization). When CAM++ is unavailable we therefore emit NO embeddings
    // (each chunk â†’ empty â†’ None label) instead of confidently-wrong labels; the filename-based speaker
    // hint (assign_speaker_from_filename) still applies, and a curator can assign speakers by hand.
    if !embedding_service.is_available() {
        return chunk_ranges.iter().map(|_| Vec::new()).collect();
    }
    let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
    chunk_ranges
        .iter()
        .map(|&(start, end)| {
            let start = start.min(f32_pcm.len());
            let end = end.min(f32_pcm.len());
            if end <= start {
                return Vec::new();
            }
            // An empty result stays empty (None label) â€” never a different-dimension substitute.
            embedding_service.compute_embedding(&f32_pcm[start..end], sample_rate)
        })
        .collect()
}

/// Cluster a sequence of precomputed chunk embeddings into `SPEAKER_xx` labels. Clustering the whole
/// file's embeddings in ONE call keeps each physical speaker on a single label across decode windows.
pub fn cluster_embeddings(embeddings: &[Vec<f32>], max_speakers: u32) -> Vec<Option<String>> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    let max_speakers = max_speakers.clamp(1, 32) as usize;
    online_cluster(embeddings, max_speakers)
}

fn online_cluster(embeddings: &[Vec<f32>], max_speakers: usize) -> Vec<Option<String>> {
    let mut centroids: Vec<Vec<f32>> = Vec::new();
    let mut labels = Vec::with_capacity(embeddings.len());

    for emb in embeddings {
        if emb.is_empty() {
            labels.push(None);
            continue;
        }

        // Round-23 #1: a non-empty but DEGENERATE embedding â€” all-zeros (a near-silent slice that
        // still passed VAD) or containing NaN/Inf â€” must be treated like an empty one. cosine_similarity
        // returns None for both, which `best_centroid` scores as -1.0, so such a chunk never matches any
        // centroid yet gets PUSHED as a brand-new permanent phantom centroid: it wastes a max_speakers
        // slot and, once the budget is exhausted, later chunks fall through to SPEAKER_00, misattributing
        // real speakers. Leave it unlabeled instead.
        let norm_sq: f32 = emb.iter().map(|x| x * x).sum();
        if emb.iter().any(|x| !x.is_finite()) || norm_sq <= 1e-12 {
            labels.push(None);
            continue;
        }

        // Defense-in-depth: never cluster a mismatched-dimension embedding. cosine_similarity returns
        // None on a length mismatch, scored as -1.0, which would force a brand-new phantom speaker for
        // every such chunk.
        // The per-file backend choice already keeps embeddings uniform; if one ever differs from the
        // established centroid dimension, leave it unlabeled rather than manufacture a bogus speaker.
        if centroids.first().is_some_and(|c| c.len() != emb.len()) {
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
        // -1.0 preserves the pre-Option behaviour exactly: below every threshold, so a
        // verdict-less pair never matches a centroid.
        let sim = cosine_similarity(emb, cen).unwrap_or(-1.0);
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

/// None when there is NO VERDICT: empty or dimension-mismatched embeddings, a degenerate norm, or a
/// non-finite result.
///
/// This returned a -1.0 sentinel until the speaker-change splitter needed it. -1.0 is numerically
/// "maximally different", so a caller that cannot tell it apart from a real score treats an ABSENT
/// embedding (CAM++ not installed) as proof of two speakers and splits the clip. Silence about a clip
/// is not evidence against it. `best_centroid` restores the old sentinel at its call site, so
/// clustering behaviour is byte-for-byte unchanged.
///
/// `Some` therefore means a real, comparable score. The first Option version did not hold that: a NaN
/// or infinite input still came back as `Some(NaN)`, which is a SECOND no-verdict value that no caller
/// can spot â€” every comparison against it is false, so `sim < T` reads "same speaker" and `sim >= T`
/// reads "different speaker" from the identical value. Making the function total is the whole point of
/// returning an Option at all.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
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
        return None;
    }
    // Non-finite covers both directions: a NaN/Inf sample poisoning the sums, and finite-but-huge
    // samples whose squares overflow f32 to Inf (making the quotient NaN or a meaningless 0.0).
    Some(dot / (na.sqrt() * nb.sqrt())).filter(|s| s.is_finite())
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in v {
            *x /= norm;
        }
    }
}

/// Below this, two embeddings are different people â€” for the WITHIN-CLIP half-vs-half question only.
///
/// NOT WIRED INTO CHUNKING, and that is a measured decision rather than an omission. Splitting chunks on
/// this signal was built, tested and evaluated against the owner's real recording, and could not be
/// shown to help: three variants gave 12 -> 10 (while violating the segment-length floor), 12 -> 11, and
/// 12 -> 14 mixed chunks out of 139. The evaluation is also length-confounded â€” the threshold below is
/// calibrated on ~14 s clips with 7 s halves, and a split piece has ~3 s halves whose embeddings are
/// noisier and score systematically lower â€” so the comparison cannot be trusted in either direction.
/// See PROGRESS_LEDGER iteration 225 before attempting it again.
///
/// CALIBRATED, not chosen. The owner listened to 15 clips blind â€” shuffled, with the score, the CAM++
/// label and the stratification band all hidden until every answer was in. Every clip he heard as
/// turn-taking scored â‰¤ 0.428; every clip he heard as one speaker scored â‰¥ 0.753. Nothing landed in
/// between, so any threshold in that 0.325-wide empty band classifies the labelled set perfectly and
/// this is simply its midpoint. Misclassifies 0 of 15. The 15 clips and their answers are recorded in
/// `src/bin/speaker_change_probe.rs::GROUND_TRUTH` so this number can be re-checked rather than trusted.
///
/// Deliberately NOT `MIN_SPEAKER_SIMILARITY` (0.85). That one compares a chunk against an AVERAGED,
/// re-normalised centroid; this compares two raw embeddings, which never score that high even for one
/// speaker. Using 0.85 here flagged 100% of the corpus.
pub const SPEAKER_CHANGE_THRESHOLD: f32 = 0.59;

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
    fn cluster_embeddings_does_not_spawn_phantom_speaker_for_mismatched_dim() {
        // Round-13 audit: a mismatched-dimension embedding (a 161-dim fbank chunk mixed in with 192-dim
        // CAM++ centroids) must NOT manufacture a new speaker â€” cosine_similarity has no verdict on a
        // length mismatch, which previously forced a phantom SPEAKER_xx per such chunk. It is left None.
        let a = vec![1.0_f32; 192];
        let b = vec![1.0_f32; 192]; // same speaker as `a`
        let odd = vec![1.0_f32; 161]; // a backend-mismatched chunk
        let labels = cluster_embeddings(&[a, odd, b], 8);
        assert_eq!(labels[0].as_deref(), Some("SPEAKER_00"));
        assert_eq!(labels[1], None, "mismatched-dimension chunk must be unlabeled, not a new speaker");
        assert_eq!(labels[2].as_deref(), Some("SPEAKER_00"), "same-dim same-direction stays one speaker");
    }

    #[test]
    fn cluster_embeddings_does_not_spawn_phantom_speaker_for_degenerate_embedding() {
        // Round-23 #1: a zero-vector or non-finite embedding (a near-silent slice that passed VAD, or a
        // NaN/Inf CAM++ output) must be left unlabeled â€” NOT pushed as a permanent phantom centroid that
        // wastes a max_speakers slot and later misattributes real speakers to SPEAKER_00.
        let a = vec![1.0_f32; 192];
        let zero = vec![0.0_f32; 192];
        let nan = {
            let mut v = vec![1.0_f32; 192];
            v[0] = f32::NAN;
            v
        };
        let b = vec![1.0_f32; 192]; // same speaker as `a`
        let labels = cluster_embeddings(&[a, zero, nan, b], 8);
        assert_eq!(labels[0].as_deref(), Some("SPEAKER_00"));
        assert_eq!(labels[1], None, "zero-norm embedding must be unlabeled, not a new speaker");
        assert_eq!(labels[2], None, "non-finite embedding must be unlabeled, not a new speaker");
        assert_eq!(labels[3].as_deref(), Some("SPEAKER_00"), "the real speaker still clusters to one label");
    }

    #[test]
    fn cosine_similarity_has_no_verdict_rather_than_an_uncomparable_one() {
        // Self-audit of iteration 225's own change. Returning Option is only worth anything if `Some`
        // means "a real, comparable score" â€” and it did not: a NaN/Inf sample came back as Some(NaN),
        // a SECOND no-verdict value that no caller can detect. Every comparison against NaN is false,
        // so `sim < T` reads "same speaker" and `sim >= T` reads "different speaker" off the identical
        // value; which way a chunk goes then depends on how the caller happened to write the test.
        // FAILS before the is_finite filter: every one of these returns Some(NaN).
        let clean = [1.0_f32; 192];

        let mut nan = clean;
        nan[0] = f32::NAN;
        assert_eq!(cosine_similarity(&nan, &clean), None, "NaN input has no verdict");

        let mut inf = clean;
        inf[0] = f32::INFINITY;
        assert_eq!(cosine_similarity(&inf, &clean), None, "Inf input has no verdict");

        // Finite-but-huge: the squares overflow f32 to Inf, so the quotient is not a real score either.
        let huge = [1e30_f32; 192];
        assert_eq!(cosine_similarity(&huge, &huge), None, "an overflowing norm has no verdict");

        // The ordinary paths are untouched: identical vectors still score 1.0, opposites -1.0.
        let a = [1.0_f32, 0.0, 0.0];
        let opp = [-1.0_f32, 0.0, 0.0];
        assert!((cosine_similarity(&a, &a).expect("real score") - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&a, &opp).expect("real score") + 1.0).abs() < 1e-6);
    }

    #[test]
    fn whole_file_clustering_keeps_one_label_per_speaker_across_windows() {
        // Round-10 audit MEDIUM: the streaming import re-clustered diarization per 90s window, so the
        // first speaker of EACH window became SPEAKER_00 and a physical speaker got different labels in
        // different windows. Clustering the WHOLE file's embedding sequence in one pass must keep each
        // speaker on a single label regardless of window boundaries.
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        // Sequence spanning two "windows": window 1 = [A, A, B], window 2 = [B, B, A]. Per-window
        // clustering would relabel window 2's leading B as SPEAKER_00, colliding with window 1's A.
        let whole = vec![a.clone(), a.clone(), b.clone(), b.clone(), b.clone(), a.clone()];
        let labels = cluster_embeddings(&whole, 2);

        let la = labels[0].clone();
        let lb = labels[2].clone();
        assert!(la.is_some() && lb.is_some(), "both speakers labeled: {labels:?}");
        assert_ne!(la, lb, "distinct speakers must get distinct labels");
        assert_eq!(labels[1], la, "A stays A within the first window");
        assert_eq!(labels[5], la, "A in the SECOND window keeps A's label (the fix)");
        assert_eq!(labels[3], lb);
        assert_eq!(labels[4], lb);
    }

    #[test]
    fn diarization_without_campp_produces_no_speaker_labels() {
        // Round-17: without the high-tier CAM++ model (the default install state â€” CAM++ is not
        // bundled), the only available backend was the fbank fallback, whose log-mel-statistic vectors
        // are NOT speaker-discriminative (baseline pairwise cosine ~0.99). It collapsed every chunk into
        // SPEAKER_00 â€” a confidently-wrong single speaker for ANY multi-speaker file, which looks
        // authoritative and corrupts the exported dataset. The honest behavior is to emit NO speaker
        // labels when CAM++ is unavailable, leaving assignment to the filename hint or a human.
        let pcm_a: Vec<i16> = (0..128_000).map(|i| (((i * 17) % 500) as i16).saturating_mul(60)).collect();
        let pcm_b: Vec<i16> = (0..128_000).map(|i| (((i * 43) % 900) as i16).saturating_mul(35)).collect();
        let split = pcm_a.len();
        let mut pcm = pcm_a;
        pcm.extend_from_slice(&pcm_b);
        let ranges = vec![(0, split), (split, pcm.len())];
        let service = SpeakerEmbeddingService { manager: None }; // CAM++ unavailable

        let labels = label_chunk_speakers(&pcm, 16000, &ranges, 4, &service);
        assert_eq!(labels.len(), 2);
        assert!(
            labels.iter().all(Option::is_none),
            "without CAM++, diarization must emit NO labels rather than collapse speakers: {labels:?}"
        );

        // The streaming path (compute_chunk_embeddings directly) is likewise unlabeled â€” no embeddings.
        let embeddings = compute_chunk_embeddings(&pcm, 16000, &ranges, &service);
        assert!(embeddings.iter().all(Vec::is_empty), "no embeddings without CAM++");
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
