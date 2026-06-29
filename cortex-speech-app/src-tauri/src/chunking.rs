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

    // Don't merge VAD regions across a silence longer than this — keeps clips tight around speech
    // (intra-sentence pauses are shorter; a 2s+ gap is a real break worth a chunk boundary).
    const MAX_MERGE_GAP_MS: u32 = 2000;
    let max_gap_samples = ms_to_samples(MAX_MERGE_GAP_MS, sample_rate);
    regions = merge_adjacent_regions(regions, max_samples, max_gap_samples);
    regions = split_oversized_regions(pcm, sample_rate, regions, max_samples, min_samples, pcm.len());
    regions = absorb_short_regions(regions, min_samples, max_samples, max_gap_samples, pcm.len());

    if regions.is_empty() {
        regions = silence_aware_split(pcm, sample_rate, 0, pcm.len(), max_samples, min_samples);
    }

    // Safety: enforce the max-duration cap. A last-resort split still cuts on the quietest
    // point so it never slices through a word (the bug that split "کەسایەتی" across chunks).
    let mut final_regions = Vec::new();
    for (start, end) in regions {
        let s = start.min(pcm.len());
        let e = end.min(pcm.len());
        if e <= s {
            continue;
        }
        if e - s <= max_samples {
            final_regions.push((s, e));
        } else {
            final_regions.extend(silence_aware_split(pcm, sample_rate, s, e, max_samples, min_samples));
        }
    }

    if final_regions.is_empty() {
        final_regions.push((0, pcm.len().min(max_samples)));
    }

    Ok(final_regions)
}

