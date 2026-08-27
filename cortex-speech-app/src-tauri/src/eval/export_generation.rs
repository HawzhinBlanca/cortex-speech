use crate::error::{AppError, AppResult};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExportPublishPoint {
    StagedAndSynced,
    PreviousGenerationMoved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportArtifactDigest {
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExportGenerationDigest {
    directories: std::collections::BTreeSet<std::path::PathBuf>,
    files: std::collections::BTreeMap<std::path::PathBuf, ExportArtifactDigest>,
}

fn hash_export_artifact(path: &std::path::Path) -> AppResult<ExportArtifactDigest> {
    use sha2::{Digest as _, Sha256};
    use std::io::Read as _;

    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::Validation(format!(
            "export staging artifact must be a regular non-symlink file: {}",
            path.display()
        )));
    }
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size_bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size_bytes = size_bytes
            .checked_add(read as u64)
            .ok_or_else(|| AppError::Validation(format!("export artifact is too large: {}", path.display())))?;
    }
    let sha256 = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(ExportArtifactDigest { size_bytes, sha256 })
}

fn collect_export_generation(
    root: &std::path::Path,
    directory: &std::path::Path,
    digest: &mut ExportGenerationDigest,
) -> AppResult<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(AppError::Validation(format!(
                "export staging tree may not contain symlinks: {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| AppError::Validation("export staging artifact escaped its private root".to_string()))?
            .to_path_buf();
        if metadata.is_dir() {
            if !digest.directories.insert(relative) {
                return Err(AppError::Validation("duplicate export staging directory identity".to_string()));
            }
            collect_export_generation(root, &path, digest)?;
        } else if metadata.is_file() {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
            let numbered_temporary = name.rfind(".tmp-").is_some_and(|index| {
                let tail = &name[index + ".tmp-".len()..];
                !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-')
            });
            if name.ends_with(".tmp")
                || numbered_temporary
                || name.contains(".cortex-wav-")
                || name.contains(".cortex-ledger-")
            {
                return Err(AppError::Validation(format!(
                    "export staging tree contains an unpublished temporary artifact: {}",
                    path.display()
                )));
            }
            let artifact = hash_export_artifact(&path)?;
            if digest.files.insert(relative, artifact).is_some() {
                return Err(AppError::Validation("duplicate export staging artifact identity".to_string()));
            }
        } else {
            return Err(AppError::Validation(format!(
                "export staging tree contains a non-file artifact: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn export_generation_digest(root: &std::path::Path) -> AppResult<ExportGenerationDigest> {
    let metadata = std::fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::Validation("export staging root must be a regular non-symlink directory".to_string()));
    }
    let mut digest = ExportGenerationDigest {
        directories: std::collections::BTreeSet::new(),
        files: std::collections::BTreeMap::new(),
    };
    collect_export_generation(root, root, &mut digest)?;
    Ok(digest)
}

pub(super) fn verify_sealed_export_generation(root: &std::path::Path) -> AppResult<bool> {
    let digest = export_generation_digest(root)?;
    if digest.files.is_empty() && digest.directories.is_empty() {
        return Ok(false);
    }
    let sums_relative = std::path::PathBuf::from("SHA256SUMS");
    if !digest.files.contains_key(&sums_relative) {
        return Err(AppError::Validation(format!(
            "existing export generation is non-empty but has no SHA256SUMS: {}",
            root.display()
        )));
    }
    let sums = std::fs::read_to_string(root.join(&sums_relative))?;
    let mut declared = std::collections::BTreeMap::new();
    for (line_number, line) in sums.lines().enumerate() {
        let (sha256, relative) = line.split_once("  ").ok_or_else(|| {
            AppError::Validation(format!("invalid SHA256SUMS line {} in {}", line_number + 1, root.display()))
        })?;
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AppError::Validation(format!(
                "invalid SHA-256 at SHA256SUMS line {} in {}",
                line_number + 1,
                root.display()
            )));
        }
        let relative = std::path::PathBuf::from(relative);
        if relative.as_os_str().is_empty()
            || !relative.components().all(|component| matches!(component, std::path::Component::Normal(_)))
            || relative == sums_relative
        {
            return Err(AppError::Validation(format!(
                "unsafe artifact path at SHA256SUMS line {} in {}",
                line_number + 1,
                root.display()
            )));
        }
        if declared.insert(relative, sha256.to_ascii_lowercase()).is_some() {
            return Err(AppError::Validation(format!(
                "duplicate artifact at SHA256SUMS line {} in {}",
                line_number + 1,
                root.display()
            )));
        }
    }
    let mut actual = digest.files;
    actual.remove(&sums_relative);
    if actual.len() != declared.len()
        || actual.iter().any(|(relative, artifact)| declared.get(relative) != Some(&artifact.sha256))
    {
        return Err(AppError::Validation(format!(
            "export generation inventory does not match SHA256SUMS: {}",
            root.display()
        )));
    }
    Ok(true)
}

#[cfg(target_os = "windows")]
fn sync_export_directory(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let directory = std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)?;
    match directory.sync_all() {
        Ok(()) => Ok(()),
        // NTFS commonly refuses FlushFileBuffers on directory handles even with
        // FILE_FLAG_BACKUP_SEMANTICS. Every file is flushed separately and directory renames use
        // MoveFileExW(MOVEFILE_WRITE_THROUGH), which is the Windows durability boundary.
        Err(error)
            if matches!(error.kind(), std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(target_os = "windows"))]
fn sync_export_directory(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

fn sync_export_tree(directory: &std::path::Path) -> AppResult<()> {
    let metadata = std::fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::Validation(format!(
            "export staging directory is not a regular directory: {}",
            directory.display()
        )));
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(AppError::Validation(format!(
                "export staging tree may not contain symlinks: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            sync_export_tree(&path)?;
        } else if metadata.is_file() {
            std::fs::OpenOptions::new().write(true).open(&path)?.sync_all()?;
        } else {
            return Err(AppError::Validation(format!(
                "export staging tree contains a non-file artifact: {}",
                path.display()
            )));
        }
    }
    sync_export_directory(directory).map_err(AppError::Io)
}

#[cfg(target_os = "windows")]
fn rename_export_directory(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: both buffers are NUL-terminated and remain live for the call. The caller always uses
    // same-parent, non-overlapping private directory names, so MoveFileExW performs one filesystem
    // rename and WRITE_THROUGH makes that metadata transition durable before it returns.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn rename_export_directory(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[derive(Debug)]
pub(super) struct ExportTargetLayout {
    pub(super) parent: std::path::PathBuf,
    stage_prefix: String,
    pub(super) backup_prefix: String,
}

pub(super) fn export_target_layout(output_dir: &std::path::Path) -> AppResult<ExportTargetLayout> {
    let parent = output_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    std::fs::create_dir_all(&parent)?;
    let output_name = output_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| AppError::Validation("export target must have a UTF-8 directory name".to_string()))?
        .to_string();
    Ok(ExportTargetLayout {
        parent,
        stage_prefix: format!(".{output_name}.cortex-export-stage-"),
        backup_prefix: format!(".{output_name}.cortex-export-backup-"),
    })
}

fn validate_export_target(output_dir: &std::path::Path) -> AppResult<()> {
    match std::fs::symlink_metadata(output_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(AppError::Validation(format!(
            "export target must be an absent or regular non-symlink directory: {}",
            output_dir.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::Io(error)),
    }
}

fn cleanup_private_export_directory(path: &std::path::Path, parent: &std::path::Path, prefix: &str) {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not inspect private export directory for cleanup");
            return;
        }
    };
    let safe_name = path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(prefix));
    let safe_parent = path
        .canonicalize()
        .ok()
        .zip(parent.canonicalize().ok())
        .is_some_and(|(resolved, resolved_parent)| resolved.parent() == Some(resolved_parent.as_path()));
    if metadata.file_type().is_symlink() || !metadata.is_dir() || !safe_name || !safe_parent {
        tracing::error!(path = %path.display(), "refusing unsafe private export directory cleanup");
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(path) {
        tracing::warn!(path = %path.display(), %error, "could not remove private export directory");
    }
}

struct ExportStageGuard<'a> {
    path: std::path::PathBuf,
    parent: &'a std::path::Path,
    prefix: &'a str,
}

impl Drop for ExportStageGuard<'_> {
    fn drop(&mut self) {
        cleanup_private_export_directory(&self.path, self.parent, self.prefix);
    }
}

pub(super) fn recover_interrupted_export_publication(
    output_dir: &std::path::Path,
    layout: &ExportTargetLayout,
) -> AppResult<bool> {
    if output_dir.exists() {
        return Ok(false);
    }
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(&layout.parent)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_str().is_some_and(|name| name.starts_with(&layout.backup_prefix)) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        match verify_sealed_export_generation(&entry.path()) {
            Ok(true) | Ok(false) => {}
            Err(error) => {
                tracing::error!(path = %entry.path().display(), %error, "refusing invalid interrupted-export backup");
                continue;
            }
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        if newest.as_ref().map_or(true, |(best, _)| modified >= *best) {
            newest = Some((modified, entry.path()));
        }
    }
    let Some((_, backup)) = newest else {
        return Ok(false);
    };
    rename_export_directory(&backup, output_dir)?;
    sync_export_directory(&layout.parent)?;
    tracing::warn!(
        output = %output_dir.display(),
        backup = %backup.display(),
        "recovered complete export generation after interrupted directory swap"
    );
    Ok(true)
}

fn restore_export_backup(
    output_dir: &std::path::Path,
    backup: &std::path::Path,
    parent: &std::path::Path,
) -> AppResult<()> {
    if output_dir.exists() {
        return Err(AppError::Other(format!(
            "cannot restore export backup because target was concurrently recreated: {}",
            output_dir.display()
        )));
    }
    rename_export_directory(backup, output_dir)?;
    sync_export_directory(parent)?;
    Ok(())
}

pub(super) fn publish_export_generation<T, Build, Hook>(
    output_dir: &std::path::Path,
    build: Build,
    mut hook: Hook,
) -> AppResult<T>
where
    Build: FnOnce(&std::path::Path) -> AppResult<T>,
    Hook: FnMut(ExportPublishPoint, &std::path::Path, Option<&std::path::Path>) -> AppResult<()>,
{
    let layout = export_target_layout(output_dir)?;
    recover_interrupted_export_publication(output_dir, &layout)?;
    validate_export_target(output_dir)?;
    let predecessor = if output_dir.exists() {
        verify_sealed_export_generation(output_dir)?;
        Some(export_generation_digest(output_dir)?)
    } else {
        None
    };

    let stage_path = layout.parent.join(format!("{}{}", layout.stage_prefix, Uuid::new_v4().simple()));
    std::fs::create_dir(&stage_path)?;
    let stage = ExportStageGuard { path: stage_path, parent: &layout.parent, prefix: &layout.stage_prefix };
    let result = build(&stage.path)?;
    sync_export_tree(&stage.path)?;
    if !verify_sealed_export_generation(&stage.path)? {
        return Err(AppError::Other("export builder produced no sealed generation".to_string()));
    }
    let sealed_digest = export_generation_digest(&stage.path)?;
    hook(ExportPublishPoint::StagedAndSynced, &stage.path, None)?;
    if export_generation_digest(&stage.path)? != sealed_digest {
        return Err(AppError::Other(
            "private export generation changed after it was sealed; publication refused".to_string(),
        ));
    }

    // Revalidate at the actual commit boundary. Existing complete output is moved, never edited;
    // an interruption between the two same-parent renames therefore leaves that exact generation
    // recoverable at a typed private backup path rather than mixing old and new artifacts.
    validate_export_target(output_dir)?;
    let output_exists = output_dir.exists();
    if output_exists != predecessor.is_some() {
        return Err(AppError::Other(
            "export target generation appeared or disappeared during private staging; publication refused".to_string(),
        ));
    }
    let backup_path = if let Some(expected_predecessor) = predecessor {
        // A non-empty predecessor must itself be one complete checksummed generation. Empty picker
        // directories are allowed and represent "no generation"; arbitrary mixed legacy files are
        // never blessed as the rollback authority for an atomic swap.
        verify_sealed_export_generation(output_dir)?;
        if export_generation_digest(output_dir)? != expected_predecessor {
            return Err(AppError::Other(
                "export target generation changed during private staging; publication refused".to_string(),
            ));
        }
        let backup = layout.parent.join(format!("{}{}", layout.backup_prefix, Uuid::new_v4().simple()));
        rename_export_directory(output_dir, &backup)?;
        if let Err(error) = sync_export_directory(&layout.parent) {
            let restore = restore_export_backup(output_dir, &backup, &layout.parent);
            return match restore {
                Ok(()) => Err(AppError::Io(error)),
                Err(restore_error) => Err(AppError::Other(format!(
                    "export directory fsync failed after preserving the old generation ({error}); backup restore also failed: {restore_error}"
                ))),
            };
        }
        if let Err(error) = hook(ExportPublishPoint::PreviousGenerationMoved, &stage.path, Some(&backup)) {
            let message = error.to_string();
            return match restore_export_backup(output_dir, &backup, &layout.parent) {
                Ok(()) => Err(error),
                Err(restore_error) => Err(AppError::Other(format!(
                    "{message}; previous export generation remains at {} because rollback failed: {restore_error}",
                    backup.display()
                ))),
            };
        }
        if export_generation_digest(&stage.path)? != sealed_digest {
            let error = AppError::Other(
                "private export generation changed before atomic promotion; publication refused".to_string(),
            );
            return match restore_export_backup(output_dir, &backup, &layout.parent) {
                Ok(()) => Err(error),
                Err(restore_error) => Err(AppError::Other(format!(
                    "{error}; previous export generation remains at {} because rollback failed: {restore_error}",
                    backup.display()
                ))),
            };
        }
        Some(backup)
    } else {
        if export_generation_digest(&stage.path)? != sealed_digest {
            return Err(AppError::Other(
                "private export generation changed before atomic promotion; publication refused".to_string(),
            ));
        }
        None
    };

    if let Err(error) = rename_export_directory(&stage.path, output_dir) {
        if let Some(backup) = backup_path.as_deref() {
            let restore = restore_export_backup(output_dir, backup, &layout.parent);
            return match restore {
                Ok(()) => Err(AppError::Io(error)),
                Err(restore_error) => Err(AppError::Other(format!(
                    "new export promotion failed ({error}); previous generation remains at {} because rollback failed: {restore_error}",
                    backup.display()
                ))),
            };
        }
        return Err(AppError::Io(error));
    }
    sync_export_directory(&layout.parent)?;

    if let Some(backup) = backup_path {
        cleanup_private_export_directory(&backup, &layout.parent, &layout.backup_prefix);
        if let Err(error) = sync_export_directory(&layout.parent) {
            tracing::warn!(%error, "new export is durable but obsolete backup cleanup could not be fsynced");
        }
    }
    Ok(result)
}
