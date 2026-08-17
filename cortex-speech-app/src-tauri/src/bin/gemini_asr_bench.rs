//! Measure Gemini 2.5 Pro AS AN ASR ENGINE on the frozen ckb gold set — the measure-first step the
//! dataset blueprint requires before Gemini's opinion may outrank anything in the pipeline.
//!
//! WHY THIS EXISTS. The owner asked whether reviewers should be served a machine-arbitrated blend of
//! the OmniASR-7B champion and Gemini 2.5 Pro. That question has no honest answer without a number:
//! the champion is 7.03% CER on this exact frozen manifest, and NOBODY has measured Gemini's ckb
//! TRANSCRIPTION quality (what was measured, on 492 review clips, is that Gemini as a text REFINER
//! rewrites 11.1% of characters — a different thing entirely). Repo law: any new cloud judge needs a
//! measured ckb CER on the frozen gold set before it may be configured.
//!
//! WHAT IT DOES / DOES NOT DO. It writes ONLY a hypothesis TSV (`path\treference\thypothesis`). It
//! computes no metric: `scripts/scorecard_gemini.py` scores that TSV through the SAME
//! normalization + seed-42 utterance bootstrap as `scorecard_7b.py`, so the two numbers are
//! comparable by construction rather than by hope. Two implementations of one metric is how a
//! comparison starts lying (the mirror-drift class this repo has fixed repeatedly).
//!
//! CREDENTIALS. The key comes from `ApiKeys::load` — the ONE sanctioned loader, which handles both
//! plaintext and the DPAPI `dpapi:` ciphertext. Hand-parsing `secrets.env` here would re-create the
//! regression that once sent DPAPI ciphertext as a bearer token. The key is never printed or logged.
//!
//! Usage (writes nothing to the app database, safe while reviewers work):
//!   cargo run --release --bin gemini_asr_bench -- <manifest.tsv> <out.tsv> [--limit N] [--model M]
//!
//! `--limit N` runs the first N rows — use it for a cheap pilot before spending on the full set.
//! Resumable: rows already present in <out.tsv> are skipped, so an interrupted run continues.

use cortex_speech_app_lib::api_keys::ApiKeys;
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// VERBATIM instruction. The whole point of the measurement is Gemini's transcription under THIS
/// project's law, so the prompt states the law explicitly: exactly what is said, no cleanup, no
/// translation of loanwords, no invented punctuation, no spelling-out of digits. Anything less
/// would measure a Gemini that is solving a different task than the champion.
const PROMPT: &str = "Transcribe this Central Kurdish (Sorani) audio VERBATIM in Kurdish Arabic script.\n\
Write EXACTLY what the speaker says:\n\
- Keep repeated words, stutters and filler words exactly as spoken.\n\
- Keep loanwords (Arabic/Persian/English) in the form the speaker actually used — never replace them with a \"proper\" Kurdish word.\n\
- Write numbers the way the speaker says them.\n\
- Do NOT add punctuation, do NOT correct grammar, do NOT summarize or translate.\n\
Output ONLY the transcript text, with no quotes, labels or explanation.";

