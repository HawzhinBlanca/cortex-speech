use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// How many times to try clearing a stale lockfile before giving up. Windows holds by an antivirus
/// scanner or the Search indexer are measured in milliseconds; a genuinely live instance holds the
/// file for its whole lifetime and will fail all of them.
#[cfg(windows)]
const STALE_LOCK_ATTEMPTS: u32 = 5;
#[cfg(windows)]
const STALE_LOCK_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(80);

pub struct InstanceLock {
    /// RAII guard, never read on purpose: the OS lock lives as long as this handle is open, so
    /// dropping the field to satisfy dead_code would silently disable single-instance locking and
    /// let a second copy of the app open the same database. Held, not used.
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
            // Clearing a STALE lockfile is retried, not attempted once.
            //
            // A previous process that exited leaves the file behind; the OS has closed its handle, so
            // the file is deletable — but on Windows it can still be held for a few milliseconds by
            // something that is not Cortex at all: an antivirus scanner, the Search indexer, a backup
            // agent. A single failed `remove_file` then made `create_new` fail with PermissionDenied,
            // which this code reported as "Another instance is already running".
            //
            // That message is a FALSE POSITIVE the user cannot act on: they go looking for a second
            // window that does not exist, and the app will not start. Retrying across that brief hold
            // turns the transient case into a normal start, while a genuinely live instance — which
            // holds the file with share_mode(0) for as long as it runs — keeps failing every attempt
            // and is still reported correctly.
            for attempt in 0..STALE_LOCK_ATTEMPTS {
                if !lock_path.exists() {
                    break;
                }
                if attempt + 1 == STALE_LOCK_ATTEMPTS {
                    // The LAST attempt goes through the observable helper, so a lock that genuinely
                    // cannot be cleared is logged with context rather than vanishing. The earlier
                    // attempts deliberately do not log: a transient antivirus hold that clears on
                    // retry is not an incident, and warning five times per start would train the
                    // owner to ignore the one warning that matters.
                    remove_lock_file(&lock_path, "stale Windows instance lock");
                    break;
                }
                match std::fs::remove_file(&lock_path) {
                    Ok(()) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                    Err(_transient) => std::thread::sleep(STALE_LOCK_RETRY_DELAY),
                }
            }
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .share_mode(0) // Enforce exclusive lock by disabling sharing
                .open(&lock_path)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists || e.kind() == std::io::ErrorKind::PermissionDenied
                    {
                        // Name the file. If the user really has no other window open, this is the one
                        // thing they can do about it — a bare "another instance is running" leaves them
                        // with no move at all.
                        format!(
                            "Another instance is already running. If no other Cortex window is open, close it from Task Manager, or delete {} and try again.",
                            lock_path.display()
                        )
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
    fn a_stale_lockfile_from_a_dead_process_does_not_block_startup() {
        // THE FAILURE THE OWNER ACTUALLY HIT: "another instance is running" with no other instance.
        //
        // A process that dies without running Drop (force-killed, crashed, power loss) leaves the
        // lockfile on disk with no owner. Startup MUST recover from that on its own — a leftover file
        // is not a running app, and telling the user otherwise blocks them from their own tool with a
        // message they cannot act on.
        //
        // Nothing covered this path before: the existing test only exercised a LIVE holder.
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("cortex.lock");
        std::fs::write(&lock_path, b"leftover from a killed process").expect("plant a stale lockfile");
        assert!(lock_path.exists());

        let lock =
            InstanceLock::try_lock(dir.path()).expect("a stale lockfile with no owning process must NOT block startup");
        assert!(lock_path.exists(), "the fresh lock replaces it");
        drop(lock);
        assert!(!lock_path.exists(), "and is cleaned up again on release");
    }

    #[test]
    fn a_live_holder_is_still_refused_and_the_message_is_actionable() {
        // The retry above must not weaken the guarantee it sits in front of: two processes on one
        // SQLite database is the thing this lock exists to prevent. A live holder keeps the file open
        // for its whole lifetime, so every retry fails and the refusal stands.
        let dir = tempfile::tempdir().expect("tempdir");
        let _held = InstanceLock::try_lock(dir.path()).expect("first lock acquires");
        let err = match InstanceLock::try_lock(dir.path()) {
            Ok(_) => panic!("a live second instance must be refused"),
            Err(e) => e,
        };
        assert!(err.contains("Another instance"), "the refusal must say what it is: {err}");
        // And it must tell the user what they can DO. A bare refusal leaves them with no move when
        // the lock is stale in a way the retry could not clear.
        #[cfg(windows)]
        assert!(err.contains("cortex.lock"), "the message must name the lock file so the user has a way out: {err}");
    }

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
