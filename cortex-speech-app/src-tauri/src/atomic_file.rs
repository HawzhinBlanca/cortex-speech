use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Replace `final_path` with a fully written temp file.
///
/// On Unix, `rename` replaces an existing destination. On Windows it does not,
/// so we move the current destination aside, promote the temp file, then remove
/// the backup. The temp file should live on the same filesystem as the final
/// path.
#[cfg(not(target_os = "windows"))]
pub fn replace_file(tmp_path: &Path, final_path: &Path) -> io::Result<()> {
    fs::rename(tmp_path, final_path)
}

#[cfg(target_os = "windows")]
pub fn replace_file(tmp_path: &Path, final_path: &Path) -> io::Result<()> {
    if !final_path.exists() {
        return fs::rename(tmp_path, final_path);
    }

    let backup_path = replacement_backup_path(final_path);
    match fs::remove_file(&backup_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    fs::rename(final_path, &backup_path)?;
    if let Err(replace_err) = fs::rename(tmp_path, final_path) {
        if let Err(restore_err) = fs::rename(&backup_path, final_path) {
            tracing::warn!(
                "Failed to restore {} from replacement backup {} after replace error: {restore_err}",
                final_path.display(),
                backup_path.display()
            );
        }
        return Err(replace_err);
    }

    match fs::remove_file(&backup_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(target_os = "windows")]
fn replacement_backup_path(final_path: &Path) -> PathBuf {
    let file_name = final_path.file_name().and_then(|name| name.to_str()).unwrap_or("target");
    final_path.with_file_name(format!("{file_name}.replace-bak-{}", std::process::id()))
}

pub fn remove_file_on_error<T, E>(path: &Path, result: Result<T, E>) -> Result<T, E> {
    if result.is_err() {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("Failed to remove temporary file {} after error: {e}", path.display()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_file_creates_new_target() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = tmp_dir.path().join("data.json.tmp");
        let final_path = tmp_dir.path().join("data.json");
        fs::write(&tmp_path, "new").expect("write tmp");

        replace_file(&tmp_path, &final_path).expect("replace file");

        assert_eq!(fs::read_to_string(&final_path).expect("read final"), "new");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn replace_file_replaces_existing_target() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = tmp_dir.path().join("data.json.tmp");
        let final_path = tmp_dir.path().join("data.json");
        fs::write(&final_path, "old").expect("write final");
        fs::write(&tmp_path, "new").expect("write tmp");

        replace_file(&tmp_path, &final_path).expect("replace file");

        assert_eq!(fs::read_to_string(&final_path).expect("read final"), "new");
        assert!(!tmp_path.exists());
        assert_no_backup_left(tmp_dir.path());
    }

    fn assert_no_backup_left(dir: &Path) {
        let backup_left = fs::read_dir(dir)
            .expect("read dir")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".replace-bak-"));
        assert!(!backup_left, "replacement backup should be cleaned up");
    }

    #[test]
    fn remove_file_on_error_removes_file_when_result_is_error() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = tmp_dir.path().join("data.json.tmp");
        fs::write(&tmp_path, "partial").expect("write tmp");

        let result = remove_file_on_error::<(), _>(&tmp_path, Err(io::Error::other("synthetic failure")));

        assert!(result.is_err());
        assert!(!tmp_path.exists());
    }

    #[test]
    fn remove_file_on_error_keeps_file_when_result_is_ok() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = tmp_dir.path().join("data.json.tmp");
        fs::write(&tmp_path, "complete").expect("write tmp");

        remove_file_on_error(&tmp_path, Ok::<_, io::Error>(())).expect("ok");

        assert_eq!(fs::read_to_string(&tmp_path).expect("read tmp"), "complete");
    }
}
