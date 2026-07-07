// Throwaway harness: real-OmniASR WER/CER over the Halwest gold set
// (audio + human reference text). Honest accuracy from audio, never caller-supplied text.
//
// Run:
//   CORTEX_GOLD_MANIFEST=<path/to/tts_voice_cloning_gold/manifest.jsonl> \
//   CORTEX_MODELS_DIR=<dir containing omniasr-ctc-300m/> \
//   CORTEX_GOLD_MAX=<n>  (optional cap) \
//   cargo test --test gold_wer_eval -- --ignored --nocapture
use cortex_speech_app_lib::asr::{AsrLoadConfig, KurdishAsrService};
use cortex_speech_app_lib::audio;
use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::error::AppError;
use cortex_speech_app_lib::eval;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::{AppSettings, AsrModelSize};
use cortex_speech_app_lib::significance::{bootstrap_ci, mapsswe, SegmentError};
use cortex_speech_app_lib::wer;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

#[derive(serde::Deserialize)]
struct GoldRow {
    audio_filepath: String,
    text: String,
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[test]
#[ignore]
fn gold_wer_real_omniasr() {
    let manifest = match std::env::var("CORTEX_GOLD_MANIFEST") {
        Ok(m) => m,
        Err(_) => {
            eprintln!("[gold-wer] skip: set CORTEX_GOLD_MANIFEST");
            return;
        }
    };
    let model_dir = PathBuf::from(std::env::var("CORTEX_MODELS_DIR").unwrap_or_else(|_| "models".to_string()));
    let max: usize = std::env::var("CORTEX_GOLD_MAX").ok().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);

    if !model_dir.join("omniasr-ctc-300m/model.int8.onnx").exists() {
        eprintln!("[gold-wer] skip: omniasr-ctc-300m model not found under {}", model_dir.display());
        return;
    }

