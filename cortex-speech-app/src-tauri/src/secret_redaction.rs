const REDACTED_API_KEY: &str = "[REDACTED_API_KEY]";

pub(crate) fn redact_api_key(message: &str, api_key: &str) -> String {
    let trimmed_key = api_key.trim();
    let redacted =
        if trimmed_key.is_empty() { message.to_string() } else { message.replace(trimmed_key, REDACTED_API_KEY) };
    redact_query_param(&redacted, "key")
}

fn redact_query_param(message: &str, param: &str) -> String {
    let needle = format!("{param}=");
    let mut output = String::with_capacity(message.len());
    let mut cursor = 0;

    while let Some(relative_pos) = message[cursor..].find(&needle) {
        let start = cursor + relative_pos;
        if !is_query_param_start(message.as_bytes(), start) {
            output.push_str(&message[cursor..start + needle.len()]);
            cursor = start + needle.len();
            continue;
        }

        let value_start = start + needle.len();
        output.push_str(&message[cursor..value_start]);
        output.push_str(REDACTED_API_KEY);

        let value_end =
            message[value_start..].find(is_query_param_delimiter).map(|pos| value_start + pos).unwrap_or(message.len());
        cursor = value_end;
    }

    output.push_str(&message[cursor..]);
    output
}

fn is_query_param_start(bytes: &[u8], start: usize) -> bool {
    start == 0 || matches!(bytes[start - 1], b'?' | b'&' | b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'')
}

fn is_query_param_delimiter(ch: char) -> bool {
    matches!(ch, '&' | ' ' | '\t' | '\n' | '\r' | '"' | '\'' | ')' | '>' | '<')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_exact_api_key() {
        let message = "Gemini failed for key AIzaSySECRET and Authorization AIzaSySECRET";

        let redacted = redact_api_key(message, "AIzaSySECRET");

        assert!(!redacted.contains("AIzaSySECRET"));
        assert!(redacted.contains(REDACTED_API_KEY));
    }

    #[test]
    fn redacts_local_llm_bearer_token_when_secret_is_known() {
        let message = "Local LLM failed with Authorization: Bearer local-secret-token";

        let redacted = redact_api_key(message, "local-secret-token");

        assert!(!redacted.contains("local-secret-token"));
        assert_eq!(redacted, "Local LLM failed with Authorization: Bearer [REDACTED_API_KEY]");
    }

    #[test]
    fn redacts_key_query_value_without_known_secret() {
        let message = "POST https://example.test/v1/models/gemini:generateContent?key=AIzaSySECRET&alt=json failed";

        let redacted = redact_api_key(message, "");

        assert_eq!(
            redacted,
            "POST https://example.test/v1/models/gemini:generateContent?key=[REDACTED_API_KEY]&alt=json failed"
        );
    }

    #[test]
    fn does_not_redact_non_key_words() {
        let message = "monkey=value and keyboard=value are not API key query params";

        let redacted = redact_api_key(message, "");

        assert_eq!(redacted, message);
    }

    // --- Multi-byte / Sorani robustness ------------------------------------
    // Redaction runs over arbitrary log lines, which in this app routinely carry
    // Sorani (every character is multi-byte UTF-8). The byte-offset arithmetic here
    // must stay on char boundaries: a regression to naive byte slicing would either
    // panic or, worse, slice a key in half and leak it. These pin the invariant.

    #[test]
    fn redacts_query_key_embedded_in_sorani_text() {
        // A real-looking failure log: Sorani prose on both sides of the URL.
        let message = "هەڵە لە داواکاری ?key=AIzaSyLEAK&alt=json بۆ مۆدێل";
        let redacted = redact_api_key(message, "");
        assert!(!redacted.contains("AIzaSyLEAK"), "secret must be gone: {redacted}");
        assert!(redacted.contains(REDACTED_API_KEY));
        // Surrounding Sorani must survive intact (proves boundary-correct slicing).
        assert!(redacted.contains("هەڵە لە داواکاری"));
        assert!(redacted.contains("بۆ مۆدێل"));
    }

    #[test]
    fn sorani_letter_directly_before_key_is_not_a_query_param() {
        // "key=" glued to a Sorani letter (no delimiter) is NOT a query param start.
        // This exercises the bytes[start-1] read landing on a UTF-8 continuation byte:
        // it must compare false against the ASCII delimiters, leaving the text untouched.
        let message = "ووشەkey=AIzaSyKEEP";
        let redacted = redact_api_key(message, "");
        assert_eq!(redacted, message, "must not redact a non-delimited key= and must not panic");
    }

    #[test]
    fn redacts_exact_key_surrounded_by_sorani() {
        let message = "سەرکەوتوو نەبوو بە کلیلی AIzaSySECRET لە کاتی پرۆسێس";
        let redacted = redact_api_key(message, "AIzaSySECRET");
        assert!(!redacted.contains("AIzaSySECRET"));
        assert!(redacted.contains(REDACTED_API_KEY));
        assert!(redacted.contains("سەرکەوتوو نەبوو"));
    }

    #[test]
    fn empty_and_pathological_inputs_never_panic() {
        // No boundary, no key, just multibyte and edge cases — must return, not panic.
        for msg in ["", "ـ", "key=", "key=کلیل", "؟key=AIza", "key=key=key=", "ک"] {
            let _ = redact_api_key(msg, "");
            let _ = redact_api_key(msg, "AIza");
        }
    }
}
