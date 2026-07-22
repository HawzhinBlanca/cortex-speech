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
    // REJECT GUARD (true-10 audit): a human-REJECTED chunk has no valid text — its draft is known
    // wrong, yet the holdout WAV still CONTAINS that chunk's speech. Including the rejected draft
    // poisons the reference with wrong text; silently omitting the chunk poisons it the other way
    // (the audio has speech the reference lacks, so every eval scores spurious insertions). Neither
    // is a usable whole-file gold reference — refuse the file until the chunk is corrected.
    let rejected: i64 = db.connection().query_row(
        "SELECT COUNT(*) FROM speech_segments
         WHERE audio_path = ?1
           AND (human_decision IN ('reject', 'human_reject') OR verdict = 'human_reject')",
        params![audio_path],
        |row| row.get(0),
    )?;
    if rejected > 0 {
        return Err(crate::error::AppError::Validation(format!(
            "'{audio_path}' has {rejected} rejected chunk(s) — their audio is still in the file, so no \
             correct whole-file reference exists; correct (edit) or re-transcribe them first"
        )));
    }

    // COMPLETENESS GUARD (true-10 audit 2026-07-09): the same hazard the reject guard describes,
    // from the other side — an UNREVIEWED chunk's speech is in the holdout WAV but its text is
    // missing from the concatenated reference, so every future engine benchmark scores those spans
    // as spurious insertions, permanently inflating WER/CER on the very yardstick the promotion
    // gate measures against. Refuse until every chunk of the file carries a human decision;
    // import_verified_segments_as_gold skips incomplete files via its warn-and-continue path.
    let unreviewed: i64 = db.connection().query_row(
        "SELECT COUNT(*) FROM speech_segments
         WHERE audio_path = ?1 AND (human_decision IS NULL OR human_decision = '')",
        params![audio_path],
        |row| row.get(0),
    )?;
    if unreviewed > 0 {
        return Err(crate::error::AppError::Validation(format!(
            "'{audio_path}' has {unreviewed} unreviewed chunk(s) — their speech would be in the holdout \
             WAV but missing from the reference (every eval then scores spurious insertions); review \
             every chunk of the file before promoting it to gold"
        )));
    }

    let mut stmt = db.connection().prepare(
        // Preference order MUST match the training/eval hypothesis surface form (quality::hypothesis_
        // transcript is RAW ASR, with digits): verdict_transcript (human-decided) ▸ annotated_transcript
        // (human edit) ▸ raw_transcript. NEVER normalized_transcript — with verbalize_numbers on it turns
        // "٥" into "پێنج", so every future eval hypothesis (which emits digits) scores a built-in WER/CER
        // penalty against the gold reference that no model can beat, corrupting the promotion-gate numbers.
        "SELECT COALESCE(NULLIF(verdict_transcript, ''), NULLIF(annotated_transcript, ''), raw_transcript)
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

/// M2.7 / P1.6: bulk-promote every source file that has human-reviewed segments into the gold set
/// (file-level, reusing the boundary-safe concatenation of `create_gold_from_verified_file`). Idempotent
/// — re-running replaces each file's gold row rather than duplicating it. A file that turns out to have
/// no reviewed segments (a race) is skipped with a log, never failing the batch. Returns rows created.
pub fn import_verified_segments_as_gold(db: &Database) -> AppResult<usize> {
    let paths: Vec<String> = {
        let mut stmt = db.connection().prepare(
            "SELECT DISTINCT audio_path FROM speech_segments
             WHERE human_decision IS NOT NULL AND human_decision != '' ORDER BY audio_path",
        )?;
        let rows: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
        rows
    };
    let mut total = 0usize;
    for path in paths {
        match create_gold_from_verified_file(db, &path) {
            Ok(n) => total += n,
            Err(error) => tracing::warn!("gold promotion skipped for {path}: {error}"),
        }
    }
    Ok(total)
}

/// Summary of an `export_gold_eval_set` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoldEvalExport {
    pub manifest_path: String,
    pub total_gold: usize,
    pub exported: usize,
    /// Gold rows whose source audio could not be decoded (missing/corrupt) — excluded so the eval set
    /// stays self-consistent (every manifest row has a real clip).
    pub skipped: usize,
}

/// One JSONL row in the exported gold eval-set manifest — the trainer/eval schema.
#[derive(Serialize)]
struct GoldManifestRow<'a> {
    /// Relative to the manifest, so the exported set is portable.
    audio_path: String,
    sentence: &'a str,
    is_holdout: bool,
    duration_seconds: f64,
}

/// Write a 16 kHz mono 16-bit PCM WAV clip.
fn write_wav_16k_mono(path: &std::path::Path, pcm: &[i16], sample_rate: u32) -> AppResult<()> {
    let spec =
        hound::WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| crate::error::AppError::Other(format!("gold clip WAV create: {e}")))?;
    for &s in pcm {
        writer.write_sample(s).map_err(|e| crate::error::AppError::Other(format!("gold clip WAV write: {e}")))?;
    }
    writer.finalize().map_err(|e| crate::error::AppError::Other(format!("gold clip WAV finalize: {e}")))?;
    Ok(())
}