    let content = std::fs::read_to_string(&manifest).expect("read manifest");
    let mut rows: Vec<GoldRow> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("parse jsonl row"))
        .collect();
    rows.truncate(max);
    eprintln!("[gold-wer] {} gold clips from manifest; model_dir={}", rows.len(), model_dir.display());

    let db = Database::open(":memory:").expect("open db");
    db.initialize().expect("init db");

    let inputs: Vec<eval::GoldSegmentInput> = rows
        .iter()
        .filter(|r| Path::new(&r.audio_filepath).exists())
        .map(|r| eval::GoldSegmentInput {
            audio_path: r.audio_filepath.clone(),
            reference: r.text.clone(),
            is_holdout: true,
        })
        .collect();
    let n_in = inputs.len();
    assert!(n_in > 0, "no gold clips with existing audio paths found");
    eval::import_gold_segments(&db, inputs).expect("import gold");
    eprintln!("[gold-wer] imported {n_in} clips with valid audio paths");

    let mut asr = KurdishAsrService::new(&model_dir, false).expect("ASR init with real models");
    assert!(asr.is_available(), "ASR should be available");

    let t0 = std::time::Instant::now();
    let result = eval::run_gold_eval_with_transcriber(&db, "omniasr-ctc-300m", |seg| {
        let (sr, pcm) = audio::decode_to_pcm(&seg.audio_path)?;
        let (_sr, pcm16) = audio::ensure_pcm_16khz(sr, pcm)?;
        let f32_pcm: Vec<f32> = pcm16.iter().map(|&s| s as f32 / 32768.0).collect();
        asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE).map(|(text, _conf)| text).map_err(AppError::Other)
    })
    .expect("gold eval with real ASR");
    let elapsed = t0.elapsed().as_secs_f64();

    let meta = result.run.meta_json.as_deref().unwrap_or("{}");
    eprintln!("\n========== GOLD WER/CER — real OmniASR-300M ==========");
    eprintln!("clips scored : {}", result.run.num_segs);
    eprintln!("MICRO  WER   : {:.4}    CER : {:.4}    (corpus-level headline)", result.run.wer, result.run.cer);
    eprintln!("micro/macro  : {meta}");
    eprintln!("elapsed      : {elapsed:.1}s for {} clips", result.run.num_segs);

    // Content-word overlap (word Jaccard) is an alignment proxy: it answers "does the
    // clip's audio actually contain the reference's content?" independent of exact WER.
    // High overlap + high CER = aligned but noisy ASR (a real data point). Low overlap =
    // the clip is misaligned (audio says something else) — exclude from the WER estimate.
    let jaccard = |a: &str, b: &str| -> f64 {
        use std::collections::HashSet;
        let sa: HashSet<&str> = a.split_whitespace().filter(|w| w.chars().count() >= 3).collect();
        let sb: HashSet<&str> = b.split_whitespace().filter(|w| w.chars().count() >= 3).collect();
        if sa.is_empty() && sb.is_empty() {
            return 1.0;
        }
        let inter = sa.intersection(&sb).count() as f64;
        let union = sa.union(&sb).count() as f64;
        if union > 0.0 {
            inter / union
        } else {
            0.0
        }
    };

    let mut scored: Vec<(f64, &eval::EvalSegmentResult)> =
        result.segments.iter().map(|s| (jaccard(&s.reference, &s.hypothesis), s)).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    const ALIGN_THRESHOLD: f64 = 0.40;
    eprintln!("\n-- per-clip, sorted by content overlap (alignment proxy) --");
    for (ov, s) in scored.iter() {
        let flag = if *ov >= ALIGN_THRESHOLD { "ALIGNED " } else { "drifted " };
        eprintln!("  [{flag} ov {:.2}] WER {:.2} CER {:.2}", ov, s.wer, s.cer);
        eprintln!("    ref: {}", head(&s.reference, 70));
        eprintln!("    hyp: {}", head(&s.hypothesis, 70));
    }

    // Verified-aligned subset estimate (micro, char + word) over clips above threshold.
    let aligned: Vec<&eval::EvalSegmentResult> =
        scored.iter().filter(|(ov, _)| *ov >= ALIGN_THRESHOLD).map(|(_, s)| *s).collect();
    let mut wd = 0usize;
    let mut wr = 0usize;
    let mut cd = 0usize;
    let mut cr = 0usize;
    for s in &aligned {
        let w = wer::word_edit_distance(&s.reference, &s.hypothesis);
        let c = wer::char_edit_distance(&s.reference, &s.hypothesis);
        wd += w.distance;
        wr += w.ref_len;
        cd += c.distance;
        cr += c.ref_len;
    }
    eprintln!("\n========== VERIFIED-ALIGNED SUBSET (overlap >= {ALIGN_THRESHOLD}) ==========");
    eprintln!("aligned clips: {} / {}", aligned.len(), result.run.num_segs);
    if wr > 0 {
        eprintln!(
            "MICRO  WER {:.4}   CER {:.4}   (over aligned clips only — confirm pairs below)",
            (wd as f64 / wr as f64).min(1.0),
            (cd as f64 / cr as f64).min(1.0)
        );
    }

    // Optional: emit a verbatim-correction worksheet (TSV). The reviewer plays each clip
    // and fills `verbatim` with exactly what is spoken; re-running with CORTEX_VERBATIM_TSV
    // then yields a real, trustworthy WER over hand-verified pairs.
    if let Ok(out) = std::env::var("CORTEX_WORKSHEET_OUT") {
        let clean = |x: &str| x.replace(['\t', '\n', '\r'], " ");
        let mut tsv = String::from("clip\taudio_path\tscript_hint\tasr_300m\tverbatim\n");
        for (i, s) in result.segments.iter().enumerate() {
            tsv.push_str(&format!(
                "{}\t{}\t{}\t{}\t\n",
                i + 1,
                clean(&s.audio_path),
                clean(&s.reference),
                clean(&s.hypothesis)
            ));
        }
        std::fs::write(&out, tsv).expect("write worksheet");
        eprintln!(
            "\n[worksheet] wrote {} rows to {out} — fill the `verbatim` column to build a real WER set",
            result.segments.len()
        );
    }

    assert!(result.run.num_segs > 0, "should score at least one clip");
}

