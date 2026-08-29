//! Batch review-action IPC commands — slice 3 of the Week-4 `commands.rs` decomposition.
//!
//! Command names remain stable while unsafe legacy batch verification fails closed: `commands.rs` re-exports this module
//! (`pub use batch::*;`), so `lib.rs`'s invoke_handler still names `commands::batch_verify` and the
//! frontend's `invoke('batch_verify')` is untouched. Same functions, only relocated.
//!
//! Long-running normalization runs behind the durable batch journal and streams progress. Speaker
//! assignment remains a generated async, all-or-nothing store operation.

use super::{
    batch_start_commit_error, canonical_batch_config_sha256, canonical_batch_operation_id, durable_batch_outcome,
    emit_or_log, new_batch_executor_identity, validate_batch_segment_ids, DurableBatchWorkerGuard, STRICT_RATE_LIMITER,
};
use crate::db::{BatchItemCommitOutcomeV1, BatchTerminalIntentV1};
use crate::ipc_contract::{
    AssignSpeakersRequestV1, AssignedSpeakersV1, BatchOperationV1, BatchStartStatusV1, BatchStartedV1, CommandErrorV1,
    SuggestedActionV1,
};
use crate::stores::SpeakerAssignmentError;
use crate::validation::input as validate;
use crate::AppState;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::{LazyLock, Mutex};
use tauri::{Manager, State};

/// Process-wide normalizer memoizer: normalization is pure per (config, text), so identical
/// transcripts across a batch normalize once. Sole caller is `batch_normalize` below — inlined
/// rather than a generic cache type (ponytail-audit round 2, 2026-08-13: the old generic
/// `Memoizer<K,V>` in perf/mod.rs had exactly one instantiation).
static NORMALIZER_CACHE: LazyLock<Mutex<LruCache<String, String>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(NonZeroUsize::new(2000).unwrap_or(NonZeroUsize::MIN))));

/// Lock held across `compute` on a miss (matches the prior Memoizer exactly): the first caller to
/// see a given cache_key does the normalization while holding the lock, so concurrent workers on
/// the same never-seen key serialize onto it rather than duplicating the work.
fn normalize_cached(cache_key: String, compute: impl FnOnce() -> String) -> String {
    let mut cache = NORMALIZER_CACHE.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned normalizer cache");
        poisoned.into_inner()
    });
    if let Some(value) = cache.get(&cache_key) {
        return value.clone();
    }
    let value = compute();
    cache.put(cache_key, value.clone());
    value
}

#[tauri::command]
pub fn batch_verify(
    ids: Vec<String>,
    _verified: bool,
    _state: State<'_, AppState>,
    _app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("batch_verify")?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    Err(
        "legacy batch verify/unverify is disabled; use the review decision flow so every human verdict has immutable evidence"
            .into(),
    )
}

