//! Tauri-free restore admission and durable cross-generation marker authority.
//!
//! Restore commands adapt `AppState` into this module, but process-wide admission ordering,
//! fail-closed marker recovery and atomic marker publication belong here. Semantic database/config
//! validation is still being strangled out of `commands.rs` in later slices.

use crate::database_runtime::{RestoreAdmission, RestoreReservation, RESTORE_ADMISSION};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NamedRestorePending {
    pub(crate) schema: u32,
    pub(crate) source_selector: String,
    pub(crate) pre_restore_pin_selector: String,
    /// Written only after DB + every required config/settings file has committed. If marker cleanup
    /// then fails or the process crashes, startup clears the barrier without replaying or rolling
    /// back an already-coherent generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_selector: Option<String>,
}

pub(crate) const NAMED_RESTORE_PENDING_SCHEMA: u32 = 2;

pub(crate) fn atomic_write_restore_state(path: &Path, bytes: &[u8]) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            return Err(format!("restore destination {} must be a regular file or absent", path.display()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not inspect restore destination {}: {error}", path.display())),
    }
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("restore-state");
    let temp = path.with_file_name(format!(".{file_name}.restore-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    if let Err(error) = std::fs::write(&temp, bytes) {
        return Err(format!("could not stage {}: {error}", path.display()));
    }
    if let Err(error) = crate::atomic_file::replace_file(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not atomically install {}: {error}", path.display()));
    }
    Ok(())
}

pub(crate) fn remove_live_restore_state(destination: &Path) -> Result<(), String> {
    crate::atomic_file::recover_interrupted_replace(destination)
        .map_err(|error| format!("could not recover {} before explicit removal: {error}", destination.display()))?;
    // Remove recoverable backups FIRST while the canonical file still exists. If cleanup is blocked
    // by an antivirus/indexer lock, returning here leaves the old committed state intact; deleting
    // canonical first could let a leftover backup resurrect it after we had reported absence.
    crate::atomic_file::remove_replacement_backups(destination)
        .map_err(|error| format!("could not remove stale backups for {}: {error}", destination.display()))?;
    match std::fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not remove {}: {error}", destination.display())),
    }
    Ok(())
}

pub(crate) fn load_named_restore_pending(data_dir: &Path) -> Result<Option<NamedRestorePending>, String> {
    let pending = data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
    crate::atomic_file::recover_interrupted_replace(&pending)
        .map_err(|error| format!("could not recover the paid-review restore barrier: {error}"))?;
    let bytes = match std::fs::read(&pending) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read restore transaction {}: {error}", pending.display())),
    };
    let state: NamedRestorePending = serde_json::from_slice(&bytes).map_err(|error| {
        format!("restore transaction {} is invalid and paid review remains blocked: {error}", pending.display())
    })?;
    if !matches!(state.schema, 1 | NAMED_RESTORE_PENDING_SCHEMA) {
        return Err(format!("unsupported restore transaction schema {}", state.schema));
    }
    if state.schema == 1 && state.completed_selector.is_some() {
        return Err("legacy restore transaction cannot claim a completed generation".to_string());
    }
    if let Some(completed) = state.completed_selector.as_deref() {
        if completed != state.source_selector && completed != state.pre_restore_pin_selector {
            return Err("restore transaction completion selector is not its target or original pin".to_string());
        }
    }
    Ok(Some(state))
}

/// Conservatively decide whether dropping a restore command must keep the process-wide admission
/// fence parked. Invalid/unreadable marker state is recovery-required too: uncertainty can never be
/// interpreted as permission to resume writes.
pub(crate) fn named_restore_barrier_may_exist(data_dir: &Path) -> bool {
    match load_named_restore_pending(data_dir) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            tracing::error!("restore barrier is invalid or unreadable; keeping database fenced: {error}");
            true
        }
    }
}

pub(crate) fn write_named_restore_pending(data_dir: &Path, state: &NamedRestorePending) -> Result<(), String> {
    if let Some(existing) = load_named_restore_pending(data_dir)? {
        return (existing == *state).then_some(()).ok_or_else(|| {
            format!(
                "another interrupted restore transaction is pending for '{}'; retry that exact snapshot before selecting '{}'",
                existing.source_selector, state.source_selector
            )
        });
    }
    let mut bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("could not serialize restore transaction: {error}"))?;
    bytes.push(b'\n');
    atomic_write_restore_state(&data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), &bytes)
}

pub(crate) fn mark_named_restore_completed(data_dir: &Path, completed_selector: &str) -> Result<(), String> {
    let mut pending = load_named_restore_pending(data_dir)?
        .ok_or_else(|| "restore completion cannot be recorded because its durable marker is missing".to_string())?;
    if completed_selector != pending.source_selector && completed_selector != pending.pre_restore_pin_selector {
        return Err("restore completion selector is not the recorded target or original pin".to_string());
    }
    if let Some(existing) = pending.completed_selector.as_deref() {
        return (existing == completed_selector)
            .then_some(())
            .ok_or_else(|| format!("restore was already completed with a different generation '{existing}'"));
    }
    pending.schema = NAMED_RESTORE_PENDING_SCHEMA;
    pending.completed_selector = Some(completed_selector.to_string());
    let mut bytes = serde_json::to_vec_pretty(&pending)
        .map_err(|error| format!("could not serialize completed restore transaction: {error}"))?;
    bytes.push(b'\n');
    atomic_write_restore_state(&data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), &bytes)
}