/// P2.4: a bounded, ALWAYS-RUNNING (not `#[ignore]`'d) gold-regression check using the one committed,
/// CC-BY-4.0 real-audio fixture (`tests/fixtures/fleurs_ckb_sample.{wav,txt}` — see ATTRIBUTION.md), so
/// the fine-tuned engine's accuracy is exercised on every `cargo test` / `make ship-check` run, not only
/// on `--ignored` runs that need the owner's private corpus. Model-present-gated: the ~970 MB fine-tuned
/// ONNX is gitignored, so CI (which has no models) skips honestly via an eprintln, exactly like every
/// other real-audio test in this file — this never turns CI red for a missing model.
///
/// HONESTY SCOPING (deliberately narrower than `scorecard::check_gold_regression`): that gate requires a
/// baseline with BOTH WER and CER (`docs/finetuned_scorecard_baseline.json` only ever published CER —
/// see docs/EVAL.md, no fine-tuned WER was ever measured), and its Hoeffding-style bootstrap CI is
/// DEGENERATE at N=1 (resampling one point always redraws that same point, so the CI half-width is
/// exactly zero) — asserting the full dual-metric gate here would either fabricate a WER baseline or
/// wire a hair-trigger, statistically meaningless pass/fail. So this test: (1) actually measures a REAL
/// CER from the REAL fine-tuned model on REAL committed audio via the real `eval`/`scorecard` machinery
/// (not a shortcut `wer::compute_cer` call), (2) prints it honestly labeled as N=1, and (3) asserts only
/// what N=1 CAN honestly support — the pipeline produces a finite, non-degenerate transcript, and it is
/// not a wild regression (a generous ceiling, not a tight bound masquerading as statistical confidence).
#[test]
fn finetuned_gold_regression_on_committed_fleurs_fixture() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let model_dir = manifest_dir.join("models").join("finetuned-mms-ckb");
    let onnx = model_dir.join("model.onnx");
    let vocab = model_dir.join("vocab.json");
    if !onnx.is_file() || !vocab.is_file() {
        eprintln!(
            "[gold-regression] skip: fine-tuned model not found under {} (gitignored; not on CI)",
            model_dir.display()
        );
        return;
    }

    let wav_path = manifest_dir.join("tests/fixtures/fleurs_ckb_sample.wav");
    let txt_path = manifest_dir.join("tests/fixtures/fleurs_ckb_sample.txt");
    let reference = std::fs::read_to_string(&txt_path).expect("read committed FLEURS reference text");

    let db = Database::open(":memory:").expect("open db");
    db.initialize().expect("init db");
    eval::import_gold_segments(
        &db,
        vec![eval::GoldSegmentInput {
            audio_path: wav_path.to_string_lossy().to_string(),
            reference: reference.clone(),
            is_holdout: true,
        }],
    )
    .expect("import the one committed gold row");

    let result = eval::run_gold_eval_with_transcriber(&db, "finetuned-mms-ckb", |seg| {
        let (sr, pcm) = audio::decode_to_pcm(&seg.audio_path)?;
        let (_sr, pcm16) = audio::ensure_pcm_16khz(sr, pcm)?;
        let f32_pcm: Vec<f32> = pcm16.iter().map(|&s| s as f32 / 32768.0).collect();
        cortex_speech_app_lib::wav2vec2_asr::run_wav2vec2(&onnx, &vocab, "ckb", &f32_pcm).map_err(AppError::Other)
    })
    .expect("gold eval with the real fine-tuned model");

    assert_eq!(result.run.num_segs, 1, "the committed fixture is exactly one clip");
    let hyp = result.segments[0].hypothesis.trim();
    assert!(!hyp.is_empty(), "the fine-tuned model must produce a non-empty transcript for real speech");

    // Build a REAL Scorecard via the actual scorecard-building code path (not an ad hoc CER calc), so
    // this test exercises the same machinery the publishable scorecard uses.
    let opts = cortex_speech_app_lib::scorecard::ScorecardOptions::default();
    let candidate = cortex_speech_app_lib::scorecard::build_scorecard(&result, None, opts);

    eprintln!("\n========== FINE-TUNED GOLD REGRESSION (N=1, committed FLEURS fixture) ==========");
    eprintln!("reference : {}", reference.trim());
    eprintln!("hypothesis: {hyp}");
    eprintln!("measured micro CER (this ONE clip): {:.4}", candidate.system.micro_cer);

    // The published baseline (docs/finetuned_scorecard_baseline.json) only ever measured CER (N=900);
    // compare against it HONESTLY labeled as informational at N=1, never asserted as a statistically
    // valid regression verdict (see the doc comment above for why check_gold_regression's dual-metric,
    // CI-band gate cannot be honestly applied here).
    if let Ok(baseline_json) = std::fs::read_to_string(manifest_dir.join("../docs/finetuned_scorecard_baseline.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&baseline_json) {
            if let Some(baseline_cer_pct) = v.get("cer").and_then(|x| x.as_f64()) {
                eprintln!(
                    "published baseline  (N=900): {:.2}% CER — informational only, NOT a valid N=1 vs N=900 \
                     statistical comparison",
                    baseline_cer_pct
                );
            }
        }
    }

    // The one honest, N=1-appropriate assertion: a generous sanity ceiling that would catch a genuine
    // pipeline break (e.g. a corrupt ONNX, wrong vocab, garbage decode) without pretending N=1 precision
    // it does not have. 0.5 is far above the published 21% aggregate — it only trips on real breakage.
    assert!(
        candidate.system.micro_cer < 0.5,
        "fine-tuned CER on the committed fixture is {:.4}, far above the 0.5 sanity ceiling — likely a \
         broken model/decode path, not normal per-clip variance",
        candidate.system.micro_cer
    );
}

