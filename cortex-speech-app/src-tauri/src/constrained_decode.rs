//! Constrained (Kurdish-token-masked) CTC greedy decode for OmniASR-CTC.
//!
//! OmniASR-CTC emits per-frame logits over a 9812-token, ~1600-language vocabulary with only a
//! small set of Arabic-script tokens, so per frame it can pick a non-Kurdish token and the output
//! drifts out of Kurdish script (a "language-lock" failure). Masking the decoder to
//! `{blank, space, Arabic-script tokens}` guarantees Kurdish-script output and modestly lowers CER
//! with no retraining. This is the Rust port of the validated probe
//! `scripts/constrained_decode_probe.py` (see `docs/EVAL.md`).
//!
//! This module is intentionally NOT wired into the default sherpa-onnx ASR path; it is an opt-in
//! capability (the `ort` inference entry point is gated behind the model being present). The pure
//! decode functions are unit-tested; `run_constrained` is covered by a Windows-only `#[ignore]`d
//! parity test that runs the real model.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Cached `ort` session for the constrained decode path, keyed by model path. The 300M model load
/// (~hundreds of ms) is amortized across repeated opt-in calls; the Mutex serializes constrained
/// inferences (acceptable — this is a user-initiated, off-the-default-path action).
static SESSION_CACHE: LazyLock<Mutex<Option<(PathBuf, ort::session::Session)>>> = LazyLock::new(|| Mutex::new(None));

/// A token is treated as Kurdish (Arabic-script) if any of its chars fall in the Arabic block
/// (U+0600..U+06FF) or the Arabic Supplement block (U+0750..U+077F) — the ranges Central Kurdish
/// (Sorani) draws from. Mirrors `is_arabic` in the Python probe.
pub fn is_arabic(tok: &str) -> bool {
    tok.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c) || ('\u{0750}'..='\u{077F}').contains(&c))
}

/// Parse an OmniASR `tokens.txt`: every non-empty line is `<token> <id>`, where the id is the last
/// whitespace-separated field. Returns a Vec indexed by id (gaps filled with empty strings).
pub fn load_tokens(path: &Path) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    parse_tokens(&text)
}

/// Parse tokens from already-read text (split out so it can be unit-tested without a file).
pub fn parse_tokens(text: &str) -> std::io::Result<Vec<String>> {
    let mut pairs: Vec<(usize, String)> = Vec::new();
    let mut max_id = 0usize;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        // The id is the final field; the token itself may be a literal space (" 4").
        if let Some(pos) = line.rfind(' ') {
            let tok = &line[..pos];
            if let Ok(id) = line[pos + 1..].trim().parse::<usize>() {
                max_id = max_id.max(id);
                pairs.push((id, tok.to_string()));
            }
        }
    }
    let mut toks = vec![String::new(); max_id.saturating_add(1)];
    for (id, tok) in pairs {
        toks[id] = tok;
    }
    Ok(toks)
}

/// Build the constrained keep-set: the CTC blank, the space token(s), and every Arabic-script
/// token. Any token NOT in this set is masked out during decoding.
pub fn kurdish_keep_set(tokens: &[String], blank: usize) -> Vec<usize> {
    let mut keep: Vec<usize> = vec![blank];
    for (i, t) in tokens.iter().enumerate() {
        if t == " " || is_arabic(t) {
            keep.push(i);
        }
    }
    keep.sort_unstable();
    keep.dedup();
    keep
}

