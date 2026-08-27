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

/// Return a renderer-safe notice when any crash report exists and remove ALL crash reports so it
/// surfaces exactly once. Panic messages and locations remain in backend-owned diagnostics: either
/// can contain a private path, transcript fragment or secret supplied to a dependency, so neither is
/// a public IPC value. Best-effort: any directory error yields `None`.
pub fn take_latest_crash_summary(data_dir: &Path) -> Option<String> {
    let dir = data_dir.join("crashes");
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("crash-"))
        .collect();
    if entries.is_empty() {
        return None;
    }
    let summary = "the previous session ended unexpectedly (details in the logs folder)".to_string();
    // Shown once: remove EVERY crash report REGARDLESS of parse success (crashes are rare; the file log
    // retains detail). Doing this unconditionally is what prevents a single corrupt report from wedging
    // the notification forever and leaking reports on disk.
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
        assert!(summary.contains("ended unexpectedly"));
        assert!(!summary.contains("newer panic"), "panic text must remain backend-only: {summary}");
        assert!(!summary.contains("b.rs:2:2"), "panic location must remain backend-only: {summary}");
        // Surfaced once: a second call finds nothing (reports cleared).
        assert!(take_latest_crash_summary(tmp.path()).is_none(), "reports cleared after being surfaced");
    }

    #[test]
    fn corrupt_latest_report_still_surfaces_generically_and_clears() {
        // A crash report is written DURING a panic, so a truncated/half-written file is realistic. The
        // newest report being unparseable must NOT wedge the notification forever (return None every
        // startup) nor leak reports — it must surface a generic notice and clear EVERY report.
        let tmp = tempfile::tempdir().unwrap();
        write_crash_report(tmp.path(), "a.rs:1:1", "a real older panic", "2026-06-20T00:00:00Z").unwrap();
        // The NEWEST file (sorts last) is corrupt JSON.
        let crashes = tmp.path().join("crashes");
        std::fs::write(crashes.join("crash-2026-06-21T00-00-00Z.json"), b"{ this is not valid json").unwrap();

        let summary = take_latest_crash_summary(tmp.path()).expect("a corrupt latest must STILL surface");
        assert!(summary.contains("ended unexpectedly"), "generic fallback for a corrupt report: {summary}");
        // Cleared once: no report (corrupt or valid) survives to wedge the next startup.
        assert!(take_latest_crash_summary(tmp.path()).is_none(), "all reports cleared despite the corrupt one");
        assert_eq!(std::fs::read_dir(&crashes).unwrap().count(), 0, "no report leaked on disk");
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
