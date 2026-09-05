//! Recoverable publication of HF's four managed entries inside a possibly mixed user directory.
//! Readers must verify SHA256SUMS: several directory entries cannot be atomically exchanged by
//! one portable filesystem operation. A durable intent precedes every move; incomplete publication
//! rolls back on the next export. Unrelated destination files are never moved or deleted.

use crate::atomic_file::{fsync_directory_strict, fsync_parent_dir_strict, rename_no_replace_write_through};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MANAGED: [&str; 4] = ["data", "README.md", "dataset_infos.json", "SHA256SUMS"];
const PREFIX: &str = ".cortex-hf-publication-";
const LOCK: &str = ".cortex-hf-export.lock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum EntryDigest {
    Directory,
    File { bytes: u64, sha256: String },
}

type Artifact = BTreeMap<String, EntryDigest>;
type Inventory = BTreeMap<String, Option<Artifact>>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    schema: u32,
    root: PathBuf,
    nonce: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    previous: Inventory,
    next: Inventory,
}

fn refusal(message: impl Into<String>) -> AppError {
    AppError::Validation(format!("HF publication: {}", message.into()))
}

fn collect(path: &Path, relative: &Path, out: &mut Artifact, sync: bool) -> AppResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(refusal(format!("symlink artifacts are not replaceable: {}", path.display())));
    }
    let key = relative.to_str().ok_or_else(|| refusal("artifact paths must be UTF-8"))?.replace('\\', "/");
    if metadata.is_dir() {
        out.insert(key, EntryDigest::Directory);
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            collect(&entry.path(), &relative.join(entry.file_name()), out, sync)?;
        }
        if sync {
            fsync_directory_strict(path)?;
        }
    } else if metadata.is_file() {
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            bytes += count as u64;
        }
        if bytes != metadata.len() {
            return Err(refusal("artifact changed while its publication identity was captured"));
        }
        if sync {
            OpenOptions::new().write(true).open(path)?.sync_all()?;
        }
        let sha256 = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
        out.insert(key, EntryDigest::File { bytes, sha256 });
    } else {
        return Err(refusal("only regular files and directories can be published"));
    }
    Ok(())
}

fn artifact(path: &Path, sync: bool) -> AppResult<Option<Artifact>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let mut digest = BTreeMap::new();
    collect(path, Path::new(""), &mut digest, sync)?;
    Ok(Some(digest))
}

fn inventory(root: &Path, sync: bool) -> AppResult<Inventory> {
    MANAGED.iter().map(|name| Ok(((*name).to_string(), artifact(&root.join(name), sync)?))).collect()
}

fn validate_inventory(value: &Inventory, complete: bool) -> AppResult<()> {
    if value.len() != MANAGED.len() || MANAGED.iter().any(|name| !value.contains_key(*name)) {
        return Err(refusal("journal contains an invalid managed-artifact inventory"));
    }
    for name in MANAGED {
        let Some(entries) = &value[name] else {
            if complete {
                return Err(refusal(format!("staged generation is missing {name}")));
            }
            continue;
        };
        match (name, entries.get("")) {
            ("data", Some(EntryDigest::Directory)) => {}
            (_, Some(EntryDigest::File { .. })) if name != "data" && entries.len() == 1 => {}
            _ => return Err(refusal(format!("managed artifact has the wrong type: {name}"))),
        }
    }
    Ok(())
}

fn verify_checksums(stage: &Path, digest: &Inventory) -> AppResult<()> {
    let mut expected = BTreeMap::new();
    for name in MANAGED.into_iter().filter(|name| *name != "SHA256SUMS") {
        for (relative, entry) in digest[name].as_ref().ok_or_else(|| refusal("incomplete staged inventory"))? {
            if let EntryDigest::File { sha256, .. } = entry {
                let path = if relative.is_empty() { name.to_string() } else { format!("{name}/{relative}") };
                expected.insert(path, sha256.clone());
            }
        }
    }
    let mut declared = BTreeMap::new();
    for line in fs::read_to_string(stage.join("SHA256SUMS"))?.lines() {
        let (hash, path) = line.split_once("  ").ok_or_else(|| refusal("malformed staged SHA256SUMS"))?;
        if declared.insert(path.to_string(), hash.to_string()).is_some() {
            return Err(refusal("duplicate staged checksum entry"));
        }
    }
    if declared != expected {
        return Err(refusal("staged SHA256SUMS does not exactly describe the managed generation"));
    }
    Ok(())
}

