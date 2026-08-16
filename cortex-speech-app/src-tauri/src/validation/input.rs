use std::path::Path;

/// Validate that a file path is safe from directory traversal attacks.
/// Returns the canonicalized path if valid.
///
/// Security properties:
/// - Path traversal: handled via `canonicalize()` which resolves `..`
/// - Null bytes: rejected explicitly
/// - Empty strings: `canonicalize()` will fail on empty input ✓
/// - Very long strings: `canonicalize()` will fail gracefully ✓
/// - Unicode normalization: NOT normalized (user should NFC-normalize
///   paths before calling if needed to avoid BIDI/file-system tricks)
pub fn validate_file_path(path: &str) -> Result<String, String> {
    let path = Path::new(path);

    // Reject paths with null bytes
    if path.to_string_lossy().contains('\0') {
        return Err("Path contains null bytes".to_string());
    }

    // Reject UNC / network paths on Windows BEFORE canonicalizing. `std::fs::canonicalize` opens a
    // handle to the target to resolve it, so canonicalizing a `\\attacker.com\share\x.wav` handed in by
    // the (untrusted) webview would ITSELF drive the SMB redirector — an outbound TCP/445 session that
    // transmits the logged-in user's NTLM credentials to the attacker-controlled host — before any
    // post-canonicalize prefix check could ever reject it (and canonicalize returns Err if the host is
    // unreachable, so the guard never even runs, yet the creds are already on the wire). A SYNTACTIC
    // prefix check on the RAW input needs zero filesystem/network I/O, so it blocks the leak at its
    // source. Both `\\server\share` and `//server/share` parse to a UNC prefix on Windows.
    #[cfg(windows)]
    if is_unc_path(path) {
        return Err("Network (UNC) paths are not allowed".to_string());
    }

    // Canonicalize to resolve any `..` or symlinks
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("Invalid path: {e}"))?;

    // Defense-in-depth: also reject if canonicalization RESOLVED to a UNC path (e.g. a local symlink
    // pointing at a network share). The syntactic pre-check above already blocks a directly-supplied
    // UNC; this catches the indirect symlink case — which requires local filesystem access to plant —
    // so a UNC path is never handed back to a caller that would then open it (a second SMB round-trip).
    #[cfg(windows)]
    if is_unc_path(&canonical) {
        return Err("Network (UNC) paths are not allowed".to_string());
    }

    Ok(canonical.to_string_lossy().to_string())
}

/// True if `p` begins with any Windows prefix that routes through the network (MUP) redirector — the
/// forced-authentication / NTLM-leak surface. Purely syntactic: inspects the parsed prefix only, with
/// zero filesystem/network access.
///
/// Covers FOUR spellings, not two. Rust's `Path` parser only classifies `\\server\share` and the
/// uppercase `\\?\UNC\server\share` as `Prefix::UNC`/`VerbatimUNC`; but `\\?\unc\...` (lowercase — Rust
/// sees `Prefix::Verbatim("unc")`) and `\\.\UNC\...` (device namespace — `Prefix::DeviceNS("UNC")`)
/// ALSO drive the redirector (measured: `std::fs::metadata` returns os error 53 ERROR_BAD_NETPATH for
/// all four, vs os error 3 for a local path). A guard that matched only the first two left the exact
/// leak open under two aliases, so the "UNC" keyword is matched case-insensitively in the verbatim and
/// device-namespace forms too. Legit local prefixes (`Disk`, `VerbatimDisk` — what `canonicalize`
/// returns) never match.
#[cfg(windows)]
fn is_unc_path(p: &Path) -> bool {
    use std::path::{Component, Prefix};
    match p.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => true,
            Prefix::Verbatim(s) | Prefix::DeviceNS(s) => s.eq_ignore_ascii_case("unc"),
            _ => false,
        },
        _ => false,
    }
}

