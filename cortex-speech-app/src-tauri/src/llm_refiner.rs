use crate::gemini_api;
use crate::secret_redaction::redact_api_key;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmProvider {
    Local,
    Gemini,
}

pub struct LlmRefiner {
    pub provider: LlmProvider,
    pub endpoint: String,
    pub api_key: String,
    pub system_prompt: String,
    pub model: String,
}

/// Is this refinement error worth another attempt?
///
/// TRANSIENT = the same request could succeed shortly: rate limiting, provider-side 5xx, a timeout or
/// dropped connection, and "no message content", which is how an overloaded OpenRouter reply arrives
/// once the JSON parses but carries no choice.
///
/// NOT transient: 401/403 (a bad key stays bad), 404 and "model" errors (an unknown model id will not
/// appear), and content refusals. Retrying those only delays an honest stop.
fn is_transient(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    if e.contains("401") || e.contains("403") || e.contains("unauthorized") || e.contains("api key") {
        return false;
    }
    if e.contains("404") || e.contains("not a valid model") || e.contains("no endpoints found") {
        return false;
    }
    // A 429 is USUALLY throttling, but the billing/quota family is permanent for this run: no amount
    // of backoff refills a depleted balance. MEASURED 2026-08-12 on the real API — "Your prepayment
    // credits are depleted" arrives as HTTP 429, so the blanket 429 rule below spent 4 attempts and
    // 7 s of backoff PER CLIP before failing, and reported it as a retry exhaustion rather than as
    // the billing problem it is. Checked before the 429 rule, like the other permanent classes.
    if e.contains("credits are depleted")
        || e.contains("insufficient credit")
        || e.contains("quota exceeded")
        || e.contains("exceeded your current quota")
        || e.contains("billing")
    {
        return false;
    }
    e.contains("429")
        || e.contains("rate limit")
        || e.contains("timed out")
        || e.contains("timeout")
        || e.contains("connection")
        || e.contains("temporarily")
        || e.contains("overloaded")
        || e.contains("no message content")
        || e.contains(" 500")
        || e.contains(" 502")
        || e.contains(" 503")
        || e.contains(" 504")
}

impl LlmRefiner {
    pub fn new(
        mode: &crate::settings::LlmMode,
        endpoint: String,
        api_key: String,
        system_prompt: String,
        model: String,
    ) -> Option<Self> {
        let provider = match mode {
            crate::settings::LlmMode::None => return None,
            crate::settings::LlmMode::Local => LlmProvider::Local,
            crate::settings::LlmMode::Gemini => LlmProvider::Gemini,
        };
        Some(Self { provider, endpoint, api_key, system_prompt, model })
    }

    /// Build a refiner backed by OpenRouter — the VERIFIED gateway to Gemini 2.5 Pro, GPT-4o-mini,
    /// etc. OpenRouter speaks the OpenAI-compatible API, so this is the `Local` provider pointed at
    /// OpenRouter's endpoint with the user's OpenRouter key. Returns `None` if no key. This is the
    /// way around the direct-Gemini 429 quota block. Default model (when unset) is the reliable,
    /// cheap `openai/gpt-4o-mini`; pass `google/gemini-2.5-pro` for Gemini-class.
    pub fn for_openrouter(api_key: String, model: String, system_prompt: String) -> Option<Self> {
        if api_key.trim().is_empty() {
            return None;
        }
        let model = if model.trim().is_empty() { "openai/gpt-4o-mini".to_string() } else { model };
        Some(Self {
            provider: LlmProvider::Local,
            endpoint: "https://openrouter.ai/api/v1/chat/completions".to_string(),
            api_key,
            system_prompt,
            model,
        })
    }

    pub fn refine_text(&self, raw_text: &str) -> Result<String, String> {
        if raw_text.trim().is_empty() {
            return Ok(raw_text.to_string());
        }
        let user_content = format!("Fix the following Kurdish text:\n{raw_text}");
        self.call_provider(&user_content)
    }

    /// Generative error correction primed on the diverse N-best hypotheses + relevant past
    /// corrections — the LOOP-1 upgrade over single-string refinement. The hypotheses fail
    /// differently, so their agreement localizes errors; the few-shot pairs prime the model on this
    /// curator's actual corrections.
    pub fn refine_with_context(
        &self,
        primary: &str,
        hypotheses: &[String],
        few_shot: &[(String, String)],
    ) -> Result<String, String> {
        if primary.trim().is_empty() {
            return Ok(primary.to_string());
        }
        let user_content = build_ger_user_prompt(primary, hypotheses, few_shot);
        self.call_provider(&user_content)
    }

