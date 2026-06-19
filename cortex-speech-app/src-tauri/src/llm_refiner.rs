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

        match self.provider {
            LlmProvider::Local => self.call_openai_compatible(raw_text),
            LlmProvider::Gemini => self.call_gemini(raw_text),
        }
    }

    fn call_openai_compatible(&self, raw_text: &str) -> Result<String, String> {
        let model_name = if self.model.trim().is_empty() { "omniASR_LLM_7B_v2" } else { &self.model };

        let payload = json!({
            "model": model_name,
            "messages": [
                { "role": "system", "content": &self.system_prompt },
                { "role": "user", "content": format!("Fix the following Kurdish text:\n{}", raw_text) }
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

    fn call_gemini(&self, raw_text: &str) -> Result<String, String> {
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
                        { "text": format!("Fix the following Kurdish text:\n{}", raw_text) }
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
