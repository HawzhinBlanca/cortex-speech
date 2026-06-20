//! VAD-guided speech chunking for long-form audio (podcasts, audiobooks).
//!
//! Splits decoded PCM into annotatable segments bounded by `min_segment_duration_ms`
//! and `max_segment_duration_ms` from app settings.

use crate::audio;
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};

/// Maximum decoded PCM samples kept in memory (~16.6 min at 16 kHz).
pub const MAX_PCM_SAMPLES: usize = 16_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentSourceMeta {
    pub source_start_ms: i64,
    pub source_end_ms: i64,
    pub chunk_index: u32,
    pub chunk_count: u32,
}

impl SegmentSourceMeta {
    pub fn to_alignment_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_alignment_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        if v.get("source_start_ms").is_some() {
            return serde_json::from_value(v).ok();
        }
        None
    }
}

/// Merge forced-alignment word timestamps into existing chunk metadata JSON.
pub fn merge_word_timestamps(existing: Option<&str>, words: &[crate::aligner::WordTimestamp]) -> String {
    let words_val = serde_json::to_value(words).unwrap_or(serde_json::json!([]));
    match existing.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) {
        Some(mut obj) if obj.is_object() => {
            if let Some(map) = obj.as_object_mut() {
                map.insert("words".to_string(), words_val);
            }
            obj.to_string()
        }
        Some(_) | None => serde_json::json!({ "words": words_val }).to_string(),
    }
}

/// Extract word-level timestamps from alignment JSON (object with `words` or legacy array).
pub fn word_timestamps_from_alignment(s: &str) -> Option<Vec<crate::aligner::WordTimestamp>> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    if v.is_array() {
        return serde_json::from_value(v).ok();
    }
    v.get("words").and_then(|w| serde_json::from_value(w.clone()).ok())
}

pub fn ms_to_samples(ms: u32, sample_rate: u32) -> usize {
    (ms as u64 * sample_rate as u64 / 1000) as usize
}

pub fn samples_to_ms(samples: usize, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    (samples as i64 * 1000) / sample_rate as i64
}

/// Whether the buffer should be split into multiple DB segments.
pub fn needs_chunking(pcm_len: usize, sample_rate: u32, max_segment_ms: u32) -> bool {
    pcm_len > MAX_PCM_SAMPLES || pcm_len > ms_to_samples(max_segment_ms, sample_rate)
}

/// Whether to decode incrementally (time windows) instead of loading full PCM.
pub fn should_stream_decode(duration_ms: i64, max_segment_ms: u32) -> bool {
    if duration_ms <= 0 {
        return false;
    }
    let estimated_samples = (duration_ms as u64 * audio::TARGET_SAMPLE_RATE as u64 / 1000) as usize;
    estimated_samples > MAX_PCM_SAMPLES || duration_ms > (max_segment_ms as i64 * 2)
}

/// Plan sample-index ranges `[start, end)` for transcription/annotation.
pub fn plan_speech_chunks(
    pcm: &[i16],
    sample_rate: u32,
    vad_threshold: f32,
    min_segment_ms: u32,
    max_segment_ms: u32,
) -> AppResult<Vec<(usize, usize)>> {
    if pcm.is_empty() {
        return Ok(Vec::new());
    }

    let min_samples = ms_to_samples(min_segment_ms, sample_rate).max(1);
    let max_samples = ms_to_samples(max_segment_ms, sample_rate).max(min_samples);

    let mut regions = if needs_chunking(pcm.len(), sample_rate, max_segment_ms) {
        audio::voice_activity_detection(pcm, sample_rate, vad_threshold)?
    } else {
        vec![(0, pcm.len())]
    };

    if regions.is_empty() {
        regions.push((0, pcm.len()));
    }

    regions = merge_adjacent_regions(regions, max_samples);
    regions = split_oversized_regions(regions, max_samples, pcm.len());
    regions = absorb_short_regions(regions, min_samples, max_samples, pcm.len());

    if regions.len() == 1 && regions[0].1.saturating_sub(regions[0].0) > max_samples {
        regions = fixed_window_split(regions[0].0, regions[0].1, max_samples);
    } else if regions.is_empty() {
        regions = fixed_window_split(0, pcm.len(), max_samples);
    }

    // Safety: enforce max sample cap per chunk
    let mut final_regions = Vec::new();
    for (start, end) in regions {
        let mut s = start.min(pcm.len());
        let e = end.min(pcm.len());
        while e > s {
            let chunk_end = (s + max_samples).min(e);
            final_regions.push((s, chunk_end));
            s = chunk_end;
        }
    }

    if final_regions.is_empty() {
        final_regions.push((0, pcm.len().min(max_samples)));
    }

    Ok(final_regions)
}

