//! Opt-in Wav2Vec2-CTC ASR engine for the fine-tuned Kurdish (Sorani) model, via `ort`.
//!
//! Loads the ONNX exported from the fine-tuned `Wav2Vec2ForCTC` (input `input_values: [1, samples]`,
//! output `logits: [1, frames, 111]` for the ckb head) and decodes it to text, reproducing the HF
//! `Wav2Vec2FeatureExtractor` + `Wav2Vec2CTCTokenizer`:
//!   - feature normalization: zero-mean / unit-variance over the waveform (`do_normalize`, eps 1e-7);
//!   - CTC greedy decode: argmax per frame, collapse repeats, drop `<pad>` (id 0 = the CTC blank),
//!     map ids to chars from `vocab.json["ckb"]`, replace the word-delimiter `|` with a space, and
//!     skip the `<s>`/`</s>`/`<unk>` specials.
//!
//! Verified to reproduce the fine-tuned model's CER (≈18.6% on the N=50 gold sample, matching the
//! transformers/onnxruntime measurement). Additive: the default sherpa-onnx OmniASR path is unchanged.

use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// `<pad>` (id 0) is the CTC blank for this tokenizer.
const PAD_ID: usize = 0;

static SESSION_CACHE: LazyLock<Mutex<Option<(std::path::PathBuf, ort::session::Session)>>> =
    LazyLock::new(|| Mutex::new(None));

// ── P3.4: bundled fine-tuned model integrity ────────────────────────────────────────────────────
// The champion MMS-CTC-ckb model produced the published 21.0% CER; a truncated/incomplete-copy or
// swapped file would silently degrade transcripts (the exact thing the honesty law forbids). These
// pins are for the CURRENT bundled model — UPDATE them when it is retrained/replaced (M5). Computed
// from src-tauri/models/finetuned-mms-ckb/.
// 2026-07-13: re-exported int8 ONNX from the owner's MMS_CTC_1B_Champion checkpoint (the original
// model.onnx was not on disk; the checkpoint + matching vocab.json SHA were). fp32 ONNX decode
// verified bit-for-bit against transformers; the vocab pin is unchanged (same vocab.json).
pub const FINETUNED_MODEL_SHA256: &str = "064d6ec2225500cf7d47d267402f50c7c7da4d29f34d0fb6a9cb77272aef5ae0";
pub const FINETUNED_MODEL_BYTES: u64 = 970_236_520;
pub const FINETUNED_VOCAB_SHA256: &str = "31dcd5c4361451991bd8241eb99bdc822d2ef2d8a4906404884c2196aa8f3a41";

/// True when `onnx_path` is the bundled fine-tuned model (`…/finetuned-mms-ckb/model.onnx`). A custom
/// `CORTEX_FINETUNED_ONNX` or the alignment model is not this file and is not pinned.
fn is_bundled_finetuned(onnx_path: &Path) -> bool {
    onnx_path.file_name().and_then(|n| n.to_str()) == Some("model.onnx")
        && onnx_path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) == Some("finetuned-mms-ckb")
}

/// Verify a model/vocab pair against an expected model byte-size + vocab SHA. model.onnx gets only the
/// (instant) size check here — it catches a truncated/incomplete copy, the realistic corruption — while
/// the small vocab gets a full SHA. Extracted so the pass/fail logic is unit-testable without the 970 MB
/// model. The definitive full model SHA is `verify_finetuned_full` (on-demand).
fn verify_integrity_against(
    onnx_path: &Path,
    expected_bytes: u64,
    vocab_path: &Path,
    expected_vocab_sha: &str,
) -> Result<(), String> {
    let size = std::fs::metadata(onnx_path).map_err(|e| format!("stat model.onnx: {e}"))?.len();
    if size != expected_bytes {
        return Err(format!(
            "fine-tuned model.onnx integrity: expected {expected_bytes} bytes, found {size} — the file is truncated or replaced; re-install the model."
        ));
    }
    let vocab_sha = crate::models::compute_file_sha256(vocab_path)?;
    if vocab_sha != expected_vocab_sha {
        return Err(format!(
            "fine-tuned vocab.json integrity: SHA mismatch (expected {expected_vocab_sha}, got {vocab_sha}) — re-install the model."
        ));
    }
    Ok(())
}

