//! Backup verification kept below the command boundary.
//!
//! Commands validate user input and orchestrate services; they must not carry SQL or receive raw
//! database connections. This service reopens the artifact read-only and proves that the bytes
//! actually written are a structurally valid Cortex database before the command reports success.

use crate::db::Database;
use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackupVerification {
    pub integrity_ok: bool,
    pub segment_count: i64,
}

pub(crate) fn verify_backup_file(path: &Path) -> AppResult<BackupVerification> {
    let path_text =
        path.to_str().ok_or_else(|| AppError::Validation("backup path is not valid Unicode".to_string()))?;
    // FTS5's integrity probe performs temporary internal writes. Copy the exact on-disk artifact
    // through SQLite's online-backup API into a disposable writable connection; the source is still
    // opened read-only and can never be modified by verification.
    let database = Database::open_detached_read_snapshot(path_text).map_err(|error| {
        AppError::Other(format!("backup written but could not be opened for verification: {error}"))
    })?;

    let integrity = database
        .integrity_check()
        .map_err(|error| AppError::Other(format!("backup written but failed verification: {error}")))?;
    if integrity != "ok" {
        return Err(AppError::Other(format!("backup written but FAILED integrity check: {integrity}")));
    }

    let segment_count = database
        .segment_count()
        .map_err(|error| AppError::Other(format!("backup written but could not count segments: {error}")))?;

    Ok(BackupVerification { integrity_ok: true, segment_count })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_the_written_artifact_instead_of_the_source_database() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("backup.db");
        let database = Database::open(":memory:").unwrap();
        database.initialize().unwrap();
        database.backup(&destination).unwrap();

        assert_eq!(
            verify_backup_file(&destination).unwrap(),
            BackupVerification { integrity_ok: true, segment_count: 0 }
        );
    }

    #[test]
    fn corrupt_or_non_database_artifacts_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("not-a-backup.db");
        std::fs::write(&destination, b"not sqlite").unwrap();

        assert!(verify_backup_file(&destination).is_err());
    }
}
