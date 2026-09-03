//! Desktop ingest selection, resumable import orchestration and batch transcription commands.

use super::*;

use crate::ipc_contract::{
    public_agent_stage_progress, BatchOperationV1, BatchStartStatusV1, BatchStartedV1, CommandErrorV1,
    SuggestedActionV1,
};

const PICKER_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const PICKER_CANCEL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerWaitError {
    Cancelled,
    Closed,
    TimedOut,
}

async fn await_picker_response<T, Cancel, Deadline>(
    mut response: tokio::sync::oneshot::Receiver<T>,
    cancel: Cancel,
    deadline: Deadline,
) -> Result<T, PickerWaitError>
where
    Cancel: std::future::Future<Output = ()>,
    Deadline: std::future::Future<Output = ()>,
{
    tokio::pin!(cancel);
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        _ = &mut cancel => Err(PickerWaitError::Cancelled),
        value = &mut response => value.map_err(|_| PickerWaitError::Closed),
        _ = &mut deadline => Err(PickerWaitError::TimedOut),
    }
}

async fn wait_for_import_cancel(cancel: crate::CancellationToken) {
    loop {
        if cancel.is_cancelled() {
            return;
        }
        tokio::time::sleep(PICKER_CANCEL_POLL_INTERVAL).await;
    }
}

/// Renderer-safe view of one durable interrupted-import journal. The source directory and every
/// completed absolute path remain backend-only; the owner needs identity and progress to resume or
/// discard, not a copy of private filesystem history in the webview.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportJobV1 {
    pub id: String,
    pub total_files: usize,
    pub completed_count: usize,
    pub created_at: String,
}

impl From<crate::db::ImportJob> for ImportJobV1 {
    fn from(value: crate::db::ImportJob) -> Self {
        Self {
            id: value.id,
            total_files: value.total_files,
            completed_count: value.completed_paths.len(),
            created_at: value.created_at,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ImportResumeStatusV1 {
    Started,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportResumeV1 {
    pub status: ImportResumeStatusV1,
    pub resuming: bool,
    pub import_job_id: String,
    pub run_id: String,
}

/// Exact in-process admission truth used only to reconcile an import command whose response may
/// have been lost after the backend accepted it. It intentionally makes no claim about durable
/// transcript success; terminal import events and refreshed database reads remain authoritative for
/// that result.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ImportRunStatusV1 {
    Running,
    Settled,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportRunStatusResponseV1 {
    pub run_id: String,
    pub status: ImportRunStatusV1,
}

impl From<crate::ImportRunAdmission> for ImportRunStatusV1 {
    fn from(value: crate::ImportRunAdmission) -> Self {
        match value {
            crate::ImportRunAdmission::Running => Self::Running,
            crate::ImportRunAdmission::Settled => Self::Settled,
            crate::ImportRunAdmission::Rejected => Self::Rejected,
            crate::ImportRunAdmission::Unknown => Self::Unknown,
        }
    }
}

/// Releases a claimed import run as `rejected` on every pre-worker early return. This makes the
/// status query authoritative even when validation, recovery inspection, dialog handling, or OS
/// thread creation fails after the caller has committed to a run identity.
struct ClaimedImportStart<'a> {
    state: &'a AppState,
    run_id: &'a str,
    armed: bool,
}

impl<'a> ClaimedImportStart<'a> {
    fn new(state: &'a AppState, run_id: &'a str) -> Self {
        Self { state, run_id, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ClaimedImportStart<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.state.abort_import_start(self.run_id);
        }
    }
}

/// RAII ownership for the native file-picker cancellation slot. Async command cancellation,
/// timeout, callback-channel closure and normal selection all release the exact token they armed.
struct ClaimedFilePicker<'a> {
    state: &'a AppState,
    token: crate::CancellationToken,
}

impl Drop for ClaimedFilePicker<'_> {
    fn drop(&mut self) {
        self.state.finish_file_picker(&self.token);
    }
}

fn canonical_import_run_id(run_id: &str) -> Result<String, ()> {
    let canonical = uuid::Uuid::parse_str(run_id).map(|id| id.to_string()).map_err(|_| ())?;
    (canonical == run_id).then_some(canonical).ok_or(())
}

pub(super) fn canonical_batch_operation_id(operation_id: &str) -> Result<String, ()> {
    let canonical = uuid::Uuid::parse_str(operation_id).map(|id| id.to_string()).map_err(|_| ())?;
    (canonical == operation_id).then_some(canonical).ok_or(())
}

pub(super) fn validate_batch_segment_ids(ids: &[String]) -> Result<(), String> {
    if ids.is_empty() || ids.len() > 100_000 {
        return Err("INVALID_BATCH_SELECTION: select between one and 100,000 segments".into());
    }
    let mut unique = std::collections::HashSet::with_capacity(ids.len());
    for id in ids {
        validate::validate_identifier(id)?;
        if !unique.insert(id.as_str()) {
            return Err("INVALID_BATCH_SELECTION: duplicate segment identity".into());
        }
    }
    Ok(())
}

fn import_rate_limited_error() -> CommandErrorV1 {
    CommandErrorV1::new("RATE_LIMITED", "Import recovery is busy. Wait a moment, then retry.", true)
        .suggested(SuggestedActionV1::Retry)
}

fn import_not_ready_error() -> CommandErrorV1 {
    CommandErrorV1::new(
        crate::DEDUP_INDEX_UNAVAILABLE_CODE,
        "Audio import is disabled because duplicate protection could not be verified. Open Health before importing.",
        false,
    )
    .suggested(SuggestedActionV1::OpenHealth)
}

fn import_journal_read_error(_private_detail: &str) -> CommandErrorV1 {
    CommandErrorV1::new(
        "IMPORT_JOURNAL_READ_FAILED",
        "The interrupted import could not be read. Retry; if it continues, open Health.",
        true,
    )
    .suggested(SuggestedActionV1::Retry)
}

fn import_journal_write_error(_private_detail: &str) -> CommandErrorV1 {
    CommandErrorV1::new(
        "IMPORT_JOURNAL_UPDATE_FAILED",
        "The interrupted import could not be updated. Its durable journal remains preserved.",
        true,
    )
    .suggested(SuggestedActionV1::Retry)
}

fn invalid_import_job_id_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_IMPORT_JOB_ID", "The interrupted import identity is invalid.", false)
}

fn invalid_import_run_id_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_IMPORT_RUN_ID", "The import run identity is invalid.", false)
}

pub(super) fn invalid_batch_operation_id_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_BATCH_OPERATION_ID", "The batch operation identity is invalid.", false)
}

fn changed_import_job_error() -> CommandErrorV1 {
    CommandErrorV1::new(
        "IMPORT_JOB_CHANGED",
        "The interrupted import changed since it was shown. Its current durable state has been reloaded.",
        false,
    )
}

fn no_interrupted_import_error() -> CommandErrorV1 {
    CommandErrorV1::new("NO_INTERRUPTED_IMPORT", "There is no interrupted import to recover.", false)
}

fn public_import_start_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail == RESTORE_IN_PROGRESS_MSG {
        return CommandErrorV1::new(
            "RESTORE_IN_PROGRESS",
            "Import cannot start while database recovery is in progress. Wait for it to finish, then retry.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }
    if private_detail == "Import already in progress" {
        return CommandErrorV1::new(
            "IMPORT_IN_PROGRESS",
            "Another import is already running. Wait for it to finish or cancel it, then retry.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }
    if private_detail.contains(crate::DEDUP_INDEX_UNAVAILABLE_CODE) {
        return import_not_ready_error();
    }
    CommandErrorV1::new(
        "IMPORT_RESUME_FAILED",
        "The interrupted import could not be resumed. Its durable journal remains preserved.",
        true,
    )
    .suggested(SuggestedActionV1::Retry)
}

#[tauri::command]
#[specta::specta]
pub fn get_import_run_status(
    run_id: String,
    state: State<'_, AppState>,
) -> Result<ImportRunStatusResponseV1, CommandErrorV1> {
    RATE_LIMITER.check("get_import_run_status").map_err(|_| import_rate_limited_error())?;
    let run_id = canonical_import_run_id(&run_id).map_err(|_| invalid_import_run_id_error())?;
    Ok(ImportRunStatusResponseV1 { status: state.import_run_admission(&run_id).into(), run_id })
}

#[tauri::command]
#[specta::specta]
pub async fn open_audio_file(app: tauri::AppHandle) -> Result<Option<String>, CommandErrorV1> {
    RATE_LIMITER
        .check("open_audio_file")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("open_audio_file"))?;
    let state = app.state::<AppState>();
    let token = state.try_start_file_picker().map_err(|error| crate::ipc_contract::public_file_picker_error(&error))?;
    let _picker_claim = ClaimedFilePicker { state: &state, token: token.clone() };
    use tauri_plugin_dialog::DialogExt;
    // MUST be async + non-blocking: this command runs on the main thread, and blocking_pick_file
    // there freezes the ENTIRE app UI for as long as the picker is open (confirmed: a second
    // command hangs while the dialog is up). pick_file schedules the native dialog on the event
    // loop and delivers the result via a oneshot without ever blocking the main thread.
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Audio", &["wav", "mp3", "flac", "m4a", "ogg", "aac", "opus", "mp4", "webm", "wma", "mov"])
        .pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    let picked = await_picker_response(rx, wait_for_import_cancel(token), tokio::time::sleep(PICKER_RESPONSE_TIMEOUT))
        .await
        .map_err(|error| {
            tracing::warn!(?error, "Native file picker did not return normally");
            let code = match error {
                PickerWaitError::TimedOut => "E_FILE_PICKER_TIMEOUT",
                PickerWaitError::Closed => "E_FILE_PICKER_CLOSED",
                PickerWaitError::Cancelled => "E_FILE_PICKER_CANCELLED",
            };
            crate::ipc_contract::public_file_picker_error(code)
        })?;
    Ok(picked.and_then(|p| p.as_path().map(|p| p.to_string_lossy().to_string())))
}

pub(super) fn emit_or_log<R: tauri::Runtime, T>(app: &tauri::AppHandle<R>, event: &str, payload: T)
where
    T: serde::Serialize + Clone,
{
    if let Err(error) = app.emit(event, payload) {
        tracing::warn!("Failed to emit {event}: {error}");
    }
}

const IMPORT_PROCESSING_FAILED: &str = "IMPORT_PROCESSING_FAILED";
const IMPORT_ENRICHMENT_FAILED: &str = "IMPORT_ENRICHMENT_FAILED";

/// Reduce an owner-visible source identity to a bounded basename before it crosses into the
/// webview. Splitting both separator styles is intentional: journals can retain Windows paths even
/// when a fixture is inspected under another host. Control characters are never useful UI.
fn public_import_item_label(private_file: &str) -> String {
    crate::ipc_contract::public_file_label(private_file, "")
}

fn public_event_run_id(run_id: Option<&str>) -> String {
    run_id.and_then(|value| uuid::Uuid::parse_str(value).ok()).map(|value| value.to_string()).unwrap_or_default()
}

fn public_pipeline_error_payload(run_id: Option<&str>, private_file: &str, code: &'static str) -> serde_json::Value {
    serde_json::json!({
        "runId": public_event_run_id(run_id),
        "file": public_import_item_label(private_file),
        "code": code,
    })
}

/// Log private diagnostics only in the native log, then emit the closed public event shape. Raw
/// database/decoder errors and absolute paths must never hitchhike around the typed command layer
/// through an asynchronous event.
fn emit_public_pipeline_error<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run_id: Option<&str>,
    private_file: &str,
    private_error: &str,
    code: &'static str,
) {
    tracing::warn!(file = private_file, error = private_error, error_code = code, "Pipeline operation failed");
    emit_or_log(app, "pipeline-error", public_pipeline_error_payload(run_id, private_file, code));
}

fn emit_import_enrichment_complete<R: tauri::Runtime>(app: &tauri::AppHandle<R>, run_id: &str, segment_ids: &[String]) {
    emit_or_log(
        app,
        "import-enrichment-complete",
        serde_json::json!({
            "runId": public_event_run_id(Some(run_id)),
            "source": "file",
            "segmentCount": segment_ids.len(),
            "segmentIds": segment_ids,
        }),
    );
}

pub(super) fn send_audio_duration_probe_result(
    tx: std::sync::mpsc::Sender<crate::error::AppResult<i64>>,
    result: crate::error::AppResult<i64>,
) {
    if tx.send(result).is_err() {
        tracing::warn!("Audio duration probe worker could not send result; receiver was dropped or timed out");
    }
}

struct AgentStageEmission<'a> {
    stage: &'a str,
    status: &'a str,
    file: &'a str,
    detail: &'a str,
    current: usize,
    total: usize,
}

fn emit_agent_stage_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run_id: Option<&str>,
    source: &str,
    event: AgentStageEmission<'_>,
) {
    // Raw detail remains available to native diagnostics and (when correlated) the durable audit
    // record. It is deliberately not part of the renderer event below.
    tracing::debug!(
        run_id = run_id.unwrap_or(""),
        source,
        stage = event.stage,
        status = event.status,
        file = event.file,
        detail = event.detail,
        current = event.current,
        total = event.total,
        "Agent import stage changed"
    );
    if let Some(run_id) = run_id {
        if let Some(app_state) = app.try_state::<AppState>() {
            let database = app_state.db_runtime();
            match database.begin_mutation() {
                Ok(mutation) => {
                    let db = database.lock_after_mutation(&mutation).unwrap_or_else(|poisoned| {
                        tracing::warn!("Recovering poisoned database lock during agent-stage persistence");
                        poisoned.into_inner()
                    });
                    if let Err(error) = crate::runs::record_agent_stage_event(
                        &db,
                        run_id,
                        source,
                        event.stage,
                        event.status,
                        event.file,
                        event.detail,
                        event.current,
                        event.total,
                    ) {
                        tracing::warn!("Failed to persist agent stage event {run_id}/{}: {error}", event.stage);
                    }
                }
                Err(error) => {
                    tracing::warn!(run_id, stage = event.stage, %error, "Agent stage event refused during restore");
                }
            };
        }
    }

    emit_or_log(
        app,
        "pipeline-agent-stage",
        public_agent_stage_progress(
            run_id.unwrap_or_default(),
            event.stage,
            event.status,
            event.file,
            event.current,
            event.total,
        ),
    );
}

