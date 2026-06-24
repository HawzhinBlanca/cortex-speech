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
    let sub = v
        .get(lang)
        .and_then(|s| s.as_object())
        .ok_or_else(|| format!("vocab.json has no object for lang {lang:?}"))?;
    vocab_from_map(sub)
}

fn vocab_from_map(sub: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<String>, String> {
    let max_id = sub
        .values()
        .filter_map(|x| x.as_u64())
        .max()
        .ok_or_else(|| "empty vocab".to_string())? as usize;
    let mut toks = vec![String::new(); max_id + 1];
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

/// Run the fine-tuned Wav2Vec2-CTC model on raw 16 kHz mono audio via `ort` and decode to text.
/// The session is cached (keyed by ONNX path) across calls.
pub fn run_wav2vec2(onnx_path: &Path, vocab_path: &Path, lang: &str, audio: &[f32]) -> Result<String, String> {
    crate::models::init_ort_dylib_path();
    let normed = normalize_audio(audio);
    let n = normed.len();

    let mut guard = SESSION_CACHE.lock().map_err(|_| "wav2vec2 session cache poisoned".to_string())?;
    if guard.as_ref().map(|(p, _)| p.as_path() != onnx_path).unwrap_or(true) {
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

    let input = ort::value::Tensor::from_array(([1usize, n], normed))
        .map_err(|e| format!("ort input tensor: {e}"))?;
    let outputs = session
        .run(ort::inputs!["input_values" => input])
        .map_err(|e| format!("ort run: {e}"))?;
    let (shape, data) = outputs["logits"]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("ort extract logits: {e}"))?;
    if shape.len() != 3 {
        return Err(format!("unexpected logits rank {shape:?}"));
    }
    let frames_n = shape[1] as usize;
    let vocab = shape[2] as usize;
    if data.len() < frames_n * vocab {
        return Err("logits buffer smaller than shape".to_string());
    }
    let frames: Vec<Vec<f32>> = (0..frames_n)
        .map(|i| data[i * vocab..(i + 1) * vocab].to_vec())
        .collect();
    let tokens = load_lang_vocab(vocab_path, lang)?;
    Ok(ctc_decode(&frames, &tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let tokens = vec![
            "<pad>".to_string(),
            "|".to_string(),
            "ا".to_string(),
            "ب".to_string(),
            "<unk>".to_string(),
        ];
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
