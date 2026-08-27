//! Versioned backup, maintenance and recovery IPC.
//!
//! Recovery is an owner-critical boundary: the renderer needs stable outcomes and actionable
//! error codes, never raw SQLite errors, private paths, snapshot internals or ad-hoc JSON.

use super::*;

use crate::ipc_contract::{CommandErrorV1, SuggestedActionV1};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackupVerificationV1 {
    pub integrity_ok: bool,
    pub segment_count: i64,
}

impl From<crate::backup_service::BackupVerification> for BackupVerificationV1 {
    fn from(value: crate::backup_service::BackupVerification) -> Self {
        Self { integrity_ok: value.integrity_ok, segment_count: value.segment_count }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineNoticeV1 {
    /// A count is sufficient for the warning surface. Local quarantine filenames remain private.
    pub quarantined_file_count: usize,
    pub snapshot_count: usize,
    pub newest_snapshot_segments: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfoV1 {
    /// Opaque selector returned to `restore_db_from_snapshot`; never an arbitrary filesystem path.
    pub name: String,
    pub timestamp: u64,
    pub db_size_bytes: u64,
    pub segment_count: Option<i64>,
}

impl From<crate::snapshot::SnapshotInfo> for SnapshotInfoV1 {
    fn from(value: crate::snapshot::SnapshotInfo) -> Self {
        Self {
            name: value.name,
            timestamp: value.timestamp,
            db_size_bytes: value.db_size_bytes,
            segment_count: value.segment_count,
        }
    }
}

#[derive(Clone, Copy)]
enum RecoveryOperation {
    Backup,
    RestoreBackup,
    Vacuum,
    ReadState,
    ArchiveQuarantine,
    RestoreSnapshot,
}

fn recovery_rate_limited_error() -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", "Recovery tools are busy. Wait a moment, then retry.", true)
        .suggested(SuggestedActionV1::Retry)
}

fn invalid_recovery_request(code: &str, message: &str) -> CommandErrorV1 {
    // The public action enum has no file/snapshot chooser. Do not send the owner to Health for an
    // input they can repair only by choosing another source or destination.
    CommandErrorV1::new(code, message, false)
}

fn recovery_state_unavailable_error() -> CommandErrorV1 {
    CommandErrorV1::new(
        "RECOVERY_STATE_UNAVAILABLE",
        "Recovery state is unavailable. Open Health before attempting recovery again.",
        false,
    )
    .suggested(SuggestedActionV1::OpenHealth)
}

fn public_recovery_failure(operation: RecoveryOperation, private_detail: &str) -> CommandErrorV1 {
    let normalized = private_detail.to_ascii_lowercase();
    if private_detail == RESTORE_IN_PROGRESS_MSG || normalized.contains("restore is already in progress") {
        return CommandErrorV1::new(
            "RESTORE_IN_PROGRESS",
            "Database recovery is already in progress. Wait for it to finish, then retry.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }
    if normalized.contains("background write is in progress")
        || normalized.contains("database or configuration mutation is already in progress")
    {
        return CommandErrorV1::new(
            "RESTORE_BLOCKED_BY_ACTIVE_WORK",
            "Active work must finish before recovery. Cancel it or wait for it to finish, then retry.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }
    if normalized.contains("database is busy")
        || normalized.contains("database is locked")
        || normalized.contains("active database writer")
    {
        return CommandErrorV1::new(
            "DATABASE_BUSY",
            "The library is busy. Wait for active work to finish, then retry.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }

    let (code, message, retryable) = match operation {
        RecoveryOperation::Backup => {
            ("BACKUP_FAILED", "The backup could not be created and verified. The live library was not replaced.", true)
        }
        RecoveryOperation::RestoreBackup => (
            "BACKUP_RESTORE_FAILED",
            "The selected backup could not be safely restored. The current library remains protected.",
            false,
        ),
        RecoveryOperation::Vacuum => (
            "DATABASE_MAINTENANCE_FAILED",
            "Library maintenance could not finish. No recovery source was applied.",
            true,
        ),
        RecoveryOperation::ReadState => (
            "RECOVERY_READ_FAILED",
            "Recovery information could not be read. Retry; if it continues, open Health.",
            true,
        ),
        RecoveryOperation::ArchiveQuarantine => (
            "QUARANTINE_ARCHIVE_FAILED",
            "The quarantined database files could not be archived. They remain preserved.",
            true,
        ),
        RecoveryOperation::RestoreSnapshot => (
            "SNAPSHOT_RESTORE_FAILED",
            "The selected snapshot could not be safely restored. The current library remains protected.",
            false,
        ),
    };
    CommandErrorV1::new(code, message, retryable).suggested(if retryable {
        SuggestedActionV1::Retry
    } else {
        SuggestedActionV1::OpenHealth
    })
}

fn prepare_restore(state: &State<'_, AppState>) -> Result<(RestoreReservation<'static>, std::path::PathBuf), String> {
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| {
        "Database restore refused: the app data directory is unavailable, so a mandatory pre-restore safety snapshot cannot be created."
            .to_string()
    })?;
    prepare_restore_admission(data_dir, || state.writers_active())
}

#[tauri::command]
#[specta::specta]
pub async fn db_backup(dest: String, state: State<'_, AppState>) -> Result<BackupVerificationV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("db_backup").map_err(|_| recovery_rate_limited_error())?;
    let validated = validate::validate_output_path(&dest).map_err(|_| {
        invalid_recovery_request(
            "INVALID_BACKUP_DESTINATION",
            "Choose a valid local backup destination outside the active library.",
        )
    })?;
    // One bounded, restore-gated read snapshot keeps a slow external-drive backup away from the
    // serialized writer while binding it to one restore generation.
    let database = state.db_runtime();
    let verified = run_blocking(move || {
        let backup_db = database.open_read().map_err(|error| error.to_string())?;
        backup_db.backup(&validated).map_err(|error| error.to_string())?;
        crate::backup_service::verify_backup_file(Path::new(&validated)).map_err(String::from)
    })
    .await
    .map_err(|error| public_recovery_failure(RecoveryOperation::Backup, &error))?;
    Ok(verified.into())
}

#[tauri::command]
#[specta::specta]
pub async fn db_restore(src: String, state: State<'_, AppState>) -> Result<(), CommandErrorV1> {
    STRICT_RATE_LIMITER.check("db_restore").map_err(|_| recovery_rate_limited_error())?;
    let validated = validate::validate_file_path(&src).map_err(|_| {
        invalid_recovery_request("INVALID_BACKUP_SOURCE", "Choose a readable local Cortex backup database.")
    })?;
    let (restore_reservation, data_dir) =
        prepare_restore(&state).map_err(|error| public_recovery_failure(RecoveryOperation::RestoreBackup, &error))?;
    refuse_bare_restore_during_controlled_pilot(&data_dir)
        .map_err(|error| public_recovery_failure(RecoveryOperation::RestoreBackup, &error))?;
    let database = state.db_runtime();
    let history = state.history_arc_for_restore();
    let restore_reservation = run_blocking(move || {
        database.with_restore_writer(&restore_reservation, |writer| {
            restore_with_mandatory_snapshot(&restore_reservation, writer, &data_dir, Path::new(&validated))?;
            Ok(())
        })?;
        // A cancelled Tauri future detaches spawn_blocking. Clear stale undo history in the same
        // worker after publication and before the reservation can be dropped.
        history.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
        Ok(restore_reservation)
    })
    .await
    .map_err(|error| public_recovery_failure(RecoveryOperation::RestoreBackup, &error))?;
    drop(restore_reservation);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn acknowledge_quarantine(state: State<'_, AppState>) -> Result<usize, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("acknowledge_quarantine").map_err(|_| recovery_rate_limited_error())?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(recovery_state_unavailable_error)?;
    crate::snapshot::acknowledge_quarantine(&data_dir)
        .map_err(|error| public_recovery_failure(RecoveryOperation::ArchiveQuarantine, &error.to_string()))
}

#[tauri::command]
#[specta::specta]
pub async fn db_vacuum(state: State<'_, AppState>) -> Result<(), CommandErrorV1> {
    STRICT_RATE_LIMITER.check("db_vacuum").map_err(|_| recovery_rate_limited_error())?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        db.vacuum().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| public_recovery_failure(RecoveryOperation::Vacuum, &error))
}

#[tauri::command]
#[specta::specta]
pub fn get_quarantine_notice(state: State<'_, AppState>) -> Result<QuarantineNoticeV1, CommandErrorV1> {
    RATE_LIMITER.check("get_quarantine_notice").map_err(|_| recovery_rate_limited_error())?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(recovery_state_unavailable_error)?;
    let quarantined_file_count = std::fs::read_dir(&data_dir)
        .map_err(|error| public_recovery_failure(RecoveryOperation::ReadState, &error.to_string()))?
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.contains(".corrupt.") && !name.ends_with("-wal") && !name.ends_with("-shm")
        })
        .count();
    let snapshots = crate::snapshot::list_snapshots(&data_dir);
    Ok(QuarantineNoticeV1 {
        quarantined_file_count,
        snapshot_count: snapshots.len(),
        newest_snapshot_segments: snapshots.first().and_then(|snapshot| snapshot.segment_count),
    })
}

#[tauri::command]
#[specta::specta]
pub fn list_db_snapshots(state: State<'_, AppState>) -> Result<Vec<SnapshotInfoV1>, CommandErrorV1> {
    RATE_LIMITER.check("list_db_snapshots").map_err(|_| recovery_rate_limited_error())?;
    let data_dir = state.lock_data_dir().clone().ok_or_else(recovery_state_unavailable_error)?;
    Ok(crate::snapshot::list_snapshots(&data_dir).into_iter().map(SnapshotInfoV1::from).collect())
}

#[tauri::command]
#[specta::specta]
pub async fn restore_db_from_snapshot(name: String, state: State<'_, AppState>) -> Result<(), CommandErrorV1> {
    STRICT_RATE_LIMITER.check("restore_db_from_snapshot").map_err(|_| recovery_rate_limited_error())?;
    if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err(invalid_recovery_request("INVALID_SNAPSHOT_SELECTOR", "Choose a snapshot from the recovery list."));
    }
    restore_db_from_snapshot_inner(name, state)
        .await
        .map_err(|error| public_recovery_failure(RecoveryOperation::RestoreSnapshot, &error))
}

async fn restore_db_from_snapshot_inner(name: String, state: State<'_, AppState>) -> Result<(), String> {
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let snap_dir = crate::snapshot::resolve_snapshot_dir(&data_dir, &name)?;
    let src = snap_dir.join("cortex-speech.db");
    let source_metadata = std::fs::symlink_metadata(&src)
        .map_err(|error| format!("snapshot '{name}' has no readable database file: {error}"))?;
    if !source_metadata.file_type().is_file() || source_metadata.file_type().is_symlink() {
        return Err(format!("snapshot '{name}' has no database file"));
    }
    let (restore_reservation, restore_data_dir) = prepare_restore(&state)?;
    if let Some(pending) = load_named_restore_pending(&data_dir)? {
        if let Some(completed_selector) = pending.completed_selector.as_deref() {
            if completed_selector != name {
                return Err(format!(
                    "restore '{}' already completed and only its barrier cleanup remains; refusing selector '{}'",
                    completed_selector, name
                ));
            }
            clear_review_pilot_restore_pending(&data_dir)?;
            restore_reservation.commit_named_restore()?;
            tracing::info!("completed pending restore-barrier cleanup for auto-snapshot {name}");
            return Ok(());
        }
    }
    let (restore_plan, restore_reservation) = {
        let database = state.db_runtime();
        let restore_src = src.clone();
        let restore_snapshot_dir = snap_dir.clone();
        let restore_selector = name.clone();
        run_blocking(move || {
            let restore_plan = database.with_restore_writer(&restore_reservation, |writer| {
                prepare_and_restore_named_transaction(
                    &restore_reservation,
                    writer,
                    &restore_data_dir,
                    &restore_snapshot_dir,
                    &restore_src,
                    &restore_selector,
                )
            })?;
            Ok((restore_plan, restore_reservation))
        })
        .await
    }?;
    state.lock_history().clear();
    let live_controls = state.lock_settings().clone();
    let restored = install_snapshot_restore_plan(&restore_plan, &data_dir, &live_controls)?;
    *state.lock_settings() = restored.clone();
    state.update_pipeline_settings(restored);
    mark_named_restore_completed(&data_dir, &name)?;
    clear_review_pilot_restore_pending(&data_dir)?;
    restore_reservation.commit_named_restore()?;
    drop(restore_reservation);
    tracing::info!("database and config restored from auto-snapshot {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_recovery_contracts_are_camel_case_and_hide_private_details() {
        let backup = serde_json::to_value(BackupVerificationV1 { integrity_ok: true, segment_count: 42 })
            .expect("serialize backup result");
        assert_eq!(backup["integrityOk"], true);
        assert_eq!(backup["segmentCount"], 42);

        let notice = serde_json::to_value(QuarantineNoticeV1 {
            quarantined_file_count: 2,
            snapshot_count: 4,
            newest_snapshot_segments: Some(42),
        })
        .expect("serialize quarantine notice");
        assert_eq!(notice["quarantinedFileCount"], 2);
        assert!(notice.get("quarantinedFiles").is_none());

        let private = r#"SQL failed at C:\private\library.db; token=secret"#;
        for operation in [
            RecoveryOperation::Backup,
            RecoveryOperation::RestoreBackup,
            RecoveryOperation::Vacuum,
            RecoveryOperation::ReadState,
            RecoveryOperation::ArchiveQuarantine,
            RecoveryOperation::RestoreSnapshot,
        ] {
            let wire =
                serde_json::to_string(&public_recovery_failure(operation, private)).expect("serialize recovery error");
            assert!(!wire.contains("SQL"));
            assert!(!wire.contains("private"));
            assert!(!wire.contains("secret"));
            assert!(wire.contains("suggestedAction"));
        }
    }

    #[test]
    fn recovery_busy_and_restore_conflicts_are_retryable_without_raw_details() {
        let busy = public_recovery_failure(RecoveryOperation::Backup, "database is locked at C:\\private.db");
        assert_eq!(busy.code, "DATABASE_BUSY");
        assert!(busy.retryable);
        assert_eq!(busy.suggested_action, Some(SuggestedActionV1::Retry));

        let restore = public_recovery_failure(RecoveryOperation::RestoreSnapshot, RESTORE_IN_PROGRESS_MSG);
        assert_eq!(restore.code, "RESTORE_IN_PROGRESS");
        assert!(restore.retryable);

        for admission in [
            "A background write is in progress (import, batch, 7B refinement, jury, or the Couch Review server)",
            "A database or configuration mutation is already in progress — let it finish before restoring.",
        ] {
            let blocked = public_recovery_failure(RecoveryOperation::RestoreBackup, admission);
            assert_eq!(blocked.code, "RESTORE_BLOCKED_BY_ACTIVE_WORK");
            assert!(blocked.retryable);
            assert_eq!(blocked.suggested_action, Some(SuggestedActionV1::Retry));
        }

        let invalid = invalid_recovery_request("INVALID_BACKUP_SOURCE", "Choose another backup.");
        assert!(!invalid.retryable);
        assert_eq!(invalid.suggested_action, None);

        let read = public_recovery_failure(RecoveryOperation::ReadState, "temporary read error");
        assert!(read.retryable);
        assert_eq!(read.suggested_action, Some(SuggestedActionV1::Retry));

        let unavailable = recovery_state_unavailable_error();
        assert!(!unavailable.retryable);
        assert_eq!(unavailable.suggested_action, Some(SuggestedActionV1::OpenHealth));
    }
}
