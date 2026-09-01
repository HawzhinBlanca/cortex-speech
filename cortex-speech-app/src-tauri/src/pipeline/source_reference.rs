//! Optional whole-file source-reference transcript generation and identity-bound cache reuse.

use super::{source_audio_identity, ProcessingPipeline, SourceAudioIdentity};
use crate::db::{Database, SourceTranscriptRecord};
use crate::error::{AppError, AppResult};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

const SOURCE_REFERENCE_STAGING_DIR: &str = ".private_audio_staging";

pub(super) struct PrivateSourceReferenceSnapshot {
    path: PathBuf,
    cleaned: bool,
}

impl PrivateSourceReferenceSnapshot {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn cleanup(mut self) -> AppResult<()> {
        match remove_private_source_snapshot(&self.path) {
            Ok(()) => {
                self.cleaned = true;
                Ok(())
            }
            Err(error) => Err(AppError::Other(format!(
                "Could not remove the private source-reference audio snapshot after use: {error}"
            ))),
        }
    }
}

impl Drop for PrivateSourceReferenceSnapshot {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        if let Err(error) = remove_private_source_snapshot(&self.path) {
            tracing::error!("Failed to remove private source-reference audio snapshot during cleanup: {}", error);
        }
    }
}

#[cfg_attr(windows, allow(clippy::permissions_set_readonly_false))]
fn remove_private_source_snapshot(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if let Ok(mut permissions) = std::fs::metadata(path).map(|metadata| metadata.permissions()) {
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
    }
    std::fs::remove_file(path)
}

fn source_snapshot_extension(path: &Path) -> &str {
    path.extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 10
                && extension.chars().all(|character| character.is_ascii_alphanumeric())
        })
        .unwrap_or("audio")
}

fn require_identity_match(
    path: &Path,
    expected: &SourceAudioIdentity,
    context: &str,
) -> AppResult<SourceAudioIdentity> {
    let actual =
        source_audio_identity(path).map_err(|error| AppError::Other(format!("Cannot verify {context}: {error}")))?;
    if actual.content_hash != expected.content_hash || actual.size_bytes != expected.size_bytes {
        return Err(AppError::Other(format!("{context} changed bytes; refusing unbound transcript evidence")));
    }
    Ok(actual)
}

pub(super) fn create_private_source_reference_snapshot(
    source_path: &Path,
    output_dir: &Path,
    expected: &SourceAudioIdentity,
) -> AppResult<PrivateSourceReferenceSnapshot> {
    let staging_dir = output_dir.join(SOURCE_REFERENCE_STAGING_DIR);
    std::fs::create_dir_all(&staging_dir).map_err(|error| {
        AppError::Other(format!("Cannot create the private source-reference staging directory: {error}"))
    })?;
    let snapshot_path = staging_dir.join(format!(
        "source-reference-{}.{}",
        uuid::Uuid::new_v4(),
        source_snapshot_extension(source_path)
    ));
    let mut snapshot_file = OpenOptions::new().write(true).create_new(true).open(&snapshot_path).map_err(|error| {
        AppError::Other(format!("Cannot create a private source-reference audio snapshot: {error}"))
    })?;
    let snapshot = PrivateSourceReferenceSnapshot { path: snapshot_path, cleaned: false };
    let preparation = (|| -> AppResult<()> {
        let mut source_file = File::open(source_path).map_err(|error| {
            AppError::Other(format!("Cannot open source audio for a private reference snapshot: {error}"))
        })?;
        io::copy(&mut source_file, &mut snapshot_file).map_err(|error| {
            AppError::Other(format!("Cannot copy source audio into a private reference snapshot: {error}"))
        })?;
        snapshot_file.sync_all().map_err(|error| {
            AppError::Other(format!("Cannot durably finish the private source-reference snapshot: {error}"))
        })?;
        drop(snapshot_file);

        require_identity_match(snapshot.path(), expected, "private source-reference snapshot")?;
        require_identity_match(source_path, expected, "source audio after private snapshot capture")?;
        let mut permissions = std::fs::metadata(snapshot.path())?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(snapshot.path(), permissions)?;
        Ok(())
    })();

    match preparation {
        Ok(()) => Ok(snapshot),
        Err(preparation_error) => match snapshot.cleanup() {
            Ok(()) => Err(preparation_error),
            Err(cleanup_error) => Err(cleanup_error),
        },
    }
}

pub(super) fn verify_private_source_reference_snapshot(
    snapshot: &PrivateSourceReferenceSnapshot,
    expected: &SourceAudioIdentity,
) -> AppResult<()> {
    require_identity_match(snapshot.path(), expected, "private source-reference snapshot after generation").map(|_| ())
}