/// Determine the CTC blank id empirically as the most-frequent argmax across frames (OmniASR emits blank
/// on the large majority of frames). The blank is a fixed SPECIAL token (`<s>`/`<pad>`/…), never a real
/// script token — so the vote is restricted to special `<...>` tokens. On a LONG clip the blank wins the
/// global plurality anyway, but on a SHORT clip a single sustained Arabic token can out-count it; choosing
/// that token as "blank" would make greedy_ctc silently DELETE the one word from the transcript.
/// Restricting to special tokens makes the choice correct regardless of clip length.
pub fn empirical_blank(logits: &[Vec<f32>], tokens: &[String]) -> usize {
    use std::collections::HashMap;
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for frame in logits {
        counts.entry(argmax(frame, None)).and_modify(|c| *c += 1).or_insert(1);
    }
    // Deterministic tie-break: HashMap iteration order is per-process-randomized and max_by_key keeps the
    // LAST max seen, so on a count tie the chosen blank — and thus the decoded string — could differ
    // between runs of the SAME input. Break ties by the smallest token id so the result is reproducible.
    let is_special = |id: usize| tokens.get(id).map(|t| t.starts_with('<') && t.ends_with('>')).unwrap_or(false);
    if let Some((&id, _)) =
        counts.iter().filter(|&(&id, _)| is_special(id)).max_by_key(|&(&id, &c)| (c, std::cmp::Reverse(id)))
    {
        return id;
    }
    // No special token ever won a frame (a degenerate single-token clip, or a vocab with no `<...>`
    // specials): fall back to the unrestricted plurality with the same smallest-id tie-break.
    counts.into_iter().max_by_key(|&(id, c)| (c, std::cmp::Reverse(id))).map(|(id, _)| id).unwrap_or(0)
}

fn argmax(frame: &[f32], keep_mask: Option<&[bool]>) -> usize {
    let mut best = 0usize;
    let mut best_val = f32::NEG_INFINITY;
    for (i, &v) in frame.iter().enumerate() {
        let val = match keep_mask {
            Some(m) if !m.get(i).copied().unwrap_or(false) => f32::NEG_INFINITY,
            _ => v,
        };
        if val > best_val {
            best_val = val;
            best = i;
        }
    }
    best
}

/// Greedy CTC decode over per-frame logits `[T][V]`. When `keep` is `Some`, every token id not in
/// `keep` is treated as `-inf` before the per-frame argmax (constrained decode). Repeats are
/// collapsed and the blank is dropped. Mirrors `greedy_ctc` in the probe.
pub fn greedy_ctc(logits: &[Vec<f32>], blank: usize, tokens: &[String], keep: Option<&[usize]>) -> String {
    let vocab = tokens.len();
    let keep_mask: Option<Vec<bool>> = keep.map(|k| {
        let mut m = vec![false; vocab];
        for &i in k {
            if i < vocab {
                m[i] = true;
            }
        }
        m
    });
    let mut out = String::new();
    let mut prev: Option<usize> = None;
    for frame in logits {
        let best = argmax(frame, keep_mask.as_deref());
        if prev != Some(best) && best != blank {
            if let Some(t) = tokens.get(best) {
                out.push_str(t);
            }
        }
        prev = Some(best);
    }
    out
}

