use ort::value::Tensor;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WordTimestamp {
    pub word: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f64,
}

/// Which algorithm actually produced a clip's word timestamps. Recorded as the dataset's
/// `alignment_quality` so provenance is honest — heuristic output is never published as
/// CTC forced alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignmentQuality {
    /// Real CTC forced alignment against a loaded acoustic model.
    CtcForced,
    /// Linear/energy heuristic fallback (no aligner model loaded, or CTC found no path).
    EnergyHeuristic,
}

impl AlignmentQuality {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::CtcForced => "ctc_forced",
            Self::EnergyHeuristic => "energy_heuristic",
        }
    }
}

pub struct ForcedAligner {
    session: Option<std::sync::Mutex<ort::session::Session>>,
    #[allow(dead_code)]
    tokens: Vec<String>,
    #[allow(dead_code)]
    sample_rate: u32,
}

impl ForcedAligner {
    pub fn new(model_dir: &Path, enable_gpu: bool) -> Result<Self, String> {
        let aligner_path = model_dir.join("mms_aligner.onnx");

        if !aligner_path.exists() {
            tracing::info!("MMS aligner model not found at {:?}; using energy-based alignment", aligner_path);
            return Ok(Self { session: None, tokens: Vec::new(), sample_rate: 16000 });
        }

        crate::models::init_ort_dylib_path();
        let mut builder = ort::session::Session::builder().map_err(|e| format!("Aligner session builder: {e}"))?;

        if enable_gpu {
            #[cfg(not(target_os = "macos"))]
            {
                builder = builder.with_execution_providers([ort::ep::CUDA::default().build()]).unwrap_or_else(|e| {
                    tracing::info!("CUDA EP not available for aligner, falling back to CPU: {e}");
                    e.recover()
                });
            }
        }

        let session = builder.commit_from_file(&aligner_path).map_err(|e| format!("Load aligner model: {e}"))?;

        let tokens_path = model_dir.join("mms_aligner_tokens.txt");
        let tokens = if tokens_path.exists() {
            std::fs::read_to_string(&tokens_path)
                .map_err(|e| format!("Read aligner tokens: {e}"))?
                .lines()
                .map(|l| l.to_string())
                .collect()
        } else {
            Vec::new()
        };

        tracing::info!("MMS forced aligner loaded with {} tokens", tokens.len());
        Ok(Self { session: Some(std::sync::Mutex::new(session)), tokens, sample_rate: 16000 })
    }

    pub fn is_available(&self) -> bool {
        self.session.is_some()
    }