pub(super) fn verified_post_generation_source_identity(
    path: &Path,
    pre_generation: Option<&SourceAudioIdentity>,
) -> AppResult<SourceAudioIdentity> {
    // The cloud helper reads the source asynchronously. Re-hash after it returns: reusing the
    // pre-generation hash would bind a transcript produced during a same-path file replacement to
    // bytes that no longer exist. A changed source is not retryable evidence and must never be
    // persisted as a cache hit for either version.
    let pre_generation = pre_generation.ok_or_else(|| {
        AppError::Other(format!(
            "Cannot establish audio identity before generating whole-file reference transcript for {}; refusing unbound transcript evidence",
            path.display()
        ))
    })?;
    let post_generation = source_audio_identity(path).map_err(|error| {
        AppError::Other(format!(
            "Cannot verify audio identity after generating whole-file reference transcript for {}: {error}",
            path.display()
        ))
    })?;
    if pre_generation.content_hash != post_generation.content_hash
        || pre_generation.size_bytes != post_generation.size_bytes
    {
        return Err(AppError::Other(format!(
            "Audio source changed while generating whole-file reference transcript for {}; refusing to persist mismatched transcript evidence",
            path.display()
        )));
    }
    Ok(post_generation)
}

fn generate_bound_source_reference_artifact<Generate, Persist>(
    source_path: &Path,
    output_dir: &Path,
    expected: &SourceAudioIdentity,
    generate: Generate,
    persist: Persist,
) -> AppResult<(crate::agentic::SourceTranscriptArtifact, SourceAudioIdentity)>
where
    Generate: FnOnce(&Path) -> Result<String, String>,
    Persist: FnOnce(&str) -> Result<crate::agentic::SourceTranscriptArtifact, String>,
{
    let snapshot = create_private_source_reference_snapshot(source_path, output_dir, expected)?;

    // Do not return early after generation. Both identities and explicit cleanup are mandatory even
    // when the external model failed, so a private input can never be left behind by an error path.
    let generated = generate(snapshot.path()).map_err(AppError::Other);
    let snapshot_identity = verify_private_source_reference_snapshot(&snapshot, expected);
    let source_identity = verified_post_generation_source_identity(source_path, Some(expected));
    let cleanup = snapshot.cleanup();

    // Cleanup failure has precedence and prevents both artifact and database persistence.
    cleanup?;
    snapshot_identity?;
    let source_identity = source_identity?;
    let transcript = generated?;
    let artifact = persist(&transcript).map_err(AppError::Other)?;
    Ok((artifact, source_identity))
}

impl ProcessingPipeline {
    fn source_transcript_dir(&self) -> Option<PathBuf> {
        Path::new(&self.db_path).parent().map(|dir| dir.join("source_transcripts"))
    }

    pub(super) fn source_reference_enabled(&self) -> bool {
        // Both the persisted choice and the revocable live consent must agree. Keep status reporting
        // on the exact same predicate as the upload gate so the UI never claims an external reference
        // is running when this import is champion-only.
        self.settings.jury_cloud_opt_in && self.consent.jury_cloud()
    }

    /// The Gemini key for the whole-file reference transcript, read from the ENCRYPTED store.
    ///
    /// NOT `settings.llm_api_key`, which is where this used to look. `AppSettings::load` deliberately
    /// CLEARS that field and rewrites settings.json, so a plaintext key never survives on disk (P0.3).
    /// The consequence was that the field is empty on every run after the one where it was typed, so
    /// `ensure_source_reference_transcripts` failed with "Gemini API key is required" no matter what the
    /// owner did — and `llm_api_key_configured` stayed true, so the UI reported a key that was gone.
    /// Measured 2026-08-10: an import of three files failed 3/3 on exactly that error while
    /// secrets.env held a working OpenRouter key and an EMPTY GEMINI_API_KEY.
    ///
    /// `secrets.env` via ApiKeys is where the jury and OpenRouter paths already look, so this makes the reference transcript agree with the rest of the
    /// crate instead of being the one caller reading a field that is guaranteed empty.
    ///
    /// The in-memory settings field is still honoured as a fallback: within the single session where
    /// the owner has just typed a key, it holds the value before any reload scrubs it, and refusing it
    /// there would be a surprising "I just entered it" failure.
    pub(super) fn jury_cloud_api_key(&self) -> AppResult<Option<String>> {
        let from_store = match Path::new(&self.db_path).parent() {
            Some(data_dir) => {
                crate::api_keys::ApiKeys::load(data_dir)
                    .map_err(|error| AppError::Other(format!("Could not load the encrypted API-key store: {error}")))?
                    .gemini
            }
            None => None,
        };
        Ok(from_store.or_else(|| {
            let typed = self.settings.llm_api_key.trim();
            (!typed.is_empty()).then(|| typed.to_string())
        }))
    }