fn emit_pipeline_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: &PipelineEvent,
    run_id: Option<&str>,
    source: &str,
) {
    match event {
        PipelineEvent::Started { total } => {
            emit_or_log(
                app,
                "pipeline-started",
                serde_json::json!({ "runId": public_event_run_id(run_id), "total": total }),
            );
        }
        PipelineEvent::Phase { phase } => {
            emit_or_log(
                app,
                "pipeline-phase",
                serde_json::json!({ "runId": public_event_run_id(run_id), "phase": phase }),
            );
        }
        PipelineEvent::AgentStage { stage, status, file, detail, current, total } => {
            emit_agent_stage_event(
                app,
                run_id,
                source,
                AgentStageEmission { stage, status, file, detail, current: *current, total: *total },
            );
        }
        PipelineEvent::Progress { current, total, file, status } => {
            emit_or_log(
                app,
                "pipeline-progress",
                crate::ipc_contract::public_pipeline_progress(
                    &public_event_run_id(run_id),
                    *current,
                    *total,
                    file,
                    status,
                ),
            );
        }
        PipelineEvent::Completed { total, succeeded, failed } => {
            // Use the caller's source label, not a hardcoded "directory" — this same mapper handles
            // single-file imports (source "file"), where a stray source:"directory" completion would
            // mislabel the event the UI routes on.
            let payload = serde_json::json!({
                "total": total, "succeeded": succeeded, "failed": failed,
                "source": source, "runId": public_event_run_id(run_id),
            });
            emit_or_log(app, "pipeline-complete", payload.clone());
            emit_or_log(app, "import-complete", payload);
        }
        PipelineEvent::Error { file, error } => {
            emit_public_pipeline_error(app, run_id, file, error, IMPORT_PROCESSING_FAILED);
        }
    }
}

fn log_jury_pipeline_failure(context: &str, error: &str) {
    tracing::error!("Jury pipeline failed after {context}: {error}");
}

#[tauri::command]
#[specta::specta]
pub async fn import_directory(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<crate::ipc_contract::DirectoryImportStartedV1, CommandErrorV1> {
    let agent_run_id = canonical_import_run_id(&run_id).map_err(|_| invalid_import_run_id_error())?;
    let state = app.state::<AppState>();
    if RATE_LIMITER.check("import_directory").is_err() {
        state.remember_import_rejection(&agent_run_id);
        return Err(crate::ipc_contract::owner_critical_rate_limited("import_directory"));
    }
    // Claim the exact run BEFORE the native dialog. If the invoke response/channel is interrupted
    // while the owner is choosing a folder, get_import_run_status reports `running`, never an
    // ambiguous `unknown` that could make the renderer clear a still-pending command.
    state
        .try_start_import_for_run(&agent_run_id)
        .map_err(|error| crate::ipc_contract::public_import_start_error(&error))?;
    let mut claimed_start = ClaimedImportStart::new(&state, &agent_run_id);
    // Arm cancellation before opening the native picker. The renderer already exposes its global
    // Cancel control for this exact running ID, so a lost picker callback must remain stoppable.
    let cancel = state.start_cancel_token();
    use tauri_plugin_dialog::DialogExt;
    // async + non-blocking folder picker — blocking_pick_folder on this main-thread command froze
    // the whole UI while the picker was open (same footgun as open_audio_file).
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    let dir =
        await_picker_response(rx, wait_for_import_cancel(cancel.clone()), tokio::time::sleep(PICKER_RESPONSE_TIMEOUT))
            .await
            .map_err(|error| {
                tracing::warn!(run_id = %agent_run_id, ?error, "Native directory picker did not return normally");
                let code = match error {
                    PickerWaitError::Cancelled => "E_DIRECTORY_PICKER_CANCELLED",
                    PickerWaitError::TimedOut => "E_DIRECTORY_PICKER_TIMEOUT",
                    PickerWaitError::Closed => "E_DIRECTORY_PICKER_CLOSED",
                };
                crate::ipc_contract::public_directory_picker_error(code)
            })?;
    let dir_path = match dir.and_then(|p| p.as_path().map(|p| p.to_path_buf())) {
        Some(p) => p,
        None => return Err(crate::ipc_contract::public_directory_picker_error("E_DIRECTORY_PICKER_CANCELLED")),
    };
    if cancel.is_cancelled() {
        return Err(crate::ipc_contract::public_directory_picker_error("E_DIRECTORY_PICKER_CANCELLED"));
    }
    validate::validate_file_path(&dir_path.to_string_lossy())
        .map_err(|_| crate::ipc_contract::invalid_import_source_path_error())?;
    if cancel.is_cancelled() {
        return Err(crate::ipc_contract::public_directory_picker_error("E_DIRECTORY_PICKER_CANCELLED"));
    }
    let cancel = Some(cancel);

    let pipeline = state.lock_pipeline().clone();

    let app_clone = app.clone();
    let worker_agent_run_id = agent_run_id.clone();
    let worker = std::thread::Builder::new().name("cortex-import-directory".into()).spawn(move || {
        let agent_run_id = worker_agent_run_id;
        struct ImportGuard<R: tauri::Runtime> {
            app: tauri::AppHandle<R>,
            run_id: String,
        }
        impl<R: tauri::Runtime> Drop for ImportGuard<R> {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_import();
                    // Published only AFTER ImportState becomes Idle. The earlier import-complete
                    // event can reach the renderer before this drop runs; this settlement edge lets
                    // ambiguous/lost-response recovery reconcile without ever exposing a live job.
                    emit_or_log(
                        &self.app,
                        "import-worker-settled",
                        serde_json::json!({ "runId": self.run_id, "source": "directory" }),
                    );
                }
            }
        }
        let _guard = ImportGuard { app: app_clone.clone(), run_id: agent_run_id.clone() };

        // Panic-guard the directory worker (same rationale as the single-file path): an unwound
        // panic must not leave the import UI stuck "processing".
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.import_directory_with_agent_run_id(&dir_path, cancel, Some(&agent_run_id), None, None, |event| {
                emit_pipeline_event(&app_clone, &event, Some(&agent_run_id), "directory");
            })
        }));
        let result = match caught {
            Ok(r) => r,
            Err(_) => {
                Err(crate::error::AppError::Other("Import failed unexpectedly (internal error); see logs.".to_string()))
            }
        };

        if let Err(e) = result {
            let error = e.to_string();
            emit_public_pipeline_error(
                &app_clone,
                Some(&agent_run_id),
                &dir_path.to_string_lossy(),
                &error,
                IMPORT_PROCESSING_FAILED,
            );
            let payload = serde_json::json!({
                "runId": public_event_run_id(Some(&agent_run_id)),
                "total": 0, "succeeded": 0, "failed": 1, "cancelled": false, "source": "directory"
            });
            emit_or_log(&app_clone, "import-complete", payload.clone());
            emit_or_log(&app_clone, "pipeline-complete", payload);
        }
    });
    match worker {
        Ok(_) => claimed_start.disarm(),
        Err(error) => {
            tracing::warn!("Could not start directory import worker: {error}");
            return Err(crate::ipc_contract::import_worker_start_error());
        }
    }

    Ok(crate::ipc_contract::DirectoryImportStartedV1 {
        status: crate::ipc_contract::ImportStartStatusV1::Started,
        run_id,
    })
}

/// P3.2: the crashed directory import to resume, if any. Query at STARTUP — when no import is active,
/// a still-'running' job is a crash.
#[tauri::command]
#[specta::specta]
pub fn get_interrupted_import(state: State<'_, AppState>) -> Result<Option<ImportJobV1>, CommandErrorV1> {
    RATE_LIMITER.check("get_interrupted_import").map_err(|_| import_rate_limited_error())?;
    // A 'running' journal belongs to the live worker whenever the in-process gate is armed. Holding
    // this admission across the read also prevents a fresh import from entering between the state
    // check and the snapshot query. A caller may still receive a stale DTO if an import starts after
    // this function returns; exact discard/resume comparisons make that harmless.
    let Some(_admission) = state.try_import_recovery_admission() else {
        return Ok(None);
    };
    state
        .job_store()
        .find_interrupted_import()
        .map(|job| job.map(ImportJobV1::from))
        .map_err(|error| import_journal_read_error(&error.to_string()))
}

/// P3.2: discard an interrupted import job (the user chose not to resume).
#[tauri::command]
#[specta::specta]
pub fn discard_interrupted_import(job_id: String, state: State<'_, AppState>) -> Result<(), CommandErrorV1> {
    STRICT_RATE_LIMITER.check("discard_interrupted_import").map_err(|_| import_rate_limited_error())?;
    validate::validate_identifier(&job_id).map_err(|_| invalid_import_job_id_error())?;
    // Keep the same mutex that starts an import locked through the compare-and-delete. This is the
    // safety boundary for lost resume responses: the live successor can neither be advertised nor
    // deleted while its worker owns ImportState::Running.
    let Some(_admission) = state.try_import_recovery_admission() else {
        return Err(public_import_start_error("Import already in progress"));
    };
    match state
        .job_store()
        .discard_interrupted_import(&job_id)
        .map_err(|error| import_journal_write_error(&error.to_string()))?
    {
        crate::db::DiscardImportJobOutcome::Discarded => Ok(()),
        crate::db::DiscardImportJobOutcome::NotFound => Err(no_interrupted_import_error()),
        crate::db::DiscardImportJobOutcome::Changed => Err(changed_import_job_error()),
    }
}

/// P3.2: resume the interrupted directory import — re-run its folder, skipping files already imported
/// in the crashed run (their segments persisted per-file). Retires the old crashed job so it is not
/// offered again; the fresh import job now tracks progress.
#[tauri::command]
#[specta::specta]
pub fn resume_interrupted_import(
    job_id: String,
    run_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ImportResumeV1, CommandErrorV1> {
    resume_interrupted_import_on(job_id, run_id, app, state)
}

/// The command body, generic over the runtime so a test can drive the real worker through a mock
/// app handle; production monomorphizes to the desktop runtime through the wrapper above.
pub(super) fn resume_interrupted_import_on<R: tauri::Runtime>(
    job_id: String,
    run_id: String,
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<ImportResumeV1, CommandErrorV1> {
    let agent_run_id = canonical_import_run_id(&run_id).map_err(|_| invalid_import_run_id_error())?;
    if RATE_LIMITER.check("resume_interrupted_import").is_err() {
        state.remember_import_rejection(&agent_run_id);
        return Err(import_rate_limited_error());
    }
    if validate::validate_identifier(&job_id).is_err() {
        state.remember_import_rejection(&agent_run_id);
        return Err(invalid_import_job_id_error());
    }
    state.try_start_import_for_recovery_run(&agent_run_id).map_err(|error| public_import_start_error(&error))?;
    let mut claimed_start = ClaimedImportStart::new(&state, &agent_run_id);
    let job =
        state.job_store().find_interrupted_import().map_err(|error| import_journal_read_error(&error.to_string()))?;
    let Some(job) = job else {
        return Err(no_interrupted_import_error());
    };
    if job.id != job_id {
        return Err(changed_import_job_error());
    }
    let dir_path = std::path::PathBuf::from(&job.dir);
    if !dir_path.is_dir() {
        return Err(CommandErrorV1::new(
            "IMPORT_SOURCE_MISSING",
            "The interrupted import folder is no longer available. Discard this journal or restore the folder.",
            false,
        ));
    }
    let completed: std::collections::HashSet<String> = job.completed_paths.iter().cloned().collect();

    // Atomically hand the old journal to a successor BEFORE the worker is spawned. The transaction
    // copies every completed path and retires the old row in one commit, so a kill here leaves exactly
    // one resumable journal. On a handoff error, release the in-process single-flight claim while the
    // original durable journal remains untouched.
    let resume_job_id = match state.job_store().handoff_import_for_resume(&job.id) {
        Ok(job_id) => job_id,
        Err(error) => {
            tracing::warn!("Could not claim interrupted import journal for resume: {error}");
            return Err(import_journal_write_error(&error.to_string()));
        }
    };
    let cancel = Some(state.start_cancel_token());
    let pipeline = state.lock_pipeline().clone();
    let app_clone = app.clone();
    let worker_resume_job_id = resume_job_id.clone();
    let worker_agent_run_id = agent_run_id.clone();
    let worker = std::thread::Builder::new().name("cortex-import-resume".into()).spawn(move || {
        let agent_run_id = worker_agent_run_id;
        struct ImportGuard<R: tauri::Runtime> {
            app: tauri::AppHandle<R>,
            run_id: String,
        }
        impl<R: tauri::Runtime> Drop for ImportGuard<R> {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_import();
                    emit_or_log(
                        &self.app,
                        "import-worker-settled",
                        serde_json::json!({ "runId": self.run_id, "source": "directory" }),
                    );
                }
            }
        }
        let _guard = ImportGuard { app: app_clone.clone(), run_id: agent_run_id.clone() };
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.import_directory_with_agent_run_id(
                &dir_path,
                cancel,
                Some(&agent_run_id),
                Some(&completed),
                Some(&worker_resume_job_id),
                |event| emit_pipeline_event(&app_clone, &event, Some(&agent_run_id), "directory"),
            )
        }));
        let result = match caught {
            Ok(r) => r,
            Err(_) => {
                Err(crate::error::AppError::Other("Import failed unexpectedly (internal error); see logs.".to_string()))
            }
        };
        if let Err(e) = result {
            let error = e.to_string();
            emit_public_pipeline_error(
                &app_clone,
                Some(&agent_run_id),
                &dir_path.to_string_lossy(),
                &error,
                IMPORT_PROCESSING_FAILED,
            );
            let payload = serde_json::json!({
                "runId": public_event_run_id(Some(&agent_run_id)),
                "total": 0, "succeeded": 0, "failed": 1, "cancelled": false, "source": "directory"
            });
            emit_or_log(&app_clone, "import-complete", payload.clone());
            emit_or_log(&app_clone, "pipeline-complete", payload);
        }
    });
    match worker {
        Ok(_) => claimed_start.disarm(),
        Err(error) => {
            tracing::warn!("Could not start interrupted import worker for journal {resume_job_id}: {error}");
            return Err(public_import_start_error(&error.to_string()));
        }
    }
    Ok(ImportResumeV1 { status: ImportResumeStatusV1::Started, resuming: true, import_job_id: resume_job_id, run_id })
}

