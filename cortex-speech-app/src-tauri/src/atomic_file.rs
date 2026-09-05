use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Atomically move one file or directory only if its destination is still absent.
/// Existing destinations are never replaced. Windows also requests write-through;
/// callers must retain their explicit source/destination parent durability barriers.
#[cfg(target_os = "windows")]
pub(crate) fn rename_no_replace_write_through(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: both buffers are NUL-terminated and remain live for the call. MOVEFILE_REPLACE_EXISTING
    // is deliberately absent: a racing destination makes publication fail instead of overwriting it.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn rename_no_replace_write_through(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    const RENAME_NOREPLACE_FLAG: libc::c_uint = 1;
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "destination path contains NUL"))?;
    // renameat2(RENAME_NOREPLACE) is the Linux atomic no-clobber primitive. The strict parent
    // fsync below is the durability barrier on Unix.
    let status = unsafe {
        libc::renameat2(libc::AT_FDCWD, source.as_ptr(), libc::AT_FDCWD, destination.as_ptr(), RENAME_NOREPLACE_FLAG)
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) fn rename_no_replace_write_through(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "destination path contains NUL"))?;
    let status = unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
pub(crate) fn rename_no_replace_write_through(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace publication is unsupported on this platform",
    ))
}

/// Replace `final_path` with a fully written temp file.
///
/// On Unix, `rename` replaces an existing destination. On Windows it does not,
/// so we move the current destination aside, promote the temp file, then remove
/// the backup. The temp file should live on the same filesystem as the final
/// path.
#[cfg(not(target_os = "windows"))]
pub fn replace_file(tmp_path: &Path, final_path: &Path) -> io::Result<()> {
    // Flush the staged bytes to stable storage before the rename: atomic rename makes
    // the swap atomic, but only fsync makes the *data* durable — without it a power-loss
    // right after the rename can expose a zero-length file where a curated artifact
    // should be (the cardinal sin for a tool whose output is human-verified labels).
    fsync_path(tmp_path)?;
    fs::rename(tmp_path, final_path)?;
    // Make the rename itself durable by fsyncing the containing directory.
    fsync_parent_dir(final_path);
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn replace_file(tmp_path: &Path, final_path: &Path) -> io::Result<()> {
    // Flush the staged bytes to disk before any rename — see the non-Windows variant.
    fsync_path(tmp_path)?;
    if !final_path.exists() {
        fs::rename(tmp_path, final_path)?;
        fsync_parent_dir(final_path);
        return Ok(());
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

    // Make the two renames durable before returning, mirroring the non-Windows branch. Without
    // flushing the directory's metadata, a hard crash (power loss / BSOD — not a clean exit) can
    // recover the first rename (final -> backup) but NOT the second (tmp -> final), leaving
    // `final_path` MISSING with the only good copy stranded at the `.replace-bak` sibling — after
    // which a loader silently falls back to defaults (e.g. consent opt-ins flip OFF). `recover_
    // interrupted_replace` restores that orphan on the next load; this fsync narrows the window so it
    // is rarely needed.
    fsync_parent_dir(final_path);

    // The swap is already COMPLETE and DURABLE (the tmp -> final rename above succeeded and the parent
    // dir was fsync'd), so the new bytes are on disk at `final_path`. Deleting the throwaway backup is
    // NON-load-bearing cleanup: recover_interrupted_replace only promotes a backup when `final_path` is
    // MISSING, and the next replace_file removes any leftover (lines 39-43). On Windows a transient
    // scanner lock (Defender / Search Indexer opening the just-created `.replace-bak` file without
    // FILE_SHARE_DELETE) makes remove_file fail with ERROR_SHARING_VIOLATION; propagating it would report
    // a SUCCEEDED, durable write as FAILED — a dishonest "save failed" the caller surfaces to the user.
    // Best-effort, exactly like fsync_parent_dir above (and unlike the pre-swap cleanup at 39-43, whose
    // failure honestly means the swap never started and the file is unchanged).
    if let Err(e) = fs::remove_file(&backup_path) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!(
                "Atomic replace of {} succeeded; leftover backup {} could not be removed (transient lock?): {e}",
                final_path.display(),
                backup_path.display()
            );
        }
    }
    Ok(())
}