    fn reusable_source_reference_record(
        &self,
        import_writes: &crate::stores::ImportWriteStore,
        existing: &SourceTranscriptRecord,
        current_identity: Option<&SourceAudioIdentity>,
    ) -> AppResult<Option<SourceTranscriptRecord>> {
        let Some(current_identity) = current_identity else {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the current audio file identity could not be verified",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        };
        let identity_matches = existing.audio_content_hash.as_deref() == Some(current_identity.content_hash.as_str())
            && existing.audio_size_bytes == Some(current_identity.size_bytes);
        if !identity_matches {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the stored audio identity does not match the current file",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        }

        if !crate::agentic::is_usable_source_reference_transcript(&existing.transcript_text) {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because the stored DB text is empty or unusable",
                existing.audio_path,
                existing.model_id
            );
            return Ok(None);
        }

        let transcript_path = Path::new(&existing.transcript_path);
        let saved_text = match std::fs::read_to_string(transcript_path) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(
                    "Ignoring cached whole-file reference transcript for {} with {} because '{}' could not be read: {}",
                    existing.audio_path,
                    existing.model_id,
                    existing.transcript_path,
                    error
                );
                return Ok(None);
            }
        };
        if !crate::agentic::is_usable_source_reference_transcript(&saved_text) {
            tracing::warn!(
                "Ignoring cached whole-file reference transcript for {} with {} because '{}' is empty or unusable",
                existing.audio_path,
                existing.model_id,
                existing.transcript_path
            );
            return Ok(None);
        }

        let saved_text = saved_text.trim().to_string();
        if saved_text == existing.transcript_text.trim() {
            return Ok(Some(existing.clone()));
        }

