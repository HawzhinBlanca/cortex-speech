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
/// ElevenLabs' language code for Central Kurdish (Sorani) — verified working in the live test
/// (`language_code="kur"` returned coherent, punctuated Sorani).
pub const SORANI_LANGUAGE_CODE: &str = "kur";

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
        format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"model_id\"\r\n\r\n{model_id}\r\n")
            .as_bytes(),
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

/// Send one Scribe request and return the parsed JSON response. Shared by [`transcribe`] (text only)
/// and [`transcribe_segments`] (word timestamps). The key travels in a header; errors redact it.
fn request_scribe_json(
    audio_path: &str,
    api_key: &str,
    model_id: &str,
    language_code: &str,
) -> AppResult<serde_json::Value> {
    let audio = std::fs::read(audio_path).map_err(|e| AppError::Other(format!("read audio {audio_path}: {e}")))?;
    let filename = std::path::Path::new(audio_path).file_name().and_then(|f| f.to_str()).unwrap_or("audio.wav");
    request_scribe_json_from_bytes(&audio, filename, api_key, model_id, language_code)
}

/// Send already-in-memory audio bytes to Scribe. Lets callers transcribe a sliced segment window
/// without first materializing a temp WAV file on disk.
fn request_scribe_json_from_bytes(
    audio: &[u8],
    filename: &str,
    api_key: &str,
    model_id: &str,
    language_code: &str,
) -> AppResult<serde_json::Value> {
    let (content_type, body) = build_multipart(audio, filename, model_id, language_code);

    let redact = |s: String| s.replace(api_key, "<redacted>");
    let resp = crate::http::API_AGENT
        .post(SCRIBE_URL)
        .set("xi-api-key", api_key)
        .set("Content-Type", &content_type)
        .send_bytes(&body)
        .map_err(|e| AppError::Other(format!("Scribe request failed: {}", redact(e.to_string()))))?;
    crate::http::json_bounded(resp).map_err(|e| AppError::Other(format!("Scribe response parse: {e}")))
}

/// Transcribe an audio file with ElevenLabs Scribe. Returns the transcription text. Errors carry the
/// key redacted (defense in depth, though the key is only ever sent as a header).
pub fn transcribe(audio_path: &str, api_key: &str, model_id: &str, language_code: &str) -> AppResult<String> {
    let json = request_scribe_json(audio_path, api_key, model_id, language_code)?;
    parse_scribe_text(&json)
        .map(|text| dedupe_repeated(&text))
        .ok_or_else(|| AppError::Other("Scribe returned no transcription text".into()))
}

/// Transcribe in-memory WAV bytes (typically ONE sliced segment window) with Scribe. Same contract as
/// [`transcribe`] but avoids a temp file — used for per-segment votes, where sending the whole source
/// file would be wrong (whole-file transcript attributed to one segment) and ~N× the cost.
pub fn transcribe_wav_bytes(
    wav: &[u8],
    filename: &str,
    api_key: &str,
    model_id: &str,
    language_code: &str,
) -> AppResult<String> {
    let json = request_scribe_json_from_bytes(wav, filename, api_key, model_id, language_code)?;
    parse_scribe_text(&json)
        .map(|text| dedupe_repeated(&text))
        .ok_or_else(|| AppError::Other("Scribe returned no transcription text".into()))
}

/// One transcribed word with its timing in milliseconds from the start of the source audio.
#[derive(Debug, Clone, PartialEq)]
pub struct ScribeWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// A contiguous run of words destined to become one annotatable segment, with its source time range.
#[derive(Debug, Clone, PartialEq)]
pub struct ScribeSegment {
    pub text: String,
    pub source_start_ms: i64,
    pub source_end_ms: i64,
}

/// Silence gap (ms) before a word that ends the current segment — a natural pause between utterances.
pub const DEFAULT_PAUSE_GAP_MS: i64 = 600;
/// Maximum segment length (ms) — keeps each segment short enough to review and correct comfortably.
pub const DEFAULT_MAX_SEGMENT_MS: i64 = 12_000;