/// Best-effort fsync of `final_path`'s containing directory so a rename's directory-entry metadata
/// reaches stable storage. On Windows a directory handle requires `FILE_FLAG_BACKUP_SEMANTICS`, and
/// `sync_all()` then maps to `FlushFileBuffers`. Failure is ignored: the replacement already
/// succeeded, so a fsync error must not turn a good write into a hard error.
#[cfg(target_os = "windows")]
pub(crate) fn fsync_parent_dir(final_path: &Path) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    if let Some(parent) = final_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(dir) = fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn fsync_parent_dir(final_path: &Path) {
    if let Some(parent) = final_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

/// Strictly persist a directory's entries.
///
/// This is deliberately separate from [`fsync_parent_dir`]: existing atomic-file callers use that
/// older helper after a replacement has already committed and intentionally treat a directory-sync
/// failure as best-effort cleanup. Recovery/snapshot commit protocols, by contrast, must not report a
/// generation durable until the filesystem accepts the metadata barrier, so they use this fallible
/// variant and propagate every error.
#[cfg(target_os = "windows")]
pub(crate) fn fsync_directory_strict(directory: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("directory durability barrier requires a real directory: {}", directory.display()),
        ));
    }

    // FlushFileBuffers requires a write-capable handle on Windows. Opening the directory read-only
    // (the common Unix pattern and the former best-effort implementation above) yields
    // ERROR_ACCESS_DENIED on NTFS even with FILE_FLAG_BACKUP_SEMANTICS.
    fs::OpenOptions::new().write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(directory)?.sync_all()
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn fsync_directory_strict(directory: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("directory durability barrier requires a real directory: {}", directory.display()),
        ));
    }
    fs::File::open(directory)?.sync_all()
}

/// Strictly persist the containing directory entry for `final_path`.
///
/// A bare relative file lives in the current directory, so `.` is the correct metadata authority
/// rather than silently skipping the barrier.
pub(crate) fn fsync_parent_dir_strict(final_path: &Path) -> io::Result<()> {
    let parent = final_path.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    fsync_directory_strict(parent)
}

#[cfg(target_os = "windows")]
pub(crate) fn replacement_backup_path(final_path: &Path) -> PathBuf {
    let file_name = final_path.file_name().and_then(|name| name.to_str()).unwrap_or("target");
    final_path.with_file_name(format!("{file_name}.replace-bak-{}", std::process::id()))
}

/// fsync a file's contents to stable storage. The atomic-rename machinery guarantees the
/// *swap* is atomic; this guarantees the *bytes* are on disk first, so a crash or power-
/// loss can never leave a renamed-but-empty/partial file.
fn fsync_path(path: &Path) -> io::Result<()> {
    // Open for write: on Windows sync_all() maps to FlushFileBuffers, which requires a
    // write-access handle (a read-only handle returns ERROR_ACCESS_DENIED).
    fs::OpenOptions::new().write(true).open(path)?.sync_all()
}