#[tauri::command]
#[specta::specta]
pub fn import_audio_file(
    path: String,
    run_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::FileImportStartedV1, CommandErrorV1> {
    import_audio_file_on(path, run_id, app, state)
}

/// The command body, generic over the runtime so a test can drive the real worker through a mock
/// app handle; production monomorphizes to the desktop runtime through the wrapper above.
pub(super) fn import_audio_file_on<R: tauri::Runtime>(
    path: String,
    run_id: String,
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::FileImportStartedV1, CommandErrorV1> {
    let agent_run_id = canonical_import_run_id(&run_id).map_err(|_| invalid_import_run_id_error())?;
    if RATE_LIMITER.check("import_audio_file").is_err() {
        state.remember_import_rejection(&agent_run_id);
        return Err(crate::ipc_contract::owner_critical_rate_limited("import_audio_file"));
    }
    state
        .try_start_import_for_run(&agent_run_id)
        .map_err(|error| crate::ipc_contract::public_import_start_error(&error))?;
    let mut claimed_start = ClaimedImportStart::new(&state, &agent_run_id);
    let validated = validate::validate_file_path(&path).map_err(|_| crate::ipc_contract::invalid_audio_path_error())?;
    let file_path = Path::new(&validated).to_path_buf();

    // NOTE: do NOT pre-emit pipeline-started/-phase here. The worker emits them via
    // PipelineEvent::Started/Phase (import_single_file_with_events), exactly like the directory path.
    // Pre-emitting fired pipeline-started twice -> two stacked "Pipeline started" toasts per open.

    let cancel = Some(state.start_cancel_token());

    let pipeline = state.lock_pipeline().clone();

    let app_clone = app.clone();
    let worker_agent_run_id = agent_run_id.clone();
    let worker = std::thread::Builder::new().name("cortex-import-file".into()).spawn(move || {
        let agent_run_id = worker_agent_run_id;
        struct ImportGuard<R: tauri::Runtime> {
            app: tauri::AppHandle<R>,
            run_id: String,
        }
        impl<R: tauri::Runtime> Drop for ImportGuard<R> {
            fn drop(&mut self) {
                if let Some(app_state) = self.app.try_state::<AppState>() {
                    app_state.finish_import();
                    emit_or_log(
                        &self.app,
                        "import-worker-settled",
                        serde_json::json!({ "runId": self.run_id, "source": "file" }),
                    );
                }
            }
        }
        let _guard = ImportGuard { app: app_clone.clone(), run_id: agent_run_id.clone() };

        // Guard the decode/VAD/ASR worker against panics (e.g. a pathological tensor inside
        // onnxruntime/sherpa-onnx). Without this, an unwound panic skips every terminal event
        // below and leaves the import progress UI stuck "processing" forever.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.import_single_file_with_events(&file_path, cancel, Some(&agent_run_id), |event| {
                // This command OWNS the terminal import-complete/pipeline-complete events: it emits
                // ready_payload right after segments exist (so the list renders immediately) and
                // done_payload after the background jury finishes. Drop the pipeline's own terminal
                // Completed event for the single-file path: emit_pipeline_event's Completed arm
                // hard-codes source:"directory" and emits import-complete + pipeline-complete —
                // forwarding it here would fire a SECOND, wrongly-sourced import-complete BEFORE the
                // jury adjudication block below, producing a spurious "Successfully processed 1 file"
                // toast, a premature idle/refresh, and an idle→adjudicating→complete flicker that tears
                // down the pipeline UI (clears the agent stages, flips to idle) before adjudication even
                // starts. The worker emits its own authoritative source:"file" import-complete +
                // pipeline-complete after adjudication, so the frontend still gets exactly one of each.
                // (The directory import path uses a different code path and keeps its Completed.)
                // Forward every other event unchanged.
                if matches!(event, PipelineEvent::Completed { .. }) {
                    return;
                }
                emit_pipeline_event(&app_clone, &event, Some(&agent_run_id), "file");
            })
        }));
        let result = match caught {
            Ok(r) => r,
            Err(_) => {
                let fname = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                emit_public_pipeline_error(
                    &app_clone,
                    Some(&agent_run_id),
                    fname,
                    "Import worker panicked; see native logs",
                    IMPORT_PROCESSING_FAILED,
                );
                let payload = serde_json::json!({
                    "runId": public_event_run_id(Some(&agent_run_id)),
                    "total": 1, "succeeded": 0, "failed": 1, "source": "file"
                });
                emit_or_log(&app_clone, "import-complete", payload.clone());
                emit_or_log(&app_clone, "pipeline-complete", payload);
                return;
            }
        };
        match result {
            Ok(segments) => {
                // This command — NOT the pipeline — runs the single-file post-import jury adjudication,
                // exactly ONCE, on the background thread below. The pipeline's single-file path
                // intentionally skips inline adjudication (see pipeline.rs import_single_file_with_events)
                // so the jury runs here on its OWN WAL connection and never holds the shared DB lock
                // across ASR — running it in both places would emit a duplicate adjudicating phase, a
                // contradictory second jury count, a second agent_import_reports row, and double the jury
                // work (and cloud T2 cost/latency under cloud opt-in). We emit the authoritative
                // source:"file" import-complete here (the pipeline's terminal Completed event is dropped
                // in the callback above).
                let segment_ids: Vec<String> = segments.iter().map(|s| s.id.clone()).collect();
                let source_paths = vec![file_path.to_string_lossy().to_string()];
                let post_import_file =
                    file_path.file_name().and_then(|n| n.to_str()).unwrap_or("post-import jury").to_string();
                let seg_count = segments.len();

                // Import is complete once VAD has produced segments — signal it NOW so the UI
                // renders the segment list immediately, then run the heavy, ASR-bearing jury
                // adjudication on a background thread. (Previously adjudication ran inline holding
                // the global DB lock across ASR, starving the UI's get_segments so the list never
                // rendered during import.) Adjudication enriches the segments and emits a refresh.
                let ready_payload = serde_json::json!({
                    "runId": public_event_run_id(Some(&agent_run_id)),
                    "total": 1, "succeeded": 1, "failed": 0,
                    "segmentCount": seg_count, "segmentIds": segment_ids.clone(), "source": "file",
                });
                emit_or_log(&app_clone, "import-complete", ready_payload.clone());
                emit_or_log(&app_clone, "pipeline-complete", ready_payload);

                let app_clone = app_clone.clone();
                let agent_run_id = agent_run_id.clone();
                let segment_ids = segment_ids.clone();
                std::thread::spawn(move || {
                    // Clones reserved for the panic path; the inner closure consumes the originals.
                    let panic_app = app_clone.clone();
                    let panic_run_id = agent_run_id.clone();
                    let panic_file = post_import_file.clone();
                    let panic_seg_count = seg_count;
                    // The jury can call into ASR / onnxruntime; guard against an unwind so a crash here
                    // leaves the adjudication stage settled (blocked + refresh) rather than stuck
                    // "running". Import already emitted its terminal events before this thread spawned.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        // The import fence is already down and this child uses a dedicated WAL
                        // connection. Register the entire adjudication/report mutation BEFORE its
                        // first stage-event write or DB open. A restore that won the same admission
                        // mutex makes this enrichment stop cleanly; otherwise restore sees the active
                        // mutation and refuses until the child finishes.
                        let _mutation = match begin_mutation() {
                            Ok(guard) => guard,
                            Err(error) => {
                                emit_public_pipeline_error(
                                    &app_clone,
                                    Some(&agent_run_id),
                                    &post_import_file,
                                    &error,
                                    IMPORT_ENRICHMENT_FAILED,
                                );
                                emit_import_enrichment_complete(&app_clone, &agent_run_id, &segment_ids);
                                return;
                            }
                        };
                        emit_or_log(
                            &app_clone,
                            "pipeline-phase",
                            serde_json::json!({
                                "runId": public_event_run_id(Some(&agent_run_id)),
                                "phase": "adjudicating"
                            }),
                        );
                        let adjudication_detail = format!("Adjudicating {} imported segment(s)", segment_ids.len());
                        emit_agent_stage_event(
                            &app_clone,
                            Some(&agent_run_id),
                            "file",
                            AgentStageEmission {
                                stage: "jury_adjudication",
                                status: "running",
                                file: &post_import_file,
                                detail: &adjudication_detail,
                                current: 0,
                                total: segment_ids.len(),
                            },
                        );
                        let adjudication_result = if let Some(app_state) = app_clone.try_state::<AppState>() {
                            let settings = app_state.lock_settings().clone();
                            // Snapshot agentic readiness the same way pipeline.rs and check_agentic_readiness do
                            // (model status + external-provider status -> build_agentic_readiness_snapshot), so the
                            // background-thread jury report carries an honest readiness state.
                            let agentic_readiness = {
                                let model_status = app_state.lock_model_manager().status();
                                let external_provider = external_provider_status(&settings);
                                build_agentic_readiness_snapshot(&settings, &model_status, &external_provider)
                            };
                            // Adjudication uses its OWN database connection (WAL mode) rather than the
                            // shared Mutex<Database> guard, so the heavy ASR-bearing jury never starves the
                            // UI's get_segments (which locks the shared connection) while it runs. WAL lets
                            // the UI read concurrently while the jury writes verdicts on its own connection.
                            let jury_conn = app_state
                                .data_dir
                                .lock()
                                .ok()
                                .and_then(|g| (*g).clone())
                                .map(|dir| dir.join("cortex-speech.db"))
                                .and_then(|p| crate::db::Database::open(p.to_string_lossy().as_ref()).ok());
                            let Some(db) = jury_conn else {
                                emit_agent_stage_event(
                                    &app_clone,
                                    Some(&agent_run_id),
                                    "file",
                                    AgentStageEmission {
                                        stage: "jury_adjudication",
                                        status: "blocked",
                                        file: &post_import_file,
                                        detail: "could not open a private DB connection for adjudication",
                                        current: 0,
                                        total: segment_ids.len(),
                                    },
                                );
                                // Still emit the terminal refresh — adjudication is best-effort and the
                                // import itself already succeeded, so the UI must not hang waiting for a
                                // completion event that would otherwise never fire on this early return.
                                emit_import_enrichment_complete(&app_clone, &agent_run_id, &segment_ids);
                                return;
                            };
                            // R3: this background adjudication thread writes verdicts + the import report on
                            // its OWN connection (opened above) AFTER import-complete fired — so the import
                            // fence is already down. Arm the jury writer fence for its lifetime so a restore
                            // cannot run while it writes into a possibly-just-restored library. (The
                            // run_jury_pipeline COMMAND guards its path; this direct-core caller needs its own.)
                            let _jury_writer = BgDbWriterGuard::new();
                            let mut report_options = crate::runs::AgentImportReportOptions::from_settings(&settings);
                            report_options.agent_run_id = Some(agent_run_id.clone());
                            report_options.agentic_readiness = Some(agentic_readiness);
                            let jury_data_dir = app_state.lock_data_dir().clone();
                            match run_jury_pipeline_core_via(
                                &db,
                                &settings,
                                segment_ids.clone(),
                                jury_data_dir.as_deref(),
                            ) {
                                Ok(jury_report) => {
                                    let completion_detail = format!(
                                        "Reference commits: {}; review queue: {}",
                                        jury_report["referenceCommitted"].as_u64().unwrap_or(0),
                                        jury_report["humanInbox"].as_u64().unwrap_or(0)
                                    );
                                    emit_agent_stage_event(
                                        &app_clone,
                                        Some(&agent_run_id),
                                        "file",
                                        AgentStageEmission {
                                            stage: "jury_adjudication",
                                            status: "completed",
                                            file: &post_import_file,
                                            detail: &completion_detail,
                                            current: segment_ids.len(),
                                            total: segment_ids.len(),
                                        },
                                    );
                                    crate::runs::record_agent_import_report_with_options(
                                        &db,
                                        "file",
                                        &source_paths,
                                        &segment_ids,
                                        Some(&jury_report),
                                        None,
                                        report_options,
                                    )
                                    .map(|_| {
                                        emit_agent_stage_event(
                                            &app_clone,
                                            Some(&agent_run_id),
                                            "file",
                                            AgentStageEmission {
                                                stage: "agent_report",
                                                status: "completed",
                                                file: "agent import report",
                                                detail: "Persisted auditable multi-agent import report",
                                                current: segment_ids.len(),
                                                total: segment_ids.len(),
                                            },
                                        );
                                    })
                                    .map_err(|error| {
                                        format!(
                                            "Agent import report persistence failed after single-file import: {error}"
                                        )
                                    })
                                }
                                Err(error) => {
                                    let mut message = format!(
                                        "Post-import jury adjudication failed after single-file import: {error}"
                                    );
                                    if let Err(report_error) = crate::runs::record_agent_import_report_with_options(
                                        &db,
                                        "file",
                                        &source_paths,
                                        &segment_ids,
                                        None,
                                        Some(&error),
                                        report_options,
                                    ) {
                                        message.push_str(&format!(
                                            "; additionally failed to persist agent import report: {report_error}"
                                        ));
                                    }
                                    emit_agent_stage_event(
                                        &app_clone,
                                        Some(&agent_run_id),
                                        "file",
                                        AgentStageEmission {
                                            stage: "jury_adjudication",
                                            status: "blocked",
                                            file: &post_import_file,
                                            detail: &message,
                                            current: 0,
                                            total: segment_ids.len(),
                                        },
                                    );
                                    Err(message)
                                }
                            }
                        } else {
                            let message = "App state unavailable for post-import jury adjudication".to_string();
                            emit_agent_stage_event(
                                &app_clone,
                                Some(&agent_run_id),
                                "file",
                                AgentStageEmission {
                                    stage: "jury_adjudication",
                                    status: "blocked",
                                    file: &post_import_file,
                                    detail: &message,
                                    current: 0,
                                    total: segment_ids.len(),
                                },
                            );
                            Err(message)
                        };
                        if let Err(error) = adjudication_result {
                            // Adjudication is best-effort enrichment; the import already succeeded and the
                            // UI already rendered the segments, so a failure here is a non-fatal notice
                            // (the jury_adjudication stage event already carries the detail) — NOT an
                            // import failure.
                            log_jury_pipeline_failure("single-file import", &error);
                            emit_public_pipeline_error(
                                &app_clone,
                                Some(&agent_run_id),
                                &post_import_file,
                                &error,
                                IMPORT_ENRICHMENT_FAILED,
                            );
                        }
                        // Refresh report/evidence only. Import truth already completed at the
                        // segments-ready edge above; replaying `import-complete` here could end a
                        // newer import and select the wrong clip.
                        emit_import_enrichment_complete(&app_clone, &agent_run_id, &segment_ids);
                    }));
                    if outcome.is_err() {
                        // Adjudication unwound (e.g. a panic deep in ASR/onnxruntime). Settle the stage
                        // and refresh so the UI never hangs on "adjudicating" — the import itself
                        // already succeeded and rendered; this enrichment is best-effort.
                        emit_agent_stage_event(
                            &panic_app,
                            Some(&panic_run_id),
                            "file",
                            AgentStageEmission {
                                stage: "jury_adjudication",
                                status: "blocked",
                                file: &panic_file,
                                detail: "post-import adjudication crashed unexpectedly (internal error); see logs",
                                current: 0,
                                total: panic_seg_count,
                            },
                        );
                        emit_import_enrichment_complete(&panic_app, &panic_run_id, &[]);
                    }
                });
            }
            Err(e) => {
                let fname = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                emit_public_pipeline_error(
                    &app_clone,
                    Some(&agent_run_id),
                    fname,
                    &e.to_string(),
                    IMPORT_PROCESSING_FAILED,
                );
                let payload = serde_json::json!({
                    "runId": public_event_run_id(Some(&agent_run_id)),
                    "total": 1,
                    "succeeded": 0,
                    "failed": 1,
                    "source": "file",
                });
                emit_or_log(&app_clone, "import-complete", payload.clone());
                emit_or_log(&app_clone, "pipeline-complete", payload);
            }
        }
    });
    match worker {
        Ok(_) => claimed_start.disarm(),
        Err(error) => {
            tracing::warn!("Could not start file import worker: {error}");
            return Err(crate::ipc_contract::import_worker_start_error());
        }
    }

    Ok(crate::ipc_contract::FileImportStartedV1 {
        status: crate::ipc_contract::ImportStartStatusV1::Started,
        source: crate::ipc_contract::ImportSourceV1::File,
        run_id,
    })
}