    /// Attempts per refinement call: the first try plus 3 retries.
    const MAX_ATTEMPTS: u32 = 4;

    fn call_provider(&self, user_content: &str) -> Result<String, String> {
        // Retry transient provider failures before giving up, because the caller HARD STOPS on the
        // error (owner rule 2026-08-11) and halting a 487-clip run on one rate-limit reply would be
        // brittle, not strict.
        //
        // Measured 2026-08-10: 59 of 487 clips failed refinement during a run with 8 concurrent
        // workers — a ~12% failure rate that appeared only once the batch became concurrent, which is
        // the signature of provider rate limiting rather than bad input. There was no retry anywhere:
        // API_AGENT is a bare ureq agent.
        //
        // Exponential backoff (1s, 2s, 4s) and only for TRANSIENT classes. A 401, an unknown model or
        // a refusal is not going to change on the third ask, and retrying it just delays an honest
        // stop by seven seconds.
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = match self.provider {
                LlmProvider::Local => self.call_openai_compatible(user_content),
                LlmProvider::Gemini => self.call_gemini(user_content),
            };
            match result {
                Ok(text) => return Ok(text),
                Err(e) if attempt < Self::MAX_ATTEMPTS && is_transient(&e) => {
                    let wait = std::time::Duration::from_secs(1 << (attempt - 1));
                    tracing::warn!(
                        "LLM refinement attempt {attempt}/{} failed transiently ({e}); retrying in {:?}",
                        Self::MAX_ATTEMPTS,
                        wait
                    );
                    std::thread::sleep(wait);
                }
                Err(e) => return Err(if attempt > 1 { format!("{e} (after {attempt} attempts)") } else { e }),
            }
        }
    }

    /// Transcribe audio through the OpenAI-compatible endpoint (OpenRouter), which carries audio to
    /// Gemini 2.5 Pro via the `input_audio` content part.
    ///
    /// Lives HERE, beside `call_openai_compatible`, because this module is what the cloud-privacy
    /// gate audits: the key goes in the `Authorization` header and every provider error is passed
    /// through `redact_api_key` before it can reach a log or a UI. A caller assembling its own
    /// request would sit outside both guarantees.
    ///
    /// Measured need (2026-08-12): the DIRECT Gemini key's billing project reports "prepayment
    /// credits are depleted" (HTTP 429) on every model, while the OpenRouter key works — so this is
    /// the live path to Gemini-class audio. `temperature: 0` keeps a benchmark reproducible; an
    /// EMPTY reply is an error, never an empty transcript (scoring "" as a transcription would
    /// fabricate a CER).
    pub fn transcribe_audio(&self, audio_bytes: &[u8], format: &str, prompt: &str) -> Result<String, String> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(audio_bytes);
        let payload = json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt },
                    { "type": "input_audio", "input_audio": { "data": b64, "format": format } }
                ]
            }],
            "temperature": 0
        });

        let mut req = crate::http::API_AGENT.post(&self.endpoint).set("Content-Type", "application/json");
        if !self.api_key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.api_key));
        }
        let resp = req.send_json(payload).map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                let detail = r.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<Value>(&detail)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| detail.chars().take(300).collect());
                redact_api_key(&format!("audio transcription failed: status {code}: {msg}"), &self.api_key)
            }
            other => redact_api_key(&format!("audio transcription failed: {other}"), &self.api_key),
        })?;

        let body: Value = resp.into_json().map_err(|e| format!("bad json: {e}"))?;
        let text = body["choices"][0]["message"]["content"].as_str().unwrap_or_default().trim().to_string();
        if text.is_empty() {
            let reason = body["choices"][0]["finish_reason"].as_str().unwrap_or("no message content");
            return Err(format!("empty transcript (finish_reason: {reason})"));
        }
        Ok(text)
    }

    fn call_openai_compatible(&self, user_content: &str) -> Result<String, String> {
        let model_name = if self.model.trim().is_empty() { "omniASR_LLM_7B_v2" } else { &self.model };

        let payload = json!({
            "model": model_name,
            "messages": [
                { "role": "system", "content": &self.system_prompt },
                { "role": "user", "content": user_content }
            ],
            "temperature": 0.1
        });

        let mut req = crate::http::API_AGENT.post(&self.endpoint).set("Content-Type", "application/json");

        if !self.api_key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.api_key));
        }

        let resp = req
            .send_json(payload)
            .map_err(|e| format!("Local LLM request failed: {}", redact_api_key(&e.to_string(), &self.api_key)))?;

        let body: Value =
            crate::http::json_bounded(resp).map_err(|e| format!("Failed to parse JSON response: {}", e))?;

        if let Some(content) = body["choices"][0]["message"]["content"].as_str() {
            Ok(content.trim().to_string())
        } else {
            // Say WHAT came back instead of "invalid format". Measured 2026-08-10: 59 clips failed
            // here and the message named neither the provider nor the reason, so diagnosing it meant
            // reading pipeline source to discover the request had gone to OpenRouter at all. The
            // provider's own `error.message` is the useful field; the endpoint host names who
            // answered. Truncated, and run through redact_api_key, because a provider error body can
            // echo the request.
            let provider = self.endpoint.split('/').nth(2).unwrap_or("the LLM endpoint");
            let detail = body["error"]["message"]
                .as_str()
                .map(|m| m.to_string())
                .unwrap_or_else(|| body.to_string().chars().take(300).collect());
            Err(format!("{provider} returned no message content: {}", redact_api_key(&detail, &self.api_key)))
        }
    }

    fn call_gemini(&self, user_content: &str) -> Result<String, String> {
        if self.api_key.trim().is_empty() {
            return Err("Gemini API Key is missing. Please configure it in settings.".to_string());
        }

        // Round-23 #10: default to a model id that actually exists on the v1beta REST surface. The
        // previous "gemini-3.1-pro-latest" is not a published model, so an unconfigured direct-Gemini
        // refiner 404'd on every call. Match the jury/source-reference default.
        let model_name = if self.model.trim().is_empty() { "gemini-2.5-pro" } else { &self.model };

        let url = gemini_api::generate_content_url(model_name);

        let payload = json!({
            "systemInstruction": {
                "parts": [
                    { "text": &self.system_prompt }
                ]
            },
            "contents": [
                {
                    "parts": [
                        { "text": user_content }
                    ]
                }
            ],
            "generationConfig": {
                "temperature": 0.1
            }
        });

        let resp = gemini_api::with_api_key(crate::http::API_AGENT.post(&url), &self.api_key)
            .set("Content-Type", "application/json")
            .send_json(payload)
            .map_err(|e| format!("Gemini API request failed: {}", redact_api_key(&e.to_string(), &self.api_key)))?;

        let body: Value =
            crate::http::json_bounded(resp).map_err(|e| format!("Failed to parse Gemini response: {}", e))?;

        // Concatenate ALL text parts — Gemini can split a refined transcript across content.parts[0..N]
        // (and a 2.5-class "thinking" model may emit a leading thought part); reading parts[0] alone
        // would truncate the refinement or return the thought instead of the answer.
        let joined = body["candidates"][0]["content"]["parts"]
            .as_array()
            .map(|ps| ps.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        let trimmed = joined.trim();
        if trimmed.is_empty() {
            Err("Invalid response format from Gemini".to_string())
        } else {
            Ok(trimmed.to_string())
        }
    }
}

