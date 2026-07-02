/// M0.5 — crash observability: log panic to a crash marker file so the app can detect and
/// report "last session crashed" on next startup.
use std::fs;
use std::path::Path;

/// Write a crash marker file when the app panics, so we can detect it on the next startup.
pub fn install_panic_hook(app_data_dir: &Path) {
    let marker_path = app_data_dir.join("last_crash.txt");

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let msg = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };

        let location = panic_info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_string());

        let crash_info = format!("CRASH: {} at {}", msg, location);

        // Try to write marker; if it fails, the default panic hook will still run
        let _ = fs::write(&marker_path, &crash_info);

        default_hook(panic_info);
    }));
}

/// Check if a crash marker exists and return its content. The app shows this on startup.
pub fn check_last_crash(app_data_dir: &Path) -> Option<String> {
    let marker_path = app_data_dir.join("last_crash.txt");
    if marker_path.exists() {
        fs::read_to_string(&marker_path).ok()
    } else {
        None
    }
}

/// Clear the crash marker after the user has seen it or after a successful startup.
pub fn clear_crash_marker(app_data_dir: &Path) {
    let marker_path = app_data_dir.join("last_crash.txt");
    let _ = fs::remove_file(&marker_path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn crash_marker_write_and_read() {
        let tmpdir = TempDir::new().unwrap();
        let path = tmpdir.path().to_path_buf();

        // Simulate a crash by directly writing a marker
        let marker = path.join("last_crash.txt");
        fs::write(&marker, "test crash").unwrap();

        assert_eq!(check_last_crash(&path), Some("test crash".to_string()));

        clear_crash_marker(&path);
        assert!(check_last_crash(&path).is_none());
    }
}
