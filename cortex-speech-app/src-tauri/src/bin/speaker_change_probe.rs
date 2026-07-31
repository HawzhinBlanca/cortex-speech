//! Measure how many existing segments contain a SPEAKER CHANGE, using the CAM++ model already on disk.
//!
//! WHY THIS EXISTS. The owner hit a clip containing two speakers while reviewing, and asked for a better
//! chunking architecture. Before adding anything, the honest question is *how often does this actually
//! happen in his corpus* — and nothing in this repo measured it. Diarization accuracy, overlap rate and
//! speaker-change rate are all currently unmeasured (there is no DER anywhere in eval.rs/wer.rs).
//!
//! HOW. Segment boundaries are planned by silence alone (`chunking::plan_speech_chunks`), and speaker
//! labels are assigned to whole chunks AFTERWARDS — so one label per chunk, by construction, however many
//! people are talking in it. This probe embeds the FIRST HALF and the SECOND HALF of each segment with
//! CAM++ and takes their cosine similarity. Two halves of one person speaking are near-identical; a
//! turn-taking exchange is not. The threshold is `MIN_SPEAKER_SIMILARITY` (0.85) — the same number
//! `diarization::online_cluster` already uses to decide "different person", so this measurement is on the
//! app's own terms rather than a new one invented for it.
//!
//! WHAT IT DOES NOT MEASURE. Simultaneous overlap. Two voices talking at once produce a single blended
//! embedding, and CAM++ has no way to report "both". This finds turn-taking within a clip, which is the
//! common case; true cross-talk needs an overlap-aware segmentation model (pyannote powerset).
//!
//! READ-ONLY. Opens the library `mode=ro` and never writes. Safe to run while the app is up.
//!
//! Usage: cargo run --release --bin speaker_change_probe [-- <db_path>]

use cortex_speech_app_lib::{audio, chunking, diarization::SpeakerEmbeddingService, models};
use rusqlite::{Connection, OpenFlags};

/// CAM++ needs enough audio for a stable embedding; half of a clip shorter than this is too little to
/// judge, and reporting a verdict on it would be measuring noise.
const MIN_HALF_MS: i64 = 1500;

fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 || !dot.is_finite() {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

fn main() -> Result<(), String> {
    let db_path = std::env::args().nth(1).unwrap_or_else(|| {
        let base = std::env::var("APPDATA").unwrap_or_default();
        format!("{base}\\cortex-speech\\cortex-speech.db")
    });
    // Deliberately NOT `Database::open`: that opens read-write and runs `PRAGMA journal_mode=WAL`, which
    // is itself a write, and its `Connection::open` does not enable URI parsing — so a `file:…?mode=ro`
    // string would be taken as a literal filename and quietly create a stray empty database. Opening with
    // SQLITE_OPEN_READ_ONLY is the only form that genuinely cannot touch the owner's library, which
    // matters because this is expected to run while the app is up and he is reviewing.
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open {db_path} read-only: {e}"))?;

    // PER-FILE resolution, deliberately. `resolve_models_dir` is all-or-nothing — it keys the whole root
    // on OmniASR-CTC being present — so a user models dir holding only the ASR model would orphan a
    // bundled-only CAM++ and this probe would report "no model" while the file sits right there.
    // `resolve_model_file` resolves CAM++ on its own terms; the service wants the ROOT above it.
    let campp = models::resolve_model_file(models::CAMPP_MODEL);
    let models_dir = campp
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| format!("cannot derive a models root from {}", campp.display()))?;
    println!("CAM++: {} (exists: {})", campp.display(), campp.exists());
    let embed = SpeakerEmbeddingService::new(models_dir);
    if !embed.is_available() {
        return Err("CAM++ is not present — this probe measures nothing without it".into());
    }

    // Only the four columns this needs, so the probe cannot depend on (or be broken by) the wider schema.
    let mut stmt = conn
        .prepare(
            "SELECT id, audio_path, duration_ms, alignment_json, COALESCE(speaker_id, '')
             FROM speech_segments ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let segments: Vec<(String, String, i64, Option<String>, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    println!("segments in library: {}\n", segments.len());

    let mut sims: Vec<(String, f32, i64)> = Vec::new();
    let mut skipped_short = 0usize;
    let mut skipped_audio = 0usize;

    // Keep each half's embedding, not just the within-clip score. The controls below need them.
    let mut halves: Vec<(String, String, Vec<f32>, Vec<f32>)> = Vec::new();

    for (id, audio_path, duration_ms, alignment_json, speaker) in &segments {
        if *duration_ms < MIN_HALF_MS * 2 {
            skipped_short += 1;
            continue;
        }
        let Ok((rate, pcm)) = audio::decode_to_pcm(audio_path) else {
            skipped_audio += 1;
            continue;
        };
        let Ok((rate, pcm)) = audio::ensure_pcm_16khz(rate, pcm) else {
            skipped_audio += 1;
            continue;
        };
        let Ok((clip, _)) = chunking::slice_pcm_by_alignment(&pcm, rate, alignment_json.as_deref()) else {
            skipped_audio += 1;
            continue;
        };
        if clip.len() < 2 {
            skipped_short += 1;
            continue;
        }
        let mid = clip.len() / 2;
        let to_f32 = |s: &[i16]| s.iter().map(|&v| v as f32 / 32768.0).collect::<Vec<f32>>();
        let a = embed.compute_embedding(&to_f32(&clip[..mid]), rate);
        let b = embed.compute_embedding(&to_f32(&clip[mid..]), rate);
        match cosine(&a, &b) {
            Some(sim) => {
                sims.push((id.clone(), sim, *duration_ms));
                halves.push((id.clone(), speaker.clone(), a, b));
            }
            None => skipped_audio += 1,
        }
    }

    if sims.is_empty() {
        println!("no segment could be measured (short: {skipped_short}, audio: {skipped_audio})");
        return Ok(());
    }

    println!("measured:            {}", sims.len());
    println!("skipped (too short): {skipped_short}");
    println!("skipped (audio):     {skipped_audio}");

    // ── THE CONTROLS ──────────────────────────────────────────────────────────
    // An absolute cosine number means nothing on its own. A first pass of this probe reported "100% of
    // segments contain a speaker change" purely because the threshold was borrowed from
    // `online_cluster`, where 0.85 compares a chunk against an AVERAGED, re-normalised centroid — not two
    // raw 6-second embeddings against each other. Raw same-speaker pairs simply do not score that high.
    //
    // So measure two reference distributions from the same embeddings and let them set the scale:
    //   SAME  — halves of DIFFERENT clips that CAM++ already gave the SAME label. Same person, different
    //           moment: this is the ceiling a single-speaker clip can realistically hit.
    //   DIFF  — halves of clips CAM++ gave DIFFERENT labels. Genuinely two people: the floor.
    // If the within-clip distribution sits with SAME, the clips are single-speaker and the first result
    // was an artefact. If it sits with DIFF, the clips really do straddle turns.
    let mut within: Vec<f32> = sims.iter().map(|(_, s, _)| *s).collect();
    let mut same: Vec<f32> = Vec::new();
    let mut diff: Vec<f32> = Vec::new();
    for (i, (_, spk_a, a1, _)) in halves.iter().enumerate() {
        for (_, spk_b, b1, _) in halves.iter().skip(i + 1) {
            if spk_a.is_empty() || spk_b.is_empty() {
                continue;
            }
            if let Some(sim) = cosine(a1, b1) {
                if spk_a == spk_b {
                    same.push(sim);
                } else {
                    diff.push(sim);
                }
            }
        }
    }

    let stats = |label: &str, v: &mut Vec<f32>| {
        if v.is_empty() {
            println!("  {label:<28} (no pairs)");
            return;
        }
        v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        let at = |q: f64| v[((v.len() - 1) as f64 * q) as usize];
        println!(
            "  {label:<28} n={:<6} min {:.3}  p10 {:.3}  median {:.3}  p90 {:.3}  max {:.3}",
            v.len(),
            v[0],
            at(0.10),
            at(0.50),
            at(0.90),
            v[v.len() - 1]
        );
    };

    println!("\ncosine similarity distributions (the controls are what make the first line meaningful)");
    stats("WITHIN-clip (half vs half)", &mut within);
    stats("SAME speaker, other clip", &mut same);
    stats("DIFFERENT speaker, other clip", &mut diff);

    // The verdict is relative, never against a borrowed absolute. Compare the within-clip median to the
    // two controls and report which it resembles — and say so plainly when the controls overlap so much
    // that nothing can be concluded.
    let med = |v: &[f32]| if v.is_empty() { f32::NAN } else { v[v.len() / 2] };
    let (mw, ms, md) = (med(&within), med(&same), med(&diff));
    println!("\nmedians: within {mw:.3}   same-speaker {ms:.3}   different-speaker {md:.3}");
    if ms.is_nan() || md.is_nan() {
        println!("VERDICT: not enough labelled pairs to calibrate — no conclusion.");
    } else if (ms - md).abs() < 0.05 {
        println!(
            "VERDICT: INCONCLUSIVE. The same-speaker and different-speaker controls are only \
             {:.3} apart, so CAM++ is not separating speakers on this material at all and no \
             within-clip number can be interpreted.",
            (ms - md).abs()
        );
    } else {
        let midpoint = (ms + md) / 2.0;
        let leaning = if mw < midpoint { "DIFFERENT-speaker" } else { "SAME-speaker" };
        println!(
            "VERDICT: the within-clip median sits on the {leaning} side of the midpoint ({midpoint:.3}). \
             Clips scoring below it are the ones most likely to straddle a speaker turn."
        );
        let below = within.iter().filter(|s| **s < midpoint).count();
        println!(
            "  segments below the midpoint: {} / {} ({:.1}%)",
            below,
            within.len(),
            100.0 * below as f64 / within.len() as f64
        );
        println!("\nlowest within-clip similarity (most likely to hold two speakers):");
        let mut worst = sims.clone();
        worst.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        for (id, sim, dur) in worst.iter().take(10) {
            println!("  {id}  cosine {sim:.4}  {:.1}s", *dur as f64 / 1000.0);
        }
    }
    Ok(())
}