/// Merge consecutive VAD regions when the combined span fits within `max_samples` AND the silence gap
/// between them is no longer than `max_gap_samples`. Without the gap limit, two short utterances with a
/// long pause between them (e.g. 2s speech + 8s silence + 3s speech) merged into one ~13s clip that is
/// mostly silence — the "clip far longer than the words" complaint. Splitting on a long pause keeps each
/// clip tight around contiguous speech while still merging across natural intra-sentence pauses.
fn merge_adjacent_regions(
    regions: Vec<(usize, usize)>,
    max_samples: usize,
    max_gap_samples: usize,
) -> Vec<(usize, usize)> {
    if regions.is_empty() {
        return regions;
    }
    let mut merged = Vec::new();
    let mut cur_start = regions[0].0;
    let mut cur_end = regions[0].1;

    for &(start, end) in regions.iter().skip(1) {
        let combined_len = end.saturating_sub(cur_start);
        let gap = start.saturating_sub(cur_end);
        if combined_len <= max_samples && gap <= max_gap_samples {
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

/// Split any region longer than `max_samples` into sub-ranges, cutting on the quietest
/// point near each boundary so chunk edges fall in pauses rather than mid-word.
fn split_oversized_regions(
    pcm: &[i16],
    sample_rate: u32,
    regions: Vec<(usize, usize)>,
    max_samples: usize,
    min_samples: usize,
    total_len: usize,
) -> Vec<(usize, usize)> {
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
            out.extend(silence_aware_split(pcm, sample_rate, s, e, max_samples, min_samples));
        }
    }
    out
}

/// Split [start, end) into pieces of at most `max_samples`, choosing each cut at the
/// lowest-energy (most pause-like) sample in a window just before the max boundary. This
/// keeps word and sentence boundaries intact instead of guillotining at a fixed offset.
fn silence_aware_split(
    pcm: &[i16],
    sample_rate: u32,
    start: usize,
    end: usize,
    max_samples: usize,
    min_samples: usize,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut s = start;
    while end - s > max_samples {
        // Prefer cutting in the last ~30% of the window, but never sooner than min_samples in.
        let lo = (s + max_samples * 7 / 10).max(s + min_samples).min(end.saturating_sub(1));
        // Keep at least min_samples after the cut so we never leave a tiny trailing fragment.
        let mut hi = s + max_samples;
        if end.saturating_sub(hi) < min_samples {
            hi = end.saturating_sub(min_samples);
        }
        let hi = hi.clamp(lo + 1, end);
        let cut = find_quietest_cut(pcm, lo, hi, sample_rate).clamp(s + 1, end);
        out.push((s, cut));
        s = cut;
    }
    if end > s {
        out.push((s, end));
    }
    out
}

/// Return the sample index within [lo, hi) at the centre of the lowest-energy short frame —
/// the most silence-like place to cut. Returns `lo` when the range is flat or degenerate.
fn find_quietest_cut(pcm: &[i16], lo: usize, hi: usize, sample_rate: u32) -> usize {
    if hi <= lo {
        return lo;
    }
    let half = (ms_to_samples(15, sample_rate) / 2).max(1); // ~15 ms analysis frame
    let step = half.max(1);
    let mut best_idx = lo;
    let mut best_energy = u64::MAX;
    let mut c = lo;
    while c < hi {
        let f_start = c.saturating_sub(half);
        let f_end = (c + half).min(pcm.len());
        let mut energy = 0u64;
        let mut n = 0u64;
        let mut i = f_start;
        while i < f_end {
            let v = pcm[i] as i64;
            energy += (v * v) as u64;
            n += 1;
            i += 1;
        }
        let mean = energy.checked_div(n).unwrap_or(u64::MAX);
        if mean < best_energy {
            best_energy = mean;
            best_idx = c;
        }
        c += step;
    }
    best_idx
}

/// Merge regions shorter than `min_samples` into a neighbor when possible — but NOT across a silence
/// longer than `max_gap_samples`, otherwise this would undo the gap-aware split in
/// `merge_adjacent_regions` and re-create a clip that is mostly silence.
fn absorb_short_regions(
    regions: Vec<(usize, usize)>,
    min_samples: usize,
    max_samples: usize,
    max_gap_samples: usize,
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
                    let (ns, ne) = working[i + 1];
                    if ns.saturating_sub(e) <= max_gap_samples && ne.saturating_sub(s) <= max_samples {
                        working[i + 1] = (s, ne);
                        changed = true;
                        i += 1;
                        continue;
                    }
                }
                if let Some(last) = next.last_mut() {
                    let (ps, pe) = *last;
                    if s.saturating_sub(pe) <= max_gap_samples && e.saturating_sub(ps) <= max_samples {
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
        let (start_ms, end_ms) = (meta.source_start_ms.max(0), meta.source_end_ms.max(0));
        // Reject an absurd offset rather than truncating via `as u32` (i64 -> u32 wraps mod 2^32, which
        // would silently slice an UNRELATED in-bounds window with no error). The app never emits an
        // offset > u32::MAX ms (~49.7 days); a value this large is a malformed or crafted alignment blob.
        if start_ms > u32::MAX as i64 || end_ms > u32::MAX as i64 {
            return Err(AppError::Validation("Chunk time range out of bounds".into()));
        }
        let start = ms_to_samples(start_ms as u32, sample_rate);
        let end = ms_to_samples(end_ms as u32, sample_rate).min(pcm.len());
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
    fn silence_aware_split_covers_range_and_bounds() {
        let sr = 16000;
        let max_samples = ms_to_samples(10_000, sr);
        let min_samples = ms_to_samples(2_000, sr);
        let pcm = vec![4000i16; sr as usize * 50]; // 50 s flat tone
        let parts = silence_aware_split(&pcm, sr, 0, pcm.len(), max_samples, min_samples);
        assert_eq!(parts.first().map(|c| c.0), Some(0));
        assert_eq!(parts.last().map(|c| c.1), Some(pcm.len()));
        for w in parts.windows(2) {
            assert_eq!(w[0].1, w[1].0, "chunks must be contiguous (no gaps/overlaps)");
        }
        assert!(parts.iter().all(|(s, e)| e > s && e - s <= max_samples), "every chunk within max");
        let total: usize = parts.iter().map(|(s, e)| e - s).sum();
        assert_eq!(total, pcm.len(), "chunks must cover the whole range");
    }

    #[test]
    fn silence_aware_split_cuts_on_the_pause_not_mid_word() {
        // Regression for the Nawras bug: a continuous-speech region must be cut at the
        // silent gap, not blindly at the 15 s boundary (which sliced "کەسایەتی" in two).
        let sr = 16000;
        let max_samples = ms_to_samples(15_000, sr);
        let min_samples = ms_to_samples(3_000, sr);
        let mut pcm = vec![6000i16; sr as usize * 20]; // 20 s loud tone
        let gap = ms_to_samples(13_000, sr);
        let gap_half = ms_to_samples(120, sr); // 240 ms silent gap at 13 s
        for s in pcm.iter_mut().take(gap + gap_half).skip(gap - gap_half) {
            *s = 0;
        }
        let parts = silence_aware_split(&pcm, sr, 0, pcm.len(), max_samples, min_samples);
        assert!(parts.len() >= 2);
        let cut_ms = samples_to_ms(parts[0].1, sr);
        assert!((cut_ms - 13_000).abs() < 300, "expected the cut at the ~13 s pause, got {cut_ms}ms");
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
    fn slice_pcm_by_alignment_rejects_out_of_range_offset_instead_of_truncating() {
        // A source_*_ms beyond u32::MAX must error, NOT be truncated by `as u32` (which wraps mod 2^32 and
        // would silently slice an unrelated in-bounds window). The app never emits such an offset; this
        // guards a malformed/crafted alignment blob arriving via an IPC command.
        let pcm: Vec<i16> = (0..32000).map(|i| i as i16).collect();
        let bad = SegmentSourceMeta {
            source_start_ms: u32::MAX as i64 + 1, // ~49.7 days; would truncate to a small in-bounds value
            source_end_ms: u32::MAX as i64 + 2000,
            chunk_index: 0,
            chunk_count: 1,
        };
        assert!(slice_pcm_by_alignment(&pcm, 16000, Some(&bad.to_alignment_json())).is_err());
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
        // Contiguous regions (no gap) still merge up to max_samples.
        let regions = vec![(0, 5000), (5000, 10_000), (10_000, 15_000)];
        let merged = merge_adjacent_regions(regions, 20_000, usize::MAX);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], (0, 15_000));

        let merged2 = merge_adjacent_regions(vec![(0, 12_000), (12_000, 25_000)], 20_000, usize::MAX);
        assert_eq!(merged2.len(), 2);
    }

    #[test]
    fn merge_adjacent_splits_across_a_long_silence_gap() {
        // 2s speech, then an 8s silence, then 3s speech: combined span (13s) fits under max, but the
        // long gap must force a chunk boundary so neither clip is mostly silence.
        let sr = 16_000usize;
        let speech_a = (0, 2 * sr);
        let speech_b = (10 * sr, 13 * sr); // 8s gap before it
        let max_gap = ms_to_samples(2000, sr as u32); // 2s
        let merged = merge_adjacent_regions(vec![speech_a, speech_b], 20 * sr, max_gap);
        assert_eq!(merged.len(), 2, "a long silence gap must split, not merge into one mostly-silent clip");
        assert_eq!(merged[0], speech_a);
        assert_eq!(merged[1], speech_b);
        // A short (sub-gap) pause still merges into one clip.
        let close_b = (2 * sr + sr / 2, 5 * sr); // 0.5s gap
        let merged_close = merge_adjacent_regions(vec![speech_a, close_b], 20 * sr, max_gap);
        assert_eq!(merged_close.len(), 1, "a short intra-sentence pause stays merged");
    }

    #[test]
    fn absorb_short_regions_does_not_merge_across_a_long_silence_gap() {
        // The "2s speech / 8s silence / 3s speech" case where the FIRST region is shorter than min.
        // Without the gap guard, absorb_short_regions re-merged across the 8s silence (undoing the
        // merge_adjacent_regions split) and recreated a mostly-silent 13s clip. It must stay split.
        let (min, max, max_gap) = (48_000usize, 240_000usize, 32_000usize); // 3s / 15s / 2s
        let split = absorb_short_regions(vec![(0, 32_000), (160_000, 208_000)], min, max, max_gap, 208_000);
        assert_eq!(split.len(), 2, "must NOT absorb a short region across an 8s silence");
        // A short region with only a short (sub-gap) pause to its neighbor is still absorbed.
        let absorbed = absorb_short_regions(vec![(0, 32_000), (40_000, 88_000)], min, max, max_gap, 88_000);
        assert_eq!(absorbed.len(), 1, "a short region with a sub-gap neighbor is still absorbed");
    }
}
