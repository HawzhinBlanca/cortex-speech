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
            // A placeholder is NON-empty, so the !is_empty() push below would join it verbatim into the
            // PERMANENT holdout reference (accepting a chunk BEFORE its ASR finishes leaves verdict/annotated
            // empty, so the COALESCE falls to raw_transcript='[Pending WSL 7B ASR]'). Its speech is in the
            // WAV but it has no real text — the same hazard the reject/unreviewed guards refuse. Refuse the
            // file until the chunk is transcribed/corrected, rather than poisoning every future benchmark.
            if crate::quality::is_placeholder_transcript(trimmed) {
                return Err(crate::error::AppError::Validation(format!(
                    "'{audio_path}' has a reviewed chunk whose only reference text is a placeholder \
                     ('{trimmed}') — its speech is in the holdout WAV but it has no real transcript (ASR \
                     unfinished, or accepted before transcription); transcribe/correct it before marking it gold"
                )));
            }
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
    pub excluded_unexportable: usize,
    /// Verified segments the training-grade rubric refused (the B1 guard): human-rejected (mark-bad
    /// carries verified=true only to leave the review queue), severe audio issues, placeholder text,
    /// or any other non-training-ready grade. Without this filter a REJECTED clip's bad draft would
    /// ship as a training label.
    pub excluded_not_training_ready: usize,
    pub emitted: usize,
    /// Skipped for an empty transcript, a duplicate, or undecodable audio.
    pub skipped: usize,
    /// Emitted rows that carry NO live human decision — batch-verified, or a verdict the reviewer
    /// undid (undo clears the decision but leaves `verified = 1`). The rubric grades these SILVER,
    /// so they are machine labels, correctly marked; this counter makes the quantity visible.
    pub emitted_without_human_decision: usize,
    /// Content id of this selection (the manifest hash). A training run cites this; `dataset_runs`
    /// holds the sealed record, and the same rows always produce the same id.
    pub snapshot_id: String,
    /// False when an identical snapshot was already sealed — the export was reproducible, not new.
    pub newly_sealed: bool,
}

/// One JSONL row in the fine-tune pack — the trainer's schema.
///
/// The first three fields are what the trainer consumes. Everything after them is PROVENANCE, added
/// 2026-08-17 after an external audit found the pack emitted "essentially audio, text and duration":
/// a trainer could not tell which human decision produced a label, which recording it came from, or
/// whether the audio was a neural separator's output rather than an original recording — and the
/// pack carried no split at all, so nothing stopped a fine-tune from testing on its own training
/// voices. Provenance that only lives in the app is provenance the training run cannot check.
#[derive(Serialize)]
struct FinetuneRow<'a> {
    audio_path: String,
    sentence: &'a str,
    duration_seconds: f64,
    /// The library row this label came from — the join key back to every decision on it.
    segment_id: &'a str,
    /// Source recording (file name only; the full path is machine-specific and not the trainer's).
    source_recording: &'a str,
    /// Leakage-safe split: every clip from one recording lands in the same split, so a fine-tune
    /// cannot validate on the voice it trained on. Computed by the same `assign_splits` the
    /// HuggingFace export uses, over exactly the rows this pack emits.
    split: &'static str,
    /// Which human action produced this label: `accept`, `edit`, or absent when the row was
    /// verified without a recorded decision (batch-verify, or pre-v43 history).
    decision: Option<&'a str>,
    /// The row revision that decision landed on. "Exactly once, latest" is enforced by the library
    /// holding one row per clip; this pins WHICH state of it was trained on, so a later edit is
    /// visibly a different snapshot rather than a silent change.
    decision_revision: i64,
    /// GOLD (human-decided) or SILVER (machine, training-ready) — the rubric's own verdict, so the
    /// trainer can weight or filter without re-deriving it.
    grade: &'a str,
    /// True when the SOURCE audio was processed before import (separated from music, non-speech cut,
    /// re-concatenated). Declared by migration v54; false means unclaimed, never "verified original".
    audio_processed: bool,
}