/// P0.2 — expose the git SHA baked into the running exe at build time so the frontend/e2e harness
/// (and a curious user, via the About panel) can confirm the running binary matches a given commit.
/// Referencing `crate::GIT_SHA` here also guarantees the const is retained in the compiled binary.
#[tauri::command]
#[specta::specta]
pub fn app_git_sha() -> Result<String, crate::ipc_contract::CommandErrorV1> {
    Ok(crate::GIT_SHA.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn app_health(
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::AppHealthV1, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("app_health").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new("RATE_LIMITED", "The health check is busy. Retry in a moment.", true)
            .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?;
    let data_dir = state.lock_data_dir().clone();
    let settings = state.lock_settings().clone();
    let db = state.lock_db();
    let mm = state.lock_model_manager();
    health::health_check(&db, &mm, &settings, data_dir.as_deref()).map(Into::into).map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "HEALTH_CHECK_FAILED",
            "The workspace health check could not be completed.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })
}

/// One clip to slice out of a source recording during a grouped decode. `end_ms == i64::MAX` means
/// "to the end of the file" — the whole-file case expressed as a span, so one walk covers both kinds.
pub(crate) struct ClipSpan {
    pub segment_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum BatchHaltCode {
    ChampionUnavailable,
    ChampionIdentityMismatch,
    TranscriptionSourceChanged,
    AudioDecodeFailed,
    BatchRefinementFailed,
    BatchTranscriptionFailed,
}

fn batch_halt_code_for_error(error: &crate::error::AppError) -> BatchHaltCode {
    use crate::error::{AppError, AudioError};
    match error {
        AppError::Audio(
            AudioError::UnsupportedCodec(_)
            | AudioError::Decode(_)
            | AudioError::Resample(_)
            | AudioError::NoTracks(_)
            | AudioError::EmptyBuffer,
        ) => BatchHaltCode::AudioDecodeFailed,
        AppError::Validation(message) if message.starts_with("E_TRANSCRIPTION_SOURCE_CHANGED:") => {
            BatchHaltCode::TranscriptionSourceChanged
        }
        AppError::Validation(message)
            if message.starts_with(crate::pipeline::ASR_7B_UNAVAILABLE_TAG)
                && (message.contains("does not match registry champion")
                    || message.contains("transcription reply identity")) =>
        {
            BatchHaltCode::ChampionIdentityMismatch
        }
        AppError::Validation(message) if message.starts_with(crate::pipeline::ASR_7B_UNAVAILABLE_TAG) => {
            BatchHaltCode::ChampionUnavailable
        }
        AppError::Other(message) if message.starts_with("LLM refinement failed") => {
            BatchHaltCode::BatchRefinementFailed
        }
        // These are primary recognizer/model failures. Calling them a refinement failure would send
        // the owner toward the wrong recovery path and misstate which stage produced no transcript.
        AppError::Asr(_) | AppError::Onnx(_) | AppError::ModelNotFound { .. } => {
            BatchHaltCode::BatchTranscriptionFailed
        }
        _ => BatchHaltCode::BatchTranscriptionFailed,
    }
}

fn batch_halt_error(code: BatchHaltCode) -> crate::ipc_contract::CommandErrorV1 {
    use crate::ipc_contract::SuggestedActionV1;
    let (wire_code, message, retryable, suggested_action) = match code {
        BatchHaltCode::ChampionUnavailable => {
            ("CHAMPION_UNAVAILABLE", "The champion engine is unavailable.", true, Some(SuggestedActionV1::OpenHealth))
        }
        BatchHaltCode::ChampionIdentityMismatch => (
            "CHAMPION_IDENTITY_MISMATCH",
            "The loaded engine is not the registered champion.",
            false,
            Some(SuggestedActionV1::OpenModels),
        ),
        BatchHaltCode::TranscriptionSourceChanged => (
            "TRANSCRIPTION_SOURCE_CHANGED",
            "The transcription source changed.",
            false,
            Some(SuggestedActionV1::ReloadClip),
        ),
        BatchHaltCode::AudioDecodeFailed => {
            ("AUDIO_DECODE_FAILED", "The audio could not be decoded.", false, Some(SuggestedActionV1::ReloadClip))
        }
        BatchHaltCode::BatchRefinementFailed => {
            ("BATCH_REFINEMENT_FAILED", "Transcript refinement failed.", true, Some(SuggestedActionV1::Retry))
        }
        BatchHaltCode::BatchTranscriptionFailed => {
            ("BATCH_TRANSCRIPTION_FAILED", "Batch transcription failed.", true, Some(SuggestedActionV1::Retry))
        }
    };
    let error = crate::ipc_contract::CommandErrorV1::new(wire_code, message, retryable);
    match suggested_action {
        Some(action) => error.suggested(action),
        None => error,
    }
}

fn batch_transcribe_admission_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains("BATCH_ADMISSION_CANCELLED") {
        return batch_start_commit_error(crate::BatchStartCommitError::Cancelled);
    }
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return CommandErrorV1::new(
            "RESTORE_IN_PROGRESS",
            "A database restore is in progress. Wait for it to finish, then retry.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }
    if private_detail.contains("restore generation changed") {
        return CommandErrorV1::new(
            "RESTORE_GENERATION_CHANGED",
            "The database changed during batch preparation. Retry from the current workspace.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }
    if private_detail.contains("already in progress") || private_detail.contains("one_live_batch") {
        return CommandErrorV1::new("BATCH_ALREADY_RUNNING", "Another batch operation is already running.", true)
            .suggested(SuggestedActionV1::Retry);
    }
    if private_detail.contains("does not exist") {
        return CommandErrorV1::new(
            "BATCH_SEGMENT_MISSING",
            "A selected segment no longer exists. Reload the library before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip);
    }
    CommandErrorV1::new(
        "BATCH_ADMISSION_FAILED",
        "The transcription batch could not be admitted durably. Open Health before retrying.",
        false,
    )
    .suggested(SuggestedActionV1::OpenHealth)
}

fn durable_transcription_failure_code(error: &crate::error::AppError) -> String {
    batch_halt_error(batch_halt_code_for_error(error)).code
}

fn batch_transcription_source_cache_cardinality(cache: &crate::pipeline::TranscriptionSourceLeaseCache) -> usize {
    cache
        .lock()
        .unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned bounded batch transcription source cache");
            poisoned.into_inner()
        })
        .len()
}

fn batch_transcription_source_cache_is_within_page_bound(
    cache: &crate::pipeline::TranscriptionSourceLeaseCache,
) -> bool {
    batch_transcription_source_cache_cardinality(cache) <= crate::db::BATCH_PENDING_PAGE_SIZE_V1
}

/// Start one exact-champion batch whose immutable request, before images, item outcomes, canonical
/// writes and undo authority all share the schema-68 journal. Inference is deliberately sequential
/// for the owner workstation: correctness, deterministic hard-stop behavior and bounded pressure
/// outrank speculative provider fan-out.
#[tauri::command]
#[specta::specta]
pub async fn batch_transcribe(
    ids: Vec<String>,
    operation_id: String,
    app: tauri::AppHandle,
) -> Result<BatchStartedV1, CommandErrorV1> {
    tokio::task::spawn_blocking(move || batch_transcribe_blocking(ids, operation_id, app)).await.map_err(|error| {
        tracing::error!(%error, "Transcription admission worker stopped unexpectedly");
        CommandErrorV1::new(
            "BATCH_START_WORKER_FAILED",
            "The transcription batch could not be started. Retry; if it continues, open Health.",
            true,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    })?
}

fn batch_transcribe_blocking<R: tauri::Runtime>(
    ids: Vec<String>,
    operation_id: String,
    app: tauri::AppHandle<R>,
) -> Result<BatchStartedV1, CommandErrorV1> {
    let state = app.state::<AppState>();
    let operation = crate::BatchOperation::Transcribe;
    let operation_id = canonical_batch_operation_id(&operation_id).map_err(|_| invalid_batch_operation_id_error())?;
    if STRICT_RATE_LIMITER.check("batch_transcribe").is_err() {
        state.remember_batch_rejection(&operation_id, operation);
        return Err(CommandErrorV1::new("RATE_LIMITED", "Too many batch requests. Wait a moment, then retry.", true)
            .suggested(SuggestedActionV1::Retry));
    }
    if let Err(error) = validate_batch_segment_ids(&ids) {
        state.remember_batch_rejection(&operation_id, operation);
        tracing::warn!(%error, "Rejected invalid transcription batch selection");
        return Err(CommandErrorV1::new(
            "INVALID_BATCH_SELECTION",
            "Select between one and 100,000 unique segments before transcribing.",
            false,
        ));
    }

    let total = ids.len();
    let cancel = state
        .try_start_batch_for_run(&operation_id, operation, total)
        .map_err(|error| batch_transcribe_admission_error(&error))?;
    let mut claimed_start = crate::ClaimedBatchStart::new(&state, &operation_id, operation);
    let restore_generation = crate::database_runtime::capture_restore_generation()
        .map_err(|error| batch_transcribe_admission_error(&error))?;
    if cancel.is_cancelled() {
        return Err(batch_start_commit_error(crate::BatchStartCommitError::Cancelled));
    }

    // The command may say "started" only after the exact registered champion and result-affecting
    // configuration have both been proved. A smaller diagnostic engine is never a batch fallback.
    let pipeline = state.lock_pipeline().clone();
    let preflight = pipeline.preflight_batch_champion();
    if cancel.is_cancelled() {
        return Err(batch_start_commit_error(crate::BatchStartCommitError::Cancelled));
    }
    if let Err(error) = preflight {
        tracing::warn!(%error, "Rejected transcription batch before champion admission");
        return Err(batch_halt_error(batch_halt_code_for_error(&error)));
    }
    let config_sha256 = pipeline.batch_transcription_config_sha256().map_err(|error| {
        tracing::error!(%error, "Transcription configuration could not be hashed");
        batch_transcribe_admission_error(&error.to_string())
    })?;

    let executor = new_batch_executor_identity();
    // Allocate every captured owner before durable admission. Once the journal exists, the unified
    // guard below is the sole authority that may reopen the process-local batch gate.
    let worker_app = app.clone();
    let worker_operation_id = operation_id.clone();
    let admission_commit =
        state.commit_batch_start(&operation_id, operation, &cancel).map_err(batch_start_commit_error)?;
    drop(admission_commit);
    let (lease, admitted) = state
        .batch_store()
        .admit(crate::stores::BatchAdmissionV1 {
            operation_id: &operation_id,
            kind: crate::db::BatchJobKindV1::Transcribe,
            segment_ids: &ids,
            config_sha256: &config_sha256,
            executor,
            cancel: cancel.as_atomic(),
            restore_generation,
        })
        .map_err(|error| batch_transcribe_admission_error(&error.to_string()))?;
    let mut worker = DurableBatchWorkerGuard::new(worker_app.clone(), worker_operation_id.clone(), operation, lease);
    if !state.mark_batch_durable_admitted(&operation_id, operation) {
        tracing::error!(%operation_id, "Transcription durable-admission phase lost exact start authority");
        claimed_start.disarm();
        worker
            .finish(crate::db::BatchTerminalIntentV1::Failed { code: "BATCH_START_AUTHORITY_LOST".into() })
            .map_err(|error| batch_transcribe_admission_error(&error.to_string()))?;
        drop(worker);
        return Err(batch_start_commit_error(crate::BatchStartCommitError::AuthorityLost));
    }
    // From here the unified guard owns both journal and process-gate settlement.
    claimed_start.disarm();
    if usize::try_from(admitted.total).ok() != Some(total) {
        tracing::error!(expected = total, admitted = admitted.total, "Durable batch admission count mismatch");
        worker
            .finish(crate::db::BatchTerminalIntentV1::Failed { code: "BATCH_EVIDENCE_INVALID".into() })
            .map_err(|error| batch_transcribe_admission_error(&error.to_string()))?;
        drop(worker);
        return Err(CommandErrorV1::new(
            "BATCH_EVIDENCE_INVALID",
            "The admitted batch evidence is inconsistent. Open Health before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth));
    }
    let start_commit = match state.commit_batch_start(&operation_id, operation, &cancel) {
        Ok(commit) => commit,
        Err(error) => {
            let intent = match error {
                crate::BatchStartCommitError::Cancelled => {
                    crate::db::BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() }
                }
                crate::BatchStartCommitError::AuthorityLost => {
                    crate::db::BatchTerminalIntentV1::Failed { code: "BATCH_START_AUTHORITY_LOST".into() }
                }
            };
            worker.finish(intent).map_err(|settle_error| {
                tracing::error!(%operation_id, %settle_error, "Admitted transcription could not settle before worker spawn");
                batch_transcribe_admission_error(&settle_error.to_string())
            })?;
            drop(worker);
            return Err(batch_start_commit_error(error));
        }
    };

    // Drop the final cancel-slot guard before `spawn`: an OS refusal drops the captured worker guard
    // synchronously, and its exact settlement must be able to clear the cancellation slot.
    drop(start_commit);
    let app_clone = worker_app;
    let spawn = std::thread::Builder::new().name("cortex-batch-transcribe".into()).spawn(move || {
        worker.mark_worker_entered();
        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "started", "total": total, "operation": "transcribe",
                "operationId": worker_operation_id.as_str()
            }),
        );

        let mut terminal_intent = crate::db::BatchTerminalIntentV1::Succeeded;
        let mut page_cursor = None;

        'pages: loop {
            if cancel.is_cancelled() {
                terminal_intent = crate::db::BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() };
                break;
            }
            let page = match worker.lease().and_then(|authority| authority.pending_page(page_cursor)) {
                Ok(items) => items,
                Err(error) => {
                    tracing::error!(%error, "Durable transcription work page could not be read");
                    terminal_intent =
                        crate::db::BatchTerminalIntentV1::Failed { code: "BATCH_EVIDENCE_INVALID".into() };
                    break;
                }
            };
            if page.is_empty() {
                break;
            }
            // One cache per fixed database page preserves same-recording reuse without retaining
            // an OS source handle for every distinct recording in a 100,000-item operation.
            let source_lease_cache: crate::pipeline::TranscriptionSourceLeaseCache = Default::default();
            for item in page {
                if cancel.is_cancelled() {
                    terminal_intent = crate::db::BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() };
                    break 'pages;
                }
                if item.ordinal % 10 == 0 && health::check_memory_pressure() {
                    tracing::warn!(
                        available_mib = health::available_memory_mb(),
                        ordinal = item.ordinal,
                        "Memory pressure during durable transcription; pausing before the next inference"
                    );
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }

                let bound_source = match pipeline.bind_existing_transcription_source_cached(
                    &item.segment_id,
                    Some(&item.before.segment.audio_path),
                    item.before.segment.alignment_json.as_deref(),
                    &source_lease_cache,
                ) {
                    Ok(source) => source,
                    Err(error) => {
                        tracing::error!(segment_id = %item.segment_id, %error, "Transcription source binding failed");
                        terminal_intent = crate::db::BatchTerminalIntentV1::Failed {
                            code: durable_transcription_failure_code(&error),
                        };
                        break 'pages;
                    }
                };
                if !batch_transcription_source_cache_is_within_page_bound(&source_lease_cache) {
                    tracing::error!(
                        operation_id = %worker_operation_id,
                        "Batch transcription source cache exceeded its fixed durable page bound"
                    );
                    terminal_intent =
                        crate::db::BatchTerminalIntentV1::Failed { code: "BATCH_EVIDENCE_INVALID".into() };
                    break 'pages;
                }
                let inferred = match pipeline.transcribe_bound_draft_only(&bound_source, Some(cancel.as_atomic())) {
                    Ok(draft) => draft,
                    Err(error) => {
                        if cancel.is_cancelled() {
                            terminal_intent =
                                crate::db::BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() };
                        } else {
                            tracing::error!(segment_id = %item.segment_id, %error, "Champion batch inference failed");
                            terminal_intent = crate::db::BatchTerminalIntentV1::Failed {
                                code: durable_transcription_failure_code(&error),
                            };
                        }
                        break 'pages;
                    }
                };
                if cancel.is_cancelled() {
                    terminal_intent = crate::db::BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() };
                    break 'pages;
                }
                let draft = match pipeline.prepare_batch_champion_draft(inferred) {
                    Ok(draft) => draft,
                    Err(error) => {
                        tracing::error!(segment_id = %item.segment_id, %error, "Champion draft evidence was refused");
                        terminal_intent = crate::db::BatchTerminalIntentV1::Failed {
                            code: durable_transcription_failure_code(&error),
                        };
                        break 'pages;
                    }
                };

                match worker.lease().and_then(|authority| authority.commit_champion_draft(item.ordinal, &draft)) {
                    Ok(
                        crate::db::BatchItemCommitOutcomeV1::Applied { .. }
                        | crate::db::BatchItemCommitOutcomeV1::AlreadyApplied { .. }
                        | crate::db::BatchItemCommitOutcomeV1::Skipped { .. },
                    ) => {}
                    Ok(crate::db::BatchItemCommitOutcomeV1::Failed { code }) => {
                        terminal_intent = crate::db::BatchTerminalIntentV1::Failed { code };
                        break 'pages;
                    }
                    Ok(crate::db::BatchItemCommitOutcomeV1::AlreadyTerminal { state, code }) => {
                        if matches!(state, crate::db::BatchItemStateV1::Failed | crate::db::BatchItemStateV1::Abandoned)
                        {
                            terminal_intent = crate::db::BatchTerminalIntentV1::Failed {
                                code: code.unwrap_or_else(|| "BATCH_TRANSCRIPT_WRITE_FAILED".into()),
                            };
                            break 'pages;
                        }
                        if state == crate::db::BatchItemStateV1::Pending {
                            terminal_intent =
                                crate::db::BatchTerminalIntentV1::Failed { code: "BATCH_EVIDENCE_INVALID".into() };
                            break 'pages;
                        }
                    }
                    Err(error) => {
                        tracing::error!(segment_id = %item.segment_id, %error, "Durable champion commit failed");
                        terminal_intent =
                            crate::db::BatchTerminalIntentV1::Failed { code: "BATCH_TRANSCRIPT_WRITE_FAILED".into() };
                        break 'pages;
                    }
                }
                page_cursor = Some(item.ordinal);
                emit_or_log(
                    &app_clone,
                    "batch-progress",
                    serde_json::json!({
                        "type": "progress", "current": item.ordinal + 1, "total": total,
                        "status": "transcribing", "operation": "transcribe",
                        "operationId": worker_operation_id.as_str()
                    }),
                );
            }
        }

        let terminal = match worker.finish(terminal_intent) {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(%error, "Transcription batch could not publish terminal evidence");
                return;
            }
        };
        let outcome = match durable_batch_outcome(&terminal) {
            Ok(Some(outcome)) => outcome,
            Ok(None) => {
                tracing::error!("Transcription terminalization returned a non-terminal status");
                return;
            }
            Err(error) => {
                tracing::error!(%error, "Transcription terminal evidence is outside the public contract");
                return;
            }
        };
        if let Some(app_state) = app_clone.try_state::<AppState>() {
            if !app_state.record_batch_outcome(
                worker_operation_id.as_str(),
                crate::BatchOperation::Transcribe,
                outcome.clone(),
            ) {
                tracing::error!("Durable transcription outcome was not accepted by the liveness tracker");
            }
        }
        let event_type =
            if matches!(outcome.disposition, crate::BatchRunDisposition::Halted | crate::BatchRunDisposition::Panicked)
            {
                "halted"
            } else {
                "completed"
            };
        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": event_type,
                "total": outcome.total,
                "succeeded": outcome.succeeded,
                "failed": outcome.failed,
                "skipped": outcome.skipped,
                "abandoned": outcome.abandoned,
                "cancelled": outcome.cancelled,
                "operation": "transcribe",
                "operationId": worker_operation_id.as_str(),
                "error": outcome.error_code.as_ref().map(|code| serde_json::json!({
                    "schema": 1, "code": code,
                    "message": "The transcription batch stopped safely.", "retryable": true
                })),
            }),
        );
    });

    match spawn {
        Ok(_) => Ok(BatchStartedV1 {
            status: BatchStartStatusV1::Started,
            operation_id: operation_id.clone(),
            operation: BatchOperationV1::Transcribe,
        }),
        Err(error) => {
            tracing::error!(%error, "OS refused the durable transcription worker");
            Err(CommandErrorV1::new(
                "BATCH_WORKER_START_FAILED",
                "The transcription worker could not start. No pending segment was changed.",
                true,
            )
            .suggested(SuggestedActionV1::Retry))
        }
    }
}