fn app_data_dir() -> PathBuf {
    std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("cortex-speech")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The two live routes to a Gemini-class audio model. Both go through an AUDITED client (key in a
/// header, provider errors redacted) — never a request assembled in this binary.
enum Route {
    /// generativelanguage.googleapis.com with the direct GEMINI_API_KEY.
    Direct { key: String, model: String },
    /// OpenRouter's OpenAI-compatible endpoint with OPENROUTER_API_KEY, carrying audio as an
    /// `input_audio` part. MEASURED 2026-08-12: the direct key's billing project returns
    /// "prepayment credits are depleted" on every model while this route works, so it is the one
    /// that can actually produce the number. `google/gemini-2.5-pro` is repo-approved.
    OpenRouter { refiner: cortex_speech_app_lib::llm_refiner::LlmRefiner },
}

impl Route {
    fn transcribe(&self, wav: &Path) -> Result<String, String> {
        let bytes = std::fs::read(wav).map_err(|e| format!("read {}: {e}", wav.display()))?;
        match self {
            Route::Direct { key, model } => {
                cortex_speech_app_lib::gemini_api::transcribe_audio(key, model, &bytes, "audio/wav", PROMPT)
            }
            Route::OpenRouter { refiner } => refiner.transcribe_audio(&bytes, "wav", PROMPT),
        }
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("usage: gemini_asr_bench <manifest.tsv> <out.tsv> [--limit N] [--model M]".into());
    }
    let manifest = PathBuf::from(&args[0]);
    let out_path = PathBuf::from(&args[1]);
    let limit: Option<usize> =
        args.iter().position(|a| a == "--limit").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok());
    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "gemini-2.5-pro".to_string());

    let keys = ApiKeys::load(&app_data_dir());
    let use_openrouter = args.iter().any(|a| a == "--openrouter");
    let route = if use_openrouter {
        let key = keys.openrouter.ok_or("OPENROUTER_API_KEY is not configured (set it in the app's Settings)")?;
        let model = if model == "gemini-2.5-pro" { "google/gemini-2.5-pro".to_string() } else { model.clone() };
        let refiner = cortex_speech_app_lib::llm_refiner::LlmRefiner::for_openrouter(key, model, String::new())
            .ok_or("OpenRouter key is empty")?;
        Route::OpenRouter { refiner }
    } else {
        let key = keys.gemini.ok_or("GEMINI_API_KEY is not configured (set it in the app's Settings)")?;
        Route::Direct { key, model: model.clone() }
    };

    // Resume: skip clips already scored, so an interrupted or rate-limited run continues instead of
    // paying twice for the same audio.
    let mut done: HashSet<String> = HashSet::new();
    if out_path.exists() {
        for line in std::io::BufReader::new(std::fs::File::open(&out_path).map_err(|e| e.to_string())?).lines() {
            if let Some(first) = line.map_err(|e| e.to_string())?.split('\t').next() {
                done.insert(first.to_string());
            }
        }
    }

    let rows: Vec<(String, String)> = std::io::BufReader::new(
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
    let rows: Vec<_> = match limit {
        Some(n) => rows.into_iter().take(n).collect(),
        None => rows,
    };

    println!("route    : {}", if use_openrouter { "openrouter" } else { "direct gemini" });
    println!("model    : {model}");
    println!("manifest : {} ({} rows, {} already done)", manifest.display(), rows.len(), done.len());

    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
        .map_err(|e| format!("open {}: {e}", out_path.display()))?;

    let (mut ok, mut failed) = (0usize, 0usize);
    let started = std::time::Instant::now();
    for (i, (path, reference)) in rows.iter().enumerate() {
        if done.contains(path) {
            continue;
        }
        // Retry only TRANSIENT classes (rate limit / 5xx / timeout). A 401/403/404 is permanent and
        // must stop the run rather than burn the retry budget clip after clip.
        let mut attempt = 0;
        let hypothesis = loop {
            attempt += 1;
            match route.transcribe(Path::new(path)) {
                Ok(text) => break Some(text),
                Err(e) => {
                    // Billing/quota 429s are PERMANENT for this run (measured: "prepayment credits
                    // are depleted" arrives as 429) — retrying burns 7 s per clip and buries the
                    // real cause. Mirrors llm_refiner::is_transient.
                    let el = e.to_ascii_lowercase();
                    if el.contains("credits are depleted")
                        || el.contains("insufficient credit")
                        || el.contains("quota exceeded")
                        || el.contains("exceeded your current quota")
                        || el.contains("billing")
                    {
                        return Err(format!("HARD STOP (billing/quota, no retry can fix it): {e}"));
                    }
                    let transient = e.contains("status 429")
                        || e.contains("status 500")
                        || e.contains("status 502")
                        || e.contains("status 503")
                        || e.contains("timed out")
                        || e.contains("empty transcript");
                    if transient && attempt <= 4 {
                        let backoff = std::time::Duration::from_secs(1 << (attempt - 1));
                        eprintln!("  [{}] retry {attempt} after {e}", &path[path.len().saturating_sub(24)..]);
                        std::thread::sleep(backoff);
                        continue;
                    }
                    if !transient && (e.contains("status 401") || e.contains("status 403")) {
                        return Err(format!("HARD STOP (permanent auth error): {e}"));
                    }
                    eprintln!("  FAILED {path}: {e}");
                    break None;
                }
            }
        };

        match hypothesis {
            Some(text) => {
                // Tabs/newlines would corrupt the TSV the scorer reads; collapse to spaces.
                let flat = text.replace(['\t', '\n', '\r'], " ");
                writeln!(out, "{path}\t{reference}\t{flat}").map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                ok += 1;
            }
            None => failed += 1,
        }
        if (i + 1) % 25 == 0 {
            println!("  {}/{}  ok={ok} failed={failed}  ({:.0}s)", i + 1, rows.len(), started.elapsed().as_secs_f64());
        }
    }

    println!(
        "\ndone in {:.0}s: {ok} transcribed, {failed} failed -> {}",
        started.elapsed().as_secs_f64(),
        out_path.display()
    );
    if failed > 0 {
        // The scorer must never silently average over a partial set: say it here, loudly.
        println!("WARNING: {failed} clips have NO hypothesis. Score only the rows present, and report n honestly.");
    }
    Ok(())
}