fn write_durable(path: &Path, bytes: &[u8]) -> AppResult<()> {
    // Publish the control record by rename only after all of its bytes are durable. A process
    // dying mid-write must leave a private fragment, never a truncated authoritative journal.
    let pending = path.with_extension(format!("pending-{}", uuid::Uuid::new_v4().simple()));
    let mut file = OpenOptions::new().create_new(true).write(true).open(&pending)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    move_entry(&pending, path)
}

fn move_entry(source: &Path, destination: &Path) -> AppResult<()> {
    move_entry_with_hook(source, destination, || Ok(()))
}

fn move_entry_with_hook(
    source: &Path,
    destination: &Path,
    before_rename: impl FnOnce() -> AppResult<()>,
) -> AppResult<()> {
    // The precheck gives an actionable error; only the no-replace OS operation closes the
    // race with an external creator. The production caller supplies a no-op hook.
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(_) => return Err(refusal(format!("refusing to overwrite an unexpected entry: {}", destination.display()))),
    }
    before_rename()?;
    rename_no_replace_write_through(source, destination)?;
    fsync_parent_dir_strict(source)?;
    fsync_parent_dir_strict(destination)?;
    Ok(())
}

fn lock_destination(root: &Path) -> AppResult<File> {
    let path = root.join(LOCK);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(refusal("destination lock must be a regular non-symlink file"));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options
        .open(path)
        .map_err(|error| refusal(format!("cannot lock destination; another HF export may be active: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: file owns this valid descriptor throughout the lock lifetime. Closing it releases
        // the lock after normal completion OR process death. Never unlink the shared lock inode.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(refusal("another HF export is active for this destination"));
        }
    }
    Ok(file)
}

fn owned_stage(root: &Path, path: &Path) -> AppResult<bool> {
    let Some(nonce) = path.file_name().and_then(|name| name.to_str()).and_then(|name| name.strip_prefix(PREFIX)) else {
        return Ok(false);
    };
    if uuid::Uuid::parse_str(nonce).is_err() || path.parent() != Some(root) {
        return Ok(false);
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || path.canonicalize()?.parent() != Some(root) {
        return Err(refusal("unsafe private recovery directory"));
    }
    let owner_path = path.join("owner.json");
    match fs::symlink_metadata(&owner_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(refusal("unsafe recovery owner marker"))
        }
        Ok(_) => {}
    }
    let owner: Owner = serde_json::from_slice(&fs::read(owner_path)?)?;
    Ok(owner.schema == 1 && owner.root == root && owner.nonce == nonce)
}

fn cleanup(root: &Path, stage: &Path) -> AppResult<()> {
    if !owned_stage(root, stage)? {
        return Err(refusal("refusing cleanup of an unowned recovery directory"));
    }
    // Check every descendant before a recursive cleanup; never traverse a substituted link.
    let _ = artifact(stage, false)?;
    fs::remove_dir_all(stage)?;
    fsync_directory_strict(root)?;
    Ok(())
}

fn rollback(root: &Path, stage: &Path, journal: &Journal) -> AppResult<()> {
    let backup = stage.join("previous");
    let current = inventory(root, false)?;
    let preserved = inventory(&backup, false)?;
    // Validate ALL entries before changing ANY entry. Unexpected external edits are retained,
    // alongside the previous generation, for explicit recovery instead of being overwritten.
    for name in MANAGED {
        match (&journal.previous[name], &preserved[name]) {
            (Some(previous), Some(saved)) if previous == saved => {
                if current[name].is_some() && current[name] != journal.next[name] {
                    return Err(refusal(format!(
                        "{name} changed outside publication; preserved backup needs explicit recovery"
                    )));
                }
            }
            (Some(previous), None) if current[name].as_ref() == Some(previous) => {}
            (None, None) if current[name].is_none() || current[name] == journal.next[name] => {}
            _ => return Err(refusal(format!("previous {name} identity cannot be safely restored"))),
        }
    }
    let committed = stage.join("COMMITTED");
    if committed.exists() {
        fs::remove_file(&committed)?;
        fsync_directory_strict(stage)?;
    }
    let discarded = stage.join(format!("discard-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir(&discarded)?;
    for name in MANAGED {
        if preserved[name].is_some() || journal.previous[name].is_none() {
            if current[name].is_some() {
                move_entry(&root.join(name), &discarded.join(name))?;
            }
            if preserved[name].is_some() {
                move_entry(&backup.join(name), &root.join(name))?;
            }
        }
    }
    if inventory(root, false)? != journal.previous {
        return Err(refusal("restored generation differs from its captured identity"));
    }
    Ok(())
}

fn recover(root: &Path) -> AppResult<()> {
    for entry in fs::read_dir(root)? {
        let stage = entry?.path();
        if !owned_stage(root, &stage)? {
            continue;
        }
        let journal_path = stage.join("journal.json");
        match fs::symlink_metadata(&journal_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                cleanup(root, &stage)?; // Staging crashed before permission to move old files existed.
                continue;
            }
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(refusal("unsafe recovery journal"))
            }
            Ok(_) => {}
        }
        let journal: Journal = serde_json::from_slice(&fs::read(journal_path)?)?;
        if journal.schema != 1 {
            return Err(refusal("unsupported recovery journal schema"));
        }
        validate_inventory(&journal.previous, false)?;
        validate_inventory(&journal.next, true)?;
        let committed = stage.join("COMMITTED");
        match fs::symlink_metadata(&committed) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => rollback(root, &stage, &journal)?,
            Err(error) => return Err(error.into()),
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || fs::read(&committed)? != b"HF publication complete\n"
                    || inventory(root, false)? != journal.next
                {
                    return Err(refusal("committed generation changed; recovery artifacts were preserved"));
                }
            }
        }
        cleanup(root, &stage)?;
    }
    Ok(())
}