/// Reject a renderer-supplied path that would drive the Windows SMB redirector (a UNC / network path)
/// or that contains a null byte. This is the SYNTACTIC half of [`validate_file_path`], extracted for
/// the WRITE paths (dataset merge, snapshot restore, the relink search dir) where the path is stored
/// or searched AS-IS and must NOT be canonicalized: the target may legitimately not exist yet, and
/// canonicalizing a UNC path is ITSELF the NTLM-forced-auth leak we are preventing (it opens a handle
/// to the target). Zero filesystem/network I/O. On non-Windows only the null-byte check applies (UNC is
/// a Windows concept), matching the `#[cfg(windows)]` guards in [`validate_file_path`].
pub fn reject_unc_path(path: &str) -> Result<(), String> {
    if path.contains('\0') {
        return Err("Path contains null bytes".to_string());
    }
    #[cfg(windows)]
    if is_unc_path(Path::new(path)) {
        return Err("Network (UNC) paths are not allowed".to_string());
    }
    Ok(())
}

/// Validate that a string is a safe identifier (alphanumeric + underscore + hyphen).
pub fn validate_identifier(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("Identifier must not be empty".to_string());
    }
    if s.len() > 256 {
        return Err("Identifier too long (max 256 chars)".to_string());
    }
    if !s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err("Identifier must be alphanumeric (underscore, hyphen, dot allowed)".to_string());
    }
    Ok(())
}

/// Validate that a string is a safe text value with a reasonable length, measured in CHARACTERS.
pub fn validate_text(s: &str, max_len: usize, field_name: &str) -> Result<(), String> {
    // Count characters, not bytes: the limit is stated in chars, and Sorani (the app's primary
    // language) is ~2 bytes/char in UTF-8, so a byte check rejected valid Kurdish text at roughly half
    // the advertised budget AND the error mislabeled the byte count as "chars". chars().count() is
    // O(n), fine for a one-shot boundary check; the effective byte ceiling stays bounded (max_len × 4
    // for the widest codepoints).
    let chars = s.chars().count();
    if chars > max_len {
        return Err(format!("{field_name} too long (max {max_len} chars, got {chars})"));
    }
    Ok(())
}

/// Validate and size-bound an arbitrary JSON blob.
pub fn validate_json_blob(s: &str) -> Result<serde_json::Value, String> {
    if s.len() > 500_000 {
        return Err("JSON metadata too large (max 500KB)".to_string());
    }
    serde_json::from_str::<serde_json::Value>(s).map_err(|e| format!("Invalid JSON metadata: {e}"))
}

fn json_f64(value: &serde_json::Value, field: &str) -> Result<f64, String> {
    let n = value.as_f64().ok_or_else(|| format!("Alignment field '{field}' must be a finite number"))?;
    if !n.is_finite() {
        return Err(format!("Alignment field '{field}' must be finite"));
    }
    Ok(n)
}

fn validate_alignment_words(words: &[serde_json::Value]) -> Result<(), String> {
    let mut previous_start = 0.0;
    for (index, value) in words.iter().enumerate() {
        let word = value.as_object().ok_or_else(|| format!("Alignment word {index} must be an object"))?;
        let text = word
            .get("word")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| format!("Alignment word {index} must contain non-empty text"))?;
        if text.chars().count() > 1_000 {
            return Err(format!("Alignment word {index} is too long"));
        }
        let start = json_f64(
            word.get("start").ok_or_else(|| format!("Alignment word {index} is missing start"))?,
            "words[].start",
        )?;
        let end =
            json_f64(word.get("end").ok_or_else(|| format!("Alignment word {index} is missing end"))?, "words[].end")?;
        if start < 0.0 || end < start {
            return Err(format!("Alignment word {index} has an invalid time range"));
        }
        if index > 0 && start < previous_start {
            return Err(format!("Alignment words are out of order at index {index}"));
        }
        if let Some(confidence) = word.get("confidence") {
            let confidence = json_f64(confidence, "words[].confidence")?;
            if !(0.0..=1.0).contains(&confidence) {
                return Err(format!("Alignment word {index} confidence must be between 0 and 1"));
            }
        }
        previous_start = start;
    }
    Ok(())
}