// Head-to-head: 300M vs 1B OmniASR on the same clips. Absolute CER is inflated by the
// dataset's alignment caveat, but the RELATIVE comparison (same audio, both models,
// normalized) is valid — a model that consistently lowers CER is genuinely better.
#[test]
#[ignore]
fn compare_300m_vs_1b() {
    let manifest = match std::env::var("CORTEX_GOLD_MANIFEST") {
        Ok(m) => m,
        Err(_) => {
            eprintln!("[cmp] skip: set CORTEX_GOLD_MANIFEST");
            return;
        }
    };
    let model_dir = PathBuf::from(std::env::var("CORTEX_MODELS_DIR").unwrap_or_else(|_| "models".to_string()));
    let max: usize = std::env::var("CORTEX_GOLD_MAX").ok().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    if !model_dir.join("omniasr-ctc-300m/model.int8.onnx").exists()
        || !model_dir.join("omniasr-ctc-1b/model.int8.onnx").exists()
    {
        eprintln!("[cmp] skip: need both omniasr-ctc-300m and omniasr-ctc-1b under model_dir");
        return;
    }

    let content = std::fs::read_to_string(&manifest).expect("read manifest");
    let mut rows: Vec<GoldRow> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("parse jsonl"))
        .collect();
    rows.truncate(max);

    let mut a300 = KurdishAsrService::new(&model_dir, false).expect("300M init");
    let mut a1b = KurdishAsrService::new_with_config(
        &model_dir,
        &AsrLoadConfig { model_size: AsrModelSize::CTC1B, enable_gpu: false, ..AsrLoadConfig::default() },
    )
    .expect("1B init");
    assert!(a300.is_available() && a1b.is_available());

    let norm = SoraniNormalizer::new();
    let pct = |d: &wer::EditDistanceResult| {
        if d.ref_len > 0 {
            (d.distance as f64 / d.ref_len as f64).min(1.0)
        } else {
            0.0
        }
    };
    let (mut c3d, mut c3r, mut c1d, mut c1r) = (0usize, 0usize, 0usize, 0usize);
    let (mut n, mut wins1b, mut wins300, mut ties) = (0usize, 0usize, 0usize, 0usize);
    let mut worksheet = String::from("clip\taudio_path\tscript_hint\tasr_300m\tasr_1b\tverbatim\n");
    let clean = |x: &str| x.replace(['\t', '\n', '\r'], " ");
    let t0 = std::time::Instant::now();
    for r in rows.iter().filter(|r| Path::new(&r.audio_filepath).exists()) {
        let (sr, pcm) = match audio::decode_to_pcm(&r.audio_filepath) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let (_sr, p16) = audio::ensure_pcm_16khz(sr, pcm).expect("16khz");
        let f32_pcm: Vec<f32> = p16.iter().map(|&s| s as f32 / 32768.0).collect();
        let h3 = a300.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE).map(|(t, _)| t).unwrap_or_default();
        let h1 = a1b.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE).map(|(t, _)| t).unwrap_or_default();
        worksheet.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t\n",
            n + 1,
            clean(&r.audio_filepath),
            clean(&r.text),
            clean(&h3),
            clean(&h1)
        ));
        let refn = norm.normalize(&r.text);
        let c3 = wer::char_edit_distance(&refn, &norm.normalize(&h3));
        let c1 = wer::char_edit_distance(&refn, &norm.normalize(&h1));
        c3d += c3.distance;
        c3r += c3.ref_len;
        c1d += c1.distance;
        c1r += c1.ref_len;
        let (p3, p1) = (pct(&c3), pct(&c1));
        if (p1 - p3).abs() < 0.005 {
            ties += 1;
        } else if p1 < p3 {
            wins1b += 1;
        } else {
            wins300 += 1;
        }
        n += 1;
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let micro = |d: usize, r: usize| if r > 0 { (d as f64 / r as f64).min(1.0) } else { 0.0 };
    eprintln!("\n========== 300M vs 1B — normalized CER on {n} gold clips ==========");
    eprintln!("300M  micro CER : {:.4}", micro(c3d, c3r));
    eprintln!("1B    micro CER : {:.4}", micro(c1d, c1r));
    eprintln!("per-clip: 1B better {wins1b}  |  300M better {wins300}  |  ties {ties}  (of {n})");
    eprintln!("elapsed: {elapsed:.1}s (both models)");
    if let Ok(out) = std::env::var("CORTEX_WORKSHEET_OUT") {
        std::fs::write(&out, &worksheet).expect("write worksheet");
        eprintln!("[worksheet] wrote {n} rows (asr_300m + asr_1b) to {out}");
    }
    assert!(n > 0);
}