/// P3.4 fast load-time integrity guard for the bundled fine-tuned model (size + vocab SHA, both instant).
/// Non-bundled ONNX paths skip the check.
pub fn verify_finetuned_fast(onnx_path: &Path, vocab_path: &Path) -> Result<(), String> {
    if !is_bundled_finetuned(onnx_path) {
        return Ok(());
    }
    verify_integrity_against(onnx_path, FINETUNED_MODEL_BYTES, vocab_path, FINETUNED_VOCAB_SHA256)
}

/// P3.4 definitive full-SHA verification of the bundled fine-tuned model + vocab (on demand — the
/// `verify_finetuned_model_integrity` IPC). Hashes the full 970 MB model, so it is NOT run per-load.
pub fn verify_finetuned_full(onnx_path: &Path, vocab_path: &Path) -> Result<(), String> {
    let model_sha = crate::models::compute_file_sha256(onnx_path)?;
    if model_sha != FINETUNED_MODEL_SHA256 {
        return Err(format!("model.onnx SHA mismatch (expected {FINETUNED_MODEL_SHA256}, got {model_sha})"));
    }
    let vocab_sha = crate::models::compute_file_sha256(vocab_path)?;
    if vocab_sha != FINETUNED_VOCAB_SHA256 {
        return Err(format!("vocab.json SHA mismatch (expected {FINETUNED_VOCAB_SHA256}, got {vocab_sha})"));
    }
    Ok(())
}

/// Zero-mean / unit-variance normalize the waveform (HF `zero_mean_unit_var_norm`, population var).
pub fn normalize_audio(audio: &[f32]) -> Vec<f32> {
    let n = audio.len().max(1) as f32;
    let mean = audio.iter().sum::<f32>() / n;
    let var = audio.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
    let denom = (var + 1e-7).sqrt();
    audio.iter().map(|x| (x - mean) / denom).collect()
}

