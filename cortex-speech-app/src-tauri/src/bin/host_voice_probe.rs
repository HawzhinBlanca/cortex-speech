//! Find the one voice that appears in EVERY episode of a podcast — the host — without trusting the
//! per-file `SPEAKER_xx` labels, which name nobody (the same person is SPEAKER_05 in one episode and
//! SPEAKER_04 in the next, measured 2026-08-19 across 32 episode files).
//!
//! Owner goal: collect one podcast host's voice as a clean single-speaker set for voice cloning, and
//! point reviewers ONLY at those clips so their hours build that set. Guests stay in the library
//! untouched for a later phase. Nothing here writes to the database.
//!
//! Method, and why it is this and not something cleverer:
//!   1. embed every single-speaker clip of the target source with the production CAM++ service (the
//!      same model and vectors the app's speaker-change score already trusts);
//!   2. greedy cosine clustering across ALL files at once, so one physical voice gets one cluster;
//!   3. rank clusters by the number of DISTINCT FILES they appear in — the host is in every episode,
//!      a guest is in one or two. Volume alone would be wrong: a long-winded guest can out-talk the
//!      host in their own episode; presence-across-files cannot be faked by one episode;
//!   4. write the candidate list and a BLIND sample for the owner to judge by ear.
//!
//! The owner decides which cluster is the host. This tool never names a person: the cluster id is an
//! integer, the output lives in the data dir, and the name the owner chooses goes into
//! `voice_focus.json` there — never into tracked code.
//!
//! Usage:
//!   host_voice_probe --source-like "%_batch_%" [--data-dir DIR] [--threshold 0.62] [--sample 15]
//!   host_voice_probe --source-like "%_batch_%" --round2 10,17     # judge SPECIFIC clusters
//!
//! `--round2 A,B,...` samples from the named clusters (plus a control draw from the top cluster),
//! writes the key with the CLUSTER per clip rather than candidate/other, and writes each named
//! cluster's ids to its own file — so the owner can confirm or reject each suspect independently
//! and a confirmed one can be MERGED into the focus without re-judging the host. Output goes to
//! `voice_focus/round2/`; round-1 files are never overwritten.
//!
//! `--judge-new` (requires an ACTIVE voice_focus.json): re-clusters everything the filter matches,
//! finds every cluster whose membership is MAJORITY already-confirmed ids (that voice, possibly
//! split across mic days), and samples the ids in those clusters that are NOT yet in the focus —
//! one synthetic suspect group `cluster:new` — plus controls from the confirmed set. Judged and
//! merged with the same `--merge-round2`, since the key and files have the same shape.

use cortex_speech_app_lib::diarization::{SpeakerEmbeddingService, SPEAKER_CHANGE_THRESHOLD};
use cortex_speech_app_lib::{audio, chunking, models};
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// CAM++ needs enough audio for a stable embedding.
const MIN_CLIP_MS: i64 = 2000;

type Row = (String, String, i64, Option<String>);

fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na * nb))
}

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