// Whole-file WER: transcribe a complete recording and score against its complete
// reference transcript. Alignment-free (no clip-boundary problem), so this is the
// honest document-level WER. Reports raw + Sorani-normalized (the ASR emits no
// punctuation/numerals, so normalized isolates real phonetic/word errors).
#[test]
#[ignore]
fn whole_file_wer_real_omniasr() {
    let audio_path = match std::env::var("CORTEX_WHOLE_AUDIO") {
        Ok(a) => a,
        Err(_) => {
            eprintln!("[whole-wer] skip: set CORTEX_WHOLE_AUDIO");
            return;
        }
    };
    let ref_path = std::env::var("CORTEX_WHOLE_REF").expect("set CORTEX_WHOLE_REF");
    let model_dir = PathBuf::from(std::env::var("CORTEX_MODELS_DIR").unwrap_or_else(|_| "models".to_string()));
    if !model_dir.join("omniasr-ctc-300m/model.int8.onnx").exists() {
        eprintln!("[whole-wer] skip: omniasr-ctc-300m model not found");
        return;
    }

    let reference = std::fs::read_to_string(&ref_path).expect("read reference");
    let mut asr = KurdishAsrService::new(&model_dir, false).expect("ASR init");
    assert!(asr.is_available(), "ASR should be available");

    let t0 = std::time::Instant::now();
    let (sr, pcm) = audio::decode_to_pcm(&audio_path).expect("decode source audio");
    let (_sr, pcm16) = audio::ensure_pcm_16khz(sr, pcm).expect("resample 16khz");
    let f32_pcm: Vec<f32> = pcm16.iter().map(|&s| s as f32 / 32768.0).collect();
    let secs = f32_pcm.len() as f64 / 16000.0;
    eprintln!("[whole-wer] decoded {secs:.1}s audio; transcribing whole file…");
    let (hyp, _conf) = asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE).expect("transcribe whole file");
    let elapsed = t0.elapsed().as_secs_f64();

    let norm = SoraniNormalizer::new();
    let ref_n = norm.normalize(&reference);
    let hyp_n = norm.normalize(&hyp);

    let pct = |d: &wer::EditDistanceResult| {
        if d.ref_len > 0 {
            (d.distance as f64 / d.ref_len as f64).min(1.0)
        } else {
            0.0
        }
    };
    let raw_w = wer::word_edit_distance(&reference, &hyp);
    let raw_c = wer::char_edit_distance(&reference, &hyp);
    let n_w = wer::word_edit_distance(&ref_n, &hyp_n);
    let n_c = wer::char_edit_distance(&ref_n, &hyp_n);

    eprintln!("\n========== WHOLE-FILE WER — real OmniASR-300M ==========");
    eprintln!("audio        : {:.1}s   decode+transcribe {:.1}s  ({:.2}x realtime)", secs, elapsed, secs / elapsed);
    eprintln!(
        "ref words    : {}    hyp words: {}",
        reference.split_whitespace().count(),
        hyp.split_whitespace().count()
    );
    eprintln!("RAW   WER {:.4}   CER {:.4}", pct(&raw_w), pct(&raw_c));
    eprintln!("NORM  WER {:.4}   CER {:.4}   (SoraniNormalizer on both sides)", pct(&n_w), pct(&n_c));
    eprintln!("\nref(norm) head: {}", head(&ref_n, 110));
    eprintln!("hyp(norm) head: {}", head(&hyp_n, 110));

    assert!(!hyp.trim().is_empty(), "ASR must produce a non-blank transcript");
}