/// Build a HyPoradise-style generative-error-correction user prompt: the diverse N-best hypotheses
/// (deduped, non-empty) plus a few relevant past corrections, asking the model to output ONLY the
/// corrected Sorani transcript and to prefer what the candidates agree on — so it does not invent
/// content the acoustics do not support (the main hallucination risk for rare Sorani names).
pub fn build_ger_user_prompt(primary: &str, hypotheses: &[String], few_shot: &[(String, String)]) -> String {
    let mut out = String::from("You are correcting a Central Kurdish (Sorani) ASR transcript.\n\n");

    if !few_shot.is_empty() {
        out.push_str("Past corrections (wrong -> right) for guidance:\n");
        for (wrong, right) in few_shot {
            out.push_str(&format!("- {wrong} -> {right}\n"));
        }
        out.push('\n');
    }

    let mut seen = std::collections::HashSet::new();
    let distinct: Vec<&String> =
        hypotheses.iter().filter(|h| !h.trim().is_empty() && seen.insert(h.trim().to_string())).collect();
    if !distinct.is_empty() {
        out.push_str(
            "Candidate transcriptions from different ASR systems (they fail differently, so their agreement is a strong signal):\n",
        );
        for (i, hyp) in distinct.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, hyp.trim()));
        }
        out.push('\n');
    }

    out.push_str("Primary transcript to correct:\n");
    out.push_str(primary.trim());
    out.push_str(
        "\n\nUsing the candidates and past corrections, output ONLY the corrected Sorani transcript, \
         with no explanation or quotes. Prefer what the candidates agree on; do not add content the \
         candidates do not support. The transcript is VERBATIM: if the speaker repeated a word (e.g. \
         'کە کە'), keep the repetition — never collapse repeated words, stutters, or fillers into one.",
    );
    out
}