/// M2.7 / P1.6: export the gold set as a portable eval set — a `manifest.jsonl` (one
/// `{audio_path, sentence, is_holdout, duration_seconds}` row per gold clip) plus a self-contained
/// 16 kHz mono WAV per row under `clips/`. This is what the engine benchmark scores against; the
/// `is_holdout` flag is carried through so a downstream TRAINING pack can exclude it (holdout is a
/// train-time exclusion, not an eval-time one — the eval set deliberately includes it). Gold whose
/// source audio can no longer be decoded is skipped so the set stays self-consistent.
pub fn export_gold_eval_set(db: &Database, out_dir: &std::path::Path) -> AppResult<GoldEvalExport> {
    use std::io::Write as _;
    let gold = list_gold_segments(db)?;
    let clips_dir = out_dir.join("clips");
    std::fs::create_dir_all(&clips_dir).map_err(crate::error::AppError::Io)?;

    let manifest_path = out_dir.join("manifest.jsonl");
    let mut manifest =
        std::io::BufWriter::new(std::fs::File::create(&manifest_path).map_err(crate::error::AppError::Io)?);

    let mut exported = 0usize;
    let mut skipped = 0usize;
    for g in &gold {
        let (sample_rate, pcm) = match crate::audio::decode_to_pcm(&g.audio_path) {
            Ok(decoded) => decoded,
            Err(error) => {
                tracing::warn!("gold eval-set: skipping {} (source undecodable): {error}", g.id);
                skipped += 1;
                continue;
            }
        };
        let clip_rel = format!("clips/{}.wav", g.id);
        if let Err(error) = write_wav_16k_mono(&clips_dir.join(format!("{}.wav", g.id)), &pcm, sample_rate) {
            tracing::warn!("gold eval-set: skipping {} (clip write failed): {error}", g.id);
            skipped += 1;
            continue;
        }
        let row = GoldManifestRow {
            audio_path: clip_rel,
            sentence: &g.reference,
            is_holdout: g.is_holdout,
            duration_seconds: pcm.len() as f64 / sample_rate as f64,
        };
        let line = serde_json::to_string(&row)
            .map_err(|e| crate::error::AppError::Other(format!("gold manifest serialize: {e}")))?;
        writeln!(manifest, "{line}").map_err(crate::error::AppError::Io)?;
        exported += 1;
    }
    manifest.flush().map_err(crate::error::AppError::Io)?;
    drop(manifest);
    // Integrity over the manifest + every clip: a truncated/partially-copied gold WAV would silently
    // corrupt the yardstick every promotion decision measures against (true-10 audit 2026-07-09).
    crate::export::write_sha256sums(out_dir)?;

    Ok(GoldEvalExport {
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        total_gold: gold.len(),
        exported,
        skipped,
    })
}

/// Summary of an `export_finetune_pack` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinetunePackResult {
    pub manifest_path: String,
    /// P5.5: pins the exact rows this pack contains — the corpus-ledger key a champion traces back to.
    pub manifest_sha256: String,
    pub total_verified: usize,
    /// Verified segments dropped because their audio is a HOLDOUT gold clip (the eval-set leak guard).
    pub excluded_holdout: usize,
    /// Verified segments the training-grade rubric refused (the B1 guard): human-rejected (mark-bad
    /// carries verified=true only to leave the review queue), severe audio issues, placeholder text,
    /// or any other non-training-ready grade. Without this filter a REJECTED clip's bad draft would
    /// ship as a training label.
    pub excluded_not_training_ready: usize,
    pub emitted: usize,
    /// Skipped for an empty transcript, a duplicate, or undecodable audio.
    pub skipped: usize,
}

/// One JSONL row in the fine-tune pack — the trainer's schema.
#[derive(Serialize)]
struct FinetuneRow<'a> {
    audio_path: String,
    sentence: &'a str,
    duration_seconds: f64,
}