/// M5.1 / P5.1: export a fine-tune training pack from the segments the training-grade rubric
/// certifies — the trainer's manifest schema (`{audio_path, sentence, duration_seconds}`) + a 16 kHz
/// WAV clip per row under `clips/`. Two guards, both load-bearing:
///
/// 1. HOLDOUT leak guard: gold holdout clips are excluded (path AND content hash, via
///    `exclude_unexportable_segments`) so training never contaminates the eval set the promotion gate
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
    let kept = crate::export::exclude_unexportable_segments(db, verified)?;
    let excluded_unexportable = total_verified - kept.len();

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

    // LEAKAGE-SAFE SPLIT over exactly the rows this pack will emit (P2, 2026-08-17). The pack used
    // to ship unsplit, so nothing stopped a fine-tune from validating on a voice it had trained on —
    // and with 94.7 % of today's labels from a single recording that is not a theoretical risk.
    //
    // GROUPED BY AUDIO CONTENT, not by file name. `assign_splits` keys on the path's BASENAME, which
    // is the right unit for the HuggingFace export's flat library but is wrong twice over here:
    //
    //  * the SAME recording re-encoded under a different name is two groups, so it straddles splits.
    //    This library contains exactly that — `check_dataset_duplicates.py` documents
    //    `A1-0032_PODCAST-001.mp4` as a third encode of the Lamofull material, and Lamofull is 94.7 %
    //    of the labeled duration, so once it fills `train` every other group is forced into
    //    validation/test. Adversarial verification 2026-08-17 measured that exact split.
    //  * the audiobook corpus names every chapter `01.wav`, `02.wav`, … inside its own book folder,
    //    so chapter 4 of three different books collides into ONE group.
    //
    // `audio_content_hash` (v51) is the identity that survives a re-encode; the full path is the
    // fallback when a row predates the backfill, and it is at least never a cross-book collision.
    const SPLIT_SEED: u64 = 20_260_817;
    // path -> content hash, for every recording whose identity has been computed (v51 + the
    // backfill). A recording with no hash keeps its full path, which is at least never a cross-book
    // basename collision.
    let content_identity: std::collections::HashMap<String, String> = db
        .load_audio_identities()?
        .into_iter()
        .filter_map(|identity| identity.content.filter(|hash| !hash.is_empty()).map(|hash| (identity.audio_path, hash)))
        .collect();
    let training_ready: Vec<crate::db::SpeechSegment> = graded
        .iter()
        .filter(|(_, r)| r.training_ready)
        .map(|(s, _)| {
            let mut grouping_copy = (*s).clone();
            // This copy exists ONLY to be grouped — `assign_splits` returns (segment_id, split), so
            // rewriting the path here cannot reach the manifest.
            grouping_copy.audio_path = match content_identity.get(&s.audio_path) {
                Some(hash) => format!("content-{hash}"),
                None => s.audio_path.clone(),
            };
            grouping_copy
        })
        .collect();
    let splits: std::collections::HashMap<String, &'static str> =
        crate::export::assign_splits(&training_ready, 0.8, 0.1, 0.1, SPLIT_SEED, true).into_iter().collect();

    // Which source recordings were PROCESSED before import (migration v54) — one query, then a
    // lookup per row, so the trainer can see that a clip's audio is a separator's output rather
    // than an original recording.
    let processed_sources = db.source_audio_provenance_map()?;

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut emitted = 0usize;
    let mut skipped = 0usize;
    let mut emitted_without_human_decision = 0usize;

    // PASS 1 — decide WHICH rows this pack emits, in manifest order, WITHOUT touching audio. Text
    // only, and it settles dedup before anything is decoded, so no clip is ever decoded to be thrown
    // away.
    let mut planned: Vec<(&crate::db::SpeechSegment, &crate::quality::TrainingGradeReport, String)> = Vec::new();
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
        planned.push((seg, report, sentence));
    }

    // PASS 2 — decode, GROUPED BY SOURCE RECORDING (2026-08-18). One walk per recording, not one per
    // clip: see `commands::decode_finetuned_clips_16k` for the measurement that forced this. The
    // clips written here are byte-identical to the per-clip decoder's, so the manifest hash — the
    // snapshot id — is unchanged by the grouping.
    let mut by_source: std::collections::BTreeMap<&str, Vec<usize>> = std::collections::BTreeMap::new();
    for (index, (seg, _, _)) in planned.iter().enumerate() {
        by_source.entry(seg.audio_path.as_str()).or_default().push(index);
    }
    let mut clip_durations: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for (source, rows) in &by_source {
        let single_segment_source = sibling_counts.get(*source).copied().unwrap_or(1) <= 1;
        let mut spans: Vec<crate::commands::ClipSpan> = Vec::with_capacity(rows.len());
        for &index in rows {
            let seg = planned[index].0;
            let alignment = seg.alignment_json.as_deref().map(str::trim).filter(|a| !a.is_empty());
            let window = match alignment {
                // Truly absent alignment: a single-segment import — the whole file is the clip.
                None => Some((0, i64::MAX)),
                Some(json) => match crate::chunking::SegmentSourceMeta::from_alignment_json(json) {
                    Some(meta) => {
                        let start = meta.source_start_ms.max(0);
                        Some((start, meta.source_end_ms.max(start)))
                    }
                    // ALIGNMENT PRESENT BUT OFFSET-LESS: whole-file is correct for a legitimately
                    // aligned single-segment file and CORRUPTING for a clobbered chunk of a
                    // multi-segment source (it would ship the entire recording as one "clip").
                    None if single_segment_source => Some((0, i64::MAX)),
                    None => None,
                },
            };
            match window {
                Some((start_ms, end_ms)) => {
                    spans.push(crate::commands::ClipSpan { segment_id: seg.id.clone(), start_ms, end_ms })
                }
                None => tracing::warn!(
                    "finetune pack: skipping {} (alignment_json carries no source offsets on a \
                     multi-segment source — refusing the whole-file fallback; re-import the file \
                     through the current build to regenerate its slice offsets)",
                    seg.id
                ),
            }
        }

        // A failed WRITE is fatal to the export (a full disk must not read as "some clips skipped"),
        // while a failed DECODE skips that source's rows and says so.
        let mut write_failure: Option<crate::error::AppError> = None;
        let decoded = crate::commands::decode_finetuned_clips_16k(source, &spans, |segment_id, pcm| {
            if pcm.is_empty() {
                tracing::warn!("finetune pack: skipping {segment_id} (clip undecodable)");
                return Ok(());
            }
            let duration = pcm.len() as f64 / crate::audio::TARGET_SAMPLE_RATE as f64;
            match write_wav_16k_mono(
                &clips_dir.join(format!("{segment_id}.wav")),
                &pcm,
                crate::audio::TARGET_SAMPLE_RATE,
            ) {
                Ok(()) => {
                    clip_durations.insert(segment_id.to_string(), duration);
                    Ok(())
                }
                Err(error) => {
                    let message = error.to_string();
                    write_failure = Some(error);
                    Err(message)
                }
            }
        });
        if let Some(error) = write_failure {
            return Err(error);
        }
        if let Err(reason) = decoded {
            tracing::warn!("finetune pack: source {source} failed to decode ({reason}) — its rows are skipped");
        }
    }

    // PASS 3 — write the manifest in PASS 1's order. A planned row with no clip on disk was skipped
    // above (undecodable, offset-less, or a dead source) and is counted here exactly once.
    for (seg, report, sentence) in planned.iter() {
        let Some(duration_seconds) = clip_durations.get(seg.id.as_str()).copied() else {
            skipped += 1;
            continue;
        };
        // Counted, never silent: a row can be verified WITHOUT a live human decision — batch-verify
        // never records one, and `clear_human_decision` (undo) clears the decision while leaving
        // `verified = 1`. The rubric already grades those SILVER rather than GOLD, so nothing is
        // mislabeled; what was missing is the NUMBER. An undone verdict quietly becoming machine
        // training data is exactly the kind of thing that must appear in the pack's own record
        // instead of being discovered later by an auditor.
        if seg.human_decision.is_none() {
            emitted_without_human_decision += 1;
        }
        let row = FinetuneRow {
            audio_path: format!("clips/{}.wav", seg.id),
            sentence,
            duration_seconds,
            segment_id: &seg.id,
            source_recording: seg.audio_path.rsplit(['/', '\\']).next().unwrap_or(&seg.audio_path),
            // A row with no split assignment would be a silent hole in the leakage guarantee, so
            // fall back to `train` — the split that can never leak INTO evaluation.
            split: splits.get(&seg.id).copied().unwrap_or("train"),
            decision: seg.human_decision.as_deref(),
            decision_revision: db.segment_review_revision(&seg.id)?.unwrap_or(0),
            grade: report.grade.as_str(),
            audio_processed: processed_sources.contains_key(&seg.audio_path),
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
    // The SNAPSHOT identity is the manifest hash: same rows in the same order => same snapshot.
    // Everything below that varies per run (a timestamp, the app SHA) is recorded ALONGSIDE it, never
    // inside the identity, so "did the data change?" is answerable without diffing two packs.
    let snapshot_id = manifest_sha256.clone();
    let selection = serde_json::json!({
        "schema": 2,
        "snapshotId": snapshot_id,
        "manifestSha256": manifest_sha256,
        "emitted": emitted,
        "skipped": skipped,
        "excludedUnexportable": excluded_unexportable,
        "excludedNotTrainingReady": excluded_not_training_ready,
        "emittedWithoutHumanDecision": emitted_without_human_decision,
        "totalVerified": total_verified,
        "splitSeed": SPLIT_SEED,
        // Says what the split ACTUALLY guarantees. The first wording claimed "speaker disjoint",
        // which is not true on this library: the only automatic speaker labeler emits per-recording
        // `SPEAKER_00…`, which `assign_splits` deliberately scopes per recording, so the
        // speaker-union step links nothing. Sealing a guarantee the data cannot support is the exact
        // failure this provenance record exists to prevent.
        "splitPolicy": "assign_splits 80/10/10 over AUDIO-CONTENT groups (audio_content_hash, else full path) — every clip sharing audio content lands in one split, so a re-encode cannot straddle. NOT speaker-disjoint: diarizer labels are per-recording indices, not identities, so two recordings of one person may land in different splits.",
        "rowSchema": "audio_path, sentence, duration_seconds, segment_id, source_recording, split, decision, decision_revision, grade, audio_processed",
        "selectionPolicy": "training_ready (GOLD/SILVER) via quality::training_grade_for_segment; holdout-excluded; canonical Sorani orthography; variant-aware dedup; one row per (recording, span, text) at its LATEST review_revision",
    });
    // Seal it. INSERT OR IGNORE: re-exporting identical data is a no-op and an existing snapshot is
    // never rewritten, so a training run can cite `snapshotId` and trust it did not move.
    let newly_sealed = db.seal_dataset_snapshot(&snapshot_id, "finetune-pack", &selection.to_string())?;
    if newly_sealed {
        tracing::info!("sealed dataset snapshot {snapshot_id} ({emitted} rows)");
    } else {
        tracing::info!("dataset snapshot {snapshot_id} already sealed — identical selection, no new record");
    }

    let mut provenance = selection;
    provenance["createdAt"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    provenance["appGitSha"] = serde_json::json!(crate::GIT_SHA);
    provenance["newlySealed"] = serde_json::json!(newly_sealed);
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
        excluded_unexportable,
        excluded_not_training_ready,
        emitted,
        skipped,
        emitted_without_human_decision,
        snapshot_id,
        newly_sealed,
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

    // Utterance bootstrap over the SAME pairs the micro rates are built from, so the interval
    // describes the number actually reported rather than a differently-filtered population.
    let word_pairs: Vec<(usize, usize)> =
        seg_details.iter().filter(|(_, w, _)| w.ref_len > 0).map(|(_, w, _)| (w.distance, w.ref_len)).collect();
    let char_pairs: Vec<(usize, usize)> =
        seg_details.iter().filter(|(_, _, c)| c.ref_len > 0).map(|(_, _, c)| (c.distance, c.ref_len)).collect();
    let wer_ci = bootstrap_micro_ci(&word_pairs, EVAL_BOOTSTRAP_SAMPLES, EVAL_BOOTSTRAP_SEED);
    let cer_ci = bootstrap_micro_ci(&char_pairs, EVAL_BOOTSTRAP_SAMPLES, EVAL_BOOTSTRAP_SEED);

    let mut meta = serde_json::json!({
        "micro_wer": micro_wer,
        "micro_cer": micro_cer,
        "macro_wer": macro_wer,
        "macro_cer": macro_cer,
        "bootstrap_samples": EVAL_BOOTSTRAP_SAMPLES,
        "bootstrap_seed": EVAL_BOOTSTRAP_SEED,
    });
    // Absent rather than null-filled when there is nothing to resample: a CI that reads [0, 0] on an
    // empty run would be a fabricated interval, which is worse than no interval.
    if let (Some(m), Some((lo, hi))) = (meta.as_object_mut(), wer_ci) {
        m.insert("micro_wer_ci_low".into(), lo.into());
        m.insert("micro_wer_ci_high".into(), hi.into());
    }
    if let (Some(m), Some((lo, hi))) = (meta.as_object_mut(), cer_ci) {
        m.insert("micro_cer_ci_low".into(), lo.into());
        m.insert("micro_cer_ci_high".into(), hi.into());
    }
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
/// Seeded utterance-bootstrap 95% CI on a MICRO (ratio-of-sums) rate.
///
/// P0 #1 of the 2026-08-03 deep audit asks a published accuracy record to carry a confidence interval,
/// and this path reported micro/macro only — so the first real gold run (`omniasr-ctc-300m`, N=348,
/// CER 12.58%) landed as a bare point estimate. A point estimate at N=348 without a CI invites exactly
/// the false-precision comparison the same audit objects to.
///
/// Resamples whole UTTERANCES with replacement (Bisani & Ney): the unit of independence is the clip,
/// not the character, so a per-character bootstrap would badly understate the interval. Each replicate
/// recomputes `sum(distance) / sum(ref_len)` over the resampled clips, matching how the headline micro
/// rate is computed — including its `.min(1.0)` clamp.
///
/// Deterministic: same xorshift64 generator and seeding discipline as `compute_label_quality_lift`, so
/// a re-run of the same data reproduces the interval exactly. Zero-reference clips are already excluded
/// by the caller (they contribute to neither numerator nor denominator), so they cannot enter here.
fn bootstrap_micro_ci(pairs: &[(usize, usize)], samples: usize, seed: u64) -> Option<(f64, f64)> {
    let n = pairs.len();
    if n == 0 || samples == 0 {
        return None;
    }
    let mut state = seed | 1; // xorshift64 state must be non-zero
    let mut rates: Vec<f64> = Vec::with_capacity(samples);
    for _ in 0..samples {
        let (mut dist, mut refl) = (0usize, 0usize);
        for _ in 0..n {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let (d, r) = pairs[(state % n as u64) as usize];
            dist += d;
            refl += r;
        }
        if refl > 0 {
            rates.push((dist as f64 / refl as f64).min(1.0));
        }
    }
    if rates.is_empty() {
        return None;
    }
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let at = |q: f64| -> f64 {
        let idx = ((q * (rates.len() as f64 - 1.0)).round() as usize).min(rates.len() - 1);
        rates[idx]
    };
    Some((at(0.025), at(0.975)))
}

/// Bootstrap replicates for the eval CI. 3000 matches `scripts/scorecard_finetuned.py`, so an interval
/// from this path and one from the published scorecard are directly comparable.
const EVAL_BOOTSTRAP_SAMPLES: usize = 3000;
/// Fixed so the interval is reproducible from the run id alone; same value the published scorecard used.
const EVAL_BOOTSTRAP_SEED: u64 = 42;

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
    /// Scored rows whose reference is CHARACTER-IDENTICAL to the jury hypothesis, so their jury
    /// distance is zero no matter what the jury did.
    ///
    /// This is the honest limit of the whole metric, and it is structural rather than situational.
    ///
    /// CORRECTED 2026-08-04. The first version of this doc blamed ACCEPTING a clip ("an accept copies
    /// the jury's output into the reference"). That was wrong. `corrections.rs` is the single source of
    /// truth on the column's meaning: *"verdict_transcript ... is the human's ANSWER (the
    /// reference/target the evidence is scored AGAINST), never the model draft"*. `load_lift_triples`
    /// passes that column as the JURY hypothesis, so this metric compares the human's answer with the
    /// human's answer — for every decided row, regardless of what the reviewer did.
    ///
    /// Measured on the owner's library: 39 of 39 scored rows self-referential, INCLUDING 34 of the 35
    /// clips the reviewer edited. Editing cannot make it measurable; the advice that it would was wrong.
    ///
    /// The post-jury text is not persisted anywhere — `segment_hypotheses` stores per-ENGINE drafts, not
    /// the jury's consensus — so a raw-vs-post-jury lift cannot be computed from what this schema keeps.
    /// Callers MUST NOT present the jury figure when this equals `n`, which on real data is always.
    pub self_referential_n: usize,
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
            // A reference that normalizes to EMPTY (ref_len == 0) has no unit to score against — its raw/jury
            // char distances are pure insertions (char_edit_distance returns them as an "honest insertion
            // count for micro aggregation"). Exclude such a row from BOTH numerator and denominator, matching
            // every other micro site (run_gold_eval, load_eval_run_and_recompute, scorecard, significance).
            // Guarding only the grand-total ref_chars==0 let a single empty-ref row's insertions over a zero
            // denominator peg both engines' micro-CER (and the bootstrap CI) to a fabricated 1.0.
            if rl == 0 {
                continue;
            }
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

    // Counted over the SCORED rows only (`rl > 0`), matching `micro`'s own exclusion of empty
    // references — a row that contributes to neither numerator nor denominator cannot be evidence for
    // or against the jury either. A zero jury distance here means the reference and the jury verdict
    // are the same characters, which is what an "accept" produces.
    let self_referential_n = per.iter().filter(|&&(rl, _, jd)| rl > 0 && jd == 0).count();

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
        self_referential_n,
    }
}

/// Load `(reference, raw, jury)` triples for the label-quality lift from human-verified segments:
/// the human's correction (`annotated_transcript`) is the ground-truth reference, `raw_transcript`
/// is the raw ASR hypothesis, and `verdict_transcript` is the post-jury label. Only segments that
/// carry all three (non-empty) are included — a real measured lift needs ground truth + both hyps.
/// NOTE (2026-08-04): the third column, `verdict_transcript`, is documented in `corrections.rs` as
/// "the human's ANSWER ... never the model draft". It is therefore NOT an independent jury hypothesis,
/// and the lift computed from these triples compares the human's answer with itself on every decided
/// row. The metric is retained (and the UI withholds it) rather than silently deleted, because the
/// honest fix is to persist the jury's verdict separately from the human's — a schema decision, not a
/// calculation change.
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
        // v48: `jury_transcript`, NOT `verdict_transcript`. The latter is whichever verdict is current
        // and a human decision overwrites it with the reviewer's own correction — so reading it here
        // compared the human's answer with the human's answer and returned zero on every decided row,
        // forever, whatever the reviewer did. `jury_transcript` is written only by
        // `write_segment_verdict`, so it keeps the MACHINE's text and gives this metric a genuinely
        // independent side. Rows where the jury never committed a verdict (it escalated instead) are
        // excluded rather than defaulted: no machine text means nothing to score, not a score of zero.
        "SELECT annotated_transcript, raw_transcript, jury_transcript \
         FROM speech_segments \
         WHERE human_decision IS NOT NULL AND TRIM(human_decision) <> '' \
           AND human_decision NOT IN ('reject', 'human_reject') \
           AND COALESCE(verdict, '') <> 'human_reject' \
           AND annotated_transcript IS NOT NULL AND TRIM(annotated_transcript) <> '' \
           AND jury_transcript IS NOT NULL AND TRIM(jury_transcript) <> '' \
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
        // ...and BOTH rows are self-referential, which is the honest reading of the three lines
        // above. This test was written as "the jury restores the reference", but a reference that
        // equals the jury hypothesis is what an ACCEPT produces, and it forces jury CER to 0
        // whatever the jury did. Asserting it here so the fixture cannot be mistaken for evidence
        // that a zero jury CER means a good jury.
        assert_eq!(lift.self_referential_n, 2, "reference == jury on both rows");
    }

    #[test]
    fn label_quality_lift_counts_rows_that_scored_the_jury_against_itself() {
        // The shape measured on the owner's library on 2026-08-03: every scored row had the human's
        // confirmed transcript character-identical to the jury verdict, because accepting a clip
        // copies the jury's output into the reference. Jury CER is then 0 by construction, and
        // `cer_lift = raw - 0` is the raw ASR error wearing the jury's name.
        let accepted = ("hello world".to_string(), "hello word".to_string(), "hello world".to_string());
        // An EDIT is the only independent evidence: the human wrote something the jury did not.
        let edited = ("good morning".to_string(), "gud mrning".to_string(), "good mrning".to_string());

        let all_accepts = compute_label_quality_lift(&[accepted.clone(), accepted.clone()], 50, 3);
        assert_eq!(all_accepts.n, 2);
        assert_eq!(all_accepts.self_referential_n, all_accepts.n, "nothing here measures the jury");
        assert!(all_accepts.jury_micro_cer.abs() < 1e-9, "forced to zero, not measured");

        let mixed = compute_label_quality_lift(&[accepted, edited], 50, 3);
        assert_eq!(mixed.n, 2);
        assert_eq!(mixed.self_referential_n, 1, "only the accepted row is self-referential");
        assert!(mixed.jury_micro_cer > 0.0, "the edited row lets the jury actually be wrong");

        // Empty references are excluded from scoring, so they cannot be counted as evidence either.
        let empty_ref = (String::new(), "x".to_string(), String::new());
        let with_empty = compute_label_quality_lift(&[empty_ref], 0, 1);
        assert_eq!(with_empty.n, 1, "the row is still reported in n");
        assert_eq!(with_empty.self_referential_n, 0, "an unscored row is not self-referential evidence");
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

    #[test]
    fn label_quality_lift_excludes_an_empty_normalizing_reference_from_micro_cer() {
        // A reference that normalizes to EMPTY (ref_len == 0) must be excluded from BOTH the numerator and
        // denominator of the micro-CER — otherwise its raw/jury insertions over a zero denominator peg both
        // engines' micro-CER (and the CI) to a fabricated 1.0, even though the only scoreable reference
        // matched perfectly. Same per-segment ref_len>0 guard used at every other micro-aggregation site.
        let diacritics_only = "\u{064B}\u{064C}\u{064E}"; // fathatan/dammatan/fatha -> normalizes to ""
        assert_eq!(
            char_edit_distance(diacritics_only, "x").ref_len,
            0,
            "fixture precondition: the diacritics-only reference must normalize to an empty metric reference"
        );
        let good = ("hello".to_string(), "hello".to_string(), "hello".to_string()); // CER 0 for both
        let empty_ref = (diacritics_only.to_string(), "x y z".to_string(), "a b c".to_string());
        let lift = compute_label_quality_lift(&[good, empty_ref], 0, 1);

        assert!(
            lift.raw_micro_cer < 1e-9,
            "empty-ref insertions must not inflate raw micro-CER: {}",
            lift.raw_micro_cer
        );
        assert!(
            lift.jury_micro_cer < 1e-9,
            "empty-ref insertions must not inflate jury micro-CER: {}",
            lift.jury_micro_cer
        );
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
            // verdict / jury_transcript / human_decision are jury/decision columns that insert_segment
            // omits by design, so set them here. v48: the post-jury text is `jury_transcript` — the
            // MACHINE's own output, written only by write_segment_verdict. `verdict_transcript` is set
            // too, to the human's answer, exactly as the real decision path leaves it: this fixture then
            // reproduces the shape that made the metric self-referential, and the query must be reading
            // the jury column to survive it.
            db.connection()
                .execute(
                    "UPDATE speech_segments \
                     SET human_decision=?2, verdict=?3, jury_transcript='خاوی جوری', verdict_transcript='دەقی ڕاست' \
                     WHERE id=?1",
                    params![id, decision, verdict],
                )
                .unwrap();
        }
        let triples = load_lift_triples(&db).unwrap();
        assert_eq!(triples.len(), 1, "only the accepted clip is a lift triple; the rejected one is excluded");
        assert_eq!(triples[0].0, "دەقی ڕاست", "the surviving triple is the accepted clip's human reference");
        // The whole point of v48: the jury side must be the MACHINE's text, not the human's answer.
        // Reading `verdict_transcript` here would make this equal `triples[0].0` and the metric would
        // report a perfect jury on every decided row for ever.
        assert_eq!(triples[0].2, "خاوی جوری", "the jury side comes from jury_transcript, not the human's answer");
        assert_ne!(triples[0].0, triples[0].2, "reference and jury hypothesis must be independent texts");
    }

    #[test]
    fn a_human_edit_cannot_overwrite_the_machines_recorded_verdict() {
        // The defect v48 closes, end to end through the real write paths. `write_segment_verdict` records
        // the machine's text; the reviewer then edits the clip. Before v48 the edit replaced the only
        // copy of the machine's answer, which is why 34 of the owner's 35 edited clips scored the jury
        // against itself.
        let db = open_mem_db();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "s1".to_string(),
            audio_path: "/clips/s1.wav".to_string(),
            raw_transcript: "خاو".to_string(),
            ..Default::default()
        })
        .unwrap();

        db.write_segment_verdict("s1", "jury_edit", Some("دەقی جوری"), None, None, None, false).unwrap();
        db.record_human_decision("s1", "edit", Some("دەقی مرۆڤ"), None).unwrap();

        let (jury, human): (Option<String>, Option<String>) = db
            .connection()
            .query_row("SELECT jury_transcript, verdict_transcript FROM speech_segments WHERE id='s1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(human.as_deref(), Some("دەقی مرۆڤ"), "the human's edit is the current verdict text");
        assert_eq!(jury.as_deref(), Some("دەقی جوری"), "the machine's verdict SURVIVED the human's edit");
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
    fn create_gold_refuses_a_chunk_whose_only_text_is_a_placeholder() {
        // Round-25 hunt: accepting a chunk BEFORE its ASR finishes leaves verdict/annotated empty, so the
        // gold reference COALESCEs to raw_transcript='[Pending WSL 7B ASR]' — a placeholder that would be
        // joined verbatim into the PERMANENT holdout reference, corrupting every future engine benchmark.
        // It must be refused, like the reject/unreviewed guards.
        let db = open_mem_db();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "p1".to_string(),
            audio_path: "/clips/pending.wav".to_string(),
            raw_transcript: "[Pending WSL 7B ASR]".to_string(),
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET human_decision='accept', verdict_transcript='', \
                 annotated_transcript='' WHERE id='p1'",
                [],
            )
            .unwrap();
        let err = create_gold_from_verified_file(&db, "/clips/pending.wav").unwrap_err();
        assert!(
            format!("{err}").contains("placeholder"),
            "a placeholder-only chunk must be refused, not joined into gold: {err}"
        );
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
    fn finetune_pack_walks_each_source_once_not_once_per_clip() {
        // REGRESSION (2026-08-18). `decode_finetuned_clip_16k` decodes from byte zero to the clip's
        // end, so exporting N clips out of ONE recording decoded that recording N times, each walk
        // longer than the last. Measured on the live library — where 416 of 447 verified clips sit
        // inside a single podcast FLAC — the pack export ran at ~36 s per row: 87 rows in 56 minutes.
        // That is hours for one pack, and gate D asks the challenger loop to produce one on demand.
        //
        // This is invisible to output equality (the clips are byte-identical either way), so the
        // assertion is on DECODE PASSES, not on wall-clock, which on this box only produces flakes.
        let db = open_mem_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("one-long-podcast.wav");
        const SECONDS: usize = 60;
        const RATE: usize = 16_000;
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: RATE as u32,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::create(&source, spec).unwrap();
            // A ramp, so every sample position carries a DIFFERENT value: a clip sliced from the
            // wrong offset cannot accidentally look right.
            for index in 0..(SECONDS * RATE) {
                writer.write_sample((index / 100) as i16).unwrap();
            }
            writer.finalize().unwrap();
        }

        // 12 clips at 5 s intervals, each 6 s long: they overlap, one straddles the decoder's 30 s
        // window boundary, and the last one runs past the end of the file. All three are cases the
        // grouped walk has to get right that a per-clip walk got for free.
        const CLIPS: i64 = 12;
        for index in 0..CLIPS {
            let start_ms = index * 5_000;
            let meta = crate::chunking::SegmentSourceMeta {
                source_start_ms: start_ms,
                source_end_ms: start_ms + 6_000,
                chunk_index: index as u32,
                chunk_count: CLIPS as u32,
            };
            let id = format!("clip{index:02}");
            db.insert_segment(&crate::db::SpeechSegment {
                id: id.clone(),
                audio_path: source.to_string_lossy().to_string(),
                raw_transcript: format!("دەقی ژمارە {index}"),
                alignment_json: Some(meta.to_alignment_json()),
                ..Default::default()
            })
            .unwrap();
            db.update_verified(&id, true).unwrap();
            db.record_human_decision(&id, "accept", None, None).unwrap();
        }

        let out = tempfile::TempDir::new().unwrap();
        let before = crate::audio::decode_passes_on_this_thread();
        let result = export_finetune_pack(&db, out.path(), None).unwrap();
        let passes = crate::audio::decode_passes_on_this_thread() - before;

        assert_eq!(result.emitted, CLIPS as usize, "every clip is in the pack");
        assert_eq!(
            passes, 1,
            "{CLIPS} clips out of ONE recording must cost ONE decode walk, not {passes} — this is the \
             quadratic export that made a pack take hours"
        );

        // The clips are still sliced from the right place: clip k starts at k*5 s, and the ramp puts
        // sample value (k * 5 * 16000)/100 = k*800 there. Tolerance ±2 because decoding round-trips
        // i16 -> f32 -> i16 on a 32767 scale, so 800 comes back 799 — pre-existing and harmless, and
        // far tighter than the 800 that separates one clip's offset from the next.
        for index in 0..CLIPS {
            let path = out.path().join(format!("clips/clip{index:02}.wav"));
            let mut reader = hound::WavReader::open(&path).unwrap();
            let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
            let expected = (index * 800) as i16;
            assert!(
                (samples[0] - expected).abs() <= 2,
                "clip {index} was sliced from the wrong offset: first sample {}, expected ~{expected}",
                samples[0]
            );
            let expected_ms = if index == CLIPS - 1 { 5_000 } else { 6_000 };
            let actual_ms = (samples.len() as i64 * 1000) / RATE as i64;
            assert!(
                (actual_ms - expected_ms).abs() <= 100,
                "clip {index}: expected ~{expected_ms} ms, got {actual_ms} ms (the last clip runs past \
                 EOF and is truncated to what the file actually holds)"
            );
        }
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
        // Gold-provenance law (2026-08-12): the flag alone no longer grades human gold.
        db.record_human_decision("keep", "accept", None, None).unwrap();
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
        // Gold-provenance law (2026-08-12): the flag alone no longer grades human gold.
        db.record_human_decision("leak", "accept", None, None).unwrap();

        let out = tempfile::TempDir::new().unwrap();
        let ledger = out.path().join("corpus_ledger.jsonl");
        let result = export_finetune_pack(&db, out.path(), Some(&ledger)).unwrap();
        assert_eq!(result.total_verified, 2);
        assert_eq!(result.excluded_unexportable, 1, "the holdout-matching segment is excluded (leak guard)");
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
        assert_eq!(prov["excludedUnexportable"], 1);
        let ledger_text = std::fs::read_to_string(&ledger).unwrap();
        let ledger_line: serde_json::Value = serde_json::from_str(ledger_text.lines().next().unwrap()).unwrap();
        assert_eq!(ledger_line["manifestSha256"], result.manifest_sha256.as_str(), "ledger mirrors provenance");
        // The SHA really pins the manifest bytes.
        let recomputed = crate::models::compute_file_sha256(std::path::Path::new(&result.manifest_path)).unwrap();
        assert_eq!(recomputed, result.manifest_sha256);
    }

    /// Phase 2 (2026-08-17): the pack carries its own provenance, splits leakage-safely, and seals
    /// an immutable snapshot.
    ///
    /// An external audit found the pack emitted "essentially audio, text and duration" — a training
    /// run could not tell which human decision produced a label, which recording it came from, or
    /// whether the audio had been rebuilt by the pre-import cleaner. Worse, it was UNSPLIT: nothing
    /// stopped a fine-tune from validating on a voice it had just trained on, which with 94.7 % of
    /// today's labels coming from one recording is a certainty, not a risk.
    #[test]
    fn finetune_pack_carries_provenance_splits_safely_and_seals_one_snapshot() {
        let db = open_mem_db();
        let tmp = tempfile::TempDir::new().unwrap();

        // Two recordings, two clips each, so a split that leaked would be visible.
        let mut expected_rows = 0;
        for (recording, clips) in [("rec_a.wav", ["a1", "a2"]), ("rec_b.wav", ["b1", "b2"])] {
            let wav = tmp.path().join(recording);
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&wav, spec).unwrap();
            for i in 0..48000i32 {
                w.write_sample(((i % 100) - 50) as i16).unwrap();
            }
            w.finalize().unwrap();
            let path = wav.to_string_lossy().to_string();
            for (idx, id) in clips.iter().enumerate() {
                let start = idx as i64 * 1000;
                db.insert_segment(&crate::db::SpeechSegment {
                    id: (*id).into(),
                    audio_path: path.clone(),
                    raw_transcript: format!("دەقی {id}"),
                    alignment_json: Some(format!(
                        r#"{{"source_start_ms":{start},"source_end_ms":{},"chunk_index":{idx},"chunk_count":2}}"#,
                        start + 1000
                    )),
                    ..Default::default()
                })
                .unwrap();
                db.update_verified(id, true).unwrap();
                db.record_human_decision(id, "accept", None, None).unwrap();
                expected_rows += 1;
            }
        }
        // One recording declared as PROCESSED before import (migration v54).
        db.upsert_source_audio_provenance(&crate::db::SourceAudioProvenance {
            audio_path: tmp.path().join("rec_a.wav").to_string_lossy().to_string(),
            processing: "separated with melband_roformer_big_beta4.ckpt; non-speech CUT OUT".into(),
            separator_model: Some("melband_roformer_big_beta4.ckpt".into()),
            timeline_preserved: false,
            manifest_path: None,
        })
        .unwrap();

        let out = tempfile::TempDir::new().unwrap();
        let first = export_finetune_pack(&db, out.path(), None).unwrap();
        assert_eq!(first.emitted, expected_rows);
        assert!(first.newly_sealed, "the first export of this selection seals a snapshot");

        let manifest = std::fs::read_to_string(out.path().join("finetune_manifest.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = manifest.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.len(), expected_rows);

        // 1. Every row states where it came from and what produced it.
        for row in &rows {
            assert!(row["segment_id"].as_str().is_some_and(|s| !s.is_empty()));
            assert_eq!(row["decision"], "accept", "the human action that made this a label");
            assert!(row["decision_revision"].as_i64().is_some(), "which revision was trained on");
            assert_eq!(row["grade"], "gold", "human-decided rows grade gold");
            assert!(["train", "validation", "test"].contains(&row["split"].as_str().unwrap()));
        }

        // 2. Processed audio is flagged on exactly the declared recording — the trainer must not be
        //    told a separator's output is an original field recording.
        let processed: Vec<bool> = rows
            .iter()
            .filter(|r| r["source_recording"] == "rec_a.wav")
            .map(|r| r["audio_processed"].as_bool().unwrap())
            .collect();
        assert!(!processed.is_empty() && processed.iter().all(|p| *p), "rec_a is declared processed");
        assert!(
            rows.iter()
                .filter(|r| r["source_recording"] == "rec_b.wav")
                .all(|r| !r["audio_processed"].as_bool().unwrap()),
            "rec_b was never declared — it must not claim to be processed"
        );

        // 3. LEAKAGE: every clip of one recording shares one split. A recording straddling two
        //    splits is train→test contamination with near-identical acoustics.
        let mut by_recording: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
            std::collections::HashMap::new();
        for row in &rows {
            by_recording
                .entry(row["source_recording"].as_str().unwrap())
                .or_default()
                .insert(row["split"].as_str().unwrap());
        }
        for (recording, splits) in &by_recording {
            assert_eq!(splits.len(), 1, "{recording} straddles splits {splits:?} — that is a leak");
        }

        // 4. DETERMINISM + IMMUTABILITY: the same library exports byte-identically, and re-sealing
        //    an identical selection is a no-op rather than a second, competing snapshot.
        let out2 = tempfile::TempDir::new().unwrap();
        let second = export_finetune_pack(&db, out2.path(), None).unwrap();
        assert_eq!(second.manifest_sha256, first.manifest_sha256, "same data must hash the same");
        assert_eq!(second.snapshot_id, first.snapshot_id);
        assert!(!second.newly_sealed, "an identical selection must NOT seal a second snapshot");
        assert_eq!(
            std::fs::read_to_string(out2.path().join("finetune_manifest.jsonl")).unwrap(),
            manifest,
            "the manifest is a function of the data, not of when it was exported"
        );

        // 5. The sealed record is queryable and says what it selected.
        let (name, status, config) = db.dataset_snapshot(&first.snapshot_id).unwrap().expect("sealed");
        assert_eq!(name, "finetune-pack");
        assert_eq!(status, "sealed");
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(config["emitted"], expected_rows);
        assert_eq!(config["snapshotId"], first.snapshot_id.as_str());
        assert!(config["splitPolicy"].as_str().unwrap().contains("disjoint"));
    }

    /// Two files holding the SAME audio must never land in different splits.
    ///
    /// Found by adversarial verification 2026-08-17, and it was not theoretical: `assign_splits`
    /// groups by the path's BASENAME, and this library contains a recording imported under three
    /// names (`check_dataset_duplicates.py` documents `A1-0032_PODCAST-001.mp4` as a re-encode of
    /// the Lamofull material). Because Lamofull is 94.7 % of the labeled duration, once it fills
    /// `train` every other group is FORCED into validation/test — so its own re-encode was measured
    /// landing in `validation` while the same session trained the model. Basenames also collide
    /// across the audiobook corpus, where every book folder has its own `01.wav`, `02.wav`, …
    #[test]
    fn a_reencoded_recording_cannot_straddle_two_splits() {
        let db = open_mem_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        // The live library's SHAPE, which is what makes this bite: one recording holds the
        // overwhelming majority of the duration (Lamofull is 94.7 % of today's labels). It fills
        // `train` on its own, so every remaining group is forced into validation/test — and with
        // basename grouping the two twins are two groups, so they are forced APART.
        let mut ids_by_group: Vec<(String, Vec<String>)> = Vec::new();
        for (name, hash, chunks) in
            [("dominant.wav", "h-dom", 40), ("twin_a.wav", "same-content", 1), ("twin_b.wav", "same-content", 1)]
        {
            let wav = tmp.path().join(name);
            let mut w = hound::WavWriter::create(&wav, spec).unwrap();
            // Long enough for every chunk this file will be cut into (16 kHz, 1 s per chunk).
            for i in 0..(16000 * (chunks + 1)) {
                w.write_sample(((i % 100) - 50) as i16).unwrap();
            }
            w.finalize().unwrap();
            let path = wav.to_string_lossy().to_string();
            let mut ids = Vec::new();
            for chunk in 0..chunks {
                let id = format!("{name}-{chunk}");
                let start = chunk * 1000;
                db.insert_segment(&crate::db::SpeechSegment {
                    id: id.clone(),
                    audio_path: path.clone(),
                    raw_transcript: format!("دەقی {name} {chunk}"),
                    duration_ms: 1000,
                    alignment_json: Some(format!(
                        r#"{{"source_start_ms":{start},"source_end_ms":{},"chunk_index":{chunk},"chunk_count":{chunks}}}"#,
                        start + 1000
                    )),
                    ..Default::default()
                })
                .unwrap();
                db.update_verified(&id, true).unwrap();
                db.record_human_decision(&id, "accept", None, None).unwrap();
                ids.push(id);
            }
            // The identity that survives a re-encode (v51). The twins share theirs.
            db.set_audio_identity(&path, &crate::fingerprint::AudioIdentity { spectral: 1, content: hash.to_string() })
                .unwrap();
            ids_by_group.push((hash.to_string(), ids));
        }

        let out = tempfile::TempDir::new().unwrap();
        export_finetune_pack(&db, out.path(), None).unwrap();
        let manifest = std::fs::read_to_string(out.path().join("finetune_manifest.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = manifest.lines().map(|l| serde_json::from_str(l).unwrap()).collect();

        let split_of = |segment_id: &str| -> String {
            rows.iter()
                .find(|r| r["segment_id"] == segment_id)
                .map(|r| r["split"].as_str().unwrap().to_string())
                .unwrap_or_else(|| panic!("{segment_id} missing from the manifest"))
        };
        // Every clip sharing audio CONTENT shares a split, whatever the files are called.
        for (hash, _) in &ids_by_group {
            let splits: std::collections::HashSet<String> = ids_by_group
                .iter()
                .filter(|(h, _)| h == hash)
                .flat_map(|(_, ids)| ids.iter())
                .map(|id| split_of(id))
                .collect();
            assert_eq!(
                splits.len(),
                1,
                "content {hash} is spread across splits {splits:?} — a re-encode leaking train->test"
            );
        }
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
        // Gold-provenance law (2026-08-12): the flag alone no longer grades human gold.
        db.record_human_decision("intact", "accept", None, None).unwrap();
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
        // Gold-provenance law (2026-08-12): the flag alone no longer grades human gold.
        db.record_human_decision("clobbered", "accept", None, None).unwrap();

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
        // Gold-provenance law (2026-08-12): the flag alone no longer grades human gold.
        db.record_human_decision("good", "accept", None, None).unwrap();

        // MARK-BAD: human rejected it; the review flow still sets verified=true to clear the queue.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "markbad".into(),
            audio_path: wav_str.clone(),
            raw_transcript: "خراپە".into(),
            alignment_json: Some(r#"{"source_start_ms":0,"source_end_ms":400}"#.into()),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("markbad", true).unwrap();
        // Gold-provenance law (2026-08-12): the flag alone no longer grades human gold.
        db.record_human_decision("markbad", "reject", None, None).unwrap();
        db.connection().execute("UPDATE speech_segments SET human_decision='reject' WHERE id='markbad'", []).unwrap();

        // SEVERE AUDIO: verified but the clip is badly clipped — rubric grade REJECT.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "clipped".into(),
            audio_path: wav_str.clone(),
            raw_transcript: "قسەیەک".into(),
            alignment_json: Some(r#"{"source_start_ms":400,"source_end_ms":800}"#.into()),
            clipping_ratio: Some(0.5),
            ..Default::default()
        })
        .unwrap();
        db.update_verified("clipped", true).unwrap();
        // Gold-provenance law (2026-08-12): the flag alone no longer grades human gold.
        db.record_human_decision("clipped", "accept", None, None).unwrap();

        let out = tempfile::TempDir::new().unwrap();
        let result = export_finetune_pack(&db, out.path(), None).unwrap();
        assert_eq!(result.total_verified, 3);
        assert_eq!(result.excluded_unexportable, 1, "mark-bad is dropped by the shared filter now (audit #11)");
        assert_eq!(result.excluded_not_training_ready, 1, "severe-clipping refused by the rubric");
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
            // Gold-provenance law (2026-08-12): the flag alone no longer grades human gold.
            db.record_human_decision(id, "accept", None, None).unwrap();
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
    fn the_eval_ci_is_seeded_reproducible_and_brackets_the_point_estimate() {
        // A published accuracy record without a CI invites false precision (deep audit P0 #1). These are
        // the three properties that make the interval usable rather than decorative.
        // VARY ref_len, not just the distance. A fixture where every clip has the same denominator makes
        // sum(ref_len) constant under resampling, so the micro rate can only take a handful of discrete
        // values and the 2.5/97.5 percentiles land on the same ones whatever the seed — which made the
        // determinism assertion below pass even with the seed deliberately defeated. Caught by running
        // the fail-before; a coarse fixture is how a test becomes decorative.
        let pairs: Vec<(usize, usize)> = (0..60).map(|i| ((i * 7) % 11 + 1, (i * 13) % 37 + 8)).collect();
        let point = pairs.iter().map(|p| p.0).sum::<usize>() as f64 / pairs.iter().map(|p| p.1).sum::<usize>() as f64;

        let (lo, hi) = bootstrap_micro_ci(&pairs, 2000, 42).expect("a CI over 60 clips");
        assert!(lo < point && point < hi, "the interval must bracket the point estimate: {lo} < {point} < {hi}");
        assert!(hi - lo > 0.0, "a degenerate zero-width interval is not an interval");

        // Same seed, same data => byte-identical interval, so a run id reproduces its own numbers.
        let again = bootstrap_micro_ci(&pairs, 2000, 42).unwrap();
        assert_eq!((lo, hi), again, "the bootstrap must be deterministic under a fixed seed");

        // Resampling is over UTTERANCES: a corpus where every clip is identical has no variance to find.
        let flat: Vec<(usize, usize)> = vec![(2, 20); 40];
        let (flo, fhi) = bootstrap_micro_ci(&flat, 500, 42).unwrap();
        assert!((flo - fhi).abs() < 1e-12, "identical clips cannot produce a spread: {flo}..{fhi}");

        // Nothing to resample must yield NO interval rather than a fabricated [0,0].
        assert!(bootstrap_micro_ci(&[], 1000, 42).is_none(), "an empty run has no CI");
        assert!(bootstrap_micro_ci(&pairs, 0, 42).is_none(), "zero replicates is not a CI");
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
