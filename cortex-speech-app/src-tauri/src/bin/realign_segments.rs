//! Re-align existing segments through the PRODUCTION alignment path.
//!
//! WHY THIS EXISTS. Until iteration 230, `pipeline::enqueue_background_alignments` called a free
//! `aligner::align()` stub that returned the energy heuristic unconditionally and then hardcoded
//! `EnergyHeuristic` as the stored quality. `auto_align` is on, so that ran on every import: 129 of the
//! owner's 144 segments carry heuristic word timings, against 15 `ctc_forced` produced by the foreground
//! path on the same machine with the same models. Fixing the code only changes what happens from now on
//! — the already-stamped rows keep their heuristic timings until something re-aligns them. This is that
//! something, and re-aligning a live library is the owner's decision, which is why it is a deliberate
//! one-off tool rather than anything that runs by itself.
//!
//! WHAT IT TOUCHES. `update_segment_alignment` writes `alignment_json` + `alignment_quality` +
//! `updated_at`, and NOTHING else — no transcript, no `verified`, no `reviewed_by`, no human decision.
//! Word timings are re-derived; the human work product is untouched by construction. The new JSON is
//! built with `merge_word_timestamps`, which PRESERVES `source_start_ms`/`source_end_ms`, so every reader
//! that slices a clip out of the source (phone playback, dataset audio export, the 7B re-transcribe
//! client, jury acoustic scoring) keeps working on exactly the same bytes.
//!
//! DRY RUN BY DEFAULT. Without `--apply` it aligns everything and reports the quality each row WOULD
//! get, writing nothing. Take the snapshot, read the dry run, then re-run with `--apply`.
//!
//! Uses `ProcessingPipeline::align`, i.e. the same call the desktop re-align uses: it prefers real CTC
//! forced alignment from the fine-tuned MMS-CTC model when installed and falls back to the bundled
//! `mms_aligner.onnx`, so this produces the best alignment the machine can, not merely a non-heuristic one.
//!
//! Usage: realign_segments [--apply] [--data-dir <dir>]

use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::AppSettings;
use cortex_speech_app_lib::{chunking, corrections};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|a| a == "--apply");
    let data_dir: PathBuf = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("cortex-speech"));

    let db_path = data_dir.join("cortex-speech.db");
    // Mirrors lib.rs exactly (settings.json beside the DB, models under <data>/models) so this tool
    // resolves the SAME engine and GPU choice the app would. A tool that aligned with different
    // settings than production would produce timings the app could never reproduce.
    let settings = AppSettings::load(&data_dir.join("settings.json"));
    println!("data dir : {}", data_dir.display());
    println!("db       : {}", db_path.display());
    println!("gpu      : {}", settings.enable_gpu);
    println!("mode     : {}\n", if apply { "APPLY (writes)" } else { "DRY RUN (writes nothing)" });

    let db_path_str = db_path.to_string_lossy().to_string();
    let db = Database::open(&db_path_str).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let pipeline = ProcessingPipeline::new(
        db_path_str.clone(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(100)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(settings),
        Arc::new(ModelManager::new(data_dir.join("models"))),
    );

    let segments = db.get_segments(None).map_err(|e| e.to_string())?;
    let total = segments.len();
    // Anything not already forced is a candidate: `energy_heuristic` AND never-aligned (NULL).
    let targets: Vec<_> =
        segments.into_iter().filter(|s| s.alignment_quality.as_deref() != Some("ctc_forced")).collect();
    println!("{} segments, {} not ctc_forced\n", total, targets.len());

    let mut by_quality: BTreeMap<&'static str, usize> = BTreeMap::new();
    let (mut skipped_empty, mut no_words, mut failed) = (0usize, 0usize, 0usize);
    let started = std::time::Instant::now();

    for (i, seg) in targets.iter().enumerate() {
        // The SAME text selection the background aligner uses: annotated > raw (VERBATIM LAW).
        let text = corrections::loop0_draft_text(seg.annotated_transcript.as_deref(), &seg.raw_transcript).to_string();
        if text.trim().is_empty() {
            skipped_empty += 1;
            continue;
        }
        match pipeline.align(&seg.audio_path, &text, seg.alignment_json.as_deref()) {
            Ok((words, quality)) if !words.is_empty() => {
                *by_quality.entry(quality.as_db_str()).or_insert(0) += 1;
                if apply {
                    // MERGE, never flat-overwrite: the source offsets in the existing JSON are what
                    // every later reader slices by, and losing them silently degrades each of them to
                    // the whole recording.
                    let merged = chunking::merge_word_timestamps(seg.alignment_json.as_deref(), &words);
                    db.update_segment_alignment(&seg.id, &merged, quality.as_db_str())
                        .map_err(|e| format!("persist {}: {e}", seg.id))?;
                }
            }
            // An empty word list is NOT written: it would replace real timings with nothing.
            Ok(_) => no_words += 1,
            Err(e) => {
                failed += 1;
                eprintln!("  align failed for {}: {e}", seg.id);
            }
        }
        if (i + 1) % 25 == 0 {
            println!("  {} / {} ({:.0}s elapsed)", i + 1, targets.len(), started.elapsed().as_secs_f64());
        }
    }

    println!("\nresult after {:.0}s:", started.elapsed().as_secs_f64());
    for (q, n) in &by_quality {
        println!("  {q:<18} {n}");
    }
    println!("  {:<18} {}", "skipped (no text)", skipped_empty);
    println!("  {:<18} {}", "no words produced", no_words);
    println!("  {:<18} {}", "align failed", failed);
    if !apply {
        println!("\nDRY RUN — nothing was written. Re-run with --apply to persist.");
    }
    Ok(())
}
