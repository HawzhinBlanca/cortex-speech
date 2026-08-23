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
    /// M2.1 / P1.1: review-throughput timing derived from the decision_log rows. Zero/None until the
    /// owner has made decisions in the app (the marathon's review-speed baseline lives here).
    pub review_timing: ReviewTimingStats,
    /// P3.8: on-disk database size (page_count × page_size), surfaced so months of growth are visible.
    pub db_size_bytes: u64,
}

/// Review-throughput instrumentation (M2.1). `median_seconds` is the median gap between consecutive
/// human decisions, counting only within-session gaps (deltas above `SESSION_GAP_MS` are treated as
/// breaks, not review time). This is the honest s/segment figure the C3 ≥3× gate measures against.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTimingStats {
    /// Total rows in decision_log (one per recorded human decision that carried a timestamp).
    pub decisions_logged: usize,
    /// Median within-session seconds per decision, or None when there are fewer than 2 timed decisions.
    pub median_seconds: Option<f64>,
    /// Number of consecutive-decision deltas that fell within a session and fed the median.
    pub samples: usize,
}

/// Consecutive decisions more than this far apart are between-session breaks, not review time.
const SESSION_GAP_MS: i64 = 300_000; // 5 minutes

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

/// The COMPLETE set of speakers (no truncation), sorted by segment count desc then id. Unlike
/// `compute_stats().top_speakers`, which is a dashboard summary capped at 10, this is what the
/// speaker-management panel needs so EVERY speaker can be renamed — not just the top ten.
pub fn list_speakers(db: &Database) -> AppResult<Vec<SpeakerStat>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(
        "SELECT COALESCE(speaker_id, 'unknown') AS spk, COUNT(*), COALESCE(SUM(duration_ms), 0)
         FROM speech_segments
         GROUP BY spk",
    )?;
    let mut speakers: Vec<SpeakerStat> = stmt
        .query_map([], |r| {
            Ok(SpeakerStat {
                speaker_id: r.get::<_, String>(0)?,
                segment_count: r.get::<_, i64>(1)? as usize,
                total_duration_seconds: r.get::<_, i64>(2)? as f64 / 1000.0,
            })
        })?
        .collect::<Result<_, _>>()?;
    speakers.sort_by(|a, b| b.segment_count.cmp(&a.segment_count).then_with(|| a.speaker_id.cmp(&b.speaker_id)));
    Ok(speakers)
}

/// Median within-session seconds per decision from the ordered decision_log timestamps (M2.1).
fn compute_review_timing(conn: &rusqlite::Connection) -> AppResult<ReviewTimingStats> {
    let mut stmt = conn.prepare("SELECT timestamp_ms FROM decision_log ORDER BY timestamp_ms ASC")?;
    let timestamps: Vec<i64> = stmt.query_map([], |r| r.get::<_, i64>(0))?.collect::<Result<_, _>>()?;
    let decisions_logged = timestamps.len();

    // Consecutive positive deltas within a session; median is robust to the odd long pause.
    let mut deltas: Vec<i64> =
        timestamps.windows(2).map(|w| w[1] - w[0]).filter(|&d| d > 0 && d <= SESSION_GAP_MS).collect();
    deltas.sort_unstable();

    let median_seconds = if deltas.is_empty() {
        None
    } else {
        let mid = deltas.len() / 2;
        let median_ms =
            if deltas.len() % 2 == 1 { deltas[mid] as f64 } else { (deltas[mid - 1] + deltas[mid]) as f64 / 2.0 };
        Some(median_ms / 1000.0)
    };

    Ok(ReviewTimingStats { decisions_logged, median_seconds, samples: deltas.len() })
}

/// On-disk DB size in bytes from within the connection (no path needed): page_count × page_size (P3.8).
fn db_size_bytes(conn: &rusqlite::Connection) -> u64 {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap_or(0);
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap_or(0);
    (page_count.max(0) as u64).saturating_mul(page_size.max(0) as u64)
}