#[cfg(test)]
mod tests {

    /// The retry classifier decides whether a HARD STOP fires. Both directions are real failures:
    /// treating a permanent error as transient delays an honest stop and burns quota; treating a rate
    /// limit as permanent halts a 487-clip run on one throttled reply.
    #[test]
    fn transient_errors_retry_and_permanent_ones_stop_immediately() {
        for permanent in [
            // Billing/quota exhaustion arrives as 429 but no backoff can fix it (measured on the
            // real Gemini API, 2026-08-12): retrying burns 7 s per clip and hides the real cause.
            "generativelanguage.googleapis.com status 429: Your prepayment credits are depleted.",
            "Local LLM request failed: status code 429 - quota exceeded",
            "openrouter.ai returned no message content: You exceeded your current quota, please check your billing",
            "openrouter.ai returned no message content: 401 Unauthorized",
            "Local LLM request failed: status code 403",
            "openrouter.ai returned no message content: not a valid model id",
            "request failed: status code 404",
            "Gemini API Key is missing. Please configure it in settings.",
        ] {
            assert!(!super::is_transient(permanent), "must NOT retry: {permanent}");
        }

        for transient in [
            "Local LLM request failed: status code 429",
            "openrouter.ai returned no message content: rate limit exceeded",
            "request failed: status code 503",
            "Local LLM request failed: connection closed",
            "Local LLM request failed: read timed out",
            "openrouter.ai returned no message content: {}",
        ] {
            assert!(super::is_transient(transient), "must retry: {transient}");
        }
    }

    /// A 401 that merely mentions a rate limit in prose must still be permanent — the permanent
    /// checks run FIRST for exactly this reason.
    #[test]
    fn a_permanent_error_wins_over_a_transient_sounding_phrase() {
        assert!(!super::is_transient("401 Unauthorized (you may also be rate limited)"));
    }
    use super::*;

    #[test]
    fn ger_prompt_includes_primary_hypotheses_and_few_shot() {
        let prompt = build_ger_user_prompt(
            "ساڵی ٢٠١٥",
            &["ساڵی ٢٠١٥".to_string(), "ساڵی ٢٠١٤".to_string(), "  ".to_string(), "ساڵی ٢٠١٤".to_string()],
            &[("باش".to_string(), "خراپ".to_string())],
        );
        assert!(prompt.contains("ساڵی ٢٠١٥"), "primary present: {prompt}");
        assert!(prompt.contains("ساڵی ٢٠١٤"), "candidate present");
        assert!(prompt.contains("باش -> خراپ"), "few-shot present");
        assert!(prompt.to_lowercase().contains("only"), "instructs output-only");
        assert_eq!(prompt.matches("ساڵی ٢٠١٤").count(), 1, "blank + duplicate candidates collapse to one");
    }

    #[test]
    fn ger_prompt_omits_empty_sections() {
        let prompt = build_ger_user_prompt("دەق", &[], &[]);
        assert!(!prompt.contains("Past corrections"), "no few-shot section when none given");
        assert!(!prompt.contains("Candidate transcriptions"), "no candidates section when none given");
        assert!(prompt.contains("دەق"), "primary still present");
    }

    #[test]
    fn for_openrouter_targets_openrouter_with_a_default_model() {
        let r = LlmRefiner::for_openrouter("k".into(), String::new(), "p".into()).unwrap();
        assert_eq!(r.provider, LlmProvider::Local, "OpenRouter speaks the OpenAI-compatible API");
        assert!(r.endpoint.contains("openrouter.ai"), "{}", r.endpoint);
        assert_eq!(r.model, "openai/gpt-4o-mini", "empty model -> reliable default");
        let g = LlmRefiner::for_openrouter("k".into(), "google/gemini-2.5-pro".into(), "p".into()).unwrap();
        assert_eq!(g.model, "google/gemini-2.5-pro", "explicit model honored");
        assert!(LlmRefiner::for_openrouter("   ".into(), "m".into(), "p".into()).is_none(), "no key -> None");
    }
}
