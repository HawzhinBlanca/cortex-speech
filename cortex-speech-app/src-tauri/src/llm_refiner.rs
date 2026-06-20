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

    fn call_provider(&self, user_content: &str) -> Result<String, String> {
        match self.provider {
            LlmProvider::Local => self.call_openai_compatible(user_content),
            LlmProvider::Gemini => self.call_gemini(user_content),
        }
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

        let body: Value = resp.into_json().map_err(|e| format!("Failed to parse JSON response: {}", e))?;

        if let Some(content) = body["choices"][0]["message"]["content"].as_str() {
            Ok(content.trim().to_string())
        } else {
            Err("Invalid response format from Local LLM".to_string())
        }
    }

    fn call_gemini(&self, user_content: &str) -> Result<String, String> {
        if self.api_key.trim().is_empty() {
            return Err("Gemini API Key is missing. Please configure it in settings.".to_string());
        }

        let model_name = if self.model.trim().is_empty() { "gemini-3.1-pro-latest" } else { &self.model };

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

        let body: Value = resp.into_json().map_err(|e| format!("Failed to parse Gemini response: {}", e))?;

        if let Some(content) = body["candidates"][0]["content"]["parts"][0]["text"].as_str() {
            Ok(content.trim().to_string())
        } else {
            Err("Invalid response format from Gemini".to_string())
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
         candidates do not support.",
    );
    out
}

#[cfg(test)]
mod tests {
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
}