        let synced = SourceTranscriptRecord {
            transcript_text: saved_text,
            created_at: existing.created_at.clone(),
            ..existing.clone()
        };
        import_writes.upsert_source_transcript(&synced)?;
        tracing::info!(
            "Synced cached whole-file reference transcript for {} with {} from edited text file '{}'",
            existing.audio_path,
            existing.model_id,
            existing.transcript_path
        );
        Ok(Some(synced))
    }

    pub(super) fn ensure_source_reference_transcripts(
        &self,
        path: &Path,
        db: &Database,
    ) -> AppResult<Vec<SourceTranscriptRecord>> {
        // Snapshot AND live consent: a withdrawal after this import began must stop the upload.
        if !self.source_reference_enabled() {
            return Ok(Vec::new());
        }
        let import_writes = self.import_write_store(db.path())?;
        let Some(api_key) = self.jury_cloud_api_key()? else {
            return Err(AppError::Other(
                "Gemini API key is required for whole-file reference transcript when jury cloud opt-in \
                 is enabled. Save it from Settings (it goes to secrets.env, DPAPI-encrypted); note that \
                 settings.json is NOT a place a key can live - AppSettings::load scrubs it by design."
                    .to_string(),
            ));
        };

        let audio_path = path.to_string_lossy().to_string();
        let output_dir = self
            .source_transcript_dir()
            .ok_or_else(|| AppError::Other("Cannot resolve app data directory for source transcripts".into()))?;
        let current_identity = source_audio_identity(path).map_err(|error| {
            AppError::Other(format!(
                "Cannot establish audio identity before generating or reusing whole-file reference transcript for {}: {error}",
                path.display()
            ))
        })?;
        let mut records = Vec::new();

        for model in self.settings.source_reference_models() {
            require_identity_match(path, &current_identity, "source audio before source-reference model")?;
            if let Some(existing) = db.get_source_transcript(&audio_path, &model)? {
                if let Some(existing) =
                    self.reusable_source_reference_record(&import_writes, &existing, Some(&current_identity))?
                {
                    tracing::info!(
                        "Reusing whole-file reference transcript for {} from {}",
                        path.display(),
                        existing.transcript_path
                    );
                    records.push(existing);
                    continue;
                }
            }

            let generated = generate_bound_source_reference_artifact(
                path,
                &output_dir,
                &current_identity,
                |snapshot_path| {
                    crate::agentic::generate_whole_file_reference_text_from_input(snapshot_path, path, &model, &api_key)
                },
                |transcript| {
                    crate::agentic::persist_whole_file_reference_transcript(path, &model, transcript, &output_dir)
                },
            );
            let (artifact, identity) = match generated {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        "Whole-file reference transcript failed for {} with {}: {}",
                        path.display(),
                        model,
                        error
                    );
                    let scope = if records.is_empty() { "All" } else { "Some" };
                    return Err(AppError::Other(format!(
                        "{scope} whole-file reference transcript models failed before chunking; refusing to continue with incomplete source-reference evidence: {model}: {error}"
                    )));
                }
            };
            let record = SourceTranscriptRecord {
                audio_path: artifact.audio_path,
                model_id: artifact.model_id,
                audio_content_hash: Some(identity.content_hash),
                audio_size_bytes: Some(identity.size_bytes),
                transcript_path: artifact.transcript_path,
                transcript_text: artifact.transcript_text,
                created_at: None,
            };
            import_writes.upsert_source_transcript(&record)?;
            records.push(record);
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use tempfile::TempDir;

    fn staging_entries(output_dir: &Path) -> Vec<PathBuf> {
        let staging = output_dir.join(SOURCE_REFERENCE_STAGING_DIR);
        match std::fs::read_dir(staging) {
            Ok(entries) => entries.map(|entry| entry.expect("staging entry").path()).collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read staging directory: {error}"),
        }
    }

    /// Undoes the read-only bit `create_private_source_reference_snapshot` sets, so a test can tamper
    /// with or remove the snapshot. Same shape as `remove_private_source_snapshot` above: the lint's
    /// world-writable hazard is Unix-only, and the Unix branch here sets an explicit owner-only mode,
    /// so the allow is scoped to the Windows branch that genuinely needs `set_readonly(false)`.
    #[cfg_attr(windows, allow(clippy::permissions_set_readonly_false))]
    fn make_writable(path: &Path) {
        let mut permissions = std::fs::metadata(path).expect("snapshot metadata").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o644);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions).expect("make snapshot writable");
    }

    fn artifact_for(
        source: &Path,
        model: &str,
        transcript: &str,
        output_dir: &Path,
    ) -> crate::agentic::SourceTranscriptArtifact {
        crate::agentic::SourceTranscriptArtifact {
            audio_path: source.to_string_lossy().to_string(),
            model_id: model.to_string(),
            transcript_path: output_dir.join("reference.txt").to_string_lossy().to_string(),
            transcript_text: transcript.to_string(),
        }
    }

    #[test]
    fn private_source_snapshots_are_unique_read_only_identity_bound_and_explicitly_cleaned() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("owner source.wav");
        let output = temp.path().join("transcripts");
        std::fs::write(&source, b"immutable-source-bytes").expect("source");
        let expected = source_audio_identity(&source).expect("identity");

        let first = create_private_source_reference_snapshot(&source, &output, &expected).expect("first snapshot");
        let second = create_private_source_reference_snapshot(&source, &output, &expected).expect("second snapshot");
        let first_path = first.path().to_path_buf();
        let second_path = second.path().to_path_buf();

        assert_ne!(first_path, second_path, "create_new snapshots must never share an attempt path");
        assert_eq!(std::fs::read(&first_path).expect("first bytes"), b"immutable-source-bytes");
        assert_eq!(std::fs::read(&second_path).expect("second bytes"), b"immutable-source-bytes");
        assert!(std::fs::metadata(&first_path).expect("first metadata").permissions().readonly());
        assert!(std::fs::metadata(&second_path).expect("second metadata").permissions().readonly());
        verify_private_source_reference_snapshot(&first, &expected).expect("first verification");
        verify_private_source_reference_snapshot(&second, &expected).expect("second verification");

        first.cleanup().expect("first cleanup");
        second.cleanup().expect("second cleanup");
        assert!(!first_path.exists());
        assert!(!second_path.exists());
        assert!(staging_entries(&output).is_empty());
    }

    #[test]
    fn snapshot_creation_rejects_pre_capture_source_drift_and_removes_partial_snapshot() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source.wav");
        let output = temp.path().join("transcripts");
        std::fs::write(&source, b"source-v1").expect("source v1");
        let expected = source_audio_identity(&source).expect("identity");
        std::fs::write(&source, b"source-v2-is-different").expect("source v2");

        let error = create_private_source_reference_snapshot(&source, &output, &expected)
            .err()
            .expect("drift must fail")
            .to_string();

        assert!(error.contains("changed bytes"), "unexpected error: {error}");
        assert!(staging_entries(&output).is_empty(), "failed capture must explicitly remove its private file");
    }

    #[test]
    fn bound_generation_persists_only_after_snapshot_cleanup() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source.wav");
        let output = temp.path().join("transcripts");
        std::fs::write(&source, b"source-v1").expect("source");
        let expected = source_audio_identity(&source).expect("identity");
        let observed_snapshot = RefCell::new(None::<PathBuf>);
        let persisted = Cell::new(false);

        let (artifact, identity) = generate_bound_source_reference_artifact(
            &source,
            &output,
            &expected,
            |snapshot_path| {
                assert!(snapshot_path.exists(), "generator must receive the live private snapshot");
                assert_ne!(snapshot_path, source);
                observed_snapshot.replace(Some(snapshot_path.to_path_buf()));
                Ok("reference text".to_string())
            },
            |transcript| {
                let snapshot_path = observed_snapshot.borrow();
                assert!(!snapshot_path.as_ref().expect("observed snapshot").exists());
                persisted.set(true);
                Ok(artifact_for(&source, "model", transcript, &output))
            },
        )
        .expect("bound generation");

        assert!(persisted.get());
        assert_eq!(identity.content_hash, expected.content_hash);
        assert_eq!(artifact.audio_path, source.to_string_lossy());
        assert!(!artifact.transcript_path.contains(SOURCE_REFERENCE_STAGING_DIR));
        assert!(staging_entries(&output).is_empty());
    }

    #[test]
    fn snapshot_tampering_stops_before_artifact_persistence_and_cleans_snapshot() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source.wav");
        let output = temp.path().join("transcripts");
        std::fs::write(&source, b"source-v1").expect("source");
        let expected = source_audio_identity(&source).expect("identity");
        let persisted = Cell::new(false);

        let error = generate_bound_source_reference_artifact(
            &source,
            &output,
            &expected,
            |snapshot_path| {
                make_writable(snapshot_path);
                std::fs::write(snapshot_path, b"tampered-private-snapshot").expect("tamper snapshot");
                Ok("must not persist".to_string())
            },
            |transcript| {
                persisted.set(true);
                Ok(artifact_for(&source, "model", transcript, &output))
            },
        )
        .expect_err("snapshot tampering must fail")
        .to_string();

        assert!(error.contains("private source-reference snapshot after generation changed bytes"));
        assert!(!persisted.get());
        assert!(staging_entries(&output).is_empty());
    }

    #[test]
    fn original_source_tampering_stops_before_artifact_persistence_and_cleans_snapshot() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source.wav");
        let output = temp.path().join("transcripts");
        std::fs::write(&source, b"source-v1").expect("source");
        let expected = source_audio_identity(&source).expect("identity");
        let persisted = Cell::new(false);

        let error = generate_bound_source_reference_artifact(
            &source,
            &output,
            &expected,
            |_snapshot_path| {
                std::fs::write(&source, b"source-v2-is-different").expect("tamper source");
                Ok("must not persist".to_string())
            },
            |transcript| {
                persisted.set(true);
                Ok(artifact_for(&source, "model", transcript, &output))
            },
        )
        .expect_err("source tampering must fail")
        .to_string();

        assert!(error.contains("Audio source changed while generating whole-file reference transcript"));
        assert!(!persisted.get());
        assert!(staging_entries(&output).is_empty());
    }

    #[test]
    fn cleanup_failure_has_precedence_and_private_path_is_not_exposed_or_persisted() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source.wav");
        let output = temp.path().join("transcripts");
        std::fs::write(&source, b"source-v1").expect("source");
        let expected = source_audio_identity(&source).expect("identity");
        let swapped_path = RefCell::new(None::<PathBuf>);
        let persisted = Cell::new(false);

        let error = generate_bound_source_reference_artifact(
            &source,
            &output,
            &expected,
            |snapshot_path| {
                let path = snapshot_path.to_path_buf();
                make_writable(&path);
                std::fs::remove_file(&path).expect("remove snapshot for namespace swap");
                std::fs::create_dir(&path).expect("replace snapshot with directory");
                swapped_path.replace(Some(path));
                Ok("must not persist".to_string())
            },
            |transcript| {
                persisted.set(true);
                Ok(artifact_for(&source, "model", transcript, &output))
            },
        )
        .expect_err("cleanup failure must hard-stop")
        .to_string();

        let private_path = swapped_path.borrow().as_ref().expect("swapped private path").to_string_lossy().to_string();
        assert!(error.contains("Could not remove the private source-reference audio snapshot after use"));
        assert!(!error.contains(&private_path), "cleanup error must not expose the staging path: {error}");
        assert!(!persisted.get());
        std::fs::remove_dir(swapped_path.borrow().as_ref().expect("swapped path")).expect("test cleanup directory");
        assert!(staging_entries(&output).is_empty());
    }
}