// The valid whole-file WER: run the real PIPELINE (VAD-chunk -> per-segment ASR),
// concatenate segment hypotheses in chronological order, and score against the full
// reference. This is how the ASR is actually used (short chunks), so it reflects real
// document-level accuracy. Raw + Sorani-normalized.
#[test]
#[ignore]
fn pipeline_whole_file_wer() {
    let audio_path = match std::env::var("CORTEX_WHOLE_AUDIO") {
        Ok(a) => a,
        Err(_) => {
            eprintln!("[pipe-wer] skip: set CORTEX_WHOLE_AUDIO");
            return;
        }
    };
    let ref_path = std::env::var("CORTEX_WHOLE_REF").expect("set CORTEX_WHOLE_REF");
    let reference = std::fs::read_to_string(&ref_path).expect("read reference");

    let tmp = TempDir::new().unwrap();
    let db_path_str = tmp.path().join("pipe_wer.db").to_str().unwrap().to_string();
    let db = Database::open(&db_path_str).unwrap();
    db.initialize().unwrap();
    drop(db);
    // CTC-300M explicitly: default WSL7B fails hard without a client script (F2).
    let settings = AppSettings { asr_model_size: AsrModelSize::CTC300M, ..AppSettings::default() };
    let pipeline = ProcessingPipeline::new(
        db_path_str.clone(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(100)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(settings),
        Arc::new(ModelManager::new(tmp.path().to_path_buf())),
    );

    let db = Database::open(&db_path_str).unwrap();
    let t0 = std::time::Instant::now();
    pipeline.process_single_file(Path::new(&audio_path), &db).expect("process_single_file should succeed");
    let elapsed = t0.elapsed().as_secs_f64();
    let segments = db.get_segments(None).unwrap();

    let hyp: String = segments.iter().map(|s| s.raw_transcript.clone()).collect::<Vec<_>>().join(" ");

    let norm = SoraniNormalizer::new();
    let ref_n = norm.normalize(&reference);
    let hyp_n = norm.normalize(&hyp);

    let pct = |d: &wer::EditDistanceResult| {
        if d.ref_len > 0 {
            (d.distance as f64 / d.ref_len as f64).min(1.0)
        } else {
            0.0
        }
    };
    let raw_w = wer::word_edit_distance(&reference, &hyp);
    let raw_c = wer::char_edit_distance(&reference, &hyp);
    let n_w = wer::word_edit_distance(&ref_n, &hyp_n);
    let n_c = wer::char_edit_distance(&ref_n, &hyp_n);

    eprintln!("\n===== PIPELINE WHOLE-FILE WER — real OmniASR-300M (VAD-chunked) =====");
    eprintln!("segments     : {}    process {:.1}s", segments.len(), elapsed);
    eprintln!(
        "ref words    : {}    hyp words: {}",
        reference.split_whitespace().count(),
        hyp.split_whitespace().count()
    );
    eprintln!("RAW   WER {:.4}   CER {:.4}", pct(&raw_w), pct(&raw_c));
    eprintln!("NORM  WER {:.4}   CER {:.4}   (SoraniNormalizer on both sides)", pct(&n_w), pct(&n_c));
    eprintln!("\nref(norm) head: {}", head(&ref_n, 130));
    eprintln!("hyp(norm) head: {}", head(&hyp_n, 130));

    assert!(!segments.is_empty(), "pipeline should produce segments");
}

// Score the real 300M hypotheses (already in the worksheet) against the hand-verified
// `verbatim` column — the honest, trustworthy WER over confirmed (audio, text) pairs.
// Reuses the stored asr_300m column, so no ASR re-run / models needed.
//   CORTEX_VERBATIM_TSV=<filled worksheet> cargo test --test gold_wer_eval \
//     verbatim_wer_from_worksheet -- --ignored --nocapture
#[test]
#[ignore]
fn verbatim_wer_from_worksheet() {
    let tsv = match std::env::var("CORTEX_VERBATIM_TSV") {
        Ok(t) => t,
        Err(_) => {
            eprintln!("[verbatim] skip: set CORTEX_VERBATIM_TSV to a filled worksheet");
            return;
        }
    };
    let content = std::fs::read_to_string(&tsv).expect("read worksheet");
    let norm = SoraniNormalizer::new();

    // Per-clip SegmentError vectors (word + char) for each model vs the verbatim ref.
    let mut w300: Vec<SegmentError> = Vec::new();
    let mut c300: Vec<SegmentError> = Vec::new();
    let mut w1b: Vec<SegmentError> = Vec::new();
    let mut c1b: Vec<SegmentError> = Vec::new();
    let mut has_1b = false;

    for (li, line) in content.lines().enumerate() {
        if li == 0 || line.trim().is_empty() {
            continue; // header / blank
        }
        let cols: Vec<&str> = line.split('\t').collect();
        // 6-col: clip, audio_path, script_hint, asr_300m, asr_1b, verbatim
        // 5-col (legacy): clip, audio_path, script_hint, asr_300m, verbatim
        let (asr3, asr1, verbatim) = match cols.len() {
            n if n >= 6 => (cols[3].trim(), cols[4].trim(), cols[5].trim()),
            5 => (cols[3].trim(), "", cols[4].trim()),
            _ => continue,
        };
        if verbatim.is_empty() {
            continue; // not yet verified
        }
        let vn = norm.normalize(verbatim);
        let w = wer::word_edit_distance(&vn, &norm.normalize(asr3));
        let c = wer::char_edit_distance(&vn, &norm.normalize(asr3));
        w300.push(SegmentError::new(w.distance as f64, w.ref_len as f64));
        c300.push(SegmentError::new(c.distance as f64, c.ref_len as f64));
        if !asr1.is_empty() {
            has_1b = true;
            let w1 = wer::word_edit_distance(&vn, &norm.normalize(asr1));
            let c1 = wer::char_edit_distance(&vn, &norm.normalize(asr1));
            w1b.push(SegmentError::new(w1.distance as f64, w1.ref_len as f64));
            c1b.push(SegmentError::new(c1.distance as f64, c1.ref_len as f64));
        }
    }
    let n = w300.len();
    assert!(n > 0, "no rows with a filled `verbatim` column — fill the worksheet first");

    const RESAMPLES: usize = 2000;
    const CONF: f64 = 0.95;
    const SEED: u64 = 0xC0FFEE;
    let fmt = |ci: cortex_speech_app_lib::significance::ConfidenceInterval| {
        format!("{:.4} [{:.4}, {:.4}]", ci.point, ci.lower, ci.upper)
    };

    eprintln!("\n===== VERBATIM WER/CER — hand-verified refs (n={n}, normalized, 95% bootstrap CI) =====");
    eprintln!("300M  WER {}", fmt(bootstrap_ci(&w300, RESAMPLES, CONF, SEED)));
    eprintln!("300M  CER {}", fmt(bootstrap_ci(&c300, RESAMPLES, CONF, SEED)));
    if has_1b && w1b.len() == n {
        eprintln!("1B    WER {}", fmt(bootstrap_ci(&w1b, RESAMPLES, CONF, SEED)));
        eprintln!("1B    CER {}", fmt(bootstrap_ci(&c1b, RESAMPLES, CONF, SEED)));
        let p = mapsswe(&w300, &w1b);
        eprintln!(
            "MAPSSWE 300M vs 1B (word): p = {:.4}  ->  {}",
            p,
            if p < 0.05 { "significant difference" } else { "not statistically distinguishable" }
        );
    } else {
        eprintln!("(1B column absent — fill asr_1b to get the 300M-vs-1B significance test)");
    }
}
