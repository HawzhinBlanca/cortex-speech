//! Durable job, interrupted-import and tracked-export access.

use crate::database_runtime::{DatabaseRuntime, MutationGuard};
use crate::db::ImportJob;
use crate::error::{AppError, AppResult};
use crate::eval::{FinetunePackResult, GoldEvalExport};
use crate::export_audio::{AudioExportOptions, AudioExportResult};
use crate::export_bundle::BundleExportResult;
use crate::jobs::Job;
use crate::models::ModelManager;
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

    fn begin_mutation(&self) -> AppResult<MutationGuard<'_>> {
        self.runtime.begin_mutation().map_err(AppError::Other)
    }

    fn lock_after_mutation(
        &self,
        operation: &str,
        mutation: &MutationGuard<'_>,
    ) -> std::sync::MutexGuard<'_, crate::db::Database> {
        self.runtime.lock_after_mutation(mutation).unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during a job write");
            poisoned.into_inner()
        })
    }

    pub(crate) fn find_interrupted_import(&self) -> AppResult<Option<ImportJob>> {
        self.runtime.open_read()?.find_interrupted_import_job()
    }

    pub(crate) fn discard_interrupted_import(&self, job_id: &str) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("discard_interrupted_import", &mutation).discard_import_job(job_id)
    }

    pub(crate) fn begin_import(&self, directory: &str, total_files: usize) -> AppResult<String> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("begin_import", &mutation).begin_import_job(directory, total_files)
    }

    pub(crate) fn handoff_import_for_resume(&self, prior_job_id: &str) -> AppResult<String> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("handoff_import_for_resume", &mutation).handoff_import_job_for_resume(prior_job_id)
    }

    pub(crate) fn continue_import(&self, job_id: &str, directory: &str, total_files: usize) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("continue_import", &mutation).continue_import_job(job_id, directory, total_files)
    }

    pub(crate) fn mark_import_file_done(&self, job_id: &str, path: &str) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("mark_import_file_done", &mutation).mark_import_file_done(job_id, path)
    }

    pub(crate) fn complete_import(&self, job_id: &str) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("complete_import", &mutation).complete_import_job(job_id)
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
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation(kind, &mutation).run_tracked(job_id, kind, error_code, work)
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

    pub(crate) fn export_transcript(
        &self,
        job_id: &str,
        path: &Path,
        format: crate::transcript_export::TranscriptFormat,
    ) -> AppResult<()> {
        self.run_tracked(job_id, "export_transcript", "TRANSCRIPT_EXPORT_FAILED", |database| {
            crate::transcript_export::export_transcript(database, path, format)
        })
    }

    pub(crate) fn export_dataset_bundle(
        &self,
        job_id: &str,
        model_manager: &ModelManager,
        output_dir: &Path,
        settings: &AppSettings,
        production: bool,
        warning_threshold: usize,
    ) -> AppResult<BundleExportResult> {
        self.run_tracked(job_id, "export_dataset_bundle", "BUNDLE_EXPORT_FAILED", |database| {
            crate::export_bundle::export_dataset_bundle(
                database,
                model_manager,
                output_dir,
                settings,
                production,
                warning_threshold,
            )
        })
    }

    pub(crate) fn export_audio(
        &self,
        job_id: &str,
        segment_ids: &[String],
        options: &AudioExportOptions,
    ) -> AppResult<AudioExportResult> {
        self.run_tracked(job_id, "export_audio", "AUDIO_EXPORT_FAILED", |database| {
            crate::export_audio::export_audio_segments(database, segment_ids, options)
        })
    }

    pub(crate) fn export_gold_eval_set(&self, job_id: &str, output_dir: &Path) -> AppResult<GoldEvalExport> {
        self.run_tracked(job_id, "export_gold_eval_set", "GOLD_EVAL_EXPORT_FAILED", |database| {
            crate::eval::export_gold_eval_set(database, output_dir)
        })
    }

    pub(crate) fn export_finetune_pack(
        &self,
        job_id: &str,
        output_dir: &Path,
        corpus_ledger_path: Option<&Path>,
    ) -> AppResult<FinetunePackResult> {
        self.run_tracked(job_id, "export_finetune_pack", "FINETUNE_PACK_EXPORT_FAILED", |database| {
            crate::eval::export_finetune_pack(database, output_dir, corpus_ledger_path)
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
    fn import_journal_lifecycle_is_serialized_and_exact_through_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        let store = JobStore::new(DatabaseRuntime::new(database));

        let job_id = store.begin_import("C:/recordings", 2).unwrap();
        store.mark_import_file_done(&job_id, "C:/recordings/a.wav").unwrap();
        store.mark_import_file_done(&job_id, "C:/recordings/a.wav").unwrap();
        let running = store.find_interrupted_import().unwrap().expect("running import remains resumable");
        assert_eq!(running.id, job_id);
        assert_eq!(running.total_files, 2);
        assert_eq!(running.completed_paths, vec!["C:/recordings/a.wav"]);

        store.complete_import(&job_id).unwrap();
        assert!(store.find_interrupted_import().unwrap().is_none());
    }

    #[test]
    fn resume_handoff_and_worker_admission_stay_serialized_through_the_store() {
        let (_directory, store) = store_with_jobs();
        let crashed = store.find_interrupted_import().unwrap().unwrap();
        let successor = store.handoff_import_for_resume(&crashed.id).unwrap();

        let claimed = store.find_interrupted_import().unwrap().expect("successor is the sole resume authority");
        assert_eq!(claimed.id, successor);
        assert_eq!(claimed.completed_paths, crashed.completed_paths);

        store.continue_import(&successor, "C:/audio", 5).unwrap();
        let admitted = store.find_interrupted_import().unwrap().unwrap();
        assert_eq!(admitted.id, successor);
        assert_eq!(admitted.total_files, 5);
        assert!(store.continue_import(&successor, "C:/other", 5).is_err(), "directory drift must fail closed");
    }

    #[test]
    fn import_journal_progress_and_completion_failures_remain_visible_as_running() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal-fault.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_import_progress
                 BEFORE INSERT ON import_job_files
                 WHEN NEW.path LIKE '%blocked.wav'
                 BEGIN
                   SELECT RAISE(ABORT, 'injected progress failure');
                 END;
                 CREATE TRIGGER fail_import_completion
                 BEFORE UPDATE OF status ON import_jobs
                 WHEN NEW.status = 'completed'
                 BEGIN
                   SELECT RAISE(ABORT, 'injected completion failure');
                 END;",
            )
            .unwrap();
        let store = JobStore::new(DatabaseRuntime::new(database));

        let job_id = store.begin_import("C:/recordings", 1).unwrap();
        assert!(store.mark_import_file_done(&job_id, "C:/recordings/blocked.wav").is_err());
        let after_progress = store.find_interrupted_import().unwrap().expect("failed progress remains resumable");
        assert_eq!(after_progress.id, job_id);
        assert!(after_progress.completed_paths.is_empty());

        assert!(store.complete_import(&job_id).is_err());
        let after_completion = store.find_interrupted_import().unwrap().expect("failed completion remains resumable");
        assert_eq!(after_completion.id, job_id);
        assert!(after_completion.completed_paths.is_empty());
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

    #[test]
    fn transcript_export_publishes_output_and_terminal_job_through_the_store() {
        let (directory, store) = store_with_jobs();
        let output = directory.path().join("transcript.txt");

        store
            .export_transcript("tracked-transcript", &output, crate::transcript_export::TranscriptFormat::Txt)
            .unwrap();

        assert_eq!(std::fs::read_to_string(output).unwrap(), "");
        let job = store.runtime.open_read().unwrap().get_job("tracked-transcript").unwrap().unwrap();
        assert_eq!(job.state, crate::jobs::JobState::Succeeded);
        assert_eq!(job.kind, "export_transcript");
    }
}
