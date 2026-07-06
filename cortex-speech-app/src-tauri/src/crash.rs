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
    let safe_ts: String = timestamp.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
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

/// Return a one-line summary of the MOST RECENT crash report (if any) and remove ALL crash reports so
/// it surfaces exactly once. Called at startup to tell the owner "last session crashed" instead of
/// letting the report sit unseen (the rolling file log keeps the full detail). Best-effort: any
/// read/dir error yields `None`.
pub fn take_latest_crash_summary(data_dir: &Path) -> Option<String> {
    let dir = data_dir.join("crashes");
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("crash-"))
        .collect();
    // The sanitized ISO timestamp in the filename sorts lexicographically, so max = newest.
    let latest = entries.iter().max_by_key(|e| e.file_name())?;
    let body = std::fs::read_to_string(latest.path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let summary = format!(
        "{} at {} (v{})",
        v.get("message").and_then(|m| m.as_str()).unwrap_or("unknown"),
        v.get("location").and_then(|l| l.as_str()).unwrap_or("?"),
        v.get("version").and_then(|x| x.as_str()).unwrap_or("?"),
    );
    // Shown once: remove every crash report (crashes are rare; the file log retains detail).
    for e in &entries {
        let _ = std::fs::remove_file(e.path());
    }
    Some(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_latest_crash_summary_returns_newest_and_clears() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(take_latest_crash_summary(tmp.path()).is_none(), "no crashes -> None");
        write_crash_report(tmp.path(), "a.rs:1:1", "older panic", "2026-06-20T00:00:00Z").unwrap();
        write_crash_report(tmp.path(), "b.rs:2:2", "newer panic", "2026-06-21T00:00:00Z").unwrap();
        let summary = take_latest_crash_summary(tmp.path()).expect("a crash summary");
        assert!(summary.contains("newer panic"), "returns the NEWEST crash: {summary}");
        assert!(summary.contains("b.rs:2:2"));
        // Surfaced once: a second call finds nothing (reports cleared).
        assert!(take_latest_crash_summary(tmp.path()).is_none(), "reports cleared after being surfaced");
    }

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
