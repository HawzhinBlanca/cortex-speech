use crate::atomic_file::{remove_file_on_error, replace_file};
use crate::audio;
use crate::db::{Database, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::validation::input as validate;
use flacenc::error::Verify;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Serialize staging/recovery/swap so concurrent renderer requests cannot sweep each other's trees.
static AUDIO_EXPORT_PUBLICATION_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const AUDIO_EXPORT_STAGING_TAG: &str = ".cortex-reviewed-audio-stage-";
const AUDIO_EXPORT_BACKUP_TAG: &str = ".cortex-reviewed-audio-backup-";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioExportFault {
    None,
    /// Exercise the ENOSPC cleanup path after a clip reaches private staging.
    #[cfg(test)]
    DiskFullAfterClips,
}

struct AudioExportStage {
    path: PathBuf,
    parent: PathBuf,
    prefix: String,
    armed: bool,
}

impl AudioExportStage {
    fn new(path: PathBuf, parent: PathBuf, prefix: String) -> Self {
        Self { path, parent, prefix, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AudioExportStage {
    fn drop(&mut self) {
        if self.armed {
            cleanup_generated_audio_export_dir(&self.path, &self.parent, &self.prefix);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioExportOptions {
    pub output_dir: String,
    pub format: AudioExportFormat,
    pub sample_rate: u32,
    /// Add a spreadsheet-safe CSV view. The exact, machine-readable `metadata.jsonl` is mandatory
    /// whenever any reviewed audio is written, because audio without its approved label/revision is
    /// not a recoverable dataset artifact.
    pub include_metadata: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AudioExportFormat {
    Wav,
    Flac,
}

impl Default for AudioExportOptions {
    fn default() -> Self {
        Self { output_dir: String::new(), format: AudioExportFormat::Wav, sample_rate: 16000, include_metadata: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioExportResult {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub output_dir: String,
    pub files: Vec<String>,
    pub errors: Vec<String>,
}

struct ExportedAudioFile {
    filename: String,
    segment: SpeechSegment,
    /// The exact label the human approved. This is deliberately NOT normalized/canonicalized: the
    /// shared audio sidecar is the durable audio↔verbatim-text pairing, not a derived ASR view.
    effective_transcript: String,
    /// The database-owned revision read in the same statement as `segment`, so the sidecar says
    /// exactly which reviewed row snapshot supplied the label.
    review_revision: i64,
    /// Duration of the clip ACTUALLY written to disk, not the segment's stored duration_ms. The
    /// two drift when slice_for_export clamps an over-long window to the decoded length (source
    /// re-encoded/shortened after import, or relink_audio pointing at a shorter file) and on the
    /// no-alignment whole-file fallback. metadata.csv must describe the bytes on disk — same
    /// invariant the HF exporter already enforces (export.rs clip_dur_ms).
    clip_duration_ms: i64,
}

/// Resolve the real human decision that makes a clip eligible for this shared, reviewed-audio
/// export. `verified` is intentionally absent: bulk verification and rejected rows can both carry
/// that flag, so it is not evidence that a person accepted the audio↔text pair.
fn human_decision_for_export(seg: &SpeechSegment) -> Option<&str> {
    let is_accept_or_edit = |value: &str| {
        ["accept", "edit", "human_accept", "human_edit"].iter().any(|candidate| value.eq_ignore_ascii_case(candidate))
    };
    match seg.human_decision.as_deref() {
        // Any present current decision is authoritative. A reject/unknown value must fail closed;
        // it must never fall through to an older `human_accept`/`human_edit` verdict.
        Some(value) => is_accept_or_edit(value).then_some(value),
        // Legacy reviewed rows may predate `human_decision` while still carrying the authoritative
        // human verdict. Preserve their exact stored decision code rather than inventing one.
        None => seg.verdict.as_deref().filter(|value| is_accept_or_edit(value)),
    }
}

fn human_export_label(seg: &SpeechSegment) -> Option<(&str, &str)> {
    // `is_gold` rows are hidden answer keys/evaluation material. They have their own eval export and
    // must never leak into a reviewed training-audio bundle even when their audio fingerprint is not
    // also registered in the separate gold_segments holdout table.
    if seg.is_gold || crate::quality::is_human_rejected(seg) {
        return None;
    }
    let decision = human_decision_for_export(seg)?;
    let transcript = crate::quality::human_verified_text(seg)?;
    if transcript.trim().is_empty() || crate::quality::is_placeholder_transcript(transcript) {
        return None;
    }
    Some((transcript, decision))
}

fn audio_export_sibling_prefix(output_dir: &Path, tag: &str) -> AppResult<String> {
    let output_name =
        output_dir.file_name().and_then(|name| name.to_str()).filter(|name| !name.is_empty()).ok_or_else(|| {
            AppError::Validation("Reviewed-audio export target must have a directory name".to_string())
        })?;
    Ok(format!(".{output_name}{tag}"))
}

fn generated_audio_export_dirs(parent: &Path, prefix: &str) -> AppResult<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_str().is_some_and(|name| name.starts_with(prefix)) {
            continue;
        }
        // Never follow a caller-created symlink/reparse point during recursive cleanup.
        let file_type = entry.file_type()?;
        if file_type.is_dir() && !file_type.is_symlink() {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

/// Remove only an exact same-parent generated directory; never follow a caller-created link.
fn cleanup_generated_audio_export_dir(path: &Path, parent: &Path, prefix: &str) {
    if !path.exists() {
        return;
    }
    let lexical_safe = path.parent() == Some(parent)
        && path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(prefix));
    let resolved_safe = path
        .canonicalize()
        .ok()
        .zip(parent.canonicalize().ok())
        .is_some_and(|(resolved, resolved_parent)| resolved.parent() == Some(resolved_parent.as_path()));
    let is_real_dir = std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false);
    if !(lexical_safe && resolved_safe && is_real_dir) {
        tracing::error!(
            "Refusing unsafe reviewed-audio export cleanup outside the resolved parent: {}",
            path.display()
        );
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(path) {
        tracing::warn!("Failed to remove reviewed-audio export private directory {}: {error}", path.display());
    }
}

fn sha256_file(path: &Path) -> AppResult<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

fn safe_manifest_filename(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\'])
        && Path::new(value).file_name().and_then(|name| name.to_str()) == Some(value)
}

/// Require byte integrity plus an exact `{SHA256SUMS + manifest entries}` audio↔metadata inventory.
fn verify_complete_audio_export_dir(output_dir: &Path, expected_files: Option<&[String]>) -> AppResult<()> {
    let manifest_path = output_dir.join("SHA256SUMS");
    let metadata_path = output_dir.join("metadata.jsonl");
    if !manifest_path.is_file() || !metadata_path.is_file() {
        return Err(AppError::Validation(
            "directory is not a complete reviewed-audio export (metadata.jsonl or SHA256SUMS is missing)".to_string(),
        ));
    }
    let manifest_size = std::fs::metadata(&manifest_path)?.len();
    if manifest_size > 16 * 1024 * 1024 {
        return Err(AppError::Validation(
            "reviewed-audio SHA256SUMS exceeds the 16 MiB verification limit".to_string(),
        ));
    }

    let mut declared = HashMap::<String, String>::new();
    for line in BufReader::new(std::fs::File::open(&manifest_path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (hash, filename) = line
            .split_once("  ")
            .ok_or_else(|| AppError::Validation("reviewed-audio SHA256SUMS contains a malformed entry".to_string()))?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) || !safe_manifest_filename(filename) {
            return Err(AppError::Validation(
                "reviewed-audio SHA256SUMS contains an unsafe filename or invalid digest".to_string(),
            ));
        }
        if filename == "SHA256SUMS" || declared.insert(filename.to_string(), hash.to_ascii_lowercase()).is_some() {
            return Err(AppError::Validation(
                "reviewed-audio SHA256SUMS contains a self-reference or duplicate filename".to_string(),
            ));
        }
    }
    if !declared.contains_key("metadata.jsonl") {
        return Err(AppError::Validation(
            "reviewed-audio SHA256SUMS does not bind the authoritative metadata.jsonl".to_string(),
        ));
    }

    let mut actual = HashSet::<String>::new();
    for entry in std::fs::read_dir(output_dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let filename = entry
            .file_name()
            .into_string()
            .map_err(|_| AppError::Validation("reviewed-audio export contains a non-Unicode filename".to_string()))?;
        if !file_type.is_file() || file_type.is_symlink() || !safe_manifest_filename(&filename) {
            return Err(AppError::Validation(
                "reviewed-audio export contains a directory, link, or unsafe artifact".to_string(),
            ));
        }
        actual.insert(filename);
    }
    let mut declared_with_manifest: HashSet<String> = declared.keys().cloned().collect();
    declared_with_manifest.insert("SHA256SUMS".to_string());
    if actual != declared_with_manifest {
        return Err(AppError::Validation(
            "reviewed-audio export inventory disagrees with SHA256SUMS (missing or orphan artifact)".to_string(),
        ));
    }
    if let Some(expected) = expected_files {
        let expected: HashSet<String> = expected.iter().cloned().collect();
        if expected != actual {
            return Err(AppError::Validation(
                "reviewed-audio staged inventory disagrees with the command result".to_string(),
            ));
        }
    }

    for (filename, expected_hash) in &declared {
        let actual_hash = sha256_file(&output_dir.join(filename))?;
        if &actual_hash != expected_hash {
            return Err(AppError::Validation(format!(
                "reviewed-audio staged artifact failed SHA-256 verification: {filename}"
            )));
        }
    }

    let audio_files: HashSet<String> =
        declared.keys().filter(|name| name.ends_with(".wav") || name.ends_with(".flac")).cloned().collect();
    if audio_files.is_empty() {
        return Err(AppError::Validation("reviewed-audio export contains no audio clips".to_string()));
    }
    let mut labelled_audio = HashSet::<String>::new();
    for line in BufReader::new(std::fs::File::open(&metadata_path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: serde_json::Value = serde_json::from_str(&line)?;
        let filename = record.get("file_name").and_then(serde_json::Value::as_str).ok_or_else(|| {
            AppError::Validation("reviewed-audio metadata.jsonl row is missing file_name".to_string())
        })?;
        if !safe_manifest_filename(filename) || !labelled_audio.insert(filename.to_string()) {
            return Err(AppError::Validation(
                "reviewed-audio metadata.jsonl contains an unsafe or duplicate file_name".to_string(),
            ));
        }
    }
    if labelled_audio != audio_files {
        return Err(AppError::Validation(
            "reviewed-audio metadata.jsonl does not label exactly the published audio clips".to_string(),
        ));
    }
    Ok(())
}

fn require_replaceable_audio_export_target(output_dir: &Path) -> AppResult<()> {
    let metadata = match std::fs::symlink_metadata(output_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::Validation(
            "Reviewed-audio export target must be a real directory, not a file or link".to_string(),
        ));
    }
    if std::fs::read_dir(output_dir)?.next().is_none() {
        return Ok(());
    }
    verify_complete_audio_export_dir(output_dir, None).map_err(|error| {
        AppError::Validation(format!(
            "Reviewed-audio export target is non-empty and is not an exact complete prior Cortex export; preserving it unchanged. Choose an empty folder. ({error})"
        ))
    })
}

/// Recover either side of the two-rename swap, then sweep non-authoritative private staging.
fn recover_interrupted_audio_export(output_dir: &Path) -> AppResult<()> {
    let parent = output_dir.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let staging_prefix = audio_export_sibling_prefix(output_dir, AUDIO_EXPORT_STAGING_TAG)?;
    let backup_prefix = audio_export_sibling_prefix(output_dir, AUDIO_EXPORT_BACKUP_TAG)?;
    let mut backups = generated_audio_export_dirs(parent, &backup_prefix)?;
    backups.sort_by_key(|path| {
        std::fs::metadata(path).and_then(|metadata| metadata.modified()).unwrap_or(std::time::UNIX_EPOCH)
    });

    if !output_dir.exists() {
        if let Some(backup) = backups.pop() {
            std::fs::rename(&backup, output_dir).map_err(|error| {
                AppError::Other(format!(
                    "Could not recover the previous reviewed-audio export after an interrupted publication: {error}"
                ))
            })?;
            crate::atomic_file::fsync_parent_dir(output_dir);
        }
    }

    if output_dir.exists() && !backups.is_empty() {
        // Never discard the recoverable previous generation unless the visible destination is known
        // complete (or is the empty directory that preceded a first publication).
        require_replaceable_audio_export_target(output_dir)?;
        for backup in backups {
            cleanup_generated_audio_export_dir(&backup, parent, &backup_prefix);
        }
    }
    for staging in generated_audio_export_dirs(parent, &staging_prefix)? {
        cleanup_generated_audio_export_dir(&staging, parent, &staging_prefix);
    }
    Ok(())
}

fn publish_staged_audio_export(staging_dir: &Path, output_dir: &Path, expected_files: &[String]) -> AppResult<()> {
    verify_complete_audio_export_dir(staging_dir, Some(expected_files))?;
    require_replaceable_audio_export_target(output_dir)?;

    let parent = output_dir.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let backup_prefix = audio_export_sibling_prefix(output_dir, AUDIO_EXPORT_BACKUP_TAG)?;
    let backup_dir = parent.join(format!("{backup_prefix}{}", uuid::Uuid::new_v4().simple()));
    let had_destination = output_dir.exists();
    if had_destination {
        std::fs::rename(output_dir, &backup_dir).map_err(|error| {
            AppError::Other(format!(
                "Could not preserve the previous reviewed-audio export before publication: {error}"
            ))
        })?;
        crate::atomic_file::fsync_parent_dir(output_dir);
    }

    if let Err(promote_error) = std::fs::rename(staging_dir, output_dir) {
        if had_destination {
            if let Err(restore_error) = std::fs::rename(&backup_dir, output_dir) {
                tracing::error!(
                    preserved_backup = %backup_dir.display(),
                    %restore_error,
                    %promote_error,
                    "Reviewed-audio publication and automatic restoration both failed"
                );
                return Err(AppError::Other(format!(
                    "Reviewed-audio publication failed ({promote_error}) and automatic restoration failed ({restore_error}); the preserved prior export remains in a private recovery directory"
                )));
            }
            crate::atomic_file::fsync_parent_dir(output_dir);
        }
        return Err(AppError::Other(format!(
            "Could not atomically publish the reviewed-audio export; the previous destination was preserved: {promote_error}"
        )));
    }
    crate::atomic_file::fsync_parent_dir(output_dir);

    // A second verification after rename catches an unexpected filesystem filter/driver mutation.
    // Roll back to the previous generation before reporting failure whenever one existed.
    if let Err(verify_error) = verify_complete_audio_export_dir(output_dir, Some(expected_files)) {
        if had_destination {
            // A failed *new* generation must never share the backup namespace: if restoring the real
            // backup is itself interrupted, the next recovery must choose old truth, not this invalid tree.
            let failed_prefix = audio_export_sibling_prefix(output_dir, AUDIO_EXPORT_STAGING_TAG)?;
            let failed_new = parent.join(format!("{failed_prefix}failed-new-{}", uuid::Uuid::new_v4().simple()));
            if std::fs::rename(output_dir, &failed_new).is_ok() && std::fs::rename(&backup_dir, output_dir).is_ok() {
                cleanup_generated_audio_export_dir(&failed_new, parent, &failed_prefix);
                crate::atomic_file::fsync_parent_dir(output_dir);
            }
        } else {
            cleanup_generated_audio_export_dir(
                output_dir,
                parent,
                output_dir.file_name().and_then(|n| n.to_str()).unwrap_or(""),
            );
        }
        return Err(AppError::Other(format!(
            "Reviewed-audio publication failed post-promotion verification; the incomplete generation was not accepted: {verify_error}"
        )));
    }

    if had_destination {
        // The new generation is complete and durable. Backup deletion is non-load-bearing cleanup;
        // a transient scanner lock must not turn a successful publication into a dishonest failure.
        cleanup_generated_audio_export_dir(&backup_dir, parent, &backup_prefix);
    }
    Ok(())
}

/// Export audio segments from a dataset.
/// For each segment, copies or extracts the relevant audio portion to the output directory.
pub fn export_audio_segments(
    db: &Database,
    segment_ids: &[String],
    options: &AudioExportOptions,
) -> AppResult<AudioExportResult> {
    export_audio_segments_inner(db, segment_ids, options, AudioExportFault::None)
}

fn export_audio_segments_inner(
    db: &Database,
    segment_ids: &[String],
    options: &AudioExportOptions,
    fault: AudioExportFault,
) -> AppResult<AudioExportResult> {
    crate::review_campaign::require_export_unblocked(db, "reviewed audio export")?;
    // Round-24/25 #11: the working buffer is decoded+downmixed to 16 kHz (audio::decode_to_pcm), so a
    // requested rate ABOVE 16000 would only UPSAMPLE a band-limited signal and write a WAV/FLAC header
    // (and metadata.csv export_sample_rate) that overstates the true bandwidth of a shared dataset
    // clip. Cap the accepted range at 16000 — only downsampling (e.g. 8000) is meaningful here.
    if options.sample_rate < 8000 || options.sample_rate > 16000 {
        return Err(AppError::Validation(format!(
            "Invalid sample rate: {} (must be between 8000 and 16000; the source is 16 kHz, so higher rates would overstate fidelity)",
            options.sample_rate
        )));
    }

    let validated = validate::validate_output_path(&options.output_dir).map_err(AppError::Validation)?;
    let options = AudioExportOptions { output_dir: validated, ..options.clone() };
    let output_dir = Path::new(&options.output_dir);
    let parent_dir = output_dir.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent_dir)
        .map_err(|e| AppError::Other(format!("Failed to create reviewed-audio export parent: {e}")))?;
    let _publication = AUDIO_EXPORT_PUBLICATION_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    recover_interrupted_audio_export(output_dir)?;
    require_replaceable_audio_export_target(output_dir)?;

    let staging_prefix = audio_export_sibling_prefix(output_dir, AUDIO_EXPORT_STAGING_TAG)?;
    let staging_dir = parent_dir.join(format!("{staging_prefix}{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&staging_dir)
        .map_err(|e| AppError::Other(format!("Failed to create private reviewed-audio staging directory: {e}")))?;
    let mut staging = AudioExportStage::new(staging_dir.clone(), parent_dir.to_path_buf(), staging_prefix);
    let staged_options =
        AudioExportOptions { output_dir: staging_dir.to_string_lossy().to_string(), ..options.clone() };

    // Fail-closed: never export held-out, withdrawn, rejected, or placeholder audio, exactly like
    // export_dataset / HF / bundle. Then require a REAL human accept/edit. `verified` alone is not a
    // review decision: bulk verification can set it without anyone approving this audio↔text pair.
    // Unknown ids are deliberately not put in `policy_excluded`, so export_single_segment still
    // reports their not-found error and preserves the existing per-file failure accounting.
    let mut requested: Vec<SpeechSegment> = Vec::with_capacity(segment_ids.len());
    for id in segment_ids {
        // A policy preflight read is part of the security boundary. Treating an SQL failure like a
        // missing row let the later per-file lookup succeed and bypass holdout/rights exclusion.
        // Unknown ids still fall through to the existing per-file not-found accounting; read errors
        // abort the export before any audio is written.
        if let Some(segment) = db.get_segment_by_id(id)? {
            requested.push(segment);
        }
    }
    let loaded_ids: std::collections::HashSet<String> = requested.iter().map(|s| s.id.clone()).collect();
    let allowed_ids: std::collections::HashSet<String> = crate::export::exclude_unexportable_segments(db, requested)?
        .into_iter()
        .filter(|seg| human_export_label(seg).is_some())
        .map(|seg| seg.id)
        .collect();
    let policy_excluded: std::collections::HashSet<String> = loaded_ids.difference(&allowed_ids).cloned().collect();

    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut skipped_policy = 0usize;
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let mut exported = Vec::new();

    for id in segment_ids {
        if policy_excluded.contains(id) {
            // Intentional fail-closed exclusion; exclude_unexportable_segments already logged the reason.
            skipped_policy += 1;
            continue;
        }
        match export_single_segment(db, id, &staged_options) {
            Ok(exported_file) => {
                succeeded += 1;
                files.push(exported_file.filename.clone());
                exported.push(exported_file);
            }
            Err(e) => {
                failed += 1;
                errors.push(format!("{id}: {e}"));
            }
        }
    }

    if skipped_policy > 0 {
        tracing::warn!(
            "Audio export: skipped {skipped_policy} segment(s) without an exportable current human accept/edit — not exported (fail-closed)"
        );
    }

    if exported.is_empty() {
        // A zero-clip run is a true no-op. In particular, missing media or a policy-only selection
        // must never replace a prior complete bundle with an empty directory.
        return Ok(AudioExportResult {
            total: segment_ids.len(),
            succeeded,
            failed,
            output_dir: options.output_dir.clone(),
            files,
            errors,
        });
    }

    #[cfg(test)]
    if fault == AudioExportFault::DiskFullAfterClips {
        return Err(AppError::Io(std::io::Error::other("simulated disk full after staged reviewed-audio clips")));
    }
    #[cfg(not(test))]
    let _ = fault;

    // The JSONL sidecar is the authoritative audio↔exact-label pairing and is never optional. CSV
    // escaping must prefix formula-like cells for spreadsheet safety, so CSV cannot honestly be the
    // byte-exact transcript record for every possible human label.
    if !exported.is_empty() {
        write_metadata_jsonl(&staging_dir, &exported, &staged_options)?;
        files.push("metadata.jsonl".to_string());
    }

    if options.include_metadata && !exported.is_empty() {
        write_metadata_csv(&staging_dir, &exported, &staged_options)?;
        files.push("metadata.csv".to_string());
    }

    // Write the integrity manifest last so it covers every staged clip and sidecar.
    if !files.is_empty() {
        // The staging tree is fresh, and this explicit list is also the public command result. The
        // verifier below requires the list, manifest and actual directory inventory to match exactly.
        crate::export::write_sha256sums_for(&staging_dir, &files)?;
        files.push("SHA256SUMS".to_string());
    }

    verify_complete_audio_export_dir(&staging_dir, Some(&files))?;
    crate::atomic_file::fsync_parent_dir(&staging_dir.join(".directory-sync"));
    publish_staged_audio_export(&staging_dir, output_dir, &files)?;
    staging.disarm();

    Ok(AudioExportResult {
        total: segment_ids.len(),
        succeeded,
        failed,
        output_dir: options.output_dir.clone(),
        files,
        errors,
    })
}

fn export_single_segment(
    db: &Database,
    segment_id: &str,
    options: &AudioExportOptions,
) -> AppResult<ExportedAudioFile> {
    let (seg, review_revision) = db
        .get_segment_by_id_with_revision(segment_id)?
        .ok_or_else(|| AppError::Other(format!("Segment not found: {segment_id}")))?;
    // Defense in depth: this function is intentionally private today, but it owns the actual bytes on
    // disk. Re-run the central rights/holdout/withdrawal policy on the exact row it will decode so a
    // future caller—or any batch-preflight regression—cannot bypass the policy boundary.
    if crate::export::exclude_unexportable_segments(db, vec![seg.clone()])?.len() != 1 {
        return Err(AppError::Validation(format!(
            "Segment {segment_id}: export blocked by rights, holdout, withdrawal, or quality policy"
        )));
    }
    // Defense in depth against a future caller bypassing export_audio_segments' batch filter. It
    // also guarantees write_metadata_csv never has to guess which transcript or decision was human.
    let (effective_transcript, _) = human_export_label(&seg).ok_or_else(|| {
        AppError::Validation(format!(
            "Segment {segment_id}: no current human accept/edit with a non-empty, non-placeholder transcript"
        ))
    })?;
    let effective_transcript = effective_transcript.to_string();

    let source_path = Path::new(&seg.audio_path);
    if !source_path.exists() {
        return Err(AppError::Other(format!("Audio file not found: {}", seg.audio_path)));
    }

    // Decode audio to 16-bit PCM
    let (sample_rate, pcm_samples) =
        audio::decode_to_pcm(&seg.audio_path).map_err(|e| AppError::Other(format!("Failed to decode audio: {e}")))?;
    crate::export::require_decoded_segment_audio_identity(
        db,
        segment_id,
        &pcm_samples,
        sample_rate,
        "reviewed audio export",
    )?;

    // Slice the clip from the segment's alignment window, sharing the exact guard the HF exporter uses
    // (export::slice_for_export). When the alignment is present and parses but the window is OUT OF RANGE
    // against the (possibly re-encoded/shortened) decoded buffer, or DEGENERATE (end <= start), SKIP the
    // segment with a clear error (the caller counts it as a failed segment) — do NOT fall back to the
    // whole source file. Pairing the entire multi-minute recording with one segment's short
    // transcript/duration in metadata.csv is silent training-data corruption (the round-2 bug, fixed for
    // the HF/dataset path but never for this WAV/FLAC path). The whole-file fallback is reserved strictly
    // for genuinely-absent/unparseable alignment (handled inside slice_for_export).
    let pcm_window = crate::export::slice_for_export(&pcm_samples, sample_rate, seg.alignment_json.as_deref())
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Segment {segment_id}: alignment window out of range (pcm_len={}); refusing to export the \
                 whole source file as this segment",
                pcm_samples.len()
            ))
        })?;
    // Measured BEFORE resampling (resampling changes the sample count but preserves wall-clock
    // duration); `sample_rate` is the rate the window was sliced at.
    let clip_duration_ms = (pcm_window.len() as i64).saturating_mul(1000) / i64::from(sample_rate.max(1));
    let pcm_samples = resample_pcm_i16(pcm_window.as_ref(), sample_rate, options.sample_rate);

    // Determine output filename
    let stem = source_path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = match options.format {
        AudioExportFormat::Wav => "wav",
        AudioExportFormat::Flac => "flac",
    };
    let safe_segment_id = validate::sanitize_filename(segment_id);
    let output_filename = format!("{stem}_{safe_segment_id}.{ext}");
    let output_path = Path::new(&options.output_dir).join(&output_filename);
    let tmp_path = temporary_output_path(&output_path);

    match options.format {
        AudioExportFormat::Wav => {
            // Write WAV file (16-bit mono PCM)
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: options.sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };

            remove_file_on_error(
                &tmp_path,
                (|| -> AppResult<()> {
                    let mut writer = hound::WavWriter::create(&tmp_path, spec)
                        .map_err(|e| AppError::Other(format!("Failed to create temporary WAV file: {e}")))?;

                    for &sample in &pcm_samples {
                        writer
                            .write_sample(sample)
                            .map_err(|e| AppError::Other(format!("Failed to write sample: {e}")))?;
                    }

                    writer.finalize().map_err(|e| AppError::Other(format!("Failed to finalize WAV: {e}")))?;
                    replace_file(&tmp_path, &output_path)
                        .map_err(|e| AppError::Other(format!("Failed to promote exported WAV file: {e}")))?;
                    Ok(())
                })(),
            )?;
        }
        AudioExportFormat::Flac => {
            // Write FLAC file using flacenc
            let config = flacenc::config::Encoder::default()
                .into_verified()
                .map_err(|e| AppError::Other(format!("FLAC encoder config error: {:?}", e)))?;

            // Convert i16 samples to i32 for flacenc
            let i32_samples: Vec<i32> = pcm_samples.iter().map(|&s| s as i32).collect();

            // MemSource::from_samples(samples, channels, bits_per_sample, sample_rate)
            let source = flacenc::source::MemSource::from_samples(&i32_samples, 1, 16, options.sample_rate as usize);

            let flac_stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
                .map_err(|e| AppError::Other(format!("FLAC encoding failed: {:?}", e)))?;

            use flacenc::component::BitRepr;
            let mut sink = flacenc::bitsink::ByteSink::new();
            flac_stream
                .write(&mut sink)
                .map_err(|e| AppError::Other(format!("Failed to write FLAC stream to sink: {:?}", e)))?;

            remove_file_on_error(
                &tmp_path,
                (|| -> AppResult<()> {
                    std::fs::write(&tmp_path, sink.as_slice())
                        .map_err(|e| AppError::Other(format!("Failed to write temporary FLAC file: {e}")))?;
                    replace_file(&tmp_path, &output_path)
                        .map_err(|e| AppError::Other(format!("Failed to promote exported FLAC file: {e}")))?;
                    Ok(())
                })(),
            )?;
        }
    }

    Ok(ExportedAudioFile {
        filename: output_filename,
        segment: seg,
        effective_transcript,
        review_revision,
        clip_duration_ms,
    })
}

fn write_metadata_jsonl(
    output_dir: &Path,
    exported: &[ExportedAudioFile],
    options: &AudioExportOptions,
) -> AppResult<()> {
    let metadata_path = output_dir.join("metadata.jsonl");
    let tmp_path = metadata_path.with_extension("jsonl.tmp");
    remove_file_on_error(
        &tmp_path,
        (|| -> AppResult<()> {
            let file = std::fs::File::create(&tmp_path)?;
            let mut writer = BufWriter::new(file);
            let export_format = match options.format {
                AudioExportFormat::Wav => "wav",
                AudioExportFormat::Flac => "flac",
            };

            for item in exported {
                let decision = human_decision_for_export(&item.segment).ok_or_else(|| {
                    AppError::Other("audio export lost its required human-decision invariant".to_string())
                })?;
                // Intentionally serialize a narrow public record, not SpeechSegment: the latter
                // contains absolute paths, reviewer identity, and internal model/quality fields.
                let record = serde_json::json!({
                    "file_name": item.filename.as_str(),
                    "segment_id": item.segment.id.as_str(),
                    "source_audio_path": crate::export::export_audio_ref(&item.segment.audio_path),
                    "effective_transcript": item.effective_transcript.as_str(),
                    "transcript_source": "human_verified",
                    "human_decision": decision,
                    "review_revision": item.review_revision,
                    "duration_ms": item.clip_duration_ms,
                    "export_sample_rate": options.sample_rate,
                    "export_format": export_format,
                });
                serde_json::to_writer(&mut writer, &record)?;
                writer.write_all(b"\n")?;
            }
            writer.flush()?;
            drop(writer);
            replace_file(&tmp_path, &metadata_path)
                .map_err(|e| AppError::Other(format!("Failed to promote audio export metadata: {e}")))?;
            Ok(())
        })(),
    )
}

fn write_metadata_csv(
    output_dir: &Path,
    exported: &[ExportedAudioFile],
    options: &AudioExportOptions,
) -> AppResult<()> {
    let metadata_path = output_dir.join("metadata.csv");
    let tmp_path = metadata_path.with_extension("csv.tmp");
    remove_file_on_error(
        &tmp_path,
        (|| -> AppResult<()> {
            let mut wtr = csv::Writer::from_path(&tmp_path)?;
            wtr.write_record([
                "file_name",
                "segment_id",
                "source_audio_path",
                "raw_transcript",
                "normalized_transcript",
                "annotated_transcript",
                "effective_transcript",
                "transcript_source",
                "duration_ms",
                "speaker_id",
                "verified",
                "confidence",
                "ctc_score",
                "clipping_ratio",
                "rms_db",
                "snr_db",
                "split",
                "signal_anomaly_score",
                "verdict",
                "human_decision",
                "review_revision",
                "alignment_quality",
                "export_sample_rate",
                "export_format",
            ])?;

            for item in exported {
                let seg = &item.segment;
                let duration_ms = item.clip_duration_ms.to_string();
                let confidence = optional_f64(seg.confidence);
                let ctc_score = optional_f64(seg.ctc_score);
                let clipping_ratio = optional_f64(seg.clipping_ratio);
                let rms_db = optional_f64(seg.rms_db);
                let snr_db = optional_f64(seg.snr_db);
                let signal_anomaly_score = optional_f64(seg.signal_anomaly_score);
                let export_sample_rate = options.sample_rate.to_string();
                let export_format = match options.format {
                    AudioExportFormat::Wav => "wav",
                    AudioExportFormat::Flac => "flac",
                };
                // CWE-1236: neutralize spreadsheet formula injection on the free-text columns
                // (transcripts / speaker / verdict). Numeric/enum structural columns are left untouched.
                // The source path is reduced to its BASENAME (via export_audio_ref) so this shared
                // metadata.csv never leaks the curator's absolute path — the OS username + directory
                // layout — exactly as the JSON/JSONL/CSV/Parquet/HF exporters do.
                let source_ref = crate::export::export_audio_ref(&seg.audio_path);
                let raw_t = crate::export::csv_safe_cell(seg.raw_transcript.as_str());
                let norm_t = crate::export::csv_safe_cell(seg.normalized_transcript.as_deref().unwrap_or(""));
                let annot_t = crate::export::csv_safe_cell(seg.annotated_transcript.as_deref().unwrap_or(""));
                let effective_t = crate::export::csv_safe_cell(item.effective_transcript.as_str());
                let speaker_t = crate::export::csv_safe_cell(seg.speaker_id.as_deref().unwrap_or(""));
                let verdict_t = crate::export::csv_safe_cell(seg.verdict.as_deref().unwrap_or(""));
                let human_decision = human_decision_for_export(seg).ok_or_else(|| {
                    AppError::Other("audio export lost its required human-decision invariant".to_string())
                })?;
                let human_decision_t = crate::export::csv_safe_cell(human_decision);
                let review_revision = item.review_revision.to_string();
                wtr.write_record([
                    item.filename.as_str(),
                    seg.id.as_str(),
                    // Basename only (source_ref = export_audio_ref) — never the curator's absolute import
                    // path, which embeds the OS username + drive layout, a PII leak into this
                    // deliberately-shared metadata.csv. Matches the sanitization every export.rs exporter
                    // applies (round-22 #2). The transcript cells are additionally csv_safe_cell-escaped
                    // above to prevent CSV/formula injection.
                    source_ref,
                    raw_t.as_ref(),
                    norm_t.as_ref(),
                    annot_t.as_ref(),
                    effective_t.as_ref(),
                    "human_verified",
                    duration_ms.as_str(),
                    speaker_t.as_ref(),
                    if seg.verified { "1" } else { "0" },
                    confidence.as_str(),
                    ctc_score.as_str(),
                    clipping_ratio.as_str(),
                    rms_db.as_str(),
                    snr_db.as_str(),
                    seg.split.as_deref().unwrap_or(""),
                    signal_anomaly_score.as_str(),
                    verdict_t.as_ref(),
                    human_decision_t.as_ref(),
                    review_revision.as_str(),
                    seg.alignment_quality.as_deref().unwrap_or(""),
                    export_sample_rate.as_str(),
                    export_format,
                ])?;
            }

            wtr.flush()?;
            drop(wtr);
            replace_file(&tmp_path, &metadata_path)
                .map_err(|e| AppError::Other(format!("Failed to promote audio export metadata: {e}")))?;
            Ok(())
        })(),
    )
}

fn optional_f64(value: Option<f64>) -> String {
    value.map(|number| number.to_string()).unwrap_or_default()
}

fn resample_pcm_i16(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    if from_rate == to_rate {
        return samples.to_vec();
    }
    let f32_samples: Vec<f32> = samples.iter().map(|&sample| sample as f32 / i16::MAX as f32).collect();
    audio::resample(&f32_samples, from_rate, to_rate)
        .into_iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

fn temporary_output_path(output_path: &Path) -> PathBuf {
    let file_name = output_path.file_name().and_then(|name| name.to_str()).unwrap_or("export.wav");
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    output_path.with_file_name(format!("{file_name}.tmp-{}-{nonce}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunking::SegmentSourceMeta;
    use crate::db::{Database, SpeechSegment};
    use std::fs;
    use tempfile::TempDir;

    fn bind_test_audio_identity(db: &Database, segment_id: &str) {
        let segment = db.get_segment_by_id(segment_id).unwrap().expect("segment fixture");
        let content_hash = match crate::audio::decode_to_pcm(&segment.audio_path) {
            Ok((sample_rate, pcm)) => crate::fingerprint::AudioFingerprint::content_hash(&pcm, sample_rate),
            // Missing-media drills need a syntactically valid historical authority; the production
            // export fails on the absent file before comparing it.
            Err(_) => "0".repeat(64),
        };
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash = ?2 WHERE id = ?1",
                rusqlite::params![segment_id, content_hash],
            )
            .unwrap();
    }

    fn record_test_phone_decision(db: &Database, segment_id: &str, decision: &str, text: Option<&str>, reviewer: &str) {
        bind_test_audio_identity(db, segment_id);
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET alignment_json = json_object(
                            'source_start_ms', 0,
                            'source_end_ms', duration_ms,
                            'chunk_index', 0,
                            'chunk_count', 1
                        )
                  WHERE id = ?1",
                rusqlite::params![segment_id],
            )
            .unwrap();
        let revision = db.segment_review_revision(segment_id).unwrap().unwrap();
        assert!(db
            .record_phone_human_decision_by_at_revision(segment_id, decision, text, reviewer, revision)
            .unwrap()
            .is_some());
    }

    fn make_wav_file(path: &Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..16000 {
            let sample = (i as f32 * 0.1).sin();
            writer.write_sample((sample * i16::MAX as f32) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn insert_test_segment(db: &Database, id: &str, wav_path: &Path) {
        let seg = SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: wav_path.to_string_lossy().to_string(),
            raw_transcript: "hello".to_string(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 1000,
            speaker_id: None,
            verified: false,
            confidence: None,
            ctc_score: None,
            clipping_ratio: None,
            rms_db: None,
            snr_db: None,
            split: None,
            signal_anomaly_score: None,
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).unwrap();
        record_test_phone_decision(db, id, "accept", Some("hello"), "test-reviewer");
    }

    fn output_wav_info(path: &Path) -> (u32, u32) {
        let reader = hound::WavReader::open(path).unwrap();
        let spec = reader.spec();
        (spec.sample_rate, reader.duration())
    }

    fn directory_bytes(path: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().to_string();
                (name, fs::read(entry.path()).unwrap())
            })
            .collect()
    }

    /// A withdrawn recording's VOICE must never be written to disk. Until 2026-08-06 this exporter
    /// made no rights call at all: it wrote the WAV/FLAC plus every transcript column into
    /// metadata.csv for a recording whose consent had been revoked. Voice is biometric data under
    /// GDPR Art. 9, and revocation is the one instruction the rights schema exists to obey.
    #[test]
    fn a_revoked_recording_is_never_written_to_disk() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let keep = tmp.path().join("keep.wav");
        let gone = tmp.path().join("withdrawn.wav");
        make_wav_file(&keep);
        make_wav_file(&gone);
        insert_test_segment(&db, "keep-1", &keep);
        insert_test_segment(&db, "revoked-1", &gone);
        db.revoke_recording(&gone.to_string_lossy()).unwrap();

        let out = tmp.path().join("audio_out");
        fs::create_dir_all(&out).unwrap();
        let options = AudioExportOptions {
            output_dir: out.to_string_lossy().to_string(),
            format: AudioExportFormat::Wav,
            sample_rate: 16000,
            include_metadata: true,
        };
        let ids: Vec<String> = ["keep-1", "revoked-1"].iter().map(|s| s.to_string()).collect();
        let result = export_audio_segments(&db, &ids, &options).unwrap();

        assert_eq!(result.succeeded, 1, "only the consenting clip may export");
        // The audio itself must not be on disk under any name.
        let written: Vec<String> =
            fs::read_dir(&out).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert!(
            !written.iter().any(|f| f.contains("withdrawn") || f.contains("revoked")),
            "a withdrawn recording's audio was written: {written:?}"
        );
        // And its transcript must not survive in the sidecar either.
        let metadata = fs::read_to_string(out.join("metadata.csv")).unwrap();
        assert!(!metadata.contains("revoked-1"), "withdrawn transcript leaked into metadata.csv");
    }

    #[test]
    fn policy_preflight_database_failure_aborts_before_writing_any_artifact() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let wav = tmp.path().join("never-written.wav");
        make_wav_file(&wav);
        insert_test_segment(&db, "policy-read-fault", &wav);

        // Deterministic injected read fault after all campaign tables exist. The old
        // `.ok().flatten()` path converted this SQL error into an omitted prefilter row, then a later
        // read could admit it. One security-boundary read failure must terminate the whole export.
        db.connection().execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE speech_segments;").unwrap();
        let out = tmp.path().join("audio_out");
        let error = export_audio_segments(
            &db,
            &["policy-read-fault".to_string()],
            &AudioExportOptions {
                output_dir: out.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16_000,
                include_metadata: true,
            },
        )
        .expect_err("an unreadable policy snapshot must never degrade into per-file admission");

        assert!(error.to_string().to_ascii_lowercase().contains("speech_segments"));
        assert!(
            !out.exists() || fs::read_dir(&out).unwrap().next().is_none(),
            "policy read failure must leave no audio, metadata, or checksum artifact"
        );
    }

    #[test]
    fn missing_media_drill_exports_present_clips_and_reports_missing_per_file() {
        // Week-2 missing-media FAULT DRILL: with half the library's source audio gone (moved drive,
        // deleted files), the export family must DEGRADE, never corrupt or abort wholesale:
        //   * audio export: present clips export; each missing one is a PER-FILE error (clean message,
        //     no panic); metadata.csv + SHA256SUMS cover exactly the exported artifacts;
        //   * table export (export_dataset): succeeds with ALL rows — transcripts don't need audio.
        let tmp = TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        // Two segments with real WAVs, two pointing at files that do not exist.
        let present1 = tmp.path().join("ok1.wav");
        let present2 = tmp.path().join("ok2.wav");
        make_wav_file(&present1);
        make_wav_file(&present2);
        insert_test_segment(&db, "ok-1", &present1);
        insert_test_segment(&db, "ok-2", &present2);
        insert_test_segment(&db, "gone-1", &tmp.path().join("deleted1.wav"));
        insert_test_segment(&db, "gone-2", &tmp.path().join("deleted2.wav"));

        let out = tmp.path().join("audio_out");
        fs::create_dir_all(&out).unwrap();
        let options = AudioExportOptions {
            output_dir: out.to_string_lossy().to_string(),
            format: AudioExportFormat::Wav,
            sample_rate: 16000,
            include_metadata: true,
        };
        let ids: Vec<String> = ["ok-1", "ok-2", "gone-1", "gone-2"].iter().map(|s| s.to_string()).collect();
        let result = export_audio_segments(&db, &ids, &options).expect("mixed export must not abort wholesale");

        assert_eq!(result.succeeded, 2, "both present clips export");
        assert_eq!(result.failed, 2, "both missing sources are per-file failures");
        assert_eq!(result.errors.len(), 2);
        assert!(result.errors.iter().all(|e| e.contains("not found")), "{:?}", result.errors);
        assert!(result.files.iter().any(|f| f == "metadata.csv"));
        assert!(result.files.iter().any(|f| f == "SHA256SUMS"));
        // Exactly the two exported clips exist on disk (plus metadata + manifest).
        let wavs =
            fs::read_dir(&out).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".wav"));
        assert_eq!(wavs.count(), 2, "only the present clips land on disk");

        // Table export is audio-independent: all four rows ship.
        let table = tmp.path().join("dataset.json");
        crate::export::export_dataset(&db, &table, &crate::settings::ExportFormat::Json)
            .expect("table export must succeed with missing media");
        let text = fs::read_to_string(&table).unwrap();
        for id in ["ok-1", "ok-2", "gone-1", "gone-2"] {
            assert!(text.contains(id), "table export must include {id} regardless of audio presence");
        }
    }

    #[test]
    fn reexport_of_smaller_selection_atomically_removes_prior_generation_orphans() {
        // A complete prior bundle is replaced wholesale by the fresh staged generation. The omitted
        // clip can therefore be neither left as an orphan nor accidentally vouched for by SHA256SUMS.
        let tmp = TempDir::new().unwrap();
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let wav1 = tmp.path().join("clip1.wav");
        let wav2 = tmp.path().join("clip2.wav");
        make_wav_file(&wav1);
        make_wav_file(&wav2);
        insert_test_segment(&db, "seg-1", &wav1);
        insert_test_segment(&db, "seg-2", &wav2);

        let out = tmp.path().join("audio_out");
        fs::create_dir_all(&out).unwrap();
        let options = AudioExportOptions {
            output_dir: out.to_string_lossy().to_string(),
            format: AudioExportFormat::Wav,
            sample_rate: 16000,
            include_metadata: true,
        };
        let first = export_audio_segments(&db, &["seg-1".to_string(), "seg-2".to_string()], &options).unwrap();
        let prior_only = first.files.iter().find(|name| name.contains("seg-2") && name.ends_with(".wav")).unwrap();
        assert!(out.join(prior_only).is_file());

        let result = export_audio_segments(&db, &["seg-1".to_string()], &options).unwrap();
        assert_eq!(result.succeeded, 1);

        let sums = fs::read_to_string(out.join("SHA256SUMS")).unwrap();
        assert!(!sums.contains(prior_only), "replacement manifest must omit the prior-only clip:\n{sums}");
        assert!(sums.contains("metadata.csv"), "manifest covers this export's metadata.csv");
        let exported_wav = result.files.iter().find(|f| f.ends_with(".wav")).expect("an exported wav");
        assert!(sums.contains(exported_wav.as_str()), "manifest covers the exported clip {exported_wav}");
        assert!(!out.join(prior_only).exists(), "whole-generation replacement must remove prior-only clips");
        verify_complete_audio_export_dir(&out, Some(&result.files)).unwrap();
    }

    #[test]
    fn metadata_csv_reduces_source_path_to_basename_never_leaking_absolute_path() {
        // The shared metadata.csv must publish only the source BASENAME, never the curator's absolute
        // path (which leaks the OS username + directory layout), exactly like the JSON/JSONL/CSV/Parquet/HF
        // exporters. Test write_metadata_csv directly so no fixture audio is needed.
        let tmp = TempDir::new().unwrap();
        let seg = SpeechSegment {
            id: "s1".to_string(),
            // A fake absolute path with a username + dir layout. Deliberately NOT a Windows per-user
            // profile path: that form is itself a repo-hygiene-gate violation (the gate can't tell a fake
            // fixture username from a real one), so this uses the C:\Recordings\ convention that
            // export.rs::exports_never_leak_absolute_paths already uses.
            audio_path: "C:\\Recordings\\studio_user\\private_clips\\clip_001.wav".to_string(),
            raw_transcript: "hello".to_string(),
            verified: true,
            verdict: Some("human_accept".to_string()),
            verdict_transcript: Some("hello".to_string()),
            human_decision: Some("accept".to_string()),
            reviewed_by: Some("test-reviewer".to_string()),
            ..SpeechSegment::default()
        };
        let exported = vec![ExportedAudioFile {
            filename: "ep_s1.wav".to_string(),
            segment: seg,
            effective_transcript: "hello".to_string(),
            review_revision: 0,
            clip_duration_ms: 0,
        }];
        write_metadata_csv(tmp.path(), &exported, &AudioExportOptions::default()).unwrap();
        let csv = fs::read_to_string(tmp.path().join("metadata.csv")).unwrap();
        assert!(!csv.contains("studio_user"), "absolute path leaked the OS username:\n{csv}");
        assert!(!csv.contains("private_clips"), "absolute path leaked the directory layout:\n{csv}");
        assert!(csv.contains("clip_001.wav"), "the source basename must be published as provenance:\n{csv}");
    }

    #[test]
    fn jsonl_preserves_a_formula_like_human_label_while_csv_remains_spreadsheet_safe() {
        let tmp = TempDir::new().unwrap();
        let exact = "=SUM(1,2)";
        let seg = SpeechSegment {
            id: "formula-label".to_string(),
            audio_path: r"C:\Recordings\clip.wav".to_string(),
            raw_transcript: "machine".to_string(),
            verified: true,
            verdict: Some("human_edit".to_string()),
            verdict_transcript: Some(exact.to_string()),
            human_decision: Some("edit".to_string()),
            reviewed_by: Some("private-reviewer".to_string()),
            ..SpeechSegment::default()
        };
        let exported = vec![ExportedAudioFile {
            filename: "clip_formula-label.wav".to_string(),
            segment: seg,
            effective_transcript: exact.to_string(),
            review_revision: 7,
            clip_duration_ms: 900,
        }];
        let options = AudioExportOptions::default();

        write_metadata_jsonl(tmp.path(), &exported, &options).unwrap();
        write_metadata_csv(tmp.path(), &exported, &options).unwrap();

        let line = fs::read_to_string(tmp.path().join("metadata.jsonl")).unwrap();
        let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["effective_transcript"], exact, "JSONL is the byte-exact training label");
        assert!(!line.contains("private-reviewer"), "reviewer identity leaked into public JSONL");

        let mut reader = csv::Reader::from_path(tmp.path().join("metadata.csv")).unwrap();
        let headers = reader.headers().unwrap().clone();
        let row = reader.records().next().unwrap().unwrap();
        assert_eq!(
            metadata_value(&headers, &row, "effective_transcript"),
            "'=SUM(1,2)",
            "CSV remains safe to open in a spreadsheet; JSONL above is authoritative"
        );
    }

    #[test]
    fn verified_without_a_human_decision_is_not_exported() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("machine.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let seg = SpeechSegment {
            id: "machine-only".to_string(),
            audio_path: wav_path.to_string_lossy().to_string(),
            raw_transcript: "machine draft".to_string(),
            duration_ms: 1000,
            // This flag does not prove a person accepted the audio↔text pair.
            verified: true,
            ..SpeechSegment::default()
        };
        db.insert_legacy_segment_fixture(&seg).unwrap();

        let out = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            std::slice::from_ref(&seg.id),
            &AudioExportOptions {
                output_dir: out.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.succeeded, 0, "verified must not impersonate a human decision");
        assert_eq!(result.failed, 0, "policy exclusion is not an encoder failure");
        assert!(result.files.is_empty());
        assert!(!out.join("machine_machine-only.wav").exists());
        assert!(!out.join("metadata.csv").exists());
        assert!(!out.join("SHA256SUMS").exists());
    }

    #[test]
    fn an_is_gold_answer_key_is_not_exported_as_training_audio() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("answer-key.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let seg = SpeechSegment {
            id: "hidden-gold".to_string(),
            audio_path: wav_path.to_string_lossy().to_string(),
            raw_transcript: "wrong draft".to_string(),
            duration_ms: 1000,
            is_gold: true,
            ..SpeechSegment::default()
        };
        db.insert_legacy_segment_fixture(&seg).unwrap();
        record_test_phone_decision(&db, &seg.id, "edit", Some("known answer"), "Owner");

        let out = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &[seg.id],
            &AudioExportOptions { output_dir: out.to_string_lossy().to_string(), ..AudioExportOptions::default() },
        )
        .unwrap();
        assert_eq!((result.succeeded, result.failed), (0, 0));
        assert!(result.files.is_empty());
        assert!(!out.join("metadata.jsonl").exists());
    }

    #[test]
    fn metadata_csv_carries_the_exact_human_label_and_review_provenance() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("truth.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let exact_correction = "ئەمە دەقی ڕاستەقینەی مرۆڤە، ١٢٣!";
        let seg = SpeechSegment {
            id: "human-edit".to_string(),
            audio_path: wav_path.to_string_lossy().to_string(),
            raw_transcript: "machine draft".to_string(),
            normalized_transcript: Some("machine normalized".to_string()),
            annotated_transcript: None,
            verified: false,
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).unwrap();
        record_test_phone_decision(&db, &seg.id, "edit", Some(exact_correction), "Rubar");
        let expected_revision = db.segment_review_revision(&seg.id).unwrap().unwrap();

        let out = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            std::slice::from_ref(&seg.id),
            &AudioExportOptions {
                output_dir: out.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();
        assert_eq!(result.succeeded, 1, "a real human edit is exportable: {:?}", result.errors);

        let mut reader = csv::Reader::from_path(out.join("metadata.csv")).unwrap();
        let headers = reader.headers().unwrap().clone();
        let row = reader.records().next().unwrap().unwrap();
        assert_eq!(metadata_value(&headers, &row, "raw_transcript"), "machine draft");
        assert_eq!(metadata_value(&headers, &row, "effective_transcript"), exact_correction);
        assert_eq!(metadata_value(&headers, &row, "transcript_source"), "human_verified");
        assert_eq!(metadata_value(&headers, &row, "human_decision"), "edit");
        assert_eq!(metadata_value(&headers, &row, "review_revision"), expected_revision.to_string());
        assert!(
            !headers.iter().any(|header| header == "reviewed_by"),
            "shared artifacts must not expose reviewer names"
        );
        assert!(!row.iter().any(|value| value == "Rubar"), "reviewer identity leaked into shared metadata");
        assert!(reader.records().next().is_none());

        let jsonl = fs::read_to_string(out.join("metadata.jsonl")).unwrap();
        let exact: serde_json::Value = serde_json::from_str(jsonl.trim()).unwrap();
        assert_eq!(exact["effective_transcript"], exact_correction);
        assert_eq!(exact["review_revision"], expected_revision);
        assert!(!jsonl.contains("Rubar"), "the authoritative exact-label sidecar must not expose reviewer identity");
    }

    #[test]
    fn transcript_source_is_human_verified_only_for_a_real_or_legacy_human_decision() {
        let annotation_only = SpeechSegment {
            raw_transcript: "machine".to_string(),
            annotated_transcript: Some("unapproved human draft".to_string()),
            ..SpeechSegment::default()
        };
        assert!(
            human_export_label(&annotation_only).is_none(),
            "an annotation without accept/edit must not be stamped human_verified"
        );

        let legacy_human_accept = SpeechSegment {
            raw_transcript: "machine".to_string(),
            annotated_transcript: Some("legacy approved text".to_string()),
            verdict: Some("human_accept".to_string()),
            human_decision: None,
            ..SpeechSegment::default()
        };
        let (text, decision) =
            human_export_label(&legacy_human_accept).expect("legacy human verdict remains valid provenance");
        assert_eq!(text, "legacy approved text");
        assert_eq!(decision, "human_accept");

        let contradictory_current_row = SpeechSegment {
            raw_transcript: "machine".to_string(),
            verdict_transcript: Some("stale previously approved text".to_string()),
            verdict: Some("human_accept".to_string()),
            human_decision: Some("unknown".to_string()),
            ..SpeechSegment::default()
        };
        assert!(
            human_export_label(&contradictory_current_row).is_none(),
            "a present non-accept/edit decision must not fall through to a stale legacy accept"
        );
    }

    #[test]
    fn same_path_audio_replacement_cannot_inherit_reviewed_transcript_authority() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("same-path.wav");
        make_wav_file(&wav_path);
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "source-drift", &wav_path);

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for index in 0..16000i32 {
            writer.write_sample(((index % 401) - 200) as i16).unwrap();
        }
        writer.finalize().unwrap();

        let out = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["source-drift".to_string()],
            &AudioExportOptions { output_dir: out.to_string_lossy().to_string(), ..AudioExportOptions::default() },
        )
        .unwrap();
        assert_eq!((result.succeeded, result.failed), (0, 1));
        assert!(result.errors.iter().any(|error| error.contains("stored canonical PCM identity")));
        assert!(!out.join("same-path_source-drift.wav").exists());
        assert!(!out.join("metadata.jsonl").exists());
    }

    #[test]
    fn test_export_audio_segment() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        assert!(out_dir.join(&result.files[0]).exists());
    }

    #[test]
    fn test_export_skips_out_of_range_alignment_instead_of_whole_file() {
        // Round-16 (HIGH): a segment whose alignment window lies past the (re-encoded/shortened)
        // decoded buffer must be SKIPPED, not exported as the WHOLE source recording paired with its
        // short transcript/duration in metadata.csv — that is silent training-data corruption. The
        // 1-second source with a 5000 ms window start is out of range, so the segment must fail with a
        // clear error and produce NO clip and NO metadata.csv.
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("src.wav");
        make_wav_file(&wav_path); // 1 second @ 16 kHz = 16000 samples

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let meta = SegmentSourceMeta { source_start_ms: 5000, source_end_ms: 5300, chunk_index: 0, chunk_count: 1 };
        let seg = SpeechSegment {
            id: "oob1".to_string(),
            audio_path: wav_path.to_string_lossy().to_string(),
            raw_transcript: "short utterance".to_string(),
            duration_ms: 300,
            alignment_json: Some(meta.to_alignment_json()),
            verified: false,
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).unwrap();
        bind_test_audio_identity(&db, "oob1");
        db.record_human_decision("oob1", "accept", Some("short utterance"), None).unwrap();

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["oob1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 0, "an out-of-range segment must not be exported");
        assert_eq!(result.failed, 1);
        assert!(result.errors[0].contains("out of range"), "error must explain the skip: {:?}", result.errors);
        assert!(
            !out_dir.join("src_oob1.wav").exists(),
            "the whole source file must NOT be written as this segment's clip"
        );
        assert!(!out_dir.join("metadata.csv").exists(), "no metadata.csv when nothing was exported");
    }

    #[test]
    fn metadata_csv_reports_the_clamped_clip_duration_not_the_stored_one() {
        // A window that STARTS in range but ENDS past the decoded buffer is CLAMPED by
        // slice_for_export (not skipped): the clip on disk is shorter than the segment's stored
        // duration_ms. metadata.csv must describe the bytes on disk — the same invariant the HF
        // exporter enforces (export.rs clip_dur_ms) but that this path used to violate by writing
        // seg.duration_ms verbatim. Trigger: source re-encoded/shortened after import, or
        // relink_audio pointing the segment at a shorter file.
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("shortened.wav");
        make_wav_file(&wav_path); // 1 second @ 16 kHz = 16000 samples

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let meta = SegmentSourceMeta { source_start_ms: 0, source_end_ms: 5000, chunk_index: 0, chunk_count: 1 };
        let seg = SpeechSegment {
            id: "clamp1".to_string(),
            audio_path: wav_path.to_string_lossy().to_string(),
            raw_transcript: "clamped utterance".to_string(),
            duration_ms: 5000, // stored value claims 5 s; the decoded source only backs 1 s
            alignment_json: Some(meta.to_alignment_json()),
            verified: false,
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).unwrap();
        bind_test_audio_identity(&db, "clamp1");
        db.record_human_decision("clamp1", "accept", Some("clamped utterance"), None).unwrap();

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["clamp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();
        assert_eq!(result.succeeded, 1, "a clamped (start-in-range) window exports: {:?}", result.errors);

        let (rate, frames) = output_wav_info(&out_dir.join("shortened_clamp1.wav"));
        assert_eq!((rate, frames), (16000, 16000), "the clip on disk is the clamped 1-second window");

        let csv = fs::read_to_string(out_dir.join("metadata.csv")).unwrap();
        let row = csv.lines().nth(1).expect("one data row");
        assert!(
            row.contains(",1000,"),
            "metadata duration_ms must be the WRITTEN clip's 1000 ms, not the stored 5000: {row}"
        );
        assert!(!row.contains(",5000,"), "the stored duration the WAV does not back up must not appear: {row}");
    }

    #[test]
    fn test_export_writes_metadata_when_requested() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16_000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        assert!(result.files.contains(&"metadata.csv".to_string()));
        let metadata_path = out_dir.join("metadata.csv");
        assert!(metadata_path.exists());
        assert!(!metadata_path.with_extension("csv.tmp").exists());

        let mut reader = csv::Reader::from_path(metadata_path).unwrap();
        let headers = reader.headers().unwrap().clone();
        let row = reader.records().next().unwrap().unwrap();
        assert_eq!(metadata_value(&headers, &row, "file_name"), "test_exp1.wav");
        assert_eq!(metadata_value(&headers, &row, "segment_id"), "exp1");
        // Round-22 #2: the published source reference is the BASENAME only — never the curator's
        // absolute import path (which would leak the OS username + drive layout into a shared CSV).
        let src = metadata_value(&headers, &row, "source_audio_path");
        assert_eq!(src, "test.wav", "only the basename may be published");
        assert!(!src.contains('/') && !src.contains('\\'), "no directory separators may leak: {src}");
        assert_eq!(metadata_value(&headers, &row, "raw_transcript"), "hello");
        assert_eq!(metadata_value(&headers, &row, "verified"), "1");
        assert_eq!(metadata_value(&headers, &row, "export_sample_rate"), "16000");
        assert_eq!(metadata_value(&headers, &row, "export_format"), "wav");
        assert!(reader.records().next().is_none());
    }

    #[test]
    fn disabling_csv_still_writes_the_exact_machine_readable_label() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16_000,
                include_metadata: false,
            },
        )
        .unwrap();

        assert_eq!(result.files, vec!["test_exp1.wav", "metadata.jsonl", "SHA256SUMS"]);
        assert!(!out_dir.join("metadata.csv").exists());
        let line = fs::read_to_string(out_dir.join("metadata.jsonl")).unwrap();
        let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["effective_transcript"], "hello");
        assert_eq!(record["transcript_source"], "human_verified");
        assert!(record["review_revision"].as_i64().is_some());
    }

    #[test]
    fn test_export_audio_segment_flac() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Flac,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        let flac_file = out_dir.join(&result.files[0]);
        assert!(flac_file.exists());

        // Verify it can be decoded back to PCM using the app's decoder
        let (sample_rate, samples) = crate::audio::decode_to_pcm(&flac_file).unwrap();
        assert_eq!(sample_rate, 16000);
        assert!(!samples.is_empty());
    }

    #[test]
    fn reexport_swaps_one_complete_generation_and_cleans_private_siblings() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        fs::create_dir_all(&out_dir).unwrap();
        let first = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();
        let old_wav = first.files.iter().find(|name| name.ends_with(".wav")).unwrap().clone();
        assert!(out_dir.join(&old_wav).is_file());

        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Flac,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        assert_eq!(result.files, vec!["test_exp1.flac", "metadata.jsonl", "metadata.csv", "SHA256SUMS"]);
        assert!(!out_dir.join(old_wav).exists(), "the old generation must not leak into the replacement");
        assert!(out_dir.join("test_exp1.flac").is_file());
        verify_complete_audio_export_dir(&out_dir, Some(&result.files)).unwrap();
        let private_left = fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".cortex-reviewed-audio-"));
        assert!(!private_left, "staging/backup siblings should be promoted or removed");
    }

    #[test]
    fn disk_full_after_staged_clips_preserves_previous_generation_byte_for_byte() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);
        let out_dir = tmp.path().join("out");
        let options = AudioExportOptions {
            output_dir: out_dir.to_string_lossy().to_string(),
            format: AudioExportFormat::Wav,
            sample_rate: 16000,
            include_metadata: true,
        };
        export_audio_segments(&db, &["exp1".to_string()], &options).unwrap();
        let before = directory_bytes(&out_dir);

        let error =
            export_audio_segments_inner(&db, &["exp1".to_string()], &options, AudioExportFault::DiskFullAfterClips)
                .expect_err("simulated ENOSPC must abort before directory publication");
        assert!(error.to_string().contains("simulated disk full"));
        assert_eq!(directory_bytes(&out_dir), before, "the prior bundle must remain byte-identical");
        assert!(
            !fs::read_dir(tmp.path())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains(".cortex-reviewed-audio-")),
            "failed staging must be cleaned without touching the published directory"
        );
    }

    #[test]
    fn next_run_recovery_restores_previous_generation_after_kill_between_directory_renames() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);
        let out_dir = tmp.path().join("out");
        let options = AudioExportOptions {
            output_dir: out_dir.to_string_lossy().to_string(),
            format: AudioExportFormat::Wav,
            sample_rate: 16000,
            include_metadata: true,
        };
        export_audio_segments(&db, &["exp1".to_string()], &options).unwrap();
        let before = directory_bytes(&out_dir);

        // Exact hard-kill disk shape: old destination was moved aside, private new generation exists,
        // but the process died before the staging->destination rename.
        let backup_prefix = audio_export_sibling_prefix(&out_dir, AUDIO_EXPORT_BACKUP_TAG).unwrap();
        let staging_prefix = audio_export_sibling_prefix(&out_dir, AUDIO_EXPORT_STAGING_TAG).unwrap();
        let backup = tmp.path().join(format!("{backup_prefix}kill-fixture"));
        let staging = tmp.path().join(format!("{staging_prefix}kill-fixture"));
        fs::rename(&out_dir, &backup).unwrap();
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("partial.wav"), b"truncated new generation").unwrap();

        recover_interrupted_audio_export(&out_dir).unwrap();
        assert_eq!(directory_bytes(&out_dir), before, "the previous complete generation must be restored exactly");
        assert!(!backup.exists(), "restoration promotes the backup back to its canonical destination");
        assert!(!staging.exists(), "non-authoritative crash-left staging is swept after restoration");
        verify_complete_audio_export_dir(&out_dir, None).unwrap();
    }

    #[test]
    fn nonempty_unowned_destination_fails_closed_and_preserves_every_byte() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);
        let out_dir = tmp.path().join("out");
        fs::create_dir(&out_dir).unwrap();
        fs::write(out_dir.join("owner-notes.txt"), b"do not delete").unwrap();
        let before = directory_bytes(&out_dir);

        let error = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .expect_err("arbitrary non-empty directories are never replaceable export generations");
        assert!(error.to_string().contains("preserving it unchanged"));
        assert_eq!(directory_bytes(&out_dir), before);
    }

    #[test]
    fn export_writes_a_sha256sums_manifest_that_covers_the_clips_and_detects_tampering() {
        // Every multi-file export must ship an integrity manifest so a consumer can detect a corrupted,
        // truncated, or partially-copied WAV via `sha256sum -c`. Audio export was the lone one missing it.
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();
        assert!(result.files.contains(&"SHA256SUMS".to_string()), "manifest must be listed in the result");

        // The manifest lists the clip + metadata, sorted, and NEVER itself.
        let sums = fs::read_to_string(out_dir.join("SHA256SUMS")).unwrap();
        let entries: std::collections::HashMap<String, String> = sums
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let (hash, path) = l.split_once("  ").expect("each line is '<hex>  <path>'");
                (path.to_string(), hash.to_string())
            })
            .collect();
        assert!(entries.contains_key("test_exp1.wav"), "the exported clip must be in the manifest");
        assert!(entries.contains_key("metadata.jsonl"), "exact-label metadata must be in the manifest");
        assert!(entries.contains_key("metadata.csv"), "metadata.csv must be in the manifest");
        assert!(!entries.contains_key("SHA256SUMS"), "the manifest must not hash itself");

        // The recorded hash MATCHES the file on disk — a correct manifest verifies.
        use sha2::{Digest, Sha256};
        let sha_of = |name: &str| -> String {
            let bytes = fs::read(out_dir.join(name)).unwrap();
            Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        assert_eq!(entries["test_exp1.wav"], sha_of("test_exp1.wav"), "manifest hash matches the real clip");

        // Tampering is detectable: mutate the clip and the recorded hash no longer matches.
        fs::write(out_dir.join("test_exp1.wav"), b"corrupted bytes not the real wav").unwrap();
        assert_ne!(entries["test_exp1.wav"], sha_of("test_exp1.wav"), "a corrupted clip must fail verification");
    }

    #[test]
    fn test_export_rejects_upsampling_and_allows_downsampling() {
        // Round-25 #11: the source is decoded to 16 kHz, so a requested rate ABOVE 16000 would only
        // upsample a band-limited signal and write a header/metadata rate that overstates fidelity — it
        // is rejected. Downsampling (8000) is still allowed and writes the true requested rate.
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp", &wav_path);
        let out_dir = tmp.path().join("out");

        let upsample = export_audio_segments(
            &db,
            &["exp".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 24000,
                include_metadata: true,
            },
        );
        assert!(upsample.is_err(), "a >16 kHz export rate must be rejected (it would overstate fidelity)");

        let result = export_audio_segments(
            &db,
            &["exp".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 8000,
                include_metadata: true,
            },
        )
        .unwrap();
        assert_eq!(result.succeeded, 1);
        let (sample_rate, sample_count) = output_wav_info(&out_dir.join(&result.files[0]));
        assert_eq!(sample_rate, 8000);
        assert!(
            (7900..=8100).contains(&sample_count),
            "downsampled one-second clip should have about 8000 samples, got {sample_count}"
        );
    }

    #[test]
    fn test_export_nonexistent_segment() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let out_dir = TempDir::new().unwrap();
        let result = export_audio_segments(
            &db,
            &["nonexistent".to_string()],
            &AudioExportOptions { output_dir: out_dir.path().to_string_lossy().to_string(), ..Default::default() },
        )
        .unwrap();

        assert_eq!(result.failed, 1);
    }

    fn metadata_value<'a>(headers: &csv::StringRecord, row: &'a csv::StringRecord, name: &str) -> &'a str {
        let index = headers.iter().position(|header| header == name).unwrap();
        row.get(index).unwrap()
    }
}
