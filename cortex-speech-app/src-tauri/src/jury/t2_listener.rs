//! jury/t2_listener.rs — Gemini audio listening jury
//!
//! Receives the contested audio span + all T1 evidence, sends it to
//! Gemini 2.5 Pro with the audio inline, and returns a structured verdict.
//! Self-consistency N=3 guards against overconfidence.

use crate::db::SegmentHypothesis;
use crate::gemini_api;
use crate::jury::t1_judge::Evidence;
use crate::secret_redaction::redact_api_key;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ─── Types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T2Verdict {
    pub transcript: String,
    pub reason: String,
    pub confidence: f64,
    pub evidence: Vec<Evidence>,
    pub self_consistency_agreement: bool,
    /// Number of samples that agreed on the winning transcript.
    pub votes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T2Result {
    pub verdict: Option<T2Verdict>,
    /// If true, self-consistency failed and the segment must go to the human inbox.
    pub must_escalate: bool,
    pub error: Option<String>,
}

// Single sample returned by one Gemini call
#[derive(Debug, Clone, Deserialize)]
struct GeminiSample {
    transcript: String,
    reason: String,
    confidence: f64,
}

// ─── Prompt helpers ──────────────────────────────────────────────────────────

fn build_system_prompt() -> &'static str {
    "You are an expert Sorani Kurdish linguist and speech transcription judge. \
     You will receive an audio clip and multiple ASR transcription hypotheses. \
     Listen carefully to the audio and determine which hypothesis is most accurate, \
     or provide a corrected transcription if all hypotheses are wrong. \
     Always respond in strict JSON format with exactly three keys: \
     'transcript' (the correct Sorani Kurdish text), \
     'reason' (a single sentence explaining your decision), \
     'confidence' (a number between 0.0 and 1.0). \
     Never add any text outside the JSON object."
}