/// Merge consecutive VAD regions if the combined span fits within `max_samples`.
fn merge_adjacent_regions(regions: Vec<(usize, usize)>, max_samples: usize) -> Vec<(usize, usize)> {
    if regions.is_empty() {
        return regions;
    }
    let mut merged = Vec::new();
    let mut cur_start = regions[0].0;
    let mut cur_end = regions[0].1;

    for &(start, end) in regions.iter().skip(1) {
        let combined_len = end.saturating_sub(cur_start);
        if combined_len <= max_samples {
            cur_end = end;
        } else {
            if cur_end > cur_start {
                merged.push((cur_start, cur_end));
            }
            cur_start = cur_end.max(start);
            cur_end = end;
        }
    }
    if cur_end > cur_start {
        merged.push((cur_start, cur_end));
    }
    merged
}

/// Split any region longer than `max_samples` into sub-ranges.
fn split_oversized_regions(regions: Vec<(usize, usize)>, max_samples: usize, total_len: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (start, end) in regions {
        let s = start.min(total_len);
        let e = end.min(total_len);
        if e <= s {
            continue;
        }
        if e - s <= max_samples {
            out.push((s, e));
        } else {
            out.extend(fixed_window_split(s, e, max_samples));
        }
    }
    out
}

fn fixed_window_split(start: usize, end: usize, window: usize) -> Vec<(usize, usize)> {
    let mut chunks = Vec::new();
    let mut s = start;
    while s < end {
        let e = (s + window).min(end);
        if e > s {
            chunks.push((s, e));
        }
        s = e;
    }
    chunks
}

/// Merge regions shorter than `min_samples` into neighbors when possible.
fn absorb_short_regions(
    regions: Vec<(usize, usize)>,
    min_samples: usize,
    max_samples: usize,
    total_len: usize,
) -> Vec<(usize, usize)> {
    if regions.is_empty() {
        return regions;
    }

    let mut working: Vec<(usize, usize)> =
        regions.into_iter().map(|(s, e)| (s.min(total_len), e.min(total_len))).filter(|(s, e)| e > s).collect();

    let mut changed = true;
    while changed && working.len() > 1 {
        changed = false;
        let mut next = Vec::new();
        let mut i = 0;
        while i < working.len() {
            let (s, e) = working[i];
            let len = e - s;
            if len < min_samples {
                if i + 1 < working.len() {
                    let (_ns, ne) = working[i + 1];
                    if ne.saturating_sub(s) <= max_samples {
                        working[i + 1] = (s, ne);
                        changed = true;
                        i += 1;
                        continue;
                    }
                }
                if let Some(last) = next.last_mut() {
                    let (ps, _pe) = *last;
                    if e.saturating_sub(ps) <= max_samples {
                        *last = (ps, e);
                        changed = true;
                        i += 1;
                        continue;
                    }
                }
            }
            next.push((s, e));
            i += 1;
        }
        working = next;
    }

    working.retain(|(s, e)| e > s);
    working
}

/// Slice decoded PCM to a chunk window described by segment alignment metadata.
pub fn slice_pcm_by_alignment(
    pcm: &[i16],
    sample_rate: u32,
    alignment_json: Option<&str>,
) -> AppResult<(Vec<i16>, Option<String>)> {
    if let Some(meta) = alignment_json.and_then(SegmentSourceMeta::from_alignment_json) {
        let start = ms_to_samples(meta.source_start_ms.max(0) as u32, sample_rate);
        let end = ms_to_samples(meta.source_end_ms.max(0) as u32, sample_rate).min(pcm.len());
        if end <= start {
            return Err(AppError::Validation("Invalid chunk time range".into()));
        }
        let suffix = format!("chunk_{start}_{end}");
        Ok((pcm[start..end].to_vec(), Some(suffix)))
    } else {
        Ok((pcm.to_vec(), None))
    }
}

