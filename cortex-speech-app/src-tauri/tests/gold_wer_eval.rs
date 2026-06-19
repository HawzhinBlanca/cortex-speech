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
use cortex_speech_app_lib::settings::AsrModelSize;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::error::AppError;
use cortex_speech_app_lib::eval;
use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::AppSettings;
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
    let model_dir = PathBuf::from(
        std::env::var("CORTEX_MODELS_DIR").unwrap_or_else(|_| "models".to_string()),
    );
    let max: usize = std::env::var("CORTEX_GOLD_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);

    if !model_dir.join("omniasr-ctc-300m/model.int8.onnx").exists() {
        eprintln!(
            "[gold-wer] skip: omniasr-ctc-300m model not found under {}",
            model_dir.display()
        );
        return;
    }

    let content = std::fs::read_to_string(&manifest).expect("read manifest");
    let mut rows: Vec<GoldRow> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("parse jsonl row"))
        .collect();
    rows.truncate(max);
    eprintln!(
        "[gold-wer] {} gold clips from manifest; model_dir={}",
        rows.len(),
        model_dir.display()
    );

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
        asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
            .map(|(text, _conf)| text)
            .map_err(AppError::Other)
    })
    .expect("gold eval with real ASR");
    let elapsed = t0.elapsed().as_secs_f64();

    let meta = result.run.meta_json.as_deref().unwrap_or("{}");
    eprintln!("\n========== GOLD WER/CER — real OmniASR-300M ==========");
    eprintln!("clips scored : {}", result.run.num_segs);
    eprintln!(
        "MICRO  WER   : {:.4}    CER : {:.4}    (corpus-level headline)",
        result.run.wer, result.run.cer
    );
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
        if union > 0.0 { inter / union } else { 0.0 }
    };

    let mut scored: Vec<(f64, &eval::EvalSegmentResult)> = result
        .segments
        .iter()
        .map(|s| (jaccard(&s.reference, &s.hypothesis), s))
        .collect();
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
    let aligned: Vec<&eval::EvalSegmentResult> = scored
        .iter()
        .filter(|(ov, _)| *ov >= ALIGN_THRESHOLD)
        .map(|(_, s)| *s)
        .collect();
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
    let model_dir = PathBuf::from(
        std::env::var("CORTEX_MODELS_DIR").unwrap_or_else(|_| "models".to_string()),
    );
    let max: usize = std::env::var("CORTEX_GOLD_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
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
        &AsrLoadConfig {
            model_size: AsrModelSize::CTC1B,
            enable_gpu: false,
            ..AsrLoadConfig::default()
        },
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
    let t0 = std::time::Instant::now();
    for r in rows.iter().filter(|r| Path::new(&r.audio_filepath).exists()) {
        let (sr, pcm) = match audio::decode_to_pcm(&r.audio_filepath) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let (_sr, p16) = audio::ensure_pcm_16khz(sr, pcm).expect("16khz");
        let f32_pcm: Vec<f32> = p16.iter().map(|&s| s as f32 / 32768.0).collect();
        let h3 = a300
            .transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
            .map(|(t, _)| t)
            .unwrap_or_default();
        let h1 = a1b
            .transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
            .map(|(t, _)| t)
            .unwrap_or_default();
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
    eprintln!(
        "per-clip: 1B better {wins1b}  |  300M better {wins300}  |  ties {ties}  (of {n})"
    );
    eprintln!("elapsed: {elapsed:.1}s (both models)");
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
    let model_dir = PathBuf::from(
        std::env::var("CORTEX_MODELS_DIR").unwrap_or_else(|_| "models".to_string()),
    );
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
    let (hyp, _conf) = asr
        .transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
        .expect("transcribe whole file");
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
    eprintln!(
        "audio        : {:.1}s   decode+transcribe {:.1}s  ({:.2}x realtime)",
        secs,
        elapsed,
        secs / elapsed
    );
    eprintln!(
        "ref words    : {}    hyp words: {}",
        reference.split_whitespace().count(),
        hyp.split_whitespace().count()
    );
    eprintln!("RAW   WER {:.4}   CER {:.4}", pct(&raw_w), pct(&raw_c));
    eprintln!(
        "NORM  WER {:.4}   CER {:.4}   (SoraniNormalizer on both sides)",
        pct(&n_w),
        pct(&n_c)
    );
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
    let pipeline = ProcessingPipeline::new(
        db_path_str.clone(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(100)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(AppSettings::default()),
        Arc::new(ModelManager::new(tmp.path().to_path_buf())),
    );

    let db = Database::open(&db_path_str).unwrap();
    let t0 = std::time::Instant::now();
    pipeline
        .process_single_file(Path::new(&audio_path), &db)
        .expect("process_single_file should succeed");
    let elapsed = t0.elapsed().as_secs_f64();
    let segments = db.get_segments(None).unwrap();

    let hyp: String = segments
        .iter()
        .map(|s| s.raw_transcript.clone())
        .collect::<Vec<_>>()
        .join(" ");

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
    eprintln!(
        "NORM  WER {:.4}   CER {:.4}   (SoraniNormalizer on both sides)",
        pct(&n_w),
        pct(&n_c)
    );
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
    let pct = |d: &wer::EditDistanceResult| {
        if d.ref_len > 0 {
            (d.distance as f64 / d.ref_len as f64).min(1.0)
        } else {
            0.0
        }
    };
    let (mut wd, mut wr, mut cd, mut cr, mut n) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut nwd, mut nwr, mut ncd, mut ncr) = (0usize, 0usize, 0usize, 0usize);

    eprintln!("\n===== VERBATIM WER — real OmniASR-300M vs hand-verified refs =====");
    for (li, line) in content.lines().enumerate() {
        if li == 0 || line.trim().is_empty() {
            continue; // header / blank
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            continue;
        }
        let asr = cols[3].trim();
        let verbatim = cols[4].trim();
        if verbatim.is_empty() {
            continue; // not yet verified
        }
        let raw_w = wer::word_edit_distance(verbatim, asr);
        let raw_c = wer::char_edit_distance(verbatim, asr);
        let (vn, an) = (norm.normalize(verbatim), norm.normalize(asr));
        let n_w = wer::word_edit_distance(&vn, &an);
        let n_c = wer::char_edit_distance(&vn, &an);
        wd += raw_w.distance;
        wr += raw_w.ref_len;
        cd += raw_c.distance;
        cr += raw_c.ref_len;
        nwd += n_w.distance;
        nwr += n_w.ref_len;
        ncd += n_c.distance;
        ncr += n_c.ref_len;
        n += 1;
        eprintln!(
            "  clip {:>3}  WER {:.2}  CER {:.2}   (norm WER {:.2} CER {:.2})",
            cols[0],
            pct(&raw_w),
            pct(&raw_c),
            pct(&n_w),
            pct(&n_c)
        );
    }
    assert!(n > 0, "no rows with a filled `verbatim` column — fill the worksheet first");
    let micro = |d: usize, r: usize| if r > 0 { (d as f64 / r as f64).min(1.0) } else { 0.0 };
    eprintln!("\n----- {n} hand-verified clips -----");
    eprintln!("RAW   micro WER {:.4}   CER {:.4}", micro(wd, wr), micro(cd, cr));
    eprintln!(
        "NORM  micro WER {:.4}   CER {:.4}   (SoraniNormalizer on both sides)",
        micro(nwd, nwr),
        micro(ncd, ncr)
    );
}
