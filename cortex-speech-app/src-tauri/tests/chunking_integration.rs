//! Integration tests for chunking alignment JSON helpers.

use cortex_speech_app_lib::aligner::WordTimestamp;
use cortex_speech_app_lib::chunking::{self, SegmentSourceMeta};

fn sample_words() -> Vec<WordTimestamp> {
    vec![
        WordTimestamp { word: "hello".to_string(), start: 0.0, end: 0.4, confidence: 0.95 },
        WordTimestamp { word: "world".to_string(), start: 0.4, end: 0.9, confidence: 0.88 },
    ]
}

#[test]
fn merge_word_timestamps_preserves_chunk_metadata() {
    let meta = SegmentSourceMeta { source_start_ms: 0, source_end_ms: 5000, chunk_index: 1, chunk_count: 4 };
    let existing = meta.to_alignment_json();
    let merged = chunking::merge_word_timestamps(Some(&existing), &sample_words());
    let v: serde_json::Value = serde_json::from_str(&merged).unwrap();

    assert_eq!(v["source_start_ms"], 0);
    assert_eq!(v["chunk_count"], 4);
    assert!(v["words"].is_array());
    assert_eq!(v["words"].as_array().unwrap().len(), 2);
}

#[test]
fn merge_word_timestamps_without_existing_json() {
    let merged = chunking::merge_word_timestamps(None, &sample_words());
    let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert!(v.get("source_start_ms").is_none());
    assert_eq!(v["words"].as_array().unwrap().len(), 2);
}

#[test]
fn merge_word_timestamps_replaces_words_on_object() {
    let base = r#"{"source_start_ms":100,"source_end_ms":200,"chunk_index":0,"chunk_count":1,"words":[]}"#;
    let merged = chunking::merge_word_timestamps(Some(base), &sample_words());
    let parsed = chunking::word_timestamps_from_alignment(&merged).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].word, "hello");
}

#[test]
fn word_timestamps_from_alignment_legacy_array() {
    let words = sample_words();
    let json = serde_json::to_string(&words).unwrap();
    let parsed = chunking::word_timestamps_from_alignment(&json).unwrap();
    assert_eq!(parsed.len(), 2);
}
