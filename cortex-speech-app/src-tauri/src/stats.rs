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
    let conn = db.connection();

    // Single-pass SQL aggregate — O(1) memory and index-assisted, instead of materializing
    // the whole table into a Vec and reducing it in Rust while holding the DB lock.
    //   * LENGTH(CAST(... AS BLOB)) is the UTF-8 BYTE length, matching the previous
    //     str::len() (Sorani text is multi-byte, so character LENGTH() would differ).
    //   * The histogram uses BETWEEN ranges + an explicit over/negative bucket so a stray
    //     negative duration lands in over_30s exactly as the old `_` match arm did.
    let (total, total_duration_ms, verified_count, total_chars, u5, u10, u15, u30, o30) = conn.query_row(
        "SELECT
            COUNT(*),
            COALESCE(SUM(duration_ms), 0),
            COALESCE(SUM(CASE WHEN verified THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(LENGTH(CAST(COALESCE(annotated_transcript, normalized_transcript, raw_transcript) AS BLOB))), 0),
            COALESCE(SUM(CASE WHEN duration_ms BETWEEN 0 AND 4999 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms BETWEEN 5000 AND 9999 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms BETWEEN 10000 AND 14999 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms BETWEEN 15000 AND 29999 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms < 0 OR duration_ms >= 30000 THEN 1 ELSE 0 END), 0)
         FROM speech_segments",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
            ))
        },
    )?;

    if total == 0 {
        return Ok(DatasetStats::default());
    }

    let total = total as usize;
    let verified_count = verified_count as usize;
    let total_chars = total_chars as usize;
    let total_duration_seconds = total_duration_ms as f64 / 1000.0;

    // Speaker tallies via GROUP BY (NULL speaker → "unknown", matching the old fallback).
    let mut stmt = conn.prepare(
        "SELECT COALESCE(speaker_id, 'unknown') AS spk, COUNT(*), COALESCE(SUM(duration_ms), 0)
         FROM speech_segments
         GROUP BY spk",
    )?;
    let mut top_speakers: Vec<SpeakerStat> = stmt
        .query_map([], |r| {
            Ok(SpeakerStat {
                speaker_id: r.get::<_, String>(0)?,
                segment_count: r.get::<_, i64>(1)? as usize,
                total_duration_seconds: r.get::<_, i64>(2)? as f64 / 1000.0,
            })
        })?
        .collect::<Result<_, _>>()?;
    let unique_speakers = top_speakers.len();
    // Deterministic ordering (the old HashMap iteration order broke ties non-deterministically).
    top_speakers.sort_by(|a, b| b.segment_count.cmp(&a.segment_count).then_with(|| a.speaker_id.cmp(&b.speaker_id)));
    top_speakers.truncate(10);

    Ok(DatasetStats {
        total_segments: total,
        total_duration_seconds,
        avg_duration_seconds: total_duration_seconds / total as f64,
        verified_count,
        pending_count: total - verified_count,
        verification_rate: verified_count as f64 / total as f64 * 100.0,
        unique_speakers,
        total_chars,
        avg_chars_per_segment: total_chars as f64 / total as f64,
        duration_histogram: DurationHistogram {
            under_5s: u5 as usize,
            under_10s: u10 as usize,
            under_15s: u15 as usize,
            under_30s: u30 as usize,
            over_30s: o30 as usize,
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, SpeechSegment};

    fn seg(id: &str, dur: i64, verified: bool, speaker: Option<&str>, raw: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            audio_path: format!("/{id}.wav"),
            raw_transcript: raw.to_string(),
            duration_ms: dur,
            verified,
            speaker_id: speaker.map(str::to_string),
            ..SpeechSegment::default()
        }
    }

    #[test]
    fn compute_stats_matches_hand_computed_values() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        for s in [
            seg("s1", 3_000, true, Some("A"), "ab"),       // u5 bucket, 2 bytes
            seg("s2", 8_000, true, Some("A"), "سڵاو"),     // u10 bucket, 8 UTF-8 bytes
            seg("s3", 35_000, false, Some("B"), "x"),      // over_30s bucket, 1 byte
        ] {
            db.insert_segment(&s).unwrap();
        }
        let st = compute_stats(&db).unwrap();

        assert_eq!(st.total_segments, 3);
        assert_eq!(st.verified_count, 2);
        assert_eq!(st.pending_count, 1);
        assert!((st.total_duration_seconds - 46.0).abs() < 1e-9);
        assert!((st.avg_duration_seconds - 46.0 / 3.0).abs() < 1e-9);
        assert!((st.verification_rate - 200.0 / 3.0).abs() < 1e-9);
        assert_eq!(st.duration_histogram.under_5s, 1);
        assert_eq!(st.duration_histogram.under_10s, 1);
        assert_eq!(st.duration_histogram.under_15s, 0);
        assert_eq!(st.duration_histogram.over_30s, 1);
        assert_eq!(st.unique_speakers, 2);
        // BYTE length, not char count: 2 + 8 + 1 = 11.
        assert_eq!(st.total_chars, 11);
        assert_eq!(st.top_speakers[0].speaker_id, "A");
        assert_eq!(st.top_speakers[0].segment_count, 2);
    }

    #[test]
    fn compute_stats_empty_is_zeroed() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let st = compute_stats(&db).unwrap();
        assert_eq!(st.total_segments, 0);
        assert_eq!(st.unique_speakers, 0);
        assert!(st.avg_duration_seconds.is_finite());
    }
}
