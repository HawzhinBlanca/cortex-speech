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

/// Update a segment's source bounds in its alignment JSON, PRESERVING everything else in the object.
///
/// This used to REBUILD the JSON from `SegmentSourceMeta` (4 fields) and then re-merge one whitelisted
/// key, `words`. That shape drops every key not on the whitelist, and the whitelist has to be extended by
/// hand each time any writer adds one — a silent-data-loss design where the failure mode is invisible
/// (no error, no log, the key is simply gone the next time a reviewer nudges a boundary). It is also the
/// odd one out: `merge_word_timestamps` directly above already preserves-and-inserts.
///
/// Measured before changing it, so this is honest about what it fixes: across all 144 segments in the
/// owner's live library the only keys present are `source_start_ms`, `source_end_ms`, `chunk_index`,
/// `chunk_count` and `words` — every one of them on the old whitelist. So **no data was being lost
/// today**; this closes the hazard, it does not recover anything. The trim path
/// (`update_segment_bounds`) is the reviewer's most-used edit, which is why the hazard is worth closing
/// before a key gets added rather than after.
///
/// Word timestamps are absolute source-time positions and stay valid across a bounds change (a reviewer
/// can re-run alignment to refresh them).
pub fn rebound_alignment_json(existing: Option<&str>, start_ms: i64, end_ms: i64) -> String {
    let mut obj = match existing.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) {
        // Legacy shape: a bare array IS the word list. Lift it under `words` so there is exactly one
        // representation from here on, instead of two the rest of the file has to keep distinguishing.
        Some(serde_json::Value::Array(words)) => serde_json::json!({ "words": words }),
        Some(v) if v.is_object() => v,
        // Absent, unparseable, or a scalar: start clean rather than propagate something no reader can
        // use. Unparseable JSON cannot be preserved without preserving the corruption with it.
        _ => serde_json::json!({}),
    };
    let Some(map) = obj.as_object_mut() else {
        return obj.to_string();
    };
    map.insert("source_start_ms".to_string(), serde_json::json!(start_ms));
    map.insert("source_end_ms".to_string(), serde_json::json!(end_ms));
    // Defaults ONLY when absent: a real chunk_index/chunk_count describes where this segment came from
    // in its source file and a bounds edit does not change that.
    map.entry("chunk_index").or_insert(serde_json::json!(0));
    map.entry("chunk_count").or_insert(serde_json::json!(1));
    obj.to_string()
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
) -> AppResult<(Vec<(usize, usize)>, audio::VadBackend)> {
    if pcm.is_empty() {
        return Ok((Vec::new(), audio::VadBackend::None));
    }

    let min_samples = ms_to_samples(min_segment_ms, sample_rate).max(1);
    let max_samples = ms_to_samples(max_segment_ms, sample_rate).max(min_samples);

    // The backend is decided at the SOURCE — whether VAD actually ran. The post-processing below only
    // reshapes the regions (merge/split/absorb), so `vad_backend` stays what the detector reported. A file
    // short enough to skip chunking takes the whole buffer as one region: no VAD ran -> None.
    let (mut regions, vad_backend) = if needs_chunking(pcm.len(), sample_rate, max_segment_ms) {
        audio::voice_activity_detection(pcm, sample_rate, vad_threshold)?
    } else {
        (vec![(0, pcm.len())], audio::VadBackend::None)
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
    //
    // The tolerance is `cap_with_overrun`, NOT `max_samples`. `silence_aware_split` is allowed to run a
    // chunk slightly long to reach a real pause; re-splitting at the bare cap here would faithfully undo
    // every one of those decisions and leave the change doing nothing at all.
    let cap = cap_with_overrun(max_samples, sample_rate);
    let mut final_regions = Vec::new();
    for (start, end) in regions {
        let s = start.min(pcm.len());
        let e = end.min(pcm.len());
        if e <= s {
            continue;
        }
        if e - s <= cap {
            final_regions.push((s, e));
        } else {
            final_regions.extend(silence_aware_split(pcm, sample_rate, s, e, max_samples, min_samples));
        }
    }

    if final_regions.is_empty() {
        final_regions.push((0, pcm.len().min(max_samples)));
    }

    Ok((final_regions, vad_backend))
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
        // Same tolerance as the final pass in plan_speech_chunks, and for the same reason: a region that
        // is already within cap+overrun has nothing to gain from being cut again.
        if e - s <= cap_with_overrun(max_samples, sample_rate) {
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
    // Computed ONCE per region, not per cut: the threshold must describe this passage's speech level,
    // and recomputing it inside the loop would make it drift with whatever happens to be in each band.
    let ref_rms = region_speech_level(pcm, start, end, sample_rate);
    let overrun = ms_to_samples(MAX_OVERRUN_MS, sample_rate);
    while end - s > max_samples {
        // Prefer cutting in the last ~30% of the window, but never sooner than min_samples in.
        let lo = (s + max_samples * 7 / 10).max(s + min_samples).min(end.saturating_sub(1));
        // Keep at least min_samples after the cut so we never leave a tiny trailing fragment.
        let mut hi = s + max_samples;
        if end.saturating_sub(hi) < min_samples {
            hi = end.saturating_sub(min_samples);
        }
        let hi = hi.clamp(lo + 1, end);
        let cut = find_pause_cut(pcm, lo, hi, sample_rate, ref_rms)
            // No real pause inside the cap. Look a bounded distance FURTHER rather than cutting between
            // two syllables purely because the clock ran out — a slightly long clip is reviewable, a clip
            // that starts 20 ms into a word is not. Still bounded, and still leaves min_samples behind.
            .or_else(|| {
                let stretched = hi.saturating_add(overrun).min(end.saturating_sub(min_samples));
                (stretched > hi).then(|| find_pause_cut(pcm, hi, stretched, sample_rate, ref_rms)).flatten()
            })
            // Continuous speech, music or noise: nothing here is a pause, so fall back to the old rule.
            .unwrap_or_else(|| find_quietest_cut(pcm, lo, hi, sample_rate))
            .clamp(s + 1, end);
        out.push((s, cut));
        s = cut;
    }
    if end > s {
        out.push((s, end));
    }
    out
}

/// How far past `max_samples` a chunk may run when its band contains no real pause.
///
/// Measured on the owner's corpus (1,799,631 ms of speech, 143 boundaries): a genuine >=300 ms pause is
/// already inside the normal band for 74% of boundaries, and allowing a bounded overrun lifts that to
/// **85% at +3 s** and 92% at +5 s. 3 s is the knee — past it the return per second of slack drops off
/// while the clip gets long enough to be tiring to review. Running slightly long is far cheaper than
/// cutting between two syllables because the clock ran out.
const MAX_OVERRUN_MS: u32 = 3000;

/// How far below the region's own speech level a frame must sit to count as silence.
///
/// Calibration knob, set from the same measurement: at -25 dB, 21.4% of that file reads as silence,
/// which matches its audible pause structure. Too shallow and ordinary vowel dips read as pauses; too
/// deep and only absolute digital silence qualifies, which real recordings rarely contain.
const PAUSE_THRESHOLD_DB: f64 = -25.0;

/// The narrowest gap that counts as a real pause rather than an inter-syllable dip.
///
/// This is the whole point of the change. Measured on the same corpus: **23 of 143 existing cuts (16%)
/// sit in a gap of 100 ms or less** — far too short to be a pause — because the old rule took the single
/// quietest FRAME and a 30 ms dip between syllables wins that contest against a 400 ms pause a moment
/// later. Requiring a run this wide is what rules those out.
const MIN_PAUSE_MS: u32 = 120;

/// Total length a chunk may reach: the configured cap plus the bounded overrun above.
///
/// Every place that re-splits an over-long region must use THIS, not `max_samples` — otherwise the
/// final cap pass in `plan_speech_chunks` faithfully undoes every overrun `silence_aware_split` just
/// chose, and the whole change silently does nothing.
fn cap_with_overrun(max_samples: usize, sample_rate: u32) -> usize {
    max_samples.saturating_add(ms_to_samples(MAX_OVERRUN_MS, sample_rate))
}

/// RMS of `pcm[start..end)`. 0.0 for an empty range.
fn frame_rms(pcm: &[i16], start: usize, end: usize) -> f64 {
    if end <= start || start >= pcm.len() {
        return 0.0;
    }
    let end = end.min(pcm.len());
    let mut energy = 0f64;
    for &v in &pcm[start..end] {
        energy += (v as f64) * (v as f64);
    }
    (energy / (end - start) as f64).sqrt()
}

/// The region's own speech level, as the MEDIAN 15 ms frame RMS.
///
/// Median, not mean: about a fifth of a typical region is silence, and a mean is dragged down by it —
/// which would drag the silence threshold down with it and make real pauses fail to qualify. Measuring
/// per region rather than globally is what lets one quiet passage and one loud one both be cut well.
fn region_speech_level(pcm: &[i16], start: usize, end: usize, sample_rate: u32) -> f64 {
    let frame = ms_to_samples(15, sample_rate).max(1);
    let mut rms: Vec<f64> = Vec::new();
    let mut c = start;
    while c + frame <= end.min(pcm.len()) {
        rms.push(frame_rms(pcm, c, c + frame));
        c += frame;
    }
    if rms.is_empty() {
        return 0.0;
    }
    rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    rms[rms.len() / 2]
}

/// The centre of the WIDEST run of silence in [lo, hi), or None when nothing there is a real pause.
///
/// This replaces "the single quietest frame" as the cut criterion, and the difference is the fix.
/// Measured on the owner's corpus: no cut ever landed on speech (every one is >=37.7 dB below local
/// speech, median -51.7 dB), so the old rule was never guillotining a word at full volume — but 16% of
/// cuts landed in gaps of <=100 ms, and 56 of 143 boundaries had a WIDER silence run available in the
/// same band. Widening the search window instead buys a median of 0.0 dB: the window was never the
/// problem. Picking the widest run, and cutting at its CENTRE rather than its edge, puts the boundary
/// in the middle of a real pause with air on both sides.
fn find_pause_cut(pcm: &[i16], lo: usize, hi: usize, sample_rate: u32, ref_rms: f64) -> Option<usize> {
    find_pauses(pcm, lo, hi, sample_rate, ref_rms).first().map(|(centre, _)| *centre)
}

/// EVERY qualifying pause in [lo, hi) as `(centre, width)`, WIDEST FIRST.
///
/// `find_pause_cut` is this with `.first()`. Split out so the widest-run scan has exactly one
/// implementation and the "all pauses" form is available without duplicating it.
fn find_pauses(pcm: &[i16], lo: usize, hi: usize, sample_rate: u32, ref_rms: f64) -> Vec<(usize, usize)> {
    let hi = hi.min(pcm.len());
    if hi <= lo || ref_rms <= 0.0 {
        return Vec::new();
    }
    let frame = ms_to_samples(15, sample_rate).max(1);
    let hop = ms_to_samples(5, sample_rate).max(1); // overlapping frames, so a short pause is not straddled
    let threshold = ref_rms * 10f64.powf(PAUSE_THRESHOLD_DB / 20.0);
    let min_pause = ms_to_samples(MIN_PAUSE_MS, sample_rate).max(1);
    let mut out: Vec<(usize, usize)> = Vec::new();
    let push = |rs: usize, re: usize, out: &mut Vec<(usize, usize)>| {
        if re - rs >= min_pause {
            out.push((rs + (re - rs) / 2, re - rs));
        }
    };
    let mut run_start: Option<usize> = None;
    let mut c = lo;
    while c < hi {
        if frame_rms(pcm, c, c + frame) < threshold {
            run_start.get_or_insert(c);
        } else if let Some(rs) = run_start.take() {
            push(rs, c, &mut out);
        }
        c += hop;
    }
    if let Some(rs) = run_start {
        push(rs, hi, &mut out);
    }
    out.sort_by_key(|(_, width)| std::cmp::Reverse(*width)); // widest first
    out
}

/// Return the sample index within [lo, hi) at the centre of the lowest-energy short frame —
/// the most silence-like place to cut. Returns `lo` when the range is flat or degenerate.
///
/// Kept as the LAST RESORT behind `find_pause_cut`: when a band genuinely contains no pause (continuous
/// speech, music, noise), something still has to give and the quietest frame is the least-bad choice.
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
    match alignment_json {
        // Truly no alignment metadata -> a single-file segment; the whole file IS the clip.
        None => Ok((pcm.to_vec(), None)),
        Some(json) => match SegmentSourceMeta::from_alignment_json(json) {
            Some(meta) => {
                let (start_ms, end_ms) = (meta.source_start_ms.max(0), meta.source_end_ms.max(0));
                // Reject an absurd offset rather than truncating via `as u32` (i64 -> u32 wraps mod 2^32,
                // which would silently slice an UNRELATED in-bounds window with no error). The app never
                // emits an offset > u32::MAX ms (~49.7 days); a value this large is a malformed/crafted blob.
                if start_ms > u32::MAX as i64 || end_ms > u32::MAX as i64 {
                    return Err(AppError::Validation("Chunk time range out of bounds".into()));
                }
                let start = ms_to_samples(start_ms as u32, sample_rate);
                let end = ms_to_samples(end_ms as u32, sample_rate).min(pcm.len());
                if end <= start {
                    return Err(AppError::Validation("Invalid chunk time range".into()));
                }
                // Round-22 #12: key the per-chunk cache on the STORED ms range, not the sample indices. The
                // import write-side derives its key from the same source_start_ms/source_end_ms, so the keys
                // match exactly; a sample-index key never matched here because `start`/`end` round-trip
                // sample -> ms -> sample, so the cache missed on every re-transcribe.
                let suffix = format!("chunk_{}_{}", meta.source_start_ms, meta.source_end_ms);
                Ok((pcm[start..end].to_vec(), Some(suffix)))
            }
            // Present but no source offsets (a clobbered chunk: a broken/legacy aligner overwrote the
            // SegmentSourceMeta with a bare word array). Do NOT fall back to the whole file — re-transcribing
            // a chunk against the ENTIRE recording pairs the chunk's transcript with whole-file audio, the
            // whole-file-vs-clip training-data corruption that slice_for_export already SKIPS on the export
            // side. Import always writes a SegmentSourceMeta (with source_start_ms, even for a chunk_count=1
            // single-file segment), so a genuine whole-file segment has alignment_json = None (handled
            // above), never a present-but-offset-less blob. Refuse loudly instead of silently mis-slicing.
            None => Err(AppError::Validation(
                "Alignment metadata is present but missing source offsets (clobbered chunk metadata); \
                 refusing to transcribe this chunk against the whole file"
                    .into(),
            )),
        },
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

/// Stitch two adjacent chunk transcripts produced from OVERLAPPING audio, removing the boundary
/// words the overlap duplicated (audit P1 #4: "add limited overlap ... and duplicate removal;
/// regression-test boundary words on long recordings").
///
/// PURE core only. Today `plan_speech_chunks` emits CONTIGUOUS, non-overlapping chunks, so nothing
/// calls this yet — wiring a small acoustic overlap into the chunk plan and merging the resulting
/// per-chunk transcripts through this function runs in the pipeline (Codex-owned pipeline.rs) and is
/// the documented follow-up. This function is the tested, ready seam for that work.
///
/// Algorithm: find the LARGEST k in `1..=max_overlap_words` such that the last k normalized words of
/// `prev` equal the first k normalized words of `next`, and drop that duplicated prefix from `next`.
/// Comparison is on `char_only`-normalized words (Kaf/Yeh/Heh folding, whitespace) so an orthographic
/// variant at the seam still dedups; the KEPT surface form is `prev`'s. No overlap found → a plain
/// space-join (never drops real content). Empty inputs pass the other side through unchanged.
pub fn stitch_overlapping_transcripts(prev: &str, next: &str, max_overlap_words: usize) -> String {
    let prev_words: Vec<&str> = prev.split_whitespace().collect();
    let next_words: Vec<&str> = next.split_whitespace().collect();
    if prev_words.is_empty() {
        return next.trim().to_string();
    }
    if next_words.is_empty() {
        return prev.trim().to_string();
    }
    let normalizer = crate::normalizer::SoraniNormalizer::char_only();
    let norm = |w: &str| normalizer.normalize(w);
    let prev_norm: Vec<String> = prev_words.iter().map(|w| norm(w)).collect();
    let next_norm: Vec<String> = next_words.iter().map(|w| norm(w)).collect();

    let max_k = max_overlap_words.min(prev_words.len()).min(next_words.len());
    let mut overlap = 0;
    for k in 1..=max_k {
        if prev_norm[prev_norm.len() - k..] == next_norm[..k] {
            overlap = k; // keep scanning for the LARGEST matching overlap
        }
    }
    // prev's surface words + next's words past the duplicated boundary.
    let mut out: Vec<&str> = prev_words.clone();
    out.extend_from_slice(&next_words[overlap..]);
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebound_alignment_json_preserves_word_timestamps_on_a_bounds_edit() {
        // Editing a segment's source bounds must NOT drop the forced-alignment word array. Start from an
        // aligned clip's JSON (meta + words), rebound it to a new window, and assert both the words survive
        // and the source bounds updated (chunk_index/chunk_count preserved). Round-tripping through
        // SegmentSourceMeta alone strips words — the flat-overwrite pipeline.rs:2068 forbids.
        let meta = SegmentSourceMeta { source_start_ms: 1000, source_end_ms: 2000, chunk_index: 1, chunk_count: 3 };
        let words = vec![
            crate::aligner::WordTimestamp { word: "سڵاو".into(), start: 0.1, end: 0.4, confidence: 0.9 },
            crate::aligner::WordTimestamp { word: "دنیا".into(), start: 0.5, end: 0.8, confidence: 0.8 },
        ];
        let aligned = merge_word_timestamps(Some(&meta.to_alignment_json()), &words);

        let rebounded = rebound_alignment_json(Some(&aligned), 1500, 2500);

        let new_meta = SegmentSourceMeta::from_alignment_json(&rebounded).expect("rebounded json parses as meta");
        assert_eq!(new_meta.source_start_ms, 1500, "source_start_ms must update");
        assert_eq!(new_meta.source_end_ms, 2500, "source_end_ms must update");
        assert_eq!(new_meta.chunk_index, 1, "chunk_index must be preserved");
        assert_eq!(new_meta.chunk_count, 3, "chunk_count must be preserved");

        let kept = word_timestamps_from_alignment(&rebounded).expect("words must survive the rebound");
        assert_eq!(kept.len(), 2, "both word timestamps must survive a bounds edit, not be dropped");
        assert_eq!(kept[0].word, "سڵاو");
        assert_eq!(kept[1].word, "دنیا");
    }

    #[test]
    fn a_bounds_edit_preserves_every_key_not_just_the_ones_we_thought_of() {
        // The test above pins `words` because `words` is what was lost once. That is a whitelist, and a
        // whitelist is the bug: the old implementation REBUILT this JSON from SegmentSourceMeta's four
        // fields and re-merged exactly one key, so anything a future writer added was dropped silently on
        // the reviewer's most-used edit — no error, no log, the key simply gone the next time someone
        // nudged a boundary.
        //
        // Measured before the change: across all 144 segments in the owner's live library the only keys
        // present are the four meta fields plus `words`, so nothing was actually being lost yet. This test
        // exists so it stays that way once something new IS written here.
        let existing = serde_json::json!({
            "source_start_ms": 1000,
            "source_end_ms": 2000,
            "chunk_index": 2,
            "chunk_count": 5,
            "words": [{ "word": "سڵاو", "start": 0.1, "end": 0.4, "confidence": 0.9 }],
            // Keys no current writer produces. That is the point — they stand in for whatever gets added
            // next, and the assertion is that nobody has to remember to extend a list for them to survive.
            "alignment_backend": "mms-onnx",
            "overlap_detected": true,
            "nested": { "keep": ["me"] },
        })
        .to_string();

        let out = rebound_alignment_json(Some(&existing), 4000, 5000);
        let v: serde_json::Value = serde_json::from_str(&out).expect("output is valid json");

        assert_eq!(v["source_start_ms"], 4000, "the bounds are what a rebound is FOR");
        assert_eq!(v["source_end_ms"], 5000);
        assert_eq!(v["chunk_index"], 2, "provenance of the chunk does not change with its bounds");
        assert_eq!(v["chunk_count"], 5);
        assert_eq!(v["words"].as_array().map(Vec::len), Some(1), "the once-lost key still survives");
        assert_eq!(v["alignment_backend"], "mms-onnx", "an unknown scalar key must survive");
        assert_eq!(v["overlap_detected"], true, "an unknown bool must survive");
        assert_eq!(v["nested"]["keep"][0], "me", "an unknown nested object must survive intact");

        // Legacy shape: some rows predate the object form and ARE a bare word array. They must be lifted
        // into the object rather than thrown away, or the oldest segments in the library lose their words
        // the first time anyone trims them.
        let legacy = r#"[{"word":"کۆن","start":0.0,"end":0.5,"confidence":0.7}]"#;
        let lifted: serde_json::Value = serde_json::from_str(&rebound_alignment_json(Some(legacy), 10, 20)).unwrap();
        assert_eq!(lifted["words"].as_array().map(Vec::len), Some(1), "a legacy bare array is lifted, not lost");
        assert_eq!(lifted["source_start_ms"], 10);
        assert_eq!(lifted["chunk_index"], 0, "absent provenance defaults, present provenance is kept");
        assert_eq!(lifted["chunk_count"], 1);

        // Absent or unparseable input must still yield usable meta rather than propagating corruption.
        for input in [None, Some("not json at all"), Some("42")] {
            let fresh: serde_json::Value = serde_json::from_str(&rebound_alignment_json(input, 7, 9)).unwrap();
            assert_eq!(fresh["source_start_ms"], 7, "input {input:?} must still produce valid bounds");
            assert_eq!(fresh["source_end_ms"], 9);
            assert_eq!(fresh["chunk_count"], 1);
        }
    }

    #[test]
    fn ms_to_samples_roundtrip() {
        assert_eq!(ms_to_samples(1000, 16000), 16000);
        assert_eq!(samples_to_ms(16000, 16000), 1000);
        // Defense-in-depth: a zero sample rate must not divide-by-zero panic (pub leaf utility).
        assert_eq!(samples_to_ms(16000, 0), 0);
    }

    #[test]
    fn stitch_dedups_the_overlapping_boundary_words() {
        // The overlap re-transcribed "چوارەم پێنجەم" at the seam; the merge keeps ONE copy.
        let prev = "یەکەم دووەم سێیەم چوارەم پێنجەم";
        let next = "چوارەم پێنجەم شەشەم حەوتەم";
        assert_eq!(stitch_overlapping_transcripts(prev, next, 6), "یەکەم دووەم سێیەم چوارەم پێنجەم شەشەم حەوتەم");
    }

    #[test]
    fn stitch_takes_the_largest_overlap_not_the_first() {
        // A single word repeats mid-transcript AND at the true 2-word seam; the largest run wins so
        // the real boundary dedups without eating the earlier legitimate repeat.
        let prev = "ئاو زۆر ئاو گەرم";
        let next = "ئاو گەرم دەبێ";
        assert_eq!(stitch_overlapping_transcripts(prev, next, 4), "ئاو زۆر ئاو گەرم دەبێ");
    }

    #[test]
    fn stitch_dedups_across_an_orthographic_variant_at_the_seam() {
        // Arabic Kaf/Yeh vs Kurdish Keheh/Yeh for "کوردی" — char_only folding makes them equal, so
        // the seam still dedups. The KEPT surface form is prev's.
        let prev = "زمانی كوردی"; // Arabic Kaf U+0643 + Arabic Yeh U+064A
        let next = "کوردی خۆشە"; // Kurdish Keheh U+06A9 + Kurdish Yeh U+06CC
        assert_eq!(stitch_overlapping_transcripts(prev, next, 3), "زمانی كوردی خۆشە");
    }

    #[test]
    fn stitch_with_no_overlap_concatenates_without_dropping_content() {
        assert_eq!(stitch_overlapping_transcripts("ئەم", "ئەوە باشە", 4), "ئەم ئەوە باشە");
        assert_eq!(stitch_overlapping_transcripts("", "دەست", 4), "دەست");
        assert_eq!(stitch_overlapping_transcripts("دەست", "", 4), "دەست");
        // max_overlap_words caps the look-back: a real 3-word overlap ("b c d") EXCEEDS cap=2, so
        // no seam is detected and content is concatenated intact (never partially dropped) — the safe
        // failure mode. In practice the cap is set generously above the expected acoustic overlap.
        assert_eq!(stitch_overlapping_transcripts("a b c d", "b c d e", 2), "a b c d b c d e");
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

    /// `len_ms` of tone at `level`, 16 kHz.
    fn tone(len_ms: u32, level: i16) -> Vec<i16> {
        vec![level; ms_to_samples(len_ms, 16000)]
    }

    /// Overwrite [at_ms, at_ms + width_ms) with `level`.
    fn put(pcm: &mut [i16], at_ms: u32, width_ms: u32, level: i16) {
        let a = ms_to_samples(at_ms, 16000);
        let b = (a + ms_to_samples(width_ms, 16000)).min(pcm.len());
        for s in pcm.iter_mut().take(b).skip(a) {
            *s = level;
        }
    }

    #[test]
    fn a_wide_pause_beats_a_deeper_but_narrower_dip() {
        // THE measured defect. The old rule took the single quietest FRAME, so a 30 ms inter-syllable
        // dip — which can be dead silent — outranked a 400 ms real pause a couple of seconds later.
        // Across the owner's corpus that put 23 of 143 cuts (16%) into gaps of <=100 ms, and left 39 of
        // 143 chunks (27%) starting within 30 ms of the next word's onset.
        //
        // The dip here is DEEPER than the pause (0 vs 50), so the old quietest-frame rule must prefer
        // it and the new widest-run rule must not. Anything that merely tied would not discriminate.
        let sr = 16000;
        let mut pcm = tone(25_000, 6000);
        put(&mut pcm, 11_000, 30, 0); // razor-thin, absolutely silent, and WRONG
        put(&mut pcm, 12_800, 400, 50); // a real breath: quiet, wide, and RIGHT

        let max_samples = ms_to_samples(15_000, sr);
        let min_samples = ms_to_samples(3_000, sr);
        let parts = silence_aware_split(&pcm, sr, 0, pcm.len(), max_samples, min_samples);

        let cut_ms = samples_to_ms(parts[0].1, sr);
        assert!(
            (cut_ms - 13_000).abs() < 250,
            "the cut must land in the middle of the 400ms pause at ~13s, got {cut_ms}ms \
             ({}ms means it chose the 30ms dip — the exact defect this replaced)",
            cut_ms
        );
        // Cutting at the CENTRE of the pause, not its edge, is what leaves air on both sides.
        assert!(cut_ms > 12_800, "a cut at the pause's leading edge clips the outgoing word");
        assert!(cut_ms < 13_200, "a cut at the pause's trailing edge clips the incoming word");
    }

    #[test]
    fn a_chunk_may_run_slightly_long_to_reach_a_real_pause_but_stays_bounded() {
        // When the cap falls in continuous speech and the nearest real pause is just past it, running a
        // little long beats cutting between two syllables: a slightly long clip is reviewable, one that
        // starts 20 ms into a word is not. Measured payoff on the owner's corpus: +3s lifts boundaries
        // landing on a genuine >=300ms pause from 74% to 85%.
        let sr = 16000;
        let mut pcm = tone(30_000, 6000);
        put(&mut pcm, 16_400, 400, 0); // the only pause, 1.4s PAST the 15s cap

        let max_samples = ms_to_samples(15_000, sr);
        let min_samples = ms_to_samples(3_000, sr);
        let parts = silence_aware_split(&pcm, sr, 0, pcm.len(), max_samples, min_samples);

        let cut_ms = samples_to_ms(parts[0].1, sr);
        assert!((cut_ms - 16_600).abs() < 250, "it must reach past the cap to the real pause, got {cut_ms}ms");

        // BOUNDED, and this is the assertion that matters: an unbounded reach would let one bad file
        // produce a single enormous clip. Every chunk stays inside cap + overrun.
        let ceiling = cap_with_overrun(max_samples, sr);
        assert!(
            parts.iter().all(|(s, e)| e - s <= ceiling),
            "every chunk must stay within cap+overrun ({}ms); got {:?}",
            samples_to_ms(ceiling, sr),
            parts.iter().map(|(s, e)| samples_to_ms(e - s, sr)).collect::<Vec<_>>()
        );
        // The invariants the rest of the pipeline depends on survive the overrun.
        for w in parts.windows(2) {
            assert_eq!(w[0].1, w[1].0, "chunks must stay contiguous");
        }
        assert_eq!(parts.iter().map(|(s, e)| e - s).sum::<usize>(), pcm.len(), "coverage must be total");
    }

    #[test]
    fn the_overrun_survives_the_final_cap_pass() {
        // The easy thing to get wrong: silence_aware_split reaches past the cap for a real pause, and
        // then the safety re-split in plan_speech_chunks faithfully cuts it back at the bare cap — the
        // change compiles, every unit test on the splitter passes, and the shipped behaviour is
        // unchanged. This drives the WHOLE planner and asserts the overrun is still there at the end.
        let sr = 16000;
        let mut pcm = tone(30_000, 6000);
        put(&mut pcm, 16_400, 400, 0);
        let (chunks, _) = plan_speech_chunks(&pcm, sr, 0.5, 3_000, 15_000).unwrap();
        let max_samples = ms_to_samples(15_000, sr);
        assert!(
            chunks.iter().any(|(s, e)| e - s > max_samples),
            "a chunk should have run past the cap to reach the pause; lengths were {:?}",
            chunks.iter().map(|(s, e)| samples_to_ms(e - s, sr)).collect::<Vec<_>>()
        );
        let ceiling = cap_with_overrun(max_samples, sr);
        assert!(chunks.iter().all(|(s, e)| e - s <= ceiling), "...but never past cap+overrun");
    }

    #[test]
    fn plan_short_audio_single_chunk() {
        let pcm = vec![1000i16; 16000]; // 1 second
        let (chunks, backend) = plan_speech_chunks(&pcm, 16000, 0.5, 500, 15_000).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], (0, pcm.len()));
        // A file below the chunking threshold skips VAD entirely — the whole buffer is one region.
        assert_eq!(backend, audio::VadBackend::None, "short-file whole-buffer path runs no VAD");
    }

    #[test]
    fn plan_long_audio_multiple_chunks() {
        // ~40s of non-silence — exceeds 15s max and produces multiple chunks (keep short for Silero VAD in CI)
        let pcm = vec![8000i16; 16000 * 40];
        let (chunks, _backend) = plan_speech_chunks(&pcm, 16000, 0.5, 3000, 15_000).unwrap();
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
    fn slice_pcm_by_alignment_refuses_present_but_offsetless_alignment() {
        // A clobbered chunk (alignment present but no source offsets — a bare word array, or a
        // {"words":...} object with no source_start_ms) must NOT fall back to the whole file: that would
        // re-transcribe the chunk against the ENTIRE recording (whole-file-vs-clip training-data
        // corruption). Mirrors slice_for_export's export-side guard. Import always writes a
        // SegmentSourceMeta, so a genuine whole-file segment has alignment_json = None (still allowed).
        let pcm = vec![0i16; 16000];
        let bare_array = r#"[{"word":"x","start":0.0,"end":1.0,"confidence":0.5}]"#;
        assert!(slice_pcm_by_alignment(&pcm, 16000, Some(bare_array)).is_err(), "bare word array must refuse");
        let words_obj = r#"{"words":[]}"#;
        assert!(slice_pcm_by_alignment(&pcm, 16000, Some(words_obj)).is_err(), "offset-less object must refuse");
        // None -> whole file (a genuine single-file segment) is still allowed.
        let (whole, suffix) = slice_pcm_by_alignment(&pcm, 16000, None).unwrap();
        assert_eq!(whole.len(), pcm.len());
        assert!(suffix.is_none());
        // A valid offset still slices to its window.
        let meta = SegmentSourceMeta { source_start_ms: 100, source_end_ms: 500, chunk_index: 0, chunk_count: 2 };
        let (sliced, suffix) = slice_pcm_by_alignment(&pcm, 16000, Some(&meta.to_alignment_json())).unwrap();
        assert_eq!(sliced.len(), ms_to_samples(400, 16000));
        assert!(suffix.is_some());
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