/// Build the id->token table for `lang` from the MMS nested `vocab.json`
/// (`{"<lang>": {"<tok>": id, ...}, ...}`).
pub fn load_lang_vocab(vocab_path: &Path, lang: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(vocab_path).map_err(|e| format!("read vocab: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("parse vocab: {e}"))?;
    let sub =
        v.get(lang).and_then(|s| s.as_object()).ok_or_else(|| format!("vocab.json has no object for lang {lang:?}"))?;
    vocab_from_map(sub)
}

/// A CTC vocab far larger than any real token set indicates a corrupt or hostile vocab.json (an id
/// like 4e9 would try to allocate billions of empty strings -> OOM abort). Real MMS/Wav2Vec2 heads are
/// a few thousand tokens; cap well above that so no legitimate model is rejected.
const MAX_VOCAB_ID: u64 = 1_000_000;

fn vocab_from_map(sub: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<String>, String> {
    let max_id = sub.values().filter_map(|x| x.as_u64()).max().ok_or_else(|| "empty vocab".to_string())?;
    if max_id > MAX_VOCAB_ID {
        return Err(format!("vocab.json max token id {max_id} exceeds sane bound {MAX_VOCAB_ID}"));
    }
    let mut toks = vec![String::new(); (max_id as usize) + 1];
    for (tok, id) in sub {
        if let Some(i) = id.as_u64() {
            toks[i as usize] = tok.clone();
        }
    }
    Ok(toks)
}

fn argmax(frame: &[f32]) -> usize {
    let mut best = 0usize;
    let mut best_val = f32::NEG_INFINITY;
    for (i, &v) in frame.iter().enumerate() {
        if v > best_val {
            best_val = v;
            best = i;
        }
    }
    best
}

/// CTC greedy decode of per-frame logits `[T][vocab]` per the Wav2Vec2CTCTokenizer.
pub fn ctc_decode(frames: &[Vec<f32>], tokens: &[String]) -> String {
    let mut out = String::new();
    let mut prev: Option<usize> = None;
    for frame in frames {
        let best = argmax(frame);
        if prev != Some(best) && best != PAD_ID {
            if let Some(t) = tokens.get(best) {
                match t.as_str() {
                    "|" => out.push(' '),
                    "<s>" | "</s>" | "<unk>" | "<pad>" => {}
                    other => out.push_str(other),
                }
            }
        }
        prev = Some(best);
    }
    out.trim().to_string()
}

/// Run the fine-tuned Wav2Vec2-CTC model and return the raw CTC emission logits (flat row-major
/// `[frames * vocab]`), the frame count, the vocab width, and the char token table. Shared by
/// transcription (greedy decode) and forced alignment (Viterbi against a known transcript). The
/// session is cached (keyed by ONNX path) across calls.
pub fn wav2vec2_logits(
    onnx_path: &Path,
    vocab_path: &Path,
    lang: &str,
    audio: &[f32],
) -> Result<(Vec<f32>, usize, usize, Vec<String>), String> {
    crate::models::init_ort_dylib_path();
    let normed = normalize_audio(audio);
    let n = normed.len();

    // Recover a poisoned cache lock (into_inner) instead of hard-failing — a panic in one inference
    // path must not permanently break BOTH fine-tuned transcription and alignment until app restart.
    let mut guard = SESSION_CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.as_ref().map(|(p, _)| p.as_path() != onnx_path).unwrap_or(true) {
        // P3.4: verify the bundled fine-tuned model's integrity before loading it (once per model-path,
        // since the session is cached). A truncated/swapped model would otherwise silently degrade the
        // measured accuracy. Non-bundled paths are a no-op.
        verify_finetuned_fast(onnx_path, vocab_path)?;
        let session = ort::session::Session::builder()
            .map_err(|e| format!("ort builder: {e}"))?
            .commit_from_file(onnx_path)
            .map_err(|e| format!("ort load {}: {e}", onnx_path.display()))?;
        *guard = Some((onnx_path.to_path_buf(), session));
    }
    let session = match guard.as_mut() {
        Some((_, s)) => s,
        None => return Err("wav2vec2 session unexpectedly missing after init".to_string()),
    };

    let input = ort::value::Tensor::from_array(([1usize, n], normed)).map_err(|e| format!("ort input tensor: {e}"))?;
    let outputs = session.run(ort::inputs!["input_values" => input]).map_err(|e| format!("ort run: {e}"))?;
    let (shape, data) =
        outputs["logits"].try_extract_tensor::<f32>().map_err(|e| format!("ort extract logits: {e}"))?;
    if shape.len() != 3 {
        return Err(format!("unexpected logits rank {shape:?}"));
    }
    // Reject non-positive dims before casting to usize. An ONNX output legitimately may declare a
    // dynamic dim as -1 (or 0); `-1i64 as usize` is usize::MAX, so `frames_n * vocab` would overflow
    // (and could wrap past the buffer guard below), then the per-frame slice at decode time panics
    // out-of-bounds. A malformed/env-overridden model (verify is skipped for non-bundled paths) must
    // fail cleanly, not crash the process.
    if shape[1] <= 0 || shape[2] <= 0 {
        return Err(format!("logits shape has a non-positive dim {shape:?}"));
    }
    let frames_n = shape[1] as usize;
    let vocab = shape[2] as usize;
    let needed = frames_n.checked_mul(vocab).ok_or_else(|| format!("logits shape {shape:?} overflows"))?;
    if data.len() < needed {
        return Err("logits buffer smaller than shape".to_string());
    }
    let tokens = load_lang_vocab(vocab_path, lang)?;
    // The CTC head width MUST equal the vocab token count — otherwise argmax indices map to the WRONG
    // token (head narrower than vocab → fluent-but-wrong Kurdish) or fall off the end (head wider →
    // dropped tokens), all SILENTLY. A mismatched/swapped model.onnx + vocab.json pair (user-supplied or
    // env-overridden) would otherwise produce confidently-wrong output; fail hard and observably instead.
    if vocab != tokens.len() {
        return Err(format!(
            "fine-tuned model/vocab mismatch: logits head width {} != vocab.json '{}' token count {} \
             — the model.onnx and vocab.json do not correspond",
            vocab,
            lang,
            tokens.len()
        ));
    }
    Ok((data[..needed].to_vec(), frames_n, vocab, tokens))
}

/// Run the fine-tuned Wav2Vec2-CTC model on raw 16 kHz mono audio via `ort` and decode to text.
pub fn run_wav2vec2(onnx_path: &Path, vocab_path: &Path, lang: &str, audio: &[f32]) -> Result<String, String> {
    let (data, frames_n, vocab, tokens) = wav2vec2_logits(onnx_path, vocab_path, lang, audio)?;
    let frames: Vec<Vec<f32>> = (0..frames_n).map(|i| data[i * vocab..(i + 1) * vocab].to_vec()).collect();
    Ok(ctc_decode(&frames, &tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_bundled_finetuned_matches_only_the_pinned_path() {
        assert!(is_bundled_finetuned(Path::new("/x/models/finetuned-mms-ckb/model.onnx")));
        assert!(is_bundled_finetuned(Path::new("C:/a/finetuned-mms-ckb/model.onnx")));
        assert!(!is_bundled_finetuned(Path::new("/x/omniasr-ctc-300m/model.onnx")), "other engine");
        assert!(!is_bundled_finetuned(Path::new("/x/finetuned-mms-ckb/other.onnx")), "wrong file");
        assert!(!is_bundled_finetuned(Path::new("/custom/exported.onnx")), "custom override");
    }

    #[test]
    fn vocab_from_map_rejects_hostile_or_empty_vocab_without_oom() {
        use serde_json::json;

        // Empty vocab -> clear error, not a panic.
        let empty = json!({}).as_object().unwrap().clone();
        assert!(vocab_from_map(&empty).is_err(), "empty vocab must error");

        // A max id far above any real CTC head must be rejected BEFORE the vec![...; max_id+1]
        // allocation (a 4e9 id would otherwise try to allocate billions of strings -> OOM abort).
        let hostile = json!({ "a": 0, "b": 4_000_000_000u64 }).as_object().unwrap().clone();
        let err = vocab_from_map(&hostile).unwrap_err();
        assert!(err.contains("exceeds sane bound"), "hostile id surfaced: {err}");

        // A normal small vocab builds a dense token table indexed by id.
        let ok = json!({ "<pad>": 0, "a": 1, "b": 2 }).as_object().unwrap().clone();
        let toks = vocab_from_map(&ok).unwrap();
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[1], "a");
        assert_eq!(toks[2], "b");
    }

    #[test]
    fn verify_integrity_against_passes_and_fails_correctly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let onnx = tmp.path().join("model.onnx");
        let vocab = tmp.path().join("vocab.json");
        std::fs::write(&onnx, vec![0u8; 1234]).unwrap(); // exactly 1234 bytes
        std::fs::write(&vocab, b"{\"ckb\":{}}").unwrap();
        let vocab_sha = crate::models::compute_file_sha256(&vocab).unwrap();

        // Correct size + correct vocab SHA -> Ok.
        assert!(verify_integrity_against(&onnx, 1234, &vocab, &vocab_sha).is_ok());

        // Wrong model size (truncation) -> Err.
        let err = verify_integrity_against(&onnx, 9999, &vocab, &vocab_sha).unwrap_err();
        assert!(err.contains("truncated or replaced"), "size mismatch surfaced: {err}");

        // Right size, wrong vocab SHA (corruption) -> Err.
        let err = verify_integrity_against(&onnx, 1234, &vocab, &"0".repeat(64)).unwrap_err();
        assert!(err.contains("SHA mismatch"), "vocab mismatch surfaced: {err}");
    }

    #[test]
    fn verify_finetuned_fast_skips_non_bundled_paths() {
        // A non-bundled path must not error even if the files don't exist — the pin only guards the
        // bundled champion; custom/alignment models are out of scope.
        assert!(verify_finetuned_fast(Path::new("/custom/exported.onnx"), Path::new("/custom/vocab.json")).is_ok());
    }

    #[test]
    fn normalize_is_zero_mean_unit_var() {
        let out = normalize_audio(&[1.0, 2.0, 3.0, 4.0]);
        let n = out.len() as f32;
        let mean = out.iter().sum::<f32>() / n;
        let var = out.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
        assert!(mean.abs() < 1e-4, "mean ~0, got {mean}");
        assert!((var - 1.0).abs() < 1e-3, "var ~1, got {var}");
    }

    #[test]
    fn vocab_from_nested_lang_object() {
        let v = serde_json::json!({
            "eng": {"<pad>": 0, "a": 1},
            "ckb": {"<pad>": 0, "|": 1, "ئ": 2, "<unk>": 3, "ا": 4}
        });
        let sub = v.get("ckb").unwrap().as_object().unwrap();
        let toks = vocab_from_map(sub).unwrap();
        assert_eq!(toks.len(), 5);
        assert_eq!(toks[0], "<pad>");
        assert_eq!(toks[1], "|");
        assert_eq!(toks[2], "ئ");
        assert_eq!(toks[4], "ا");
    }

    #[test]
    fn ctc_decode_collapses_drops_pad_and_maps_delimiter() {
        // tokens: 0=<pad>(blank), 1="|"(space), 2="ا", 3="ب", 4="<unk>"
        let tokens = vec!["<pad>".to_string(), "|".to_string(), "ا".to_string(), "ب".to_string(), "<unk>".to_string()];
        let hot = |id: usize| {
            let mut f = vec![0.0f32; 5];
            f[id] = 9.0;
            f
        };
        // ا ا <pad> ب | ب  -> "اب ب"  (collapse the repeat, drop pad, "|"->space)
        let frames = vec![hot(2), hot(2), hot(0), hot(3), hot(1), hot(3)];
        assert_eq!(ctc_decode(&frames, &tokens), "اب ب");
    }

    #[test]
    fn ctc_decode_skips_specials() {
        let tokens = vec!["<pad>".to_string(), "<s>".to_string(), "ا".to_string(), "<unk>".to_string()];
        let hot = |id: usize| {
            let mut f = vec![0.0f32; 4];
            f[id] = 9.0;
            f
        };
        // <s> ا <unk> -> "ا" (specials skipped)
        let frames = vec![hot(1), hot(2), hot(3)];
        assert_eq!(ctc_decode(&frames, &tokens), "ا");
    }

    /// Parity gate: run the exported fine-tuned ONNX on a real clip and assert non-empty Kurdish.
    /// Gated on env CORTEX_FINETUNED_ONNX + CORTEX_FINETUNED_VOCAB + CORTEX_CONSTRAINED_WAV.
    #[test]
    #[ignore]
    fn wav2vec2_real_clip_is_kurdish() {
        let (onnx, vocab, wav) = match (
            std::env::var("CORTEX_FINETUNED_ONNX"),
            std::env::var("CORTEX_FINETUNED_VOCAB"),
            std::env::var("CORTEX_CONSTRAINED_WAV"),
        ) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            _ => {
                eprintln!("set CORTEX_FINETUNED_ONNX/_VOCAB + CORTEX_CONSTRAINED_WAV; skipping");
                return;
            }
        };
        let (rate, pcm) = crate::audio::decode_to_pcm(&wav).expect("decode wav");
        assert_eq!(rate, 16000);
        let audio: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let text = run_wav2vec2(Path::new(&onnx), Path::new(&vocab), "ckb", &audio).expect("run");
        eprintln!("[wav2vec2] {text:?}");
        assert!(!text.is_empty(), "empty transcript");
    }
}
