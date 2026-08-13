//! Dump ONE local acoustic engine's transcripts for a manifest, in the shared hypothesis-TSV format.
//!
//! WHY. The champion is an LLM-based ASR and was measured (2026-08-13) writing the STANDARD form
//! where all three acoustic engines agreed on the PRONOUNCED one — `سێری` -> `سەیری`, `خۆدە` ->
//! `خۆت`. For a verbatim corpus that matters, and the owner's ear caught it first. Deciding whether
//! those acoustic engines have earned a vote (or only a raised hand) needs their CER on the SAME
//! frozen gold set the champion's 7.02% comes from — and nobody has ever measured them there.
//!
//! Uses the app's own `AsrPool` + `AsrLoadConfig`, i.e. the exact decoder production runs, so the
//! number describes the engine the reviewers would actually be shown. Output is
//! `<wav_path>\t<reference>\t<hypothesis>`, scored by `scripts/scorecard_gemini.py` (which imports
//! the champion's normalization and bootstrap) — one format, one metric, no second opinion.
//!
//! Usage: local_asr_dump <manifest.tsv> <out.tsv> <ctc300m|ctc1b|finetuned> [--limit N]
//!
//! Reads audio only; writes only the TSV. It never touches the app database, so it is safe to run
//! while reviewers work.

use cortex_speech_app_lib::asr::{AsrLoadConfig, AsrPool};
use cortex_speech_app_lib::settings::AsrModelSize;
use cortex_speech_app_lib::{audio, models::ModelManager};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

fn app_data_dir() -> PathBuf {
    std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("cortex-speech")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err("usage: local_asr_dump <manifest.tsv> <out.tsv> <ctc300m|ctc1b|finetuned> [--limit N]".into());
    }
    let (manifest, out_path) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    // AsrModelSize carries no fine-tuned variant: the fine-tuned MMS is reached through its own
    // IPC path, not the CTC pool, so this tool covers the two CTC engines only. Naming the limit
    // beats silently scoring the wrong model under a "finetuned" label.
    let model_size = match args[2].as_str() {
        "ctc300m" => AsrModelSize::CTC300M,
        "ctc1b" => AsrModelSize::CTC1B,
        other => return Err(format!("unknown engine '{other}' (use ctc300m or ctc1b)")),
    };
    let limit: Option<usize> =
        args.iter().position(|a| a == "--limit").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok());

    // Same resolution the app uses, so a user-downloaded model is found exactly as in production.
    let models = ModelManager::new(app_data_dir().join("models"));
    // PER-FILE, matching the production fix: resolved_dir() is all-or-nothing, so a user dir holding
    // some engines orphans a bundled-only one. Measured 2026-08-13 — an EMPTY user-dir
    // omniasr-ctc-1b/ made all 922 clips fail "models missing" beside a 1 GB bundled copy.
    let relative = match args[2].as_str() {
        "ctc1b" => "omniasr-ctc-1b/model.int8.onnx",
        _ => "omniasr-ctc-300m/model.int8.onnx",
    };
    let model_dir = models.resolve_root_for(relative);
    let config = AsrLoadConfig { model_size, num_threads: 8, ..AsrLoadConfig::default() };
    let pool = AsrPool::new();
    pool.warmup(&model_dir, &config)?;

    let mut done: HashSet<String> = HashSet::new();
    if out_path.exists() {
        for line in std::io::BufReader::new(std::fs::File::open(&out_path).map_err(|e| e.to_string())?).lines() {
            if let Some(first) = line.map_err(|e| e.to_string())?.split('\t').next() {
                done.insert(first.to_string());
            }
        }
    }

    let mut rows: Vec<(String, String)> = std::io::BufReader::new(
        std::fs::File::open(&manifest).map_err(|e| format!("open {}: {e}", manifest.display()))?,
    )
    .lines()
    .map_while(Result::ok)
    .filter_map(|l| {
        let mut it = l.trim_end_matches('\r').splitn(3, '\t');
        match (it.next(), it.next()) {
            (Some(p), Some(r)) if !p.is_empty() && !r.trim().is_empty() => Some((p.to_string(), r.to_string())),
            _ => None,
        }
    })
    .collect();
    if let Some(n) = limit {
        rows.truncate(n);
    }

    println!("engine   : {}", args[2]);
    println!("manifest : {} ({} rows, {} already done)", manifest.display(), rows.len(), done.len());

    let mut out = std::fs::OpenOptions::new().create(true).append(true).open(&out_path).map_err(|e| e.to_string())?;
    let (mut ok, mut failed) = (0usize, 0usize);
    let started = std::time::Instant::now();

    for (i, (path, reference)) in rows.iter().enumerate() {
        if done.contains(path) {
            continue;
        }
        let decoded = audio::decode_to_pcm(Path::new(path));
        let (rate, pcm) = match decoded {
            Ok(v) => v,
            Err(e) => {
                eprintln!("  FAILED decode {path}: {e}");
                failed += 1;
                continue;
            }
        };
        let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let result = pool.with_service(&model_dir, &config, |asr| {
            if !asr.is_available() {
                return Err("ASR service unavailable (models missing?)".to_string());
            }
            asr.transcribe(&f32_pcm, rate).map_err(|e| e.to_string())
        });
        match result {
            // An empty transcript is NOT written: scoring "" as a hypothesis would report a
            // fabricated 100%-error clip instead of an honest missing one.
            Ok((text, _, _)) if !text.trim().is_empty() => {
                writeln!(out, "{path}\t{reference}\t{}", text.replace(['\t', '\n', '\r'], " "))
                    .map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                ok += 1;
            }
            Ok(_) => {
                eprintln!("  FAILED {path}: empty transcript");
                failed += 1;
            }
            Err(e) => {
                eprintln!("  FAILED {path}: {e}");
                failed += 1;
            }
        }
        if (i + 1) % 50 == 0 {
            println!("  {}/{} ok={ok} failed={failed} ({:.0}s)", i + 1, rows.len(), started.elapsed().as_secs_f64());
        }
    }

    println!(
        "\ndone in {:.0}s: {ok} transcribed, {failed} failed -> {}",
        started.elapsed().as_secs_f64(),
        out_path.display()
    );
    if failed > 0 {
        println!("WARNING: {failed} clips have NO hypothesis — score only the rows present and report n honestly.");
    }
    Ok(())
}
