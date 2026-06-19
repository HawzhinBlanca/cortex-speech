use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

pub struct InstanceLock {
    #[allow(dead_code)]
    file: Option<File>,
    path: String,
}

impl InstanceLock {
    /// Try to acquire an exclusive lock on a lockfile.
    /// Returns Ok(Lock) if acquired, Err if another instance is running.
    pub fn try_lock(data_dir: &Path) -> Result<Self, String> {
        let lock_path = data_dir.join("cortex.lock");

        #[cfg(unix)]
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(&lock_path)
            .map_err(|e| format!("Cannot create lock file: {e}"))?;

        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::OpenOptionsExt;
            if lock_path.exists() {
                remove_lock_file(&lock_path, "stale Windows instance lock");
            }
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .share_mode(0) // Enforce exclusive lock by disabling sharing
                .open(&lock_path)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists || e.kind() == std::io::ErrorKind::PermissionDenied
                    {
                        "Another instance is already running".to_string()
                    } else {
                        format!("Cannot create lock file: {e}")
                    }
                })?
        };

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if ret != 0 {
                remove_lock_file(&lock_path, "failed Unix instance lock acquisition");
                return Err("Another instance is already running".to_string());
            }
        }

        Ok(Self { file: Some(file), path: lock_path.to_string_lossy().to_string() })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(ref file) = self.file {
            use std::os::unix::io::AsRawFd;
            unsafe {
                libc::flock(file.as_raw_fd(), libc::LOCK_UN);
            }
        }
        // Close the file handle BEFORE removing the lockfile. On Windows the handle is
        // opened with share_mode(0) (no delete sharing), so the file cannot be deleted
        // while it is still open — dropping it first lets the removal succeed instead of
        // leaking a stale lockfile on disk until the next startup recovers it.
        self.file.take();
        remove_lock_file(&PathBuf::from(&self.path), "released instance lock");
    }
}

fn remove_lock_file(path: &Path, context: &str) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!("Failed to remove {context} file {}: {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_is_rejected_and_released_on_drop() {
        // The single-instance lock is what stops two app processes from racing on the
        // same SQLite database. Pin the full lifecycle: acquire → reject a concurrent
        // attempt → release on drop (RAII, so it also frees on panic/unwind) → re-acquire.
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("cortex.lock");

        let first = InstanceLock::try_lock(dir.path()).expect("first lock should acquire");
        assert!(lock_path.exists(), "lock file should exist while the lock is held");

        match InstanceLock::try_lock(dir.path()) {
            Ok(_) => panic!("a concurrent second lock must be rejected"),
            Err(e) => assert!(e.contains("Another instance"), "rejection should report another instance: {e}"),
        }

        // Dropping the holder releases the lock and removes the lockfile.
        drop(first);
        assert!(!lock_path.exists(), "lock file should be removed on drop");

        // The slot is free again.
        let third = InstanceLock::try_lock(dir.path()).expect("lock should re-acquire after release");
        drop(third);
        assert!(!lock_path.exists(), "lock file should be removed after the re-acquired lock drops");
    }
}