/// M5.1 / P5.1: export a fine-tune training pack from the segments the training-grade rubric
/// certifies — the trainer's manifest schema (`{audio_path, sentence, duration_seconds}`) + a 16 kHz
/// WAV clip per row under `clips/`. Two guards, both load-bearing:
///
/// 1. HOLDOUT leak guard: gold holdout clips are excluded (path AND content hash, via
///    `exclude_holdout_segments`) so training never contaminates the eval set the promotion gate
///    measures against.
/// 2. RUBRIC guard (B1): every candidate must be `training_ready` per
///    `quality::training_grade_for_segment` (GOLD/SILVER only). `verified=true` alone is NOT
///    sufficient — mark-bad sets verified=true merely to leave the review queue, so without the
///    rubric a human-REJECTED clip's bad draft would ship as a training label. The rubric also
///    refuses severe audio issues and placeholder text, and its transcript preference (unlike a
///    naive verdict-first pick) never selects a rejected verdict draft.
///
/// Rows are deduped by (audio span, normalized text) so a re-imported duplicate is not trained on
/// twice; segments with an empty transcript or undecodable audio are skipped.
pub fn export_finetune_pack(
    db: &Database,
    out_dir: &std::path::Path,
    corpus_ledger_path: Option<&std::path::Path>,
) -> AppResult<FinetunePackResult> {
    use std::io::Write as _;
    let verified = db.get_segments(Some(true))?;
    let total_verified = verified.len();
    // THE LEAK GUARD: drop any verified segment whose audio is a holdout gold clip.
    let kept = crate::export::exclude_holdout_segments(db, verified)?;
    let excluded_holdout = total_verified - kept.len();

    // THE RUBRIC GUARD (B1): only training-ready (GOLD/SILVER) rows may ship, and the shipped
    // sentence is the rubric's own transcript — the single source of truth shared with the CSV/HF
    // exports' training_transcript column.
    let graded: Vec<(&crate::db::SpeechSegment, crate::quality::TrainingGradeReport)> =
        kept.iter().map(|seg| (seg, crate::quality::training_grade_for_segment(seg))).collect();
    let excluded_not_training_ready = graded.iter().filter(|(_, report)| !report.training_ready).count();

    let clips_dir = out_dir.join("clips");
    std::fs::create_dir_all(&clips_dir).map_err(crate::error::AppError::Io)?;
    let manifest_path = out_dir.join("finetune_manifest.jsonl");
    let mut manifest =
        std::io::BufWriter::new(std::fs::File::create(&manifest_path).map_err(crate::error::AppError::Io)?);

    // Sibling counts over the WHOLE library (not just verified rows): a segment whose alignment is
    // offset-less may only fall back to whole-file decode when its source truly has one segment —
    // on a multi-segment source that fallback shipped the entire recording as one "clip" (true-10
    // audit 2026-07-09; decode_finetuned_clip_16k enforces it, this map supplies the context).
    let sibling_counts: std::collections::HashMap<String, i64> = {
        let mut stmt = db
            .connection()
            .prepare("SELECT audio_path, COUNT(*) FROM speech_segments GROUP BY audio_path")
            .map_err(crate::error::AppError::from)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(crate::error::AppError::from)?;
        rows.collect::<Result<_, _>>().map_err(crate::error::AppError::from)?
    };

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut emitted = 0usize;
    let mut skipped = 0usize;
    for (seg, report) in graded.iter().filter(|(_, report)| report.training_ready) {
        // Canonical Sorani orthography for the SHIPPED sentence — ك/ک, ي/ی, ه/ھ variants unified so
        // the retrain corpus has one label per grapheme (mixed forms inflate the CTC label space).
        let sentence = crate::normalizer::canonical_training_text(&report.transcript);
        if sentence.trim().is_empty() {
            skipped += 1;
            continue;
        }
        // Dedup by the variant-unifying hash form (not plain lowercase) so a re-imported duplicate
        // written with different codepoint variants still dedups to one training row.
        let norm = crate::quality::normalize_transcript_for_hash(&sentence);
        let dedup_key = format!("{}|{}|{}", seg.audio_path, seg.alignment_json.as_deref().unwrap_or(""), norm);
        if !seen.insert(dedup_key) {
            skipped += 1;
            continue;
        }
        let single_segment_source = sibling_counts.get(&seg.audio_path).copied().unwrap_or(1) <= 1;
        let pcm = match crate::commands::decode_finetuned_clip_16k(
            &seg.audio_path,
            seg.alignment_json.as_deref(),
            single_segment_source,
        ) {
            Ok(pcm) if !pcm.is_empty() => pcm,
            Err(reason) => {
                tracing::warn!("finetune pack: skipping {} ({reason})", seg.id);
                skipped += 1;
                continue;
            }
            _ => {
                tracing::warn!("finetune pack: skipping {} (clip undecodable)", seg.id);
                skipped += 1;
                continue;
            }
        };
        let clip_rel = format!("clips/{}.wav", seg.id);
        write_wav_16k_mono(&clips_dir.join(format!("{}.wav", seg.id)), &pcm, crate::audio::TARGET_SAMPLE_RATE)?;
        let row = FinetuneRow {
            audio_path: clip_rel,
            sentence: &sentence,
            duration_seconds: pcm.len() as f64 / crate::audio::TARGET_SAMPLE_RATE as f64,
        };
        let line = serde_json::to_string(&row)
            .map_err(|e| crate::error::AppError::Other(format!("finetune manifest serialize: {e}")))?;
        writeln!(manifest, "{line}").map_err(crate::error::AppError::Io)?;
        emitted += 1;
    }
    manifest.flush().map_err(crate::error::AppError::Io)?;
    drop(manifest);

    // P5.5 (corpus ledger): every training pack is traceable to its exact data. A self-describing
    // provenance record goes INSIDE the pack (pack_provenance.json) and the same line is appended to
    // the durable <data_dir>/corpus_ledger.jsonl (survives pack deletion) when a ledger path is
    // given. The manifest SHA pins the exact rows a future champion was trained on.
    let manifest_sha256 = crate::models::compute_file_sha256(&manifest_path)
        .map_err(|e| crate::error::AppError::Other(format!("pack manifest sha: {e}")))?;
    let provenance = serde_json::json!({
        "schema": 1,
        "createdAt": chrono::Utc::now().to_rfc3339(),
        "appGitSha": crate::GIT_SHA,
        "manifestSha256": manifest_sha256,
        "emitted": emitted,
        "skipped": skipped,
        "excludedHoldout": excluded_holdout,
        "excludedNotTrainingReady": excluded_not_training_ready,
        "totalVerified": total_verified,
        "selectionPolicy": "training_ready (GOLD/SILVER) via quality::training_grade_for_segment; holdout-excluded; canonical Sorani orthography; variant-aware dedup",
    });
    let provenance_text = serde_json::to_string_pretty(&provenance)
        .map_err(|e| crate::error::AppError::Other(format!("pack provenance serialize: {e}")))?;
    std::fs::write(out_dir.join("pack_provenance.json"), &provenance_text).map_err(crate::error::AppError::Io)?;
    // Integrity over EVERY pack file (clips included), written last so it also covers the provenance
    // record: manifestSha256 alone pins the rows but not the audio bytes — a truncated/partially-
    // copied WAV was undetectable while the manifest SHA stayed green (true-10 audit 2026-07-09).
    crate::export::write_sha256sums(out_dir)?;
    if let Some(ledger) = corpus_ledger_path {
        use std::io::Write as _;
        let line = serde_json::to_string(&provenance)
            .map_err(|e| crate::error::AppError::Other(format!("corpus ledger serialize: {e}")))?;
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(ledger).map_err(crate::error::AppError::Io)?;
        writeln!(file, "{line}").map_err(crate::error::AppError::Io)?;
    }

    Ok(FinetunePackResult {
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        manifest_sha256,
        total_verified,
        excluded_holdout,
        excluded_not_training_ready,
        emitted,
        skipped,
    })
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
        // LLM-refined annotated_transcript (commands.rs notes annotated can be human OR LLM). EXCLUDE
        // human-REJECTED clips: a reject is a human decision that means the OPPOSITE of ground truth — the
        // reviewer discarded the clip, so its (possibly LLM-refined) annotated_transcript was never
        // confirmed. record_human_decision leaves the transcripts populated on a reject, so without this
        // guard a rejected clip inflates LabelQualityLift.n and folds its char distances into the MEASURED
        // raw/jury micro-CER + lift + CI, crediting the jury for "improving" a label on a clip that never
        // ships (export_dataset drops it via is_human_rejected).
        "SELECT annotated_transcript, raw_transcript, verdict_transcript \
         FROM speech_segments \
         WHERE human_decision IS NOT NULL AND TRIM(human_decision) <> '' \
           AND human_decision NOT IN ('reject', 'human_reject') \
           AND COALESCE(verdict, '') <> 'human_reject' \
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
    fn load_lift_triples_excludes_human_rejected_clips() {
        // The label-quality lift uses the human's annotated_transcript as GROUND TRUTH. A human-REJECTED
        // clip's annotation was never confirmed (the reviewer discarded the clip), and record_human_decision
        // leaves its transcripts populated — so without a reject guard it inflates the MEASURED lift (n +
        // micro-CER + CI) over a row that never ships (export_dataset drops it via is_human_rejected).
        let db = open_mem_db();
        for (id, decision, verdict) in [("acc", "edit", "jury_edit"), ("rej", "reject", "human_reject")] {
            db.insert_segment(&crate::db::SpeechSegment {
                id: id.to_string(),
                audio_path: format!("/clips/{id}.wav"),
                raw_transcript: "خاو".to_string(),
                annotated_transcript: Some("دەقی ڕاست".to_string()),
                ..Default::default()
            })
            .unwrap();
            // verdict / verdict_transcript / human_decision are jury/decision columns that insert_segment
            // omits by design, so set them here — including verdict_transcript (the post-jury label).
            db.connection()
                .execute(
                    "UPDATE speech_segments SET human_decision=?2, verdict=?3, verdict_transcript='دەقی ڕاست' WHERE id=?1",
                    params![id, decision, verdict],
                )
                .unwrap();
        }
        let triples = load_lift_triples(&db).unwrap();
        assert_eq!(triples.len(), 1, "only the accepted clip is a lift triple; the rejected one is excluded");
        assert_eq!(triples[0].0, "دەقی ڕاست", "the surviving triple is the accepted clip's human reference");
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
        // True-10 audit 2026-07-09 CONTRACT CHANGE: an unreviewed segment of the same file no longer
        // silently drops out of the reference (its speech would stay in the holdout WAV unlabeled,
        // scoring spurious insertions on every eval) — promotion now REFUSES the incomplete file.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "c3".to_string(),
            audio_path: "/clips/nawras.wav".to_string(),
            raw_transcript: "ناوەند unreviewed".to_string(),
            ..Default::default()
        })
        .unwrap();
        let err = create_gold_from_verified_file(&db, "/clips/nawras.wav").unwrap_err();
        assert!(err.to_string().contains("unreviewed chunk"), "incomplete file is refused: {err}");

        // Review the last chunk (with a later timestamp so order is deterministic) — now it promotes
        // and the reference includes EVERY chunk's speech, in time order.
        db.connection()
            .execute(
                "UPDATE speech_segments SET human_decision='edit', verdict_transcript='ناوەندی نوێ',
                 created_at='2020-01-01 00:00:03' WHERE id='c3'",
                [],
            )
            .unwrap();
        let created = create_gold_from_verified_file(&db, "/clips/nawras.wav").unwrap();
        assert_eq!(created, 1, "one whole-file gold entry");
        let gold = list_gold_segments(&db).unwrap();
        assert_eq!(gold.len(), 1);
        assert!(gold[0].is_holdout, "gold must be holdout so the learning loop never trains on it");
        assert_eq!(
            gold[0].reference, "ساڵی نوێ پیرۆز بەخێربێیت بۆ کوردستان ناوەندی نوێ",
            "corrected segments are concatenated in time order, covering every chunk"
        );

        // A file with no reviewed segments errors (correct it in the app first).
        assert!(create_gold_from_verified_file(&db, "/clips/missing.wav").is_err());
    }

    #[test]
    fn gold_reference_prefers_annotated_over_verbalized_normalized() {
        // The gold reference must match the raw-ASR hypothesis surface form (digits), so it uses
        // annotated_transcript, never the number-VERBALIZED normalized_transcript — otherwise every
        // future eval hypothesis carries a built-in WER/CER penalty vs the gold that no model can beat.
        let db = open_mem_db();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "g1".to_string(),
            audio_path: "/clips/nums.wav".to_string(),
            raw_transcript: "raw".to_string(),
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET human_decision='accept', verdict_transcript='',
                     annotated_transcript='ساڵی ٥', normalized_transcript='ساڵی پێنج' WHERE id='g1'",
                [],
            )
            .unwrap();
        create_gold_from_verified_file(&db, "/clips/nums.wav").unwrap();
        let gold = list_gold_segments(&db).unwrap();
        assert_eq!(
            gold[0].reference, "ساڵی ٥",
            "gold reference uses the annotated digit form, not the verbalized normalized text"
        );
    }

    #[test]
    fn import_verified_segments_as_gold_promotes_every_reviewed_file() {
        // P1.6: bulk ingest promotes each distinct reviewed source file (file-level), skipping files
        // with no reviewed segments.
        let db = open_mem_db();
        for (id, path) in [("a1", "/clips/a.wav"), ("b1", "/clips/b.wav")] {
            db.insert_segment(&crate::db::SpeechSegment {
                id: id.to_string(),
                audio_path: path.to_string(),
                raw_transcript: "draft".to_string(),
                ..Default::default()
            })
            .unwrap();
            db.connection()
                .execute(
                    "UPDATE speech_segments SET human_decision='accept', verdict_transcript='گۆڵد' WHERE id=?1",
                    params![id],
                )
                .unwrap();
        }
        db.insert_segment(&crate::db::SpeechSegment {
            id: "c1".to_string(),
            audio_path: "/clips/c.wav".to_string(),
            raw_transcript: "unreviewed".to_string(),
            ..Default::default()
        })
        .unwrap();

        let n = import_verified_segments_as_gold(&db).unwrap();
        assert_eq!(n, 2, "the two reviewed files become gold; the unreviewed file is skipped");
        assert_eq!(list_gold_segments(&db).unwrap().len(), 2);
    }

    #[test]
    fn export_gold_eval_set_writes_manifest_and_clips() {
        // P1.6: the exported eval set is a JSONL manifest + a self-contained 16 kHz WAV per gold row.
        let db = open_mem_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let wav_path = tmp.path().join("clip.wav");
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&wav_path, spec).unwrap();
            for i in 0..16000i32 {
                w.write_sample(((i % 100) - 50) as i16).unwrap(); // ~1 second
            }
            w.finalize().unwrap();
        }
        let wav_str = wav_path.to_string_lossy().to_string();
        import_gold_segments(
            &db,
            vec![GoldSegmentInput { audio_path: wav_str, reference: "ڕەفەرێنس".to_string(), is_holdout: true }],
        )
        .unwrap();

        let out = tempfile::TempDir::new().unwrap();
        let export = export_gold_eval_set(&db, out.path()).unwrap();
        assert_eq!(export.total_gold, 1);
        assert_eq!(export.exported, 1);
        assert_eq!(export.skipped, 0);

        let manifest = std::fs::read_to_string(out.path().join("manifest.jsonl")).unwrap();
        let lines: Vec<&str> = manifest.lines().collect();
        assert_eq!(lines.len(), 1, "one manifest row per exported gold clip");
        let row: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row["sentence"], "ڕەفەرێنس");
        assert_eq!(row["is_holdout"], true, "the holdout flag is carried through for downstream training exclusion");
        let clip_rel = row["audio_path"].as_str().unwrap();
        assert!(clip_rel.starts_with("clips/"), "audio_path is relative for portability");
        assert!((row["duration_seconds"].as_f64().unwrap() - 1.0).abs() < 0.1, "~1s clip");
        assert!(out.path().join(clip_rel).exists(), "the referenced clip WAV was written");
    }

    #[test]
    fn export_gold_eval_set_skips_undecodable_source() {
        // A gold row whose source can't be decoded is skipped so the eval set stays self-consistent.
        let db = open_mem_db();
        import_gold_segments(
            &db,
            vec![GoldSegmentInput {
                audio_path: "/nonexistent/x.wav".to_string(),
                reference: "r".to_string(),
                is_holdout: false,
            }],
        )
        .unwrap();
        let out = tempfile::TempDir::new().unwrap();
        let export = export_gold_eval_set(&db, out.path()).unwrap();
        assert_eq!(export.total_gold, 1);
        assert_eq!(export.exported, 0);
        assert_eq!(export.skipped, 1);
        let manifest = std::fs::read_to_string(out.path().join("manifest.jsonl")).unwrap();
        assert!(manifest.trim().is_empty(), "no row for a skipped clip");
    }

    #[test]
    fn finetune_pack_excludes_holdout_and_emits_verified() {
        // P5.1: THE leak guard — a verified segment whose audio is a HOLDOUT gold clip must never enter
        // the training pack (it would contaminate the eval set the promotion gate measures against).
        let db = open_mem_db();
        let tmp = tempfile::TempDir::new().unwrap();
        // Real 16 kHz WAV so the KEEP segment's clip extracts.
        let keep_wav = tmp.path().join("keep.wav");
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&keep_wav, spec).unwrap();
            for i in 0..16000i32 {
                w.write_sample(((i % 100) - 50) as i16).unwrap();
            }
            w.finalize().unwrap();
        }
        // KEEP: a verified segment on a non-holdout file.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "keep".into(),
            audio_path: keep_wav.to_string_lossy().to_string(),
            raw_transcript: "ڕاستکراوە".into(),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("keep", true).unwrap();
        // LEAK: a holdout gold entry + a verified segment on the SAME audio path.
        import_gold_segments(
            &db,
            vec![GoldSegmentInput { audio_path: "/leak.wav".into(), reference: "r".into(), is_holdout: true }],
        )
        .unwrap();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "leak".into(),
            audio_path: "/leak.wav".into(),
            raw_transcript: "گۆڵد".into(),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("leak", true).unwrap();

        let out = tempfile::TempDir::new().unwrap();
        let ledger = out.path().join("corpus_ledger.jsonl");
        let result = export_finetune_pack(&db, out.path(), Some(&ledger)).unwrap();
        assert_eq!(result.total_verified, 2);
        assert_eq!(result.excluded_holdout, 1, "the holdout-matching segment is excluded (leak guard)");
        assert_eq!(result.emitted, 1, "only the non-holdout verified segment is emitted");

        let manifest = std::fs::read_to_string(out.path().join("finetune_manifest.jsonl")).unwrap();
        let lines: Vec<&str> = manifest.lines().collect();
        assert_eq!(lines.len(), 1, "one training row for the kept segment");
        let row: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row["sentence"], "ڕاستکراوە");
        let clip_rel = row["audio_path"].as_str().unwrap();
        assert!(clip_rel.starts_with("clips/"), "audio_path is relative");
        assert!(out.path().join(clip_rel).is_file(), "the training clip was written");
        assert!(!manifest.contains("گۆڵد"), "the holdout segment's text never enters the pack");
        assert_eq!(result.excluded_not_training_ready, 0, "both candidates were rubric-clean here");

        // P5.5: the pack is self-describing and the durable corpus ledger got the same record.
        let prov: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.path().join("pack_provenance.json")).unwrap()).unwrap();
        assert_eq!(prov["manifestSha256"], result.manifest_sha256.as_str());
        assert_eq!(prov["emitted"], 1);
        assert_eq!(prov["excludedHoldout"], 1);
        let ledger_text = std::fs::read_to_string(&ledger).unwrap();
        let ledger_line: serde_json::Value = serde_json::from_str(ledger_text.lines().next().unwrap()).unwrap();
        assert_eq!(ledger_line["manifestSha256"], result.manifest_sha256.as_str(), "ledger mirrors provenance");
        // The SHA really pins the manifest bytes.
        let recomputed = crate::models::compute_file_sha256(std::path::Path::new(&result.manifest_path)).unwrap();
        assert_eq!(recomputed, result.manifest_sha256);
    }

    #[test]
    fn finetune_pack_refuses_offsetless_alignment_on_multi_segment_sources() {
        // True-10 audit 2026-07-09 MAJOR: a chunk whose alignment_json is present but offset-less
        // (the historically-shipped clobber wrote word-array-only blobs) used to collapse into the
        // whole-file decode branch — the ENTIRE recording shipped as one "clip" labeled with a
        // single sentence, while the HF exporter correctly skipped the same row. On a MULTI-segment
        // source the pack must skip it; a truly single-segment source may still mean whole-file.
        let db = open_mem_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let wav = tmp.path().join("multi.wav");
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&wav, spec).unwrap();
            for i in 0..32000i32 {
                w.write_sample(((i % 100) - 50) as i16).unwrap();
            }
            w.finalize().unwrap();
        }
        let path = wav.to_string_lossy().to_string();
        // Chunk with intact offsets: emits.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "intact".into(),
            audio_path: path.clone(),
            raw_transcript: "چاکە".into(),
            alignment_json: Some(
                r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":2}"#.into(),
            ),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("intact", true).unwrap();
        // Sibling chunk whose offsets were clobbered to a bare word array: must be SKIPPED,
        // never shipped as the whole recording.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "clobbered".into(),
            audio_path: path.clone(),
            raw_transcript: "خراپ".into(),
            alignment_json: Some(r#"[{"word":"خراپ","start":0.0,"end":0.5,"confidence":0.9}]"#.into()),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("clobbered", true).unwrap();

        let out = tempfile::TempDir::new().unwrap();
        let result = export_finetune_pack(&db, out.path(), None).unwrap();
        assert_eq!(result.emitted, 1, "only the offset-intact chunk ships");
        assert_eq!(result.skipped, 1, "the clobbered chunk is skipped and counted");
        assert!(!out.path().join("clips/clobbered.wav").exists(), "no whole-recording clip is ever written");
        let manifest = std::fs::read_to_string(out.path().join("finetune_manifest.jsonl")).unwrap();
        assert!(!manifest.contains("خراپ"), "the clobbered row's sentence never enters the manifest");
        // The pack now carries integrity sums over every file (clips included).
        let sums = std::fs::read_to_string(out.path().join("SHA256SUMS")).unwrap();
        assert!(sums.contains("finetune_manifest.jsonl"));
        assert!(sums.contains("clips/intact.wav"), "clip bytes are integrity-pinned: {sums}");
    }

    #[test]
    fn finetune_pack_refuses_mark_bad_and_severe_audio_rows() {
        // B1 (true-10 audit blocker): verified=true alone must NOT admit a row. Mark-bad sets
        // verified=true merely to leave the review queue, so without the rubric guard a human-
        // REJECTED clip's bad draft would ship as a training label; a severe-clipping clip is
        // equally unfit. Both must be refused and counted, never emitted.
        let db = open_mem_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let wav = tmp.path().join("clips.wav");
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&wav, spec).unwrap();
            for i in 0..16000i32 {
                w.write_sample(((i % 100) - 50) as i16).unwrap();
            }
            w.finalize().unwrap();
        }
        let wav_str = wav.to_string_lossy().to_string();

        // GOOD: a clean human-verified segment — the only row allowed through.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "good".into(),
            audio_path: wav_str.clone(),
            raw_transcript: "ڕاستکراوە".into(),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("good", true).unwrap();

        // MARK-BAD: human rejected it; the review flow still sets verified=true to clear the queue.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "markbad".into(),
            audio_path: wav_str.clone(),
            raw_transcript: "خراپە".into(),
            alignment_json: Some(r#"{"start_ms":0,"end_ms":400}"#.into()),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("markbad", true).unwrap();
        db.connection().execute("UPDATE speech_segments SET human_decision='reject' WHERE id='markbad'", []).unwrap();

        // SEVERE AUDIO: verified but the clip is badly clipped — rubric grade REJECT.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "clipped".into(),
            audio_path: wav_str.clone(),
            raw_transcript: "قسەیەک".into(),
            alignment_json: Some(r#"{"start_ms":400,"end_ms":800}"#.into()),
            clipping_ratio: Some(0.5),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("clipped", true).unwrap();

        let out = tempfile::TempDir::new().unwrap();
        let result = export_finetune_pack(&db, out.path(), None).unwrap();
        assert_eq!(result.total_verified, 3);
        assert_eq!(result.excluded_holdout, 0);
        assert_eq!(result.excluded_not_training_ready, 2, "mark-bad + severe-clipping both refused");
        assert_eq!(result.emitted, 1, "only the rubric-clean row ships");

        let manifest = std::fs::read_to_string(out.path().join("finetune_manifest.jsonl")).unwrap();
        assert!(manifest.contains("ڕاستکراوە"), "the clean row's text ships");
        assert!(!manifest.contains("خراپە"), "the rejected draft NEVER becomes a training label");
        assert!(!manifest.contains("قسەیەک"), "the severe-clipping row never ships");
        assert!(!out.path().join("clips/markbad.wav").exists(), "no clip written for a refused row");
        assert!(!out.path().join("clips/clipped.wav").exists(), "no clip written for a refused row");
    }

    #[test]
    fn finetune_pack_ships_canonical_orthography_and_dedups_variants() {
        // True-10 audit: human-typed and ASR text mix Sorani codepoint variants (ك/ک, ي/ی). The
        // shipped sentence must be canonicalized, and two rows on the same audio span differing
        // ONLY by codepoint variant must dedup to ONE training row.
        let db = open_mem_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let wav = tmp.path().join("v.wav");
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&wav, spec).unwrap();
            for i in 0..16000i32 {
                w.write_sample(((i % 100) - 50) as i16).unwrap();
            }
            w.finalize().unwrap();
        }
        let wav_str = wav.to_string_lossy().to_string();
        // Same audio span, same word — one written with Arabic forms, one with Kurdish forms.
        for (id, text) in [("arabic", "كوردي"), ("kurdish", "کوردی")] {
            db.insert_segment(&crate::db::SpeechSegment {
                id: id.to_string(),
                audio_path: wav_str.clone(),
                raw_transcript: text.to_string(),
                ..Default::default()
            })
            .unwrap();
            db.update_verified(id, true).unwrap();
        }

        let out = tempfile::TempDir::new().unwrap();
        let result = export_finetune_pack(&db, out.path(), None).unwrap();
        assert_eq!(result.emitted, 1, "codepoint-variant duplicates collapse to one training row");
        assert_eq!(result.skipped, 1, "the variant duplicate is skipped as a dup");

        let manifest = std::fs::read_to_string(out.path().join("finetune_manifest.jsonl")).unwrap();
        assert!(manifest.contains("کوردی"), "the shipped sentence uses the canonical Kurdish forms");
        assert!(!manifest.contains("كوردي"), "no Arabic codepoint variants ship: {manifest}");
    }

    #[test]
    fn gold_promotion_refuses_files_with_rejected_chunks() {
        // True-10 audit: a rejected chunk's draft is known-wrong text, but its speech is still in
        // the holdout WAV — including the draft poisons the reference one way, omitting the chunk
        // poisons it the other (spurious insertions on every eval). No correct whole-file reference
        // exists, so promotion must refuse the file until the chunk is corrected.
        let db = open_mem_db();
        for (id, decision, fix) in [("c1", "edit", "alpha"), ("c2", "reject", "WRONG DRAFT"), ("c3", "accept", "gamma")]
        {
            db.insert_segment(&crate::db::SpeechSegment {
                id: id.to_string(),
                audio_path: "/clips/mixed.wav".to_string(),
                raw_transcript: "draft".to_string(),
                ..Default::default()
            })
            .unwrap();
            db.connection()
                .execute(
                    "UPDATE speech_segments SET human_decision=?2, verdict_transcript=?3 WHERE id=?1",
                    params![id, decision, fix],
                )
                .unwrap();
        }

        let err = create_gold_from_verified_file(&db, "/clips/mixed.wav").unwrap_err();
        assert!(err.to_string().contains("rejected chunk"), "refusal explains the reason: {err}");
        assert!(list_gold_segments(&db).unwrap().is_empty(), "no gold row was created for the poisoned file");

        // The bulk promoter skips the poisoned file (warn) but must not fail the batch; a clean
        // file still promotes.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "ok1".to_string(),
            audio_path: "/clips/clean.wav".to_string(),
            raw_transcript: "draft".to_string(),
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET human_decision='accept', verdict_transcript='بەڵێ' WHERE id='ok1'", [])
            .unwrap();
        let created = import_verified_segments_as_gold(&db).unwrap();
        assert_eq!(created, 1, "only the clean file promotes");
        let gold = list_gold_segments(&db).unwrap();
        assert_eq!(gold.len(), 1);
        assert_eq!(gold[0].audio_path, "/clips/clean.wav");
        assert!(!gold[0].reference.contains("WRONG DRAFT"), "the rejected draft never reaches a gold reference");
    }

    #[test]
    fn gold_promotion_refuses_partially_reviewed_files() {
        // True-10 audit 2026-07-09: the same hazard as the reject guard, from the other side — an
        // UNREVIEWED chunk's speech is in the holdout WAV but its text is missing from the
        // concatenated reference, so every future engine benchmark scores those spans as spurious
        // insertions, permanently inflating WER/CER on the promotion yardstick. Refuse until every
        // chunk carries a decision; the bulk promoter skips the incomplete file without failing.
        let db = open_mem_db();
        for (id, decision) in [("p1", Some("accept")), ("p2", None), ("p3", Some("edit"))] {
            db.insert_segment(&crate::db::SpeechSegment {
                id: id.to_string(),
                audio_path: "/clips/partial.wav".to_string(),
                raw_transcript: "draft".to_string(),
                ..Default::default()
            })
            .unwrap();
            if let Some(d) = decision {
                db.connection()
                    .execute(
                        "UPDATE speech_segments SET human_decision=?2, verdict_transcript='fix' WHERE id=?1",
                        params![id, d],
                    )
                    .unwrap();
            }
        }

        let err = create_gold_from_verified_file(&db, "/clips/partial.wav").unwrap_err();
        assert!(err.to_string().contains("unreviewed chunk"), "refusal explains the reason: {err}");
        assert!(list_gold_segments(&db).unwrap().is_empty(), "no incomplete gold reference is minted");

        // Reviewing the last chunk unblocks promotion.
        db.connection()
            .execute("UPDATE speech_segments SET human_decision='accept', verdict_transcript='mid' WHERE id='p2'", [])
            .unwrap();
        let created = create_gold_from_verified_file(&db, "/clips/partial.wav").unwrap();
        assert_eq!(created, 1, "a fully-reviewed file promotes");
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
