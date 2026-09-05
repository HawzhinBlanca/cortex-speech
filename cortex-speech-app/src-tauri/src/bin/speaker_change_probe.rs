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
//! CAM++ and takes their cosine similarity.
//!
//! THE CONTROLS LIED, AND THE GROUND TRUTH RESCUED IT. Same-speaker and different-speaker control pairs
//! came back 0.009 apart, which reads as "CAM++ cannot separate these voices" — but those controls are
//! built from CAM++'s own CHUNK labels, and a chunk holding two speakers gets one label, so both groups
//! were mixtures and of course looked alike. A blind 15-clip listening pass by the owner (GROUND_TRUTH
//! below) separated perfectly: every turn-taking clip <= 0.428, every single-speaker clip >= 0.753, an
//! empty 0.325-wide gap. The signal was always good; the control was circular. `--export <dir>` writes
//! the listening set that breaks the circle.
//!
//! WHAT IT DOES NOT MEASURE. Simultaneous overlap. Two voices talking at once produce a single blended
//! embedding, and CAM++ has no way to report "both". This finds turn-taking within a clip, which is the
//! common case; true cross-talk needs an overlap-aware segmentation model (pyannote powerset).
//!
//! READ-ONLY BY DEFAULT. Opens the library with SQLITE_OPEN_READ_ONLY, so it is safe to run while the
//! app is up. `--persist` additionally takes the desktop's exclusive instance lock before reading,
//! then opens a SECOND, read-write connection at the end and stores each measured score in
//! `speech_segments.speaker_change_score` (Migration v47). The lock enforces one coherent generation
//! and refuses the persistent form until the app is closed.
//!
//! Usage: speaker_change_probe [<db_path>] [--export <dir>] [--persist]

use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::diarization::{SpeakerEmbeddingService, SPEAKER_CHANGE_THRESHOLD};
use cortex_speech_app_lib::{audio, chunking, models};
use rusqlite::{Connection, OpenFlags};

/// CAM++ needs enough audio for a stable embedding; half of a clip shorter than this is too little to
/// judge, and reporting a verdict on it would be measuring noise.
const MIN_HALF_MS: i64 = 1500;

/// One library row, in the only five-plus-one columns this probe reads:
/// `(id, audio_path, duration_ms, alignment_json, speaker_id, verified)`.
///
/// Named rather than repeated inline because it appears in three signatures, and a bare 6-tuple spelled
/// out in each is how adding `verified` broke two call sites that had nothing to do with the change.
type SegRow = (String, String, i64, Option<String>, String, i64);

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

/// Model-derived labels are descriptive groups, never independent ground truth. In particular,
/// a missing comparison group cannot establish either separation or equality of distributions.
fn comparison_summary(same: &[f32], different: &[f32]) -> String {
    if same.is_empty() || different.is_empty() || same.iter().chain(different).any(|score| !score.is_finite()) {
        return format!(
            "Comparison unavailable: same-label pairs={}, different-label pairs={}; both finite groups are required. \
             No speaker-separation conclusion is supported by these controls.",
            same.len(),
            different.len()
        );
    }
    let median = |values: &[f32]| {
        let mut sorted = values.to_vec();
        sorted.sort_by(f32::total_cmp);
        // Match the empirical 50th-percentile definition used in the distribution table below.
        sorted[(sorted.len() - 1) / 2]
    };
    format!(
        "Model-derived label-group medians differ by {:.3}. These groups are not independent human labels; \
         their difference alone does not establish speaker separation or equal distributions.",
        (median(same) - median(different)).abs()
    )
}

fn screening_summary(flagged: usize, measured: usize) -> String {
    if measured == 0 {
        return "No clips measured; speaker-change prevalence is unknown.".to_string();
    }
    format!(
        "MEASURED SET: {flagged} / {measured} clips ({:.1}%) fall below {SPEAKER_CHANGE_THRESHOLD}: \
         speaker-change candidates for listening review, not confirmed changes. \
         This describes only the measured set, not unsampled clips. Simultaneous overlap is not detected.",
        100.0 * flagged as f64 / measured as f64
    )
}

