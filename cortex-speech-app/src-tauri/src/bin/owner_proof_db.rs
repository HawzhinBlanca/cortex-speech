//! Narrow, fail-closed database inspector/migrator for owner-product proof inputs.
//!
//! `inspect` never gives SQLite write authority over the supplied file: it works through the
//! application's detached in-memory snapshot opener. `migrate` accepts only a hash-bound database
//! inside a tool-owned staging directory and runs the exact `Database::initialize` path used by the
//! desktop application. It deliberately has no command for deleting campaign policy or authority.

use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::{migrations, review_campaign, review_pool, GIT_SHA};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
#[cfg(test)]
use std::io::BufReader;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::os::windows::ffi::OsStringExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

const STAGING_PREFIX: &str = ".owner-proof-inputs.staging-";
const HELPER_SOURCE: &str = include_str!("owner_proof_db.rs");
const CAMPAIGN_SETTING_KEYS: &[&str] =
    &["review_campaign.sequential_first_pass.v1", "review_campaign.sequential_progress.v1"];
const CAMPAIGN_TABLES: &[&str] = &[
    "review_campaign_registry",
    "review_campaign_focus",
    "review_campaign_transitions",
    "independent_review_decisions",
    "independent_review_reversals",
    "review_campaign_adjudications",
    "review_pool_registry",
    "review_pool_members",
    "review_pool_decisions",
    "review_pool_reversals",
    "review_pool_owner_adjudications",
    "review_pool_voice_certificates",
    "review_pool_dedup_manifests",
    "review_pool_duplicate_exclusions",
];

#[cfg(windows)]
#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
const FOLDER_ID_ROAMING_APP_DATA: Guid =
    Guid { data1: 0x3eb685db, data2: 0x65f9, data3: 0x4cf6, data4: [0xa0, 0x3a, 0xe3, 0xef, 0x65, 0x72, 0x9f, 0x3d] };

#[cfg(windows)]
const FOLDER_ID_LOCAL_APP_DATA: Guid =
    Guid { data1: 0xf1b32785, data2: 0x6fba, data3: 0x4fcf, data4: [0x9d, 0x55, 0x7b, 0x8e, 0x7f, 0x15, 0x70, 0x91] };

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHGetKnownFolderPath(rfid: *const Guid, flags: u32, token: isize, path: *mut *mut u16) -> i32;
}

#[cfg(windows)]
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(memory: *const c_void);
}

#[cfg(windows)]
#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}

#[cfg(windows)]
#[repr(C)]
struct ByHandleFileInformation {
    file_attributes: u32,
    creation_time: FileTime,
    last_access_time: FileTime,
    last_write_time: FileTime,
    volume_serial_number: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileInformationByHandle(handle: *mut c_void, information: *mut ByHandleFileInformation) -> i32;
    fn GetFinalPathNameByHandleW(handle: *mut c_void, path: *mut u16, length: u32, flags: u32) -> u32;
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Inspection {
    schema: u32,
    schema_version: i64,
    migration_history_entries: i64,
    schema_fingerprint_sha256: String,
    quick_check: Vec<String>,
    integrity_check: Vec<String>,
    foreign_key_violations: i64,
    segment_count: i64,
    distinct_audio_path_count: i64,
    sequential_campaign_present: bool,
    review_pool_present: bool,
    campaign_authority_rows: i64,
    campaign_authority_counts: BTreeMap<String, i64>,
}

fn usage() -> &'static str {
    "usage: owner_proof_db inspect --db <path> --expected-schema <n> --campaign <absent|required>\n\
     or: owner_proof_db schema-contract --expected-schema <n>\n\
     or: owner_proof_db migrate --source-db <path> --output-db <path> --staging-root <path> --source-sha256 <sha256> \
--expected-source-schema <n> --expected-target-schema <n>"
}

fn parse_flags(args: &[String], allowed: &[&str]) -> Result<BTreeMap<String, String>, String> {
    if args.len() % 2 != 0 {
        return Err(usage().to_string());
    }
    let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
    let mut parsed = BTreeMap::new();
    for pair in args.chunks_exact(2) {
        let flag = pair[0].as_str();
        if !allowed.contains(flag) {
            return Err(format!("unknown or unauthorized option {flag}"));
        }
        if pair[1].is_empty() {
            return Err(format!("{flag} cannot be empty"));
        }
        if parsed.insert(flag.to_string(), pair[1].clone()).is_some() {
            return Err(format!("duplicate option {flag}"));
        }
    }
    for flag in allowed {
        if !parsed.contains_key(flag) {
            return Err(format!("missing required option {flag}"));
        }
    }
    Ok(parsed)
}

fn flag<'a>(flags: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, String> {
    flags.get(name).map(String::as_str).ok_or_else(|| format!("missing required option {name}"))
}

fn parse_schema(value: &str, name: &str) -> Result<i64, String> {
    let parsed = value.parse::<i64>().map_err(|_| format!("{name} must be a positive integer"))?;
    if parsed <= 0 {
        return Err(format!("{name} must be a positive integer"));
    }
    Ok(parsed)
}

#[cfg(windows)]
fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse(_metadata: &fs::Metadata) -> bool {
    false
}

fn absolute_lexical(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("path cannot be empty".to_string());
    }
    let absolute =
        if path.is_absolute() { path.to_path_buf() } else { env::current_dir().map_err(|e| e.to_string())?.join(path) };
    if absolute.components().any(|component| matches!(component, Component::ParentDir)) {
        return Err("parent traversal is not permitted in proof-input paths".to_string());
    }
    Ok(absolute)
}

fn reject_links_and_reparse_points(path: &Path) -> Result<PathBuf, String> {
    let absolute = absolute_lexical(path)?;
    let mut ancestors: Vec<&Path> = absolute.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| "proof-input path does not exist".to_string())?;
        if metadata.file_type().is_symlink() || metadata_is_reparse(&metadata) {
            return Err("proof-input path contains a symlink or reparse point".to_string());
        }
    }
    fs::canonicalize(&absolute).map_err(|_| "proof-input path cannot be canonicalized".to_string())
}

fn canonical_comparison_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = absolute_lexical(path)?;
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
        let name =
            existing.file_name().ok_or_else(|| "proof-input comparison path has no existing ancestor".to_string())?;
        missing.push(name.to_os_string());
        existing =
            existing.parent().ok_or_else(|| "proof-input comparison path has no existing ancestor".to_string())?;
    }
    let mut canonical =
        fs::canonicalize(existing).map_err(|_| "proof-input comparison path cannot be canonicalized".to_string())?;
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

fn normalized_path(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('\\', "/");
    if value.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("//?/UNC/")) {
        value = format!("//{}", &value[8..]);
    } else if value
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("//?/") || prefix.eq_ignore_ascii_case("/??/"))
    {
        value = value[4..].to_string();
    }
    value.trim_end_matches('/').to_ascii_lowercase()
}

fn is_within(path: &Path, root: &Path) -> Result<bool, String> {
    let path = normalized_path(&canonical_comparison_path(path)?);
    let root = normalized_path(&canonical_comparison_path(root)?);
    Ok(path == root || path.strip_prefix(&root).is_some_and(|tail| tail.starts_with('/')))
}

#[cfg(windows)]
fn known_folder(identifier: &Guid) -> Result<PathBuf, String> {
    let mut raw: *mut u16 = std::ptr::null_mut();
    // SAFETY: `identifier` and the output pointer are valid for this call; a successful allocation
    // is owned by the shell and released exactly once with CoTaskMemFree below.
    let status = unsafe { SHGetKnownFolderPath(identifier, 0, 0, &mut raw) };
    if status < 0 || raw.is_null() {
        if !raw.is_null() {
            // SAFETY: the non-null pointer came from SHGetKnownFolderPath.
            unsafe { CoTaskMemFree(raw.cast()) };
        }
        return Err("Windows Known Folder authority cannot be resolved".to_string());
    }
    let mut length = 0_usize;
    // SAFETY: Windows returns a NUL-terminated PWSTR. The defensive 32K bound prevents an unbounded
    // read if that platform contract is ever violated.
    while length < 32_768 && unsafe { *raw.add(length) } != 0 {
        length += 1;
    }
    if length == 32_768 {
        // SAFETY: the pointer came from SHGetKnownFolderPath and has not yet been freed.
        unsafe { CoTaskMemFree(raw.cast()) };
        return Err("Windows Known Folder path is not bounded".to_string());
    }
    // SAFETY: the scan above proved `length` initialized UTF-16 code units before the terminator.
    let value = unsafe { std::slice::from_raw_parts(raw, length) };
    let path = PathBuf::from(OsString::from_wide(value));
    // SAFETY: the pointer came from SHGetKnownFolderPath and is released exactly once.
    unsafe { CoTaskMemFree(raw.cast()) };
    if path.as_os_str().is_empty() {
        return Err("Windows Known Folder authority resolved to an empty path".to_string());
    }
    Ok(path)
}