/// Validate the supported alignment schema: either a legacy word array, or an object containing
/// source bounds and/or `words`. Unknown object keys are preserved for forward compatibility, but
/// the timing fields used by playback/export are strictly typed and internally consistent.
pub fn validate_alignment_json(s: &str) -> Result<(), String> {
    let value = validate_json_blob(s)?;
    match &value {
        serde_json::Value::Array(words) => validate_alignment_words(words)?,
        serde_json::Value::Object(object) => {
            let has_start = object.contains_key("source_start_ms");
            let has_end = object.contains_key("source_end_ms");
            if has_start != has_end {
                return Err("Alignment source_start_ms and source_end_ms must be provided together".into());
            }
            if has_start {
                let start = object["source_start_ms"]
                    .as_i64()
                    .ok_or_else(|| "Alignment source_start_ms must be a non-negative integer".to_string())?;
                let end = object["source_end_ms"]
                    .as_i64()
                    .ok_or_else(|| "Alignment source_end_ms must be a positive integer".to_string())?;
                if start < 0 || end <= start {
                    return Err("Alignment source range must satisfy 0 <= start < end".into());
                }
            }
            let has_index = object.contains_key("chunk_index");
            let has_count = object.contains_key("chunk_count");
            if has_index != has_count {
                return Err("Alignment chunk_index and chunk_count must be provided together".into());
            }
            if has_index {
                let index = object["chunk_index"]
                    .as_u64()
                    .ok_or_else(|| "Alignment chunk_index must be a non-negative integer".to_string())?;
                let count = object["chunk_count"]
                    .as_u64()
                    .ok_or_else(|| "Alignment chunk_count must be a positive integer".to_string())?;
                if count == 0 || index >= count {
                    return Err("Alignment chunk range must satisfy 0 <= chunk_index < chunk_count".into());
                }
            }
            if let Some(words) = object.get("words") {
                let words = words.as_array().ok_or_else(|| "Alignment words must be an array".to_string())?;
                validate_alignment_words(words)?;
            }
            if !has_start && !object.contains_key("words") {
                return Err("Alignment object must contain source bounds or words".into());
            }
        }
        _ => return Err("Alignment JSON must be an object or word array".into()),
    }
    Ok(())
}

/// Validate a segment ID exists in the database.
pub fn validate_segment_id(db: &crate::db::Database, id: &str) -> Result<(), String> {
    validate_identifier(id)?;
    db.get_segment_by_id(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("Segment not found: {id}"))?;
    Ok(())
}

/// Validate that an output path is safe from directory traversal attacks.
/// The parent directory must exist and be canonical; the filename itself may not exist yet.
pub fn validate_output_path(path: &str) -> Result<String, String> {
    if path.contains('\0') {
        return Err("Path contains null bytes".into());
    }
    let p = Path::new(path);
    // SAME UNC guard as validate_file_path (this is its export/backup sibling). std::fs::canonicalize opens
    // a handle to the target and on Windows drives the SMB redirector, so canonicalizing a webview-supplied
    // UNC destination (\\attacker\share\out) would transmit the logged-in user's NTLM credentials to the
    // attacker host BEFORE any post-check could reject it (and canonicalize errors if unreachable, so a
    // post-check never runs — yet the creds are already on the wire). Block it SYNTACTICALLY on the raw
    // input (zero I/O), and again after canonicalizing the parent (a local symlink parent resolving to a
    // share — the indirect case). Both `\\server\share` and `//server/share` parse to a UNC prefix.
    #[cfg(windows)]
    if is_unc_path(p) {
        return Err("Network (UNC) paths are not allowed".to_string());
    }
    let parent = p.parent().ok_or_else(|| "No parent directory".to_string())?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|e| format!("Invalid output directory: {e}"))?;
    #[cfg(windows)]
    if is_unc_path(&canonical_parent) {
        return Err("Network (UNC) paths are not allowed".to_string());
    }
    let filename = p.file_name().ok_or_else(|| "No filename".to_string())?;
    Ok(canonical_parent.join(filename).to_string_lossy().to_string())
}