pub fn compute_stats(db: &Database) -> AppResult<DatasetStats> {
    let conn = db.connection();
    let size_bytes = db_size_bytes(conn);

    // Single-pass SQL aggregate — O(1) memory and index-assisted, instead of materializing
    // the whole table into a Vec and reducing it in Rust while holding the DB lock.
    //   * LENGTH(CAST(... AS BLOB)) is the UTF-8 BYTE length, matching the previous
    //     str::len() (Sorani text is multi-byte, so character LENGTH() would differ).
    //   * The histogram uses BETWEEN ranges + an explicit over/negative bucket so a stray
    //     negative duration lands in over_30s exactly as the old `_` match arm did.
    // A human-REJECTED clip ("mark bad") carries verified=true ONLY to leave the review queue — it is
    // discarded bad data, not confirmed-good. Exclude it from `verified` (matching quality::is_human_
    // rejected, export::export_dataset, and the frontend isVerifiedGood) so the dashboard never counts a
    // rejected clip as verified; exclude it from `pending` too (it HAS been reviewed), so a rejected clip
    // is NEITHER bucket, exactly like segmentStore's segmentStats. COALESCE guards the NULL-propagation
    // trap: without it, `verified AND NOT (NULL IN ... OR NULL = ...)` collapses to NULL and a normal
    // verified clip (NULL human_decision/verdict) would drop OUT of the verified count.
    const REJECTED: &str =
        "(COALESCE(human_decision,'') IN ('reject','human_reject') OR COALESCE(verdict,'') = 'human_reject')";
    // A clip whose EFFECTIVE transcript is still an ASR placeholder is not a transcript, and
    // `export_dataset` refuses to publish it. Counting it as verified told the owner the corpus
    // contained work the export would never write — the same "a tally counts rows the export drops"
    // defect this file already guards against for rejects, on a second axis.
    //
    // `quality::effective_transcript` / `is_placeholder_transcript` are the source of truth; this is a
    // SQL mirror of them, kept because the single-pass aggregate is what makes this O(1) memory. A
    // mirror can drift, so `the_dashboards_verified_count_equals_what_the_export_actually_writes` pins
    // it against the REAL export path rather than against a second copy of the rules.
    //
    // substr(), not LIKE: SQLite's LIKE is case-INSENSITIVE for ASCII while Rust's `starts_with` is
    // not, so LIKE would exclude a '[pending…' row that the export keeps — drift in the other
    // direction. `n/a` and `null` DO compare case-insensitively, matching eq_ignore_ascii_case.
    // VERBATIM LAW (2026-08-12): human-decided verdict → annotated → champion raw. Machine text
    // (jury verdicts without a human decision, LLM-refined normalized) never surfaces as THE
    // transcript — mirrors the updated quality::training_transcript_with_source exactly.
    const EFFECTIVE: &str = "TRIM(CASE
            WHEN TRIM(COALESCE(verdict_transcript,'')) <> ''
                 AND (LOWER(COALESCE(human_decision,'')) IN ('accept','edit','human_accept','human_edit')
                      OR LOWER(COALESCE(verdict,'')) IN ('human_accept','human_edit'))
                THEN verdict_transcript
            WHEN TRIM(COALESCE(annotated_transcript,'')) <> '' THEN annotated_transcript
            ELSE COALESCE(raw_transcript,'') END)";
    let placeholder = format!(
        "(substr({EFFECTIVE}, 1, 8) = '[Pending'
          OR substr({EFFECTIVE}, 1, 16) = '[ASR unavailable'
          OR LOWER({EFFECTIVE}) IN ('n/a','null'))"
    );
    // Neither verified nor pending, exactly like a reject: it has no usable text either way.
    let excluded = format!("({REJECTED} OR {placeholder})");
    // 2026-08-06: the char sum used to run over EVERY row with `COALESCE(annotated, normalized, raw)`
    // — a third transcript-selection rule, in the same query as two counters that correctly exclude.
    // MEASURED on the live 144-clip library: 42679 chars over 144 rows versus 34685 over the 117 the
    // export keeps. 7994 characters, 18.7%, from clips marked bad. (The averages happened to land at
    // 296.4 vs 296.5 because the rejected clips are average length — coincidence, not correctness, and
    // reading it as "no impact" is the comfortable mistake.) Now uses EFFECTIVE over the same rows the
    // verified/pending counters use, so all three describe one population.
    let sql = format!(
        "SELECT
            COUNT(*),
            COALESCE(SUM(duration_ms), 0),
            COALESCE(SUM(CASE WHEN verified AND NOT {excluded} THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN NOT verified AND NOT {REJECTED} THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN NOT {excluded} THEN LENGTH(CAST({EFFECTIVE} AS BLOB)) ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN NOT {excluded} THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms BETWEEN 0 AND 4999 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms BETWEEN 5000 AND 9999 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms BETWEEN 10000 AND 14999 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms BETWEEN 15000 AND 29999 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN duration_ms < 0 OR duration_ms >= 30000 THEN 1 ELSE 0 END), 0)
         FROM speech_segments"
    );
    // Named before widening: this tuple grew from 10 to 11 columns when `counted_rows` was added, and
    // a positional tuple is exactly where an off-by-one silently reassigns every later field.
    struct Aggregate {
        total: i64,
        total_duration_ms: i64,
        verified_count: i64,
        pending_count: i64,
        total_chars: i64,
        counted_rows: i64,
        u5: i64,
        u10: i64,
        u15: i64,
        u30: i64,
        o30: i64,
    }
    let Aggregate {
        total,
        total_duration_ms,
        verified_count,
        pending_count,
        total_chars,
        counted_rows,
        u5,
        u10,
        u15,
        u30,
        o30,
    } = conn.query_row(&sql, [], |r| {
        Ok(Aggregate {
            total: r.get(0)?,
            total_duration_ms: r.get(1)?,
            verified_count: r.get(2)?,
            pending_count: r.get(3)?,
            total_chars: r.get(4)?,
            counted_rows: r.get(5)?,
            u5: r.get(6)?,
            u10: r.get(7)?,
            u15: r.get(8)?,
            u30: r.get(9)?,
            o30: r.get(10)?,
        })
    })?;

    if total == 0 {
        return Ok(DatasetStats { db_size_bytes: size_bytes, ..Default::default() });
    }

    let total = total as usize;
    let verified_count = verified_count as usize;
    let pending_count = pending_count as usize;
    let total_chars = total_chars as usize;
    let counted_rows = counted_rows as usize;
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
        // From SQL (NOT total - verified): a human-rejected clip is neither verified nor pending, so
        // total - verified_count would wrongly bucket rejected clips into pending.
        pending_count,
        verification_rate: verified_count as f64 / total as f64 * 100.0,
        unique_speakers,
        total_chars,
        // Divided by the rows the sum actually covers, NOT by `total`. Dividing an excluded-rows sum
        // by an all-rows count would trade one wrong number for a subtler one.
        avg_chars_per_segment: if counted_rows > 0 { total_chars as f64 / counted_rows as f64 } else { 0.0 },
        duration_histogram: DurationHistogram {
            under_5s: u5 as usize,
            under_10s: u10 as usize,
            under_15s: u15 as usize,
            under_30s: u30 as usize,
            over_30s: o30 as usize,
        },
        top_speakers,
        review_timing: compute_review_timing(conn)?,
        db_size_bytes: size_bytes,
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
            review_timing: ReviewTimingStats::default(),
            db_size_bytes: 0,
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
    fn list_speakers_returns_all_speakers_not_just_the_top_ten() {
        // Round-2 audit LOW: the speaker-management panel used compute_stats().top_speakers, which is
        // truncated to 10, so speakers beyond the top ten could never be renamed. list_speakers
        // returns the complete set.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        for i in 0..12 {
            let speaker = format!("SPEAKER_{i:02}");
            db.insert_legacy_segment_fixture(&seg(&format!("s{i}"), 1_000, true, Some(&speaker), "x")).unwrap();
        }
        assert_eq!(list_speakers(&db).unwrap().len(), 12, "every distinct speaker is returned");
        assert_eq!(compute_stats(&db).unwrap().top_speakers.len(), 10, "the dashboard summary stays capped at 10");
    }

    #[test]
    fn review_timing_median_uses_within_session_deltas_only() {
        // P1.1: proves decision_log is actually populated (the M2.1 write path was dead) AND that the
        // median read path counts only within-session gaps.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        for i in 0..4 {
            db.insert_legacy_segment_fixture(&seg(&format!("d{i}"), 3_000, false, Some("A"), "x")).unwrap();
        }
        // Decisions at 1s, 4s, 6s (within-session deltas 3s, 2s) then one 500s later (a break, excluded).
        db.record_human_decision("d0", "accept", None, Some(1_000)).unwrap();
        db.record_human_decision("d1", "accept", None, Some(4_000)).unwrap();
        db.record_human_decision("d2", "accept", None, Some(6_000)).unwrap();
        db.record_human_decision("d3", "accept", None, Some(500_000)).unwrap();

        let rt = compute_stats(&db).unwrap().review_timing;
        assert_eq!(rt.decisions_logged, 4, "every timestamped decision is logged");
        assert_eq!(rt.samples, 2, "the 494s break delta is excluded as a between-session gap");
        assert_eq!(rt.median_seconds, Some(2.5), "median of within-session deltas 3s and 2s is 2.5s");
    }

    #[test]
    fn review_timing_is_empty_without_timed_decisions() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_legacy_segment_fixture(&seg("s1", 3_000, false, None, "x")).unwrap();
        let rt = compute_stats(&db).unwrap().review_timing;
        assert_eq!(rt.decisions_logged, 0);
        assert_eq!(rt.median_seconds, None);
        assert_eq!(rt.samples, 0);
    }

    #[test]
    fn compute_stats_reports_nonzero_db_size() {
        // P3.8: an initialized DB (with its schema pages) has a nonzero page-based size, surfaced so
        // months of growth are visible.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let st = compute_stats(&db).unwrap();
        assert!(st.db_size_bytes > 0, "db size should be nonzero: {}", st.db_size_bytes);
    }

    #[test]
    fn compute_stats_matches_hand_computed_values() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        for s in [
            seg("s1", 3_000, true, Some("A"), "ab"),  // u5 bucket, 2 bytes
            seg("s2", 8_000, true, Some("A"), "سڵاو"), // u10 bucket, 8 UTF-8 bytes
            seg("s3", 35_000, false, Some("B"), "x"), // over_30s bucket, 1 byte
        ] {
            db.insert_legacy_segment_fixture(&s).unwrap();
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
    fn a_human_rejected_clip_counts_as_neither_verified_nor_pending() {
        // markBad sets verified=true only to leave the review queue — a rejected clip is discarded bad
        // data, NOT confirmed-good. It must count as NEITHER verified nor pending (matching
        // quality::is_human_rejected / export_dataset / the frontend isVerifiedGood), so the dashboard
        // never overstates the verified count by folding rejected clips into it.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_legacy_segment_fixture(&seg("good", 1000, true, Some("A"), "ڕاست")).unwrap(); // verified-good
        db.insert_legacy_segment_fixture(&seg("pend", 1000, false, Some("A"), "چاوەڕوان")).unwrap(); // pending
        db.insert_legacy_segment_fixture(&seg("bad", 1000, true, Some("A"), "خراپ")).unwrap(); // verified=true but rejected
        db.connection().execute("UPDATE speech_segments SET human_decision='reject' WHERE id='bad'", []).unwrap();

        let st = compute_stats(&db).unwrap();
        assert_eq!(st.total_segments, 3);
        assert_eq!(st.verified_count, 1, "a rejected clip (verified=true) must NOT count as verified");
        assert_eq!(st.pending_count, 1, "a rejected clip must NOT count as pending either");
        assert_eq!(st.verified_count + st.pending_count, 2, "rejected is excluded from BOTH buckets");
        assert!((st.verification_rate - 100.0 / 3.0).abs() < 1e-9, "1 of 3 segments verified-good");
    }

    #[test]
    fn char_totals_describe_the_same_population_as_the_verified_and_pending_counters() {
        // Same class as the counter above, on a third axis, and it was live: MEASURED 2026-08-06 the
        // dashboard reported 42679 chars over the owner's 144 clips where the export-consistent figure
        // is 34685 over 117 — 7994 characters of text from clips marked bad, an 18.7% overstatement of
        // how much transcript the corpus actually holds.
        //
        // The average is asserted too, and deliberately: on the live corpus the rejected clips were
        // average length, so avg_chars barely moved (296.4 -> 296.5) while the total was badly wrong.
        // A test that only checked the average would have called this clean.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_legacy_segment_fixture(&seg("keep1", 1000, true, Some("A"), "abcd")).unwrap(); // 4 bytes
        db.insert_legacy_segment_fixture(&seg("keep2", 1000, false, Some("A"), "ef")).unwrap(); // 2 bytes, pending
        db.insert_legacy_segment_fixture(&seg("bad", 1000, true, Some("A"), "ZZZZZZZZZZ")).unwrap(); // 10 bytes, rejected
        db.connection().execute("UPDATE speech_segments SET human_decision='reject' WHERE id='bad'", []).unwrap();

        let st = compute_stats(&db).unwrap();
        assert_eq!(st.total_segments, 3, "total_segments is corpus-wide and stays so");
        assert_eq!(st.total_chars, 6, "a rejected clip's 10 characters must not be counted as corpus text");
        assert!(
            (st.avg_chars_per_segment - 3.0).abs() < 1e-9,
            "average must divide by the 2 rows the sum covers, not by all 3: got {}",
            st.avg_chars_per_segment
        );
    }

    #[test]
    fn char_totals_use_the_effective_transcript_the_rest_of_this_file_uses() {
        // The old sum read COALESCE(annotated, normalized, raw) — a THIRD selection rule, differing
        // from EFFECTIVE (which prefers verdict_transcript on a human decision) and from the
        // frontend's own order. A human edit stored in verdict_transcript was therefore measured as
        // whatever stale text sat in annotated/raw.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_legacy_segment_fixture(&seg("edited", 1000, true, Some("A"), "old")).unwrap(); // raw = 3 bytes
        db.connection()
            .execute(
                "UPDATE speech_segments SET verdict_transcript='corrected', human_decision='edit' WHERE id='edited'",
                [],
            )
            .unwrap();

        let st = compute_stats(&db).unwrap();
        assert_eq!(st.total_chars, 9, "must measure the human's 'corrected' (9), not the stale raw 'old' (3)");
    }

    #[test]
    fn the_dashboards_verified_count_equals_what_the_export_actually_writes() {
        // THE INVARIANT THAT KEEPS BREAKING. Per the project ledger this exact class — a tally that
        // counts rows the export drops — has been found and fixed six times. It keeps coming back
        // because the two sides are maintained independently and each has its own tests: stats.rs
        // proves it excludes rejects, export.rs proves it excludes rejects, and NOTHING ever compares
        // the two numbers. So the day one side gains an exclusion rule the other does not, the
        // dashboard silently overstates the corpus and no test notices.
        //
        // That is not hypothetical here: export_dataset drops rejected AND placeholder AND held-out
        // clips; compute_stats drops only rejected. The live library happens to contain no
        // placeholders, which is the only reason the two agree today.
        //
        // Asserted against the REAL export path rather than a re-implementation of its rules — a
        // second copy of the filter would drift in exactly the same way and prove nothing.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_legacy_segment_fixture(&seg("good", 1000, true, Some("A"), "دەقی ڕاست")).unwrap();
        db.insert_legacy_segment_fixture(&seg("pend", 1000, false, Some("A"), "چاوەڕوان")).unwrap();
        db.insert_legacy_segment_fixture(&seg("bad", 1000, true, Some("A"), "خراپ")).unwrap();
        db.connection().execute("UPDATE speech_segments SET human_decision='reject' WHERE id='bad'", []).unwrap();
        // A clip whose EFFECTIVE transcript is still the ASR placeholder, marked verified. Export
        // refuses to publish it (it is not a transcript); the dashboard must not call it verified
        // either, or it reports progress the dataset does not contain.
        db.insert_legacy_segment_fixture(&seg("hold", 1000, true, Some("A"), "[Pending WSL 7B ASR]")).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("dataset.jsonl");
        crate::export::export_dataset(&db, &out, &crate::settings::ExportFormat::Jsonl).unwrap();
        let exported_verified = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.get("verified").and_then(serde_json::Value::as_bool).unwrap_or(false))
            .count();

        let st = compute_stats(&db).unwrap();
        assert_eq!(
            st.verified_count, exported_verified,
            "the dashboard says {} clips are verified but the export writes {} — one of them is lying \
             to the owner about how much of the corpus is real",
            st.verified_count, exported_verified
        );
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