pub(crate) fn clear_review_pilot_restore_pending(data_dir: &Path) -> Result<(), String> {
    let pending = data_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE);
    // Canonical marker removal is the FINAL commit point. Backups must disappear first; otherwise a
    // cleanup failure after canonical removal could let load-time atomic recovery resurrect a barrier
    // after the in-process admission guard had already been released.
    crate::atomic_file::remove_replacement_backups(&pending).map_err(|error| {
        format!("restore completed, but a stale paid-review restore-barrier backup could not be removed: {error}")
    })?;
    std::fs::remove_file(&pending).map_err(|error| {
        format!(
            "restore completed, but paid review remains fail-closed because {} could not be removed: {error}",
            pending.display()
        )
    })?;
    Ok(())
}

/// Reserve before probing active writers so the writer-side publish/recheck protocol closes the
/// check-then-register race. The callback keeps this ordering testable without importing AppState.
pub(crate) fn prepare_restore_admission(
    data_dir: PathBuf,
    writers_active: impl FnOnce() -> bool,
) -> Result<(RestoreReservation<'static>, PathBuf), String> {
    prepare_restore_admission_with(data_dir, RESTORE_ADMISSION.as_ref(), writers_active)
}

fn prepare_restore_admission_with<'a>(
    data_dir: PathBuf,
    admission: &'a RestoreAdmission,
    writers_active: impl FnOnce() -> bool,
) -> Result<(RestoreReservation<'a>, PathBuf), String> {
    let reservation = if named_restore_barrier_may_exist(&data_dir) {
        admission.claim_recovery()?
    } else {
        admission.try_reserve()?
    };
    if writers_active() {
        return Err(
            "A background write is in progress (import, batch, 7B refinement, jury, or the Couch Review server) — cancel it, let it finish, or stop Couch Review before restoring. Restoring mid-write would mix pre-restore rows into the restored library and re-arm stale undo history."
                .to_string(),
        );
    }
    Ok((reservation, data_dir))
}

pub(crate) fn refuse_bare_restore_during_controlled_pilot(data_dir: &Path) -> Result<(), String> {
    match crate::review_pilot::load(data_dir) {
        Ok(None) => match crate::couch::durable_controlled_pilot_state(data_dir) {
            Ok(false) => Ok(()),
            Ok(true) => Err(
                "Bare database restore is refused because the durable Couch session retains a controlled paid-review baseline. Use a policy-bearing named snapshot restore instead."
                    .to_string(),
            ),
            Err(error) => Err(format!(
                "Bare database restore is refused because durable Couch pilot state is not provably safe: {error}"
            )),
        },
        Ok(Some(_)) => Err(
            "Bare database restore is refused while a controlled paid-review pilot is active: its external baseline/policy would no longer match review_events. Use a policy-bearing named snapshot restore instead."
                .to_string(),
        ),
        Err(error) => Err(format!(
            "Bare database restore is refused because controlled paid-review state is not provably safe: {error}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_is_published_before_the_writer_fence_and_released_on_refusal() {
        let directory = tempfile::tempdir().unwrap();
        let admission = RestoreAdmission::new();
        let error = match prepare_restore_admission_with(directory.path().to_path_buf(), &admission, || {
            assert!(admission.is_pending(), "writer fence must observe the published reservation");
            true
        }) {
            Err(error) => error,
            Ok(_) => panic!("active writer must refuse restore admission"),
        };
        assert!(error.contains("background write is in progress"));
        assert!(!admission.is_pending(), "refusal must release an unarmed reservation");

        let (reservation, returned) =
            prepare_restore_admission_with(directory.path().to_path_buf(), &admission, || false).unwrap();
        assert_eq!(returned, directory.path());
        assert!(reservation.is_active());
        assert!(admission.is_pending());
        drop(reservation);
        assert!(!admission.is_pending());
    }

    #[test]
    fn durable_marker_round_trip_binds_one_target_and_completion_generation() {
        let directory = tempfile::tempdir().unwrap();
        let state = NamedRestorePending {
            schema: NAMED_RESTORE_PENDING_SCHEMA,
            source_selector: "rotating/snapshot-1".to_string(),
            pre_restore_pin_selector: "pinned/prerestore-1".to_string(),
            completed_selector: None,
        };
        write_named_restore_pending(directory.path(), &state).unwrap();
        assert_eq!(load_named_restore_pending(directory.path()).unwrap(), Some(state.clone()));
        assert!(named_restore_barrier_may_exist(directory.path()));

        let error = mark_named_restore_completed(directory.path(), "rotating/snapshot-2").unwrap_err();
        assert!(error.contains("not the recorded target"));
        mark_named_restore_completed(directory.path(), &state.source_selector).unwrap();
        let completed = load_named_restore_pending(directory.path()).unwrap().unwrap();
        assert_eq!(completed.completed_selector.as_deref(), Some(state.source_selector.as_str()));

        clear_review_pilot_restore_pending(directory.path()).unwrap();
        assert_eq!(load_named_restore_pending(directory.path()).unwrap(), None);
        assert!(!named_restore_barrier_may_exist(directory.path()));
    }
}
