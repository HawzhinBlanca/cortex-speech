pub(crate) const API_KEY_HEADER: &str = "x-goog-api-key";

pub(crate) fn generate_content_url(model: &str) -> String {
    let encoded_model = percent_encode_path_segment(model.trim());
    format!("https://generativelanguage.googleapis.com/v1beta/models/{encoded_model}:generateContent")
}

pub(crate) fn with_api_key(request: ureq::Request, api_key: &str) -> ureq::Request {
    request.set(API_KEY_HEADER, api_key)
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_unreserved_path_byte(byte) {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn is_unreserved_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Transcribe ONE audio file with a Gemini audio-capable model, returning the raw transcript text.
///
/// Lives HERE rather than in the calling binary on purpose: this module is what
/// `test_cloud_privacy_policy.py` audits for the two properties that matter — the API key travels in
/// the `x-goog-api-key` HEADER and never in the URL, and provider errors are redacted before they
/// surface. A caller that assembled its own request would be outside that audit, which is exactly
/// how a key ends up in a query string. The audio bytes go in the body; nothing secret is logged.
///
/// `prompt` is the caller's instruction (the verbatim law for this project). `temperature: 0` with
/// no thinking budget keeps the call deterministic — a benchmark that re-rolls its own number is not
/// a benchmark. An EMPTY reply is an error, never an empty transcript: silently returning "" would
/// let a refusal or a truncated response be scored as a real (terrible) transcription.
pub fn transcribe_audio(
    api_key: &str,
    model: &str,
    audio_bytes: &[u8],
    mime_type: &str,
    prompt: &str,
) -> Result<String, String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(audio_bytes);
    let body = serde_json::json!({
        "contents": [{
            "role": "user",
            "parts": [
                { "text": prompt },
                { "inline_data": { "mime_type": mime_type, "data": b64 } }
            ]
        }],
        // temperature 0 for reproducibility. NO thinkingConfig: gemini-2.5-pro REJECTS
        // `thinkingBudget: 0` with "Budget 0 is invalid. This model only works in thinking mode"
        // (measured 2026-08-12), so the model's own default is the only valid setting for it.
        "generationConfig": { "temperature": 0 }
    });

    let request = crate::http::API_AGENT.post(&generate_content_url(model)).set("Content-Type", "application/json");
    let response = with_api_key(request, api_key).send_json(body).map_err(|e| match e {
        ureq::Error::Status(code, r) => {
            let detail = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&detail)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| detail.chars().take(300).collect());
            crate::secret_redaction::redact_api_key(
                &format!("generativelanguage.googleapis.com status {code}: {msg}"),
                api_key,
            )
        }
        other => crate::secret_redaction::redact_api_key(&format!("request failed: {other}"), api_key),
    })?;

    let value: serde_json::Value = response.into_json().map_err(|e| format!("bad json: {e}"))?;
    let text = value["candidates"][0]["content"]["parts"][0]["text"].as_str().unwrap_or_default().trim().to_string();
    if text.is_empty() {
        let reason = value["candidates"][0]["finishReason"].as_str().unwrap_or("no text in response");
        return Err(format!("empty transcript (finishReason: {reason})"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_content_url_does_not_embed_api_key_query() {
        let url = generate_content_url("gemini-2.5-pro");

        assert_eq!(url, "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent");
        assert!(!url.contains("?key="));
    }

    #[test]
    fn api_key_header_name_matches_gemini_rest_auth() {
        assert_eq!(API_KEY_HEADER, "x-goog-api-key");
    }

    #[test]
    fn generate_content_url_percent_encodes_model_path_segment() {
        let url = generate_content_url(" gemini/pro?key=leak&alt=json ");

        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini%2Fpro%3Fkey%3Dleak%26alt%3Djson:generateContent"
        );
        assert!(!url.contains("?key="));
        assert!(!url.contains("&alt="));
    }

    #[test]
    fn generate_content_url_percent_encodes_unicode_model_text() {
        let url = generate_content_url("gemini-کوردی");

        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-%DA%A9%D9%88%D8%B1%D8%AF%DB%8C:generateContent"
        );
    }
}