/// Sanitize a filename to prevent path traversal.
pub fn sanitize_filename(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_identifier() {
        assert!(validate_identifier("hello_world-123").is_ok());
        assert!(validate_identifier("").is_err());
        assert!(validate_identifier("a".repeat(257).as_str()).is_err());
        assert!(validate_identifier("../evil").is_err());
        assert!(validate_identifier("good.name").is_ok());
    }

    #[test]
    fn alignment_schema_rejects_ranges_and_words_that_break_editor_math() {
        assert!(validate_alignment_json(
            r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1,"words":[{"word":"x","start":0.0,"end":0.5,"confidence":0.9}]}"#
        )
        .is_ok());
        assert!(validate_alignment_json(r#"{"words":[]}"#).is_ok());
        assert!(validate_alignment_json(r#"[]"#).is_ok(), "legacy word arrays remain readable");

        for invalid in [
            r#"{"source_start_ms":1000,"source_end_ms":1000}"#,
            r#"{"source_start_ms":0}"#,
            r#"{"chunk_index":1,"chunk_count":1,"words":[]}"#,
            r#"{"words":[{"word":"x","start":1.0,"end":0.5,"confidence":0.9}]}"#,
            r#"{"words":[{"word":"x","start":0.0,"end":0.5,"confidence":1.1}]}"#,
            r#"{"unrelated":true}"#,
            r#"null"#,
        ] {
            assert!(validate_alignment_json(invalid).is_err(), "must reject {invalid}");
        }
    }

    #[test]
    fn arbitrary_evidence_json_is_not_forced_through_the_alignment_schema() {
        assert!(validate_json_blob(r#"{"votes":[{"model":"local","accepted":true}]}"#).is_ok());
    }

    #[test]
    fn test_validate_text() {
        assert!(validate_text("hello", 100, "test").is_ok());
        assert!(validate_text("hello", 3, "test").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn validate_output_path_rejects_unc_before_canonicalize() {
        // Twin of validate_file_path's UNC guard: canonicalizing a UNC output/backup destination drives the
        // SMB redirector and leaks the user's NTLM credentials to the attacker host. The syntactic pre-check
        // must reject it — returning the UNC error, NOT "Invalid output directory" (which would mean
        // canonicalize ran and the leak already fired). The ".invalid" TLD never resolves, so no real host is
        // contacted even by the pre-fix path.
        for p in ["\\\\nonexistent.invalid\\share\\out.wav", "//nonexistent.invalid/share/out.wav"] {
            let err = validate_output_path(p).expect_err("a UNC output path must be rejected");
            assert!(err.contains("UNC"), "must reject UNC syntactically, not via canonicalize: {err}");
        }
    }

    #[test]
    fn validate_text_counts_characters_not_bytes() {
        // Sorani letters are multi-byte in UTF-8; the limit is stated in CHARS, so a Kurdish string AT
        // the char limit must pass even though its byte length exceeds it, and the error must report
        // the char count (not the byte count it used to leak).
        let three_sorani = "ککک"; // 3 chars, 6 bytes
        assert_eq!(three_sorani.chars().count(), 3);
        assert!(three_sorani.len() > 3, "precondition: byte length exceeds the char count");
        assert!(validate_text(three_sorani, 3, "t").is_ok(), "3 chars must pass a 3-char limit");
        let err = validate_text(three_sorani, 2, "t").expect_err("3 chars must fail a 2-char limit");
        assert!(err.contains("got 3"), "error must report the CHAR count, not bytes: {err}");
    }

    #[test]
    fn sanitize_filename_keeps_sorani_letters_replaces_unsafe() {
        // P3.7: export filenames derive from Sorani source names — Arabic-script letters (Unicode
        // alphanumeric) must be KEPT so the name stays meaningful; only unsafe chars become '_'.
        assert_eq!(sanitize_filename("کوردی"), "کوردی", "Sorani letters are preserved");
        assert_eq!(sanitize_filename("clip 01/bad:name"), "clip_01_bad_name", "space + path/reserved -> _");
        assert_eq!(sanitize_filename("gesht-01.wav"), "gesht-01.wav", "safe punctuation kept");
        assert!(!sanitize_filename("گەشتی مێژوویی").contains(' '), "no spaces survive in an export filename");
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("hello.txt"), "hello.txt");
        assert_eq!(sanitize_filename("../evil/path"), ".._evil_path");
        assert_eq!(sanitize_filename("normal_file.wav"), "normal_file.wav");
    }

    #[test]
    fn test_validate_file_path_nonexistent() {
        let result = validate_file_path("C:\\nonexistent\\file.wav");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_file_path_null_byte() {
        let result = validate_file_path("safe.txt\0malicious.exe");
        assert!(result.is_err());
    }

    #[test]
    fn reject_unc_path_blocks_null_everywhere_and_unc_on_windows() {
        // P1.1: the syntactic write-path guard. Null bytes are rejected on every platform; a
        // non-UNC relative/local path passes WITHOUT canonicalization (no existence requirement);
        // and on Windows both UNC spellings are rejected with zero I/O.
        assert!(reject_unc_path("some/local/clip.wav").is_ok(), "a plain local path must pass (no canonicalize)");
        assert!(reject_unc_path("").is_ok(), "an empty audio_path is not a UNC path");
        assert!(reject_unc_path("a.wav\0b.exe").is_err(), "null bytes rejected on all platforms");
        #[cfg(windows)]
        {
            // All FOUR redirector-driving spellings must be rejected — not just the two canonical ones.
            // The lowercase-verbatim and device-namespace forms were a real bypass of the pre-#131 guard
            // (Rust parses them as Prefix::Verbatim("unc") / Prefix::DeviceNS("UNC"), not Prefix::UNC),
            // confirmed to hit the SMB redirector (os error 53). Verified on this box.
            for p in [
                r"\\host.invalid\share\x.wav",
                "//host.invalid/share/x.wav",
                r"\\?\UNC\host.invalid\share\x.wav",
                r"\\?\unc\host.invalid\share\x.wav",
                r"\\.\UNC\host.invalid\share\x.wav",
            ] {
                let e = reject_unc_path(p).expect_err("UNC spelling must be rejected");
                assert!(e.contains("UNC"), "must reject as UNC ({p}): {e}");
            }
            // Legit local prefixes must still pass — including the verbatim-disk form canonicalize returns.
            assert!(reject_unc_path(r"\\?\C:\media\clip.wav").is_ok(), "verbatim-disk local path must pass");
        }
    }

    #[cfg(windows)]
    #[test]
    fn validate_file_path_rejects_unc_syntactically_before_canonicalize() {
        // A webview-supplied UNC path must be rejected by the SYNTACTIC prefix guard, BEFORE canonicalize
        // opens a handle to it (which on Windows drives the SMB redirector -> outbound NTLM auth to the
        // attacker host). We assert the error is the UNC rejection, not canonicalize's "Invalid path"
        // (the old order), which is the observable proof the leaking canonicalize was never reached. The
        // host uses the RFC-2606 `.invalid` TLD so that if the guard regressed, the fallback DNS lookup
        // fails fast (NXDOMAIN) instead of hanging on an SMB timeout.
        let err = validate_file_path(r"\\host.invalid\share\x.wav").expect_err("UNC must be rejected");
        assert!(err.contains("UNC"), "must be rejected by the UNC guard, not canonicalize: {err}");
        // The forward-slash form parses to a UNC prefix on Windows too.
        let err2 = validate_file_path("//host.invalid/share/x.wav").expect_err("//UNC must be rejected");
        assert!(err2.contains("UNC"), "forward-slash UNC must also be rejected: {err2}");
    }
}