    pub fn align(
        &self,
        pcm: &[i16],
        sample_rate: u32,
        text: &str,
    ) -> Result<(Vec<WordTimestamp>, AlignmentQuality), Box<dyn std::error::Error>> {
        if self.session.is_none() || text.trim().is_empty() || pcm.is_empty() {
            return Ok((fallback_align(pcm, sample_rate, text), AlignmentQuality::EnergyHeuristic));
        }

        let mut session_guard = self
            .session
            .as_ref()
            .ok_or_else(|| Box::<dyn std::error::Error>::from("Aligner session missing"))?
            .lock()
            .map_err(|e| format!("Aligner lock: {e}"))?;
        let input_name = session_guard
            .inputs()
            .first()
            .ok_or_else(|| Box::<dyn std::error::Error>::from("Aligner model exposes no inputs"))?
            .name()
            .to_string();
        let output_name = session_guard
            .outputs()
            .first()
            .ok_or_else(|| Box::<dyn std::error::Error>::from("Aligner model exposes no outputs"))?
            .name()
            .to_string();

        let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let input_nd = ndarray::Array2::from_shape_vec((1, f32_pcm.len()), f32_pcm)
            .map_err(|e| format!("Aligner input reshape: {e}"))?;
        let input_tensor = Tensor::from_array(input_nd).map_err(|e| format!("Aligner input tensor: {e}"))?;

        let outputs = session_guard
            .run(ort::inputs![
                input_name => input_tensor
            ])
            .map_err(|e| format!("Aligner inference: {e}"))?;

        let extract_res = outputs[output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Extract aligner output: {e}"))?;
        let (output_shape, logits) = extract_res;

        if output_shape.len() < 3 {
            return Ok((fallback_align(pcm, sample_rate, text), AlignmentQuality::EnergyHeuristic));
        }

        let num_frames = output_shape[1] as usize;
        let vocab_size = output_shape[2] as usize;
        // A corrupt-but-loadable model can report a zero frame/vocab dim, which would
        // divide-by-zero in ctc_align; degrade to the energy aligner instead of panicking.
        if num_frames == 0 || vocab_size == 0 {
            return Ok((fallback_align(pcm, sample_rate, text), AlignmentQuality::EnergyHeuristic));
        }
        let blank_idx = self.tokens.iter().position(|t| t == "<pad>" || t == "_" || t == "<blank>").unwrap_or(0);

        // Tokenize text
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut target_tokens = Vec::new();
        let mut word_char_to_token_idx = Vec::new();

        for &w in &words {
            let mut char_indices = Vec::new();
            for c in w.chars() {
                let char_str = c.to_lowercase().to_string();
                if let Some(idx) = self.tokens.iter().position(|t| t == &char_str) {
                    if idx < vocab_size && idx != blank_idx {
                        char_indices.push(Some(target_tokens.len()));
                        target_tokens.push(idx);
                        continue;
                    }
                }
                char_indices.push(None);
            }
            word_char_to_token_idx.push(char_indices);
        }

        if target_tokens.is_empty() {
            return Ok((fallback_align(pcm, sample_rate, text), AlignmentQuality::EnergyHeuristic));
        }

        let (path, _best_val) = ctc_align(logits, vocab_size, &target_tokens, blank_idx);
        if path.is_empty() {
            return Ok((fallback_align(pcm, sample_rate, text), AlignmentQuality::EnergyHeuristic));
        }

        // Map path back to character index ranges
        let mut char_alignments = vec![(0usize, 0usize); target_tokens.len()];
        let mut active_state = usize::MAX;
        let mut start_frame = 0;

        for (f, &state) in path.iter().enumerate() {
            if state != active_state {
                if active_state != usize::MAX && active_state % 2 == 1 {
                    let char_idx = active_state / 2;
                    if char_idx < char_alignments.len() {
                        char_alignments[char_idx] = (start_frame, f);
                    }
                }
                active_state = state;
                start_frame = f;
            }
        }
        if active_state != usize::MAX && active_state % 2 == 1 {
            let char_idx = active_state / 2;
            if char_idx < char_alignments.len() {
                char_alignments[char_idx] = (start_frame, num_frames);
            }
        }

        let mut word_timestamps = Vec::new();

        for (word_idx, &word) in words.iter().enumerate() {
            let char_indices = &word_char_to_token_idx[word_idx];
            let mut word_start_frame = usize::MAX;
            let mut word_end_frame = 0;

            for &opt_idx in char_indices {
                if let Some(token_idx) = opt_idx {
                    if token_idx < char_alignments.len() {
                        let (s, e) = char_alignments[token_idx];
                        if s < e {
                            if s < word_start_frame {
                                word_start_frame = s;
                            }
                            if e > word_end_frame {
                                word_end_frame = e;
                            }
                        }
                    }
                }
            }

            let start_time = if word_start_frame != usize::MAX {
                word_start_frame as f64 * 0.02
            } else {
                word_timestamps.last().map(|w: &WordTimestamp| w.end).unwrap_or(0.0)
            };

            let end_time = if word_end_frame > 0 { word_end_frame as f64 * 0.02 } else { start_time + 0.25 };

            word_timestamps.push(WordTimestamp {
                word: word.to_string(),
                start: start_time,
                end: end_time,
                confidence: 0.95,
            });
        }

        Ok((word_timestamps, AlignmentQuality::CtcForced))
    }

    pub fn score_consistency(&self, pcm: &[i16], _sample_rate: u32, text: &str) -> Result<f64, String> {
        if self.session.is_none() || text.trim().is_empty() || pcm.is_empty() {
            return Ok(-5.0);
        }

        let mut session_guard = self
            .session
            .as_ref()
            .ok_or_else(|| "Aligner session missing".to_string())?
            .lock()
            .map_err(|e| format!("Aligner lock: {e}"))?;
        let input_name = session_guard
            .inputs()
            .first()
            .ok_or_else(|| "Aligner model exposes no inputs".to_string())?
            .name()
            .to_string();
        let output_name = session_guard
            .outputs()
            .first()
            .ok_or_else(|| "Aligner model exposes no outputs".to_string())?
            .name()
            .to_string();

        let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let input_nd = ndarray::Array2::from_shape_vec((1, f32_pcm.len()), f32_pcm)
            .map_err(|e| format!("Aligner input reshape: {e}"))?;
        let input_tensor = Tensor::from_array(input_nd).map_err(|e| format!("Aligner input tensor: {e}"))?;

        let outputs = session_guard
            .run(ort::inputs![
                input_name => input_tensor
            ])
            .map_err(|e| format!("Aligner inference: {e}"))?;

        let extract_res = outputs[output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Extract aligner output: {e}"))?;
        let (output_shape, logits) = extract_res;

        if output_shape.len() < 3 {
            return Ok(-5.0);
        }

        let vocab_size = output_shape[2] as usize;
        if vocab_size == 0 {
            return Ok(-5.0);
        }
        let blank_idx = self.tokens.iter().position(|t| t == "<pad>" || t == "_" || t == "<blank>").unwrap_or(0);

        let words: Vec<&str> = text.split_whitespace().collect();
        let mut target_tokens = Vec::new();

        for &w in &words {
            for c in w.chars() {
                let char_str = c.to_lowercase().to_string();
                if let Some(idx) = self.tokens.iter().position(|t| t == &char_str) {
                    if idx < vocab_size && idx != blank_idx {
                        target_tokens.push(idx);
                    }
                }
            }
        }

        if target_tokens.is_empty() {
            return Ok(-5.0);
        }

        let score = forward_backward_ctc_score(logits, vocab_size, &target_tokens, blank_idx);
        Ok(score)
    }
}

fn get_log_prob(logits: &[f32], vocab_size: usize, frame: usize, token: usize) -> f32 {
    let offset = frame * vocab_size;
    let mut max_val = f32::NEG_INFINITY;
    for i in 0..vocab_size {
        let val = logits[offset + i];
        if val > max_val {
            max_val = val;
        }
    }
    let mut sum_exp = 0.0f32;
    for i in 0..vocab_size {
        sum_exp += (logits[offset + i] - max_val).exp();
    }
    logits[offset + token] - max_val - sum_exp.ln()
}

fn ctc_align(logits: &[f32], vocab_size: usize, target_tokens: &[usize], blank_idx: usize) -> (Vec<usize>, f32) {
    if vocab_size == 0 {
        return (Vec::new(), f32::NEG_INFINITY);
    }
    let num_frames = logits.len() / vocab_size;
    if num_frames == 0 || target_tokens.is_empty() {
        return (Vec::new(), f32::NEG_INFINITY);
    }

    let mut target_states = Vec::with_capacity(target_tokens.len() * 2 + 1);
    for &tok in target_tokens {
        target_states.push(blank_idx);
        target_states.push(tok);
    }
    target_states.push(blank_idx);

    let num_states = target_states.len();
    let mut dp = vec![vec![f32::NEG_INFINITY; num_states]; num_frames];
    let mut backtrack = vec![vec![0usize; num_states]; num_frames];

    dp[0][0] = get_log_prob(logits, vocab_size, 0, target_states[0]);
    if num_states > 1 {
        dp[0][1] = get_log_prob(logits, vocab_size, 0, target_states[1]);
    }

    for f in 1..num_frames {
        for s in 0..num_states {
            let mut best_val = dp[f - 1][s];
            let mut best_prev = s;

            if s > 0 {
                let val = dp[f - 1][s - 1];
                if val > best_val {
                    best_val = val;
                    best_prev = s - 1;
                }
            }

            if s > 1 && target_states[s] != blank_idx && target_states[s] != target_states[s - 2] {
                let val = dp[f - 1][s - 2];
                if val > best_val {
                    best_val = val;
                    best_prev = s - 2;
                }
            }

            if best_val > f32::NEG_INFINITY {
                dp[f][s] = best_val + get_log_prob(logits, vocab_size, f, target_states[s]);
                backtrack[f][s] = best_prev;
            }
        }
    }

    let mut path = vec![0usize; num_frames];
    let mut best_s = 0;
    let mut best_val = f32::NEG_INFINITY;
    for (s, &val) in dp[num_frames - 1].iter().enumerate().take(num_states).skip(num_states.saturating_sub(2)) {
        if val > best_val {
            best_val = val;
            best_s = s;
        }
    }

    let mut cur_s = best_s;
    for f in (0..num_frames).rev() {
        path[f] = cur_s;
        cur_s = backtrack[f][cur_s];
    }

    (path, best_val)
}

fn fallback_align(pcm: &[i16], sample_rate: u32, text: &str) -> Vec<WordTimestamp> {
    let duration = pcm.len() as f64 / sample_rate as f64;
    let words: Vec<&str> = text.split_whitespace().collect();
    let per_word = duration / words.len().max(1) as f64;
    words
        .iter()
        .enumerate()
        .map(|(i, w)| WordTimestamp {
            word: w.to_string(),
            start: i as f64 * per_word,
            end: (i + 1) as f64 * per_word,
            confidence: 0.5,
        })
        .collect()
}

pub fn align(pcm: &[i16], sample_rate: u32, text: &str) -> Result<Vec<WordTimestamp>, Box<dyn std::error::Error>> {
    Ok(fallback_align(pcm, sample_rate, text))
}

pub fn score_consistency(_pcm: &[i16], _sample_rate: u32, _text: &str) -> Result<f64, String> {
    Ok(-5.0)
}

fn log_sum_exp(a: f32, b: f32) -> f32 {
    if a == f32::NEG_INFINITY {
        return b;
    }
    if b == f32::NEG_INFINITY {
        return a;
    }
    let max_val = a.max(b);
    max_val + ((a - max_val).exp() + (b - max_val).exp()).ln()
}

pub fn forward_backward_ctc_score(logits: &[f32], vocab_size: usize, target_tokens: &[usize], blank_idx: usize) -> f64 {
    if vocab_size == 0 {
        return -20.0;
    }
    let num_frames = logits.len() / vocab_size;
    if num_frames == 0 || target_tokens.is_empty() {
        return -20.0;
    }

    let mut target_states = Vec::with_capacity(target_tokens.len() * 2 + 1);
    for &tok in target_tokens {
        target_states.push(blank_idx);
        target_states.push(tok);
    }
    target_states.push(blank_idx);
    let num_states = target_states.len();

    let mut log_probs = vec![vec![0.0f32; num_states]; num_frames];
    for (f, row) in log_probs.iter_mut().enumerate().take(num_frames) {
        for (s, cell) in row.iter_mut().enumerate().take(num_states) {
            *cell = get_log_prob(logits, vocab_size, f, target_states[s]);
        }
    }

    let mut alpha = vec![vec![f32::NEG_INFINITY; num_states]; num_frames];
    alpha[0][0] = log_probs[0][0];
    if num_states > 1 {
        alpha[0][1] = log_probs[0][1];
    }

    for t in 1..num_frames {
        for s in 0..num_states {
            let mut val = alpha[t - 1][s];
            if s > 0 {
                val = log_sum_exp(val, alpha[t - 1][s - 1]);
            }
            if s > 1 && target_states[s] != blank_idx && target_states[s] != target_states[s - 2] {
                val = log_sum_exp(val, alpha[t - 1][s - 2]);
            }
            if val != f32::NEG_INFINITY {
                alpha[t][s] = val + log_probs[t][s];
            }
        }
    }

    let mut beta = vec![vec![f32::NEG_INFINITY; num_states]; num_frames];
    beta[num_frames - 1][num_states - 1] = 0.0;
    if num_states > 1 {
        beta[num_frames - 1][num_states - 2] = 0.0;
    }

    for t in (0..num_frames - 1).rev() {
        for s in 0..num_states {
            let mut val = beta[t + 1][s] + log_probs[t + 1][s];
            if s + 1 < num_states {
                val = log_sum_exp(val, beta[t + 1][s + 1] + log_probs[t + 1][s + 1]);
            }
            if s + 2 < num_states && target_states[s] != blank_idx && target_states[s] != target_states[s + 2] {
                val = log_sum_exp(val, beta[t + 1][s + 2] + log_probs[t + 1][s + 2]);
            }
            beta[t][s] = val;
        }
    }

    let mut log_total_prob = f32::NEG_INFINITY;
    for s in 0..num_states {
        let a = alpha[0][s];
        let b = beta[0][s];
        if a != f32::NEG_INFINITY && b != f32::NEG_INFINITY {
            let term = a + b - log_probs[0][s];
            log_total_prob = log_sum_exp(log_total_prob, term);
        }
    }

    if log_total_prob == f32::NEG_INFINITY {
        log_total_prob = alpha[num_frames - 1][num_states - 1];
        if num_states > 1 {
            log_total_prob = log_sum_exp(log_total_prob, alpha[num_frames - 1][num_states - 2]);
        }
    }

    log_total_prob as f64 / num_frames as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_sum_exp() {
        assert_eq!(log_sum_exp(f32::NEG_INFINITY, 0.0), 0.0);
        assert_eq!(log_sum_exp(0.0, f32::NEG_INFINITY), 0.0);
        let val = log_sum_exp(0.0, 0.0);
        assert!((val - 2.0f32.ln()).abs() < 1e-5);
    }

    #[test]
    fn test_forward_backward_score() {
        let logits = vec![0.5, 0.2, 0.3, 0.1, 0.8, 0.1, 0.2, 0.2, 0.6];
        let target_tokens = vec![1, 2];
        let score = forward_backward_ctc_score(&logits, 3, &target_tokens, 0);
        assert!(score > f64::MIN);
        assert!(score <= 0.0);
    }

    #[test]
    fn align_reports_energy_heuristic_when_no_model_is_loaded() {
        // With no mms_aligner.onnx present, ForcedAligner falls back to linear timestamps.
        // The provenance label MUST be energy_heuristic, never ctc_forced — otherwise the
        // published dataset claims real forced alignment for heuristic output.
        let tmp = tempfile::tempdir().expect("tempdir");
        let aligner = ForcedAligner::new(tmp.path(), false).expect("aligner");
        assert!(!aligner.is_available(), "no model should be available in an empty dir");

        let pcm = vec![0i16; 16_000];
        let (timestamps, quality) = aligner.align(&pcm, 16_000, "سڵاو جیهان").expect("align");
        assert_eq!(quality, AlignmentQuality::EnergyHeuristic);
        assert_eq!(quality.as_db_str(), "energy_heuristic");
        assert!(!timestamps.is_empty(), "fallback still yields per-word timestamps");
    }

    #[test]
    fn ctc_functions_are_total_on_zero_vocab() {
        // A corrupt-but-loadable ONNX model can report vocab_size = 0 (output dim 2);
        // `logits.len() / vocab_size` would divide-by-zero panic on a pipeline thread.
        // Both CTC kernels must return their no-result sentinel instead of panicking.
        let logits = vec![0.1f32, 0.2, 0.3];
        assert_eq!(ctc_align(&logits, 0, &[1], 0), (Vec::new(), f32::NEG_INFINITY));
        assert_eq!(forward_backward_ctc_score(&logits, 0, &[1], 0), -20.0);
    }
}
