//! Durable job, interrupted-import and tracked-export access.

use crate::database_runtime::{begin_mutation, DatabaseRuntime};
use crate::db::ImportJob;
use crate::error::{AppError, AppResult};
use crate::jobs::Job;
use crate::settings::{AppSettings, ExportFormat};
use std::path::Path;

#[derive(Clone)]
pub(crate) struct JobStore {
    runtime: DatabaseRuntime,
}

impl JobStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    fn lock(&self, operation: &str) -> std::sync::MutexGuard<'_, crate::db::Database> {
        self.runtime.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during a job write");
            poisoned.into_inner()
        })
    }

    pub(crate) fn find_interrupted_import(&self) -> AppResult<Option<ImportJob>> {
        self.runtime.open_read()?.find_interrupted_import_job()
    }

    pub(crate) fn discard_interrupted_import(&self, job_id: &str) -> AppResult<()> {
        let _mutation = begin_mutation().map_err(AppError::Other)?;
        self.lock("discard_interrupted_import").discard_import_job(job_id)
    }

    pub(crate) fn list_recent(&self, limit: i64) -> AppResult<Vec<Job>> {
        self.runtime.open_read()?.list_recent_jobs(limit)
    }

    fn run_tracked<T>(
        &self,
        job_id: &str,
        kind: &str,
        error_code: &str,
        work: impl FnOnce(&crate::db::Database) -> AppResult<T>,
    ) -> AppResult<T> {
        let _mutation = begin_mutation().map_err(AppError::Other)?;
        self.lock(kind).run_tracked(job_id, kind, error_code, work)
    }

    pub(crate) fn export_dataset(&self, job_id: &str, path: &Path, format: &ExportFormat) -> AppResult<()> {
        self.run_tracked(job_id, "export_dataset", "EXPORT_FAILED", |database| {
            crate::export::export_dataset(database, path, format)
        })
    }

    pub(crate) fn export_huggingface_dataset(
        &self,
        job_id: &str,
        path: &Path,
        settings: &AppSettings,
    ) -> AppResult<()> {
        self.run_tracked(job_id, "export_huggingface_dataset", "HF_EXPORT_FAILED", |database| {
            crate::export::export_huggingface_dataset(database, path, settings)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn store_with_jobs() -> (tempfile::TempDir, JobStore) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jobs.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        let interrupted = database.begin_import_job("C:/audio", 3).unwrap();
        database.mark_import_file_done(&interrupted, "C:/audio/a.wav").unwrap();
        database.create_or_get_job("export-job", "export", None, Some(4)).unwrap();
        (directory, JobStore::new(DatabaseRuntime::new(database)))
    }

    #[test]
    fn interrupted_import_is_read_from_a_bounded_snapshot_and_discarded_serially() {
        let (_directory, store) = store_with_jobs();
        let interrupted = store.find_interrupted_import().unwrap().expect("running import is resumable");
        assert_eq!(interrupted.dir, "C:/audio");
        assert_eq!(interrupted.completed_paths, vec!["C:/audio/a.wav"]);

        store.discard_interrupted_import(&interrupted.id).unwrap();
        assert!(store.find_interrupted_import().unwrap().is_none());
    }

    #[test]
    fn recent_jobs_are_bounded_and_newest_first_through_the_store() {
        let (_directory, store) = store_with_jobs();
        let rows = store.list_recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "export-job");
    }

    #[test]
    fn tracked_work_persists_exact_terminal_state_and_error_code() {
        let (_directory, store) = store_with_jobs();
        let value = store.run_tracked("tracked-ok", "test", "TEST_FAILED", |_database| Ok(42)).unwrap();
        assert_eq!(value, 42);
        let succeeded = store.runtime.open_read().unwrap().get_job("tracked-ok").unwrap().unwrap();
        assert_eq!(succeeded.state, crate::jobs::JobState::Succeeded);
        assert_eq!(succeeded.error_code, None);

        let error = store
            .run_tracked::<()>("tracked-failed", "test", "TEST_FAILED", |_database| {
                Err(AppError::Other("injected tracked failure".into()))
            })
            .unwrap_err();
        assert!(error.to_string().contains("injected tracked failure"));
        let failed = store.runtime.open_read().unwrap().get_job("tracked-failed").unwrap().unwrap();
        assert_eq!(failed.state, crate::jobs::JobState::Failed);
        assert_eq!(failed.error_code.as_deref(), Some("TEST_FAILED"));
    }

    #[test]
    fn dataset_export_publishes_output_and_terminal_job_through_the_store() {
        let (directory, store) = store_with_jobs();
        let output = directory.path().join("dataset.json");

        store.export_dataset("tracked-export", &output, &ExportFormat::Json).unwrap();

        assert!(output.is_file());
        let rows: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(output).unwrap()).unwrap();
        assert_eq!(rows["segments"], serde_json::json!([]));
        assert_eq!(rows["metadata"]["total_segments"], 0);
        let job = store.runtime.open_read().unwrap().get_job("tracked-export").unwrap().unwrap();
        assert_eq!(job.state, crate::jobs::JobState::Succeeded);
        assert_eq!(job.kind, "export_dataset");
    }
}
