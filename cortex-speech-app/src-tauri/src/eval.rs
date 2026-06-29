use crate::db::Database;
use crate::error::AppResult;
use crate::wer::{char_edit_distance, word_edit_distance};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ────────────────────────────────────────────────────────────────────────────
// Data types
// ────────────────────────────────────────────────────────────────────────────

/// A single verified clip in the permanent gold-set holdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoldSegment {
    pub id: String,
    pub audio_path: String,
    pub reference: String,
    /// When true this segment is never used for DPO fine-tuning updates.
    pub is_holdout: bool,
    pub created_at: Option<String>,
}

/// Input payload for bulk-importing gold clips.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoldSegmentInput {
    pub audio_path: String,
    pub reference: String,
    /// Default true — mark as holdout so the learning loop never trains on it.
    #[serde(default = "default_true")]
    pub is_holdout: bool,
}

fn default_true() -> bool {
    true
}

/// A per-model WER/CER snapshot against the gold set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalRun {
    pub id: String,
    pub model_id: String,
    pub run_at: String,
    pub num_segs: i64,
    pub wer: f64,
    pub cer: f64,
    pub meta_json: Option<String>,
}

/// Per-segment detail returned from an eval run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalSegmentResult {
    pub gold_id: String,
    pub audio_path: String,
    pub reference: String,
    pub hypothesis: String,
    pub wer: f64,
    pub cer: f64,
}

/// Full result object returned by `run_gold_eval`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalRunResult {
    pub run: EvalRun,
    pub segments: Vec<EvalSegmentResult>,
}

// ────────────────────────────────────────────────────────────────────────────
// Database helpers
// ────────────────────────────────────────────────────────────────────────────

/// Bulk-insert gold segments, IDEMPOTENT on the audio file identity.
///
/// Re-marking the same clip as gold must not create a second holdout row: the row id is a fresh UUID,
/// so the previous `INSERT OR IGNORE` never actually dedup'd, and `run_gold_eval` would then transcribe
/// the clip once but score it once PER duplicate row — double-counting it in the published WER/CER
/// aggregates. We therefore replace any existing gold row(s) for the same `audio_path` inside one
/// transaction, so a partial failure never drops the old row without writing the new one.
pub fn import_gold_segments(db: &Database, inputs: Vec<GoldSegmentInput>) -> AppResult<usize> {
    let conn = db.connection();
    let tx = conn.unchecked_transaction()?;
    let mut count = 0usize;
    {
        let mut delete_stmt = tx.prepare("DELETE FROM gold_segments WHERE audio_path = ?1")?;
        let mut insert_stmt = tx.prepare(
            "INSERT INTO gold_segments (id, audio_path, reference, is_holdout, audio_content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for inp in &inputs {
            delete_stmt.execute(params![inp.audio_path])?;
            let id = Uuid::new_v4().to_string();
            // Persist the audio content hash NOW — the file is present when the user marks it gold — so
            // holdout exclusion no longer depends on the file still existing at export time (fail-closed).
            let content_hash = crate::pipeline::source_audio_identity(std::path::Path::new(&inp.audio_path))
                .ok()
                .map(|identity| identity.content_hash);
            insert_stmt.execute(params![id, inp.audio_path, inp.reference, inp.is_holdout as i32, content_hash])?;
            count += 1;
        }
    }
    tx.commit()?;
    Ok(count)
}