pub(super) struct Publication {
    root: PathBuf,
    stage: PathBuf,
    predecessor: Inventory,
    preserve: bool,
    // This handle deliberately outlives cleanup. The persistent inode is NEVER unlinked.
    _lock: File,
}

impl Publication {
    pub(super) fn begin(root: &Path) -> AppResult<Self> {
        fs::create_dir_all(root)?;
        let metadata = fs::symlink_metadata(root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(refusal("destination must be a regular non-symlink directory"));
        }
        let root = root.canonicalize()?;
        let lock = lock_destination(&root)?;
        recover(&root)?;
        let predecessor = inventory(&root, false)?;
        validate_inventory(&predecessor, false)?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let stage = root.join(format!("{PREFIX}{nonce}"));
        fs::create_dir(&stage)?;
        let owner = Owner { schema: 1, root: root.clone(), nonce };
        write_durable(&stage.join("owner.json"), &serde_json::to_vec(&owner)?)?;
        let result = Self { root, stage, predecessor, preserve: false, _lock: lock };
        fs::create_dir(result.staging())?;
        fs::create_dir(result.stage.join("previous"))?;
        fsync_directory_strict(&result.stage)?;
        fsync_directory_strict(&result.root)?;
        Ok(result)
    }

    pub(super) fn staging(&self) -> PathBuf {
        self.stage.join("next")
    }