/// The positional db path: the first argument that is neither a flag nor the value of `--export`.
fn db_path_arg(args: &[String]) -> Option<String> {
    args.iter()
        .enumerate()
        .find(|(i, a)| !(a.starts_with("--") || (*i > 0 && args[i - 1] == "--export")))
        .map(|(_, a)| a.clone())
}

fn main() -> Result<(), String> {
    // Parsed together, because taking `args().nth(1)` as the db path swallows `--export` when it comes
    // first and then tries to open a database called "--export".
    let args: Vec<String> = std::env::args().skip(1).collect();
    let export_dir = args.iter().position(|a| a == "--export").and_then(|i| args.get(i + 1).cloned());
    let persist = args.iter().any(|a| a == "--persist");
    // `--replan <wav>`: the measurement iteration 225 demands before speaker-aware cutting may ship.
    // Plans the SAME real recording silence-only and judge-aware and reports exactly what the judge
    // changed, so the wiring in pipeline.rs is kept or cut on numbers, not on the design sounding right.
    if let Some(wav) = args.iter().position(|a| a == "--replan").and_then(|i| args.get(i + 1).cloned()) {
        return replan_comparison(&wav);
    }
    let db_path = db_path_arg(&args).unwrap_or_else(|| {
        let base = std::env::var("APPDATA").unwrap_or_default();
        format!("{base}\\cortex-speech\\cortex-speech.db")
    });
    let _instance_lock = if persist {
        let path = std::path::Path::new(&db_path);
        let data_dir =
            path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| std::path::Path::new("."));
        Some(cortex_speech_app_lib::flock::InstanceLock::try_lock(data_dir).map_err(|error| {
            format!("Cannot persist speaker-change scores while Cortex is running: {error}. Stop review and close the app first.")
        })?)
    } else {
        None
    };
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

    // Only the columns this needs (see `SegRow`), so the probe cannot depend on — or be broken by — the
    // wider schema.
    // ORDERED BY SOURCE FILE, and that ordering is load-bearing — see the decode cache below.
    let mut stmt = conn
        .prepare(
            "SELECT id, audio_path, duration_ms, alignment_json, COALESCE(speaker_id, ''), verified
             FROM speech_segments ORDER BY audio_path, id",
        )
        .map_err(|e| e.to_string())?;
    let segments: Vec<SegRow> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    println!("segments in library: {}\n", segments.len());

    let mut sims: Vec<(String, f32, i64)> = Vec::new();
    let mut skipped_short = 0usize;
    let mut skipped_audio = 0usize;

    // Keep each half's embedding, not just the within-clip score. The controls below need them.
    let mut halves: Vec<(String, String, Vec<f32>, Vec<f32>)> = Vec::new();

    // ONE decode per SOURCE FILE, not per clip. The library's clips point at whole episode WAVs —
    // ~450 clips per ~58-minute file — and decoding the full source for every row meant ~112 MB of
    // PCM churned 450 times per episode. MEASURED 2026-08-17: the per-clip version died mid-run on
    // the 14,828-clip library (Windows RADAR_PRE_LEAK_64 fault on this exe, zero scores persisted,
    // no output past the header) after over an hour of compute. Rows are ordered by audio_path
    // above, so one cached decode serves every clip cut from that source before it is dropped —
    // bounded memory, and the decode cost falls by the clips-per-source factor.
    let mut cached: Option<(String, u32, Vec<i16>)> = None;
    for (done, (id, audio_path, duration_ms, alignment_json, speaker, _verified)) in segments.iter().enumerate() {
        // A long silent loop is undiagnosable when it dies — that is exactly how the per-clip
        // version failed: the log ended at the header and the crash site was anyone's guess.
        if done % 1000 == 0 && done > 0 {
            println!("  ...{done} of {} clips", segments.len());
        }
        if *duration_ms < MIN_HALF_MS * 2 {
            skipped_short += 1;
            continue;
        }
        // `is_some_and` rather than `is_none_or`: the latter is stable since 1.82 and the project's
        // MSRV is 1.81 — CI clippy enforces it even though the local toolchain happily compiles it.
        if !cached.as_ref().is_some_and(|(path, _, _)| path == audio_path) {
            let Ok((raw_rate, raw_pcm)) = audio::decode_to_pcm(audio_path) else {
                skipped_audio += 1;
                cached = None;
                continue;
            };
            let Ok(resampled) = audio::ensure_pcm_16khz(raw_rate, raw_pcm) else {
                skipped_audio += 1;
                cached = None;
                continue;
            };
            cached = Some((audio_path.clone(), resampled.0, resampled.1));
        }
        let Some((_, rate, pcm)) = cached.as_ref() else {
            skipped_audio += 1;
            continue;
        };
        let (rate, pcm) = (*rate, pcm.as_slice());
        let Ok((clip, _)) = chunking::slice_pcm_by_alignment(pcm, rate, alignment_json.as_deref()) else {
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

    if persist {
        persist_scores(&db_path, &sims)?;
    }

    // ── THE CONTROLS ──────────────────────────────────────────────────────────
    // An absolute cosine number means nothing on its own. A first pass of this probe reported "100% of
    // segments contain a speaker change" purely because the threshold was borrowed from
    // `online_cluster`, where 0.85 compares a chunk against an AVERAGED, re-normalised centroid — not two
    // raw 6-second embeddings against each other. Raw same-speaker pairs simply do not score that high.
    //
    // These descriptive groups use existing model-derived labels, not independently verified people.
    // SAME and DIFF labels can both contain mixed-speaker clips; neither establishes a true baseline.
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

    println!("\ncosine similarity distributions (label groups are descriptive, not independent controls)");
    stats("WITHIN-clip (half vs half)", &mut within);
    stats("SAME label, other clip", &mut same);
    stats("DIFFERENT label, other clip", &mut diff);

    // Report group availability and descriptive differences without inferring verified speaker identity.
    // ── optional: export a listening set ──────────────────────────────────────
    // The probe cannot settle this on its own (see the verdict above): it uses CAM++'s own labels as
    // ground truth for CAM++. Only a human ear can break that circle. `--export <dir>` writes a
    // stratified sample spanning the observed similarity range, so whichever way the answer falls it is
    // informative: if the low-similarity clips hold two speakers and the high ones do not, the signal
    // works and the threshold was simply wrong; if there is no relationship, the signal is dead.
    if let Some(dir) = export_dir.as_deref() {
        export_listening_set(dir, &sims, &segments)?;
    }

    println!("\n{}", comparison_summary(&same, &diff));

    // ── GROUND TRUTH ──────────────────────────────────────────────────────────
    report_against_ground_truth(&within, &sims, &segments);
    Ok(())
}

/// The owner's blind listening pass, 2026-07-31. Fifteen clips, shuffled, with the similarity score,
/// the CAM++ label and the stratification band all hidden until after every answer was given.
///
/// This is the only ground truth this repo has about speaker content, and it is what rescued the
/// measurement: the probe's own controls said INCONCLUSIVE because they are derived from CAM++'s chunk
/// labels, and those labels are unreliable precisely BECAUSE chunks hold more than one speaker. Both
/// control groups were mixtures, so of course they looked identical. A human ear is outside that loop.
const GROUND_TRUTH: &[(&str, &str, f32)] = &[
    ("0817584d", "multi", 0.305),
    ("f684c691", "multi", 0.412),
    ("97370a88", "multi", 0.415),
    ("6f23f57d", "multi", 0.427),
    ("290f5f58", "multi", 0.428),
    ("12664c57", "one", 0.753),
    ("104e7bc2", "one", 0.753),
    ("c8eb6b3b", "one", 0.754),
    ("63c705ef", "one", 0.757),
    ("4d5c933c", "one", 0.757),
    // Genuinely overlapping speech, and it scored with the SINGLE-speaker clips. Two voices talking at
    // once blend into one consistent texture across both halves, so this method is structurally blind to
    // overlap — predicted before the listening pass, confirmed by it.
    ("2840c3fd", "overlap", 0.841),
    ("828e0213", "one", 0.842),
    ("99ccc369", "one", 0.843),
    ("9c4b4e36", "one", 0.844),
    ("0ca5fea3", "one", 0.845),
];

// SPEAKER_CHANGE_THRESHOLD is imported from `diarization`, not restated here. It used to be declared
// in both places — two copies of one calibration that had to agree, with nothing making them. The doc
// comment on the real one carries the derivation from the ground truth above.

/// Store every MEASURED score, so the flag lives on the row instead of in this console output.
///
/// EVERY measured clip, not just the ones below the threshold. Writing only the flagged ones would
/// leave NULL meaning two different things — "not measured" and "measured, one speaker" — and the
/// phone's badge decides what to show from exactly that distinction.
///
/// A SECOND connection, deliberately: the measuring one above is read-only and stays that way. This
/// one is opened only when `--persist` was asked for, and only after all the measuring is done.
///
/// `Database::open` does NOT run migrations (`initialize` does), so on a library that predates v47 the
/// UPDATE fails with "no such column" instead of a side tool quietly migrating the owner's schema.
///
/// ponytail: 144 individual UPDATEs, no transaction. An interruption leaves fewer rows flagged, never
/// a wrong one — and the fix is to run it again.
/// Plan one real recording both ways and report the difference. The verdict discipline is iteration
/// 225's: the judge-aware plan ships only if what it changes is small, targeted and audible — so
/// alongside the counts, every refused boundary's timestamp is printed for the owner to listen to.
/// Uses the app's own settings defaults (0.5 / 3s / 15s) so the plan is the plan production would make.
fn replan_comparison(wav: &str) -> Result<(), String> {
    let campp = models::resolve_model_file(models::CAMPP_MODEL);
    let models_dir = campp
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| format!("cannot derive a models root from {}", campp.display()))?;
    let embed = SpeakerEmbeddingService::new(models_dir);
    if !embed.is_available() {
        return Err("CAM++ is not present — nothing to measure".into());
    }
    let (rate, pcm) = audio::decode_to_pcm(wav).map_err(|e| format!("decode {wav}: {e}"))?;
    let (rate, pcm) = audio::ensure_pcm_16khz(rate, pcm).map_err(|e| format!("resample: {e}"))?;
    println!("replan: {wav}\n  {:.1} min of audio at {rate} Hz", pcm.len() as f64 / rate as f64 / 60.0);

    let (vad_th, min_ms, max_ms) = (0.5f32, 3_000u32, 15_000u32);
    let (silence_only, _) = chunking::plan_speech_chunks(&pcm, rate, vad_th, min_ms, max_ms)
        .map_err(|e| format!("silence-only plan: {e}"))?;
    // The production judge, instrumented: every consulted boundary's similarity is recorded, because
    // "0 boundaries added" is ambiguous — no candidates at all, or candidates that all scored same-
    // speaker — and those two readings call for opposite next steps.
    let consulted = std::cell::RefCell::new(Vec::<f32>::new());
    let judge = |a: &[i16], b: &[i16]| -> Option<bool> {
        let to_f32 = |s: &[i16]| s.iter().map(|&v| v as f32 / 32768.0).collect::<Vec<f32>>();
        let ea = embed.compute_embedding(&to_f32(a), rate);
        let eb = embed.compute_embedding(&to_f32(b), rate);
        let sim = cortex_speech_app_lib::diarization::cosine_similarity(&ea, &eb)?;
        consulted.borrow_mut().push(sim);
        Some(sim >= cortex_speech_app_lib::diarization::SPEAKER_TURN_REFUSAL_CEILING)
    };
    let (judged, _) = chunking::plan_speech_chunks_with_judge(&pcm, rate, vad_th, min_ms, max_ms, Some(&judge))
        .map_err(|e| format!("judge-aware plan: {e}"))?;

    let ms = |samples: usize| chunking::samples_to_ms(samples, rate);
    let stats = |label: &str, plan: &[(usize, usize)]| {
        let durs: Vec<i64> = plan.iter().map(|&(s, e)| ms(e - s)).collect();
        let min = durs.iter().min().copied().unwrap_or(0);
        let sub_floor = durs.iter().filter(|&&d| d < min_ms as i64).count();
        println!("  {label:13} {:4} chunks   min {min} ms   {sub_floor} under the {min_ms} ms floor", plan.len());
    };
    stats("silence-only:", &silence_only);
    stats("judge-aware:", &judged);

    // The boundaries the judge ADDED — each one a claim that two voices meet there. Listed as
    // timestamps into the source so the owner can seek to each and listen; that listening IS the
    // ground truth this feature ships or dies on.
    let mut sims = consulted.into_inner();
    sims.sort_by(|a, b| a.total_cmp(b));
    println!("  boundaries the judge was CONSULTED on: {}", sims.len());
    if !sims.is_empty() {
        let show = |v: &[f32]| v.iter().map(|s| format!("{s:.3}")).collect::<Vec<_>>().join(" ");
        println!("    lowest 10 sims:  {}", show(&sims[..sims.len().min(10)]));
        println!("    highest 5 sims:  {}", show(&sims[sims.len().saturating_sub(5)..]));
        let below_059 = sims.iter().filter(|&&s| s < 0.59).count();
        println!("    below the 0.59 within-clip threshold: {below_059} (refusal bar is 0.43)");
    }
    let old: std::collections::HashSet<usize> = silence_only.iter().map(|&(s, _)| s).collect();
    let added: Vec<usize> = judged.iter().map(|&(s, _)| s).filter(|s| !old.contains(s)).collect();
    println!("  cut positions changed by the judge: {}", added.len());
    for s in &added {
        let t = ms(*s) as f64 / 1000.0;
        println!(
            "    now cuts at {:02}:{:05.2}  ({t:.2}s)  — listen here: is this a voice change?",
            (t as i64) / 60,
            t % 60.0
        );
    }
    Ok(())
}

