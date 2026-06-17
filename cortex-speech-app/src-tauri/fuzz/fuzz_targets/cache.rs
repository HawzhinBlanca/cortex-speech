#![no_main]

use libfuzzer_sys::fuzz_target;
use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::cache::CacheEntry;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let cache = TranscriptCache::new(10);

        let path = Path::new(s);
        if !s.is_empty() && !s.contains('\0') && s.len() < 260 {
            let _ = cache.get(path, "fuzz_model");

            let entry = CacheEntry {
                audio_hash: "fuzz_hash".to_string(),
                raw_transcript: s.to_string(),
                normalized_transcript: None,
                created_at: chrono::Utc::now(),
                model_id: "fuzz_model".to_string(),
            };
            cache.set(path, entry);

            let got = cache.get(path, "fuzz_model");
            if got.is_some() {
                assert_eq!(got.unwrap().raw_transcript, s);
            }

            cache.invalidate(path);
            assert!(cache.get(path, "fuzz_model").is_none() || path != Path::new(s));
        }

        cache.clear();
        assert_eq!(cache.size(), 0, "Cache must be empty after clear");
    }
});