    pub(super) fn publish(
        mut self,
        mut hook: impl FnMut(super::HuggingFacePublishPoint) -> AppResult<()>,
        finish: impl FnOnce() -> AppResult<()>,
    ) -> AppResult<()> {
        let next = inventory(&self.staging(), true)?;
        validate_inventory(&next, true)?;
        verify_checksums(&self.staging(), &next)?;
        if inventory(&self.root, false)? != self.predecessor {
            return Err(refusal("destination changed while the new generation was staged"));
        }
        let journal = Journal { schema: 1, previous: self.predecessor.clone(), next };
        write_durable(&self.stage.join("journal.json"), &serde_json::to_vec(&journal)?)?;
        self.preserve = true; // Every subsequent failure must retain recovery authority until rollback succeeds.
        let result = (|| -> AppResult<()> {
            for name in MANAGED {
                if journal.previous[name].is_some() {
                    move_entry(&self.root.join(name), &self.stage.join("previous").join(name))?;
                }
            }
            hook(super::HuggingFacePublishPoint::BeforeDataPromotion)?;
            if inventory(&self.staging(), false)? != journal.next {
                return Err(refusal("staged generation changed before publication"));
            }
            for name in MANAGED {
                move_entry(&self.staging().join(name), &self.root.join(name))?;
                match name {
                    "data" => hook(super::HuggingFacePublishPoint::AfterDataPromotion)?,
                    "README.md" | "dataset_infos.json" => hook(super::HuggingFacePublishPoint::AfterMetadataPromotion)?,
                    _ => {}
                }
            }
            hook(super::HuggingFacePublishPoint::BeforePublicationCommit)?;
            if inventory(&self.root, true)? != journal.next {
                return Err(refusal("published generation failed its final identity check"));
            }
            write_durable(&self.stage.join("COMMITTED"), b"HF publication complete\n")?;
            hook(super::HuggingFacePublishPoint::AfterFilesCommitted)?;
            // A failure while committing the caller's split transaction still restores all files.
            // Across process death, DB split hints can lag the independently sealed file generation;
            // training consumers must use its manifest, not treat these hints as publication authority.
            finish()?;
            Ok(())
        })();
        if let Err(error) = result {
            if let Err(restore) = rollback(&self.root, &self.stage, &journal) {
                return Err(AppError::Other(format!(
                    "{error}; automatic HF recovery failed ({restore}); previous artifacts remain at {}",
                    self.stage.display()
                )));
            }
            self.preserve = false;
            return Err(error);
        }
        self.preserve = false;
        Ok(())
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        if !self.preserve {
            if let Err(error) = cleanup(&self.root, &self.stage) {
                tracing::warn!(path = %self.stage.display(), %error, "HF private publication cleanup deferred");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::HuggingFacePublishPoint;

    #[test]
    fn racing_file_after_move_precheck_preserves_both_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        let destination = root.path().join("destination.txt");
        fs::write(&source, "new generation").unwrap();
        let result = move_entry_with_hook(&source, &destination, || {
            let mut external = OpenOptions::new().create_new(true).write(true).open(&destination)?;
            external.write_all(b"foreign bytes")?;
            Ok(())
        });
        assert!(result.is_err(), "a racing file must never be overwritten");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "foreign bytes");
        assert_eq!(fs::read_to_string(&source).unwrap(), "new generation");
    }

    #[test]
    fn racing_empty_directory_after_move_precheck_preserves_both_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("clip.wav"), "new generation").unwrap();
        let result = move_entry_with_hook(&source, &destination, || {
            fs::create_dir(&destination)?;
            Ok(())
        });
        assert!(result.is_err(), "even an empty racing directory belongs to its creator");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(source.join("clip.wav")).unwrap(), "new generation");
    }

    // These are byte-generation fixtures. Real decoding and exporter wiring are exercised by
    // export_tests.rs; the publication layer must preserve bytes without interpreting the audio.
    fn generation(root: &Path, text: &str) {
        fs::create_dir_all(root.join("data/train")).unwrap();
        fs::write(root.join("data/train/clip.wav"), text.as_bytes()).unwrap();
        fs::write(root.join("README.md"), text).unwrap();
        fs::write(root.join("dataset_infos.json"), "{}").unwrap();
        crate::export::write_sha256sums(root).unwrap();
    }

    #[test]
    fn staged_substitution_is_refused_and_the_previous_generation_is_restored() {
        let output = tempfile::tempdir().unwrap();
        generation(output.path(), "previous");
        fs::write(output.path().join("owner-note.txt"), "unrelated").unwrap();
        let before = inventory(output.path(), false).unwrap();
        let publication = Publication::begin(output.path()).unwrap();
        let staged = publication.staging();
        generation(&staged, "new");
        let error = publication
            .publish(
                |point| {
                    if point == HuggingFacePublishPoint::BeforeDataPromotion {
                        fs::write(staged.join("data/train/clip.wav"), "substituted").unwrap();
                    }
                    Ok(())
                },
                || Ok(()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("staged generation changed"), "{error}");
        assert_eq!(inventory(output.path(), false).unwrap(), before);
        assert_eq!(fs::read_to_string(output.path().join("owner-note.txt")).unwrap(), "unrelated");
    }

    #[test]
    fn external_destination_edits_during_staging_are_not_overwritten() {
        let output = tempfile::tempdir().unwrap();
        generation(output.path(), "previous");
        let publication = Publication::begin(output.path()).unwrap();
        generation(&publication.staging(), "new");
        fs::write(output.path().join("README.md"), "operator edit").unwrap();
        let edited = inventory(output.path(), false).unwrap();
        let error = publication.publish(|_| Ok(()), || Ok(())).unwrap_err();
        assert!(error.to_string().contains("destination changed"), "{error}");
        assert_eq!(inventory(output.path(), false).unwrap(), edited);
    }

    #[test]
    fn first_export_failure_preserves_unrelated_files_without_publishing_a_partial_dataset() {
        let output = tempfile::tempdir().unwrap();
        fs::write(output.path().join("owner-note.txt"), "unrelated").unwrap();
        let before = inventory(output.path(), false).unwrap();
        let publication = Publication::begin(output.path()).unwrap();
        generation(&publication.staging(), "new");
        assert!(publication
            .publish(
                |point| {
                    if point == HuggingFacePublishPoint::AfterDataPromotion {
                        return Err(refusal("injected first-export failure"));
                    }
                    Ok(())
                },
                || Ok(())
            )
            .is_err());
        assert_eq!(inventory(output.path(), false).unwrap(), before);
        assert_eq!(fs::read_to_string(output.path().join("owner-note.txt")).unwrap(), "unrelated");
        drop(Publication::begin(output.path()).unwrap());
    }

    #[test]
    fn unexpected_target_change_preserves_both_the_foreign_bytes_and_the_prior_backup() {
        let output = tempfile::tempdir().unwrap();
        generation(output.path(), "previous");
        let before = inventory(output.path(), false).unwrap();
        let publication = Publication::begin(output.path()).unwrap();
        let stage = publication.stage.clone();
        generation(&publication.staging(), "new");
        let error = publication
            .publish(
                |point| {
                    if point == HuggingFacePublishPoint::AfterDataPromotion {
                        fs::write(output.path().join("data/train/clip.wav"), "foreign bytes").unwrap();
                        return Err(refusal("injected failure after external modification"));
                    }
                    Ok(())
                },
                || Ok(()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("automatic HF recovery failed"), "{error}");
        assert_eq!(fs::read_to_string(output.path().join("data/train/clip.wav")).unwrap(), "foreign bytes");
        assert_eq!(inventory(&stage.join("previous"), false).unwrap(), before);
        assert!(Publication::begin(output.path()).is_err(), "automatic recovery must not overwrite external edits");
        assert_eq!(inventory(&stage.join("previous"), false).unwrap(), before);
        // The fixture owner explicitly resolves its external edit. Recovery is then retryable.
        fs::write(output.path().join("data/train/clip.wav"), "new").unwrap();
        drop(Publication::begin(output.path()).unwrap());
        assert_eq!(inventory(output.path(), false).unwrap(), before);
    }

    fn interrupt_before_commit(output: &Path) -> PathBuf {
        let publication = Publication::begin(output).unwrap();
        let stage = publication.stage.clone();
        generation(&publication.staging(), "new");
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            publication
                .publish(
                    |point| {
                        assert_ne!(
                            point,
                            HuggingFacePublishPoint::AfterMetadataPromotion,
                            "injected interrupted publication"
                        );
                        Ok(())
                    },
                    || Ok(()),
                )
                .unwrap();
        }));
        assert!(stopped.is_err());
        assert!(stage.join("journal.json").is_file());
        stage
    }

    #[test]
    fn partial_rollback_can_resume_without_losing_an_already_restored_artifact() {
        let output = tempfile::tempdir().unwrap();
        generation(output.path(), "previous");
        let before = inventory(output.path(), false).unwrap();
        let stage = interrupt_before_commit(output.path());
        // Exact interrupted-rollback shape: one new artifact moved aside, one backup restored,
        // and the remaining metadata still waiting. Process-exit tests cover the publication cuts.
        move_entry(&output.path().join("data"), &stage.join("discarded-data")).unwrap();
        move_entry(&stage.join("previous/data"), &output.path().join("data")).unwrap();
        drop(Publication::begin(output.path()).unwrap());
        assert_eq!(inventory(output.path(), false).unwrap(), before);
    }

    #[test]
    fn corrupted_recovery_journal_is_refused_without_changing_any_artifact() {
        let output = tempfile::tempdir().unwrap();
        generation(output.path(), "previous");
        let stage = interrupt_before_commit(output.path());
        fs::write(stage.join("journal.json"), "broken journal").unwrap();
        let visible = inventory(output.path(), false).unwrap();
        let saved = inventory(&stage.join("previous"), false).unwrap();
        assert!(Publication::begin(output.path()).is_err());
        assert_eq!(inventory(output.path(), false).unwrap(), visible);
        assert_eq!(inventory(&stage.join("previous"), false).unwrap(), saved);
    }

    #[test]
    fn wrong_type_managed_target_is_preserved_and_refused() {
        let output = tempfile::tempdir().unwrap();
        fs::write(output.path().join("data"), "this is a user's file, not an export directory").unwrap();
        assert!(Publication::begin(output.path()).is_err());
        assert_eq!(
            fs::read_to_string(output.path().join("data")).unwrap(),
            "this is a user's file, not an export directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_data_cannot_redirect_publication_or_cleanup() {
        let output = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::write(external.path().join("keep.txt"), "external").unwrap();
        std::os::unix::fs::symlink(external.path(), output.path().join("data")).unwrap();
        assert!(Publication::begin(output.path()).is_err());
        assert_eq!(fs::read_to_string(external.path().join("keep.txt")).unwrap(), "external");
    }
}