fn validate_normalization_text(text: &str) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    validate::validate_text(text, 100_000, "Normalization text").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "INVALID_NORMALIZATION_TEXT",
            "The transcript normalization input is invalid.",
            false,
        )
    })
}

#[cfg(test)]
mod picker_wait_tests {
    use super::*;

    #[test]
    fn pre_sent_response_beats_a_simultaneously_ready_deadline() {
        tauri::async_runtime::block_on(async {
            let (tx, rx) = tokio::sync::oneshot::channel();
            tx.send("selected").expect("send picker response");
            let result = await_picker_response(rx, std::future::pending(), std::future::ready(())).await;
            assert_eq!(result, Ok("selected"));
        });
    }

    #[test]
    fn live_unsent_picker_times_out_without_wall_clock_wait() {
        tauri::async_runtime::block_on(async {
            let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
            let result = await_picker_response(rx, std::future::pending(), std::future::ready(())).await;
            assert_eq!(result, Err(PickerWaitError::TimedOut));
        });
    }

    #[test]
    fn dropped_picker_sender_is_reported_closed() {
        tauri::async_runtime::block_on(async {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            drop(tx);
            let result = await_picker_response(rx, std::future::pending(), std::future::pending()).await;
            assert_eq!(result, Err(PickerWaitError::Closed));
        });
    }

    #[test]
    fn explicit_cancel_interrupts_a_live_picker() {
        tauri::async_runtime::block_on(async {
            let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
            let result = await_picker_response(rx, std::future::ready(()), std::future::pending()).await;
            assert_eq!(result, Err(PickerWaitError::Cancelled));
        });
    }

    #[test]
    fn explicit_cancel_has_precedence_over_response_and_deadline() {
        tauri::async_runtime::block_on(async {
            let (tx, rx) = tokio::sync::oneshot::channel();
            tx.send("selected").expect("send picker response");
            let result = await_picker_response(rx, std::future::ready(()), std::future::ready(())).await;
            assert_eq!(result, Err(PickerWaitError::Cancelled));
        });
    }
}

#[cfg(test)]
mod typed_import_journal_ipc_tests {
    use super::*;

    fn assert_private_detail_absent(error: &CommandErrorV1) {
        let wire = serde_json::to_string(error).expect("serialize typed import error");
        assert!(!wire.contains("Wareen"));
        assert!(!wire.contains("secret-token"));
        assert!(!wire.contains("SELECT *"));
        assert!(!wire.contains("D:\\\\private"));
    }

    #[test]
    fn interrupted_import_wire_shape_reports_progress_without_paths() {
        let public = ImportJobV1::from(crate::db::ImportJob {
            id: "import-job-1".to_string(),
            dir: r"D:\private\owner-audio".to_string(),
            total_files: 3,
            completed_paths: vec![
                r"D:\private\owner-audio\first.wav".to_string(),
                r"D:\private\owner-audio\second.wav".to_string(),
            ],
            created_at: "2026-08-28T10:00:00Z".to_string(),
        });

        let wire = serde_json::to_value(public).expect("serialize import job DTO");
        assert_eq!(wire["id"], "import-job-1");
        assert_eq!(wire["totalFiles"], 3);
        assert_eq!(wire["completedCount"], 2);
        assert_eq!(wire["createdAt"], "2026-08-28T10:00:00Z");
        assert!(wire.get("dir").is_none());
        assert!(wire.get("completedPaths").is_none());
        assert!(!wire.to_string().contains("owner-audio"));
    }

    #[test]
    fn typed_import_failures_are_actionable_and_scrub_private_details() {
        let private = r"D:\private\Wareen\source.wav secret-token SELECT * FROM import_jobs";
        let cases = [
            import_journal_read_error(private),
            import_journal_write_error(private),
            public_import_start_error(private),
        ];
        for error in &cases {
            assert!(error.retryable);
            assert_eq!(error.suggested_action, Some(SuggestedActionV1::Retry));
            assert_private_detail_absent(error);
        }

        let restore = public_import_start_error(RESTORE_IN_PROGRESS_MSG);
        assert_eq!(restore.code, "RESTORE_IN_PROGRESS");
        assert!(restore.retryable);
        assert_eq!(restore.suggested_action, Some(SuggestedActionV1::Retry));

        let busy = public_import_start_error("Import already in progress");
        assert_eq!(busy.code, "IMPORT_IN_PROGRESS");
        assert!(busy.retryable);

        let dedup = public_import_start_error(&format!("{}: {}", crate::DEDUP_INDEX_UNAVAILABLE_CODE, private));
        assert_eq!(dedup.code, crate::DEDUP_INDEX_UNAVAILABLE_CODE);
        assert!(!dedup.retryable);
        assert_eq!(dedup.suggested_action, Some(SuggestedActionV1::OpenHealth));
        assert_private_detail_absent(&dedup);

        let invalid = invalid_import_job_id_error();
        assert_eq!(invalid.code, "INVALID_IMPORT_JOB_ID");
        assert!(!invalid.retryable);
        assert_eq!(invalid.suggested_action, None);

        let changed = changed_import_job_error();
        assert_eq!(changed.code, "IMPORT_JOB_CHANGED");
        assert!(!changed.retryable);
        assert_eq!(changed.suggested_action, None);
    }