#[cfg(windows)]
fn protected_roots() -> Result<[PathBuf; 2], String> {
    Ok([
        known_folder(&FOLDER_ID_ROAMING_APP_DATA)?.join("cortex-speech"),
        known_folder(&FOLDER_ID_LOCAL_APP_DATA)?.join("CortexSpeech").join("private-production-releases"),
    ])
}

#[cfg(not(windows))]
fn protected_roots() -> Result<[PathBuf; 2], String> {
    let roaming = env::var_os("APPDATA").ok_or_else(|| "Windows AppData authority is unavailable".to_string())?;
    let local = env::var_os("LOCALAPPDATA").ok_or_else(|| "Windows AppData authority is unavailable".to_string())?;
    Ok([
        PathBuf::from(roaming).join("cortex-speech"),
        PathBuf::from(local).join("CortexSpeech").join("private-production-releases"),
    ])
}

fn reject_live_and_snapshot_paths(path: &Path) -> Result<(), String> {
    for component in path.components() {
        let value = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        if value == "snapshots" || value == "pinned" || value.starts_with("snapshot_") {
            return Err("snapshot paths are immutable recovery authority, not proof-input workspaces".to_string());
        }
    }
    for root in protected_roots()? {
        if is_within(path, &root)? {
            return Err("live AppData and active release paths are never proof-input targets".to_string());
        }
    }
    Ok(())
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    volume: u32,
    index: u64,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
struct HandleInformation {
    identity: FileIdentity,
    links: u32,
    attributes: u32,
}

#[cfg(windows)]
fn file_information(file: &File) -> Result<HandleInformation, String> {
    let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: `file` owns a valid Windows handle and `information` is writable storage for the
    // exact structure required by GetFileInformationByHandle.
    let status = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), information.as_mut_ptr()) };
    if status == 0 {
        return Err("proof-input file identity is unavailable".to_string());
    }
    // SAFETY: a successful Windows call initialized the complete output structure.
    let information = unsafe { information.assume_init() };
    let index = (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    Ok(HandleInformation {
        identity: FileIdentity { volume: information.volume_serial_number, index },
        links: information.number_of_links,
        attributes: information.file_attributes,
    })
}

#[cfg(windows)]
fn final_handle_path(file: &File) -> Result<PathBuf, String> {
    let handle = file.as_raw_handle().cast();
    // SAFETY: a null buffer with zero length asks Windows for the required UTF-16 buffer size.
    let required = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, 0) };
    if required == 0 || required > 32_768 {
        return Err("locked proof-input final path is unavailable or unbounded".to_string());
    }
    let mut buffer = vec![0_u16; usize::try_from(required).map_err(|_| "locked path length is invalid")? + 1];
    // SAFETY: `buffer` is writable for the capacity supplied and `file` owns a valid handle.
    let written = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).map_err(|_| "locked path length is invalid")?,
            0,
        )
    };
    if written == 0 || usize::try_from(written).map_err(|_| "locked path length is invalid")? >= buffer.len() {
        return Err("locked proof-input final path cannot be read".to_string());
    }
    buffer.truncate(usize::try_from(written).map_err(|_| "locked path length is invalid")?);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

#[cfg(windows)]
struct LockedDirectoryEntry {
    path: PathBuf,
    identity: FileIdentity,
    file: File,
}

#[cfg(windows)]
struct LockedDirectoryTree {
    path: PathBuf,
    entries: Vec<LockedDirectoryEntry>,
}