fn build_user_prompt(
    hypotheses: &[SegmentHypothesis],
    t1_evidence: &[Evidence],
    few_shots: &[crate::jury::FewShotExample],
    call_index: usize,
) -> String {
    // Randomize hypothesis order to mitigate position bias (each call gets a different order)
    let mut order: Vec<usize> = (0..hypotheses.len()).collect();
    // Simple deterministic shuffle based on call_index
    let shift = if order.is_empty() { 0 } else { call_index % order.len() };
    order.rotate_left(shift);

    let mut hyp_block = String::new();
    for (rank, &idx) in order.iter().enumerate() {
        let h = &hypotheses[idx];
        hyp_block.push_str(&format!("Hypothesis {} (from {}): {}\n", rank + 1, h.model_id, h.transcript));
    }

    let evidence_block = if t1_evidence.is_empty() {
        "No prior tool evidence.".to_string()
    } else {
        t1_evidence
            .iter()
            .map(|e| format!("[{}] {} (supports: {})", e.tool, e.result, e.supports_hypothesis))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut few_shot_block = String::new();
    if !few_shots.is_empty() {
        few_shot_block.push_str("Here are examples of past human corrections to guide your judgment:\n");
        for (idx, ex) in few_shots.iter().enumerate() {
            few_shot_block.push_str(&format!(
                "Example {}:\n- Incorrect transcription/ASR: {}\n- Corrected transcription (Human Fix): {}\n\n",
                idx + 1,
                ex.wrong_transcript,
                ex.human_fix
            ));
        }
    }

    format!(
        "Listen to the audio clip and judge these transcription hypotheses:\n\
         {hyp_block}\n\
         Prior text-tool evidence:\n\
         {evidence_block}\n\n\
         {few_shot_block}\
         Respond ONLY with a JSON object: {{\"transcript\": \"...\", \"reason\": \"...\", \"confidence\": 0.0}}"
    )
}

// ─── Gemini API call ─────────────────────────────────────────────────────────

fn call_gemini_audio(
    audio_b64: &str,
    model: &str,
    api_key: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<GeminiSample, String> {
    let url = gemini_api::generate_content_url(model);

    let payload = json!({
        "systemInstruction": {
            "parts": [{ "text": system_prompt }]
        },
        "contents": [{
            "parts": [
                {
                    "inlineData": {
                        "mimeType": "audio/wav",
                        "data": audio_b64
                    }
                },
                { "text": user_prompt }
            ]
        }],
        "generationConfig": {
            "temperature": 0.1,
            "responseMimeType": "application/json"
        }
    });

    let resp = gemini_api::with_api_key(ureq::post(&url), api_key)
        .set("Content-Type", "application/json")
        .send_json(payload)
        .map_err(|e| format!("Gemini API request failed: {}", redact_api_key(&e.to_string(), api_key)))?;

    let body: serde_json::Value = resp.into_json().map_err(|e| format!("Failed to parse Gemini response: {e}"))?;

    let raw_text =
        body["candidates"][0]["content"]["parts"][0]["text"].as_str().ok_or("Missing text in Gemini response")?;

    serde_json::from_str::<GeminiSample>(raw_text)
        .map_err(|e| format!("Failed to parse Gemini JSON sample: {e}\nRaw: {raw_text}"))
}

// ─── Self-consistency voting ─────────────────────────────────────────────────

fn majority_vote(samples: &[GeminiSample]) -> Option<(String, usize)> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in samples {
        let transcript = s.transcript.trim();
        if transcript.is_empty() {
            continue;
        }
        *counts.entry(transcript.to_string()).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, c)| *c).filter(|(_, c)| *c > samples.len() / 2)
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Run the T2 listening jury with N-sample self-consistency.
///
/// - `audio_b64`: base-64 encoded WAV audio of the contested span.
/// - `hypotheses`: all candidate transcripts from T0/T1.
/// - `t1_evidence`: evidence gathered by the T1 tool judge.
/// - `api_key`: Gemini API key.
/// - `model`: e.g. `"gemini-2.5-pro"`.
/// - `n_samples`: self-consistency samples (default 3).
pub fn listen_and_judge(
    audio_b64: &str,
    hypotheses: &[SegmentHypothesis],
    t1_evidence: &[Evidence],
    few_shots: &[crate::jury::FewShotExample],
    api_key: &str,
    model: &str,
    n_samples: usize,
) -> T2Result {
    if api_key.trim().is_empty() {
        return T2Result { verdict: None, must_escalate: true, error: Some("Gemini API key not configured.".into()) };
    }

    let sys = build_system_prompt();
    let mut samples: Vec<GeminiSample> = Vec::with_capacity(n_samples);

    for i in 0..n_samples {
        let prompt = build_user_prompt(hypotheses, t1_evidence, few_shots, i);
        match call_gemini_audio(audio_b64, model, api_key, sys, &prompt) {
            Ok(s) => samples.push(s),
            Err(e) => {
                tracing::warn!("T2 sample {i} failed: {e}");
                // Continue — partial agreement is still meaningful
            }
        }
    }

    if samples.is_empty() {
        return T2Result { verdict: None, must_escalate: true, error: Some("All T2 Gemini calls failed.".into()) };
    }

    match majority_vote(&samples) {
        Some((winning_transcript, votes)) => {
            // Pick the sample whose transcript matches the winner for reason/confidence
            let Some(representative) = samples.iter().find(|s| s.transcript.trim() == winning_transcript) else {
                return T2Result {
                    verdict: None,
                    must_escalate: true,
                    error: Some("T2 majority winner had no representative sample.".into()),
                };
            };

            T2Result {
                verdict: Some(T2Verdict {
                    transcript: winning_transcript,
                    reason: representative.reason.clone(),
                    confidence: representative.confidence,
                    evidence: t1_evidence.to_vec(),
                    self_consistency_agreement: true,
                    votes,
                }),
                must_escalate: false,
                error: None,
            }
        }
        None => {
            let judge_a_transcript = samples.first().map(|s| s.transcript.as_str()).unwrap_or("");
            let judge_b_transcript = hypotheses
                .iter()
                .find(|h| h.model_id == "omniasr-ctc-1b")
                .or_else(|| hypotheses.get(1))
                .or_else(|| hypotheses.first())
                .map(|h| h.transcript.as_str())
                .unwrap_or("");
            let judge_a_swapped = samples.get(1).map(|s| s.transcript.as_str()).unwrap_or(judge_a_transcript);
            let judge_b_swapped = judge_b_transcript;

            let debate_res = crate::jury::debate::adjudicate(
                judge_a_transcript,
                judge_b_transcript,
                judge_a_swapped,
                judge_b_swapped,
            );

            if !debate_res.must_escalate {
                T2Result {
                    verdict: Some(T2Verdict {
                        transcript: debate_res.winning_transcript,
                        reason: debate_res.reason,
                        confidence: 0.85,
                        evidence: t1_evidence.to_vec(),
                        self_consistency_agreement: false,
                        votes: 1,
                    }),
                    must_escalate: false,
                    error: None,
                }
            } else {
                T2Result {
                    verdict: None,
                    must_escalate: true,
                    error: Some(format!(
                        "T2 self-consistency failed and debate could not resolve: {}",
                        debate_res.reason
                    )),
                }
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_majority_vote_clear_winner() {
        let samples = vec![
            GeminiSample { transcript: "کوردستان".into(), reason: "".into(), confidence: 0.9 },
            GeminiSample { transcript: "کوردستان".into(), reason: "".into(), confidence: 0.8 },
            GeminiSample { transcript: "ئێران".into(), reason: "".into(), confidence: 0.5 },
        ];
        let result = majority_vote(&samples);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, "کوردستان");
    }

    #[test]
    fn test_majority_vote_no_majority() {
        let samples = vec![
            GeminiSample { transcript: "کوردستان".into(), reason: "".into(), confidence: 0.9 },
            GeminiSample { transcript: "ئێران".into(), reason: "".into(), confidence: 0.9 },
        ];
        // 1/2 each — no strict majority
        let result = majority_vote(&samples);
        assert!(result.is_none());
    }

    #[test]
    fn test_listen_and_judge_no_key() {
        let result = listen_and_judge("", &[], &[], &[], "", "gemini-2.5-pro", 3);
        assert!(result.must_escalate);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_majority_vote_rejects_blank_winner() {
        let samples = vec![
            GeminiSample { transcript: "".into(), reason: "blank".into(), confidence: 0.9 },
            GeminiSample { transcript: "   ".into(), reason: "blank".into(), confidence: 0.8 },
            GeminiSample { transcript: "valid transcript".into(), reason: "minority".into(), confidence: 0.7 },
        ];

        let result = majority_vote(&samples);

        assert!(result.is_none(), "blank transcripts must not form an accepted T2 majority");
    }

    #[test]
    fn test_prompt_order_varies() {
        use crate::db::SegmentHypothesis;
        let hyps = vec![
            SegmentHypothesis {
                segment_id: "s".into(),
                model_id: "m1".into(),
                transcript: "A".into(),
                confidence: None,
            },
            SegmentHypothesis {
                segment_id: "s".into(),
                model_id: "m2".into(),
                transcript: "B".into(),
                confidence: None,
            },
        ];
        let p0 = build_user_prompt(&hyps, &[], &[], 0);
        let p1 = build_user_prompt(&hyps, &[], &[], 1);
        // Different rotation — at least the order should differ
        assert_ne!(p0, p1);
    }
}
