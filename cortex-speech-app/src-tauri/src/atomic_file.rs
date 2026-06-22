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
    // Flush the staged bytes to stable storage before the rename: atomic rename makes
    // the swap atomic, but only fsync makes the *data* durable — without it a power-loss
    // right after the rename can expose a zero-length file where a curated artifact
    // should be (the cardinal sin for a tool whose output is human-verified labels).
    fsync_path(tmp_path)?;
    fs::rename(tmp_path, final_path)?;
    // Make the rename itself durable by fsyncing the containing directory.
    if let Some(parent) = final_path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
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

    match fs::remove_file(&backup_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Best-effort fsync of `final_path`'s containing directory so a rename's directory-entry metadata
/// reaches stable storage. On Windows a directory handle requires `FILE_FLAG_BACKUP_SEMANTICS`, and
/// `sync_all()` then maps to `FlushFileBuffers`. Failure is ignored: the replacement already
/// succeeded, so a fsync error must not turn a good write into a hard error.
#[cfg(target_os = "windows")]
fn fsync_parent_dir(final_path: &Path) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    if let Some(parent) = final_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(dir) = fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(target_os = "windows")]
fn replacement_backup_path(final_path: &Path) -> PathBuf {
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