    #[test]
    fn resume_wire_status_is_a_closed_literal() {
        let wire = serde_json::to_value(ImportResumeV1 {
            status: ImportResumeStatusV1::Started,
            resuming: true,
            import_job_id: "import-job-2".to_string(),
            run_id: "00000000-0000-4000-8000-000000000001".to_string(),
        })
        .expect("serialize import resume DTO");
        assert_eq!(wire["status"], "started");
        assert_eq!(wire["resuming"], true);
        assert_eq!(wire["importJobId"], "import-job-2");
        assert_eq!(wire["runId"], "00000000-0000-4000-8000-000000000001");
    }

    #[test]
    fn import_run_status_wire_shape_is_closed_and_exact() {
        let run_id = "00000000-0000-4000-8000-000000000001";
        for (status, expected) in [
            (ImportRunStatusV1::Running, "running"),
            (ImportRunStatusV1::Settled, "settled"),
            (ImportRunStatusV1::Rejected, "rejected"),
            (ImportRunStatusV1::Unknown, "unknown"),
        ] {
            let wire = serde_json::to_value(ImportRunStatusResponseV1 { run_id: run_id.to_string(), status })
                .expect("serialize import status DTO");
            assert_eq!(wire["runId"], run_id);
            assert_eq!(wire["status"], expected);
        }
    }

    #[test]
    fn batch_operation_identity_requires_exact_canonical_uuid_text() {
        let canonical = "00000000-0000-4000-8000-000000000001";
        assert_eq!(canonical_batch_operation_id(canonical), Ok(canonical.to_string()));
        assert!(canonical_batch_operation_id("{00000000-0000-4000-8000-000000000001}").is_err());
        assert!(canonical_batch_operation_id("00000000-0000-4000-8000-00000000000A").is_err());
        assert!(canonical_batch_operation_id("batch-1").is_err());
    }

    #[test]
    fn batch_selection_rejects_empty_duplicate_invalid_and_unbounded_inputs() {
        assert!(validate_batch_segment_ids(&[]).is_err());
        assert!(validate_batch_segment_ids(&["segment-1".into(), "segment-1".into()]).is_err());
        assert!(validate_batch_segment_ids(&["../segment-1".into()]).is_err());
        assert!(validate_batch_segment_ids(&vec!["segment-1".into(); 100_001]).is_err());
        assert!(validate_batch_segment_ids(&["segment-1".into(), "segment-2".into()]).is_ok());
    }

    #[test]
    fn transcription_source_cache_enforces_the_durable_page_handle_bound() {
        let cache: crate::pipeline::TranscriptionSourceLeaseCache = Default::default();
        {
            let mut entries = cache.lock().unwrap();
            for ordinal in 0..crate::db::BATCH_PENDING_PAGE_SIZE_V1 {
                entries.insert(
                    (format!("C:/audio/{ordinal}.wav"), format!("pcm-{ordinal}")),
                    std::sync::Arc::new(std::sync::OnceLock::new()),
                );
            }
        }
        assert_eq!(batch_transcription_source_cache_cardinality(&cache), crate::db::BATCH_PENDING_PAGE_SIZE_V1);
        assert!(batch_transcription_source_cache_is_within_page_bound(&cache));

        cache.lock().unwrap().insert(
            ("C:/audio/overflow.wav".into(), "pcm-overflow".into()),
            std::sync::Arc::new(std::sync::OnceLock::new()),
        );
        assert!(!batch_transcription_source_cache_is_within_page_bound(&cache));
    }

    #[test]
    fn pipeline_error_event_scrubs_paths_and_private_errors() {
        let payload = public_pipeline_error_payload(
            Some("00000000-0000-4000-8000-000000000001"),
            r"D:\private\Wareen\source.wav",
            IMPORT_PROCESSING_FAILED,
        );
        let wire = payload.to_string();
        assert_eq!(payload["file"], "source.wav");
        assert_eq!(payload["code"], IMPORT_PROCESSING_FAILED);
        assert_eq!(payload["runId"], "00000000-0000-4000-8000-000000000001");
        assert!(payload.get("error").is_none());
        assert!(!wire.contains("Wareen"));
        assert!(!wire.contains("D:"));
    }

    #[test]
    fn batch_halt_classification_distinguishes_refinement_from_primary_transcription() {
        let refinement =
            crate::error::AppError::Other("LLM refinement failed for segment s1: provider unavailable".into());
        let primary = crate::error::AppError::Other("primary transcription worker failed".into());

        assert_eq!(batch_halt_code_for_error(&refinement), BatchHaltCode::BatchRefinementFailed);
        assert_eq!(batch_halt_code_for_error(&primary), BatchHaltCode::BatchTranscriptionFailed);
    }
}

#[tauri::command]
#[specta::specta]
pub fn normalize_text(text: String, state: State<'_, AppState>) -> Result<String, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("normalize_text").map_err(|_| {
        crate::ipc_contract::CommandErrorV1::new(
            "RATE_LIMITED",
            "Too many normalization requests. Retry in a moment.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
    })?;
    validate_normalization_text(&text)?;
    let settings = state.lock_settings();
    let config = crate::normalizer::NormalizationConfig {
        normalize_numbers: settings.auto_normalize,
        verbalize_numbers: settings.verbalize_numbers,
        normalize_hamza: true,
        remove_diacritics: false,
    };
    let normalizer = crate::normalizer::SoraniNormalizer::with_config(config);
    Ok(normalizer.normalize(&text))
}

#[cfg(test)]
mod typed_normalization_ipc_tests {
    use super::*;

    #[test]
    fn normalization_validation_error_is_typed_and_scrubbed() {
        let hostile = format!("token=secret {}", "x".repeat(100_000));
        let error = validate_normalization_text(&hostile).expect_err("oversized normalization input must refuse");
        let wire = serde_json::to_string(&error).expect("serialize normalization error");
        assert!(wire.contains("INVALID_NORMALIZATION_TEXT"));
        assert!(!wire.contains("secret"));
        assert!(!wire.contains("token"));
    }

    #[test]
    fn normalization_text_within_the_character_budget_is_accepted() {
        // The limit is measured in CHARACTERS, not bytes: multi-byte Sorani must keep the full
        // advertised budget (validate_text documents the byte-counting regression this fixed).
        validate_normalization_text("سڵاو ئەمە دەقێکی ئاسایی کوردییە").expect("valid Sorani text is accepted");
        validate_normalization_text(&"ک".repeat(100_000)).expect("exactly the budget is accepted");
    }
}

#[cfg(test)]
mod typed_ingest_refusal_and_identity_tests {
    use super::*;

    #[test]
    fn import_run_admission_maps_exactly_onto_the_public_wire_enum() {
        // The renderer reconciles a lost import response purely from this mapping; a swapped arm
        // would make it clear a still-running command or retry a settled one.
        assert_eq!(ImportRunStatusV1::from(crate::ImportRunAdmission::Running), ImportRunStatusV1::Running);
        assert_eq!(ImportRunStatusV1::from(crate::ImportRunAdmission::Settled), ImportRunStatusV1::Settled);
        assert_eq!(ImportRunStatusV1::from(crate::ImportRunAdmission::Rejected), ImportRunStatusV1::Rejected);
        assert_eq!(ImportRunStatusV1::from(crate::ImportRunAdmission::Unknown), ImportRunStatusV1::Unknown);
    }

    #[test]
    fn import_run_identity_requires_exact_canonical_uuid_text() {
        let canonical = "00000000-0000-4000-8000-000000000001";
        assert_eq!(canonical_import_run_id(canonical), Ok(canonical.to_string()));
        // Parseable-but-non-canonical spellings are refused, not normalized: admission state is
        // keyed by exact text, so an alternate spelling would fork one run into two identities.
        assert!(canonical_import_run_id("{00000000-0000-4000-8000-000000000001}").is_err());
        assert!(canonical_import_run_id("00000000-0000-4000-8000-00000000000A").is_err());
        assert!(canonical_import_run_id("run-1").is_err());
    }

    #[test]
    fn import_refusal_helpers_pin_owner_actionable_codes() {
        let busy = import_rate_limited_error();
        assert_eq!(busy.code, "RATE_LIMITED");
        assert!(busy.retryable);
        assert_eq!(busy.suggested_action, Some(SuggestedActionV1::Retry));

        // Dedup-unavailable is a hard stop toward Health, never a blind retry: retrying without
        // duplicate protection is exactly the import this refusal exists to prevent.
        let not_ready = import_not_ready_error();
        assert_eq!(not_ready.code, crate::DEDUP_INDEX_UNAVAILABLE_CODE);
        assert!(!not_ready.retryable);
        assert_eq!(not_ready.suggested_action, Some(SuggestedActionV1::OpenHealth));

        let invalid_run = invalid_import_run_id_error();
        assert_eq!(invalid_run.code, "INVALID_IMPORT_RUN_ID");
        assert!(!invalid_run.retryable);

        let invalid_batch = invalid_batch_operation_id_error();
        assert_eq!(invalid_batch.code, "INVALID_BATCH_OPERATION_ID");
        assert!(!invalid_batch.retryable);

        let missing = no_interrupted_import_error();
        assert_eq!(missing.code, "NO_INTERRUPTED_IMPORT");
        assert!(!missing.retryable);
    }

    #[test]
    fn public_import_item_label_is_basename_only_for_both_separator_styles() {
        // Journals can hold Windows paths inspected under another host, so both slash styles must
        // reduce to a basename — the directory part is private filesystem history.
        assert_eq!(public_import_item_label(r"D:\private\Wareen\clip one.wav"), "clip one.wav");
        assert_eq!(public_import_item_label("/home/user/audio/clip.flac"), "clip.flac");
        // Control and bidi-formatting characters are display attacks, never useful UI.
        assert_eq!(public_import_item_label("evil\u{202e}gnp.wav"), "evilgnp.wav");
        assert_eq!(public_import_item_label("bad\u{0007}name.wav"), "badname.wav");
        // A separator-only "path" has no basename; the empty fallback comes back verbatim.
        assert_eq!(public_import_item_label("///"), "");
    }

    #[test]
    fn public_event_run_id_admits_only_parseable_uuid_text() {
        assert_eq!(public_event_run_id(None), "");
        assert_eq!(public_event_run_id(Some("not-a-uuid")), "");
        let canonical = "00000000-0000-4000-8000-000000000001";
        assert_eq!(public_event_run_id(Some(canonical)), canonical);
        // Unlike command admission, event correlation NORMALIZES any parseable spelling to
        // canonical text so the renderer can match run ids by simple equality.
        assert_eq!(public_event_run_id(Some("{00000000-0000-4000-8000-000000000001}")), canonical);
    }

    #[test]
    fn probe_result_send_survives_a_dropped_receiver() {
        // Live receiver: the probe result arrives intact.
        let (tx, rx) = std::sync::mpsc::channel();
        send_audio_duration_probe_result(tx, Ok(1234));
        assert_eq!(rx.recv().expect("probe result must arrive").expect("probe result is Ok"), 1234);

        // Timed-out caller: the receiver is gone. The worker must swallow the send failure — a
        // panic here would unwind the probe worker thread instead of merely logging a warning.
        let (tx, rx) = std::sync::mpsc::channel::<crate::error::AppResult<i64>>();
        drop(rx);
        send_audio_duration_probe_result(tx, Ok(1));
    }

    #[test]
    fn import_cancel_wait_returns_immediately_for_a_pre_cancelled_token() {
        let token = crate::CancellationToken::new();
        token.cancel();
        // A regression here hangs this test forever (the loop never observes cancellation) —
        // that hang IS the failure signal for the picker's cancel arm.
        tauri::async_runtime::block_on(wait_for_import_cancel(token.clone()));
        assert!(token.is_cancelled());
    }

    #[test]
    fn import_cancel_wait_observes_a_cancellation_raised_mid_poll() {
        let token = crate::CancellationToken::new();
        let canceller = token.clone();
        let handle = std::thread::spawn(move || {
            // Give the waiter time to enter its poll sleep so the sleeping loop arm — not the
            // fast pre-cancelled path — is the one that observes the flag.
            std::thread::sleep(std::time::Duration::from_millis(250));
            canceller.cancel();
        });
        tauri::async_runtime::block_on(wait_for_import_cancel(token.clone()));
        handle.join().expect("canceller thread");
        assert!(token.is_cancelled());
    }

    #[test]
    fn app_git_sha_exposes_the_baked_build_identity() {
        let sha = app_git_sha().expect("git sha command is infallible");
        assert_eq!(sha, crate::GIT_SHA);
        // The About panel and e2e harness compare against this value; a blank identity would make
        // "which build is running" unanswerable (see the silent-rollback incident).
        assert!(!sha.trim().is_empty(), "build identity must never be blank");
    }

    #[test]
    fn import_journal_failure_helpers_stay_retryable_and_scrub_the_private_detail() {
        // Both helpers exist so a durable-journal failure never leaks SQL text or an absolute
        // library path into the webview, and never reads as a permanent loss: the journal itself
        // survives every one of these refusals, so retrying is the honest owner action.
        let read = import_journal_read_error(r"SQLITE_CORRUPT reading D:\private\library.db token=secret");
        assert_eq!(read.code, "IMPORT_JOURNAL_READ_FAILED");
        assert!(read.retryable);
        assert_eq!(read.suggested_action, Some(SuggestedActionV1::Retry));

        let write = import_journal_write_error(r"SQLITE_BUSY writing D:\private\library.db token=secret");
        assert_eq!(write.code, "IMPORT_JOURNAL_UPDATE_FAILED");
        assert!(write.retryable);
        assert_eq!(write.suggested_action, Some(SuggestedActionV1::Retry));

        for error in [&read, &write] {
            let wire = serde_json::to_string(error).expect("serialize journal failure");
            for forbidden in ["SQLITE", "D:\\", "private", "token", "secret"] {
                assert!(!wire.contains(forbidden), "{wire} leaked {forbidden}");
            }
        }
    }