/// Recover from a `replace_file` that was interrupted — a hard crash between its two renames, or a
/// rename failure whose restore also failed — both of which can leave `final_path` MISSING with the
/// good data stranded at a `.replace-bak-*` sibling. If `final_path` is absent but such a sibling
/// exists, promote the NEWEST one back into place and return `Ok(true)`. No-op (returns `Ok(false)`)
/// when `final_path` exists or no backup is present, so it is harmless on platforms/paths that never
/// create backups. Call it at LOAD time, before falling back to defaults: under the single-instance
/// lock there is no concurrent writer, so promoting an orphaned backup is safe and prevents silently
/// reverting persisted state (e.g. consent opt-ins) to defaults while a valid copy sits next to it.
pub fn recover_interrupted_replace(final_path: &Path) -> io::Result<bool> {
    if final_path.exists() {
        return Ok(false);
    }
    let Some(parent) = final_path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(false);
    };
    let Some(file_name) = final_path.file_name().and_then(|n| n.to_str()) else {
        return Ok(false);
    };
    let prefix = format!("{file_name}.replace-bak-");

    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };

    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        let is_newer = match &newest {
            None => true,
            Some((best, _)) => mtime >= *best,
        };
        if is_newer {
            newest = Some((mtime, path));
        }
    }

    match newest {
        Some((_, backup)) => {
            fs::rename(&backup, final_path)?;
            tracing::warn!(
                "Recovered {} from an interrupted-replacement backup {}",
                final_path.display(),
                backup.display()
            );
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Remove only replacement backups belonging to `final_path`.
///
/// This is used when ABSENCE is itself the committed state (for example a snapshot explicitly says
/// there is no paid-review pilot policy). Deleting the canonical file without deleting its known
/// `.replace-bak-*` siblings would let the next load's crash recovery resurrect stale state.
pub fn remove_replacement_backups(final_path: &Path) -> io::Result<usize> {
    let Some(parent) = final_path.parent().filter(|path| !path.as_os_str().is_empty()) else {
        return Ok(0);
    };
    let Some(file_name) = final_path.file_name().and_then(|name| name.to_str()) else {
        return Ok(0);
    };
    let prefix = format!("{file_name}.replace-bak-");
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_str().is_some_and(|name| name.starts_with(&prefix)) || !entry.path().is_file() {
            continue;
        }
        fs::remove_file(entry.path())?;
        removed += 1;
    }
    Ok(removed)
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
    fn replace_file_fsyncs_and_preserves_exact_bytes() {
        // Exercises the durability path (fsync tmp → rename → fsync dir) and asserts the
        // promoted file holds exactly the staged bytes — a non-trivial multi-block payload.
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = tmp_dir.path().join("dataset.parquet.tmp");
        let final_path = tmp_dir.path().join("dataset.parquet");
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&tmp_path, &payload).expect("write tmp");

        replace_file(&tmp_path, &final_path).expect("replace file");

        assert_eq!(fs::read(&final_path).expect("read final"), payload);
        assert!(!tmp_path.exists());
    }

    #[test]
    fn strict_directory_barriers_propagate_and_accept_real_directories() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let child = tmp_dir.path().join("durable-entry");
        fs::write(&child, b"durable").expect("write child");
        fs::OpenOptions::new().write(true).open(&child).unwrap().sync_all().unwrap();

        fsync_directory_strict(tmp_dir.path()).expect("strict directory FlushFileBuffers/fsync");
        fsync_parent_dir_strict(&child).expect("strict parent directory barrier");

        let ordinary_file = tmp_dir.path().join("not-a-directory");
        fs::write(&ordinary_file, b"file").unwrap();
        assert_eq!(
            fsync_directory_strict(&ordinary_file).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "strict directory barriers must never silently accept a file"
        );
    }

    #[test]
    fn recover_interrupted_replace_promotes_orphaned_backup() {
        // Post-crash state: the canonical file is missing, the good data is stranded at a
        // `.replace-bak-*` sibling (the second rename never became durable). Recovery must promote it.
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let final_path = tmp_dir.path().join("settings.json");
        let backup = tmp_dir.path().join("settings.json.replace-bak-1234");
        fs::write(&backup, r#"{"recovered":true}"#).expect("write backup");
        assert!(!final_path.exists());

        let recovered = recover_interrupted_replace(&final_path).expect("recover");

        assert!(recovered, "an orphaned backup must be reported as recovered");
        assert_eq!(fs::read_to_string(&final_path).expect("read final"), r#"{"recovered":true}"#);
        assert!(!backup.exists(), "the promoted backup is consumed (renamed into place)");
    }

    #[test]
    fn recover_interrupted_replace_is_noop_when_final_exists() {
        // The common case: the canonical file is present, so recovery must NOT touch it or a stray
        // backup — promoting over a live file would clobber the current state with stale data.
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let final_path = tmp_dir.path().join("settings.json");
        fs::write(&final_path, "current").expect("write final");
        let backup = tmp_dir.path().join("settings.json.replace-bak-1");
        fs::write(&backup, "stale").expect("write backup");

        assert!(!recover_interrupted_replace(&final_path).expect("recover"));
        assert_eq!(fs::read_to_string(&final_path).expect("read final"), "current");
        assert!(backup.exists(), "a live file must leave any stray backup untouched");
    }

    #[test]
    fn recover_interrupted_replace_is_noop_with_no_backup() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let final_path = tmp_dir.path().join("settings.json");
        assert!(!recover_interrupted_replace(&final_path).expect("recover"));
        assert!(!final_path.exists());
    }

    #[test]
    fn explicit_absence_cleanup_removes_only_the_targets_replacement_backups() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let final_path = tmp_dir.path().join("review_pilot_policy.json");
        let first = tmp_dir.path().join("review_pilot_policy.json.replace-bak-10");
        let second = tmp_dir.path().join("review_pilot_policy.json.replace-bak-20");
        let unrelated = tmp_dir.path().join("settings.json.replace-bak-10");
        fs::write(&first, "old pilot one").unwrap();
        fs::write(&second, "old pilot two").unwrap();
        fs::write(&unrelated, "settings").unwrap();

        assert_eq!(remove_replacement_backups(&final_path).unwrap(), 2);
        assert!(!first.exists() && !second.exists());
        assert!(unrelated.exists(), "cleanup must never widen beyond the exact target file");
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
