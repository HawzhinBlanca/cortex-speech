//! Durable job and interrupted-import access.

use crate::database_runtime::DatabaseRuntime;
use crate::db::ImportJob;
use crate::error::AppResult;
use crate::jobs::Job;

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
        self.lock("discard_interrupted_import").discard_import_job(job_id)
    }

    pub(crate) fn list_recent(&self, limit: i64) -> AppResult<Vec<Job>> {
        self.runtime.open_read()?.list_recent_jobs(limit)
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
}