    #[test]
    fn public_import_start_error_classifies_every_private_admission_detail() {
        let restore = public_import_start_error(RESTORE_IN_PROGRESS_MSG);
        assert_eq!(restore.code, "RESTORE_IN_PROGRESS");
        assert!(restore.retryable);
        assert_eq!(restore.suggested_action, Some(SuggestedActionV1::Retry));

        let busy = public_import_start_error("Import already in progress");
        assert_eq!(busy.code, "IMPORT_IN_PROGRESS");
        assert!(busy.retryable);
        assert_eq!(busy.suggested_action, Some(SuggestedActionV1::Retry));

        // Dedup unavailability arrives wrapped inside a longer private admission message. It must
        // still degrade to the hard-stop Health refusal — a retryable code here would invite exactly
        // the import without duplicate protection this refusal exists to prevent.
        let not_ready =
            public_import_start_error(&format!("audio import refused: {}", crate::DEDUP_INDEX_UNAVAILABLE_CODE));
        assert_eq!(not_ready.code, crate::DEDUP_INDEX_UNAVAILABLE_CODE);
        assert!(!not_ready.retryable);
        assert_eq!(not_ready.suggested_action, Some(SuggestedActionV1::OpenHealth));

        // Anything else is the generic resume failure, with the private detail left behind.
        let other = public_import_start_error(r"OS thread creation failed for D:\private\audio");
        assert_eq!(other.code, "IMPORT_RESUME_FAILED");
        assert!(other.retryable);
        let wire = serde_json::to_string(&other).expect("serialize resume failure");
        for forbidden in ["D:\\", "private", "thread"] {
            assert!(!wire.contains(forbidden), "{wire} leaked {forbidden}");
        }
    }

    #[test]
    fn jury_pipeline_failure_logging_never_unwinds_the_import_worker() {
        // Diagnostics only. The post-import jury runs ON the import worker thread, so this helper
        // must absorb any context/error pair — including empty and pathologically long text — and
        // return normally. An unwind here would skip the worker's terminal import-complete event
        // and wedge the import UI at "processing" forever.
        log_jury_pipeline_failure("single-file import", "jury adjudication failed");
        log_jury_pipeline_failure("", "");
        log_jury_pipeline_failure("directory import", &"e".repeat(10_000));
    }
}

#[cfg(test)]
mod typed_batch_halt_classification_tests {
    use super::*;

    #[test]
    fn batch_halt_codes_classify_every_failure_family() {
        use crate::error::{AppError, AudioError};
        let tag = crate::pipeline::ASR_7B_UNAVAILABLE_TAG;
        // Decode-shaped audio failures route the owner to the clip, not to a champion retry.
        for audio in [
            AudioError::UnsupportedCodec("wma".into()),
            AudioError::Decode("truncated frame".into()),
            AudioError::Resample("rate mismatch".into()),
            AudioError::NoTracks(std::path::PathBuf::from("clip.mp4")),
            AudioError::EmptyBuffer,
        ] {
            assert_eq!(batch_halt_code_for_error(&AppError::Audio(audio)), BatchHaltCode::AudioDecodeFailed);
        }
        assert_eq!(
            batch_halt_code_for_error(&AppError::Validation("E_TRANSCRIPTION_SOURCE_CHANGED: clip edited".into())),
            BatchHaltCode::TranscriptionSourceChanged
        );
        // Both identity clauses mean silent substitution — the exact failure the champion-supremacy
        // rule exists to hard-stop — and must never soften into a retryable "unavailable".
        assert_eq!(
            batch_halt_code_for_error(&AppError::Validation(format!(
                "{tag}: loaded model does not match registry champion"
            ))),
            BatchHaltCode::ChampionIdentityMismatch
        );
        assert_eq!(
            batch_halt_code_for_error(&AppError::Validation(format!("{tag}: transcription reply identity a/b"))),
            BatchHaltCode::ChampionIdentityMismatch
        );
        // The plain tag (no identity clause) is availability, not substitution.
        assert_eq!(
            batch_halt_code_for_error(&AppError::Validation(format!("{tag}: connection refused"))),
            BatchHaltCode::ChampionUnavailable
        );
        // Primary recognizer failures must never masquerade as refinement failures — the wrong
        // label sends the owner toward the wrong recovery path.
        assert_eq!(
            batch_halt_code_for_error(&AppError::Asr("engine fault".into())),
            BatchHaltCode::BatchTranscriptionFailed
        );
        assert_eq!(
            batch_halt_code_for_error(&AppError::Onnx("ort fault".into())),
            BatchHaltCode::BatchTranscriptionFailed
        );
        assert_eq!(
            batch_halt_code_for_error(&AppError::ModelNotFound {
                path: std::path::PathBuf::from("model.onnx"),
                reason: "missing".into()
            }),
            BatchHaltCode::BatchTranscriptionFailed
        );
        // A validation message with neither guard prefix falls through to the generic family.
        assert_eq!(
            batch_halt_code_for_error(&AppError::Validation("unrelated refusal".into())),
            BatchHaltCode::BatchTranscriptionFailed
        );
    }

    #[test]
    fn batch_halt_errors_carry_actionable_wire_contracts() {
        let cases = [
            (BatchHaltCode::ChampionUnavailable, "CHAMPION_UNAVAILABLE", true, SuggestedActionV1::OpenHealth),
            (
                BatchHaltCode::ChampionIdentityMismatch,
                "CHAMPION_IDENTITY_MISMATCH",
                false,
                SuggestedActionV1::OpenModels,
            ),
            (
                BatchHaltCode::TranscriptionSourceChanged,
                "TRANSCRIPTION_SOURCE_CHANGED",
                false,
                SuggestedActionV1::ReloadClip,
            ),
            (BatchHaltCode::AudioDecodeFailed, "AUDIO_DECODE_FAILED", false, SuggestedActionV1::ReloadClip),
            (BatchHaltCode::BatchRefinementFailed, "BATCH_REFINEMENT_FAILED", true, SuggestedActionV1::Retry),
            (BatchHaltCode::BatchTranscriptionFailed, "BATCH_TRANSCRIPTION_FAILED", true, SuggestedActionV1::Retry),
        ];
        for (halt, code, retryable, suggested) in cases {
            let error = batch_halt_error(halt);
            assert_eq!(error.code, code);
            assert_eq!(error.retryable, retryable, "halt {code}");
            assert_eq!(error.suggested_action, Some(suggested), "halt {code}");
        }
        // The durable journal's terminal failure code is fed from the same table, so a rename in
        // one place cannot silently fork the wire contract from the journal evidence.
        let decode = crate::error::AppError::Audio(crate::error::AudioError::EmptyBuffer);
        assert_eq!(durable_transcription_failure_code(&decode), "AUDIO_DECODE_FAILED");
        let asr = crate::error::AppError::Asr("engine crashed".into());
        assert_eq!(durable_transcription_failure_code(&asr), "BATCH_TRANSCRIPTION_FAILED");
    }

    #[test]
    fn batch_admission_refusals_classify_and_scrub_private_details() {
        let cases = [
            ("prep aborted: BATCH_ADMISSION_CANCELLED", "BATCH_START_CANCELLED", true),
            (RESTORE_IN_PROGRESS_MSG, "RESTORE_IN_PROGRESS", true),
            ("restore generation changed during admission", "RESTORE_GENERATION_CHANGED", true),
            ("batch already in progress", "BATCH_ALREADY_RUNNING", true),
            ("one_live_batch constraint violated", "BATCH_ALREADY_RUNNING", true),
            ("segment seg-42 does not exist", "BATCH_SEGMENT_MISSING", false),
            (r"disk I/O error at D:\private\Wareen\cortex.db", "BATCH_ADMISSION_FAILED", false),
        ];
        for (private, code, retryable) in cases {
            let error = batch_transcribe_admission_error(private);
            assert_eq!(error.code, code, "detail: {private}");
            assert_eq!(error.retryable, retryable, "detail: {private}");
            // Raw admission details can carry paths and row identities; the wire error never may.
            let wire = serde_json::to_string(&error).expect("serialize admission refusal");
            assert!(!wire.contains("Wareen"), "detail: {private}");
            assert!(!wire.contains("seg-42"), "detail: {private}");
        }
    }
}

// State-taking ingest command coverage through the shared MockRuntime harness (test_support).
#[cfg(test)]
mod state_ingest_command_harness_tests {
    use super::*;
    use crate::test_support::managed_app_state;

    #[test]
    fn get_import_run_status_reconciles_every_admission_state() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();

        let invalid = get_import_run_status("not-a-run".to_string(), app.state())
            .expect_err("a non-canonical run identity must refuse");
        assert_eq!(invalid.code, "INVALID_IMPORT_RUN_ID");

        let unseen = "00000000-0000-4000-8000-0000000000aa".to_string();
        let status = get_import_run_status(unseen.clone(), app.state()).expect("unseen run status");
        assert_eq!(status, ImportRunStatusResponseV1 { run_id: unseen, status: ImportRunStatusV1::Unknown });

        let rejected = "00000000-0000-4000-8000-0000000000ab".to_string();
        harness.remember_import_rejection(&rejected);
        let status = get_import_run_status(rejected.clone(), app.state()).expect("rejected run status");
        assert_eq!(status.status, ImportRunStatusV1::Rejected);

        let live = "00000000-0000-4000-8000-0000000000ac".to_string();
        harness.try_start_import_for_run(&live).expect("claim the import gate for a live run");
        let status = get_import_run_status(live.clone(), app.state()).expect("running run status");
        assert_eq!(status.status, ImportRunStatusV1::Running, "a claimed run must never read as unknown");
        harness.finish_import();
        let status = get_import_run_status(live, app.state()).expect("settled run status");
        assert_eq!(status.status, ImportRunStatusV1::Settled);
    }

    #[test]
    fn interrupted_import_recovery_serves_discards_and_refuses_exactly() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();

        assert!(
            get_interrupted_import(app.state()).expect("fresh library").is_none(),
            "no journal exists yet, so there is nothing to recover"
        );

        let import_dir = tmp.path().join("import-source");
        std::fs::create_dir_all(&import_dir).unwrap();
        let journal_id =
            harness.job_store().begin_import(&import_dir.to_string_lossy(), 2).expect("open a durable import journal");
        harness
            .job_store()
            .mark_import_file_done(&journal_id, "C:/fixtures/first.wav")
            .expect("record one completed file");

        let offered = get_interrupted_import(app.state())
            .expect("read the crashed journal")
            .expect("a running journal with no live worker is a crash to offer");
        assert_eq!(offered.id, journal_id);
        assert_eq!(offered.total_files, 2);
        assert_eq!(offered.completed_count, 1, "progress must reflect the durable per-file journal");

        let invalid = discard_interrupted_import("bad id".to_string(), app.state())
            .expect_err("a malformed journal identity must refuse");
        assert_eq!(invalid.code, "INVALID_IMPORT_JOB_ID");

        let changed = discard_interrupted_import("import-job-mismatch".to_string(), app.state())
            .expect_err("a stale journal identity must not delete the live successor");
        assert_eq!(changed.code, "IMPORT_JOB_CHANGED");

        discard_interrupted_import(journal_id.clone(), app.state()).expect("exact-identity discard");
        assert!(get_interrupted_import(app.state()).expect("after discard").is_none());

        let gone = discard_interrupted_import(journal_id, app.state())
            .expect_err("discarding an already-discarded journal must say so");
        assert_eq!(gone.code, "NO_INTERRUPTED_IMPORT");

        // While an import worker owns the gate, recovery reads serve None and discards refuse —
        // the live successor can be neither advertised nor deleted.
        let running = "00000000-0000-4000-8000-0000000000ad".to_string();
        harness.try_start_import_for_run(&running).expect("claim the import gate");
        assert!(
            get_interrupted_import(app.state()).expect("recovery read during a live import").is_none(),
            "a running import's journal belongs to the worker, never to the recovery prompt"
        );
        let busy = discard_interrupted_import("import-job-any".to_string(), app.state())
            .expect_err("discard during a live import must refuse");
        assert_eq!(busy.code, "IMPORT_IN_PROGRESS");
        harness.finish_import();
    }

    #[test]
    fn app_health_serves_the_typed_workspace_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let health = app_health(app.state()).expect("health snapshot");

        assert_eq!(health.segment_count, 0);
        assert!(health.db_size > 0, "a migrated database file is never empty");
        assert!(
            health.status == "ok" || health.status == "models_needed",
            "health status is a closed vocabulary, got {:?}",
            health.status
        );
        if !health.missing_models.is_empty() {
            assert_eq!(health.status, "models_needed", "missing required models must degrade the status");
        }
        assert!(health.primary_asr_model.contains("WSL7B"), "the champion engine is the shipped default");
    }

    #[test]
    fn normalize_text_normalizes_sorani_through_the_typed_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();

        let input = "سلاو  جیهان ١٢".to_string();
        let served = normalize_text(input.clone(), app.state()).expect("normalize real Sorani text");
        let expected = {
            let settings = harness.lock_settings();
            crate::normalizer::SoraniNormalizer::with_config(crate::normalizer::NormalizationConfig {
                normalize_numbers: settings.auto_normalize,
                verbalize_numbers: settings.verbalize_numbers,
                normalize_hamza: true,
                remove_diacritics: false,
            })
            .normalize(&input)
        };
        assert_eq!(served, expected, "the command must serve exactly the configured production normalization");
        assert!(!served.trim().is_empty());

        let refused = normalize_text("ک".repeat(100_001), app.state())
            .expect_err("input beyond the character budget must refuse");
        assert_eq!(refused.code, "INVALID_NORMALIZATION_TEXT");
    }

    #[test]
    fn claimed_import_start_rejects_on_drop_unless_a_worker_disarmed_it() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();

        // A pre-worker early return (validation, recovery inspection, OS thread creation): the
        // claim is still armed when it drops, so the run must settle as REJECTED — the renderer may
        // surface the original command error only for a definitively rejected run.
        let aborted = "00000000-0000-4000-8000-0000000000b1".to_string();
        harness.try_start_import_for_run(&aborted).expect("claim the import gate");
        assert_eq!(
            get_import_run_status(aborted.clone(), app.state()).expect("claimed run status").status,
            ImportRunStatusV1::Running
        );
        drop(ClaimedImportStart::new(&harness, &aborted));
        assert_eq!(
            get_import_run_status(aborted, app.state()).expect("aborted run status").status,
            ImportRunStatusV1::Rejected,
            "an armed claim that drops before a worker exists must publish a rejection, never a settlement"
        );

        // The gate must also have reopened: an armed drop that released the run identity but kept
        // ImportState::Running would wedge every later import behind IMPORT_IN_PROGRESS.
        let handed_off = "00000000-0000-4000-8000-0000000000b2".to_string();
        harness.try_start_import_for_run(&handed_off).expect("the gate reopened after an aborted claim");
        {
            let mut claim = ClaimedImportStart::new(&harness, &handed_off);
            claim.disarm();
        }
        assert_eq!(
            get_import_run_status(handed_off.clone(), app.state()).expect("disarmed run status").status,
            ImportRunStatusV1::Running,
            "a disarmed claim hands the run to the spawned worker; only the worker may settle it"
        );
        harness.finish_import();
        assert_eq!(
            get_import_run_status(handed_off, app.state()).expect("settled run status").status,
            ImportRunStatusV1::Settled
        );
    }

    #[test]
    fn claimed_file_picker_releases_only_the_token_it_armed() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();

        let token = harness.try_start_file_picker().expect("claim the native picker slot");
        assert_eq!(
            harness.try_start_file_picker().err().as_deref(),
            Some("E_FILE_PICKER_BUSY"),
            "the picker slot is exclusive while a claim holds it"
        );

        drop(ClaimedFilePicker { state: &harness, token: token.clone() });
        let successor = harness.try_start_file_picker().expect("the picker slot reopened when the claim dropped");

        // Cancellation, timeout and channel closure can all drop a STALE claim after a successor
        // armed its own token. Releasing the slot on that stale token would strand the live picker
        // with an empty cancel slot, exactly the token-loss shape finish_import documents.
        drop(ClaimedFilePicker { state: &harness, token });
        assert_eq!(
            harness.try_start_file_picker().err().as_deref(),
            Some("E_FILE_PICKER_BUSY"),
            "a stale claim must never release the live picker's slot"
        );
        harness.finish_file_picker(&successor);
        harness.try_start_file_picker().expect("the owner's own release reopens the slot");
    }
}