#[cfg(windows)]
impl LockedDirectoryTree {
    const SHARE_READ: u32 = 0x1;
    const SHARE_WRITE: u32 = 0x2;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    fn existing(path: &Path) -> Result<Self, String> {
        let absolute = absolute_lexical(path)?;
        let mut ancestors: Vec<PathBuf> = absolute.ancestors().map(Path::to_path_buf).collect();
        ancestors.reverse();
        if ancestors.is_empty() {
            return Err("proof-input directory has no lockable ancestry".to_string());
        }

        let mut entries = Vec::with_capacity(ancestors.len());
        for requested in ancestors {
            let metadata = fs::symlink_metadata(&requested)
                .map_err(|_| "proof-input directory ancestry does not exist".to_string())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata_is_reparse(&metadata) {
                return Err(
                    "proof-input directory ancestry contains a link, reparse point, or non-directory".to_string()
                );
            }
            let file = OpenOptions::new()
                .read(true)
                .share_mode(Self::SHARE_READ | Self::SHARE_WRITE)
                .custom_flags(Self::FILE_FLAG_BACKUP_SEMANTICS | Self::FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&requested)
                .map_err(|_| "proof-input directory ancestry cannot be identity-locked".to_string())?;
            let information = file_information(&file)?;
            if information.attributes & Self::FILE_ATTRIBUTE_DIRECTORY == 0
                || information.attributes & Self::FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err("locked proof-input ancestry is not an ordinary directory".to_string());
            }
            let final_path = final_handle_path(&file)?;
            if normalized_path(&final_path) != normalized_path(&requested) {
                return Err("proof-input directory resolved through an alias or changed while locking".to_string());
            }
            entries.push(LockedDirectoryEntry { path: final_path, identity: information.identity, file });
        }
        let path = entries.last().ok_or_else(|| "proof-input directory lock is empty".to_string())?.path.clone();
        let locked = Self { path, entries };
        locked.verify()?;
        Ok(locked)
    }

    fn verify(&self) -> Result<(), String> {
        for entry in &self.entries {
            let information = file_information(&entry.file)?;
            if information.identity != entry.identity
                || information.attributes & Self::FILE_ATTRIBUTE_DIRECTORY == 0
                || information.attributes & Self::FILE_ATTRIBUTE_REPARSE_POINT != 0
                || normalized_path(&final_handle_path(&entry.file)?) != normalized_path(&entry.path)
            {
                return Err("locked proof-input directory ancestry changed".to_string());
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
struct LockedDirectoryEntry {
    path: PathBuf,
    device: u64,
    inode: u64,
    file: File,
}

#[cfg(not(windows))]
struct LockedDirectoryTree {
    path: PathBuf,
    entries: Vec<LockedDirectoryEntry>,
}

#[cfg(not(windows))]
impl LockedDirectoryTree {
    fn existing(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;

        let absolute = absolute_lexical(path)?;
        let mut ancestors: Vec<PathBuf> = absolute.ancestors().map(Path::to_path_buf).collect();
        ancestors.reverse();
        let mut entries = Vec::with_capacity(ancestors.len());
        for requested in ancestors {
            let metadata = fs::symlink_metadata(&requested)
                .map_err(|_| "proof-input directory ancestry does not exist".to_string())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("proof-input directory ancestry contains a link or non-directory".to_string());
            }
            let file = File::open(&requested)
                .map_err(|_| "proof-input directory ancestry cannot be identity-locked".to_string())?;
            let locked_metadata =
                file.metadata().map_err(|_| "locked directory metadata is unavailable".to_string())?;
            if locked_metadata.dev() != metadata.dev() || locked_metadata.ino() != metadata.ino() {
                return Err("proof-input directory changed while locking".to_string());
            }
            let final_path = fs::canonicalize(&requested)
                .map_err(|_| "locked proof-input directory path is unavailable".to_string())?;
            entries.push(LockedDirectoryEntry {
                path: final_path,
                device: locked_metadata.dev(),
                inode: locked_metadata.ino(),
                file,
            });
        }
        let path = entries.last().ok_or_else(|| "proof-input directory lock is empty".to_string())?.path.clone();
        let locked = Self { path, entries };
        locked.verify()?;
        Ok(locked)
    }

    fn verify(&self) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;

        for entry in &self.entries {
            let metadata = entry.file.metadata().map_err(|_| "locked directory metadata is unavailable".to_string())?;
            if metadata.dev() != entry.device || metadata.ino() != entry.inode || !metadata.is_dir() {
                return Err("locked proof-input directory ancestry changed".to_string());
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
struct LockedPath {
    path: PathBuf,
    identity: FileIdentity,
    file: File,
    parent: LockedDirectoryTree,
}

#[cfg(windows)]
impl LockedPath {
    const SHARE_READ: u32 = 0x1;
    const SHARE_WRITE: u32 = 0x2;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    fn existing(path: &Path, writable: bool) -> Result<Self, String> {
        let absolute = absolute_lexical(path)?;
        let name = absolute.file_name().ok_or_else(|| "proof-input file has no filename".to_string())?;
        let parent_path = absolute.parent().ok_or_else(|| "proof-input file has no parent directory".to_string())?;
        let parent = LockedDirectoryTree::existing(parent_path)?;
        let locked_path = parent.path.join(name);
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(writable)
            .share_mode(Self::SHARE_READ | if writable { Self::SHARE_WRITE } else { 0 })
            .custom_flags(Self::FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(&locked_path).map_err(|_| "proof-input file cannot be identity-locked".to_string())?;
        let information = file_information(&file)?;
        if information.links != 1 {
            return Err("proof-input files must have exactly one hardlink".to_string());
        }
        if information.attributes & (Self::FILE_ATTRIBUTE_DIRECTORY | Self::FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            return Err("proof-input authority must be an ordinary file".to_string());
        }
        let final_path = final_handle_path(&file)?;
        if normalized_path(final_path.parent().ok_or_else(|| "locked proof-input file has no parent".to_string())?)
            != normalized_path(&parent.path)
            || normalized_path(&final_path) != normalized_path(&locked_path)
        {
            return Err("proof-input file escaped or changed its locked parent namespace".to_string());
        }
        let locked = Self { path: final_path, identity: information.identity, file, parent };
        locked.verify()?;
        Ok(locked)
    }

    fn create_new(path: &Path) -> Result<Self, String> {
        let absolute = absolute_lexical(path)?;
        let name = absolute.file_name().ok_or_else(|| "migration output has no filename".to_string())?;
        let parent_path = absolute.parent().ok_or_else(|| "migration output has no parent".to_string())?;
        let parent = LockedDirectoryTree::existing(parent_path)?;
        let locked_path = parent.path.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(0)
            .custom_flags(Self::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&locked_path)
            .map_err(|_| "migration output must be a new exclusive file".to_string())?;
        let information = file_information(&file)?;
        if information.links != 1 {
            return Err("migration output unexpectedly has multiple hardlinks".to_string());
        }
        if information.attributes & (Self::FILE_ATTRIBUTE_DIRECTORY | Self::FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            return Err("migration output is not an ordinary file".to_string());
        }
        let final_path = final_handle_path(&file)?;
        if normalized_path(final_path.parent().ok_or_else(|| "locked migration output has no parent".to_string())?)
            != normalized_path(&parent.path)
            || normalized_path(&final_path) != normalized_path(&locked_path)
        {
            return Err("migration output escaped or changed its locked parent namespace".to_string());
        }
        let locked = Self { path: final_path, identity: information.identity, file, parent };
        locked.verify()?;
        Ok(locked)
    }

    fn verify(&self) -> Result<(), String> {
        self.parent.verify()?;
        let information = file_information(&self.file)?;
        if information.identity != self.identity
            || information.links != 1
            || information.attributes & (Self::FILE_ATTRIBUTE_DIRECTORY | Self::FILE_ATTRIBUTE_REPARSE_POINT) != 0
            || normalized_path(&final_handle_path(&self.file)?) != normalized_path(&self.path)
        {
            return Err("locked proof-input identity or hardlink count changed".to_string());
        }
        Ok(())
    }

    fn sha256(&mut self) -> Result<String, String> {
        self.file.seek(SeekFrom::Start(0)).map_err(|_| "proof-input file cannot be rewound".to_string())?;
        sha256_reader(&mut self.file)
    }

    fn write_exact_and_sync(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.file.seek(SeekFrom::Start(0)).map_err(|_| "migration output cannot be rewound".to_string())?;
        self.file.set_len(0).map_err(|_| "migration output cannot be truncated".to_string())?;
        self.file.write_all(bytes).map_err(|_| "migration output cannot be written".to_string())?;
        self.file.flush().map_err(|_| "migration output cannot be flushed".to_string())?;
        self.file.sync_all().map_err(|_| "migration output cannot be durably synchronized".to_string())?;
        if self.file.metadata().map_err(|_| "migration output metadata is unavailable".to_string())?.len()
            != u64::try_from(bytes.len()).map_err(|_| "migration output size is invalid".to_string())?
        {
            return Err("migration output size differs from the serialized database".to_string());
        }
        self.verify()
    }

    fn parent_path(&self) -> &Path {
        &self.parent.path
    }
}

#[cfg(not(windows))]
struct LockedPath {
    path: PathBuf,
    _file: File,
    parent: LockedDirectoryTree,
}

#[cfg(not(windows))]
impl LockedPath {
    fn existing(path: &Path, writable: bool) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let parent_path = path.parent().ok_or_else(|| "proof-input file has no parent directory".to_string())?;
        let parent = LockedDirectoryTree::existing(parent_path)?;
        let name = path.file_name().ok_or_else(|| "proof-input file has no filename".to_string())?;
        let path = parent.path.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(&path)
            .map_err(|_| "proof-input file cannot be identity-locked".to_string())?;
        if file.metadata().map_err(|_| "locked file metadata is unavailable")?.nlink() != 1 {
            return Err("proof-input files must have exactly one hardlink".to_string());
        }
        Ok(Self { path, _file: file, parent })
    }

    fn create_new(path: &Path) -> Result<Self, String> {
        let parent_path = path.parent().ok_or_else(|| "migration output has no parent".to_string())?;
        let parent = LockedDirectoryTree::existing(parent_path)?;
        let name = path.file_name().ok_or_else(|| "migration output has no filename".to_string())?;
        let path = parent.path.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "migration output must be a new exclusive file".to_string())?;
        Ok(Self { path, _file: file, parent })
    }

    fn verify(&self) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;
        self.parent.verify()?;
        if fs::metadata(&self.path).map_err(|_| "locked proof-input path disappeared".to_string())?.nlink() != 1 {
            return Err("locked proof-input hardlink count changed".to_string());
        }
        Ok(())
    }

    fn sha256(&mut self) -> Result<String, String> {
        self._file.seek(SeekFrom::Start(0)).map_err(|_| "proof-input file cannot be rewound".to_string())?;
        sha256_reader(&mut self._file)
    }

    fn write_exact_and_sync(&mut self, bytes: &[u8]) -> Result<(), String> {
        self._file.seek(SeekFrom::Start(0)).map_err(|_| "migration output cannot be rewound".to_string())?;
        self._file.set_len(0).map_err(|_| "migration output cannot be truncated".to_string())?;
        self._file.write_all(bytes).map_err(|_| "migration output cannot be written".to_string())?;
        self._file.flush().map_err(|_| "migration output cannot be flushed".to_string())?;
        self._file.sync_all().map_err(|_| "migration output cannot be durably synchronized".to_string())?;
        if self._file.metadata().map_err(|_| "migration output metadata is unavailable".to_string())?.len()
            != u64::try_from(bytes.len()).map_err(|_| "migration output size is invalid".to_string())?
        {
            return Err("migration output size differs from the serialized database".to_string());
        }
        self.verify()
    }

    fn parent_path(&self) -> &Path {
        &self.parent.path
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn reject_sqlite_sidecars(path: &Path) -> Result<(), String> {
    for suffix in ["-wal", "-shm", "-journal"] {
        match fs::symlink_metadata(sidecar(path, suffix)) {
            Ok(_) => return Err(format!("database has a {suffix} sidecar; a single-file authority cannot be proven")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(format!("database {suffix} sidecar namespace cannot be inspected")),
        }
    }
    Ok(())
}

#[cfg(test)]
fn sha256_file(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|_| "proof-input file cannot be opened".to_string())?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    sha256_reader(&mut reader)
}

fn sha256_reader(reader: &mut impl Read) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|_| "proof-input file cannot be read".to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn serialize_database(connection: &Connection) -> Result<Vec<u8>, String> {
    let mut size = 0_i64;
    // SAFETY: `connection` remains alive and exclusively owned by this helper while SQLite reads its
    // main schema. The returned allocation is copied immediately and released with sqlite3_free.
    let pointer = unsafe { rusqlite::ffi::sqlite3_serialize(connection.handle(), c"main".as_ptr(), &mut size, 0) };
    if pointer.is_null() || size < 0 {
        return Err("migrated in-memory database cannot be serialized".to_string());
    }
    let length = usize::try_from(size).map_err(|_| "serialized database size is invalid".to_string())?;
    // SAFETY: SQLite returned `pointer` for exactly `length` bytes and keeps it valid until
    // sqlite3_free. `to_vec` creates the helper-owned copy before that release.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec();
    // SAFETY: `pointer` came from sqlite3_serialize without SQLITE_SERIALIZE_NOCOPY and is released
    // exactly once after the copy above.
    unsafe { rusqlite::ffi::sqlite3_free(pointer.cast()) };
    Ok(bytes)
}

fn helper_source_sha256() -> String {
    Sha256::digest(HELPER_SOURCE.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn normalized_schema_sql(name: &str, sql: Option<String>) -> String {
    if name.starts_with("segments_fts_") {
        return "<sqlite-fts5-shadow>".to_string();
    }
    sql.unwrap_or_default()
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn schema_fingerprint(connection: &Connection) -> Result<String, String> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .map_err(|error| format!("schema fingerprint query cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            let kind: String = row.get(0)?;
            let name: String = row.get(1)?;
            let table: String = row.get(2)?;
            let sql: Option<String> = row.get(3)?;
            Ok((kind, name.clone(), table, normalized_schema_sql(&name, sql)))
        })
        .map_err(|error| format!("schema fingerprint cannot be read: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("schema fingerprint row cannot be read: {error}"))?;
    let encoded =
        serde_json::to_vec(&rows).map_err(|error| format!("schema fingerprint cannot be encoded: {error}"))?;
    Ok(Sha256::digest(encoded).iter().map(|byte| format!("{byte:02x}")).collect())
}

fn expected_schema_fingerprint(version: i64) -> Result<String, String> {
    let current = migrations::max_supported_version();
    if version <= 0 || version > current {
        return Err(format!("schema {version} is outside this helper's exact history 1..={current}"));
    }
    let reference =
        Database::open(":memory:").map_err(|error| format!("reference schema cannot be opened: {error}"))?;
    reference.initialize().map_err(|error| format!("reference schema cannot be initialized: {error}"))?;
    if version < current {
        let count = usize::try_from(current - version)
            .map_err(|_| "reference schema rollback distance is invalid".to_string())?;
        migrations::rollback(&reference, count)
            .map_err(|error| format!("reference schema cannot be reconstructed at v{version}: {error}"))?;
    }
    let observed = migrations::validate_applied_history(reference.connection())
        .map_err(|error| format!("reference migration history is invalid: {error}"))?;
    if observed != version {
        return Err(format!("reference schema reconstruction stopped at v{observed}, expected v{version}"));
    }
    schema_fingerprint(reference.connection())
}

fn require_exact_schema_fingerprint(inspection: &Inspection) -> Result<(), String> {
    let expected = expected_schema_fingerprint(inspection.schema_version)?;
    if inspection.schema_fingerprint_sha256 != expected {
        return Err(format!(
            "database schema fingerprint differs from the exact release contract at v{}",
            inspection.schema_version
        ));
    }
    Ok(())
}

fn sqlite_check(connection: &Connection, pragma: &str) -> Result<Vec<String>, String> {
    let sql = match pragma {
        "quick_check" => "PRAGMA quick_check",
        "integrity_check" => "PRAGMA integrity_check",
        _ => return Err("unsupported SQLite integrity pragma".to_string()),
    };
    let mut statement = connection.prepare(sql).map_err(|error| format!("{pragma} cannot start: {error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("{pragma} cannot run: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{pragma} row cannot be read: {error}"))?;
    Ok(rows)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)", [table], |row| {
            row.get::<_, i64>(0)
        })
        .map(|value| value == 1)
        .map_err(|error| format!("campaign schema cannot be inspected: {error}"))
}

fn campaign_authority_counts(connection: &Connection) -> Result<BTreeMap<String, i64>, String> {
    let mut counts = BTreeMap::new();
    let settings: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE key IN (?1, ?2)",
            [CAMPAIGN_SETTING_KEYS[0], CAMPAIGN_SETTING_KEYS[1]],
            |row| row.get(0),
        )
        .map_err(|error| format!("campaign settings cannot be counted: {error}"))?;
    counts.insert("settings".to_string(), settings);
    for table in CAMPAIGN_TABLES {
        let count = if table_exists(connection, table)? {
            let sql = format!("SELECT COUNT(*) FROM {table}");
            connection
                .query_row(&sql, [], |row| row.get(0))
                .map_err(|error| format!("campaign authority table {table} cannot be counted: {error}"))?
        } else {
            0
        };
        counts.insert((*table).to_string(), count);
    }
    Ok(counts)
}

fn inspect_database(db: &Database) -> Result<Inspection, String> {
    let connection = db.connection();
    let schema_version = migrations::validate_applied_history(connection)
        .map_err(|error| format!("migration history is not an exact release prefix: {error}"))?;
    let migration_history_entries: i64 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row.get(0))
        .map_err(|error| format!("migration history count cannot be read: {error}"))?;
    let quick_check = sqlite_check(connection, "quick_check")?;
    let integrity_check = sqlite_check(connection, "integrity_check")?;
    let foreign_key_violations: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0))
        .map_err(|error| format!("foreign-key check cannot be read: {error}"))?;
    if quick_check.as_slice() != ["ok"] || integrity_check.as_slice() != ["ok"] || foreign_key_violations != 0 {
        return Err(format!(
            "database integrity proof failed: quick={quick_check:?}, full={integrity_check:?}, foreignKeys={foreign_key_violations}"
        ));
    }
    let segment_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM speech_segments", [], |row| row.get(0))
        .map_err(|error| format!("segment count cannot be read: {error}"))?;
    let distinct_audio_path_count: i64 = connection
        .query_row("SELECT COUNT(DISTINCT audio_path) FROM speech_segments WHERE trim(audio_path) <> ''", [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("audio-path count cannot be read: {error}"))?;
    let sequential_campaign_present = review_campaign::load(db)
        .map_err(|error| format!("sequential campaign authority is invalid: {error}"))?
        .is_some();
    let review_pool_present =
        review_pool::load(db).map_err(|error| format!("review-pool authority is invalid: {error}"))?.is_some();
    let campaign_authority_counts = campaign_authority_counts(connection)?;
    let campaign_authority_rows = campaign_authority_counts.values().sum();
    Ok(Inspection {
        schema: 1,
        schema_version,
        migration_history_entries,
        schema_fingerprint_sha256: schema_fingerprint(connection)?,
        quick_check,
        integrity_check,
        foreign_key_violations,
        segment_count,
        distinct_audio_path_count,
        sequential_campaign_present,
        review_pool_present,
        campaign_authority_rows,
        campaign_authority_counts,
    })
}

fn inspect_path(path: &Path) -> Result<(Inspection, String), String> {
    let canonical = reject_links_and_reparse_points(path)?;
    let mut locked = LockedPath::existing(&canonical, false)?;
    reject_live_and_snapshot_paths(&locked.path)?;
    reject_sqlite_sidecars(&locked.path)?;
    let before_hash = locked.sha256()?;
    let db = Database::open_detached_immutable_snapshot(&locked.path)
        .map_err(|error| format!("database cannot be detached for read-only inspection: {error}"))?;
    let inspection = inspect_database(&db)?;
    drop(db);
    locked.verify()?;
    let after_hash = locked.sha256()?;
    if before_hash != after_hash {
        return Err("database changed during read-only inspection".to_string());
    }
    reject_sqlite_sidecars(&locked.path)?;
    Ok((inspection, before_hash))
}

fn require_campaign_mode(inspection: &Inspection, mode: &str) -> Result<(), String> {
    match mode {
        "absent"
            if !inspection.sequential_campaign_present
                && !inspection.review_pool_present
                && inspection.campaign_authority_rows == 0 =>
        {
            Ok(())
        }
        "required" if inspection.sequential_campaign_present && inspection.campaign_authority_rows > 0 => Ok(()),
        "absent" => Err("database unexpectedly contains campaign or review-pool authority".to_string()),
        "required" => Err("database lacks valid sequential campaign authority".to_string()),
        _ => Err("--campaign must be absent or required".to_string()),
    }
}

fn migrate_path(
    source_path: &Path,
    output_path: &Path,
    staging_root: &Path,
    expected_source_hash: &str,
    expected_source_schema: i64,
    expected_target_schema: i64,
) -> Result<serde_json::Value, String> {
    if expected_source_hash.len() != 64
        || !expected_source_hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("--source-sha256 must be lowercase SHA-256".to_string());
    }
    if expected_target_schema != migrations::max_supported_version() {
        return Err(format!(
            "target schema {expected_target_schema} does not equal this helper's current schema {}",
            migrations::max_supported_version()
        ));
    }
    let checked_root = reject_links_and_reparse_points(staging_root)?;
    let root_lock = LockedDirectoryTree::existing(&checked_root)?;
    reject_live_and_snapshot_paths(&root_lock.path)?;
    if !root_lock.path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(STAGING_PREFIX)) {
        return Err("migration staging root is not owned by the proof-input preparer".to_string());
    }
    let authorities_lock = LockedDirectoryTree::existing(&root_lock.path.join("db-authorities"))?;
    let derived_lock = LockedDirectoryTree::existing(&root_lock.path.join("db-derived"))?;
    if normalized_path(
        authorities_lock.path.parent().ok_or_else(|| "authority directory has no locked parent".to_string())?,
    ) != normalized_path(&root_lock.path)
        || normalized_path(
            derived_lock.path.parent().ok_or_else(|| "derived directory has no locked parent".to_string())?,
        ) != normalized_path(&root_lock.path)
    {
        return Err("migration database directories escaped the locked staging root".to_string());
    }

    let checked_source = reject_links_and_reparse_points(source_path)?;
    reject_live_and_snapshot_paths(&checked_source)?;
    if normalized_path(checked_source.parent().ok_or_else(|| "migration source has no parent".to_string())?)
        != normalized_path(&authorities_lock.path)
    {
        return Err("migration source must be an immutable authority inside staging".to_string());
    }
    let source_name = checked_source.file_name().ok_or_else(|| "migration source has no filename".to_string())?;
    let mut source_lock = LockedPath::existing(&authorities_lock.path.join(source_name), false)?;
    if normalized_path(source_lock.parent_path()) != normalized_path(&authorities_lock.path) {
        return Err("migration source escaped the locked authority directory".to_string());
    }
    reject_live_and_snapshot_paths(&source_lock.path)?;

    let checked_output = absolute_lexical(output_path)?;
    reject_live_and_snapshot_paths(&checked_output)?;
    let checked_output_parent = checked_output.parent().ok_or_else(|| "migration output has no parent".to_string())?;
    let output_name = checked_output.file_name().ok_or_else(|| "migration output has no filename".to_string())?;
    if normalized_path(checked_output_parent) != normalized_path(&derived_lock.path) {
        return Err("migration output must be inside the staging db-derived directory".to_string());
    }
    let output = derived_lock.path.join(output_name);
    if !output_name.to_str().is_some_and(|name| name.ends_with(".work.db")) {
        return Err("migration output must use the disposable .work.db suffix".to_string());
    }
    match fs::symlink_metadata(&output) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Err("migration output must not preexist".to_string()),
        Err(_) => return Err("migration output namespace cannot be inspected".to_string()),
    }
    reject_sqlite_sidecars(&source_lock.path)?;
    reject_sqlite_sidecars(&output)?;
    let actual_source_hash = source_lock.sha256()?;
    if actual_source_hash != expected_source_hash {
        return Err("disposable migration source does not match its exact authority hash".to_string());
    }

    let db = Database::open_detached_immutable_snapshot(&source_lock.path)
        .map_err(|error| format!("migration source cannot be detached immutably: {error}"))?;
    let before = inspect_database(&db)?;
    if before.schema_version != expected_source_schema {
        return Err("disposable migration source schema/hash does not match its contract".to_string());
    }
    require_exact_schema_fingerprint(&before)?;
    require_campaign_mode(&before, "absent")?;

    db.initialize().map_err(|error| format!("real application migration path failed: {error}"))?;
    let after = inspect_database(&db)?;
    if after.schema_version != expected_target_schema {
        return Err(format!(
            "real application migration stopped at schema {}, expected {expected_target_schema}",
            after.schema_version
        ));
    }
    if after.segment_count != before.segment_count
        || after.distinct_audio_path_count != before.distinct_audio_path_count
    {
        return Err("migration changed scale segment or source-path counts".to_string());
    }
    require_campaign_mode(&after, "absent")?;
    let serialized = serialize_database(db.connection())?;
    let serialized_hash = sha256_bytes(&serialized);
    drop(db);
    source_lock.verify()?;
    let stable_source_hash = source_lock.sha256()?;
    if stable_source_hash != expected_source_hash {
        return Err("immutable migration source changed during the operation".to_string());
    }
    authorities_lock.verify()?;
    derived_lock.verify()?;
    root_lock.verify()?;
    let mut output_lock = LockedPath::create_new(&output)?;
    if normalized_path(output_lock.parent_path()) != normalized_path(&derived_lock.path) {
        return Err("migration output escaped the locked derived directory".to_string());
    }
    reject_live_and_snapshot_paths(&output_lock.path)?;
    output_lock.write_exact_and_sync(&serialized)?;
    reject_sqlite_sidecars(&output_lock.path)?;
    let result_hash = output_lock.sha256()?;
    if result_hash != serialized_hash {
        return Err("migration output bytes differ from the inspected in-memory database".to_string());
    }
    output_lock.verify()?;
    authorities_lock.verify()?;
    derived_lock.verify()?;
    root_lock.verify()?;
    let applied_migrations: Vec<i64> = ((expected_source_schema + 1)..=expected_target_schema).collect();
    Ok(serde_json::json!({
        "schema": 1,
        "operation": "migrate",
        "appGitSha": GIT_SHA,
        "helperSourceSha256": helper_source_sha256(),
        "sourceSha256": expected_source_hash,
        "resultSha256": result_hash,
        "appliedMigrations": applied_migrations,
        "before": before,
        "after": after,
    }))
}

fn run() -> Result<serde_json::Value, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    run_args(&args)
}

/// Exact former body of `run()` after the `env::args` read, extracted unchanged so the command
/// dispatch is testable without a process boundary. Behavior is identical.
fn run_args(args: &[String]) -> Result<serde_json::Value, String> {
    let command = args.first().map(String::as_str).ok_or_else(|| usage().to_string())?;
    match command {
        "inspect" => {
            let flags = parse_flags(&args[1..], &["--db", "--expected-schema", "--campaign"])?;
            let expected_schema = parse_schema(flag(&flags, "--expected-schema")?, "--expected-schema")?;
            let (inspection, database_hash) = inspect_path(Path::new(flag(&flags, "--db")?))?;
            if inspection.schema_version != expected_schema {
                return Err(format!("database schema is {}, expected {expected_schema}", inspection.schema_version));
            }
            require_exact_schema_fingerprint(&inspection)?;
            require_campaign_mode(&inspection, flag(&flags, "--campaign")?)?;
            Ok(serde_json::json!({
                "schema": 1,
                "operation": "inspect",
                "appGitSha": GIT_SHA,
                "helperSourceSha256": helper_source_sha256(),
                "databaseSha256": database_hash,
                "inspection": inspection,
            }))
        }
        "schema-contract" => {
            let flags = parse_flags(&args[1..], &["--expected-schema"])?;
            let expected_schema = parse_schema(flag(&flags, "--expected-schema")?, "--expected-schema")?;
            Ok(serde_json::json!({
                "schema": 1,
                "operation": "schema-contract",
                "appGitSha": GIT_SHA,
                "helperSourceSha256": helper_source_sha256(),
                "schemaVersion": expected_schema,
                "schemaFingerprintSha256": expected_schema_fingerprint(expected_schema)?,
            }))
        }
        "migrate" => {
            let flags = parse_flags(
                &args[1..],
                &[
                    "--source-db",
                    "--output-db",
                    "--staging-root",
                    "--source-sha256",
                    "--expected-source-schema",
                    "--expected-target-schema",
                ],
            )?;
            migrate_path(
                Path::new(flag(&flags, "--source-db")?),
                Path::new(flag(&flags, "--output-db")?),
                Path::new(flag(&flags, "--staging-root")?),
                flag(&flags, "--source-sha256")?,
                parse_schema(flag(&flags, "--expected-source-schema")?, "--expected-source-schema")?,
                parse_schema(flag(&flags, "--expected-target-schema")?, "--expected-target-schema")?,
            )
        }
        _ => Err(usage().to_string()),
    }
}

fn main() {
    match run() {
        Ok(value) => match serde_json::to_string(&value) {
            Ok(encoded) => println!("{encoded}"),
            Err(error) => {
                eprintln!("OWNER PROOF DB FAILED: output cannot be encoded: {error}");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("OWNER PROOF DB FAILED: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_verbatim_prefixes_have_the_same_comparison_identity() {
        assert_eq!(normalized_path(Path::new(r"\\?\C:\proof-root\owner")), "c:/proof-root/owner");
        assert_eq!(normalized_path(Path::new(r"\\?\UNC\server\share\Owner")), "//server/share/owner");
    }

    #[test]
    fn migrate_uses_real_schema_path_and_preserves_empty_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let staging = temporary.path().join(format!("{STAGING_PREFIX}fixture"));
        let authorities = staging.join("db-authorities");
        let derived = staging.join("db-derived");
        fs::create_dir_all(&authorities).unwrap();
        fs::create_dir_all(&derived).unwrap();
        let source = authorities.join("scale-schema60.db");
        let output = derived.join("scale-current.work.db");
        let db = Database::open(&source.to_string_lossy()).unwrap();
        db.initialize().unwrap();
        let target = migrations::max_supported_version();
        migrations::rollback(&db, usize::try_from(target - 60).unwrap()).unwrap();
        let journal: String = db.connection().query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0)).unwrap();
        assert_eq!(journal, "delete");
        drop(db);
        let source_hash = sha256_file(&source).unwrap();

        let result = migrate_path(&source, &output, &staging, &source_hash, 60, target).unwrap();
        assert_eq!(result["before"]["schemaVersion"], 60);
        assert_eq!(result["after"]["schemaVersion"], target);
        assert_eq!(result["after"]["campaignAuthorityRows"], 0);
        assert_eq!(sha256_file(&source).unwrap(), source_hash);
        assert!(output.exists());
        assert!(!sidecar(&output, "-wal").exists());
    }

    #[test]
    fn migrate_refuses_snapshot_and_non_staging_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let snapshots = temporary.path().join("snapshots");
        fs::create_dir_all(&snapshots).unwrap();
        let source = snapshots.join("copy.db");
        let output = snapshots.join("copy.work.db");
        fs::write(&source, b"not-a-database").unwrap();
        let error =
            migrate_path(&source, &output, &snapshots, &"0".repeat(64), 60, migrations::max_supported_version())
                .unwrap_err();
        assert!(error.contains("snapshot"));
    }

    #[test]
    fn migrate_refuses_a_hash_bound_but_schema_drifted_source() {
        let temporary = tempfile::tempdir().unwrap();
        let staging = temporary.path().join(format!("{STAGING_PREFIX}drift"));
        let authorities = staging.join("db-authorities");
        let derived = staging.join("db-derived");
        fs::create_dir_all(&authorities).unwrap();
        fs::create_dir_all(&derived).unwrap();
        let source = authorities.join("drift.db");
        let output = derived.join("drift.work.db");
        let db = Database::open(&source.to_string_lossy()).unwrap();
        db.initialize().unwrap();
        let target = migrations::max_supported_version();
        migrations::rollback(&db, usize::try_from(target - 60).unwrap()).unwrap();
        db.connection().execute("CREATE TABLE forged_authority(value TEXT)", []).unwrap();
        let journal: String = db.connection().query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0)).unwrap();
        assert_eq!(journal, "delete");
        drop(db);
        let source_hash = sha256_file(&source).unwrap();

        let error = migrate_path(&source, &output, &staging, &source_hash, 60, target).unwrap_err();
        assert!(error.contains("schema fingerprint differs"), "{error}");
    }

    #[test]
    fn migrate_refuses_hardlinked_source_and_preexisting_output() {
        let temporary = tempfile::tempdir().unwrap();
        let staging = temporary.path().join(format!("{STAGING_PREFIX}identity"));
        let authorities = staging.join("db-authorities");
        let derived = staging.join("db-derived");
        fs::create_dir_all(&authorities).unwrap();
        fs::create_dir_all(&derived).unwrap();
        let source = authorities.join("source.db");
        fs::write(&source, b"fixture").unwrap();
        fs::hard_link(&source, authorities.join("alias.db")).unwrap();
        let output = derived.join("result.work.db");
        let error = migrate_path(
            &source,
            &output,
            &staging,
            &sha256_file(&source).unwrap(),
            60,
            migrations::max_supported_version(),
        )
        .unwrap_err();
        assert!(error.contains("hardlink"), "{error}");

        fs::remove_file(authorities.join("alias.db")).unwrap();
        fs::write(&output, b"must-not-overwrite").unwrap();
        let error = migrate_path(
            &source,
            &output,
            &staging,
            &sha256_file(&source).unwrap(),
            60,
            migrations::max_supported_version(),
        )
        .unwrap_err();
        assert!(error.contains("must not preexist"), "{error}");
        assert_eq!(fs::read(&output).unwrap(), b"must-not-overwrite");
    }

    #[cfg(windows)]
    #[test]
    fn retained_windows_handles_deny_namespace_swaps_and_output_writers() {
        let temporary = tempfile::tempdir().unwrap();
        let staging = temporary.path().join(format!("{STAGING_PREFIX}locks"));
        let authorities = staging.join("db-authorities");
        let derived = staging.join("db-derived");
        fs::create_dir_all(&authorities).unwrap();
        fs::create_dir_all(&derived).unwrap();
        let source = authorities.join("source.db");
        fs::write(&source, b"source-authority").unwrap();

        let source_lock = LockedPath::existing(&source, false).unwrap();
        assert!(fs::rename(&authorities, staging.join("moved-authorities")).is_err());
        assert!(OpenOptions::new().write(true).open(&source).is_err());
        assert!(fs::remove_file(&source).is_err());
        source_lock.verify().unwrap();

        let output = derived.join("result.work.db");
        let mut output_lock = LockedPath::create_new(&output).unwrap();
        assert!(OpenOptions::new().read(true).open(&output).is_err());
        assert!(OpenOptions::new().write(true).open(&output).is_err());
        assert!(fs::rename(&derived, staging.join("moved-derived")).is_err());
        assert!(fs::remove_file(&output).is_err());
        output_lock.write_exact_and_sync(b"serialized-output").unwrap();
        assert_eq!(output_lock.sha256().unwrap(), sha256_bytes(b"serialized-output"));
    }

    // ── Flag and schema parsing ──────────────────────────────────────────────────────────────────

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn parse_flags_refuses_every_malformed_invocation_shape() {
        let allowed = ["--db", "--campaign"];
        assert_eq!(parse_flags(&args(&["--db"]), &allowed).unwrap_err(), usage().to_string());
        assert!(parse_flags(&args(&["--bogus", "x", "--db", "a", "--campaign", "b"]), &allowed)
            .unwrap_err()
            .contains("unknown or unauthorized option --bogus"));
        assert!(parse_flags(&args(&["--db", "", "--campaign", "b"]), &allowed)
            .unwrap_err()
            .contains("--db cannot be empty"));
        assert!(parse_flags(&args(&["--db", "a", "--db", "b", "--campaign", "c"]), &allowed)
            .unwrap_err()
            .contains("duplicate option --db"));
        assert!(parse_flags(&args(&["--db", "a"]), &allowed)
            .unwrap_err()
            .contains("missing required option --campaign"));

        let parsed = parse_flags(&args(&["--campaign", "absent", "--db", "a.db"]), &allowed).unwrap();
        assert_eq!(flag(&parsed, "--db").unwrap(), "a.db");
        assert_eq!(flag(&parsed, "--campaign").unwrap(), "absent");
        assert!(flag(&parsed, "--missing").unwrap_err().contains("missing required option --missing"));
    }

    #[test]
    fn parse_schema_accepts_only_positive_integers() {
        assert!(parse_schema("abc", "--expected-schema").unwrap_err().contains("positive integer"));
        assert!(parse_schema("0", "--expected-schema").unwrap_err().contains("positive integer"));
        assert!(parse_schema("-4", "--expected-schema").unwrap_err().contains("positive integer"));
        assert_eq!(parse_schema("61", "--expected-schema").unwrap(), 61);
    }

    // ── Path normalization and containment ───────────────────────────────────────────────────────

    #[test]
    fn absolute_lexical_refuses_empty_and_parent_traversal() {
        assert!(absolute_lexical(Path::new("")).unwrap_err().contains("path cannot be empty"));
        let relative = absolute_lexical(Path::new("proof-fixture.db")).unwrap();
        assert!(relative.is_absolute());
        assert!(relative.ends_with("proof-fixture.db"));
        let temporary = tempfile::tempdir().unwrap();
        let traversal = temporary.path().join("..").join("elsewhere.db");
        assert!(absolute_lexical(&traversal).unwrap_err().contains("parent traversal is not permitted"));
    }

    #[test]
    fn canonical_comparison_and_containment_handle_missing_tails() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let missing_child = root.join("not-yet").join("leaf.db");
        let comparable = canonical_comparison_path(&missing_child).unwrap();
        assert!(comparable.ends_with(Path::new("not-yet").join("leaf.db")));

        assert!(is_within(&missing_child, root).unwrap());
        assert!(is_within(root, root).unwrap(), "a root contains itself");
        let sibling = root.with_file_name("unrelated-sibling-dir");
        assert!(!is_within(&sibling, root).unwrap(), "a sibling sharing the prefix is not inside");
    }

    #[test]
    fn nt_object_prefixes_normalize_like_verbatim_prefixes() {
        assert_eq!(normalized_path(Path::new(r"\??\C:\Proof-Root\")), "c:/proof-root");
        assert_eq!(normalized_path(Path::new(r"C:\Proof-Root\Owner\")), "c:/proof-root/owner");
    }

    #[test]
    fn reject_links_requires_existing_plain_paths() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(reject_links_and_reparse_points(&temporary.path().join("missing").join("anywhere.db"))
            .unwrap_err()
            .contains("does not exist"));
        let file = temporary.path().join("plain.db");
        fs::write(&file, b"fixture").unwrap();
        let canonical = reject_links_and_reparse_points(&file).unwrap();
        assert_eq!(normalized_path(&canonical), normalized_path(&fs::canonicalize(&file).unwrap()));
    }

    #[test]
    fn snapshot_pinned_and_live_appdata_paths_are_refused() {
        let temporary = tempfile::tempdir().unwrap();
        for reserved in ["snapshots", "pinned", "snapshot_2026-01-01"] {
            let error = reject_live_and_snapshot_paths(&temporary.path().join(reserved).join("copy.db")).unwrap_err();
            assert!(error.contains("immutable recovery authority"), "{reserved}: {error}");
        }
        // Pure path comparison against the resolved protected roots; nothing is opened or written.
        for root in protected_roots().unwrap() {
            let error = reject_live_and_snapshot_paths(&root.join("cortex-speech.db")).unwrap_err();
            assert!(error.contains("never proof-input targets"), "{error}");
        }
        assert!(reject_live_and_snapshot_paths(&temporary.path().join("workspace.db")).is_ok());
    }

    // ── Sidecars and identity locks ──────────────────────────────────────────────────────────────

    #[test]
    fn sidecar_suffixes_are_appended_and_detected() {
        assert_eq!(sidecar(Path::new("a.db"), "-wal"), PathBuf::from("a.db-wal"));
        let temporary = tempfile::tempdir().unwrap();
        let db = temporary.path().join("single.db");
        fs::write(&db, b"fixture").unwrap();
        assert!(reject_sqlite_sidecars(&db).is_ok());
        fs::write(sidecar(&db, "-journal"), b"leftover").unwrap();
        let error = reject_sqlite_sidecars(&db).unwrap_err();
        assert!(error.contains("-journal sidecar"), "{error}");
    }

    #[test]
    fn identity_locks_refuse_directories_and_missing_files() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("actually-a-directory");
        fs::create_dir(&directory).unwrap();
        assert!(LockedPath::existing(&directory, false).is_err());
        assert!(LockedPath::existing(&temporary.path().join("missing.db"), false).is_err());
        let file = temporary.path().join("not-a-directory");
        fs::write(&file, b"fixture").unwrap();
        assert!(LockedDirectoryTree::existing(&file).is_err());
        assert!(LockedDirectoryTree::existing(temporary.path()).is_ok());
    }

    // ── Schema fingerprint and integrity helpers ─────────────────────────────────────────────────

    #[test]
    fn schema_sql_normalization_shadows_fts_and_flattens_whitespace() {
        assert_eq!(normalized_schema_sql("segments_fts_data", Some("CREATE ...".into())), "<sqlite-fts5-shadow>");
        assert_eq!(normalized_schema_sql("plain_table", None), "");
        assert_eq!(
            normalized_schema_sql("plain_table", Some("  CREATE TABLE  X\n  (id TEXT) ;".into())),
            "create table x (id text)"
        );
    }

    #[test]
    fn sqlite_check_supports_only_the_two_integrity_pragmas() {
        let db = Database::open(":memory:").unwrap();
        assert_eq!(sqlite_check(db.connection(), "quick_check").unwrap(), vec!["ok".to_string()]);
        assert_eq!(sqlite_check(db.connection(), "integrity_check").unwrap(), vec!["ok".to_string()]);
        assert!(sqlite_check(db.connection(), "journal_mode").unwrap_err().contains("unsupported SQLite integrity"));
    }

    #[test]
    fn expected_schema_fingerprint_is_bounded_and_deterministic() {
        let current = migrations::max_supported_version();
        assert!(expected_schema_fingerprint(0).unwrap_err().contains("outside this helper's exact history"));
        assert!(expected_schema_fingerprint(current + 1).unwrap_err().contains("outside this helper's exact history"));
        let fingerprint = expected_schema_fingerprint(current).unwrap();
        assert_eq!(fingerprint.len(), 64);
        assert_eq!(expected_schema_fingerprint(current).unwrap(), fingerprint);
    }

    // ── Campaign-mode contract ───────────────────────────────────────────────────────────────────

    fn inspection_fixture(campaign: bool, pool: bool, authority_rows: i64) -> Inspection {
        Inspection {
            schema: 1,
            schema_version: migrations::max_supported_version(),
            migration_history_entries: 0,
            schema_fingerprint_sha256: "0".repeat(64),
            quick_check: vec!["ok".into()],
            integrity_check: vec!["ok".into()],
            foreign_key_violations: 0,
            segment_count: 0,
            distinct_audio_path_count: 0,
            sequential_campaign_present: campaign,
            review_pool_present: pool,
            campaign_authority_rows: authority_rows,
            campaign_authority_counts: BTreeMap::new(),
        }
    }

    #[test]
    fn campaign_mode_contract_matches_only_exact_states() {
        assert!(require_campaign_mode(&inspection_fixture(false, false, 0), "absent").is_ok());
        assert!(require_campaign_mode(&inspection_fixture(true, false, 3), "required").is_ok());
        assert!(require_campaign_mode(&inspection_fixture(true, false, 3), "absent")
            .unwrap_err()
            .contains("unexpectedly contains campaign or review-pool authority"));
        assert!(require_campaign_mode(&inspection_fixture(false, true, 1), "absent")
            .unwrap_err()
            .contains("unexpectedly contains campaign or review-pool authority"));
        assert!(require_campaign_mode(&inspection_fixture(false, false, 0), "required")
            .unwrap_err()
            .contains("lacks valid sequential campaign authority"));
        assert!(require_campaign_mode(&inspection_fixture(true, false, 0), "required")
            .unwrap_err()
            .contains("lacks valid sequential campaign authority"));
        assert!(require_campaign_mode(&inspection_fixture(false, false, 0), "everything")
            .unwrap_err()
            .contains("--campaign must be absent or required"));
    }

    // ── inspect and the CLI dispatch ─────────────────────────────────────────────────────────────

    /// Current-schema single-file database fixture with no WAL/SHM sidecars.
    fn single_file_current_db(directory: &Path) -> PathBuf {
        let path = directory.join("inspect-fixture.db");
        let db = Database::open(&path.to_string_lossy()).unwrap();
        db.initialize().unwrap();
        let journal: String = db.connection().query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0)).unwrap();
        assert_eq!(journal, "delete");
        drop(db);
        path
    }

    #[test]
    fn inspect_path_proves_single_file_read_only_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let path = single_file_current_db(temporary.path());
        let before_hash = sha256_file(&path).unwrap();

        let (inspection, reported_hash) = inspect_path(&path).unwrap();
        assert_eq!(reported_hash, before_hash);
        assert_eq!(inspection.schema_version, migrations::max_supported_version());
        assert_eq!(inspection.segment_count, 0);
        assert_eq!(inspection.foreign_key_violations, 0);
        assert!(!inspection.sequential_campaign_present);
        assert_eq!(sha256_file(&path).unwrap(), before_hash, "inspection must be read-only");

        fs::write(sidecar(&path, "-wal"), b"stray").unwrap();
        assert!(inspect_path(&path).unwrap_err().contains("-wal sidecar"));
    }

    #[test]
    fn cli_dispatch_refuses_unknown_commands_and_serves_contracts() {
        assert_eq!(run_args(&[]).unwrap_err(), usage().to_string());
        assert_eq!(run_args(&args(&["sanitize-campaign"])).unwrap_err(), usage().to_string());

        let current = migrations::max_supported_version();
        let contract = run_args(&args(&["schema-contract", "--expected-schema", &current.to_string()])).unwrap();
        assert_eq!(contract["operation"], "schema-contract");
        assert_eq!(contract["schemaVersion"], current);
        assert_eq!(contract["schemaFingerprintSha256"], expected_schema_fingerprint(current).unwrap());
        assert_eq!(contract["helperSourceSha256"], helper_source_sha256());

        // The migrate arm routes through the same hash-bound refusals as the direct calls below.
        let refused = run_args(&args(&[
            "migrate",
            "--source-db",
            "unused.db",
            "--output-db",
            "unused.work.db",
            "--staging-root",
            "unused-root",
            "--source-sha256",
            "NOT-A-HASH",
            "--expected-source-schema",
            "60",
            "--expected-target-schema",
            &current.to_string(),
        ]))
        .unwrap_err();
        assert!(refused.contains("--source-sha256 must be lowercase SHA-256"), "{refused}");
    }

    #[test]
    fn cli_inspect_binds_schema_campaign_and_hash() {
        let temporary = tempfile::tempdir().unwrap();
        let path = single_file_current_db(temporary.path());
        let path_text = path.to_string_lossy().to_string();
        let current = migrations::max_supported_version();

        let report = run_args(&args(&[
            "inspect",
            "--db",
            &path_text,
            "--expected-schema",
            &current.to_string(),
            "--campaign",
            "absent",
        ]))
        .unwrap();
        assert_eq!(report["operation"], "inspect");
        assert_eq!(report["databaseSha256"], sha256_file(&path).unwrap());
        assert_eq!(report["inspection"]["schemaVersion"], current);

        let wrong_schema =
            run_args(&args(&["inspect", "--db", &path_text, "--expected-schema", "1", "--campaign", "absent"]))
                .unwrap_err();
        assert!(wrong_schema.contains("database schema is"), "{wrong_schema}");
        let bad_mode = run_args(&args(&[
            "inspect",
            "--db",
            &path_text,
            "--expected-schema",
            &current.to_string(),
            "--campaign",
            "everything",
        ]))
        .unwrap_err();
        assert!(bad_mode.contains("--campaign must be absent or required"), "{bad_mode}");
    }

    // ── migrate refusal arms ─────────────────────────────────────────────────────────────────────

    fn staged_layout(root: &Path, name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let staging = root.join(format!("{STAGING_PREFIX}{name}"));
        let authorities = staging.join("db-authorities");
        let derived = staging.join("db-derived");
        fs::create_dir_all(&authorities).unwrap();
        fs::create_dir_all(&derived).unwrap();
        (staging, authorities, derived)
    }

    #[test]
    fn migrate_refuses_malformed_hash_and_foreign_target_schema() {
        let temporary = tempfile::tempdir().unwrap();
        let (staging, authorities, derived) = staged_layout(temporary.path(), "contract");
        let source = authorities.join("source.db");
        let output = derived.join("result.work.db");
        fs::write(&source, b"fixture").unwrap();
        let current = migrations::max_supported_version();

        let uppercase = migrate_path(&source, &output, &staging, &"A".repeat(64), 60, current).unwrap_err();
        assert!(uppercase.contains("--source-sha256 must be lowercase SHA-256"), "{uppercase}");
        let short = migrate_path(&source, &output, &staging, "abc123", 60, current).unwrap_err();
        assert!(short.contains("--source-sha256 must be lowercase SHA-256"), "{short}");
        let foreign = migrate_path(&source, &output, &staging, &"0".repeat(64), 60, current + 1).unwrap_err();
        assert!(foreign.contains("does not equal this helper's current schema"), "{foreign}");
    }

    #[test]
    fn migrate_refuses_unowned_or_incomplete_staging_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let unowned = temporary.path().join("plain-workspace");
        fs::create_dir_all(unowned.join("db-authorities")).unwrap();
        fs::create_dir_all(unowned.join("db-derived")).unwrap();
        let error = migrate_path(
            &unowned.join("db-authorities").join("a.db"),
            &unowned.join("db-derived").join("a.work.db"),
            &unowned,
            &"0".repeat(64),
            60,
            migrations::max_supported_version(),
        )
        .unwrap_err();
        assert!(error.contains("not owned by the proof-input preparer"), "{error}");

        let bare = temporary.path().join(format!("{STAGING_PREFIX}bare"));
        fs::create_dir_all(&bare).unwrap();
        let error = migrate_path(
            &bare.join("db-authorities").join("a.db"),
            &bare.join("db-derived").join("a.work.db"),
            &bare,
            &"0".repeat(64),
            60,
            migrations::max_supported_version(),
        )
        .unwrap_err();
        assert!(error.contains("ancestry does not exist"), "{error}");
    }

    #[test]
    fn migrate_refuses_sources_and_outputs_outside_their_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let (staging, authorities, derived) = staged_layout(temporary.path(), "placement");
        let stray_source = staging.join("stray.db");
        fs::write(&stray_source, b"fixture").unwrap();
        let current = migrations::max_supported_version();

        let outside_authorities =
            migrate_path(&stray_source, &derived.join("r.work.db"), &staging, &"0".repeat(64), 60, current)
                .unwrap_err();
        assert!(outside_authorities.contains("immutable authority inside staging"), "{outside_authorities}");

        let source = authorities.join("source.db");
        fs::write(&source, b"fixture").unwrap();
        let outside_derived =
            migrate_path(&source, &staging.join("r.work.db"), &staging, &"0".repeat(64), 60, current).unwrap_err();
        assert!(outside_derived.contains("inside the staging db-derived directory"), "{outside_derived}");

        let wrong_suffix =
            migrate_path(&source, &derived.join("result.db"), &staging, &"0".repeat(64), 60, current).unwrap_err();
        assert!(wrong_suffix.contains("disposable .work.db suffix"), "{wrong_suffix}");
    }

    #[test]
    fn migrate_refuses_hash_and_source_schema_drift() {
        let temporary = tempfile::tempdir().unwrap();
        let (staging, authorities, derived) = staged_layout(temporary.path(), "authority");
        let source = authorities.join("source.db");
        let output = derived.join("result.work.db");
        fs::write(&source, b"not-the-promised-bytes").unwrap();
        let current = migrations::max_supported_version();

        let mismatch = migrate_path(&source, &output, &staging, &"0".repeat(64), 60, current).unwrap_err();
        assert!(mismatch.contains("does not match its exact authority hash"), "{mismatch}");
        assert!(!output.exists(), "a refused migration must not create output");

        // A real current-schema database offered under a WRONG claimed source schema.
        fs::remove_file(&source).unwrap();
        let db = Database::open(&source.to_string_lossy()).unwrap();
        db.initialize().unwrap();
        let journal: String = db.connection().query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0)).unwrap();
        assert_eq!(journal, "delete");
        drop(db);
        let real_hash = sha256_file(&source).unwrap();
        let drift = migrate_path(&source, &output, &staging, &real_hash, current - 1, current).unwrap_err();
        assert!(drift.contains("schema/hash does not match its contract"), "{drift}");
        assert!(!output.exists());
    }
}
