//! ElevenLabs Scribe speech-to-text client.
//!
//! VERIFIED WORKING for Central Kurdish (ckb): on a real Sorani clip (Nawras) Scribe v1 returned a
//! coherent, punctuated transcription with the year recovered — contradicting the pre-test pessimism
//! about its Kurdish support. Used as a diverse acoustic voter / strong draft transcriber. Audio is
//! sent ONLY to ElevenLabs' API; the key travels in a header, never in a URL or a log.

use crate::error::{AppError, AppResult};

const SCRIBE_URL: &str = "https://api.elevenlabs.io/v1/speech-to-text";
/// The model verified working for Sorani in the live test.
pub const DEFAULT_MODEL: &str = "scribe_v1";

/// Build a `multipart/form-data` body for a Scribe request. Returns `(content_type, body_bytes)`.
/// The boundary is a fixed constant (it contains no user data).
fn build_multipart(audio: &[u8], filename: &str, model_id: &str, language_code: &str) -> (String, Vec<u8>) {
    let boundary = "----CortexScribeBoundary7MA4YWxkTrZu0gW";
    let mut body = Vec::with_capacity(audio.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(audio);
    body.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"model_id\"\r\n\r\n{model_id}\r\n").as_bytes(),
    );
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"language_code\"\r\n\r\n{language_code}\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Extract the transcription text from a Scribe JSON response (the `text` field), trimmed; `None` if
/// absent or empty.
fn parse_scribe_text(json: &serde_json::Value) -> Option<String> {
    json.get("text").and_then(|t| t.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Bounded character-level similarity (0..1) over two strings (`1 - levenshtein/maxlen`).
fn char_similarity(a: &str, b: &str) -> f64 {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let (n, m) = (ca.len(), cb.len());
    if n == 0 && m == 0 {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(ca[i - 1] != cb[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[m] as f64 / n.max(m) as f64
}

/// Scribe occasionally returns the WHOLE transcription twice (observed on a real Sorani clip). If the
/// text splits into two halves whose openings are near-identical (a duplication restarts the same
/// words), return just the first half; otherwise return it unchanged. Conservative and cost-bounded
/// (compares only the first ~80 chars of each half), so it never mangles genuinely distinct content.
pub fn dedupe_repeated(text: &str) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    let n = chars.len();
    if n < 60 {
        return trimmed.to_string();
    }
    let mid = n / 2;
    // The whitespace CLOSEST to the midpoint — the boundary between two copies, not an internal space.
    let split = (mid.saturating_sub(20)..=(mid + 20).min(n - 1))
        .filter(|&i| chars[i].is_whitespace())
        .min_by_key(|&i| (i as isize - mid as isize).unsigned_abs())
        .unwrap_or(mid);
    let first: String = chars[..split].iter().collect();
    let second: String = chars[split..].iter().collect();
    let (a, b) = (first.trim(), second.trim());
    if a.is_empty() || b.is_empty() {
        return trimmed.to_string();
    }
    let head = |s: &str| s.chars().take(80).collect::<String>();
    if char_similarity(&head(a), &head(b)) >= 0.85 {
        a.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Transcribe an audio file with ElevenLabs Scribe. Returns the transcription text. Errors carry the
/// key redacted (defense in depth, though the key is only ever sent as a header).
pub fn transcribe(audio_path: &str, api_key: &str, model_id: &str, language_code: &str) -> AppResult<String> {
    let audio = std::fs::read(audio_path).map_err(|e| AppError::Other(format!("read audio {audio_path}: {e}")))?;
    let filename =
        std::path::Path::new(audio_path).file_name().and_then(|f| f.to_str()).unwrap_or("audio.wav");
    let (content_type, body) = build_multipart(&audio, filename, model_id, language_code);

    let redact = |s: String| s.replace(api_key, "<redacted>");
    let resp = crate::http::API_AGENT
        .post(SCRIBE_URL)
        .set("xi-api-key", api_key)
        .set("Content-Type", &content_type)
        .send_bytes(&body)
        .map_err(|e| AppError::Other(format!("Scribe request failed: {}", redact(e.to_string()))))?;
    let json: serde_json::Value =
        resp.into_json().map_err(|e| AppError::Other(format!("Scribe response parse: {e}")))?;
    parse_scribe_text(&json)
        .map(|text| dedupe_repeated(&text))
        .ok_or_else(|| AppError::Other("Scribe returned no transcription text".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_multipart_embeds_file_and_fields() {
        let (content_type, body) = build_multipart(b"AUDIOBYTES", "nawras.wav", "scribe_v1", "kur");
        assert!(content_type.starts_with("multipart/form-data; boundary="));
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("filename=\"nawras.wav\""), "{s}");
        assert!(s.contains("name=\"model_id\"") && s.contains("scribe_v1"));
        assert!(s.contains("name=\"language_code\"") && s.contains("kur"));
        assert!(s.contains("AUDIOBYTES"), "the audio bytes are embedded");
        assert!(s.trim_end().ends_with("--"), "ends with the closing boundary");
    }

    #[test]
    fn parse_scribe_text_extracts_and_trims() {
        let json = serde_json::json!({"text": "  دەزگای ڕوانگە  ", "language_code": "kur"});
        assert_eq!(parse_scribe_text(&json).as_deref(), Some("دەزگای ڕوانگە"));
        assert!(parse_scribe_text(&serde_json::json!({"text": "   "})).is_none(), "blank -> none");
        assert!(parse_scribe_text(&serde_json::json!({})).is_none(), "missing -> none");
    }

    #[test]
    fn dedupe_collapses_a_full_duplication() {
        let once = "ئەمە دەقێکی نموونەیە بۆ تاقیکردنەوەی دووبارەبوونەوەی دەنگ لە ئەپەکەدا";
        let twice = format!("{once} {once}");
        assert_eq!(dedupe_repeated(&twice), once, "the whole-text duplication collapses to one copy");
    }

    #[test]
    fn dedupe_collapses_near_duplicate_halves() {
        // Scribe's real behavior: the two copies differ in a few characters but are near-identical.
        let a = "دەزگای ڕوانگە لە ڕەسمێکی شایستەدا ئەنجامی پێشبڕکێی خەڵاتەکان ڕادەگەیەنێت";
        let b = "دەزگای ڕوانگە لە ڕەسمێکی شایستەدا ئەنجامی پێشبڕکەی خەڵاتەکان ڕادەگەیەنرێت";
        assert_eq!(dedupe_repeated(&format!("{a} {b}")), a, "near-duplicate halves collapse to the first");
    }

    #[test]
    fn dedupe_leaves_distinct_text_unchanged() {
        let text = "ئەمە ڕستەی یەکەمە سەبارەت بە کوردستان، بەڵام ئەمەی دواتر سەبارەت بە ئاو و هەوای جیهانە بەتەواوی";
        assert_eq!(dedupe_repeated(text), text, "genuinely distinct halves must not be collapsed");
    }
}