/// Deterministic in-place shuffle (LCG) — reproducible blind samples, no rand dependency.
fn shuffle<T>(v: &mut [T], mut seed: u64) {
    for i in (1..v.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let j = (seed >> 33) as usize % (i + 1);
        v.swap(i, j);
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data_dir: PathBuf = arg(&args, "--data-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("cortex-speech"));
    let source_like = arg(&args, "--source-like").ok_or("--source-like is required (SQL LIKE on audio_path)")?;
    let threshold: f32 = arg(&args, "--threshold").and_then(|v| v.parse().ok()).unwrap_or(0.62);
    let sample_n: usize = arg(&args, "--sample").and_then(|v| v.parse().ok()).unwrap_or(15);
    let judge_new = args.iter().any(|a| a == "--judge-new");
    let round2: Option<Vec<usize>> =
        arg(&args, "--round2").map(|v| v.split(',').filter_map(|t| t.trim().parse().ok()).collect());

    let db_path = data_dir.join("cortex-speech.db");
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    println!("db        : {}", db_path.display());
    println!("source    : audio_path LIKE {source_like}");
    println!("threshold : cosine >= {threshold} joins a cluster");

    let campp = models::resolve_model_file(models::CAMPP_MODEL);
    let models_dir = campp
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| format!("cannot derive a models root from {}", campp.display()))?;
    let embed = SpeakerEmbeddingService::new(models_dir);
    if !embed.is_available() {
        return Err("CAM++ is not present — nothing can be measured without it".into());
    }

    // Single-speaker clips ONLY (the same bar the phone badge and the export use): a clip holding two
    // voices would give an embedding of neither and could seed a phantom cluster.
    let mut stmt = conn
        .prepare(
            "SELECT id, audio_path, duration_ms, alignment_json FROM speech_segments
             WHERE audio_path LIKE ?1
               AND speaker_change_score IS NOT NULL AND speaker_change_score >= ?2
               AND duration_ms >= ?3
             ORDER BY audio_path, id",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<Row> = stmt
        .query_map(rusqlite::params![source_like, SPEAKER_CHANGE_THRESHOLD as f64, MIN_CLIP_MS], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    let files: HashSet<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    println!("clips     : {} single-speaker, >= {MIN_CLIP_MS} ms, across {} file(s)\n", rows.len(), files.len());
    if rows.is_empty() {
        return Err("no clips matched — check --source-like".into());
    }

    // One decode per source file (the speaker probe's lesson: per-clip decode died on the full library).
    let mut cached: Option<(String, u32, Vec<i16>)> = None;
    let mut embedded: Vec<(String, String, i64, Vec<f32>)> = Vec::new();
    // Kept so the blind sample can be cut from the SAME PCM that was embedded — what the owner
    // hears is exactly what the model judged, with no second decode pass.
    let mut pcm_by_id: HashMap<String, (u32, Vec<i16>)> = HashMap::new();
    let mut skipped = 0usize;
    for (done, (id, path, dur, align)) in rows.iter().enumerate() {
        if done % 500 == 0 && done > 0 {
            println!("  ...{done} of {} clips embedded", rows.len());
        }
        if !cached.as_ref().is_some_and(|(p, _, _)| p == path) {
            let Ok((raw_rate, raw_pcm)) = audio::decode_to_pcm(path) else {
                skipped += 1;
                cached = None;
                continue;
            };
            let Ok((rate, pcm)) = audio::ensure_pcm_16khz(raw_rate, raw_pcm) else {
                skipped += 1;
                cached = None;
                continue;
            };
            cached = Some((path.clone(), rate, pcm));
        }
        let Some((_, rate, pcm)) = cached.as_ref() else {
            skipped += 1;
            continue;
        };
        let Ok((clip, _)) = chunking::slice_pcm_by_alignment(pcm, *rate, align.as_deref()) else {
            skipped += 1;
            continue;
        };
        let f32s: Vec<f32> = clip.iter().map(|&v| v as f32 / 32768.0).collect();
        let vec = embed.compute_embedding(&f32s, *rate);
        if vec.is_empty() {
            skipped += 1;
            continue;
        }
        pcm_by_id.insert(id.clone(), (*rate, clip.to_vec()));
        embedded.push((id.clone(), path.clone(), *dur, vec));
    }
    println!("embedded  : {}   skipped: {skipped}\n", embedded.len());
    if embedded.is_empty() {
        return Err("nothing embedded — CAM++ returned empty vectors for every clip".into());
    }

    // Greedy online clustering with running centroids — deterministic, and k stays small because
    // rows are ordered by file and each file has few speakers.
    let mut centroids: Vec<Vec<f32>> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    let mut assign: Vec<usize> = Vec::with_capacity(embedded.len());
    for (_, _, _, vec) in &embedded {
        let best = centroids
            .iter()
            .enumerate()
            .filter_map(|(i, c)| cosine(c, vec).map(|s| (i, s)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((i, sim)) if sim >= threshold => {
                let n = counts[i] as f32;
                for (c, v) in centroids[i].iter_mut().zip(vec) {
                    *c = (*c * n + v) / (n + 1.0);
                }
                counts[i] += 1;
                assign.push(i);
            }
            _ => {
                centroids.push(vec.clone());
                counts.push(1);
                assign.push(centroids.len() - 1);
            }
        }
    }

    // Rank by DISTINCT FILES present, then by total audio.
    let mut per_cluster: HashMap<usize, (HashSet<&str>, i64, usize)> = HashMap::new();
    for ((_, path, dur, _), &c) in embedded.iter().zip(&assign) {
        let e = per_cluster.entry(c).or_insert_with(|| (HashSet::new(), 0, 0));
        e.0.insert(path.as_str());
        e.1 += dur;
        e.2 += 1;
    }
    let mut ranked: Vec<(usize, usize, i64, usize)> =
        per_cluster.iter().map(|(&c, (f, ms, n))| (c, f.len(), *ms, *n)).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));

    println!("clusters  : {}   (top 10 by files-present, then audio)", ranked.len());
    println!("  cluster  files  audio_h   clips");
    for (c, f, ms, n) in ranked.iter().take(10) {
        println!("  {c:>7}  {f:>5}  {:>7.2}  {n:>6}", *ms as f64 / 3_600_000.0);
    }
    let total_files = files.len();
    let (host_c, host_f, host_ms, host_n) = ranked[0];
    println!(
        "\ncandidate host: cluster {host_c} — present in {host_f}/{total_files} files, {:.2} h, {host_n} clips",
        host_ms as f64 / 3_600_000.0
    );
    if host_f + 2 < total_files {
        println!(
            "  WARNING: the top cluster misses {} file(s); the threshold may be splitting one voice.",
            total_files - host_f
        );
    }

    // Blind sample: ids only, no cluster hint, so the owner's ear is tested rather than led.
    //
    // Round 1 (default): two-thirds from the candidate-host cluster, one-third from everything else;
    // the key says CANDIDATE / other. Round 2 (`--round2 A,B`): draws from each NAMED cluster plus a
    // control draw from the confirmed host cluster; the key names the cluster per clip, so each
    // suspect can be confirmed or rejected on its own.
    let is_round2 = round2.is_some() || judge_new;
    let out_dir = if is_round2 { data_dir.join("voice_focus").join("round2") } else { data_dir.join("voice_focus") };
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let ids_of = |cluster: usize| -> Vec<&str> {
        embedded.iter().zip(&assign).filter(|(_, &c)| c == cluster).map(|((id, ..), _)| id.as_str()).collect()
    };
    let mut candidate_ids: Vec<&str> = ids_of(host_c);
    shuffle(&mut candidate_ids, 20_260_819);

    // (id, label): "CANDIDATE"/"other" in round 1, "cluster:N" in round 2.
    let mut sample: Vec<(&str, String)> = Vec::new();
    if judge_new {
        // The confirmed set is the ground truth this mode extends.
        let focus_text = std::fs::read_to_string(data_dir.join("voice_focus.json"))
            .map_err(|e| format!("--judge-new needs an active voice_focus.json: {e}"))?;
        let focus: std::collections::HashSet<String> = serde_json::from_str::<serde_json::Value>(&focus_text)
            .ok()
            .and_then(|v| {
                v.get("segment_ids")
                    .and_then(|a| a.as_array())
                    .map(|items| items.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            })
            .ok_or("voice_focus.json holds no segment_ids")?;

        // A cluster is "that voice" when confirmed ids are its MAJORITY (and at least 3 of them —
        // one stray confirmed clip inside a big foreign cluster proves nothing). New ids inside
        // those clusters clustered WITH him; everything else stays out and is reported for --round2.
        let mut new_ids: Vec<&str> = Vec::new();
        let mut him_clusters: Vec<usize> = Vec::new();
        for (c, _, _, _) in &ranked {
            let members: Vec<&str> =
                embedded.iter().zip(&assign).filter(|(_, &a)| a == *c).map(|((id, ..), _)| id.as_str()).collect();
            let confirmed = members.iter().filter(|id| focus.contains(**id)).count();
            if confirmed >= 3 && confirmed * 2 > members.len() {
                him_clusters.push(*c);
                new_ids.extend(members.into_iter().filter(|id| !focus.contains(*id)));
            }
        }
        println!(
            "judge-new: {} confirmed-majority cluster(s) {:?}; {} NEW id(s) clustered with the confirmed voice",
            him_clusters.len(),
            him_clusters,
            new_ids.len()
        );
        for (c, f, ms, n) in ranked.iter().take(10) {
            if !him_clusters.contains(c) && *n >= 50 {
                let has_any = embedded.iter().zip(&assign).any(|((id, ..), &a)| a == *c && focus.contains(id.as_str()));
                if !has_any {
                    println!(
                        "  note: cluster {c} ({f} files, {:.2} h, {n} clips) holds NO confirmed id — if it might be him, judge it via --round2 {c}",
                        *ms as f64 / 3_600_000.0
                    );
                }
            }
        }
        if new_ids.is_empty() {
            return Err("no new ids clustered with the confirmed voice — nothing to judge".into());
        }
        shuffle(&mut new_ids, 20_261_000);
        let controls = (sample_n / 4).max(2);
        let take_new = sample_n.saturating_sub(controls).min(new_ids.len());
        sample.extend(new_ids[..take_new].iter().map(|&i| (i, "cluster:new".to_string())));
        let mut confirmed_ids: Vec<&str> =
            embedded.iter().filter(|(id, ..)| focus.contains(id)).map(|(id, ..)| id.as_str()).collect();
        shuffle(&mut confirmed_ids, 20_261_001);
        let take_ctl = controls.min(confirmed_ids.len());
        sample.extend(confirmed_ids[..take_ctl].iter().map(|&i| (i, "cluster:confirmed".to_string())));
        std::fs::write(
            out_dir.join("cluster_new_segment_ids.txt"),
            new_ids.iter().map(|id| format!("{id}\n")).collect::<String>(),
        )
        .map_err(|e| e.to_string())?;
    } else {
        match &round2 {
            None => {
                let mut other_ids: Vec<&str> = embedded
                    .iter()
                    .zip(&assign)
                    .filter(|(_, &c)| c != host_c)
                    .map(|((id, ..), _)| id.as_str())
                    .collect();
                shuffle(&mut other_ids, 20_260_820);
                let take_c = (sample_n * 2 / 3).min(candidate_ids.len());
                let take_o = (sample_n - take_c).min(other_ids.len());
                sample.extend(candidate_ids[..take_c].iter().map(|&i| (i, "CANDIDATE".to_string())));
                sample.extend(other_ids[..take_o].iter().map(|&i| (i, "other".to_string())));
            }
            Some(suspects) => {
                // Even split across the suspects, plus ~a quarter of the sample as host CONTROLS: if the
                // owner calls a control clip "not him", the ear is off today and the round is void.
                let controls = (sample_n / 4).max(2);
                let per = ((sample_n.saturating_sub(controls)) / suspects.len().max(1)).max(1);
                for (k, &c) in suspects.iter().enumerate() {
                    let mut ids = ids_of(c);
                    shuffle(&mut ids, 20_260_900 + k as u64);
                    let n = per.min(ids.len());
                    println!("round2: cluster {c} has {} clips, sampling {n}", ids.len());
                    sample.extend(ids[..n].iter().map(|&i| (i, format!("cluster:{c}"))));
                    std::fs::write(
                        out_dir.join(format!("cluster_{c}_segment_ids.txt")),
                        ids.iter().map(|id| format!("{id}\n")).collect::<String>(),
                    )
                    .map_err(|e| e.to_string())?;
                }
                let take_ctl = controls.min(candidate_ids.len());
                sample.extend(candidate_ids[..take_ctl].iter().map(|&i| (i, format!("cluster:{host_c}"))));
            }
        }
    }
    shuffle(&mut sample, 7);

    let blind = out_dir.join("blind_sample.txt");
    let key = out_dir.join("blind_sample_KEY.txt");
    std::fs::write(&blind, sample.iter().map(|(id, _)| format!("{id}\n")).collect::<String>())
        .map_err(|e| e.to_string())?;
    std::fs::write(&key, sample.iter().map(|(id, label)| format!("{id}\t{label}\n")).collect::<String>())
        .map_err(|e| e.to_string())?;
    if !is_round2 {
        std::fs::write(
            out_dir.join("candidate_segment_ids.txt"),
            candidate_ids.iter().map(|id| format!("{id}\n")).collect::<String>(),
        )
        .map_err(|e| e.to_string())?;
    }

    // The blind sample as playable WAVs, numbered in judging order. 16 kHz mono PCM, the clip only.
    let sample_dir = out_dir.join("blind_sample");
    std::fs::create_dir_all(&sample_dir).map_err(|e| e.to_string())?;
    for (n, (id, _)) in sample.iter().enumerate() {
        let Some((rate, pcm)) = pcm_by_id.get(*id) else { continue };
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: *rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let path = sample_dir.join(format!("{:02}.wav", n + 1));
        let mut w = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
        for &v in pcm {
            w.write_sample(v).map_err(|e| e.to_string())?;
        }
        w.finalize().map_err(|e| e.to_string())?;
    }

    println!("\nwrote:");
    println!(
        "  {}   <- 01.wav .. {:02}.wav, the sample as audio, in judging order",
        sample_dir.display(),
        sample.len()
    );
    println!("  {}   <- {} ids to judge by ear, in order", blind.display(), sample.len());
    println!("  {}   <- DO NOT open until judged", key.display());
    if is_round2 {
        println!("  {}   <- one cluster_N_segment_ids.txt per suspect cluster", out_dir.display());
    } else {
        println!(
            "  {}   <- all {} candidate-host clip ids",
            out_dir.join("candidate_segment_ids.txt").display(),
            candidate_ids.len()
        );
    }
    Ok(())
}
