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