fn persist_scores(db_path: &str, sims: &[(String, f32, i64)]) -> Result<(), String> {
    let db = Database::open(db_path).map_err(|e| format!("open {db_path} read-write: {e}"))?;
    let mut written = 0usize;
    for (id, sim, _) in sims {
        db.set_speaker_change_score(id, *sim as f64)
            .map_err(|e| format!("persist {id} (is the library at Migration v47?): {e}"))?;
        written += 1;
    }
    let flagged = sims.iter().filter(|(_, s, _)| *s < SPEAKER_CHANGE_THRESHOLD).count();
    println!("\npersisted:           {written} scores ({flagged} below {SPEAKER_CHANGE_THRESHOLD}, i.e. flagged)");
    Ok(())
}

fn report_against_ground_truth(within: &[f32], sims: &[(String, f32, i64)], segments: &[SegRow]) {
    // (stored speaker label, still awaiting review) for a measured clip.
    let meta = |id: &str| {
        segments.iter().find(|s| s.0 == id).map_or(("?", false), |(_, _, _, _, spk, ver)| (spk.as_str(), *ver == 0))
    };
    let multi_max = GROUND_TRUTH.iter().filter(|(_, a, _)| *a == "multi").map(|(_, _, s)| *s).fold(f32::MIN, f32::max);
    let single_min = GROUND_TRUTH.iter().filter(|(_, a, _)| *a != "multi").map(|(_, _, s)| *s).fold(f32::MAX, f32::min);
    println!("\nHISTORICAL CALIBRATION (owner's blind listening pass, {} clips)", GROUND_TRUTH.len());
    println!("  These stored calibration labels are not new listening judgments of the current measured set.");
    println!("  turn-taking clips   max similarity {multi_max:.3}");
    println!("  single-speaker      min similarity {single_min:.3}");
    println!("  separation gap      {:.3} wide, threshold {SPEAKER_CHANGE_THRESHOLD}", single_min - multi_max);
    let wrong =
        GROUND_TRUTH.iter().filter(|(_, a, s)| (*s < SPEAKER_CHANGE_THRESHOLD) != (*a == "multi")).collect::<Vec<_>>();
    println!("  misclassified at that threshold: {} / {}", wrong.len(), GROUND_TRUTH.len());
    // Overlap is called out separately because it is NOT caught and must not be counted as a success.
    let overlap = GROUND_TRUTH.iter().filter(|(_, a, _)| *a == "overlap").count();
    println!(
        "  clips with genuine OVERLAP: {overlap} / {} - all scored with the single-speaker group, i.e. \
         invisible to this method",
        GROUND_TRUTH.len()
    );

    let flagged = within.iter().filter(|s| **s < SPEAKER_CHANGE_THRESHOLD).count();
    println!("\n{}", screening_summary(flagged, within.len()));
    println!("(Turn-taking only. Overlap is a separate, unmeasured problem this cannot see.)");
    let mut worst = sims.to_vec();
    worst.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    // EVERY flagged clip, not a top-10 sample. The owner asked which clips these are so he knows before
    // opening one, and a truncated list cannot answer that. `speaker` is printed alongside because it is
    // the field this measurement contradicts: a clip holding a turn between two people still carries one
    // authoritative SPEAKER_xx in the DB and in every export column.
    // CSV-ish and greppable on purpose — this output is the deliverable, not a debug aid.
    println!(
        "\nspeaker-change candidates (cosine < {SPEAKER_CHANGE_THRESHOLD}) — id,cosine,seconds,storedSpeaker,pending"
    );
    for (id, sim, dur) in worst.iter().filter(|(_, s, _)| *s < SPEAKER_CHANGE_THRESHOLD) {
        let (speaker, pending) = meta(id);
        println!(
            "  {id},{sim:.4},{:.1},{speaker},{}",
            *dur as f64 / 1000.0,
            if pending { "pending" } else { "reviewed" }
        );
    }

    // The pending count is the number that decides whether this is worth acting on: a flagged clip the
    // owner has already reviewed cost him the friction once and is done.
    let pending_flagged = worst.iter().filter(|(id, s, _)| *s < SPEAKER_CHANGE_THRESHOLD && meta(id).1).count();
    println!("\nof those, still awaiting review: {pending_flagged}");

    println!("\nlowest within-clip similarity (including clips ABOVE the threshold, for context):");
    for (id, sim, dur) in worst.iter().take(10) {
        println!("  {id}  cosine {sim:.4}  {:.1}s", *dur as f64 / 1000.0);
    }
}