/// Create a gold benchmark entry from the human-corrected segments of one source audio file. Gathers
/// the REVIEWED segments of `audio_path` (those the curator gave a decision on), in time order,
/// concatenates their corrected transcripts into the full reference, and imports it as a single
/// holdout gold clip (is_holdout = true, so the learning loop never trains on it). Returns the number
/// of gold rows created. Errors if the file has no reviewed segments — correct it in the app first.
pub fn create_gold_from_verified_file(db: &Database, audio_path: &str) -> AppResult<usize> {
    let mut stmt = db.connection().prepare(
        "SELECT COALESCE(NULLIF(verdict_transcript, ''), NULLIF(normalized_transcript, ''), raw_transcript)
         FROM speech_segments
         WHERE audio_path = ?1 AND human_decision IS NOT NULL AND human_decision != ''
         -- `, rowid ASC` tiebreaker: all of one file's chunks batch-insert in the same created_at
         -- second (a tie), in chunk/chronological order — so rowid ASC keeps the concatenated gold
         -- reference in true segment order rather than SQLite's undefined tie order.
         ORDER BY created_at ASC, rowid ASC",
    )?;
    let rows = stmt.query_map(params![audio_path], |row| row.get::<_, Option<String>>(0))?;

    let mut parts = Vec::new();
    for row in rows {
        if let Some(text) = row? {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }
    if parts.is_empty() {
        return Err(crate::error::AppError::Validation(format!(
            "no human-reviewed segments found for '{audio_path}' — correct it in the app first, then mark it as gold"
        )));
    }

    let reference = parts.join(" ");
    import_gold_segments(db, vec![GoldSegmentInput { audio_path: audio_path.to_string(), reference, is_holdout: true }])
}

/// Load all gold segments from the DB.
pub fn list_gold_segments(db: &Database) -> AppResult<Vec<GoldSegment>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(
        "SELECT id, audio_path, reference, is_holdout, created_at
         FROM gold_segments ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(GoldSegment {
            id: row.get(0)?,
            audio_path: row.get(1)?,
            reference: row.get(2)?,
            is_holdout: row.get::<_, i32>(3)? != 0,
            created_at: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Load all eval-run records.
pub fn list_eval_runs(db: &Database) -> AppResult<Vec<EvalRun>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(
        "SELECT id, model_id, run_at, num_segs, wer, cer, meta_json
         FROM eval_runs ORDER BY run_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(EvalRun {
            id: row.get(0)?,
            model_id: row.get(1)?,
            run_at: row.get(2)?,
            num_segs: row.get(3)?,
            wer: row.get(4)?,
            cer: row.get(5)?,
            meta_json: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn insert_eval_run(conn: &rusqlite::Connection, run: &EvalRun) -> AppResult<()> {
    conn.execute(
        "INSERT INTO eval_runs (id, model_id, run_at, num_segs, wer, cer, meta_json)
         VALUES (?1, ?2, datetime('now'), ?3, ?4, ?5, ?6)",
        params![run.id, run.model_id, run.num_segs, run.wer, run.cer, run.meta_json],
    )?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// Core evaluation logic
// ────────────────────────────────────────────────────────────────────────────

/// Compute WER/CER for a model against a subset of gold segments.
///
/// `hypotheses` is a slice of `(gold_segment_id, hypothesis_text)` pairs.
/// This is intentionally model-agnostic: the caller supplies the hypotheses
/// (from the pipeline, ASR, or the existing segment_hypotheses table).
pub fn run_gold_eval(
    db: &Database,
    model_id: &str,
    hypotheses: Vec<(String, String)>, // (gold_id, hypothesis)
) -> AppResult<EvalRunResult> {
    // Load all gold segments into a map for O(1) reference lookup
    let gold_map: std::collections::HashMap<String, GoldSegment> =
        list_gold_segments(db)?.into_iter().map(|g| (g.id.clone(), g)).collect();

    let mut seg_details = Vec::new();
    let mut total_wer = 0.0f64;
    let mut total_cer = 0.0f64;
    let mut total_word_distance = 0usize;
    let mut total_word_ref_len = 0usize;
    let mut total_char_distance = 0usize;
    let mut total_char_ref_len = 0usize;
    let mut n = 0usize;

    for (gold_id, hypothesis) in &hypotheses {
        let gold = match gold_map.get(gold_id) {
            Some(g) => g,
            None => {
                tracing::warn!("Gold segment {} not found; skipping", gold_id);
                continue;
            }
        };

        let w_dist = word_edit_distance(&gold.reference, hypothesis);
        let c_dist = char_edit_distance(&gold.reference, hypothesis);

        let w = if w_dist.ref_len == 0 {
            if w_dist.distance > 0 {
                1.0
            } else {
                0.0
            }
        } else {
            (w_dist.distance as f64 / w_dist.ref_len as f64).min(1.0)
        };
        let c = if c_dist.ref_len == 0 {
            if c_dist.distance > 0 {
                1.0
            } else {
                0.0
            }
        } else {
            (c_dist.distance as f64 / c_dist.ref_len as f64).min(1.0)
        };

        seg_details.push((
            EvalSegmentResult {
                gold_id: gold_id.clone(),
                audio_path: gold.audio_path.clone(),
                reference: gold.reference.clone(),
                hypothesis: hypothesis.clone(),
                wer: w,
                cer: c,
            },
            w_dist,
            c_dist,
        ));

        total_wer += w;
        total_cer += c;
        // Micro (corpus) rate is errors-per-REFERENCE-token, so a zero-reference segment (a gold ref
        // that normalizes to empty) must contribute to NEITHER accumulator. The old code added its
        // insertions to the numerator but 0 to the denominator, so a single empty-ref hallucination
        // pegged the whole corpus micro-WER/CER to its 1.0 clamp even when every other clip was perfect.
        // The hallucination is still reflected in the macro average (its per-segment rate is 1.0 above).
        if w_dist.ref_len > 0 {
            total_word_distance += w_dist.distance;
            total_word_ref_len += w_dist.ref_len;
        }
        if c_dist.ref_len > 0 {
            total_char_distance += c_dist.distance;
            total_char_ref_len += c_dist.ref_len;
        }
        n += 1;
    }

    let macro_wer = if n > 0 { total_wer / n as f64 } else { 0.0 };
    let macro_cer = if n > 0 { total_cer / n as f64 } else { 0.0 };

    let micro_wer = if total_word_ref_len > 0 {
        (total_word_distance as f64 / total_word_ref_len as f64).min(1.0)
    } else {
        if total_word_distance > 0 {
            1.0
        } else {
            0.0
        }
    };
    let micro_cer = if total_char_ref_len > 0 {
        (total_char_distance as f64 / total_char_ref_len as f64).min(1.0)
    } else {
        if total_char_distance > 0 {
            1.0
        } else {
            0.0
        }
    };

    let meta = serde_json::json!({
        "micro_wer": micro_wer,
        "micro_cer": micro_cer,
        "macro_wer": macro_wer,
        "macro_cer": macro_cer,
    });
    let meta_str = serde_json::to_string(&meta).ok();

    let run = EvalRun {
        id: Uuid::new_v4().to_string(),
        model_id: model_id.to_string(),
        run_at: String::new(), // filled by DB DEFAULT
        num_segs: n as i64,
        wer: micro_wer, // corpus-level (micro) WER reported as headline
        cer: micro_cer, // corpus-level (micro) CER reported as headline
        meta_json: meta_str,
    };

    // Persist the parent eval_runs row and all child eval_segment_results rows ATOMICALLY. The
    // headline micro WER/CER on the run is computed over ALL N segments, so a partial write (e.g. a
    // child insert hitting SQLITE_BUSY past the busy_timeout) must not leave a run whose stored
    // metrics disagree with its surviving per-segment rows. The Transaction rolls back on any early
    // `?` (Drop), and commits only after every row succeeds.
    let conn = db.connection();
    let tx = conn.unchecked_transaction()?;
    insert_eval_run(&tx, &run)?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO eval_segment_results (id, eval_run_id, gold_id, audio_path, reference, hypothesis, wer, cer, word_distance, word_ref_len, char_distance, char_ref_len)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
        )?;
        for (seg_res, w_dist, c_dist) in &seg_details {
            stmt.execute(params![
                Uuid::new_v4().to_string(),
                run.id,
                seg_res.gold_id,
                seg_res.audio_path,
                seg_res.reference,
                seg_res.hypothesis,
                seg_res.wer,
                seg_res.cer,
                w_dist.distance as i64,
                w_dist.ref_len as i64,
                c_dist.distance as i64,
                c_dist.ref_len as i64,
            ])?;
        }
    }
    tx.commit()?;

    // Re-query to get the DB-generated run_at timestamp
    let stored = list_eval_runs(db)?.into_iter().find(|r| r.id == run.id).unwrap_or(run);

    let segments = seg_details.into_iter().map(|(r, _, _)| r).collect();
    Ok(EvalRunResult { run: stored, segments })
}

pub fn load_eval_run_and_recompute(
    db: &Database,
    run_id: &str,
) -> AppResult<Option<(EvalRun, Vec<EvalSegmentResult>)>> {
    let conn = db.connection();

    // 1. Load the EvalRun
    let mut run_stmt = conn.prepare(
        "SELECT id, model_id, run_at, num_segs, wer, cer, meta_json
         FROM eval_runs WHERE id = ?1",
    )?;
    let mut run_rows = run_stmt.query_map(params![run_id], |row| {
        Ok(EvalRun {
            id: row.get(0)?,
            model_id: row.get(1)?,
            run_at: row.get(2)?,
            num_segs: row.get(3)?,
            wer: row.get(4)?,
            cer: row.get(5)?,
            meta_json: row.get(6)?,
        })
    })?;

    let run = match run_rows.next() {
        Some(r) => r?,
        None => return Ok(None),
    };

    // 2. Load the segment results
    let mut seg_stmt = conn.prepare(
        "SELECT gold_id, audio_path, reference, hypothesis, wer, cer, word_distance, word_ref_len, char_distance, char_ref_len
         FROM eval_segment_results WHERE eval_run_id = ?1",
    )?;

    let mut total_word_distance = 0usize;
    let mut total_word_ref_len = 0usize;
    let mut total_char_distance = 0usize;
    let mut total_char_ref_len = 0usize;
    let mut seg_results = Vec::new();

    let rows = seg_stmt.query_map(params![run_id], |row| {
        let gold_id: String = row.get(0)?;
        let audio_path: String = row.get(1)?;
        let reference: String = row.get(2)?;
        let hypothesis: String = row.get(3)?;
        let wer: f64 = row.get(4)?;
        let cer: f64 = row.get(5)?;
        let w_dist: i64 = row.get(6)?;
        let w_ref: i64 = row.get(7)?;
        let c_dist: i64 = row.get(8)?;
        let c_ref: i64 = row.get(9)?;

        Ok((EvalSegmentResult { gold_id, audio_path, reference, hypothesis, wer, cer }, w_dist, w_ref, c_dist, c_ref))
    })?;

    for row in rows {
        let (seg, w_dist, w_ref, c_dist, c_ref) = row?;
        // Skip empty-reference rows (ref_len == 0) from the micro accumulators, EXACTLY as the write
        // path (run_gold_eval, lines ~260-267) does. Otherwise the recompute counts an empty-ref row's
        // insertions over a zero denominator and pegs the reloaded micro WER/CER to its 1.0 clamp, while
        // the stored run reads ~0% for the identical rows — breaking the documented "reload by id ==
        // stored micro" invariant. The all-empty-corpus case still yields 0.0 below (distance is 0 too).
        if w_ref > 0 {
            total_word_distance += w_dist as usize;
            total_word_ref_len += w_ref as usize;
        }
        if c_ref > 0 {
            total_char_distance += c_dist as usize;
            total_char_ref_len += c_ref as usize;
        }
        seg_results.push(seg);
    }

    // 3. Recompute micro averages
    let micro_wer = if total_word_ref_len > 0 {
        (total_word_distance as f64 / total_word_ref_len as f64).min(1.0)
    } else {
        if total_word_distance > 0 {
            1.0
        } else {
            0.0
        }
    };

    let micro_cer = if total_char_ref_len > 0 {
        (total_char_distance as f64 / total_char_ref_len as f64).min(1.0)
    } else {
        if total_char_distance > 0 {
            1.0
        } else {
            0.0
        }
    };

    let mut recomputed_run = run;
    recomputed_run.wer = micro_wer;
    recomputed_run.cer = micro_cer;

    Ok(Some((recomputed_run, seg_results)))
}

/// Run the gold-set eval end-to-end by producing each hypothesis through `transcribe`.
///
/// Closed-loop counterpart to [`run_gold_eval`]: instead of trusting caller-supplied
/// hypotheses, the closure produces a hypothesis from each gold segment — in production
/// this runs the real ASR engine on the segment audio (see
/// `ProcessingPipeline::run_gold_eval_asr`). The loop is generic over the transcriber so
/// it is fully unit-testable without loading any model. Segments whose transcription
/// fails are logged and skipped — never silently scored as an empty hypothesis, which
/// would understate WER/CER.
pub fn run_gold_eval_with_transcriber<F>(db: &Database, model_id: &str, mut transcribe: F) -> AppResult<EvalRunResult>
where
    F: FnMut(&GoldSegment) -> AppResult<String>,
{
    let gold = list_gold_segments(db)?;
    let total = gold.len();
    let mut hypotheses: Vec<(String, String)> = Vec::with_capacity(total);
    let mut failed = 0usize;
    for seg in &gold {
        match transcribe(seg) {
            Ok(hyp) => hypotheses.push((seg.id.clone(), hyp)),
            Err(e) => {
                failed += 1;
                tracing::warn!("gold eval: transcription failed for {} ({}): {e}", seg.id, seg.audio_path);
            }
        }
    }
    if failed > 0 {
        tracing::warn!("gold eval: {failed}/{total} segments failed to transcribe and were skipped");
    }
    run_gold_eval(db, model_id, hypotheses)
}

// ────────────────────────────────────────────────────────────────────────────
// Label-quality lift (M3.1)
// ────────────────────────────────────────────────────────────────────────────

/// Measured raw-ASR vs post-jury label-quality lift (blueprint M3.1). Over segments that carry a
/// ground-truth reference, a raw ASR hypothesis, and a post-jury verdict, this reports the micro
/// CER of each and the CER reduction (`cer_lift = raw - jury`; positive means the jury improved
/// labels), with a seeded paired bootstrap 95% CI on the per-replicate micro-CER lift.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelQualityLift {
    pub n: usize,
    pub raw_micro_cer: f64,
    pub jury_micro_cer: f64,
    pub cer_lift: f64,
    pub lift_ci_low: f64,
    pub lift_ci_high: f64,
}

/// Compute the label-quality lift over `(reference, raw_hyp, jury_hyp)` triples. CER flows through
/// the same normalized char-edit-distance path as the scorecard. The paired bootstrap resamples
/// whole segments (seeded xorshift64, deterministic) so the CI reflects sampling variability and
/// the raw/jury micro-CERs are resampled together (paired).
pub fn compute_label_quality_lift(
    triples: &[(String, String, String)],
    bootstrap_samples: usize,
    seed: u64,
) -> LabelQualityLift {
    let n = triples.len();
    // (ref_char_len, raw_char_dist, jury_char_dist) per segment — ref_len is shared (same reference).
    let per: Vec<(usize, usize, usize)> = triples
        .iter()
        .map(|(reference, raw, jury)| {
            let dr = char_edit_distance(reference, raw);
            let dj = char_edit_distance(reference, jury);
            (dr.ref_len, dr.distance, dj.distance)
        })
        .collect();

    let micro = |indices: &[usize]| -> (f64, f64) {
        let mut ref_chars = 0usize;
        let mut raw_d = 0usize;
        let mut jury_d = 0usize;
        for &i in indices {
            let (rl, rd, jd) = per[i];
            ref_chars += rl;
            raw_d += rd;
            jury_d += jd;
        }
        if ref_chars == 0 {
            (0.0, 0.0)
        } else {
            (raw_d as f64 / ref_chars as f64, jury_d as f64 / ref_chars as f64)
        }
    };

    let all: Vec<usize> = (0..n).collect();
    let (raw_micro_cer, jury_micro_cer) = micro(&all);
    let cer_lift = raw_micro_cer - jury_micro_cer;

    let mut lifts: Vec<f64> = Vec::with_capacity(bootstrap_samples);
    if n > 0 && bootstrap_samples > 0 {
        let mut state = seed | 1; // xorshift64 state must be non-zero
        for _ in 0..bootstrap_samples {
            let sample: Vec<usize> = (0..n)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state % n as u64) as usize
                })
                .collect();
            let (rc, jc) = micro(&sample);
            lifts.push(rc - jc);
        }
        lifts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    }
    let percentile = |p: f64| -> f64 {
        if lifts.is_empty() {
            return cer_lift;
        }
        let idx = ((p * (lifts.len() as f64 - 1.0)).round() as usize).min(lifts.len() - 1);
        lifts[idx]
    };

    LabelQualityLift {
        n,
        raw_micro_cer,
        jury_micro_cer,
        cer_lift,
        lift_ci_low: percentile(0.025),
        lift_ci_high: percentile(0.975),
    }
}

/// Load `(reference, raw, jury)` triples for the label-quality lift from human-verified segments:
/// the human's correction (`annotated_transcript`) is the ground-truth reference, `raw_transcript`
/// is the raw ASR hypothesis, and `verdict_transcript` is the post-jury label. Only segments that
/// carry all three (non-empty) are included — a real measured lift needs ground truth + both hyps.
pub fn load_lift_triples(db: &Database) -> AppResult<Vec<(String, String, String)>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(
        // Require human_decision so the reference is HUMAN-confirmed ground truth, not just an
        // LLM-refined annotated_transcript (commands.rs notes annotated can be human OR LLM).
        "SELECT annotated_transcript, raw_transcript, verdict_transcript \
         FROM speech_segments \
         WHERE human_decision IS NOT NULL AND TRIM(human_decision) <> '' \
           AND annotated_transcript IS NOT NULL AND TRIM(annotated_transcript) <> '' \
           AND verdict_transcript IS NOT NULL AND TRIM(verdict_transcript) <> '' \
           AND raw_transcript IS NOT NULL AND TRIM(raw_transcript) <> ''",
    )?;
    let rows =
        stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_quality_lift_rewards_jury_corrections() {
        // Raw ASR is wrong; the jury restores the reference -> positive lift, jury CER 0.
        let triples = vec![
            ("hello world".to_string(), "hello word".to_string(), "hello world".to_string()),
            ("good morning".to_string(), "good mrning".to_string(), "good morning".to_string()),
        ];
        let lift = compute_label_quality_lift(&triples, 200, 42);
        assert_eq!(lift.n, 2);
        assert!(lift.raw_micro_cer > 0.0, "raw should have errors: {}", lift.raw_micro_cer);
        assert!(lift.jury_micro_cer.abs() < 1e-9, "jury matches reference: {}", lift.jury_micro_cer);
        assert!(lift.cer_lift > 0.0, "jury improved labels: lift={}", lift.cer_lift);
        assert!(lift.lift_ci_low <= lift.lift_ci_high, "CI bounds ordered");
    }

    #[test]
    fn label_quality_lift_zero_when_jury_no_better() {
        let triples = vec![("hello world".to_string(), "hello word".to_string(), "hello word".to_string())];
        let lift = compute_label_quality_lift(&triples, 50, 1);
        assert!(lift.cer_lift.abs() < 1e-9, "no improvement -> ~0 lift: {}", lift.cer_lift);
    }

    #[test]
    fn label_quality_lift_empty_is_zero() {
        let lift = compute_label_quality_lift(&[], 100, 7);
        assert_eq!(lift.n, 0);
        assert_eq!(lift.cer_lift, 0.0);
    }

    fn open_mem_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db
    }

    #[test]
    fn create_gold_from_verified_file_concatenates_corrected_segments() {
        let db = open_mem_db();
        // Two REVIEWED segments of the SAME source file, corrected (verdict_transcript), with explicit
        // ordered timestamps so concatenation order is deterministic.
        for (id, fix, at) in
            [("c1", "ساڵی نوێ پیرۆز", "2020-01-01 00:00:01"), ("c2", "بەخێربێیت بۆ کوردستان", "2020-01-01 00:00:02")]
        {
            db.insert_segment(&crate::db::SpeechSegment {
                id: id.to_string(),
                audio_path: "/clips/nawras.wav".to_string(),
                raw_transcript: "draft".to_string(),
                ..Default::default()
            })
            .unwrap();
            db.connection()
                .execute(
                    "UPDATE speech_segments SET human_decision='edit', verdict_transcript=?2, created_at=?3 WHERE id=?1",
                    params![id, fix, at],
                )
                .unwrap();
        }
        // An UNREVIEWED segment of the same file must be excluded from the gold reference.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "c3".to_string(),
            audio_path: "/clips/nawras.wav".to_string(),
            raw_transcript: "ناوەند unreviewed".to_string(),
            ..Default::default()
        })
        .unwrap();

        let created = create_gold_from_verified_file(&db, "/clips/nawras.wav").unwrap();
        assert_eq!(created, 1, "one whole-file gold entry");
        let gold = list_gold_segments(&db).unwrap();
        assert_eq!(gold.len(), 1);
        assert!(gold[0].is_holdout, "gold must be holdout so the learning loop never trains on it");
        assert_eq!(
            gold[0].reference, "ساڵی نوێ پیرۆز بەخێربێیت بۆ کوردستان",
            "corrected segments are concatenated in time order"
        );
        assert!(!gold[0].reference.contains("unreviewed"), "unreviewed segments are excluded");

        // A file with no reviewed segments errors (correct it in the app first).
        assert!(create_gold_from_verified_file(&db, "/clips/missing.wav").is_err());
    }

    #[test]
    fn gold_reference_stays_in_segment_order_on_same_second_ties() {
        // Round-3 audit: a chunked file's segments batch-insert with the SAME created_at second. The
        // `, rowid ASC` tiebreaker must keep the concatenation in true (insertion = chunk) order
        // instead of SQLite's undefined tie order.
        let db = open_mem_db();
        for (id, fix) in [("g1", "alpha"), ("g2", "beta"), ("g3", "gamma")] {
            db.insert_segment(&crate::db::SpeechSegment {
                id: id.to_string(),
                audio_path: "/clips/tie.wav".to_string(),
                raw_transcript: "draft".to_string(),
                ..Default::default()
            })
            .unwrap();
            db.connection()
                .execute(
                    "UPDATE speech_segments SET human_decision='edit', verdict_transcript=?2, \
                     created_at='2020-01-01 00:00:05' WHERE id=?1",
                    params![id, fix],
                )
                .unwrap();
        }
        create_gold_from_verified_file(&db, "/clips/tie.wav").unwrap();
        let gold = list_gold_segments(&db).unwrap();
        assert_eq!(gold[0].reference, "alpha beta gamma", "concatenation stays in segment order on a tie");
    }

    #[test]
    fn test_import_and_list_gold() {
        let db = open_mem_db();
        let inputs = vec![
            GoldSegmentInput {
                audio_path: "/tmp/a.wav".into(), reference: "کوردستان".into(), is_holdout: true
            },
            GoldSegmentInput {
                audio_path: "/tmp/b.wav".into(), reference: "ئەمە دەنگە".into(), is_holdout: true
            },
        ];
        let count = import_gold_segments(&db, inputs).unwrap();
        assert_eq!(count, 2);
        let list = list_gold_segments(&db).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn re_marking_same_audio_as_gold_is_idempotent() {
        // Round-9 audit MEDIUM: re-marking the same clip as gold inserted a SECOND holdout row (the id
        // is a fresh UUID, so INSERT OR IGNORE never dedup'd), which run_gold_eval then double-counts
        // in the WER/CER aggregates. Re-import must REPLACE the prior row for the same audio_path.
        let db = open_mem_db();
        import_gold_segments(
            &db,
            vec![GoldSegmentInput {
                audio_path: "/tmp/dup.wav".into(),
                reference: "first reference".into(),
                is_holdout: true,
            }],
        )
        .unwrap();
        import_gold_segments(
            &db,
            vec![GoldSegmentInput {
                audio_path: "/tmp/dup.wav".into(),
                reference: "corrected reference".into(),
                is_holdout: true,
            }],
        )
        .unwrap();

        let list = list_gold_segments(&db).unwrap();
        let for_clip: Vec<_> = list.iter().filter(|g| g.audio_path == "/tmp/dup.wav").collect();
        assert_eq!(for_clip.len(), 1, "re-marking the same audio must keep exactly one gold row");
        assert_eq!(for_clip[0].reference, "corrected reference", "the latest reference wins");
    }

    #[test]
    fn test_run_gold_eval_empty() {
        let db = open_mem_db();
        let result = run_gold_eval(&db, "test-model", vec![]).unwrap();
        assert_eq!(result.run.num_segs, 0);
        assert_eq!(result.run.wer, 0.0);
    }

    #[test]
    fn test_run_gold_eval_with_data() {
        let db = open_mem_db();
        let inputs =
            vec![GoldSegmentInput {
                audio_path: "/tmp/a.wav".into(), reference: "کوردستان".into(), is_holdout: true
            }];
        import_gold_segments(&db, inputs).unwrap();
        let gold = list_gold_segments(&db).unwrap();
        let gold_id = gold[0].id.clone();

        // Perfect match → WER = 0
        let result = run_gold_eval(&db, "perfect-model", vec![(gold_id.clone(), "کوردستان".into())]).unwrap();
        assert_eq!(result.run.num_segs, 1);
        assert!(result.run.wer < 0.01, "Perfect match should have ~0 WER");

        // Wrong hypothesis → WER > 0
        let result2 = run_gold_eval(&db, "bad-model", vec![(gold_id, "خراب".into())]).unwrap();
        assert!(result2.run.wer > 0.0, "Wrong hypothesis should have WER > 0");
    }

    #[test]
    fn empty_reference_segment_does_not_peg_micro_rate_to_one() {
        let db = open_mem_db();
        // One real gold (will match perfectly) + one whose reference is tatweel-only, so it normalizes to
        // EMPTY for metrics, paired with a non-empty (hallucinated) hypothesis. The corpus micro WER/CER
        // must reflect only the reference-bearing clip (~0%), NOT be pegged to its 1.0 clamp by the
        // zero-reference insertions.
        import_gold_segments(
            &db,
            vec![
                GoldSegmentInput {
                    audio_path: "/a.wav".into(), reference: "کوردستان".into(), is_holdout: true
                },
                GoldSegmentInput {
                    audio_path: "/b.wav".into(),
                    reference: "\u{0640}\u{0640}\u{0640}".into(), // tatweel-only -> normalizes to ""
                    is_holdout: true,
                },
            ],
        )
        .unwrap();
        let gold = list_gold_segments(&db).unwrap();
        let hyps: Vec<(String, String)> = gold
            .iter()
            .map(|g| {
                let hyp = if g.audio_path == "/a.wav" { "کوردستان" } else { "one two three" };
                (g.id.clone(), hyp.to_string())
            })
            .collect();
        let result = run_gold_eval(&db, "m", hyps).unwrap();
        assert!(
            result.run.wer < 0.01 && result.run.cer < 0.01,
            "an empty-reference hallucination must not peg micro WER/CER to 1.0 (wer={}, cer={})",
            result.run.wer,
            result.run.cer
        );
    }

    #[test]
    fn run_gold_eval_with_transcriber_runs_per_segment_and_scores() {
        let db = open_mem_db();
        import_gold_segments(
            &db,
            vec![
                GoldSegmentInput {
                    audio_path: "/tmp/a.wav".into(), reference: "کوردستان".into(), is_holdout: true
                },
                GoldSegmentInput {
                    audio_path: "/tmp/b.wav".into(), reference: "ئەمە دەنگە".into(), is_holdout: true
                },
            ],
        )
        .unwrap();

        // Fake transcriber: perfect on the first reference, wrong on the second.
        let mut calls = 0usize;
        let result = run_gold_eval_with_transcriber(&db, "fake-asr", |seg| {
            calls += 1;
            Ok(if seg.reference == "کوردستان" {
                "کوردستان".to_string()
            } else {
                "خراب".to_string()
            })
        })
        .unwrap();

        assert_eq!(calls, 2, "transcriber must be invoked exactly once per gold segment");
        assert_eq!(result.run.num_segs, 2);
        assert_eq!(result.run.model_id, "fake-asr");
        assert!(result.run.wer > 0.0, "one wrong hypothesis should yield a non-zero mean WER");
        assert_eq!(result.segments.len(), 2);
    }

    #[test]
    fn run_gold_eval_with_transcriber_skips_failures_without_scoring_them() {
        let db = open_mem_db();
        import_gold_segments(
            &db,
            vec![
                GoldSegmentInput {
                    audio_path: "/tmp/ok.wav".into(), reference: "کوردستان".into(), is_holdout: true
                },
                GoldSegmentInput { audio_path: "/missing.wav".into(), reference: "ئەمە".into(), is_holdout: true },
            ],
        )
        .unwrap();

        let result = run_gold_eval_with_transcriber(&db, "partial-asr", |seg| {
            if seg.audio_path.contains("missing") {
                Err(crate::error::AppError::Other("decode failed".into()))
            } else {
                Ok("کوردستان".to_string())
            }
        })
        .unwrap();

        // Only the successfully-transcribed segment is scored; the failed one is skipped,
        // not counted as an empty hypothesis (which would understate accuracy).
        assert_eq!(result.run.num_segs, 1);
        assert_eq!(result.segments.len(), 1);
        assert!(result.run.wer < 0.01);
    }

    #[test]
    fn test_list_eval_runs() {
        let db = open_mem_db();
        let runs = list_eval_runs(&db).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn test_run_gold_eval_and_recompute_from_db() {
        let db = open_mem_db();
        let inputs = vec![
            GoldSegmentInput {
                audio_path: "/tmp/a.wav".into(), reference: "کوردستان".into(), is_holdout: true
            },
            GoldSegmentInput {
                audio_path: "/tmp/b.wav".into(), reference: "ئەمە دەنگە".into(), is_holdout: true
            },
            // Empty-reference clip (tatweel-only ref -> normalizes to "") paired with a hallucinated
            // hypothesis. Its persisted row carries word/char_distance > 0 with ref_len == 0; the reload
            // path must skip it from the micro accumulators just like the write path, or recompute pegs
            // to 1.0 while the stored run reads ~0% — exactly the divergence the reload invariant exists
            // to catch. Without an empty-ref row in the fixture, this assertion never exercised it.
            GoldSegmentInput {
                audio_path: "/tmp/c.wav".into(),
                reference: "\u{0640}\u{0640}\u{0640}".into(),
                is_holdout: true,
            },
        ];
        import_gold_segments(&db, inputs).unwrap();
        let gold = list_gold_segments(&db).unwrap();

        let hyps: Vec<(String, String)> = gold
            .iter()
            .map(|g| {
                let hyp = match g.audio_path.as_str() {
                    "/tmp/a.wav" => "کوردستان",
                    "/tmp/b.wav" => "ئەمە",
                    _ => "one two three", // hallucination on the empty-reference clip
                };
                (g.id.clone(), hyp.to_string())
            })
            .collect();

        let result = run_gold_eval(&db, "test-model", hyps).unwrap();
        let recomputed = load_eval_run_and_recompute(&db, &result.run.id).unwrap().unwrap();

        assert_eq!(result.run.id, recomputed.0.id);
        assert_eq!(result.run.num_segs, recomputed.0.num_segs);
        assert_eq!(
            result.run.wer, recomputed.0.wer,
            "reloaded micro WER must equal the stored value (empty-ref guard)"
        );
        assert_eq!(
            result.run.cer, recomputed.0.cer,
            "reloaded micro CER must equal the stored value (empty-ref guard)"
        );
        // The reloaded micro rate must reflect only the reference-bearing clips (~0.33 from b's one
        // deletion), NOT be pegged to 1.0 by the empty-ref hallucination's insertions over a 0 denominator.
        assert!(recomputed.0.wer < 1.0, "empty-ref hallucination must not peg the RELOADED micro WER to 1.0");
        assert!(recomputed.0.cer < 1.0, "empty-ref hallucination must not peg the RELOADED micro CER to 1.0");
        assert_eq!(result.segments.len(), recomputed.1.len());
    }
}