/// Extract word-level timestamps from a Scribe response. Only `type == "word"` entries are kept
/// (`spacing` entries are layout, not content) and times convert seconds → ms. Unlike the `text`
/// field (which Scribe sometimes duplicates), the words array is a single clean monotonic pass —
/// verified on a real Sorani clip — so no de-duplication is needed here.
fn parse_scribe_words(json: &serde_json::Value) -> Vec<ScribeWord> {
    let Some(arr) = json.get("words").and_then(|w| w.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter(|w| w.get("type").and_then(|t| t.as_str()) == Some("word"))
        .filter_map(|w| {
            let text = w.get("text").and_then(|t| t.as_str())?.trim();
            if text.is_empty() {
                return None;
            }
            let start = w.get("start").and_then(serde_json::Value::as_f64)?;
            let end = w.get("end").and_then(serde_json::Value::as_f64)?;
            Some(ScribeWord {
                text: text.to_string(),
                start_ms: (start * 1000.0).round() as i64,
                end_ms: (end * 1000.0).round() as i64,
            })
        })
        .collect()
}

/// Join a non-empty run of words into a [`ScribeSegment`] spanning first.start..last.end.
fn flush_segment(words: &[&ScribeWord]) -> ScribeSegment {
    ScribeSegment {
        text: words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
        source_start_ms: words.first().map_or(0, |w| w.start_ms),
        source_end_ms: words.last().map_or(0, |w| w.end_ms),
    }
}

/// Group transcribed words into review-sized segments. A new segment begins when the silence before
/// a word exceeds `pause_gap_ms`, or when extending the current one would pass `max_segment_ms`.
pub fn segment_words(words: &[ScribeWord], pause_gap_ms: i64, max_segment_ms: i64) -> Vec<ScribeSegment> {
    let mut segments = Vec::new();
    let mut cur: Vec<&ScribeWord> = Vec::new();
    for w in words {
        if let Some(prev) = cur.last() {
            // Saturating: timestamps derive from an untrusted API response (f64 -> i64 saturates),
            // so a hostile/garbled response must never overflow-panic the segmenter.
            let gap = w.start_ms.saturating_sub(prev.end_ms);
            let span = w.end_ms.saturating_sub(cur[0].start_ms);
            if gap > pause_gap_ms || span > max_segment_ms {
                segments.push(flush_segment(&cur));
                cur.clear();
            }
        }
        cur.push(w);
    }
    if !cur.is_empty() {
        segments.push(flush_segment(&cur));
    }
    segments
}

/// Transcribe a whole audio file with Scribe and split it into review-sized segments using the
/// word-level timestamps — ONE Scribe API call per file (cost-efficient for long recordings). Falls
/// back to a single whole-file segment (from the de-duplicated text) when no word timings are present.
pub fn transcribe_segments(
    audio_path: &str,
    api_key: &str,
    model_id: &str,
    language_code: &str,
) -> AppResult<Vec<ScribeSegment>> {
    let json = request_scribe_json(audio_path, api_key, model_id, language_code)?;
    let words = parse_scribe_words(&json);
    if words.is_empty() {
        let text = parse_scribe_text(&json)
            .map(|t| dedupe_repeated(&t))
            .ok_or_else(|| AppError::Other("Scribe returned no words and no text".into()))?;
        return Ok(vec![ScribeSegment { text, source_start_ms: 0, source_end_ms: 0 }]);
    }
    Ok(segment_words(&words, DEFAULT_PAUSE_GAP_MS, DEFAULT_MAX_SEGMENT_MS))
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

    #[test]
    fn parse_words_keeps_words_drops_spacing_and_converts_to_ms() {
        // Shape mirrors the real Scribe response (verified live): word + spacing entries, secs.
        let json = serde_json::json!({
            "words": [
                {"text": "دەزگای", "start": 0.7, "end": 1.06, "type": "word"},
                {"text": " ", "start": 1.06, "end": 1.12, "type": "spacing"},
                {"text": "ڕوانگە", "start": 1.12, "end": 1.54, "type": "word"},
                {"text": "  ", "start": 0.0, "end": 0.0, "type": "spacing"}
            ]
        });
        let words = parse_scribe_words(&json);
        assert_eq!(words.len(), 2, "spacing entries are dropped");
        assert_eq!(words[0], ScribeWord { text: "دەزگای".into(), start_ms: 700, end_ms: 1060 });
        assert_eq!(words[1].start_ms, 1120, "seconds round to ms");
    }

    #[test]
    fn parse_words_empty_when_no_words_field() {
        assert!(parse_scribe_words(&serde_json::json!({"text": "x"})).is_empty());
    }

    #[test]
    fn parse_words_survives_malformed_entries() {
        // A garbled/hostile response must degrade gracefully: drop every malformed entry, keep the
        // valid ones, and never panic.
        let json = serde_json::json!({
            "words": [
                {"text": "ok1", "start": 1.0, "end": 2.0, "type": "word"},          // valid
                {"text": "nostart", "end": 2.0, "type": "word"},                    // missing start
                {"text": "noend", "start": 1.0, "type": "word"},                    // missing end
                {"start": 1.0, "end": 2.0, "type": "word"},                          // missing text
                {"text": "   ", "start": 1.0, "end": 2.0, "type": "word"},          // blank text
                {"text": "strstart", "start": "1.0", "end": 2.0, "type": "word"},   // start not a number
                {"text": "notype", "start": 1.0, "end": 2.0},                        // no type field
                {"text": "spacing", "start": 1.0, "end": 2.0, "type": "spacing"},   // not a word
                "garbage-string",                                                    // non-object entry
                42,                                                                  // non-object entry
                {"text": "ok2", "start": 3.0, "end": 4.0, "type": "word"},          // valid
            ]
        });
        let words = parse_scribe_words(&json);
        assert_eq!(words.len(), 2, "only the two fully-valid words survive: {words:?}");
        assert_eq!(words[0].text, "ok1");
        assert_eq!(words[1], ScribeWord { text: "ok2".into(), start_ms: 3000, end_ms: 4000 });
    }

    #[test]
    fn parse_words_handles_non_array_words_field() {
        // `words` present but the wrong shape (object / string / number) -> empty, no panic.
        for bad in
            [serde_json::json!({"words": {"a": 1}}), serde_json::json!({"words": "x"}), serde_json::json!({"words": 7})]
        {
            assert!(parse_scribe_words(&bad).is_empty(), "non-array words -> empty: {bad}");
        }
    }

    fn w(text: &str, start_ms: i64, end_ms: i64) -> ScribeWord {
        ScribeWord { text: text.into(), start_ms, end_ms }
    }

    #[test]
    fn segment_words_splits_on_a_long_pause() {
        // Two words tight together, then a 1s pause, then a word -> two segments.
        let words = vec![w("a", 0, 300), w("b", 350, 600), w("c", 1700, 2000)];
        let segs = segment_words(&words, 600, 12_000);
        assert_eq!(segs.len(), 2, "the >600ms gap before 'c' starts a new segment");
        assert_eq!(segs[0], ScribeSegment { text: "a b".into(), source_start_ms: 0, source_end_ms: 600 });
        assert_eq!(segs[1], ScribeSegment { text: "c".into(), source_start_ms: 1700, source_end_ms: 2000 });
    }

    #[test]
    fn segment_words_splits_when_max_length_exceeded() {
        // No pauses, but the run exceeds max_segment_ms -> it splits.
        let words = vec![w("a", 0, 1000), w("b", 1000, 2000), w("c", 2000, 3000)];
        let segs = segment_words(&words, 600, 1500);
        assert!(segs.len() >= 2, "a 3s run with a 1.5s cap must split, got {}", segs.len());
        assert_eq!(segs[0].source_start_ms, 0);
        // Segments stay contiguous and ordered.
        assert!(segs.windows(2).all(|p| p[1].source_start_ms >= p[0].source_end_ms));
    }

    #[test]
    fn segment_words_keeps_one_segment_when_no_boundary() {
        let words = vec![w("a", 0, 300), w("b", 400, 700), w("c", 800, 1100)];
        let segs = segment_words(&words, 600, 12_000);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "a b c");
        assert_eq!((segs[0].source_start_ms, segs[0].source_end_ms), (0, 1100));
    }

    #[test]
    fn segment_words_handles_empty_input() {
        assert!(segment_words(&[], 600, 12_000).is_empty());
    }

    #[test]
    fn segment_words_does_not_overflow_on_extreme_timestamps() {
        // A malformed/hostile Scribe response can yield saturated i64 timestamps (f64 -> i64 saturates
        // on out-of-range / non-finite values). The gap/span subtraction must not overflow-panic.
        let words = vec![
            ScribeWord { text: "a".into(), start_ms: i64::MIN, end_ms: i64::MIN },
            ScribeWord { text: "b".into(), start_ms: i64::MAX, end_ms: i64::MAX },
        ];
        let segs = segment_words(&words, 600, 12_000);
        assert!(!segs.is_empty(), "must still segment without panicking on extreme timestamps");
    }

    /// Live end-to-end proof against the real API. Off by default (network + key + audio); run with
    ///   ELEVENLABS_API_KEY=... SCRIBE_TEST_AUDIO=/path/to/clip.wav cargo test --lib -- --ignored live_transcribe_segments
    /// Asserts the whole-file path yields ordered, in-range segments and prints them for inspection.
    #[test]
    #[ignore = "live: set ELEVENLABS_API_KEY and SCRIBE_TEST_AUDIO"]
    fn live_transcribe_segments() {
        let Some(key) = std::env::var("ELEVENLABS_API_KEY").ok().or_else(|| {
            let appdata = std::env::var("APPDATA").ok()?;
            crate::api_keys::ApiKeys::load(&std::path::Path::new(&appdata).join("cortex-speech")).elevenlabs
        }) else {
            eprintln!("ElevenLabs key is not configured; skipping live Scribe request");
            return;
        };
        let audio = std::env::var("SCRIBE_TEST_AUDIO").unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/fleurs_ckb_sample.wav")
                .to_string_lossy()
                .into_owned()
        });
        let segs = transcribe_segments(&audio, &key, DEFAULT_MODEL, "kur").expect("transcribe_segments");
        assert!(!segs.is_empty(), "expected at least one segment");
        for s in &segs {
            assert!(s.source_end_ms >= s.source_start_ms, "each segment ends at/after it starts");
            assert!(!s.text.trim().is_empty(), "no empty-text segments");
        }
        for pair in segs.windows(2) {
            assert!(pair[1].source_start_ms >= pair[0].source_start_ms, "segments are time-ordered");
        }
        eprintln!("LIVE_SEGMENTS_COUNT::{}", segs.len());
        for (i, s) in segs.iter().enumerate() {
            eprintln!("SEG[{i}] {}..{}ms :: {}", s.source_start_ms, s.source_end_ms, s.text);
        }
    }
}
