use crate::db::Database;
use crate::error::AppResult;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetStats {
    pub total_segments: usize,
    pub total_duration_seconds: f64,
    pub avg_duration_seconds: f64,
    pub verified_count: usize,
    pub pending_count: usize,
    pub verification_rate: f64,
    pub unique_speakers: usize,
    pub total_chars: usize,
    pub avg_chars_per_segment: f64,
    pub duration_histogram: DurationHistogram,
    pub top_speakers: Vec<SpeakerStat>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurationHistogram {
    pub under_5s: usize,
    pub under_10s: usize,
    pub under_15s: usize,
    pub under_30s: usize,
    pub over_30s: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerStat {
    pub speaker_id: String,
    pub segment_count: usize,
    pub total_duration_seconds: f64,
}

pub fn compute_stats(db: &Database) -> AppResult<DatasetStats> {
    let segments = db.get_segments(None)?;
    let total = segments.len();

    if total == 0 {
        return Ok(DatasetStats::default());
    }

    let total_duration_ms: i64 = segments.iter().map(|s| s.duration_ms).sum();
    let total_duration_seconds = total_duration_ms as f64 / 1000.0;
    let verified_count = segments.iter().filter(|s| s.verified).count();
    let total_chars: usize = segments
        .iter()
        .map(|s| {
            s.annotated_transcript.as_deref().or(s.normalized_transcript.as_deref()).unwrap_or(&s.raw_transcript).len()
        })
        .sum();

    // Speaker stats
    let mut speaker_map: std::collections::HashMap<String, (usize, i64)> = std::collections::HashMap::new();
    for seg in &segments {
        let speaker = seg.speaker_id.as_deref().unwrap_or("unknown").to_string();
        let entry = speaker_map.entry(speaker).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += seg.duration_ms;
    }
    let unique_speakers = speaker_map.len();
    let mut top_speakers: Vec<SpeakerStat> = speaker_map
        .into_iter()
        .map(|(k, (count, dur))| SpeakerStat {
            speaker_id: k,
            segment_count: count,
            total_duration_seconds: dur as f64 / 1000.0,
        })
        .collect();
    top_speakers.sort_by_key(|a| std::cmp::Reverse(a.segment_count));
    top_speakers.truncate(10);

    // Duration histogram
    let mut hist = DurationHistogram { under_5s: 0, under_10s: 0, under_15s: 0, under_30s: 0, over_30s: 0 };
    for seg in &segments {
        match seg.duration_ms {
            0..=4999 => hist.under_5s += 1,
            5000..=9999 => hist.under_10s += 1,
            10000..=14999 => hist.under_15s += 1,
            15000..=29999 => hist.under_30s += 1,
            _ => hist.over_30s += 1,
        }
    }

    Ok(DatasetStats {
        total_segments: total,
        total_duration_seconds,
        avg_duration_seconds: total_duration_seconds / total as f64,
        verified_count,
        pending_count: total - verified_count,
        verification_rate: if total > 0 { verified_count as f64 / total as f64 * 100.0 } else { 0.0 },
        unique_speakers,
        total_chars,
        avg_chars_per_segment: if total > 0 { total_chars as f64 / total as f64 } else { 0.0 },
        duration_histogram: hist,
        top_speakers,
    })
}

impl Default for DatasetStats {
    fn default() -> Self {
        Self {
            total_segments: 0,
            total_duration_seconds: 0.0,
            avg_duration_seconds: 0.0,
            verified_count: 0,
            pending_count: 0,
            verification_rate: 0.0,
            unique_speakers: 0,
            total_chars: 0,
            avg_chars_per_segment: 0.0,
            duration_histogram: DurationHistogram {
                under_5s: 0,
                under_10s: 0,
                under_15s: 0,
                under_30s: 0,
                over_30s: 0,
            },
            top_speakers: Vec::new(),
        }
    }
}
