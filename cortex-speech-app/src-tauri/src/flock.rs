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