/// Write a stratified 15-clip listening set plus a manifest, for the human ear that has to break the
/// probe's circularity.
///
/// STRATIFIED, not the 15 worst. Taking only the lowest-similarity clips would guarantee an
/// uninformative answer: if they all hold two speakers that is equally consistent with the signal
/// working and with the corpus being full of two-speaker clips at every similarity. Five from the
/// bottom, five around the median and five from the top makes the two outcomes distinguishable.
fn export_listening_set(dir: &str, sims: &[(String, f32, i64)], segments: &[SegRow]) -> Result<(), String> {
    let dir = std::path::Path::new(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let mut sorted = sims.to_vec();
    sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let mut picks: Vec<(usize, &(String, f32, i64), &'static str)> = Vec::new();
    let mid = n / 2;
    for (k, item) in sorted.iter().enumerate().take(5.min(n)) {
        picks.push((k, item, "low"));
    }
    for k in 0..5 {
        let i = (mid + k).min(n - 1);
        picks.push((i, &sorted[i], "mid"));
    }
    for k in 0..5 {
        let i = n.saturating_sub(1 + k);
        picks.push((i, &sorted[i], "high"));
    }
    picks.dedup_by_key(|(i, _, _)| *i);

    let mut manifest = Vec::new();
    for (rank, (id, sim, dur), band) in &picks {
        let Some((_, audio_path, _, alignment_json, speaker, _)) = segments.iter().find(|s| &s.0 == id) else {
            continue;
        };
        let (rate, pcm) = audio::decode_to_pcm(audio_path).map_err(|e| e.to_string())?;
        let (rate, pcm) = audio::ensure_pcm_16khz(rate, pcm).map_err(|e| e.to_string())?;
        let (clip, _) =
            chunking::slice_pcm_by_alignment(&pcm, rate, alignment_json.as_deref()).map_err(|e| e.to_string())?;

        let name = format!("{band}_{rank:03}_{}.wav", &id[..8]);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(dir.join(&name), spec).map_err(|e| e.to_string())?;
        for s in &clip {
            w.write_sample(*s).map_err(|e| e.to_string())?;
        }
        w.finalize().map_err(|e| e.to_string())?;

        manifest.push(serde_json::json!({
            "file": name,
            "id": id,
            "similarity": sim,
            "durationMs": dur,
            "band": band,
            "camppLabel": speaker,
        }));
    }
    let path = dir.join("manifest.json");
    std::fs::write(&path, serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    println!("\nexported {} clips + manifest.json to {}", manifest.len(), dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_invalid_controls_cannot_claim_a_distribution_match() {
        for (same, different) in
            [(vec![0.64], vec![]), (vec![], vec![0.31]), (vec![], vec![]), (vec![f32::NAN], vec![0.31])]
        {
            let report = comparison_summary(&same, &different);
            assert!(report.contains("Comparison unavailable"), "{report}");
            assert!(report.contains("No speaker-separation conclusion"), "{report}");
            assert!(!report.contains("NaN"), "missing evidence is named, not printed as a numeric result");
        }
        let report = comparison_summary(&[0.7, 0.9, 0.8], &[0.4, 0.2, 0.3]);
        assert!(report.contains("0.500"), "the reported descriptive difference must use actual medians: {report}");
        assert!(report.contains("not independent human labels"), "{report}");
        let even = comparison_summary(&[0.9, 0.7], &[0.3, 0.4]);
        assert!(even.contains("0.400"), "summary must match distribution-table percentiles: {even}");
    }

    #[test]
    fn screening_reports_only_candidates_in_the_measured_population() {
        let report = screening_summary(8, 48);
        assert!(report.contains("8 / 48 clips (16.7%)"), "{report}");
        assert!(report.contains("not confirmed changes"), "{report}");
        assert!(report.contains("not unsampled clips"), "{report}");
        assert!(report.contains("Simultaneous overlap is not detected"), "{report}");
        let empty = screening_summary(0, 0);
        assert!(empty.contains("unknown"), "empty measurements cannot certify a clean corpus: {empty}");
        assert!(!empty.contains("NaN"), "{empty}");
    }
    use cortex_speech_app_lib::db::SpeechSegment;
    use std::collections::HashSet;

    #[test]
    fn cosine_is_exact_on_aligned_and_orthogonal_vectors() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]), Some(1.0));
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), Some(0.0));
        assert_eq!(cosine(&[1.0, 0.0], &[-1.0, 0.0]), Some(-1.0));
    }

    #[test]
    fn cosine_refuses_unusable_pairs_instead_of_guessing() {
        assert_eq!(cosine(&[], &[]), None);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), None); // length mismatch
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), None); // zero norm
        assert_eq!(cosine(&[f32::INFINITY], &[f32::INFINITY]), None); // non-finite dot
    }

    #[test]
    fn db_path_arg_never_swallows_the_export_directory() {
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // The bug this parse exists to avoid: `--export <dir>` first must not read <dir> as a database.
        assert_eq!(db_path_arg(&a(&["--export", "out"])), None);
        assert_eq!(db_path_arg(&a(&["--export", "out", "lib.db"])), Some("lib.db".into()));
        assert_eq!(db_path_arg(&a(&["lib.db", "--export", "out"])), Some("lib.db".into()));
        assert_eq!(db_path_arg(&a(&["--persist", "lib.db"])), Some("lib.db".into()));
        assert_eq!(db_path_arg(&a(&["--persist"])), None);
        assert_eq!(db_path_arg(&[]), None);
    }

    #[test]
    fn ground_truth_gap_brackets_the_shipped_threshold() {
        let multi_max =
            GROUND_TRUTH.iter().filter(|(_, a, _)| *a == "multi").map(|(_, _, s)| *s).fold(f32::MIN, f32::max);
        let single_min =
            GROUND_TRUTH.iter().filter(|(_, a, _)| *a != "multi").map(|(_, _, s)| *s).fold(f32::MAX, f32::min);
        assert!(multi_max < single_min, "the blind pass separated perfectly; the recorded scores must keep that gap");
        // The shipped threshold must sit inside the measured gap: every turn-taking clip below it,
        // every single-speaker (and overlap — structurally invisible) clip at or above it.
        assert!(multi_max < SPEAKER_CHANGE_THRESHOLD && SPEAKER_CHANGE_THRESHOLD <= single_min);
        for (id, answer, score) in GROUND_TRUTH {
            assert_eq!(*score < SPEAKER_CHANGE_THRESHOLD, *answer == "multi", "clip {id} flips at the threshold");
        }
    }

    fn seeded_db(dir: &std::path::Path, ids: &[&str]) -> String {
        let path = dir.join("probe-test.db");
        let db = Database::open(path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        for id in ids {
            db.insert_segment(&SpeechSegment {
                id: id.to_string(),
                audio_path: format!("{id}.wav"),
                raw_transcript: "test".to_string(),
                duration_ms: 1000,
                ..SpeechSegment::default()
            })
            .unwrap();
        }
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn persist_scores_stores_every_measured_score_not_only_flagged_ones() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = seeded_db(dir.path(), &["seg-flagged", "seg-single"]);
        persist_scores(&db_path, &[("seg-flagged".into(), 0.25, 4_000), ("seg-single".into(), 0.75, 6_000)]).unwrap();
        let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let score = |id: &str| -> f64 {
            conn.query_row("SELECT speaker_change_score FROM speech_segments WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        // Both rows get a score — NULL must keep meaning "not measured", never "measured, one speaker".
        assert_eq!(score("seg-flagged"), 0.25);
        assert_eq!(score("seg-single"), 0.75);
    }

    #[test]
    fn persist_scores_refuses_a_library_without_the_score_column() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("pre-v47.db");
        // A library predating Migration v47 has no speaker_change_score column; persist must name the
        // missing migration instead of quietly migrating the owner's schema.
        Connection::open(&db_path).unwrap().execute("CREATE TABLE speech_segments(id TEXT PRIMARY KEY)", []).unwrap();
        let err = persist_scores(db_path.to_str().unwrap(), &[("seg-a".into(), 0.5, 3_000)]).unwrap_err();
        assert!(err.contains("Migration v47"), "error must name the missing migration: {err}");
    }

    fn write_wav(path: &std::path::Path, samples: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..samples {
            w.write_sample(((i % 97) as i16) * 64).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn export_listening_set_is_stratified_across_the_similarity_range() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("episode.wav");
        write_wav(&wav, 16_000);
        let wav = wav.to_string_lossy().into_owned();
        let ids = ["clip-low-aaaa", "clip-mid-bbbb", "clip-high-cccc"];
        let segments: Vec<SegRow> =
            ids.iter().map(|id| (id.to_string(), wav.clone(), 1_000, None, "SPEAKER_00".to_string(), 0)).collect();
        let sims = vec![
            (ids[0].to_string(), 0.20, 1_000),
            (ids[1].to_string(), 0.55, 1_000),
            (ids[2].to_string(), 0.90, 1_000),
        ];
        let out = dir.path().join("listen");
        export_listening_set(out.to_str().unwrap(), &sims, &segments).unwrap();

        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
        let entries = manifest.as_array().unwrap();
        assert!(entries.len() >= 3);
        let bands: HashSet<&str> = entries.iter().map(|e| e["band"].as_str().unwrap()).collect();
        assert_eq!(
            bands,
            HashSet::from(["low", "mid", "high"]),
            "the sample must span the range, not just the worst clips"
        );
        for e in entries {
            assert!(out.join(e["file"].as_str().unwrap()).is_file(), "manifest names a WAV that was not written");
            assert!(ids.contains(&e["id"].as_str().unwrap()));
        }
        let manifest_ids: Vec<&str> = entries.iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert!(manifest_ids.contains(&ids[0]), "the lowest-similarity clip anchors the low band");
        assert!(manifest_ids.contains(&ids[2]), "the highest-similarity clip anchors the high band");
    }

    #[test]
    fn ground_truth_report_handles_flagged_pending_reviewed_and_unknown_clips() {
        let seg = |id: &str, verified: i64| -> SegRow {
            (id.to_string(), "episode.wav".to_string(), 4_000, None, "SPEAKER_01".to_string(), verified)
        };
        let segments = vec![seg("clip-pending1", 0), seg("clip-reviewed", 1)];
        let sims = vec![
            ("clip-pending1".to_string(), 0.30, 4_000),
            ("clip-reviewed".to_string(), 0.35, 4_000),
            // Measured but absent from `segments`: the meta lookup must fall back, not panic.
            ("clip-unknown1".to_string(), 0.80, 4_000),
        ];
        let within: Vec<f32> = sims.iter().map(|(_, s, _)| *s).collect();
        report_against_ground_truth(&within, &sims, &segments);
    }
}
