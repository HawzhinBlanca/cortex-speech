#![no_main]

use libfuzzer_sys::fuzz_target;
use cortex_speech_app_lib::validation::input::{sanitize_filename, validate_identifier, validate_text};

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Test validate_identifier — must never panic
        let id_result = validate_identifier(s);
        if let Err(ref e) = id_result {
            // Error messages should always be non-empty
            assert!(!e.is_empty(), "Error message must not be empty");
        }
        if id_result.is_ok() {
            // If valid, the identifier must pass basic constraints
            assert!(!s.is_empty(), "Valid identifier must not be empty");
            assert!(s.len() <= 256, "Valid identifier must be <= 256 chars");
            assert!(
                s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.'),
                "Valid identifier must only contain alphanumeric, _, -, or ."
            );
        }

        // Test validate_text with various max lengths — must never panic
        for max_len in &[0usize, 1, 10, 100, 1024, 65536] {
            let text_result = validate_text(s, *max_len, "fuzz_field");
            if let Err(ref e) = text_result {
                assert!(!e.is_empty(), "Error message must not be empty");
            }
            if text_result.is_ok() {
                // CHARACTERS, not bytes. validate_text deliberately counts chars: Sorani is
                // ~2 bytes/char in UTF-8, and a byte-based limit rejected valid Kurdish text at
                // roughly half the advertised budget (see the comment on validate_text). This
                // assertion previously used s.len(), i.e. it encoded the very contract production
                // fixed — so it fired on almost any Kurdish string and would have reported CORRECT
                // behaviour as a crash. Found the first time these targets were ever executed.
                let chars = s.chars().count();
                assert!(chars <= *max_len, "Valid text must be <= {max_len} chars (got {chars})");
            }
        }

        // Test sanitize_filename — must never panic and never contain path separators
        let sanitized = sanitize_filename(s);
        assert!(
            !sanitized.contains('/'),
            "Sanitized filename must not contain '/'"
        );
        assert!(
            !sanitized.contains('\\'),
            "Sanitized filename must not contain '\\'"
        );
        assert!(
            !sanitized.contains('\0'),
            "Sanitized filename must not contain null bytes"
        );
    }
});