/// Run OmniASR-CTC over raw 16 kHz mono audio via `ort` and decode. With `constrained = true` the
/// decode is masked to Kurdish tokens (guaranteed Kurdish-script output). Loads a fresh session per
/// call — production wiring should cache it; this entry point is for the opt-in path + parity tests.
pub fn run_constrained(
    model_path: &Path,
    tokens_path: &Path,
    audio: &[f32],
    constrained: bool,
) -> Result<String, String> {
    crate::models::init_ort_dylib_path();
    let mut guard = SESSION_CACHE.lock().map_err(|_| "constrained session cache poisoned".to_string())?;
    if guard.as_ref().map(|(p, _)| p.as_path() != model_path).unwrap_or(true) {
        let session = ort::session::Session::builder()
            .map_err(|e| format!("ort builder: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| format!("ort load {}: {e}", model_path.display()))?;
        *guard = Some((model_path.to_path_buf(), session));
    }
    let session = match guard.as_mut() {
        Some((_, s)) => s,
        None => return Err("constrained session unexpectedly missing after init".to_string()),
    };

    let n = audio.len();
    let input =
        ort::value::Tensor::from_array(([1usize, n], audio.to_vec())).map_err(|e| format!("ort input tensor: {e}"))?;
    let outputs = session.run(ort::inputs!["x" => input]).map_err(|e| format!("ort run: {e}"))?;
    let (shape, data) =
        outputs["logits"].try_extract_tensor::<f32>().map_err(|e| format!("ort extract logits: {e}"))?;
    if shape.len() != 3 {
        return Err(format!("unexpected logits rank {:?}", shape));
    }
    let frames_n = shape[1] as usize;
    let vocab = shape[2] as usize;
    if data.len() < frames_n * vocab {
        return Err("logits buffer smaller than shape".to_string());
    }
    let frames: Vec<Vec<f32>> = (0..frames_n).map(|i| data[i * vocab..(i + 1) * vocab].to_vec()).collect();

    let tokens = load_tokens(tokens_path).map_err(|e| format!("tokens {}: {e}", tokens_path.display()))?;
    let blank = empirical_blank(&frames, &tokens);
    let keep = if constrained { Some(kurdish_keep_set(&tokens, blank)) } else { None };
    Ok(greedy_ctc(&frames, blank, &tokens, keep.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empirical_blank_tie_break_is_deterministic_smallest_id() {
        // Special tokens 0 (<s>) and 2 (</s>) each win 2 argmaxes (a tie). The blank must deterministically
        // be the smallest id (0), not a HashMap-iteration-order coin flip.
        let frame_for = |winner: usize| {
            let mut f = vec![0.0f32; 4];
            f[winner] = 1.0;
            f
        };
        let tokens = vec!["<s>".to_string(), "<pad>".to_string(), "</s>".to_string(), "ئ".to_string()];
        let logits = vec![frame_for(2), frame_for(0), frame_for(2), frame_for(0)];
        for _ in 0..50 {
            assert_eq!(empirical_blank(&logits, &tokens), 0, "tie must resolve to the smallest token id, every run");
        }
    }

    #[test]
    fn empirical_blank_ignores_a_real_token_that_out_counts_blank_on_a_short_clip() {
        // A short clip where a sustained Arabic token (id 3) wins MORE frames than the real blank
        // (<pad>, id 1). The blank must still be the special token, never the Arabic one — otherwise
        // greedy_ctc would treat the word as blank and DELETE it from the transcript.
        let frame_for = |winner: usize| {
            let mut f = vec![0.0f32; 4];
            f[winner] = 1.0;
            f
        };
        let tokens = vec!["<s>".to_string(), "<pad>".to_string(), "</s>".to_string(), "ئ".to_string()];
        let logits = vec![frame_for(3), frame_for(3), frame_for(3), frame_for(1)];
        assert_eq!(empirical_blank(&logits, &tokens), 1, "a real script token must never be chosen as the blank");
    }

    #[test]
    fn is_arabic_detects_kurdish_script() {
        assert!(is_arabic("سڵاو")); // Kurdish
        assert!(is_arabic("ئ"));
        assert!(!is_arabic("hello"));
        assert!(!is_arabic("t不")); // Latin + CJK, no Arabic block -> not kept
        assert!(!is_arabic(" "));
        assert!(!is_arabic(""));
    }

    #[test]
    fn parse_tokens_indexes_by_id_including_space() {
        // tokens.txt style: "<token> <id>". The space token is written as TWO spaces + id
        // ("  4" = space-token + separator + "4"), matching the real OmniASR tokens.txt.
        let toks = parse_tokens("<s> 0\n<unk> 3\n  4\nس 72\n").unwrap();
        assert_eq!(toks.len(), 73);
        assert_eq!(toks[0], "<s>");
        assert_eq!(toks[3], "<unk>");
        assert_eq!(toks[4], " ");
        assert_eq!(toks[72], "س");
        assert_eq!(toks[1], ""); // gap
    }

    #[test]
    fn keep_set_is_blank_space_and_arabic_only() {
        let tokens = vec![
            "<blank>".to_string(), // 0 (blank)
            "a".to_string(),       // 1 latin -> dropped
            " ".to_string(),       // 2 space -> kept
            "س".to_string(),       // 3 arabic -> kept
            "不".to_string(),      // 4 CJK -> dropped
        ];
        let keep = kurdish_keep_set(&tokens, 0);
        assert_eq!(keep, vec![0, 2, 3]);
    }

    #[test]
    fn constrained_decode_remaps_off_script_frame_to_kurdish() {
        // 3 tokens: blank=0, latin "x"=1, arabic "س"=2.
        let tokens = vec!["<blank>".to_string(), "x".to_string(), "س".to_string()];
        // Frame strongly favors the latin token; unconstrained picks "x", constrained must avoid it.
        let logits = vec![vec![0.0_f32, 5.0, 1.0]];
        let unconstrained = greedy_ctc(&logits, 0, &tokens, None);
        assert_eq!(unconstrained, "x");
        let keep = kurdish_keep_set(&tokens, 0); // {0,2}
        let constrained = greedy_ctc(&logits, 0, &tokens, Some(&keep));
        // "x" (id 1) is masked out, so the best remaining non-blank is the arabic token.
        assert_eq!(constrained, "س");
    }

    #[test]
    fn greedy_collapses_repeats_and_drops_blank() {
        let tokens = vec!["<blank>".to_string(), "ا".to_string(), "ب".to_string()];
        // ا ا <blank> ا ب  -> "ااب" collapses repeats across blank boundary -> "ااب"
        let logits = vec![
            vec![0.0, 9.0, 0.0], // ا
            vec![0.0, 9.0, 0.0], // ا (repeat -> dropped)
            vec![9.0, 0.0, 0.0], // blank (dropped, resets prev)
            vec![0.0, 9.0, 0.0], // ا (new run -> emitted)
            vec![0.0, 0.0, 9.0], // ب
        ];
        assert_eq!(greedy_ctc(&logits, 0, &tokens, None), "ااب");
    }

    /// Parity gate: run the REAL model on a real clip and confirm the constrained decode stays in
    /// Kurdish script. Gated on the model + a clip being present; set CORTEX_CONSTRAINED_WAV to a
    /// 16 kHz mono WAV. Mirrors scripts/constrained_decode_probe.py.
    #[test]
    #[ignore]
    fn constrained_decode_real_clip_is_kurdish_script() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let model = root.join("models/omniasr-ctc-300m/model.int8.onnx");
        let tokens = root.join("models/omniasr-ctc-300m/tokens.txt");
        if !model.exists() || !tokens.exists() {
            eprintln!("model/tokens not present; skipping constrained real-clip test");
            return;
        }
        let wav = match std::env::var("CORTEX_CONSTRAINED_WAV") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                eprintln!("set CORTEX_CONSTRAINED_WAV to a 16 kHz mono WAV; skipping");
                return;
            }
        };
        let (rate, pcm) = crate::audio::decode_to_pcm(&wav).expect("decode wav");
        assert_eq!(rate, 16000, "expected 16 kHz mono");
        let audio: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

        let unconstrained = run_constrained(&model, &tokens, &audio, false).expect("unconstrained");
        let constrained = run_constrained(&model, &tokens, &audio, true).expect("constrained");
        eprintln!("[constrained] unconstrained={unconstrained:?}\n[constrained] constrained={constrained:?}");
        assert!(!constrained.is_empty(), "constrained decode produced empty output");
        // Every non-space char of the constrained output must be Arabic-script.
        for c in constrained.chars().filter(|c| !c.is_whitespace()) {
            assert!(is_arabic(&c.to_string()), "constrained output left Kurdish script: {c:?}");
        }
    }
}