fn public_speaker_assignment_error(error: SpeakerAssignmentError) -> CommandErrorV1 {
    match error {
        SpeakerAssignmentError::Invalid => CommandErrorV1::new(
            "INVALID_SPEAKER_ASSIGNMENT",
            "The batch speaker assignment is invalid and was not applied.",
            false,
        ),
        SpeakerAssignmentError::Stale => CommandErrorV1::new(
            "STALE_SEGMENT_SELECTION",
            "The selected segment set changed. Reload the library before assigning a speaker.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip),
        SpeakerAssignmentError::Busy => {
            CommandErrorV1::new("DATABASE_BUSY", "The workspace is busy. Retry the speaker assignment.", true)
                .suggested(SuggestedActionV1::Retry)
        }
        SpeakerAssignmentError::Application => CommandErrorV1::new(
            "SPEAKER_ASSIGNMENT_FAILED",
            "The speaker assignment could not be saved. Open Health before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth),
    }
}

/// One generated, bounded and all-or-nothing speaker assignment. SQLite and exact history work run
/// on the blocking pool; the restore-admission token remains live through session persistence.
#[tauri::command]
#[specta::specta]
pub async fn assign_speakers_v1(
    request: AssignSpeakersRequestV1,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<AssignedSpeakersV1, CommandErrorV1> {
    STRICT_RATE_LIMITER.check("assign_speakers_v1").map_err(|_| {
        CommandErrorV1::new("RATE_LIMITED", "Too many speaker assignment requests. Retry in a moment.", true)
            .suggested(SuggestedActionV1::Retry)
    })?;
    if request.ids.is_empty() || request.ids.len() > 100_000 {
        return Err(CommandErrorV1::new(
            "INVALID_SPEAKER_ASSIGNMENT",
            "Assign a speaker to between one and 100,000 unique segments.",
            false,
        ));
    }
    let mut unique_ids = std::collections::HashSet::with_capacity(request.ids.len());
    for id in &request.ids {
        validate::validate_identifier(id)
            .map_err(|_| CommandErrorV1::new("INVALID_SEGMENT_ID", "A selected segment identity is invalid.", false))?;
        if !unique_ids.insert(id.as_str()) {
            return Err(CommandErrorV1::new(
                "INVALID_SPEAKER_ASSIGNMENT",
                "The speaker assignment contains a duplicate segment identity.",
                false,
            ));
        }
    }
    if let Some(speaker_id) = request.target_speaker_id.as_deref() {
        validate::validate_speaker_label(speaker_id)
            .map_err(|_| CommandErrorV1::new("INVALID_SPEAKER_ID", "The speaker label is invalid.", false))?;
    }

    let requested_count = request.ids.len();
    let target_speaker_id = request.target_speaker_id;
    let segment_writes = state.segment_writes();
    let worker_app = app.clone();
    let (assigned, _mutation) = tokio::task::spawn_blocking(move || {
        let result = segment_writes.assign_speaker_batch_v1(&request.ids, target_speaker_id.as_deref())?;
        if result.0.changed_count > 0 {
            if let Some(app_state) = worker_app.try_state::<AppState>() {
                app_state.session_auto_save();
            }
        }
        Ok::<_, crate::stores::SpeakerAssignmentError>(result)
    })
    .await
    .map_err(|_| {
        CommandErrorV1::new(
            "SPEAKER_ASSIGNMENT_FAILED",
            "The speaker assignment worker stopped unexpectedly. Retry the operation.",
            true,
        )
        .suggested(SuggestedActionV1::Retry)
    })?
    .map_err(public_speaker_assignment_error)?;
    Ok(AssignedSpeakersV1 {
        requested_count,
        changed_count: assigned.changed_count,
        unchanged_count: assigned.requested_count - assigned.changed_count,
    })
}

fn batch_normalize_start_error(error: &str) -> CommandErrorV1 {
    if error.contains("BATCH_ADMISSION_CANCELLED") {
        return batch_start_commit_error(crate::BatchStartCommitError::Cancelled);
    }
    if error.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return CommandErrorV1::new(
            "RESTORE_IN_PROGRESS",
            "A database restore is in progress. Wait for it to finish, then retry.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }
    if error.contains("restore generation changed") {
        return CommandErrorV1::new(
            "RESTORE_GENERATION_CHANGED",
            "The database changed during batch preparation. Retry from the current workspace.",
            true,
        )
        .suggested(SuggestedActionV1::Retry);
    }
    if error.contains("already in progress") || error.contains("one_live_batch") {
        return CommandErrorV1::new("BATCH_ALREADY_RUNNING", "Another batch operation is already running.", true)
            .suggested(SuggestedActionV1::Retry);
    }
    if error.contains("does not exist") {
        return CommandErrorV1::new(
            "BATCH_SEGMENT_MISSING",
            "A selected segment no longer exists. Reload the library before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip);
    }
    CommandErrorV1::new(
        "BATCH_ADMISSION_FAILED",
        "The normalization batch could not be admitted durably. Open Health before retrying.",
        false,
    )
    .suggested(SuggestedActionV1::OpenHealth)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchNormalizationConfigV1 {
    schema: u8,
    protocol: &'static str,
    build_git_sha: &'static str,
    normalize_numbers: bool,
    verbalize_numbers: bool,
    normalize_hamza: bool,
    remove_diacritics: bool,
    normalizer_version: &'static str,
}

#[tauri::command]
#[specta::specta]
pub async fn batch_normalize(
    ids: Vec<String>,
    operation_id: String,
    app: tauri::AppHandle,
) -> Result<BatchStartedV1, CommandErrorV1> {
    tokio::task::spawn_blocking(move || batch_normalize_blocking(ids, operation_id, app)).await.map_err(|error| {
        tracing::error!(%error, "Normalization admission worker stopped unexpectedly");
        CommandErrorV1::new(
            "BATCH_START_WORKER_FAILED",
            "The normalization batch could not be started. Retry; if it continues, open Health.",
            true,
        )
        .suggested(SuggestedActionV1::OpenHealth)
    })?
}

fn batch_normalize_blocking(
    ids: Vec<String>,
    operation_id: String,
    app: tauri::AppHandle,
) -> Result<BatchStartedV1, CommandErrorV1> {
    let state = app.state::<AppState>();
    let operation = crate::BatchOperation::Normalize;
    let operation_id = canonical_batch_operation_id(&operation_id).map_err(|_| {
        CommandErrorV1::new("INVALID_BATCH_OPERATION_ID", "The batch operation identity is invalid.", false)
    })?;
    if STRICT_RATE_LIMITER.check("batch_normalize").is_err() {
        state.remember_batch_rejection(&operation_id, operation);
        return Err(CommandErrorV1::new("RATE_LIMITED", "Too many batch requests. Wait a moment, then retry.", true)
            .suggested(SuggestedActionV1::Retry));
    }
    if let Err(error) = validate_batch_segment_ids(&ids) {
        state.remember_batch_rejection(&operation_id, operation);
        tracing::warn!(%error, "Rejected invalid normalization batch selection");
        return Err(CommandErrorV1::new(
            "INVALID_BATCH_SELECTION",
            "Select between one and 100,000 unique segments before normalizing.",
            false,
        ));
    }

    let total = ids.len();
    let cancel = state
        .try_start_batch_for_run(&operation_id, operation, total)
        .map_err(|error| batch_normalize_start_error(&error))?;
    let mut claimed_start = crate::ClaimedBatchStart::new(&state, &operation_id, operation);
    let restore_generation =
        crate::database_runtime::capture_restore_generation().map_err(|error| batch_normalize_start_error(&error))?;
    let settings = state.lock_settings().clone();
    let config = crate::normalizer::NormalizationConfig {
        normalize_numbers: settings.auto_normalize,
        verbalize_numbers: settings.verbalize_numbers,
        normalize_hamza: true,
        remove_diacritics: false,
    };
    let config_sha256 = canonical_batch_config_sha256(&BatchNormalizationConfigV1 {
        schema: 1,
        protocol: "durable-batch-normalize-v1",
        build_git_sha: crate::GIT_SHA,
        normalize_numbers: config.normalize_numbers,
        verbalize_numbers: config.verbalize_numbers,
        normalize_hamza: config.normalize_hamza,
        remove_diacritics: config.remove_diacritics,
        normalizer_version: crate::normalizer::NORMALIZER_VERSION,
    })
    .map_err(|error| batch_normalize_start_error(&error))?;
    let normalizer = crate::normalizer::SoraniNormalizer::with_config(config);
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
            kind: crate::db::BatchJobKindV1::Normalize,
            segment_ids: &ids,
            config_sha256: &config_sha256,
            executor,
            cancel: cancel.as_atomic(),
            restore_generation,
        })
        .map_err(|error| batch_normalize_start_error(&error.to_string()))?;
    let mut worker = DurableBatchWorkerGuard::new(worker_app.clone(), worker_operation_id.clone(), operation, lease);
    if !state.mark_batch_durable_admitted(&operation_id, operation) {
        tracing::error!(%operation_id, "Normalization durable-admission phase lost exact start authority");
        claimed_start.disarm();
        worker
            .finish(BatchTerminalIntentV1::Failed { code: "BATCH_START_AUTHORITY_LOST".into() })
            .map_err(|error| batch_normalize_start_error(&error.to_string()))?;
        drop(worker);
        return Err(batch_start_commit_error(crate::BatchStartCommitError::AuthorityLost));
    }
    // From here the unified guard owns both journal and process-gate settlement.
    claimed_start.disarm();
    if usize::try_from(admitted.total).ok() != Some(total) {
        tracing::error!(expected = total, admitted = admitted.total, "Durable normalization admission count mismatch");
        worker
            .finish(BatchTerminalIntentV1::Failed { code: "BATCH_EVIDENCE_INVALID".into() })
            .map_err(|error| batch_normalize_start_error(&error.to_string()))?;
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
                    BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() }
                }
                crate::BatchStartCommitError::AuthorityLost => {
                    BatchTerminalIntentV1::Failed { code: "BATCH_START_AUTHORITY_LOST".into() }
                }
            };
            worker.finish(intent).map_err(|settle_error| {
                tracing::error!(%operation_id, %settle_error, "Admitted normalization could not settle before worker spawn");
                batch_normalize_start_error(&settle_error.to_string())
            })?;
            drop(worker);
            return Err(batch_start_commit_error(error));
        }
    };

    // Do not hold the cancel-slot mutex while `spawn` consumes the closure: on an OS refusal the
    // captured guard drops synchronously and must be free to clear that same slot without deadlock.
    drop(start_commit);
    let app_clone = worker_app;
    let spawn = std::thread::Builder::new().name("cortex-batch-normalize".into()).spawn(move || {
        worker.mark_worker_entered();

        emit_or_log(
            &app_clone,
            "batch-progress",
            serde_json::json!({
                "type": "started", "total": total, "operation": "normalize",
                "operationId": worker_operation_id.as_str()
            }),
        );
        let mut terminal_intent = BatchTerminalIntentV1::Succeeded;
        let mut page_cursor = None;
        'pages: loop {
            if cancel.is_cancelled() {
                terminal_intent = BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() };
                break;
            }
            let page = match worker.lease().and_then(|lease| lease.pending_page(page_cursor)) {
                Ok(items) => items,
                Err(error) => {
                    tracing::error!(%error, "Durable normalization work page could not be read");
                    terminal_intent = BatchTerminalIntentV1::Failed { code: "BATCH_EVIDENCE_INVALID".into() };
                    break;
                }
            };
            if page.is_empty() {
                break;
            }
            for item in page {
                if cancel.is_cancelled() {
                    terminal_intent = BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() };
                    break 'pages;
                }
                let cache_key = format!("{config_sha256}|{}", item.before.segment.raw_transcript);
                let normalized =
                    normalize_cached(cache_key, || normalizer.normalize(&item.before.segment.raw_transcript));
                if cancel.is_cancelled() {
                    terminal_intent = BatchTerminalIntentV1::Cancelled { code: "BATCH_CANCELLED".into() };
                    break 'pages;
                }
                match worker.lease().and_then(|lease| {
                    lease.commit_normalization(item.ordinal, &normalized, crate::normalizer::NORMALIZER_VERSION)
                }) {
                    Ok(
                        BatchItemCommitOutcomeV1::Applied { .. }
                        | BatchItemCommitOutcomeV1::AlreadyApplied { .. }
                        | BatchItemCommitOutcomeV1::Skipped { .. },
                    ) => {}
                    Ok(BatchItemCommitOutcomeV1::Failed { code }) => {
                        terminal_intent = BatchTerminalIntentV1::Failed { code };
                        break 'pages;
                    }
                    Ok(BatchItemCommitOutcomeV1::AlreadyTerminal { state, code }) => {
                        if matches!(state, crate::db::BatchItemStateV1::Failed | crate::db::BatchItemStateV1::Abandoned)
                        {
                            terminal_intent = BatchTerminalIntentV1::Failed {
                                code: code.unwrap_or_else(|| "BATCH_NORMALIZATION_FAILED".into()),
                            };
                            break 'pages;
                        }
                        if state == crate::db::BatchItemStateV1::Pending {
                            terminal_intent = BatchTerminalIntentV1::Failed { code: "BATCH_EVIDENCE_INVALID".into() };
                            break 'pages;
                        }
                    }
                    Err(error) => {
                        tracing::error!(segment_id = %item.segment_id, %error, "Durable normalization item failed");
                        terminal_intent = BatchTerminalIntentV1::Failed { code: "BATCH_NORMALIZATION_FAILED".into() };
                        break 'pages;
                    }
                }
                page_cursor = Some(item.ordinal);
                emit_or_log(
                    &app_clone,
                    "batch-progress",
                    serde_json::json!({
                        "type": "progress", "current": item.ordinal + 1, "total": total,
                        "status": "normalizing", "operation": "normalize",
                        "operationId": worker_operation_id.as_str()
                    }),
                );
            }
        }

        let terminal = match worker.finish(terminal_intent) {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(%error, "Normalization batch could not publish terminal evidence");
                return;
            }
        };
        let outcome = match durable_batch_outcome(&terminal) {
            Ok(Some(outcome)) => outcome,
            Ok(None) => {
                tracing::error!("Normalization terminalization returned a non-terminal status");
                return;
            }
            Err(error) => {
                tracing::error!(%error, "Normalization terminal evidence is outside the public contract");
                return;
            }
        };
        if let Some(app_state) = app_clone.try_state::<AppState>() {
            if !app_state.record_batch_outcome(
                worker_operation_id.as_str(),
                crate::BatchOperation::Normalize,
                outcome.clone(),
            ) {
                tracing::error!("Durable normalization outcome was not accepted by the liveness tracker");
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
                "operation": "normalize",
                "operationId": worker_operation_id.as_str(),
                "error": outcome.error_code.as_ref().map(|code| serde_json::json!({
                    "schema": 1, "code": code, "message": "The normalization batch stopped safely.",
                    "retryable": true
                })),
            }),
        );
    });

    match spawn {
        Ok(_) => Ok(BatchStartedV1 {
            status: BatchStartStatusV1::Started,
            operation_id: operation_id.clone(),
            operation: BatchOperationV1::Normalize,
        }),
        Err(error) => {
            tracing::error!(%error, "OS refused the durable normalization worker");
            Err(CommandErrorV1::new(
                "BATCH_WORKER_START_FAILED",
                "The normalization worker could not start. No pending segment was changed.",
                true,
            )
            .suggested(SuggestedActionV1::Retry))
        }
    }
}

#[cfg(test)]
mod normalizer_cache_tests {
    use super::*;

    #[test]
    fn normalize_cached_recovers_a_poisoned_lock() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = NORMALIZER_CACHE.lock().expect("lock normalizer cache");
            panic!("poison normalizer cache");
        }));

        // A poisoned std::sync::Mutex stays poisoned forever; the cache must recover via
        // unwrap_or_else(|poisoned| poisoned.into_inner()) rather than propagate the panic to
        // every subsequent batch-normalize call for the rest of the process lifetime.
        let value = normalize_cached("poison-recovery-key".to_string(), || "recovered".to_string());
        assert_eq!(value, "recovered");
        assert_eq!(normalize_cached("poison-recovery-key".to_string(), || "stale".to_string()), "recovered");
    }
}