#[cfg(test)]
mod state_ingest_worker_harness_tests {
    //! The import, resume and batch workers driven through a mock app with a real `AppState`,
    //! model-free. The champion preflight refuses against the empty test registry before any
    //! socket is opened, so these run identically on a workstation with a live champion and on a
    //! CI runner without one. What they pin is the settle contract the workers' own comments
    //! describe: a worker that fails for any reason still releases the import gate, reports a
    //! scrubbed public error and emits its terminal events -- never a progress UI stuck
    //! "processing" forever.
    use super::*;
    use crate::test_support::managed_app_state;
    use crate::{BatchRunAdmission, ImportRunAdmission};
    use std::sync::mpsc;
    use std::time::Duration;
    use tauri::Listener;

    type MockApp = tauri::App<tauri::test::MockRuntime>;

    /// Decode of a 1 s fixture plus the registry preflight refusal, with CI headroom.
    const SETTLE_BUDGET: Duration = Duration::from_secs(60);

    fn run(n: u8) -> String {
        format!("00000000-0000-4000-8000-0000000000{n:02x}")
    }

    /// A real 1 s mono 16 kHz WAV: `validate_file_path` admits it and the decode is trivial.
    fn one_second_wav(dir: &std::path::Path, name: &str) -> String {
        let path = dir.join(name);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("create wav");
        for i in 0..16_000u32 {
            let t = f64::from(i) / 16_000.0;
            writer
                .write_sample((8_000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16)
                .expect("write sample");
        }
        writer.finalize().expect("finalize wav");
        crate::test_support::await_stable_fixture(&path);
        path.to_string_lossy().into_owned()
    }

    /// Every payload the app emits under `event`, in order, on a channel the test can wait on.
    fn capture(app: &MockApp, event: &str) -> mpsc::Receiver<serde_json::Value> {
        let (tx, rx) = mpsc::channel();
        app.listen_any(event.to_string(), move |event| {
            let payload = serde_json::from_str(event.payload()).unwrap_or(serde_json::Value::Null);
            let _ = tx.send(payload);
        });
        rx
    }

    #[test]
    fn import_audio_file_refuses_bad_identity_and_paths_and_records_the_rejection() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();
        let wav = one_second_wav(tmp.path(), "refusals.wav");

        let invalid = import_audio_file_on(wav, "not-a-run".into(), app.handle().clone(), app.state())
            .expect_err("a non-canonical run identity must refuse before touching the gate");
        assert_eq!(invalid.code, "INVALID_IMPORT_RUN_ID");

        let missing = tmp.path().join("missing.wav").to_string_lossy().into_owned();
        let refused = import_audio_file_on(missing, run(0xb1), app.handle().clone(), app.state())
            .expect_err("a path that does not exist must refuse");
        assert_eq!(refused.code, "INVALID_AUDIO_PATH");
        assert_eq!(
            harness.import_run_admission(&run(0xb1)),
            ImportRunAdmission::Rejected,
            "a refused start is remembered as rejected, never left unknown"
        );
        assert!(harness.try_import_recovery_admission().is_some(), "a refused start must release the import gate");
    }

    #[test]
    fn import_worker_settles_and_reports_a_scrubbed_failure_without_a_registered_champion() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();
        let wav = one_second_wav(tmp.path(), "clip.wav");
        let settled = capture(&app, "import-worker-settled");
        let errors = capture(&app, "pipeline-error");
        let completes = capture(&app, "import-complete");

        let run_id = run(0xb2);
        let started = import_audio_file_on(wav, run_id.clone(), app.handle().clone(), app.state())
            .expect("a readable file is admitted; the worker decides the outcome");
        assert!(matches!(started.status, crate::ipc_contract::ImportStartStatusV1::Started));
        assert!(matches!(started.source, crate::ipc_contract::ImportSourceV1::File));
        assert_eq!(started.run_id, run_id);

        let settle = settled.recv_timeout(SETTLE_BUDGET).expect("the worker guard emits import-worker-settled");
        assert_eq!(settle["runId"], run_id);
        assert_eq!(settle["source"], "file");
        assert_eq!(harness.import_run_admission(&run_id), ImportRunAdmission::Settled);
        assert!(harness.try_import_recovery_admission().is_some(), "the guard released the import gate");

        let error = errors.recv_timeout(Duration::from_secs(5)).expect("a failed import reports pipeline-error");
        assert_eq!(error["code"], IMPORT_PROCESSING_FAILED);
        assert_eq!(error["runId"], run_id);
        assert_eq!(error["file"], "clip.wav", "the public label is the basename only");
        let wire = error.to_string();
        assert!(!wire.contains(&tmp.path().to_string_lossy().into_owned()), "no private path on the wire: {wire}");
        assert!(!wire.contains("E_ASR_7B"), "the private halt reason stays in the native log: {wire}");

        let complete =
            completes.recv_timeout(Duration::from_secs(5)).expect("import-complete is emitted on failure too");
        assert_eq!(complete["runId"], run_id);
        assert_eq!(complete["total"], 1);
        assert_eq!(complete["succeeded"], 0);
        assert_eq!(complete["failed"], 1);
        assert_eq!(complete["source"], "file");
    }

    #[test]
    fn resume_interrupted_import_refuses_exactly_then_reruns_the_folder_and_settles() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();
        let handle = app.handle().clone();

        let invalid_run = resume_interrupted_import_on("job".into(), "not-a-run".into(), handle.clone(), app.state())
            .expect_err("a non-canonical run identity must refuse");
        assert_eq!(invalid_run.code, "INVALID_IMPORT_RUN_ID");

        let invalid_job = resume_interrupted_import_on("bad id".into(), run(0xc1), handle.clone(), app.state())
            .expect_err("a malformed journal identity must refuse");
        assert_eq!(invalid_job.code, "INVALID_IMPORT_JOB_ID");
        assert_eq!(harness.import_run_admission(&run(0xc1)), ImportRunAdmission::Rejected);

        let none = resume_interrupted_import_on("import-job-none".into(), run(0xc2), handle.clone(), app.state())
            .expect_err("nothing to resume");
        assert_eq!(none.code, "NO_INTERRUPTED_IMPORT");
        assert!(!none.retryable, "no journal is a fact, not a retry");
        assert_eq!(harness.import_run_admission(&run(0xc2)), ImportRunAdmission::Rejected);
        assert!(harness.try_import_recovery_admission().is_some(), "a refused resume releases the gate");

        let vanished = tmp.path().join("vanished");
        let vanished_job = harness
            .job_store()
            .begin_import(&vanished.to_string_lossy(), 1)
            .expect("journal for a folder that is gone");
        let changed = resume_interrupted_import_on("import-job-other".into(), run(0xc3), handle.clone(), app.state())
            .expect_err("a stale journal identity must not resume the live successor");
        assert_eq!(changed.code, "IMPORT_JOB_CHANGED");
        let missing = resume_interrupted_import_on(vanished_job, run(0xc4), handle.clone(), app.state())
            .expect_err("the folder no longer exists");
        assert_eq!(missing.code, "IMPORT_SOURCE_MISSING");
        assert_eq!(harness.import_run_admission(&run(0xc4)), ImportRunAdmission::Rejected);

        let folder = tmp.path().join("resume-source");
        std::fs::create_dir_all(&folder).unwrap();
        let job = harness.job_store().begin_import(&folder.to_string_lossy(), 0).expect("journal for an empty folder");
        let settled = capture(&app, "import-worker-settled");
        let errors = capture(&app, "pipeline-error");
        let resumed = resume_interrupted_import_on(job.clone(), run(0xc5), handle, app.state())
            .expect("an existing folder with a journal is admitted for resume");
        assert!(matches!(resumed.status, ImportResumeStatusV1::Started));
        assert!(resumed.resuming);
        assert_eq!(resumed.run_id, run(0xc5));
        assert_ne!(
            resumed.import_job_id, job,
            "the crashed journal is retired; a fresh journal tracks the resumed run"
        );

        let settle = settled.recv_timeout(SETTLE_BUDGET).expect("the resume worker guard emits import-worker-settled");
        assert_eq!(settle["runId"], run(0xc5));
        assert_eq!(settle["source"], "directory");
        assert_eq!(harness.import_run_admission(&run(0xc5)), ImportRunAdmission::Settled);
        assert!(harness.try_import_recovery_admission().is_some(), "the resume worker released the gate");

        // An empty resume folder is a failure by design: silently reporting success would leave the
        // handed-off successor journal orphaned, so the flow refuses and RETAINS the journal for a
        // deliberate discard.
        let error = errors.recv_timeout(Duration::from_secs(5)).expect("an empty resume folder is reported");
        assert_eq!(error["code"], IMPORT_PROCESSING_FAILED);
        assert_eq!(error["runId"], run(0xc5));
        assert_eq!(error["file"], "resume-source", "the public label is the folder's basename only");
        assert!(!error.to_string().contains(&tmp.path().to_string_lossy().into_owned()), "no private path on the wire");
        let offered = get_interrupted_import(app.state())
            .expect("read the journal after the resume")
            .expect("the durable journal is retained so the user can discard it deliberately");
        assert_eq!(offered.id, resumed.import_job_id, "the retained journal is the successor minted at handoff");
        discard_interrupted_import(offered.id, app.state()).expect("an exact-identity discard closes it");
        assert!(get_interrupted_import(app.state()).expect("after discard").is_none());
    }

    #[test]
    fn batch_transcribe_refuses_before_champion_admission_and_releases_the_batch_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let harness = app.state::<AppState>();
        let handle = app.handle().clone();
        let op = |n: u8| format!("00000000-0000-4000-8000-0000000000{n:02x}");

        let invalid_op = batch_transcribe_blocking(vec!["seg-a".into()], "not-an-operation".into(), handle.clone())
            .expect_err("a non-canonical operation identity must refuse");
        assert_eq!(invalid_op.code, "INVALID_BATCH_OPERATION_ID");

        let empty = batch_transcribe_blocking(vec![], op(0xd1), handle.clone()).expect_err("an empty selection");
        assert_eq!(empty.code, "INVALID_BATCH_SELECTION");
        assert_eq!(harness.batch_run_admission(&op(0xd1)).0, BatchRunAdmission::Rejected);

        // No champion is registered in the test registry: the preflight refuses BEFORE durable admission
        // and before any socket is opened, on this workstation and on CI alike.
        let refused = batch_transcribe_blocking(vec!["seg-a".into(), "seg-b".into()], op(0xd2), handle.clone())
            .expect_err("no registered champion");
        assert_eq!(refused.code, "CHAMPION_UNAVAILABLE");
        assert!(refused.retryable);
        assert_eq!(refused.suggested_action, Some(SuggestedActionV1::OpenHealth));
        assert_eq!(
            harness.batch_run_admission(&op(0xd2)).0,
            BatchRunAdmission::Rejected,
            "a refused batch is remembered"
        );

        let again = batch_transcribe_blocking(vec!["seg-a".into()], op(0xd3), handle)
            .expect_err("still no champion; the gate was released, so this is the same refusal, not IN_PROGRESS");
        assert_eq!(again.code, "CHAMPION_UNAVAILABLE");
    }
}