pub fn build_source_meta(
    start_sample: usize,
    end_sample: usize,
    sample_rate: u32,
    chunk_index: u32,
    chunk_count: u32,
) -> SegmentSourceMeta {
    SegmentSourceMeta {
        source_start_ms: samples_to_ms(start_sample, sample_rate),
        source_end_ms: samples_to_ms(end_sample, sample_rate),
        chunk_index,
        chunk_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_to_samples_roundtrip() {
        assert_eq!(ms_to_samples(1000, 16000), 16000);
        assert_eq!(samples_to_ms(16000, 16000), 1000);
        // Defense-in-depth: a zero sample rate must not divide-by-zero panic (pub leaf utility).
        assert_eq!(samples_to_ms(16000, 0), 0);
    }

    #[test]
    fn should_stream_decode_thresholds() {
        let max_ms = 15_000u32;
        assert!(!should_stream_decode(20_000, max_ms));
        assert!(should_stream_decode(31_000, max_ms));
        let long_ms = (MAX_PCM_SAMPLES as i64 * 1000 / 16000) + 1000;
        assert!(should_stream_decode(long_ms, max_ms));
    }

    #[test]
    fn needs_chunking_respects_limits() {
        let max_ms = 15_000u32;
        let under = ms_to_samples(max_ms, 16000) - 1;
        assert!(!needs_chunking(under, 16000, max_ms));
        assert!(needs_chunking(ms_to_samples(max_ms, 16000) + 1, 16000, max_ms));
        assert!(needs_chunking(MAX_PCM_SAMPLES + 1, 16000, max_ms));
    }

    #[test]
    fn fixed_window_split_covers_range() {
        let chunks = fixed_window_split(0, 50_000, 10_000);
        assert_eq!(chunks.first().map(|c| c.0), Some(0));
        assert_eq!(chunks.last().map(|c| c.1), Some(50_000));
        assert!(chunks.iter().all(|(s, e)| e > s && e - s <= 10_000));
        let total: usize = chunks.iter().map(|(s, e)| e - s).sum();
        assert_eq!(total, 50_000);
    }

    #[test]
    fn plan_short_audio_single_chunk() {
        let pcm = vec![1000i16; 16000]; // 1 second
        let chunks = plan_speech_chunks(&pcm, 16000, 0.5, 500, 15_000).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], (0, pcm.len()));
    }

    #[test]
    fn plan_long_audio_multiple_chunks() {
        // ~40s of non-silence — exceeds 15s max and produces multiple chunks (keep short for Silero VAD in CI)
        let pcm = vec![8000i16; 16000 * 40];
        let chunks = plan_speech_chunks(&pcm, 16000, 0.5, 3000, 15_000).unwrap();
        assert!(chunks.len() > 1, "expected multiple chunks, got {}", chunks.len());
        assert_eq!(chunks.first().unwrap().0, 0);
        assert_eq!(chunks.last().unwrap().1, pcm.len());
        for (s, e) in &chunks {
            let dur_ms = samples_to_ms(e - s, 16000);
            assert!(dur_ms <= 15_500, "chunk duration {dur_ms}ms exceeds max");
            assert!(e > s);
        }
    }

    #[test]
    fn slice_pcm_by_alignment_extracts_window() {
        let pcm: Vec<i16> = (0..32000).map(|i| i as i16).collect();
        let meta = SegmentSourceMeta { source_start_ms: 500, source_end_ms: 1500, chunk_index: 0, chunk_count: 1 };
        let (slice, suffix) = slice_pcm_by_alignment(&pcm, 16000, Some(&meta.to_alignment_json())).unwrap();
        assert_eq!(slice.len(), 16000);
        assert!(suffix.as_deref().unwrap().starts_with("chunk_"));
    }

    #[test]
    fn segment_meta_json_roundtrip() {
        let meta = SegmentSourceMeta { source_start_ms: 0, source_end_ms: 15_000, chunk_index: 0, chunk_count: 3 };
        let json = meta.to_alignment_json();
        let parsed = SegmentSourceMeta::from_alignment_json(&json).unwrap();
        assert_eq!(parsed, meta);
    }

    #[test]
    fn merge_adjacent_respects_max() {
        let regions = vec![(0, 5000), (5000, 10_000), (10_000, 15_000)];
        let merged = merge_adjacent_regions(regions, 20_000);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], (0, 15_000));

        let merged2 = merge_adjacent_regions(vec![(0, 12_000), (12_000, 25_000)], 20_000);
        assert_eq!(merged2.len(), 2);
    }
}
