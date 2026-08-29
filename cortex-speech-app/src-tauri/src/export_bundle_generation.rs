use crate::atomic_file::{fsync_directory_strict, fsync_parent_dir_strict};
use crate::error::{AppError, AppResult};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProductionBundlePublishPoint {
    StagedAndDurableBeforePromotion,
    RenamedBeforeParentBarrier,
    DurableAndVerifiedBeforeReturn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactDigest {
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GenerationDigest {
    directories: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, ArtifactDigest>,
}

fn canonical_relative_artifact_path(text: &str) -> Option<PathBuf> {
    // Path::components normalizes repeated separators and some `.` aliases. Validate the textual
    // spelling first so SHA256SUMS and manifest.json cannot name one physical file multiple ways.
    if text.is_empty()
        || text.contains('\\')
        || text.starts_with('/')
        || text.ends_with('/')
        || text.split('/').any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    let path = PathBuf::from(text);
    path.components().all(|component| matches!(component, Component::Normal(_))).then_some(path)
}

fn hash_regular_file(path: &Path) -> AppResult<ArtifactDigest> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::Validation(format!(
            "production bundle artifact must be a regular non-symlink file: {}",
            path.display()
        )));
    }

    // Reopen every artifact instead of trusting metadata captured while the exporter wrote it.
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
        size_bytes = size_bytes.checked_add(read as u64).ok_or_else(|| {
            AppError::Validation(format!("production bundle artifact is too large: {}", path.display()))
        })?;
    }
    let sha256 = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(ArtifactDigest { size_bytes, sha256 })
}

fn is_unpublished_temporary_artifact(path: &Path) -> bool {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    let numbered_temporary = name.rfind(".tmp-").is_some_and(|index| {
        let tail = &name[index + ".tmp-".len()..];
        !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-')
    });
    name.ends_with(".tmp") || numbered_temporary || name.contains(".cortex-wav-") || name.contains(".cortex-ledger-")
}

