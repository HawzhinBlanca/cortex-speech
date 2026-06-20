//! Crash reporting — when the app panics, write a diagnostic dump to disk BEFORE the process dies,
//! so a crash is diagnosable/recoverable instead of vanishing silently. Local-only (no network); the
//! report carries the panic location, message, app version and a timestamp — never transcripts or keys.

use std::path::{Path, PathBuf};

/// Write a crash report as JSON into `{data_dir}/crashes/crash-<timestamp>.json`. Returns the path
/// written, or `None` on any IO error — a crash handler must NEVER panic itself, so every failure is
/// swallowed. The timestamp is sanitized for use in the filename.
pub fn write_crash_report(data_dir: &Path, location: &str, message: &str, timestamp: &str) -> Option<PathBuf> {
    let dir = data_dir.join("crashes");
    std::fs::create_dir_all(&dir).ok()?;
    let safe_ts: String =
        timestamp.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let path = dir.join(format!("crash-{safe_ts}.json"));
    let report = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp": timestamp,
        "location": location,
        "message": message,
    });
    let body = serde_json::to_string_pretty(&report).ok()?;
    std::fs::write(&path, body).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_crash_report_with_the_panic_details() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_crash_report(tmp.path(), "src/foo.rs:12:3", "index out of bounds", "2026-06-21T00:00:00Z")
            .expect("should write");
        assert!(p.exists());
        assert!(p.to_string_lossy().contains("crash-"), "filename carries the timestamp");
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("src/foo.rs:12:3"), "{content}");
        assert!(content.contains("index out of bounds"));
        assert!(content.contains("\"version\""), "records the app version");
        // Parses back as JSON.
        serde_json::from_str::<serde_json::Value>(&content).expect("valid JSON");
    }

    #[test]
    fn returns_none_instead_of_panicking_on_a_bad_path() {
        // A path containing a NUL byte cannot be created -> None, never a panic (a crash handler must
        // not crash).
        let res = write_crash_report(Path::new("bad\0dir"), "l", "m", "t");
        assert!(res.is_none());
    }
}
