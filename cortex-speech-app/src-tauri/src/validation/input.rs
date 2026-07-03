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

    // Canonicalize to resolve any `..` or symlinks
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("Invalid path: {e}"))?;

    Ok(canonical.to_string_lossy().to_string())
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

/// Validate that a string is a safe text value with reasonable length.
pub fn validate_text(s: &str, max_len: usize, field_name: &str) -> Result<(), String> {
    if s.len() > max_len {
        return Err(format!("{field_name} too long (max {max_len} chars, got {})", s.len()));
    }
    Ok(())
}

/// Validate that a string is a valid JSON alignment metadata blob.
pub fn validate_alignment_json(s: &str) -> Result<(), String> {
    if s.len() > 500_000 {
        return Err("Alignment metadata too large (max 500KB)".to_string());
    }
    serde_json::from_str::<serde_json::Value>(s).map_err(|e| format!("Invalid alignment JSON: {e}"))?;
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
    let parent = p.parent().ok_or_else(|| "No parent directory".to_string())?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|e| format!("Invalid output directory: {e}"))?;
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
    fn test_validate_text() {
        assert!(validate_text("hello", 100, "test").is_ok());
        assert!(validate_text("hello", 3, "test").is_err());
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
}