fn collect_generation(root: &Path, directory: &Path, digest: &mut GenerationDigest) -> AppResult<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(AppError::Validation(format!(
                "production bundle may not contain symlinks: {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| AppError::Validation("production bundle artifact escaped its private root".to_string()))?
            .to_path_buf();
        if metadata.is_dir() {
            if !digest.directories.insert(relative) {
                return Err(AppError::Validation("duplicate production bundle directory identity".to_string()));
            }
            collect_generation(root, &path, digest)?;
        } else if metadata.is_file() {
            if is_unpublished_temporary_artifact(&path) {
                return Err(AppError::Validation(format!(
                    "production bundle contains an unpublished temporary artifact: {}",
                    path.display()
                )));
            }
            let artifact = hash_regular_file(&path)?;
            if digest.files.insert(relative, artifact).is_some() {
                return Err(AppError::Validation("duplicate production bundle artifact identity".to_string()));
            }
        } else {
            return Err(AppError::Validation(format!(
                "production bundle contains a non-file artifact: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn generation_digest(root: &Path) -> AppResult<GenerationDigest> {
    let metadata = std::fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::Validation(format!(
            "production bundle root must be a regular non-symlink directory: {}",
            root.display()
        )));
    }
    let mut digest = GenerationDigest { directories: BTreeSet::new(), files: BTreeMap::new() };
    collect_generation(root, root, &mut digest)?;
    Ok(digest)
}

fn verified_sealed_digest(root: &Path) -> AppResult<GenerationDigest> {
    let digest = generation_digest(root)?;
    let sums_relative = PathBuf::from("SHA256SUMS");
    let manifest_relative = PathBuf::from("manifest.json");
    if !digest.files.contains_key(&manifest_relative) || !digest.files.contains_key(&sums_relative) {
        return Err(AppError::Validation(format!(
            "production bundle is missing manifest.json or SHA256SUMS: {}",
            root.display()
        )));
    }

    let sums = std::fs::read_to_string(root.join(&sums_relative))?;
    let mut declared = BTreeMap::new();
    for (line_number, line) in sums.lines().enumerate() {
        let (sha256, relative_text) = line.split_once("  ").ok_or_else(|| {
            AppError::Validation(format!("invalid SHA256SUMS line {} in {}", line_number + 1, root.display()))
        })?;
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AppError::Validation(format!(
                "invalid SHA-256 at SHA256SUMS line {} in {}",
                line_number + 1,
                root.display()
            )));
        }
        let relative = canonical_relative_artifact_path(relative_text).ok_or_else(|| {
            AppError::Validation(format!(
                "non-canonical artifact path at SHA256SUMS line {} in {}",
                line_number + 1,
                root.display()
            ))
        })?;
        if relative == sums_relative {
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

    let mut actual = digest.files.clone();
    actual.remove(&sums_relative);
    if actual.len() != declared.len()
        || actual.iter().any(|(relative, artifact)| declared.get(relative) != Some(&artifact.sha256))
    {
        return Err(AppError::Validation(format!(
            "production bundle inventory does not match SHA256SUMS: {}",
            root.display()
        )));
    }

    // `manifest.json.files` is the bundle's semantic inventory, whereas SHA256SUMS is the exact
    // physical inventory. Require every semantic artifact to be a safe, unique, present and hashed
    // relative path. Extra completion artifacts (manifest.json, dataset_card.md and SHA256SUMS) are
    // intentionally allowed because the manifest is written before those final seal files.
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join(&manifest_relative))?)?;
    let manifest_files = manifest.get("files").and_then(serde_json::Value::as_array).ok_or_else(|| {
        AppError::Validation(format!("production bundle manifest has no files array: {}", root.display()))
    })?;
    let mut semantic_inventory = BTreeSet::new();
    for (index, value) in manifest_files.iter().enumerate() {
        let relative_text = value.as_str().ok_or_else(|| {
            AppError::Validation(format!(
                "production bundle manifest files entry {} is not a string: {}",
                index + 1,
                root.display()
            ))
        })?;
        let relative = canonical_relative_artifact_path(relative_text).ok_or_else(|| {
            AppError::Validation(format!(
                "production bundle manifest files entry {} is not canonical: {}",
                index + 1,
                root.display()
            ))
        })?;
        if relative == manifest_relative || relative == sums_relative {
            return Err(AppError::Validation(format!(
                "production bundle manifest files entry {} is unsafe: {}",
                index + 1,
                root.display()
            )));
        }
        if !semantic_inventory.insert(relative.clone()) {
            return Err(AppError::Validation(format!(
                "production bundle manifest repeats artifact {}: {}",
                relative.display(),
                root.display()
            )));
        }
        if !actual.contains_key(&relative) || !declared.contains_key(&relative) {
            return Err(AppError::Validation(format!(
                "production bundle manifest lists an absent or unhashed artifact {}: {}",
                relative.display(),
                root.display()
            )));
        }
    }
    Ok(digest)
}

#[cfg(test)]
pub(super) fn verify_sealed_generation(root: &Path) -> AppResult<()> {
    verified_sealed_digest(root).map(|_| ())
}

fn sync_generation_tree(directory: &Path) -> AppResult<()> {
    let metadata = std::fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::Validation(format!(
            "production bundle staging path is not a regular directory: {}",
            directory.display()
        )));
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(AppError::Validation(format!(
                "production bundle may not contain symlinks: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            sync_generation_tree(&path)?;
        } else if metadata.is_file() {
            // FlushFileBuffers requires a write-capable handle on Windows.
            std::fs::OpenOptions::new().write(true).open(&path)?.sync_all()?;
        } else {
            return Err(AppError::Validation(format!(
                "production bundle contains a non-file artifact: {}",
                path.display()
            )));
        }
    }
    fsync_directory_strict(directory).map_err(AppError::Io)
}

#[cfg(target_os = "windows")]
fn rename_generation_no_replace_write_through(source: &Path, destination: &Path) -> std::io::Result<()> {
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
fn rename_generation_no_replace_write_through(source: &Path, destination: &Path) -> std::io::Result<()> {
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
fn rename_generation_no_replace_write_through(source: &Path, destination: &Path) -> std::io::Result<()> {
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
fn rename_generation_no_replace_write_through(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace directory publication is unsupported on this platform",
    ))
}

fn validate_initial_target(output_dir: &Path) -> AppResult<bool> {
    match std::fs::symlink_metadata(output_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(AppError::Validation(format!(
            "Production export target must be an absent or empty directory: {}",
            output_dir.display()
        ))),
        Ok(_) => {
            if std::fs::read_dir(output_dir)?.next().transpose()?.is_some() {
                Err(AppError::Validation(format!(
                    "Production export target must be absent or empty; refusing to reuse or seal existing files in {}",
                    output_dir.display()
                )))
            } else {
                Ok(true)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn require_existing_real_parent(parent_dir: &Path) -> AppResult<()> {
    let metadata = match std::fs::symlink_metadata(parent_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::Validation(format!(
                "Production export parent must already exist; refusing an unsealed ancestor creation: {}",
                parent_dir.display()
            )));
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::Validation(format!(
            "Production export parent must be a regular non-symlink directory: {}",
            parent_dir.display()
        )));
    }
    // Probe the same strict directory barrier needed for commit before writing a potentially large
    // staged export. An unsupported filesystem therefore fails before any generated bytes exist.
    fsync_directory_strict(parent_dir).map_err(AppError::Io)
}

fn require_absent_target(output_dir: &Path) -> AppResult<()> {
    match std::fs::symlink_metadata(output_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(AppError::Validation(format!(
            "Production export target appeared during generation; refusing to overwrite it: {}",
            output_dir.display()
        ))),
    }
}

fn cleanup_private_stage(staging_dir: &Path, parent_dir: &Path, staging_prefix: &str) {
    let metadata = match std::fs::symlink_metadata(staging_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(path = %staging_dir.display(), %error, "could not inspect private production-bundle stage");
            return;
        }
    };
    let safe_name =
        staging_dir.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(staging_prefix));
    let safe_parent = staging_dir
        .canonicalize()
        .ok()
        .zip(parent_dir.canonicalize().ok())
        .is_some_and(|(resolved, resolved_parent)| resolved.parent() == Some(resolved_parent.as_path()));
    if metadata.file_type().is_symlink() || !metadata.is_dir() || !safe_name || !safe_parent {
        tracing::error!(path = %staging_dir.display(), "refusing unsafe production-bundle staging cleanup");
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(staging_dir) {
        // A cleanup failure leaves only the generated hidden sibling quarantined. It never makes an
        // unpublished tree visible at the requested export path.
        tracing::warn!(path = %staging_dir.display(), %error, "could not remove private production-bundle stage");
    }
}

struct StageGuard<'a> {
    path: PathBuf,
    parent: &'a Path,
    prefix: &'a str,
}

impl Drop for StageGuard<'_> {
    fn drop(&mut self) {
        cleanup_private_stage(&self.path, self.parent, self.prefix);
    }
}

pub(super) fn publish_new_generation<T, F>(output_dir: &Path, build: F) -> AppResult<T>
where
    F: FnOnce(&Path) -> AppResult<T>,
{
    publish_new_generation_with_hook(output_dir, build, |_point, _path| Ok(()))
}

pub(super) fn publish_new_generation_with_hook<T, F, H>(output_dir: &Path, build: F, mut hook: H) -> AppResult<T>
where
    F: FnOnce(&Path) -> AppResult<T>,
    H: FnMut(ProductionBundlePublishPoint, &Path) -> AppResult<()>,
{
    let parent_dir = output_dir.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    require_existing_real_parent(parent_dir)?;
    let output_name = output_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| AppError::Validation("Production export target must have a UTF-8 directory name".to_string()))?;

    // A directory picker may create one empty directory. Remove that non-export before any bytes are
    // staged, then make absence durable. From this point onward every existing destination is foreign
    // state and is preserved; promotion uses an atomic no-replace primitive as the final race guard.
    if validate_initial_target(output_dir)? {
        std::fs::remove_dir(output_dir)?;
        fsync_parent_dir_strict(output_dir).map_err(AppError::Io)?;
    }

    let staging_prefix = format!(".{output_name}.cortex-stage-");
    let staging_dir = parent_dir.join(format!("{staging_prefix}{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&staging_dir)?;
    let _stage_guard = StageGuard { path: staging_dir.clone(), parent: parent_dir, prefix: &staging_prefix };

    let result = build(&staging_dir)?;
    sync_generation_tree(&staging_dir)?;
    let staged_digest = verified_sealed_digest(&staging_dir)?;
    hook(ProductionBundlePublishPoint::StagedAndDurableBeforePromotion, &staging_dir)?;

    // Catch mutation after the first seal verification, including mutation deliberately injected by
    // a deterministic fault test. No target entry has been created yet.
    if verified_sealed_digest(&staging_dir)? != staged_digest {
        return Err(AppError::Validation(
            "production bundle staging tree changed after its durability barrier".to_string(),
        ));
    }
    require_absent_target(output_dir)?;
    rename_generation_no_replace_write_through(&staging_dir, output_dir)?;
    hook(ProductionBundlePublishPoint::RenamedBeforeParentBarrier, output_dir)?;

    fsync_parent_dir_strict(output_dir).map_err(AppError::Io)?;
    let published_digest = verified_sealed_digest(output_dir)?;
    if published_digest != staged_digest {
        return Err(AppError::Validation(
            "published production bundle differs from its durable staged generation".to_string(),
        ));
    }
    hook(ProductionBundlePublishPoint::DurableAndVerifiedBeforeReturn, output_dir)?;
    Ok(result)
}
