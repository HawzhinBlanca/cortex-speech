// LOCK HIERARCHY (always acquire in this order):
// 1. state.db
// 2. state.pipeline
// 3. state.normalizer
// 4. state.history
// 5. state.session
// 6. state.cache
// 7. state.settings
// 8. state.data_dir
// 9. state.media_registry

use crate::aligner;
use crate::audio;
use crate::health;
use crate::models;
use crate::pipeline::PipelineEvent;
use crate::quality;
#[cfg(test)]
use crate::recovery::{
    apply_snapshot_pilot_policy, inspect_snapshot_pilot_policy, prepare_named_restore_artifacts,
    preserve_live_asr_runtime_controls, restore_required_snapshot_state_atomic, take_mandatory_pre_restore_snapshot,
    write_named_restore_pending, NamedRestorePending, SnapshotPilotPolicyRestore, NAMED_RESTORE_PENDING_SCHEMA,
};
use crate::recovery::{
    clear_review_pilot_restore_pending, install_snapshot_restore_plan, load_named_restore_pending,
    mark_named_restore_completed, prepare_restore_admission, refuse_bare_restore_during_controlled_pilot,
};
pub(crate) use crate::restore_service::recover_interrupted_named_restore_at_startup;
#[cfg(test)]
use crate::restore_service::{
    has_durable_review_activity, recover_interrupted_named_restore_with_admission, require_active_pilot_policy_binding,
    require_consent_revocation_superset, require_durable_review_history_superset, validate_active_pilot_semantics,
    validate_playback_receipt_semantics, validate_restore_target_semantics, validate_review_compensation_semantics,
    validate_review_effect_semantics,
};
use crate::restore_service::{prepare_and_restore_named_transaction, restore_with_mandatory_snapshot};
use crate::settings::{AppSettings, AsrModelSize};
use crate::throttle::{RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::validation::input as validate;
use crate::AppState;
use crate::{BatchRunDisposition, BatchRunOutcome};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;

// Week-4 decomposition: the export commands live in their own slice. Re-exported so the
// command names (and lib.rs's invoke_handler) are completely unchanged.
mod export;
pub use export::*;
mod model_download;
pub use model_download::*;
mod batch;
pub use batch::*;
mod batch_status;
pub use batch_status::*;
mod durable_batch;
pub(super) use durable_batch::{durable_batch_outcome, publishable_durable_batch_status, DurableBatchWorkerGuard};
mod dataset_analytics;
pub use dataset_analytics::*;
mod gold_eval;
pub use gold_eval::*;
mod transcribe;
pub use transcribe::*;
mod jury;
pub use jury::*;
mod segments_read;
pub use segments_read::*;
mod compensation;
pub use compensation::*;
mod segments_write;
pub use segments_write::*;
mod agentic;
pub use agentic::*;
mod infra;
pub use infra::*;
mod settings;
pub use settings::*;
mod ingest;
pub use ingest::*;
use ingest::{emit_or_log, send_audio_duration_probe_result};
#[cfg(test)]
pub(super) fn get_batch_run_status_blocking_for_test(
    operation_id: String,
    state: &AppState,
) -> Result<crate::ipc_contract::BatchRunStatusResponseV1, crate::ipc_contract::CommandErrorV1> {
    batch_status::get_batch_run_status_blocking(operation_id, state)
}
#[cfg(test)]
pub(super) fn acknowledge_batch_run_blocking_for_test(
    operation_id: String,
    state: &AppState,
) -> Result<bool, crate::ipc_contract::CommandErrorV1> {
    batch_status::acknowledge_batch_run_blocking(operation_id, state)
}
mod recovery_ipc;
pub use recovery_ipc::*;
mod system_ops;
#[cfg(test)]
use crate::database_runtime::RestoreAdmission;
pub use system_ops::*;
#[cfg(test)]
use system_ops::{
    drain_log_lines, segment_awaits_wsl7b, select_wsl_refinement_targets, wsl_log_preview, WSL_LOG_LINE_PREVIEW_CHARS,
};

fn lowercase_sha256(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// Hash one closed, field-ordered result configuration without retaining secrets or private paths
/// in the durable job header. Callers must pass a struct (not an unordered JSON map).
pub(super) fn canonical_batch_config_sha256(value: &impl serde::Serialize) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|bytes| lowercase_sha256(&bytes))
        .map_err(|error| format!("BATCH_CONFIG_SERIALIZATION_FAILED: {error}"))
}

/// Create the one process-attempt identity stored in the immutable batch payload and checked by
/// every effect write. The build script makes an unknown/non-canonical source SHA a compile error.
pub(super) fn new_batch_executor_identity() -> crate::db::BatchExecutorIdentityV1 {
    let token = uuid::Uuid::new_v4();
    crate::db::BatchExecutorIdentityV1 {
        git_sha: crate::GIT_SHA.to_string(),
        token_sha256: lowercase_sha256(token.as_bytes()),
        attempt_generation: 1,
    }
}

pub(super) fn batch_start_commit_error(error: crate::BatchStartCommitError) -> crate::ipc_contract::CommandErrorV1 {
    match error {
        crate::BatchStartCommitError::Cancelled => crate::ipc_contract::CommandErrorV1::new(
            "BATCH_START_CANCELLED",
            "The batch was cancelled before it started. Retry when ready.",
            true,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::Retry),
        crate::BatchStartCommitError::AuthorityLost => crate::ipc_contract::CommandErrorV1::new(
            "BATCH_START_AUTHORITY_LOST",
            "The batch start authority changed unexpectedly. Open Health before retrying.",
            false,
        )
        .suggested(crate::ipc_contract::SuggestedActionV1::OpenHealth),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgenticReadinessCheck {
    pub id: String,
    pub label: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgenticReadiness {
    pub status: String,
    pub ready: bool,
    pub source_reference_models: Vec<String>,
    pub available_hypothesis_models: Vec<String>,
    pub required_hypothesis_models: usize,
    pub checks: Vec<AgenticReadinessCheck>,
}

fn readiness_check(id: &str, label: &str, status: &str, detail: impl Into<String>) -> AgenticReadinessCheck {
    AgenticReadinessCheck {
        id: id.to_string(),
        label: label.to_string(),
        status: status.to_string(),
        detail: detail.into(),
    }
}

fn model_downloaded(model_status: &[serde_json::Value], filename: &str) -> bool {
    model_status.iter().any(|model| {
        model.get("filename").and_then(serde_json::Value::as_str) == Some(filename)
            && model.get("downloaded").and_then(serde_json::Value::as_bool) == Some(true)
    })
}

fn kill_and_reap_child(child: &mut std::process::Child, context: &str) {
    if let Err(error) = child.kill() {
        tracing::warn!("Failed to kill {context}: {error}");
    }
    if let Err(error) = child.wait() {
        tracing::warn!("Failed to reap {context}: {error}");
    }
}

/// Run blocking work (DB scans, serialization, hashing, file I/O) OFF the main/UI thread via a
/// `spawn_blocking` pool, so a slow `#[tauri::command]` can't freeze the window (Tauri runs sync
/// commands on the main thread — the same class that caused the Open/Import freeze). A panic in the
/// task becomes a clean error instead of aborting the process. The closure must own everything it
/// needs (clone `state.db_arc()` etc. BEFORE calling — never borrow `State` across the await).
async fn run_blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("background task failed: {e}"))?
}

/// Probe `wsl --status` with a bounded timeout. `wsl --status` is known to hang indefinitely when
/// the WSL/LxssManager subsystem is wedged; a bare `.output()` would then block the import/jury path
/// (and the check_* command handlers) forever. This mirrors the kill+reap hardening already on the
/// real WSL ASR subprocess: on timeout, kill+reap the child and degrade to "not available".
fn wsl_status_available() -> bool {
    let mut cmd = std::process::Command::new("wsl");
    cmd.arg("--status").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    kill_and_reap_child(&mut child, "timed-out WSL status probe");
                    return false; // wedged WSL -> degrade, never hang
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                kill_and_reap_child(&mut child, "failed WSL status probe");
                return false;
            }
        }
    }
}

pub(crate) fn external_provider_status(settings: &AppSettings) -> serde_json::Value {
    let Some(script) = settings.external_asr_script_path() else {
        return serde_json::json!({
            "available": false,
            "message": "No external ASR provider script configured"
        });
    };

    let wsl_available = wsl_status_available();
    serde_json::json!({
        "available": wsl_available,
        "script": script,
        "message": if wsl_available {
            "WSL is available; provider script will be used for external ASR"
        } else {
            "WSL is not available or not healthy on this machine"
        }
    })
}

fn build_agentic_readiness(
    settings: &AppSettings,
    model_status: &[serde_json::Value],
    external_provider: &serde_json::Value,
) -> AgenticReadiness {
    let mut checks = Vec::new();
    let source_reference_models = settings.source_reference_models();
    if !settings.jury_cloud_opt_in {
        // Cloud whole-file references are an OPTIONAL enhancement the user has deliberately left off
        // (offline-first). The selected primary ASR remains fully functional without them, so this is
        // the chosen configuration — NOT a degradation that should nag on every import.
        //
        // But it is not "ready" either, and saying so was a green tick the feature had not earned: a
        // reviewer scanning the panel could not tell "source references are covering your data" from
        // "source references are off", because both printed `ready` in the same emerald. Deep-audit
        // 2026-08-05 named this exactly — "not required in this mode is not the same state as ready".
        //
        // `not_required` keeps the non-nagging intent (the aggregate below treats anything that is
        // neither blocked nor degraded as ready, so the overall verdict is unchanged) while refusing to
        // claim coverage that does not exist. Pinned by
        // `disabled_cloud_reports_not_required_not_ready_but_keeps_the_overall_verdict_ready`.
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "not_required",
            "Offline mode: cloud whole-file references are off by choice (an optional cross-check). Primary ASR readiness is reported separately. Enable jury cloud opt-in to add Gemini whole-file references.",
        ));
    } else if settings.llm_api_key.trim().is_empty() {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "blocked",
            "Gemini API key is not loaded in this session, so source-reference transcription would fail before chunking.",
        ));
    } else if source_reference_models.is_empty() {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "blocked",
            "No owner-approved source-reference model is configured.",
        ));
    } else {
        checks.push(readiness_check(
            "source_reference",
            "Whole-file source references",
            "ready",
            format!("Configured source-reference models: {}", source_reference_models.join(", ")),
        ));
    }

    let ctc_300m_ready = model_downloaded(model_status, models::OMNIASR_CTC_300M_MODEL)
        && model_downloaded(model_status, models::OMNIASR_CTC_300M_TOKENS);
    let ctc_1b_ready = model_downloaded(model_status, models::OMNIASR_CTC_1B_MODEL)
        && model_downloaded(model_status, models::OMNIASR_CTC_1B_TOKENS);
    let wsl_ready = external_provider.get("available").and_then(serde_json::Value::as_bool).unwrap_or(false)
        && settings.external_asr_script_path().is_some();

    let auxiliary_mode = settings.multi_engine_hypotheses && settings.asr_model_size != AsrModelSize::WSL7B;
    let (primary_model_id, primary_label, primary_ready, primary_detail) = match &settings.asr_model_size {
        AsrModelSize::WSL7B => {
            let detail = if wsl_ready {
                "WSL external ASR provider is configured and WSL reports healthy.".to_string()
            } else {
                external_provider
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("WSL external ASR provider is not ready.")
                    .to_string()
            };
            ("omniasr-wsl-7b", "Primary OmniASR 7B", wsl_ready, detail)
        }
        AsrModelSize::CTC1B => (
            "omniasr-ctc-1b",
            "Selected OmniASR CTC 1B",
            ctc_1b_ready,
            if ctc_1b_ready {
                "The explicitly selected CTC 1B model and tokens are available.".to_string()
            } else {
                "The explicitly selected CTC 1B model or tokens are missing.".to_string()
            },
        ),
        AsrModelSize::CTC300M => (
            "omniasr-ctc-300m",
            "Selected OmniASR CTC 300M",
            ctc_300m_ready,
            if ctc_300m_ready {
                "The explicitly selected CTC 300M model and tokens are available.".to_string()
            } else {
                "The explicitly selected CTC 300M model or tokens are missing.".to_string()
            },
        ),
    };

    // Report engines that this configuration can actually invoke, not every optional model merely
    // installed on disk. In champion-only mode an installed CTC model is not an active hypothesis
    // source and must not make the UI imply that it will run.
    let mut available_hypothesis_models = Vec::new();
    if primary_ready {
        available_hypothesis_models.push(primary_model_id.to_string());
    }
    if auxiliary_mode {
        for (ready, model_id) in
            [(wsl_ready, "omniasr-wsl-7b"), (ctc_1b_ready, "omniasr-ctc-1b"), (ctc_300m_ready, "omniasr-ctc-300m")]
        {
            if ready && !available_hypothesis_models.iter().any(|existing| existing == model_id) {
                available_hypothesis_models.push(model_id.to_string());
            }
        }
    }

    if primary_ready {
        checks.push(readiness_check("primary_asr", primary_label, "ready", primary_detail));
    } else {
        checks.push(readiness_check(
            "primary_asr",
            primary_label,
            "blocked",
            format!(
                "{primary_detail} The selected primary engine is unavailable; refusing to substitute another engine."
            ),
        ));
    }

    let required_hypothesis_models = quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE;
    if !auxiliary_mode {
        checks.push(readiness_check(
            "hypothesis_coverage",
            "Multi-model hypothesis coverage",
            "not_required",
            format!(
                "Single-engine mode: {primary_model_id} is the only automatic ASR source. Optional engines will not run. Training/export promotion still validates its required stored corroboration separately (minimum {required_hypothesis_models})."
            ),
        ));
    } else if available_hypothesis_models.len() >= required_hypothesis_models {
        checks.push(readiness_check(
            "hypothesis_coverage",
            "Multi-model hypothesis coverage",
            "ready",
            format!("Available hypothesis models: {}", available_hypothesis_models.join(", ")),
        ));
    } else {
        checks.push(readiness_check(
            "hypothesis_coverage",
            "Multi-model hypothesis coverage",
            "blocked",
            format!(
                "Only {} usable hypothesis model(s) are ready; at least {required_hypothesis_models} are required for automatic training-grade promotion.",
                available_hypothesis_models.len()
            ),
        ));
    }

    let status = if checks.iter().any(|check| check.status == "blocked") {
        "blocked"
    } else if checks.iter().any(|check| check.status == "degraded") {
        "degraded"
    } else {
        "ready"
    }
    .to_string();
    AgenticReadiness {
        ready: status == "ready",
        status,
        source_reference_models,
        available_hypothesis_models,
        required_hypothesis_models,
        checks,
    }
}

pub(crate) fn build_agentic_readiness_snapshot(
    settings: &AppSettings,
    model_status: &[serde_json::Value],
    external_provider: &serde_json::Value,
) -> serde_json::Value {
    match serde_json::to_value(build_agentic_readiness(settings, model_status, external_provider)) {
        Ok(value) => value,
        Err(error) => serde_json::json!({
            "status": "blocked",
            "ready": false,
            "sourceReferenceModels": settings.source_reference_models(),
            "availableHypothesisModels": [],
            "requiredHypothesisModels": quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE,
            "checks": [{
                "id": "readiness_snapshot",
                "label": "Agentic readiness snapshot",
                "status": "blocked",
                "detail": format!("Could not serialize agentic readiness snapshot: {error}")
            }]
        }),
    }
}

/// Restrict review evidence to the ASR mode the owner selected. In champion mode, hypotheses left by
/// older builds (300M/1B/MMS/Scribe) are historical artifacts, not voters. If a pre-provenance 7B row
/// has no hypothesis record, synthesize the one honest vote from the segment's persisted champion
/// transcript so review still works without allowing stale engines back into the decision.
/// The provenance id written by the PRE-REGISTRY champion.
///
/// Production no longer names the champion by string — identity is content-addressed, and
/// `pipeline::CHAMPION_MODEL_ID` is `#[cfg(test)]` for exactly that reason. But rows drafted before
/// the registry existed still carry this id on disk, and champion review must keep recognising them
/// as champion-produced or it would hide the evidence for most of the existing corpus. This is
/// historical RECOGNITION only; nothing selects or serves a model by this string.
const LEGACY_CHAMPION_MODEL_ID: &str = "omniasr-wsl-7b";

/// Was this segment drafted by a champion-family model? Answers the DB question
/// [`hypotheses_for_selected_asr`] deliberately does not ask itself, so that filter stays pure.
/// The legacy constant covers rows written before the registry existed; the registry covers the rest.
/// Unknown provenance is NOT champion — fail closed.
fn segment_recorded_model_is_champion(db: &crate::db::Database, segment: &crate::db::SpeechSegment) -> bool {
    let Some(recorded) = segment.model_version_id.as_deref().map(str::trim).filter(|id| !id.is_empty()) else {
        return false;
    };
    if recorded == LEGACY_CHAMPION_MODEL_ID {
        return true;
    }
    match crate::registry::is_family_model(db, recorded, crate::deployment::OMNIASR_7B_FAMILY) {
        Ok(is_champion) => is_champion,
        Err(error) => {
            // Fail CLOSED (hide the votes) but never silently: a registry read that fails here would
            // otherwise look identical to "this row was drafted by a weaker engine".
            tracing::error!("champion-family lookup failed for model {recorded}: {error}");
            false
        }
    }
}

fn hypotheses_for_selected_asr(
    selected: &crate::settings::AsrModelSize,
    segment: &crate::db::SpeechSegment,
    mut hypotheses: Vec<crate::db::SegmentHypothesis>,
    recorded_model_is_champion: bool,
) -> Vec<crate::db::SegmentHypothesis> {
    if *selected != crate::settings::AsrModelSize::WSL7B {
        return hypotheses;
    }

    let Some(recorded_model_id) = segment.model_version_id.as_deref().filter(|id| !id.trim().is_empty()) else {
        // Without a persisted producing-version id, choosing the *current* champion would rewrite
        // history after promotion. Return no attributable vote instead of inventing provenance.
        return Vec::new();
    };
    // CHAMPION SUPREMACY (canon). Matching the row's own producing model is the right unit of
    // provenance, but on its own it re-admits the very thing the fixed-string filter excluded: a clip
    // drafted by a weaker engine BEFORE WSL7B was selected carries that engine's id, so a per-row
    // match would surface its hypotheses during champion review. This library contains exactly such
    // rows — 494/494 clips were once silently drafted by `finetuned-mms-ckb` while WSL7B was selected.
    // A non-champion producer contributes NO auxiliary vote, as before.
    if !recorded_model_is_champion {
        return Vec::new();
    }
    hypotheses.retain(|hypothesis| {
        hypothesis.model_id == recorded_model_id
            && !hypothesis.transcript.trim().is_empty()
            && !crate::quality::is_placeholder_transcript(&hypothesis.transcript)
    });
    if hypotheses.is_empty()
        && !segment.raw_transcript.trim().is_empty()
        && !crate::quality::is_placeholder_transcript(&segment.raw_transcript)
    {
        hypotheses.push(crate::db::SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: recorded_model_id.to_string(),
            transcript: segment.raw_transcript.clone(),
            confidence: segment.confidence,
        });
    }
    hypotheses
}

/// Offline best-of-N consensus DRAFT for a segment: an ability-weighted vote over its ASR hypotheses
/// (no cloud) so review can start from a transcript better than any single model and highlight exactly
/// where the models disagreed. Empty when the segment has no hypotheses to vote over.
#[tauri::command]
#[specta::specta]
pub fn get_segment_consensus(
    state: State<'_, AppState>,
    segment_id: String,
) -> Result<crate::ipc_contract::SegmentConsensusV1, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER
        .check("get_segment_consensus")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("get_segment_consensus"))?;
    validate::validate_identifier(&segment_id).map_err(|_| crate::ipc_contract::invalid_segment_id_error())?;
    let selected = state.lock_settings().asr_model_size.clone();
    let (segment, hyps, recorded_is_champion) = {
        let db = state.lock_db();
        let segment = db
            .get_segment_by_id(&segment_id)
            .map_err(|error| {
                tracing::warn!("Consensus segment read failed: {error}");
                crate::ipc_contract::public_consensus_error(&error.to_string())
            })?
            .ok_or_else(|| crate::ipc_contract::public_consensus_error("Segment no longer exists"))?;
        let hypotheses = db.get_hypotheses_for_segment(&segment_id).map_err(|error| {
            tracing::warn!("Consensus hypothesis read failed: {error}");
            crate::ipc_contract::public_consensus_error(&error.to_string())
        })?;
        // Answered while the lock is held; the filter itself stays pure and DB-free.
        let is_champion = segment_recorded_model_is_champion(&db, &segment);
        (segment, hypotheses, is_champion)
    };
    let hyps = hypotheses_for_selected_asr(&selected, &segment, hyps, recorded_is_champion);
    // Distinct producing engines, in first-seen order, straight from the recorded hypotheses (never
    // inferred) so the review badge can honestly say which model(s) made the draft.
    let mut models: Vec<String> = Vec::new();
    for h in &hyps {
        let id = h.model_id.trim();
        if !id.is_empty() && !models.iter().any(|m| m == id) {
            models.push(id.to_string());
        }
    }
    let words = crate::quality::irt::segment_consensus_words(&hyps);
    let draft = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    let model_count = words.first().map(|w| w.total_models).unwrap_or(0);
    let (min_agreement, mean_agreement) = if words.is_empty() {
        (0.0, 0.0)
    } else {
        let min = words.iter().map(|w| w.agreement).fold(f64::INFINITY, f64::min);
        let mean = words.iter().map(|w| w.agreement).sum::<f64>() / words.len() as f64;
        (min, mean)
    };
    Ok(crate::ipc_contract::SegmentConsensusV1 {
        draft,
        words: words.into_iter().map(crate::ipc_contract::ConsensusWordV1::from).collect(),
        model_count,
        min_agreement,
        mean_agreement,
        models,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn merge_dataset_json(
    json_content: String,
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::MergeDatasetResultV1, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("merge_dataset_json")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("merge_dataset_json"))?;
    // Sanity-bound the pasted payload (generous enough for a real multi-segment dataset) so a
    // pathological blob can't drive an unbounded parse — matching the size guard every other
    // JSON-accepting command applies.
    validate::validate_text(&json_content, 50_000_000, "Dataset JSON")
        .map_err(|_| crate::ipc_contract::invalid_dataset_payload_error())?;
    let database = state.db_runtime();
    let result = run_blocking(move || {
        let mutation = database.begin_mutation().map_err(|error| error.to_string())?;
        let db = database.lock_after_mutation(&mutation).unwrap_or_else(|p| p.into_inner());
        let (created, updated) = db.merge_dataset_json(&json_content).map_err(|e| e.to_string())?;
        Ok(crate::ipc_contract::MergeDatasetResultV1 { created, updated })
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner dataset-merge command failed: {error}");
        crate::ipc_contract::public_owner_data_error(crate::ipc_contract::OwnerDataOperationV1::MergeDataset, &error)
    })
}

/// Recent durable jobs (newest first) for a UI activity surface — a long op bracketed via
/// `Database::run_tracked` shows here as running/succeeded/failed, and a crash residue reaped at
/// startup shows as failed/INTERRUPTED. Cheap read; safe to poll.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobStateV1 {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl From<crate::jobs::JobState> for JobStateV1 {
    fn from(state: crate::jobs::JobState) -> Self {
        match state {
            crate::jobs::JobState::Queued => Self::Queued,
            crate::jobs::JobState::Running => Self::Running,
            crate::jobs::JobState::Succeeded => Self::Succeeded,
            crate::jobs::JobState::Failed => Self::Failed,
            crate::jobs::JobState::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JobV1 {
    pub id: String,
    pub kind: String,
    pub state: JobStateV1,
    pub progress: f64,
    pub completed: i64,
    pub total: Option<i64>,
    pub error_code: Option<String>,
}

impl From<crate::jobs::Job> for JobV1 {
    fn from(job: crate::jobs::Job) -> Self {
        Self {
            id: job.id,
            kind: job.kind,
            state: job.state.into(),
            progress: job.progress,
            completed: job.completed,
            total: job.total,
            error_code: job.error_code,
        }
    }
}

fn public_job_read_error(_private_detail: &str) -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(
        "JOB_CENTER_UNAVAILABLE",
        "The Job Center could not read durable operation status. Open Health for recovery options.",
        true,
    )
    .suggested(crate::ipc_contract::SuggestedActionV1::OpenHealth)
}

fn public_job_rate_limited_error() -> crate::ipc_contract::CommandErrorV1 {
    crate::ipc_contract::CommandErrorV1::new(
        "JOB_CENTER_BUSY",
        "The Job Center is refreshing too quickly. Retry in a moment.",
        true,
    )
    .suggested(crate::ipc_contract::SuggestedActionV1::Retry)
}

#[tauri::command]
#[specta::specta]
pub async fn get_jobs(state: State<'_, AppState>) -> Result<Vec<JobV1>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER.check("get_jobs").map_err(|_| public_job_rate_limited_error())?;
    let store = state.job_store();
    run_blocking(move || {
        store.list_recent(50).map(|jobs| jobs.into_iter().map(JobV1::from).collect()).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| public_job_read_error(&error))
}

#[cfg(test)]
mod typed_job_ipc_tests {
    use super::*;

    #[test]
    fn job_wire_state_is_exact_and_private_failures_are_scrubbed() {
        let job = JobV1::from(crate::jobs::Job {
            id: "job-1".to_string(),
            kind: "import".to_string(),
            state: crate::jobs::JobState::Failed,
            progress: 0.5,
            completed: 1,
            total: Some(2),
            error_code: Some("INTERRUPTED".to_string()),
        });
        let wire = serde_json::to_value(job).expect("serialize public job");
        assert_eq!(wire["state"], "failed");
        assert_eq!(wire["errorCode"], "INTERRUPTED");

        let hostile = public_job_read_error(r"SQL token=secret D:\private\jobs.sqlite");
        let wire = serde_json::to_string(&hostile).expect("serialize public job error");
        assert!(wire.contains("JOB_CENTER_UNAVAILABLE"));
        assert!(wire.contains("openHealth"));
        for forbidden in ["SQL", "token", "secret", "D:\\", "private", "jobs.sqlite"] {
            assert!(!wire.contains(forbidden));
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Phase 1 — Gold-Set Eval Harness
// ════════════════════════════════════════════════════════════════════════════

/// Response for `build_scorecard`: the structured scorecard plus a ready-to-paste
/// Markdown rendering (for a README / HuggingFace model card).
#[derive(serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardResponse {
    pub scorecard: crate::ipc_contract::ScorecardV1,
    pub markdown: String,
}

/// Build a reproducible accuracy scorecard from already-computed gold-eval results:
/// micro WER/CER with bootstrap confidence intervals, plus an optional MAPSSWE
/// significance comparison against a baseline run. Pure and deterministic.
#[tauri::command]
#[specta::specta]
pub fn build_scorecard(
    system: crate::ipc_contract::EvalRunResultV1,
    baseline: Option<crate::ipc_contract::EvalRunResultV1>,
) -> Result<ScorecardResponse, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER
        .check("build_scorecard")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("build_scorecard"))?;
    let system: crate::eval::EvalRunResult = system.into();
    let baseline = baseline.map(crate::eval::EvalRunResult::from);
    let scorecard = crate::scorecard::build_scorecard(&system, baseline.as_ref(), Default::default());
    let markdown = crate::scorecard::render_markdown(&scorecard);
    Ok(ScorecardResponse { scorecard: scorecard.into(), markdown })
}

#[tauri::command]
#[specta::specta]
pub fn list_eval_runs(
    state: State<'_, AppState>,
) -> Result<Vec<crate::ipc_contract::EvalRunV1>, crate::ipc_contract::CommandErrorV1> {
    RATE_LIMITER
        .check("list_eval_runs")
        .map_err(|_| crate::ipc_contract::owner_analysis_rate_limited("list_eval_runs"))?;
    let db = state.lock_db();
    crate::eval::list_eval_runs(&db).map(|runs| runs.into_iter().map(Into::into).collect()).map_err(|error| {
        tracing::warn!("Owner evaluation-history read failed: {error}");
        crate::ipc_contract::public_owner_analysis_error(
            crate::ipc_contract::OwnerAnalysisOperationV1::EvalHistory,
            &error.to_string(),
        )
    })
}

#[tauri::command]
pub fn list_gold_segments(state: State<'_, AppState>) -> Result<Vec<crate::eval::GoldSegment>, String> {
    RATE_LIMITER.check("list_gold_segments")?;
    let db = state.lock_db();
    crate::eval::list_gold_segments(&db).map_err(|e| e.to_string())
}

// ════════════════════════════════════════════════════════════════════════════
// Phase 2 — T0 Disagreement Gate + Jury Infrastructure
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub fn couch_review_status() -> Result<crate::couch::CouchStatus, String> {
    RATE_LIMITER.check("couch_review_status")?;
    Ok(crate::couch::status())
}

/// Per-reviewer spot-check scores — how each remote reviewer did on clips whose answer was already
/// known (Migration v44, docs/REMOTE_REVIEW_PLAN.md §2.1).
///
/// Worst `noticed` rate first, because the finding this exists to surface is a reviewer who is not
/// listening, and burying them under the diligent ones would defeat the point.
#[tauri::command]
pub fn spot_check_report(state: State<'_, AppState>) -> Result<Vec<crate::db::SpotCheckScore>, String> {
    RATE_LIMITER.check("spot_check_report")?;
    state.lock_db().spot_check_report().map_err(|e| e.to_string())
}

/// Per-reviewer throughput from the append-only review trail (Migration v45).
///
/// Distinct from `stats.rs`'s median seconds-per-decision, deliberately: that one orders
/// `decision_log` GLOBALLY, which is correct for a single reviewer and meaningless for a team — with
/// several people working at once it would time the gap between two DIFFERENT humans' decisions. This
/// one partitions per reviewer, so the existing metric keeps its meaning and this one is honest.
#[tauri::command]
pub fn reviewer_throughput(state: State<'_, AppState>) -> Result<Vec<crate::db::ReviewerThroughput>, String> {
    RATE_LIMITER.check("reviewer_throughput")?;
    state.lock_db().reviewer_throughput().map_err(|e| e.to_string())
}

/// Write the two-rater agreement sample beside the library and return where it went
/// (docs/REMOTE_REVIEW_PLAN.md §2.4).
///
/// Deliberately exports the TSV rather than computing kappa here. `scripts/agreement_kappa.py`
/// already implements the statistic and is unit-tested against the textbook kappa=0.40 example; a
/// second implementation in Rust would be an unverified copy of a verified thing, and under the
/// honesty law a metric must come from a real run of the real harness. This command produces the
/// evidence; the harness produces the number.
#[tauri::command]
pub fn export_agreement_sample(state: State<'_, AppState>) -> Result<Option<crate::db::AgreementExport>, String> {
    RATE_LIMITER.check("export_agreement_sample")?;
    let (sample, db_path) = {
        let db = state.lock_db();
        (db.agreement_sample().map_err(|e| e.to_string())?, db.path().to_string())
    };
    let Some(mut sample) = sample else {
        return Ok(None); // nothing double-reviewed yet — not an error, just no evidence
    };
    let out = std::path::Path::new(&db_path)
        .parent()
        .ok_or_else(|| "library path has no parent directory".to_string())?
        .join("agreement_sample.tsv");
    std::fs::write(&out, &sample.tsv).map_err(|e| format!("could not write {}: {e}", out.display()))?;
    sample.path = out.to_string_lossy().to_string();
    Ok(Some(sample))
}

/// Consent gate for outbound cloud-LLM channels that POST private, transcript-derived data (e.g. the
/// DPO preference-pair export). Same explicit opt-in the LLM-refine path requires — never ship the
/// user's data to a cloud endpoint without it, even though the endpoint is also allow-list-validated.
pub(crate) fn require_cloud_llm_consent(state: &AppState) -> Result<(), String> {
    if state.lock_settings().cloud_llm_opt_in {
        Ok(())
    } else {
        Err("Cloud LLM opt-in is required for this cloud upload. Enable it in Settings.".into())
    }
}

/// Count of in-flight BACKGROUND DB writers that use their OWN dedicated connection (NOT the global db
/// Mutex) and may run OUTSIDE the import/batch guards — so they escape the db-Mutex serialization the
/// restore relies on, and `AppState::writers_active` must consult this to fence a restore while any is
/// mid-write (R3). Registrants: the jury writers (run_jury_pipeline / run_t2_for_segment /
/// run_dpo_update + the post-import adjudication thread) and the detached background-alignment thread.
/// A COUNTER, not a bool: these writers may legitimately
/// overlap, and a bool would clear the fence when the FIRST of two concurrent writers finished. This is
/// the one place a NEW dedicated-connection writer registers — extend by taking a guard, not by growing
/// the writers_active() || chain (the recurring "forgot the new writer" bug this closes twice over).
pub(crate) static BG_DB_WRITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// RAII registration for a background DB writer: increments [`BG_DB_WRITERS`] on construction and
/// decrements on drop, so the restore fence is armed for exactly the writer's lifetime — even if the
/// work panics (Drop still runs on unwind). ZST, so it is `Send` and can be moved into a worker thread.
pub(crate) struct BgDbWriterGuard;
impl BgDbWriterGuard {
    pub(crate) fn new() -> Self {
        BG_DB_WRITERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}
impl Drop for BgDbWriterGuard {
    fn drop(&mut self) {
        BG_DB_WRITERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// True while any background DB writer is in flight — consulted by the restore fence.
pub(crate) fn bg_db_writers_active() -> bool {
    BG_DB_WRITERS.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// Undo a review-inbox `flag()`: clear the escalation the flag set. Distinct from clear_human_decision
/// (which reopens a decided row by SETTING escalated=1); this is the inverse of flag.
#[tauri::command]
pub fn clear_escalation(_state: State<'_, AppState>, segment_id: String) -> Result<(), String> {
    RATE_LIMITER.check("clear_escalation")?;
    validate::validate_identifier(&segment_id)?;
    Err("EXACT_FLAG_UNDO_REQUIRED: identity-free escalation clearing is retired; use the immutable flag effect and an operation UUID".into())
}

#[tauri::command]
pub fn get_few_shot_examples(
    state: State<'_, AppState>,
    segment_id: String,
    k: usize,
) -> Result<Vec<crate::jury::FewShotExample>, String> {
    RATE_LIMITER.check("get_few_shot_examples")?;
    validate::validate_identifier(&segment_id)?;
    let db = state.lock_db();
    crate::jury::get_few_shot_examples(&db, &segment_id, k).map_err(|e| e.to_string())
}

// ── Items 1 & 2: Full jury pipeline + T2 direct command ─────────────────────

/// `run_jury_pipeline` — chains T0 → T1 → T2 in a single call.
///
/// For each segment:
///   1. T0 IRT gate: if consensus is strong, auto-accept and skip.
///   2. T1 text judge (lexicon + perplexity): if score ≥ t1_threshold, commit.
///   3. T2 Gemini audio listener (only if cloud_opt_in=true & api_key set):
///      self-consistency N=3 vote.  No majority → escalate to human inbox.
///   4. Any remaining escalations write verdict="escalated" to DB.
///
/// Build a reference-aware candidate selection report for one segment.
fn reference_selection_text_key(report: &crate::agentic::CandidateSelectionReport) -> String {
    crate::wer::normalize_for_metrics(&report.selected_transcript)
}

fn reference_reports_have_commit_consensus(reports: &[crate::agentic::CandidateSelectionReport]) -> bool {
    if reports.is_empty() || !reports.iter().all(|report| report.should_commit) {
        return false;
    }

    let first_key = reference_selection_text_key(&reports[0]);
    !first_key.is_empty() && reports.iter().all(|report| reference_selection_text_key(report) == first_key)
}

fn best_reference_report(
    reports: &[crate::agentic::CandidateSelectionReport],
) -> Option<crate::agentic::CandidateSelectionReport> {
    reports.iter().cloned().max_by(|a, b| {
        a.selected_score
            .partial_cmp(&b.selected_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.margin.partial_cmp(&b.margin).unwrap_or(std::cmp::Ordering::Equal))
    })
}

fn reference_agreement_reports(
    reports: &[crate::agentic::CandidateSelectionReport],
) -> Vec<crate::agentic::ReferenceAgreementReport> {
    reports
        .iter()
        .map(|report| crate::agentic::ReferenceAgreementReport {
            reference_model_id: report.reference_model_id.clone(),
            selected_model_id: report.selected_model_id.clone(),
            selected_transcript: report.selected_transcript.clone(),
            selected_score: report.selected_score,
            confidence: report.confidence,
            margin: report.margin,
            should_commit: report.should_commit,
            reference_window_overlap: report.scores.first().map(|score| score.reference_window_overlap).unwrap_or(0.0),
            reference_global_overlap: report.scores.first().map(|score| score.reference_global_overlap).unwrap_or(0.0),
        })
        .collect()
}

fn hypothesis_coverage_guard(
    seg: &crate::db::SpeechSegment,
    hypotheses: &[crate::db::SegmentHypothesis],
) -> Option<crate::agentic::CandidateSelectionReport> {
    let coverage = crate::quality::hypothesis_coverage_for_model_outputs(hypotheses);
    if coverage.passes_minimum {
        return None;
    }

    let present_display =
        if coverage.non_empty_models.is_empty() { "none".to_string() } else { coverage.non_empty_models.join(", ") };
    Some(crate::agentic::CandidateSelectionReport {
        reference_model_id: Some("multi-model-hypothesis-coverage-guard".into()),
        selected_model_id: "multi-model-hypothesis-coverage-guard".into(),
        selected_transcript: seg
            .verdict_transcript
            .clone()
            .or_else(|| seg.normalized_transcript.clone())
            .unwrap_or_else(|| seg.raw_transcript.clone()),
        selected_score: 0.0,
        confidence: 0.0,
        margin: 0.0,
        should_commit: false,
        positional_window: false,
        rationale: format!(
            "Multi-model hypothesis coverage guard blocked automatic adjudication because fewer than 2 non-empty model hypotheses were available before jury (required {}). Present non-empty model hypotheses: {present_display}.",
            coverage.minimum_non_empty_model_count
        ),
        reference_window_preview: String::new(),
        scores: Vec::new(),
        reference_agreement: Vec::new(),
    })
}

fn source_reference_has_stored_audio_identity(reference: &crate::db::SourceTranscriptRecord) -> bool {
    reference.audio_content_hash.as_deref().map(|hash| !hash.trim().is_empty()).unwrap_or(false)
        || reference.audio_size_bytes.is_some()
}

fn source_reference_current_audio_identity(
    audio_path: &str,
    cache: &mut std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>>,
) -> Option<crate::pipeline::SourceAudioIdentity> {
    if let Some(identity) = cache.get(audio_path) {
        return identity.clone();
    }

    let identity = match crate::pipeline::source_audio_identity(Path::new(audio_path)) {
        Ok(identity) => Some(identity),
        Err(error) => {
            tracing::warn!(
                "Cannot verify source-reference audio identity for {} before jury adjudication: {}",
                audio_path,
                error
            );
            None
        }
    };
    cache.insert(audio_path.to_string(), identity.clone());
    identity
}

fn source_reference_matches_current_audio(
    reference: &crate::db::SourceTranscriptRecord,
    identity_cache: &mut std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>>,
) -> bool {
    if !source_reference_has_stored_audio_identity(reference) {
        return false;
    }

    let Some(current_identity) = source_reference_current_audio_identity(&reference.audio_path, identity_cache) else {
        return false;
    };

    reference.audio_content_hash.as_deref() == Some(current_identity.content_hash.as_str())
        && reference.audio_size_bytes == Some(current_identity.size_bytes)
}

fn filter_source_references_for_current_audio(
    references: Vec<crate::db::SourceTranscriptRecord>,
    identity_cache: &mut std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>>,
) -> (Vec<crate::db::SourceTranscriptRecord>, Vec<String>) {
    let mut usable = Vec::new();
    let mut stale_models = std::collections::BTreeSet::new();

    for reference in references {
        if source_reference_matches_current_audio(&reference, identity_cache) {
            usable.push(reference);
        } else {
            stale_models.insert(reference.model_id);
        }
    }

    (usable, stale_models.into_iter().collect())
}

fn source_reference_coverage_guard(
    settings: &crate::settings::AppSettings,
    seg: &crate::db::SpeechSegment,
    references: &[crate::db::SourceTranscriptRecord],
    stale_reference_models: &[String],
) -> Option<crate::agentic::CandidateSelectionReport> {
    let should_require_coverage = settings.jury_cloud_opt_in || !references.is_empty();
    if !should_require_coverage && stale_reference_models.is_empty() {
        return None;
    }

    let required_models = settings.source_reference_models();
    let stale_required_models = stale_reference_models
        .iter()
        .filter(|model| required_models.iter().any(|required| required == *model))
        .cloned()
        .collect::<Vec<_>>();
    if !stale_required_models.is_empty() {
        return Some(crate::agentic::CandidateSelectionReport {
            reference_model_id: Some(format!(
                "source-reference-audio-identity-guard:stale:{}",
                stale_required_models.join("+")
            )),
            selected_model_id: "source-reference-audio-identity-guard".into(),
            selected_transcript: seg
                .verdict_transcript
                .clone()
                .or_else(|| seg.normalized_transcript.clone())
                .unwrap_or_else(|| seg.raw_transcript.clone()),
            selected_score: 0.0,
            confidence: 0.0,
            margin: 0.0,
            should_commit: false,
            positional_window: false,
            rationale: format!(
                "Source-reference audio identity guard blocked automatic adjudication because stored whole-file references are missing audio identity or no longer match the current source audio bytes: {}. Recreate source references before jury.",
                stale_required_models.join(", ")
            ),
            reference_window_preview: String::new(),
            scores: Vec::new(),
            reference_agreement: Vec::new(),
        });
    }

    let present_models = references
        .iter()
        .filter(|reference| !reference.transcript_text.trim().is_empty())
        .map(|reference| reference.model_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let missing_models =
        required_models.iter().filter(|model| !present_models.contains(*model)).cloned().collect::<Vec<_>>();
    if missing_models.is_empty() {
        return None;
    }

    let present_display = if present_models.is_empty() {
        "none".to_string()
    } else {
        present_models.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    Some(crate::agentic::CandidateSelectionReport {
        reference_model_id: Some(format!("source-reference-coverage-guard:missing:{}", missing_models.join("+"))),
        selected_model_id: "source-reference-coverage-guard".into(),
        selected_transcript: seg
            .verdict_transcript
            .clone()
            .or_else(|| seg.normalized_transcript.clone())
            .unwrap_or_else(|| seg.raw_transcript.clone()),
        selected_score: 0.0,
        confidence: 0.0,
        margin: 0.0,
        should_commit: false,
        positional_window: false,
        rationale: format!(
            "Source-reference coverage guard blocked automatic adjudication because configured whole-file reference models are missing or empty: {}. Required source reference models: {}. Present non-empty source reference models: {}.",
            missing_models.join(", "),
            required_models.join(", "),
            present_display
        ),
        reference_window_preview: String::new(),
        scores: Vec::new(),
        reference_agreement: Vec::new(),
    })
}

fn reference_selection_for_segment(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    seg: &crate::db::SpeechSegment,
    hypotheses: &[crate::db::SegmentHypothesis],
    duration_cache: &mut std::collections::HashMap<String, Option<i64>>,
    identity_cache: &mut std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>>,
) -> Result<Option<crate::agentic::CandidateSelectionReport>, String> {
    let references = db.get_source_transcripts_for_audio(&seg.audio_path).map_err(|e| e.to_string())?;
    let (references, stale_reference_models) = filter_source_references_for_current_audio(references, identity_cache);
    if let Some(report) = source_reference_coverage_guard(settings, seg, &references, &stale_reference_models) {
        return Ok(Some(report));
    }
    if references.is_empty() {
        return Ok(None);
    }

    let duration = if let Some(cached) = duration_cache.get(&seg.audio_path) {
        *cached
    } else {
        let probed = crate::audio::get_duration_ms(Path::new(&seg.audio_path)).ok();
        duration_cache.insert(seg.audio_path.clone(), probed);
        probed
    };

    let mut reports: Vec<crate::agentic::CandidateSelectionReport> = Vec::new();
    for reference in references {
        if reference.transcript_text.trim().is_empty() {
            continue;
        }
        let Some(mut report) = crate::agentic::select_best_candidate_against_reference(
            seg,
            &reference.transcript_text,
            duration,
            hypotheses,
        ) else {
            continue;
        };
        report.reference_model_id = Some(reference.model_id);
        reports.push(report);
    }

    let Some(mut best) = best_reference_report(&reports) else {
        return Ok(None);
    };

    if reports.len() <= 1 {
        return Ok(Some(best));
    }

    let agreement_reports = reference_agreement_reports(&reports);
    let committing_count = reports.iter().filter(|report| report.should_commit).count();

    // ── Full consensus: all references commit and agree on the same transcript ──
    if reference_reports_have_commit_consensus(&reports) {
        // SORTED, because this names a SET of models that agreed and a set has no order. It is
        // persisted on the row and shipped in exports as `referenceModelId`, so an unstable
        // rendering would make two identical decisions store two different strings: a corpus diff
        // shows changes nobody made, and any grouping or count by this value splits one category
        // into two. Caught by the suite failing on `flash+pro` where a previous run gave
        // `pro+flash` — same code, different SQLite tie-break, so it had never been the ordering
        // anyone chose.
        //
        // The ROOT-CAUSE fix is the `model_id ASC` tiebreaker in `get_source_transcripts_for_audio`,
        // and that is measured rather than assumed: with neither change the reversed-order test
        // fails; with the tiebreaker alone both pass; the sort alone was not isolated because the
        // tiebreaker already covers it. This line stays anyway — the string names a set, so its
        // canonical form must not depend on why some query happened to order its rows.
        let mut model_ids =
            reports.iter().filter_map(|report| report.reference_model_id.as_deref()).collect::<Vec<_>>();
        model_ids.sort_unstable();
        best.reference_model_id = Some(format!("multi-reference-consensus:{}", model_ids.join("+")));
        best.confidence = reports.iter().map(|report| report.confidence).fold(best.confidence, f64::min);
        best.selected_score = reports.iter().map(|report| report.selected_score).fold(best.selected_score, f64::min);
        best.margin = reports.iter().map(|report| report.margin).fold(best.margin, f64::min);
        best.reference_agreement = agreement_reports;
        best.rationale = format!(
            "Reference-aware agent selected '{}' only after {} whole-file references agreed on the transcript. {}",
            best.selected_model_id,
            reports.len(),
            best.rationale
        );
        return Ok(Some(best));
    }

    // ── Agreement boost: references select the same candidate but not all individually
    //    pass the commit threshold.  Cross-reference agreement is strong evidence. ──
    let best_key = reference_selection_text_key(&best);
    let agreeing_on_best = reports
        .iter()
        .filter(|report| !best_key.is_empty() && reference_selection_text_key(report) == best_key)
        .count();
    // Require a positional window even for the agreement boost (round-24 hunt #15): a whole-file
    // fallback overlap makes the 0.55 bar much easier for common-word candidates and would auto-commit
    // on evidence that never positionally located the candidate in the source.
    if agreeing_on_best >= 2 && best.selected_score >= 0.55 && !best.scores.is_empty() && best.positional_window {
        let model_ids = reports.iter().filter_map(|report| report.reference_model_id.as_deref()).collect::<Vec<_>>();
        let confidence_boost = 0.08 * (agreeing_on_best as f64 - 1.0).min(2.0);
        best.reference_model_id = Some(format!("multi-reference-agreement-boost:{}", model_ids.join("+")));
        best.should_commit = true;
        best.confidence = (best.confidence + confidence_boost).clamp(0.0, 1.0);
        best.reference_agreement = agreement_reports;
        best.rationale = format!(
            "Reference-aware agent boosted commit confidence because {agreeing_on_best}/{} whole-file references independently selected the same candidate '{}' (score {:.2}, boost +{:.2}). {}",
            reports.len(),
            best.selected_model_id,
            best.selected_score,
            confidence_boost,
            best.rationale
        );
        return Ok(Some(best));
    }

    best.should_commit = false;
    best.reference_agreement = agreement_reports;
    best.rationale = format!(
        "Reference-aware agent guarded this segment because {} whole-file references produced {} committing reports without unanimous transcript agreement.",
        reports.len(),
        committing_count
    );
    Ok(Some(best))
}

fn reference_selection_evidence(report: &crate::agentic::CandidateSelectionReport) -> crate::jury::t1_judge::Evidence {
    crate::jury::t1_judge::Evidence {
        tool: "source_reference_adjudicator".into(),
        result: format!(
            "reference={} winner={} score={:.2} overlap={:.2} margin={:.2} commit={}",
            report.reference_model_id.as_deref().unwrap_or("unknown"),
            report.selected_model_id,
            report.selected_score,
            report.scores.first().map(|score| score.reference_window_overlap).unwrap_or(0.0),
            report.margin,
            report.should_commit
        ),
        supports_hypothesis: report.should_commit,
    }
}

fn load_hypotheses_for_segment(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    seg_id: &str,
    seg: &crate::db::SpeechSegment,
) -> Result<Vec<crate::db::SegmentHypothesis>, String> {
    let persisted = db.get_hypotheses_for_segment(seg_id).map_err(|e| e.to_string())?;
    let recorded_is_champion = segment_recorded_model_is_champion(db, seg);
    let mut hyps = hypotheses_for_selected_asr(&settings.asr_model_size, seg, persisted, recorded_is_champion);
    if hyps.is_empty() && settings.asr_model_size != crate::settings::AsrModelSize::WSL7B {
        hyps.push(crate::db::SegmentHypothesis {
            segment_id: seg_id.to_string(),
            model_id: "asr".into(),
            transcript: seg.raw_transcript.clone(),
            confidence: seg.confidence,
        });
    }
    Ok(hyps)
}

fn has_human_decision(seg: &crate::db::SpeechSegment) -> bool {
    seg.human_decision.as_deref().map(|decision| !decision.trim().is_empty()).unwrap_or(false)
}

fn has_final_machine_verdict(seg: &crate::db::SpeechSegment) -> bool {
    seg.verdict.as_deref().map(|verdict| !verdict.trim().is_empty()).unwrap_or(false) && !seg.escalated
}

/// Owned, `Send + 'static` jury database access, so an async command can move it into
/// `run_blocking` (a `&AppState` borrow can't cross the await). Carries the db PATH (the dedicated
/// connection is opened lazily inside `with`, on whichever thread runs it) plus the shared handle for
/// the in-memory/open-failure fallback.
struct JuryDbSource {
    db_path: String,
    shared: crate::AppDatabaseHandle,
}

fn jury_db_source(app_state: &AppState) -> JuryDbSource {
    JuryDbSource { db_path: app_state.lock_db().path().to_string(), shared: app_state.db_arc() }
}

impl JuryDbSource {
    fn with<R>(&self, f: impl FnOnce(&crate::db::Database) -> R) -> R {
        if self.db_path != ":memory:" {
            // Dedicated worker connection so we never hold the GLOBAL db Mutex across the jury's cloud T2
            // Gemini round-trips (which would freeze every other DB command for minutes). Use plain `open`,
            // NOT open_with_retry: the latter runs a full PRAGMA integrity_check (a per-review-session tax
            // that grows with the library) AND reaches the boot-time-only DESTRUCTIVE quarantine decision
            // from a live worker thread. The file was already integrity-checked at boot; `open` sets WAL +
            // busy_timeout=10000, which is the actual transient-contention retry we want here.
            match crate::db::Database::open(&self.db_path) {
                Ok(db) => return f(&db),
                // One open attempt (SQLite's busy_timeout=10s inside open() is the only retry) —
                // the write-path audit corrected an older comment that claimed app-level retries.
                Err(e) => tracing::warn!(
                    "Jury dedicated db connection open failed ({e}); using the shared handle \
                     (other DB commands may pause during adjudication)"
                ),
            }
        }
        let db = self.shared.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("Recovering poisoned database lock");
            poisoned.into_inner()
        });
        f(&db)
    }
}

/// OpenRouter's OpenAI-compatible chat endpoint. POLICY (owner, 2026-07-14): the only approved cloud
/// ASR judge for Central Kurdish is **google/gemini-2.5-pro** — Qwen-family ASR has no Sorani support
/// (measured; PROGRESS_LEDGER 2026 sweep) and must not be configured as the judge.
const OPENROUTER_CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Map a jury model name to an OpenRouter slug. A bare Gemini id (the default `gemini-2.5-pro`) maps to
/// `google/gemini-2.5-pro`; an already-slugged id passes through (mechanism only — the approved ckb
/// judge is strictly Gemini 2.5 Pro; any different model first needs a measured ckb CER on frozen gold).
fn openrouter_jury_model_id(jury_model: &str) -> String {
    let m = jury_model.trim();
    if m.is_empty() {
        "google/gemini-2.5-pro".to_string()
    } else if m.contains('/') || !m.starts_with("gemini") {
        m.to_string()
    } else {
        format!("google/{m}")
    }
}

/// Pure branch logic for the T2 audio-judge transport (no filesystem): OpenRouter when the provider is
/// opted into AND an OpenRouter key is present, otherwise direct Gemini. Never routes to keyless cloud.
fn resolve_t2_endpoint_from_keys(
    settings: &crate::settings::AppSettings,
    gemini_key: &str,
    openrouter_key: Option<&str>,
) -> (crate::jury::t2_listener::T2Endpoint, String, String) {
    use crate::jury::t2_listener::T2Endpoint;
    if settings.jury_provider.eq_ignore_ascii_case("openrouter") {
        if let Some(key) = openrouter_key.map(str::trim).filter(|k| !k.is_empty()) {
            return (
                T2Endpoint::OpenAiCompatible { url: OPENROUTER_CHAT_URL.to_string() },
                key.to_string(),
                openrouter_jury_model_id(&settings.jury_model),
            );
        }
    }
    (T2Endpoint::GeminiDirect, gemini_key.to_string(), settings.jury_model.clone())
}

/// Resolve the T2 transport/key/model, loading the OpenRouter key from `secrets.env` in `data_dir` when
/// the jury provider is "openrouter". Falls back to direct Gemini when no OpenRouter key is present.
fn resolve_t2_endpoint(
    settings: &crate::settings::AppSettings,
    gemini_key: &str,
    data_dir: Option<&std::path::Path>,
) -> Result<(crate::jury::t2_listener::T2Endpoint, String, String), String> {
    let openrouter_key = if settings.jury_provider.eq_ignore_ascii_case("openrouter") {
        match data_dir {
            Some(directory) => crate::api_keys::ApiKeys::load(directory)?.openrouter,
            None => None,
        }
    } else {
        None
    };
    Ok(resolve_t2_endpoint_from_keys(settings, gemini_key, openrouter_key.as_deref()))
}

/// Direct-Gemini entry point (the default transport, used by tests and any caller without a data dir).
/// Callers with a data dir use `run_jury_pipeline_core_via` to honor the OpenRouter jury setting.
pub fn run_jury_pipeline_core(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    segment_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    run_jury_pipeline_core_via(db, settings, segment_ids, None)
}

pub fn run_jury_pipeline_core_via(
    db: &crate::db::Database,
    settings: &crate::settings::AppSettings,
    segment_ids: Vec<String>,
    data_dir: Option<&std::path::Path>,
) -> Result<serde_json::Value, String> {
    // Schema v60 retired every machine-verdict writer: paid review truth now crosses only the
    // evidence-backed human-decision boundary. Continuing into the historical jury at v60+ would do
    // expensive work and then fail the import on its first forbidden write. Champion-only production
    // also has exactly one ASR, so a multi-ASR jury is meaningless even against an archival schema.
    // Keep the old jury executable only for explicit pre-v60 diagnostics and leave current drafts for
    // human review. This guard is deliberately at the shared core so directory, file, audiobook, and
    // direct-command callers cannot drift apart.
    let schema_version = crate::migrations::get_current_version(db).map_err(|error| error.to_string())?;
    if schema_version >= 60 || settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
        let reason = if schema_version >= 60 {
            "Schema v60+ sends machine drafts directly to the evidence-backed human review flow; machine jury writes are retired"
        } else {
            "Champion-only mode sends OmniASR 7B drafts directly to human review; auxiliary-ASR jury is not run"
        };
        return Ok(serde_json::json!({
            "mode": "not_required",
            "totalInput": segment_ids.len(),
            "t0AutoAccepted": 0,
            "t0Escalated": 0,
            "referenceCommitted": 0,
            "referenceGuarded": 0,
            "hypothesisGuarded": 0,
            "t1Committed": 0,
            "t2Committed": 0,
            "humanInbox": segment_ids.len(),
            "reason": reason
        }));
    }

    let t1_threshold = settings.jury_t1_threshold;
    let cloud_opt_in = settings.jury_cloud_opt_in;
    // Floor at 3: self-consistency is meaningless below 3 samples, and a misconfigured 1 would let a
    // single Gemini sample masquerade as a "majority". majority_vote also requires >= 2 agreeing
    // samples, so this is defense in depth at the config boundary.
    let n_samples = (settings.jury_self_consistency_n as usize).max(3);
    // T2 transport: direct Gemini by default, or OpenRouter (with the OR key from secrets.env) when the
    // jury provider is set to "openrouter".
    let (t2_endpoint, api_key, jury_model) = resolve_t2_endpoint(settings, &settings.llm_api_key, data_dir)?;

    // The Autonomy Dial governs EVERY machine-commit stage of this pipeline, not just T0
    // (round-24 hunt #1, HIGH). Observe/Propose previously gated only run_t0_gate; the SAME run then
    // re-consumed the staged ('escalated') segments as T1/T2 input and machine-committed them as
    // 'jury_accept' — silently removing from the human queue the very segments the dial had just
    // promised to stage (and, under Observe, the T2-disabled fallback still REWROTE verdicts the dial
    // said it would never touch). Enforced once here, at the single pipeline chokepoint:
    //   - Observe: the pipeline commits NOTHING beyond T0's own no-write contract — no reference
    //     commits, no T1/T2, no re-staging writes.
    //   - Propose: the machine stages ('escalated') but never commits; an already-staged segment
    //     keeps its T0 verdict + IRT confidence (riskiest-first ordering) untouched.
    let machine_commits_allowed = matches!(
        settings.jury_autonomy_level,
        crate::settings::AutonLevel::ActConfirm | crate::settings::AutonLevel::ActAuto
    );

    let initial_seg_map: std::collections::HashMap<String, crate::db::SpeechSegment> = db
        .get_segments_by_ids(&segment_ids)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();

    let mut t1_committed = 0usize;
    let mut t2_committed = 0usize;
    let mut still_escalated = 0usize;
    let mut reference_committed = 0usize;
    let mut reference_guarded = 0usize;
    let mut hypothesis_guarded = 0usize;
    let mut source_duration_cache: std::collections::HashMap<String, Option<i64>> = std::collections::HashMap::new();
    let mut source_identity_cache: std::collections::HashMap<String, Option<crate::pipeline::SourceAudioIdentity>> =
        std::collections::HashMap::new();
    let mut reference_reports: std::collections::HashMap<String, crate::agentic::CandidateSelectionReport> =
        std::collections::HashMap::new();
    let mut t0_candidate_ids = Vec::new();
    let mut review_ids = Vec::new();

    // Source-reference adjudication runs before T0 so whole-file context cannot
    // be bypassed by a local multi-ASR consensus auto-accept.
    for seg_id in &segment_ids {
        let Some(seg) = initial_seg_map.get(seg_id) else {
            continue;
        };
        if has_human_decision(seg) || has_final_machine_verdict(seg) {
            continue;
        }

        let hyps = load_hypotheses_for_segment(db, settings, seg_id, seg)?;
        if let Some(report) = hypothesis_coverage_guard(seg, &hyps) {
            reference_reports.insert(seg_id.clone(), report);
            review_ids.push(seg_id.clone());
            hypothesis_guarded += 1;
            continue;
        }
        match reference_selection_for_segment(
            db,
            settings,
            seg,
            &hyps,
            &mut source_duration_cache,
            &mut source_identity_cache,
        )? {
            Some(report) if report.should_commit && machine_commits_allowed => {
                let ev_json = serde_json::to_string(&report)
                    .map_err(|e| format!("Failed to serialize reference selection evidence for {seg_id}: {e}"))?;
                db.write_segment_verdict(
                    seg_id,
                    "jury_accept",
                    Some(&report.selected_transcript),
                    Some(&report.rationale),
                    Some(ev_json.as_str()),
                    Some(report.confidence),
                    false,
                )
                .map_err(|e| e.to_string())?;
                reference_committed += 1;
            }
            Some(report) => {
                reference_reports.insert(seg_id.clone(), report);
                review_ids.push(seg_id.clone());
                reference_guarded += 1;
            }
            None if seg.escalated => review_ids.push(seg_id.clone()),
            None => t0_candidate_ids.push(seg_id.clone()),
        }
    }

    let t0_report = if t0_candidate_ids.is_empty() {
        crate::jury::T0GateReport { total: 0, auto_accepted: 0, escalated: 0, decisions: Vec::new() }
    } else {
        crate::jury::run_t0_gate(
            db,
            &t0_candidate_ids,
            &settings.jury_autonomy_level,
            settings.irt_ability_learning_enabled,
        )
        .map_err(|e| e.to_string())?
    };

    if !t0_candidate_ids.is_empty() {
        let post_t0_map: std::collections::HashMap<String, crate::db::SpeechSegment> = db
            .get_segments_by_ids(&t0_candidate_ids)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        for seg_id in &t0_candidate_ids {
            if let Some(seg) = post_t0_map.get(seg_id) {
                if seg.escalated && !has_human_decision(seg) {
                    review_ids.push(seg_id.clone());
                }
            }
        }
    }

    let review_seg_map: std::collections::HashMap<String, crate::db::SpeechSegment> = db
        .get_segments_by_ids(&review_ids)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();

    for seg_id in &review_ids {
        let seg = match review_seg_map.get(seg_id) {
            Some(s) => s.clone(),
            None => continue,
        };

        // Observe/Propose: the machine may not commit (or, under Observe, write anything at all) —
        // stage-or-skip instead of running T1/T2. See machine_commits_allowed above.
        if !machine_commits_allowed {
            if matches!(settings.jury_autonomy_level, crate::settings::AutonLevel::Observe) {
                continue; // Observe: pure observation — this run writes nothing.
            }
            // Propose: an already-staged segment keeps its verdict + IRT confidence (rewriting it
            // would NULL the confidence that drives the queue's riskiest-first ordering).
            if seg.escalated {
                still_escalated += 1;
                continue;
            }
            // Carry the guard's rationale into the staged verdict when one exists — under the
            // shipped default (Propose) this is the reviewer's context in the inbox.
            let staged_rationale = match reference_reports.remove(seg_id) {
                Some(report) => format!("{} Staged for human review (autonomy dial: Propose).", report.rationale),
                None => "Staged for human review: autonomy dial 'Propose' disables machine commits".to_string(),
            };
            // P1.2: `policy_hold` — this clip was NOT escalated on its merits. The autonomy dial forbade
            // a machine commit, so a decidable clip was staged for a human. That needs a settings
            // change, not a reviewer listening harder, and prose alone cannot be counted or filtered.
            db.write_segment_verdict(
                seg_id,
                "escalated",
                None,
                Some(&staged_rationale),
                Some(&crate::jury::escalation_evidence(&[crate::jury::reason::POLICY_HOLD])),
                None,
                true,
            )
            .map_err(|e| e.to_string())?;
            still_escalated += 1;
            continue;
        }

        // Load hypotheses from database
        let hyps = load_hypotheses_for_segment(db, settings, seg_id, &seg)?;

        // ── Stage 2: T1 text judge ────────────────────────────────────────────
        let reference_report = match reference_reports.remove(seg_id) {
            Some(report) => Some(report),
            None => reference_selection_for_segment(
                db,
                settings,
                &seg,
                &hyps,
                &mut source_duration_cache,
                &mut source_identity_cache,
            )?,
        };
        if let Some(report) = &reference_report {
            if report.should_commit {
                let ev_json = serde_json::to_string(report)
                    .map_err(|e| format!("Failed to serialize reference selection evidence for {seg_id}: {e}"))?;
                db.write_segment_verdict(
                    seg_id,
                    "jury_accept",
                    Some(&report.selected_transcript),
                    Some(&report.rationale),
                    Some(ev_json.as_str()),
                    Some(report.confidence),
                    false,
                )
                .map_err(|e| e.to_string())?;
                reference_committed += 1;
                continue;
            }
        }

        let effective_t1_threshold = if reference_report.is_some() { 1.01 } else { t1_threshold };
        match crate::jury::t1_judge::judge_t1(seg_id, &hyps, effective_t1_threshold) {
            crate::jury::t1_judge::T1Decision::Commit { transcript, reason, evidence, confidence, .. } => {
                let evidence_payload = match &reference_report {
                    Some(report) => serde_json::json!({
                        "t1Evidence": evidence,
                        "referenceSelection": report,
                    }),
                    None => serde_json::json!(evidence),
                };
                let ev_json = serde_json::to_string(&evidence_payload)
                    .map_err(|e| format!("Failed to serialize T1 evidence for {seg_id}: {e}"))?;
                db.write_segment_verdict(
                    seg_id,
                    "jury_accept",
                    Some(&transcript),
                    Some(&reason),
                    Some(ev_json.as_str()),
                    Some(confidence),
                    false,
                )
                .map_err(|e| e.to_string())?;
                t1_committed += 1;
            }

            crate::jury::t1_judge::T1Decision::EscalateToT2 { hypotheses, mut t1_evidence, .. } => {
                if let Some(report) = &reference_report {
                    t1_evidence.push(reference_selection_evidence(report));
                }
                // ── Stage 3: T2 Gemini audio listener ─────────────────────────
                if cloud_opt_in && !api_key.trim().is_empty() {
                    // Encode only this segment's source-time span, not the whole long source file.
                    let audio_b64 = match crate::agentic::segment_audio_as_wav_base64(&seg) {
                        Ok(encoded) => encoded,
                        Err(e) => {
                            tracing::warn!("T2: cannot prepare segment audio for {seg_id}: {e}");
                            // P1.2: `missing_audio` — no judgement was possible because the audio could
                            // not be read. The prose keeps the specific IO error; the code makes the
                            // class countable, and this class is fixed by finding a file.
                            db.write_segment_verdict(
                                seg_id,
                                "escalated",
                                None,
                                Some(&e.to_string()),
                                Some(&crate::jury::escalation_evidence(&[crate::jury::reason::MISSING_AUDIO])),
                                None,
                                true,
                            )
                            .map_err(|e| e.to_string())?;
                            still_escalated += 1;
                            continue;
                        }
                    };

                    let few_shots = crate::jury::get_few_shot_examples(db, seg_id, 5).map_err(|e| e.to_string())?;
                    let t2 = crate::jury::t2_listener::listen_and_judge_via(
                        &t2_endpoint,
                        &audio_b64,
                        &hypotheses,
                        &t1_evidence,
                        &few_shots,
                        &api_key,
                        &jury_model,
                        n_samples,
                    );

                    if let Some(verdict) = t2.verdict {
                        let evidence_payload = {
                            let mut payload = serde_json::json!({
                                "t2Transcript": verdict.transcript.clone(),
                                "t2Confidence": verdict.confidence,
                                "t2SelfConsistencyAgreement": verdict.self_consistency_agreement,
                                "t2Votes": verdict.votes,
                                "t2Evidence": verdict.evidence.clone(),
                            });
                            if let Some(report) = &reference_report {
                                payload["referenceSelection"] = serde_json::json!(report);
                            }
                            payload
                        };
                        let ev_json = serde_json::to_string(&evidence_payload)
                            .map_err(|e| format!("Failed to serialize T2 evidence for {seg_id}: {e}"))?;
                        db.write_segment_verdict(
                            seg_id,
                            "jury_accept",
                            Some(&verdict.transcript),
                            Some(&verdict.reason),
                            Some(ev_json.as_str()),
                            Some(verdict.confidence),
                            false,
                        )
                        .map_err(|e| e.to_string())?;
                        t2_committed += 1;
                    } else {
                        // T2 failed or no majority — human inbox
                        let reason = t2.error.unwrap_or_else(|| "T2 no majority".into());
                        // P1.2: T2 answered, and its answer was "no consensus" — deliberately a
                        // different code from T1_UNRESOLVED below, where T2 never got to answer at all.
                        db.write_segment_verdict(
                            seg_id,
                            "escalated",
                            None,
                            Some(&reason),
                            Some(&crate::jury::escalation_evidence(&[crate::jury::reason::T2_NO_MAJORITY])),
                            None,
                            true,
                        )
                        .map_err(|e| e.to_string())?;
                        still_escalated += 1;
                    }
                } else {
                    // Cloud disabled — escalate to human inbox
                    let reason = reference_report.as_ref().map_or_else(
                        || "T1 could not resolve; T2 disabled (cloud opt-in off)".to_string(),
                        |report| format!("{} T1 could not resolve; T2 disabled (cloud opt-in off)", report.rationale),
                    );
                    // P1.2: T1 could not resolve AND T2 was never consulted, because cloud opt-in is
                    // off. Recorded as T1_UNRESOLVED rather than T2_NO_MAJORITY: the panel declining to
                    // answer is a different fact from the panel answering "no consensus", and only one
                    // of them is fixed by turning a setting on.
                    db.write_segment_verdict(
                        seg_id,
                        "escalated",
                        None,
                        Some(&reason),
                        Some(&crate::jury::escalation_evidence(&[crate::jury::reason::T1_UNRESOLVED])),
                        None,
                        true,
                    )
                    .map_err(|e| e.to_string())?;
                    still_escalated += 1;
                }
            }
        }
    }

    Ok(serde_json::json!({
        "totalInput": segment_ids.len(),
        "t0AutoAccepted": t0_report.auto_accepted,
        "t0Escalated": t0_report.escalated,
        "referenceCommitted": reference_committed,
        "referenceGuarded": reference_guarded,
        "hypothesisGuarded": hypothesis_guarded,
        "t1Committed": t1_committed,
        "t2Committed": t2_committed,
        "humanInbox": still_escalated,
    }))
}

/// Open a SEPARATE connection (WAL mode) to the app's SQLite DB for jury/adjudication batches. The
/// jury may make N blocking T2 cloud calls; running it on its own connection — rather than the shared
/// `lock_db()` guard — keeps the global lock free so the UI's `get_segments` is never starved for the
/// duration of the run. Returns None when the app data dir isn't available yet.
pub(crate) fn open_jury_db_connection(app_state: &AppState) -> Option<crate::db::Database> {
    app_state
        .data_dir
        .lock()
        .ok()
        .and_then(|g| (*g).clone())
        .map(|dir| dir.join("cortex-speech.db"))
        .and_then(|p| crate::db::Database::open(p.to_string_lossy().as_ref()).ok())
}

/// Minimal base64 encoder — avoids pulling in a full base64 crate.
/// Uses the standard alphabet (RFC 4648).
pub fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 { chunk[1] as usize } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as usize } else { 0 };
        out.push(CHARS[b0 >> 2] as char);
        out.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        out.push(if chunk.len() > 1 { CHARS[((b1 & 0xF) << 2) | (b2 >> 6)] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[b2 & 0x3F] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_segment(id: &str, audio_path: &str, raw_transcript: &str) -> crate::db::SpeechSegment {
        crate::db::SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: audio_path.to_string(),
            raw_transcript: raw_transcript.to_string(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 1000,
            speaker_id: None,
            verified: false,
            confidence: Some(0.99),
            ctc_score: Some(0.0),
            clipping_ratio: Some(0.0),
            rms_db: Some(-20.0),
            snr_db: Some(25.0),
            split: None,
            signal_anomaly_score: None,
            ..crate::db::SpeechSegment::default()
        }
    }

    fn legacy_machine_db() -> crate::db::Database {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        rollback_to_legacy_machine_schema(&db);
        db
    }

    const LEGACY_MACHINE_SCHEMA_VERSION: i64 = 59;

    fn rollback_to_legacy_machine_schema(db: &crate::db::Database) {
        let head = crate::migrations::max_supported_version();
        assert!(head >= LEGACY_MACHINE_SCHEMA_VERSION);
        let expected = ((LEGACY_MACHINE_SCHEMA_VERSION + 1)..=head).rev().collect::<Vec<_>>();
        assert_eq!(crate::migrations::rollback(db, expected.len()).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(db).unwrap(), LEGACY_MACHINE_SCHEMA_VERSION);
    }

    fn migrate_legacy_machine_schema_to_head(db: &crate::db::Database) {
        let head = crate::migrations::max_supported_version();
        let expected = ((LEGACY_MACHINE_SCHEMA_VERSION + 1)..=head).collect::<Vec<_>>();
        assert_eq!(crate::migrations::run_migrations(db).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(db).unwrap(), head);
    }

    fn copied_database(source: &crate::db::Database) -> crate::db::Database {
        let mut copy = crate::db::Database::open(":memory:").unwrap();
        copy.initialize().unwrap();
        copy.commit_staged_restore(source).unwrap();
        copy
    }

    fn insert_canonical_pay_segment(db: &crate::db::Database, id: &str) {
        db.insert_segment(&test_segment(id, &format!("{id}.wav"), "machine draft")).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?2,
                        audio_fingerprint = ?3,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}',
                        duration_ms = 1000
                  WHERE id = ?1",
                rusqlite::params![id, "a".repeat(64), 424_242_i64],
            )
            .unwrap();
    }

    fn canonical_operation(index: u64) -> String {
        format!("00000000-0000-4000-8000-{index:012x}")
    }

    fn canonical_phone_playback(
        db: &crate::db::Database,
        segment_id: &str,
        reviewer: &str,
    ) -> crate::db::PlaybackDecisionProof {
        let revision = db.segment_review_revision(segment_id).unwrap().unwrap();
        let audio_content_hash = db.segment_audio_content_hash(segment_id).unwrap().unwrap();
        let (source_start_ms, source_end_ms) = db.segment_source_span(segment_id).unwrap().unwrap();
        db.record_playback_receipt(&crate::db::PlaybackReceipt {
            segment_id: segment_id.to_string(),
            segment_revision: revision,
            audio_content_hash: audio_content_hash.clone(),
            reviewer: Some(reviewer.to_string()),
            session_id: None,
            started_at_ms: 1,
            played_ms: 1_000,
            clip_duration_ms: 1_000,
            source_start_ms: Some(source_start_ms),
            source_end_ms: Some(source_end_ms),
        })
        .unwrap();
        crate::db::PlaybackDecisionProof {
            segment_revision: revision,
            audio_content_hash,
            source_start_ms,
            source_end_ms,
            authority_session_id: None,
            source_lease: None,
        }
    }

    /// A real policy-4 Couch authority for restore characterizations. The temporary WAV remains
    /// alive for the complete decision transaction through the proof's verified source lease; no
    /// synthetic receipt row or cfg(test) writer bypass is involved.
    struct CanonicalPolicy4Playback {
        proof: crate::db::PlaybackDecisionProof,
        _source: tempfile::TempDir,
    }

    impl std::ops::Deref for CanonicalPolicy4Playback {
        type Target = crate::db::PlaybackDecisionProof;

        fn deref(&self) -> &Self::Target {
            &self.proof
        }
    }

    fn canonical_policy4_phone_playback(
        db: &crate::db::Database,
        segment_id: &str,
        reviewer: &str,
    ) -> CanonicalPolicy4Playback {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("canonical-policy4.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&source_path, spec).unwrap();
        for sample in 0..16_000_i32 {
            writer.write_sample::<i16>(((sample % 257) - 128) as i16).unwrap();
        }
        writer.finalize().unwrap();
        let content_hash = crate::export_bundle::current_canonical_pcm_blake3(&source_path).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_path = ?2,
                        audio_content_hash = ?3,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}',
                        duration_ms = 1000
                  WHERE id = ?1",
                rusqlite::params![segment_id, source_path.to_string_lossy(), content_hash],
            )
            .unwrap();
        let revision = db.segment_review_revision(segment_id).unwrap().unwrap();
        let session_binding_sha256 = "c".repeat(64);
        let issued_at_ms =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let authority = crate::db::CouchPlaybackAttemptAuthority {
            playback_receipt_id: uuid::Uuid::new_v4().to_string(),
            media_grant_id: uuid::Uuid::new_v4().to_string(),
            client_attempt_id: uuid::Uuid::new_v4().to_string(),
            session_binding_sha256: session_binding_sha256.clone(),
            reviewer: reviewer.to_string(),
            segment_id: segment_id.to_string(),
            segment_revision: revision,
            audio_content_hash: content_hash.clone(),
            source_path,
            clip_duration_ms: 1_000,
            source_start_ms: 0,
            source_end_ms: 1_000,
            issued_at_ms,
            expires_at_ms: issued_at_ms + 60_000,
        };
        let receipt = db
            .finalize_couch_playback_attempt_v1(
                &authority,
                &[crate::db::DesktopPlaybackInterval { start_ms: 0, end_ms: 1_000 }],
                1_000,
            )
            .unwrap();
        let proof = db
            .couch_playback_proof_v4(
                segment_id,
                revision,
                &content_hash,
                reviewer,
                &session_binding_sha256,
                &receipt.playback_receipt_id,
            )
            .unwrap()
            .expect("canonical policy-4 receipt must resolve to its exact source lease");
        CanonicalPolicy4Playback { proof, _source: source }
    }

    fn record_canonical_phone_edit(db: &crate::db::Database, segment_id: &str, operation_index: u64) -> (i64, String) {
        insert_canonical_pay_segment(db, segment_id);
        let proof = canonical_policy4_phone_playback(db, segment_id, "Reviewer");
        let operation = canonical_operation(operation_index);
        db.record_phone_human_decision_by_at_revision_with_operation_limit(
            segment_id,
            "edit",
            Some("machine truth"),
            "Reviewer",
            proof.segment_revision,
            &proof,
            &operation,
            &crate::db::review_operation_payload_hash(segment_id, "edit", "machine truth", "Reviewer"),
            "edit",
            "machine truth",
            None,
        )
        .unwrap()
        .unwrap();
        let effect_id = db.human_decision_effect_for_operation(&operation).unwrap().unwrap().0;
        (effect_id, operation)
    }

    fn record_canonical_skip(db: &crate::db::Database, segment_id: &str, reviewer: &str, index: u64) {
        db.record_review_event_with_operation(
            segment_id,
            reviewer,
            "skip",
            "couch",
            i64::try_from(index).unwrap(),
            &canonical_operation(index),
            &crate::db::review_operation_payload_hash(segment_id, "skip", "", reviewer),
        )
        .unwrap();
    }

    fn insert_test_compensation_ledger(db: &crate::db::Database) {
        db.connection()
            .execute(
                "INSERT INTO review_compensation_ledger
                    (id, entry_id, entry_key, policy_version, review_event_id,
                     canonical_work_id, canonical_identity_kind, reviewer, segment_id, source,
                     compensation_action, effective_decision, decision_revision, duration_ms,
                     rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                     corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id, created_at)
                 VALUES
                    (1, 'entry-1', 'key-1', ?1, NULL,
                     'work-1', 'test', 'Reviewer', 'ledger-segment', 'test',
                     'skip', 'skip', NULL, 1000,
                     0, 0, 0, 0, 0, NULL, '2026-08-22 00:00:00')",
                [crate::db::REVIEW_PAY_POLICY_VERSION],
            )
            .unwrap();
    }

    fn test_pilot_policy(
        after_review_event_id: i64,
        first: &str,
        second: &str,
    ) -> crate::review_pilot::ReviewPilotPolicy {
        crate::review_pilot::parse(
            &serde_json::json!({
                "schema_version": 1,
                "after_review_event_id": after_review_event_id,
                "max_total_corpus_actions": 20,
                "reviewers": [
                    { "name": first, "max_corpus_actions": 10 },
                    { "name": second, "max_corpus_actions": 10 }
                ]
            })
            .to_string(),
        )
        .unwrap()
    }

    fn pilot_restore_action(policy: &crate::review_pilot::ReviewPilotPolicy) -> SnapshotPilotPolicyRestore {
        SnapshotPilotPolicyRestore::Install(serde_json::to_vec(policy).unwrap())
    }

    fn insert_test_review_event(
        db: &crate::db::Database,
        segment_id: &str,
        reviewer: &str,
        action: &str,
        source: &str,
        timestamp_ms: i64,
    ) {
        let paid_provenance = matches!(source, "couch" | "couch_spot_check");
        let requested_action = match action {
            "accept" | "edit" | "reject" | "skip" => action,
            _ => "accept",
        };
        let requested_transcript = if requested_action == "skip" { "" } else { "expected" };
        let operation_id = uuid::Uuid::new_v4().to_string();
        let operation_payload_hash =
            crate::db::review_operation_payload_hash(segment_id, requested_action, requested_transcript, reviewer);
        db.connection()
            .execute(
                "INSERT INTO review_events
                    (segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                     compensation_action, created_at, app_git_sha, playback_guard_version,
                     operation_id, operation_payload_hash, requested_action,
                     requested_transcript, served_transcript, served_revision)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1000, ?3, '2026-08-22 00:00:00', ?6, ?7,
                         ?8, ?9, ?10, ?11, 'expected', 0)",
                rusqlite::params![
                    segment_id,
                    reviewer,
                    action,
                    source,
                    timestamp_ms,
                    paid_provenance.then_some(crate::GIT_SHA),
                    paid_provenance.then_some("content-hash-raw-counter-v3"),
                    paid_provenance.then_some(operation_id),
                    paid_provenance.then_some(operation_payload_hash),
                    paid_provenance.then_some(requested_action),
                    paid_provenance.then_some(requested_transcript),
                ],
            )
            .unwrap();
    }

    fn insert_test_spot_result(db: &crate::db::Database, segment_id: &str, reviewer: &str, action: &str) {
        db.connection()
            .execute(
                "INSERT INTO spot_checks
                    (segment_id, reviewer, action, submitted_transcript, expected_transcript,
                     noticed, cer, created_at)
                 VALUES (?1, ?2, ?3, 'expected', 'expected', 1, 0.0,
                         '2026-08-22 00:00:00')",
                rusqlite::params![segment_id, reviewer, action],
            )
            .unwrap();
    }

    fn assert_floor_only_durable_row_rejected<Prepare, AddFloor>(
        expected_label: &str,
        prepare_shared: Prepare,
        add_floor_only: AddFloor,
    ) where
        Prepare: FnOnce(&crate::db::Database),
        AddFloor: FnOnce(&crate::db::Database),
    {
        let base = crate::db::Database::open(":memory:").unwrap();
        base.initialize().unwrap();
        prepare_shared(&base);
        let target = copied_database(&base);
        let floor = copied_database(&base);
        add_floor_only(&floor);
        let error = require_durable_review_history_superset(&floor, &target).unwrap_err();
        assert!(error.contains(expected_label), "expected {expected_label} rejection, got: {error}");
    }

    #[test]
    fn mandatory_pre_restore_snapshot_failure_aborts_before_live_data_can_change() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().expect("local restore reservation");
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let segment = test_segment("restore-safety", "restore-safety.wav", "still live");
        db.insert_segment(&segment).unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let invalid_data_dir = temp.path().join("not-a-directory");
        std::fs::write(&invalid_data_dir, b"blocks snapshot directory creation").unwrap();

        let err = take_mandatory_pre_restore_snapshot(&reservation, &db, &invalid_data_dir).unwrap_err();
        assert!(err.contains("mandatory pre-restore safety snapshot failed"), "{err}");
        assert!(err.contains("has not been overwritten"), "{err}");
        assert!(
            !err.contains("requires an active exclusive restore reservation"),
            "the capability must let this test reach the injected filesystem failure: {err}"
        );
        assert_eq!(
            db.get_segment_by_id(&segment.id).unwrap().unwrap().raw_transcript,
            "still live",
            "a failed safety pin must leave the live library untouched"
        );
    }

    #[test]
    fn durable_restore_floor_protects_all_pre_v60_authorities_by_exact_value() {
        assert_floor_only_durable_row_rejected(
            "review_pilot_hidden_keys",
            |_| {},
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'Reviewer', 'hidden-1')",
                        ["a".repeat(64)],
                    )
                    .unwrap();
            },
        );
        assert_floor_only_durable_row_rejected(
            "review_events",
            |_| {},
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO review_events
                            (id, segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                             compensation_action, created_at)
                         VALUES (1, 'event-1', 'Reviewer', 'skip', 'test', 1, 1000, 'skip',
                                 '2026-08-22 00:00:00')",
                        [],
                    )
                    .unwrap();
            },
        );
        assert_floor_only_durable_row_rejected(
            "spot_checks",
            |_| {},
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO spot_checks
                            (segment_id, reviewer, action, submitted_transcript, expected_transcript,
                             noticed, cer, created_at)
                         VALUES ('spot-1', 'Reviewer', 'edit', 'submitted', 'expected', 1, 0.0,
                                 '2026-08-22 00:00:00')",
                        [],
                    )
                    .unwrap();
            },
        );
        assert_floor_only_durable_row_rejected("review_compensation_ledger", |_| {}, insert_test_compensation_ledger);
        assert_floor_only_durable_row_rejected(
            "review_compensation_settlements",
            insert_test_compensation_ledger,
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO review_compensation_settlements
                            (id, settlement_id, policy_version, reviewer,
                             from_ledger_id_exclusive, through_ledger_id_inclusive,
                             allocated_micro_iqd, payout_reference, created_at)
                         VALUES (1, 'settlement-1', ?1, 'Reviewer', 0, 1, 0, 'payout-1',
                                 '2026-08-22 00:00:00')",
                        [crate::db::REVIEW_PAY_POLICY_VERSION],
                    )
                    .unwrap();
            },
        );
        assert_floor_only_durable_row_rejected(
            "review_compensation_policies",
            |_| {},
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO review_compensation_policies
                            (policy_version, effective_after_event_id, base_rate_micro_iqd_per_hour,
                             edit_basis_points, accept_basis_points, reject_basis_points,
                             skip_basis_points, created_at)
                         VALUES ('test-policy-v2', 0, 1, 10000, 1000, 1000, 0,
                                 '2026-08-22 00:00:00')",
                        [],
                    )
                    .unwrap();
            },
        );
        // A legacy correction has to exist before v60 snapshots the immutable frontier. Build that
        // floor first, then remove both the row and its snapshot from a trigger-disabled staged copy;
        // rolling only the floor through v60 after the target was copied would also recreate
        // review_effect_state with a different timestamp and test the wrong authority first.
        let correction_floor = crate::db::Database::open(":memory:").unwrap();
        correction_floor.initialize().unwrap();
        rollback_to_legacy_machine_schema(&correction_floor);
        correction_floor
            .connection()
            .execute(
                "INSERT INTO corrections
                    (id, segment_id, audio_content_hash, raw_hypothesis, human_fix,
                     reviewer_id, decided_at)
                 VALUES ('correction-1', NULL, ?1, 'wrong', 'right', 'Reviewer',
                         '2026-08-22 00:00:00')",
                ["a".repeat(64)],
            )
            .unwrap();
        migrate_legacy_machine_schema_to_head(&correction_floor);
        let correction_target = copied_database(&correction_floor);
        correction_target
            .connection()
            .execute_batch(
                "DROP TRIGGER corrections_v60_effect_immutable_delete;
                 DROP TRIGGER legacy_corrections_v60_immutable_delete;
                 DELETE FROM corrections WHERE id = 'correction-1';
                 DELETE FROM legacy_corrections_v60 WHERE id = 'correction-1';",
            )
            .unwrap();
        let correction_error =
            require_durable_review_history_superset(&correction_floor, &correction_target).unwrap_err();
        assert!(correction_error.contains("corrections"), "expected corrections rejection, got: {correction_error}");
        assert_floor_only_durable_row_rejected(
            "playback_receipts",
            |base| {
                base.insert_segment(&test_segment("receipt-1", "receipt.wav", "draft")).unwrap();
            },
            |floor| {
                floor
                    .connection()
                    .execute(
                        "INSERT INTO playback_receipts
                            (id, segment_id, segment_revision, audio_fingerprint, reviewer, session_id,
                             started_at_ms, played_ms, clip_duration_ms, coverage_ratio,
                             policy_version, created_at)
                         VALUES (1, 'receipt-1', 0, 'fingerprint', 'Reviewer', 'session',
                                 1, 1000, 1000, 1.0, 1, '2026-08-22 00:00:00')",
                        [],
                    )
                    .unwrap();
            },
        );

        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        floor
            .connection()
            .execute(
                "INSERT INTO review_events
                    (id, segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                     compensation_action, created_at)
                 VALUES (1, 'value-1', 'Reviewer', 'skip', 'test', 1, 1000, 'skip',
                         '2026-08-22 00:00:00')",
                [],
            )
            .unwrap();
        let target = copied_database(&floor);
        require_durable_review_history_superset(&floor, &target).unwrap();
        target.connection().execute("DROP TRIGGER review_events_v60_post_cutoff_immutable_update", []).unwrap();
        target.connection().execute("UPDATE review_events SET reviewer = 'Changed' WHERE id = 1", []).unwrap();
        let changed = require_durable_review_history_superset(&floor, &target).unwrap_err();
        assert!(changed.contains("review_events"), "same identity with changed values must fail: {changed}");

        let superset = copied_database(&floor);
        superset
            .connection()
            .execute(
                "INSERT INTO review_events
                    (id, segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                     compensation_action, created_at)
                 VALUES (2, 'value-2', 'Reviewer', 'skip', 'test', 2, 1000, 'skip',
                         '2026-08-22 00:00:01')",
                [],
            )
            .unwrap();
        require_durable_review_history_superset(&floor, &superset).unwrap();
    }

    #[test]
    fn durable_restore_floor_protects_every_v60_effect_authority() {
        fn assert_missing_is_refused(floor: &crate::db::Database, expected_label: &str, mutation_sql: &str) {
            let target = copied_database(floor);
            target.connection().execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
            target.connection().execute_batch(mutation_sql).unwrap();
            let error = require_durable_review_history_superset(floor, &target).unwrap_err();
            assert!(error.contains(expected_label), "expected {expected_label} refusal, got: {error}");
        }

        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        record_canonical_phone_edit(&floor, "durable-effect-edit", 201);

        insert_canonical_pay_segment(&floor, "durable-effect-undo");
        canonical_phone_playback(&floor, "durable-effect-undo", "Reviewer");
        let revision = floor.segment_review_revision("durable-effect-undo").unwrap().unwrap();
        let operation = canonical_operation(202);
        floor
            .record_phone_human_decision_by_at_revision_with_operation(
                "durable-effect-undo",
                "reject",
                None,
                "Reviewer",
                revision,
                &operation,
                &crate::db::review_operation_payload_hash("durable-effect-undo", "reject", "", "Reviewer"),
            )
            .unwrap()
            .unwrap();
        let effect_id = floor.human_decision_effect_for_operation(&operation).unwrap().unwrap().0;
        assert!(matches!(
            floor.undo_human_decision(effect_id, Some("Reviewer"), &operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));

        insert_canonical_pay_segment(&floor, "durable-flag-undo");
        let flag = floor
            .record_review_flag_for_test("durable-flag-undo", "durable flag", "00000000-0000-4000-8000-000000000801")
            .unwrap();
        assert!(matches!(
            floor.undo_review_flag(flag.effect_event_id, &canonical_operation(203)).unwrap(),
            crate::db::HumanFlagUndoOutcome::Applied { .. }
        ));

        for (table, predicate) in [
            ("human_decision_effect_events", "1=1"),
            ("human_decision_effect_reversals", "1=1"),
            ("review_flag_effect_events", "1=1"),
            ("review_flag_effect_reversals", "1=1"),
            ("correction_memory", "legacy_seed=0"),
            ("correction_memory_contributions", "1=1"),
            ("corrections", "effect_event_id IS NOT NULL"),
            ("agent_examples", "effect_event_id IS NOT NULL"),
        ] {
            let count: i64 = floor
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table} WHERE {predicate}"), [], |row| row.get(0))
                .unwrap();
            assert!(count > 0, "writer fixture must populate {table}");
        }
        validate_restore_target_semantics(&floor).unwrap();
        assert!(has_durable_review_activity(&floor).unwrap());

        assert_missing_is_refused(
            &floor,
            "review_effect_state",
            "DROP TRIGGER review_effect_state_immutable_delete;
             DELETE FROM review_effect_state;",
        );
        assert_missing_is_refused(
            &floor,
            "human_decision_effect_events",
            "DROP TRIGGER human_decision_effect_events_immutable_delete;
             DELETE FROM human_decision_effect_events
              WHERE id = (SELECT MIN(id) FROM human_decision_effect_events);",
        );
        assert_missing_is_refused(
            &floor,
            "human_decision_effect_reversals",
            "DROP TRIGGER human_decision_effect_reversals_immutable_delete;
             DELETE FROM human_decision_effect_reversals
              WHERE effect_event_id = (SELECT MIN(effect_event_id) FROM human_decision_effect_reversals);",
        );
        assert_missing_is_refused(
            &floor,
            "review_flag_effect_events",
            "DROP TRIGGER review_flag_effect_events_immutable_delete;
             DELETE FROM review_flag_effect_events
              WHERE id = (SELECT MIN(id) FROM review_flag_effect_events);",
        );
        assert_missing_is_refused(
            &floor,
            "review_flag_effect_reversals",
            "DROP TRIGGER review_flag_effect_reversals_immutable_delete;
             DELETE FROM review_flag_effect_reversals
              WHERE flag_effect_event_id = (SELECT MIN(flag_effect_event_id) FROM review_flag_effect_reversals);",
        );
        assert_missing_is_refused(
            &floor,
            "correction_memory",
            "DROP TRIGGER correction_memory_v60_immutable_delete;
             DELETE FROM correction_memory
              WHERE id = (SELECT MIN(id) FROM correction_memory WHERE legacy_seed=0);",
        );
        assert_missing_is_refused(
            &floor,
            "correction_memory_contributions",
            "DROP TRIGGER correction_memory_contributions_immutable_delete;
             DELETE FROM correction_memory_contributions
              WHERE rowid = (SELECT MIN(rowid) FROM correction_memory_contributions);",
        );
        assert_missing_is_refused(
            &floor,
            "corrections",
            "DROP TRIGGER corrections_v60_effect_immutable_delete;
             DELETE FROM corrections
              WHERE effect_event_id = (SELECT MIN(effect_event_id) FROM corrections WHERE effect_event_id IS NOT NULL);",
        );
        assert_missing_is_refused(
            &floor,
            "effect-bound agent examples",
            "DROP TRIGGER agent_examples_v60_effect_immutable_delete;
             DELETE FROM agent_examples
              WHERE effect_event_id = (SELECT MIN(effect_event_id) FROM agent_examples WHERE effect_event_id IS NOT NULL);",
        );
    }

    #[test]
    fn pristine_v60_state_is_not_activity_but_a_nonzero_frontier_is() {
        let pristine = crate::db::Database::open(":memory:").unwrap();
        pristine.initialize().unwrap();
        assert!(!has_durable_review_activity(&pristine).unwrap());

        pristine.connection().execute("DROP TRIGGER review_effect_state_immutable_update", []).unwrap();
        pristine
            .connection()
            .execute("UPDATE review_effect_state SET effective_after_review_event_id = 1", [])
            .unwrap();
        assert!(
            has_durable_review_activity(&pristine).unwrap(),
            "a captured pre-v60 frontier remains durable activity even if legacy rows are later missing"
        );
    }

    #[test]
    fn durable_restore_floor_protects_reviewed_segment_export_and_pay_identity_projection() {
        let base = crate::db::Database::open(":memory:").unwrap();
        base.initialize().unwrap();
        base.insert_segment(&test_segment("reviewed-1", "reviewed.wav", "machine draft")).unwrap();
        base.insert_segment(&test_segment("desktop-accept", "desktop.wav", "desktop draft")).unwrap();
        base.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?1,
                        audio_fingerprint = 123456,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}',
                        duration_ms = 1000,
                        human_decision = 'edit', verdict = 'human_corrected',
                        verdict_transcript = 'human truth', annotated_transcript = 'human truth',
                        verified = 1, reviewed_by = 'Reviewer',
                        corrected_at = '2026-08-22 00:00:00', escalated = 0
                  WHERE id = 'reviewed-1'",
                ["a".repeat(64)],
            )
            .unwrap();
        base.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?1, audio_fingerprint = 654321,
                        alignment_json = '{\"source_start_ms\":1000,\"source_end_ms\":2000}',
                        duration_ms = 1000, human_decision = 'accept',
                        verdict = 'human_verified', verdict_transcript = 'desktop truth',
                        annotated_transcript = 'desktop truth', verified = 1,
                        corrected_at = '2026-08-22 00:00:01', escalated = 0
                  WHERE id = 'desktop-accept'",
                ["b".repeat(64)],
            )
            .unwrap();
        base.connection()
            .execute(
                "INSERT INTO review_events
                    (id, segment_id, reviewer, action, source, timestamp_ms, duration_ms,
                     compensation_action, created_at)
                 VALUES (1, 'reviewed-1', 'Reviewer', 'edit', 'legacy', 1, 1000, 'edit',
                         '2026-08-22 00:00:00')",
                [],
            )
            .unwrap();
        let floor = copied_database(&base);
        let equal = copied_database(&base);
        require_durable_review_history_superset(&floor, &equal).unwrap();

        let modified = copied_database(&base);
        modified
            .connection()
            .execute("UPDATE speech_segments SET duration_ms = 999 WHERE id = 'reviewed-1'", [])
            .unwrap();
        let error = require_durable_review_history_superset(&floor, &modified).unwrap_err();
        assert!(error.contains("reviewed speech-segment export projection"), "{error}");

        let missing = copied_database(&base);
        missing.connection().execute("DROP TRIGGER speech_segments_v60_review_authority_immutable_delete", []).unwrap();
        missing.delete_segment("reviewed-1").unwrap();
        let error = require_durable_review_history_superset(&floor, &missing).unwrap_err();
        assert!(error.contains("reviewed speech-segment export projection"), "{error}");

        let unaudited_desktop_regression = copied_database(&base);
        unaudited_desktop_regression
            .connection()
            .execute("DROP TRIGGER speech_segments_v60_review_authority_immutable_delete", [])
            .unwrap();
        unaudited_desktop_regression.delete_segment("desktop-accept").unwrap();
        let error = require_durable_review_history_superset(&floor, &unaudited_desktop_regression).unwrap_err();
        assert!(
            error.contains("reviewed speech-segment export projection"),
            "an unaudited desktop human decision must still be protected: {error}"
        );
    }

    #[test]
    fn consent_revocation_floor_follows_content_identity_across_relink_and_blocks_unrevoked_aliases() {
        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        floor.insert_segment(&test_segment("withdrawn", "old-name.wav", "withdrawn recording")).unwrap();
        let content_hash = "c".repeat(64);
        floor
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?1, rights_revoked_at = '2026-08-26 00:00:00'
                  WHERE id = 'withdrawn'",
                [&content_hash],
            )
            .unwrap();

        let renamed = copied_database(&floor);
        renamed
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_path = 'renamed.wav', rights_revoked_at = NULL
                  WHERE id = 'withdrawn'",
                [],
            )
            .unwrap();
        let error = require_consent_revocation_superset(&floor, &renamed).unwrap_err();
        assert!(error.contains("resurrect 1 withdrawn recording"), "{error}");

        renamed
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET rights_revoked_at = '2026-08-26 00:00:00'
                  WHERE id = 'withdrawn'",
                [],
            )
            .unwrap();
        require_consent_revocation_superset(&floor, &renamed)
            .expect("a relinked recording remains the same withdrawal authority through its canonical PCM hash");

        renamed.insert_segment(&test_segment("unrevoked-alias", "alias.wav", "same withdrawn recording")).unwrap();
        renamed
            .connection()
            .execute("UPDATE speech_segments SET audio_content_hash = ?1 WHERE id = 'unrevoked-alias'", [&content_hash])
            .unwrap();
        let alias_error = require_consent_revocation_superset(&floor, &renamed).unwrap_err();
        assert!(
            alias_error.contains("resurrect 1 withdrawn recording"),
            "an unrevoked alias of withdrawn PCM must not become an export bypass: {alias_error}"
        );
    }

    #[test]
    fn staged_compensation_semantics_accept_writer_history_and_refuse_segment_deletion() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_canonical_pay_segment(&db, "pay-valid");
        record_canonical_skip(&db, "pay-valid", "Reviewer", 1);
        validate_review_compensation_semantics(&db).unwrap();

        let deletion = db.delete_segment("pay-valid").unwrap_err();
        assert!(matches!(deletion, crate::error::AppError::Validation(_)), "{deletion}");
        assert!(db.get_segment_by_id("pay-valid").unwrap().is_some());
        validate_review_compensation_semantics(&db)
            .expect("a refused deletion must preserve immutable pay/event snapshots and their clip");
    }

    #[test]
    fn staged_compensation_semantics_reject_forged_identity_source_and_entry_key() {
        let base = crate::db::Database::open(":memory:").unwrap();
        base.initialize().unwrap();
        insert_canonical_pay_segment(&base, "pay-forge");
        record_canonical_skip(&base, "pay-forge", "Reviewer", 2);

        let split_work = copied_database(&base);
        split_work.connection().execute("DROP TRIGGER review_compensation_ledger_immutable_update", []).unwrap();
        split_work
            .connection()
            .execute(
                "UPDATE review_compensation_ledger
                    SET canonical_work_id = ?1",
                [format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:0:1000", "c".repeat(64))],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&split_work).unwrap_err();
        assert!(error.contains("segment identity"), "forged work split must fail: {error}");

        let wrong_source = copied_database(&base);
        wrong_source.connection().execute("DROP TRIGGER review_compensation_ledger_immutable_update", []).unwrap();
        wrong_source.connection().execute("DROP TRIGGER review_events_v60_post_cutoff_immutable_update", []).unwrap();
        wrong_source.connection().execute("UPDATE review_events SET source = 'test'", []).unwrap();
        wrong_source.connection().execute("UPDATE review_compensation_ledger SET source = 'test'", []).unwrap();
        let error = validate_review_compensation_semantics(&wrong_source).unwrap_err();
        assert!(error.contains("production Couch action"), "nonproduction pay source must fail: {error}");

        let wrong_key = copied_database(&base);
        wrong_key.connection().execute("DROP TRIGGER review_compensation_ledger_immutable_update", []).unwrap();
        wrong_key
            .connection()
            .execute("UPDATE review_compensation_ledger SET entry_key = 'review-event:999'", [])
            .unwrap();
        let error = validate_review_compensation_semantics(&wrong_key).unwrap_err();
        assert!(error.contains("disagrees with review event"), "forged event key must fail: {error}");

        let duplicate_operation = crate::db::Database::open(":memory:").unwrap();
        duplicate_operation.initialize().unwrap();
        insert_canonical_pay_segment(&duplicate_operation, "duplicate-op-a");
        insert_canonical_pay_segment(&duplicate_operation, "duplicate-op-b");
        duplicate_operation.connection().execute("DROP INDEX idx_review_events_operation_id", []).unwrap();
        record_canonical_skip(&duplicate_operation, "duplicate-op-a", "Reviewer", 11);
        record_canonical_skip(&duplicate_operation, "duplicate-op-b", "Reviewer", 11);
        let error = validate_review_compensation_semantics(&duplicate_operation).unwrap_err();
        assert!(error.contains("unique canonical lowercase UUID"), "duplicate operation UUID must fail: {error}");
    }

    #[test]
    fn staged_compensation_semantics_validate_undo_linkage_and_settlement_math() {
        let undo_db = crate::db::Database::open(":memory:").unwrap();
        undo_db.initialize().unwrap();
        insert_canonical_pay_segment(&undo_db, "pay-undo");
        canonical_phone_playback(&undo_db, "pay-undo", "Reviewer");
        let served_revision = undo_db.segment_review_revision("pay-undo").unwrap().unwrap();
        let operation = canonical_operation(3);
        undo_db
            .record_phone_human_decision_by_at_revision_with_operation(
                "pay-undo",
                "reject",
                None,
                "Reviewer",
                served_revision,
                &operation,
                &crate::db::review_operation_payload_hash("pay-undo", "reject", "", "Reviewer"),
            )
            .unwrap()
            .unwrap();
        let effect_id = undo_db.human_decision_effect_for_operation(&operation).unwrap().unwrap().0;
        assert!(matches!(
            undo_db.undo_human_decision(effect_id, Some("Reviewer"), &operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        validate_review_compensation_semantics(&undo_db).unwrap();

        let wrong_undo = copied_database(&undo_db);
        wrong_undo.connection().execute("DROP TRIGGER review_compensation_ledger_immutable_update", []).unwrap();
        wrong_undo
            .connection()
            .execute(
                "UPDATE review_compensation_ledger SET entry_key = ?1
                  WHERE compensation_action = 'undo'",
                [format!("undo:{}", canonical_operation(4))],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&wrong_undo).unwrap_err();
        assert!(error.contains("operation/event linkage"), "wrong undo operation must fail: {error}");

        let settled = crate::db::Database::open(":memory:").unwrap();
        settled.initialize().unwrap();
        insert_canonical_pay_segment(&settled, "pay-settle");
        canonical_phone_playback(&settled, "pay-settle", "Reviewer");
        let revision = settled.segment_review_revision("pay-settle").unwrap().unwrap();
        settled
            .record_phone_human_decision_by_at_revision_with_operation(
                "pay-settle",
                "reject",
                None,
                "Reviewer",
                revision,
                &canonical_operation(5),
                &crate::db::review_operation_payload_hash("pay-settle", "reject", "", "Reviewer"),
            )
            .unwrap()
            .unwrap();
        let through: i64 = settled
            .connection()
            .query_row("SELECT MAX(id) FROM review_compensation_ledger", [], |row| row.get(0))
            .unwrap();
        settled.record_review_compensation_settlement("Reviewer", through, "payout-1").unwrap();
        validate_review_compensation_semantics(&settled).unwrap();

        let forged_settlement = copied_database(&settled);
        forged_settlement
            .connection()
            .execute("DROP TRIGGER review_compensation_settlement_immutable_update", [])
            .unwrap();
        forged_settlement
            .connection()
            .execute(
                "UPDATE review_compensation_settlements
                    SET allocated_micro_iqd = allocated_micro_iqd + 1",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&forged_settlement).unwrap_err();
        assert!(error.contains("amount differs"), "forged settlement amount must fail: {error}");
    }

    #[test]
    fn staged_playback_semantics_reject_no_listen_and_future_revision_receipts() {
        let valid = crate::db::Database::open(":memory:").unwrap();
        valid.initialize().unwrap();
        insert_canonical_pay_segment(&valid, "playback-valid");
        valid
            .record_playback_receipt(&crate::db::PlaybackReceipt {
                segment_id: "playback-valid".to_string(),
                segment_revision: 0,
                audio_content_hash: "f".repeat(64),
                reviewer: Some("Reviewer".to_string()),
                session_id: Some("session".to_string()),
                started_at_ms: 1,
                played_ms: 900,
                clip_duration_ms: 1000,
                source_start_ms: None,
                source_end_ms: None,
            })
            .unwrap();
        validate_playback_receipt_semantics(&valid).unwrap();

        // Speaker/quality metadata is allowed to advance the review revision after listening.  It
        // must not invalidate the immutable policy-3 audio identity the receipt actually proves.
        valid.set_speaker_change_score("playback-valid", 0.37).unwrap();
        validate_playback_receipt_semantics(&valid)
            .expect("an unrelated metadata revision bump must preserve exact policy-3 evidence");

        let mismatched_old_revision_hash = copied_database(&valid);
        mismatched_old_revision_hash
            .connection()
            .execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", [])
            .unwrap();
        mismatched_old_revision_hash
            .connection()
            .execute("UPDATE playback_receipts SET audio_fingerprint = ?1", ["b".repeat(64)])
            .unwrap();
        let error = validate_playback_receipt_semantics(&mismatched_old_revision_hash).unwrap_err();
        assert!(
            error.contains("retained segment identity"),
            "a metadata revision bump must not hide a forged historical BLAKE3 identity: {error}"
        );

        let wrong_span = copied_database(&valid);
        wrong_span.connection().execute("DROP TRIGGER speech_segments_review_revision", []).unwrap();
        wrong_span.connection().execute("DROP TRIGGER speech_segments_v60_paid_identity_immutable_update", []).unwrap();
        wrong_span
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET alignment_json = json_object(
                        'source_start_ms', 2000, 'source_end_ms', 3000,
                        'chunk_index', 0, 'chunk_count', 1
                    )
                  WHERE id = 'playback-valid'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&wrong_span).unwrap_err();
        assert!(
            error.contains("playback receipt") && error.contains("segment identity"),
            "same hash/revision/duration on a different source window must fail the actual restore gate: {error}"
        );

        let no_listen = copied_database(&valid);
        no_listen.connection().execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", []).unwrap();
        no_listen.connection().execute("UPDATE playback_receipts SET played_ms = 0, coverage_ratio = 1.0", []).unwrap();
        let error = validate_playback_receipt_semantics(&no_listen).unwrap_err();
        assert!(error.contains("writer invariants"), "forged no-listen receipt must fail: {error}");

        let future = copied_database(&valid);
        future.connection().execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", []).unwrap();
        future
            .connection()
            .execute(
                "UPDATE playback_receipts
                    SET segment_revision = (
                        SELECT review_revision + 1
                          FROM speech_segments
                         WHERE id = playback_receipts.segment_id
                    )",
                [],
            )
            .unwrap();
        let error = validate_playback_receipt_semantics(&future).unwrap_err();
        assert!(error.contains("future segment revision"), "pre-minted future receipt must fail: {error}");

        let no_content_hash = copied_database(&valid);
        no_content_hash
            .connection()
            .execute("DROP TRIGGER speech_segments_v60_paid_identity_immutable_update", [])
            .unwrap();
        no_content_hash
            .connection()
            .execute("UPDATE speech_segments SET audio_content_hash = NULL WHERE id = 'playback-valid'", [])
            .unwrap();
        let error = validate_playback_receipt_semantics(&no_content_hash).unwrap_err();
        assert!(
            error.contains("no canonical server-derived segment BLAKE3 identity"),
            "a restore must not invent audio identity from a segment id: {error}"
        );

        let legacy = copied_database(&valid);
        legacy.connection().execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", []).unwrap();
        legacy
            .connection()
            .execute(
                "UPDATE playback_receipts
                    SET policy_version = 1, audio_fingerprint = '424242',
                        source_start_ms = NULL, source_end_ms = NULL",
                [],
            )
            .unwrap();
        validate_playback_receipt_semantics(&legacy)
            .expect("policy-1 spectral receipts remain historical/readable but never authorize policy 3");
    }

    #[test]
    fn restore_semantics_accept_exact_policy4_and_reject_interval_or_consumption_drift() {
        let valid = crate::db::Database::open(":memory:").unwrap();
        valid.initialize().unwrap();
        record_canonical_phone_edit(&valid, "policy4-restore", 98);
        validate_restore_target_semantics(&valid)
            .expect("an exact writer-produced policy-4 review generation must remain restorable");

        let forged_interval = copied_database(&valid);
        forged_interval
            .connection()
            .execute("DROP TRIGGER playback_receipts_v67_policy4_immutable_update", [])
            .unwrap();
        forged_interval
            .connection()
            .execute(
                "UPDATE playback_receipts
                    SET interval_union_sha256 = ?1
                  WHERE policy_version = ?2",
                rusqlite::params!["0".repeat(64), crate::db::DESKTOP_PLAYBACK_POLICY_VERSION],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&forged_interval).unwrap_err();
        assert!(error.contains("policy-4 playback authority is invalid"), "{error}");

        let forged_consumption = copied_database(&valid);
        forged_consumption
            .connection()
            .execute("DROP TRIGGER playback_authority_consumptions_v4_immutable_update", [])
            .unwrap();
        forged_consumption
            .connection()
            .execute("UPDATE playback_authority_consumptions_v4 SET operation_id = ?1", [canonical_operation(999)])
            .unwrap();
        let error = validate_restore_target_semantics(&forged_consumption).unwrap_err();
        assert!(error.contains("no exact consumed policy-3/4 playback authority"), "{error}");
    }

    #[test]
    fn staged_review_effect_semantics_accept_every_current_writer_state() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        record_canonical_phone_edit(&db, "effect-active-edit", 101);

        insert_canonical_pay_segment(&db, "effect-undone-reject");
        let reject_revision = db.segment_review_revision("effect-undone-reject").unwrap().unwrap();
        canonical_phone_playback(&db, "effect-undone-reject", "Reviewer");
        let reject_operation = canonical_operation(102);
        db.record_phone_human_decision_by_at_revision_with_operation(
            "effect-undone-reject",
            "reject",
            None,
            "Reviewer",
            reject_revision,
            &reject_operation,
            &crate::db::review_operation_payload_hash("effect-undone-reject", "reject", "", "Reviewer"),
        )
        .unwrap()
        .unwrap();
        let reject_effect = db.human_decision_effect_for_operation(&reject_operation).unwrap().unwrap().0;
        assert!(matches!(
            db.undo_human_decision(reject_effect, Some("Reviewer"), &reject_operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));

        insert_canonical_pay_segment(&db, "effect-desktop-edit");
        db.finalize_human_review("effect-desktop-edit", "edit", Some("desktop truth"), None, None).unwrap();

        insert_canonical_pay_segment(&db, "effect-active-flag");
        db.record_review_flag_for_test(
            "effect-active-flag",
            "needs another listen",
            "00000000-0000-4000-8000-000000000802",
        )
        .unwrap();
        db.set_speaker_change_score("effect-active-edit", 0.41).unwrap();
        db.set_speaker_change_score("effect-active-flag", 0.42).unwrap();
        insert_canonical_pay_segment(&db, "effect-undone-flag");
        let undone_flag = db
            .record_review_flag_for_test(
                "effect-undone-flag",
                "temporary concern",
                "00000000-0000-4000-8000-000000000803",
            )
            .unwrap();
        db.set_speaker_change_score("effect-undone-flag", 0.43).unwrap();
        assert!(matches!(
            db.undo_review_flag(undone_flag.effect_event_id, &canonical_operation(103)).unwrap(),
            crate::db::HumanFlagUndoOutcome::Applied { .. }
        ));

        validate_restore_target_semantics(&db)
            .expect("every current phone/desktop/flag writer state must pass the actual restore gate");
    }

    #[test]
    fn staged_restore_rejects_orphaned_sequential_campaign_authority() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_campaign_registry
                    (campaign_id, focus_segment_count, focus_sha256, first_reviewer, second_reviewer,
                     after_review_event_id, activated_at_review_event_id)
                 VALUES('123e4567-e89b-42d3-a456-426614174000', 1, ?1, 'Rubar', 'Alle', 0, 0)",
                ["a".repeat(64)],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&db).unwrap_err();
        assert!(
            error.contains("campaign authority") && error.contains("without its base campaign policy"),
            "orphaned campaign authority must fail the actual restore gate: {error}"
        );
    }

    #[test]
    fn staged_restore_rejects_effect_correction_with_wrong_but_valid_audio_content_hash() {
        let base = crate::db::Database::open(":memory:").unwrap();
        base.initialize().unwrap();
        record_canonical_phone_edit(&base, "effect-wrong-content-hash", 104);
        validate_restore_target_semantics(&base).unwrap();

        let forged = copied_database(&base);
        forged.connection().execute("DROP TRIGGER corrections_v60_effect_immutable_update", []).unwrap();
        forged
            .connection()
            .execute(
                "UPDATE corrections SET audio_content_hash = ?1 WHERE effect_event_id IS NOT NULL",
                ["b".repeat(64)],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&forged).unwrap_err();
        assert!(
            error.contains("effect-bound correction") && error.contains("audio"),
            "a different canonical decoded-PCM BLAKE3 hash must fail the real restore gate: {error}"
        );
    }

    #[test]
    fn staged_restore_rejects_forged_effect_artifacts_current_state_and_inverse() {
        let active = crate::db::Database::open(":memory:").unwrap();
        active.initialize().unwrap();
        let (_effect_id, _operation) = record_canonical_phone_edit(&active, "effect-forgery", 105);
        validate_restore_target_semantics(&active).unwrap();

        let mismatched_example = copied_database(&active);
        mismatched_example.connection().execute("DROP TRIGGER agent_examples_v60_effect_immutable_update", []).unwrap();
        mismatched_example
            .connection()
            .execute("UPDATE agent_examples SET human_fix = 'forged truth' WHERE effect_event_id IS NOT NULL", [])
            .unwrap();
        let error = validate_restore_target_semantics(&mismatched_example).unwrap_err();
        assert!(error.contains("agent example"), "example/correction split must fail: {error}");

        let forged_memory = copied_database(&active);
        forged_memory.connection().execute("DROP TRIGGER correction_memory_v60_baseline_immutable_update", []).unwrap();
        forged_memory
            .connection()
            .execute("UPDATE correction_memory SET hit_count = 1 WHERE legacy_seed = 0", [])
            .unwrap();
        let error = validate_restore_target_semantics(&forged_memory).unwrap_err();
        assert!(error.contains("zero-baseline"), "mutable memory evidence must fail: {error}");

        let arbitrary_capture = copied_database(&active);
        let effect_id: i64 = arbitrary_capture
            .connection()
            .query_row("SELECT id FROM human_decision_effect_events WHERE segment_id = 'effect-forgery'", [], |row| {
                row.get(0)
            })
            .unwrap();
        arbitrary_capture
            .connection()
            .execute(
                "INSERT INTO correction_memory
                    (id, wrong_token, human_token, slot_key, phonetic_key, source_segment,
                     confidence, hit_count, confirm_count, override_count, legacy_seed)
                 VALUES ('00000000-0000-4000-8000-000000000806', 'forged-wrong',
                         'forged-fix', 'forged|slot', 'forged', 'effect-forgery',
                         0.5, 0, 0, 0, 0)",
                [],
            )
            .unwrap();
        arbitrary_capture
            .connection()
            .execute(
                "INSERT INTO correction_memory_contributions
                    (effect_event_id, memory_id, capture_delta, confirm_delta, override_delta)
                 VALUES (?1, '00000000-0000-4000-8000-000000000806', 1, 0, 0)",
                [effect_id],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&arbitrary_capture).unwrap_err();
        assert!(
            error.contains("arbitrary or incomplete correction-memory captures"),
            "an arbitrary effect-bound memory capture must fail: {error}"
        );

        let arbitrary_outcome = copied_database(&active);
        arbitrary_outcome
            .connection()
            .execute("DROP TRIGGER correction_memory_contributions_immutable_update", [])
            .unwrap();
        arbitrary_outcome
            .connection()
            .execute(
                "UPDATE correction_memory_contributions
                    SET confirm_delta = 1, fired_at = '2026-08-22 00:00:00'
                  WHERE capture_delta = 1",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&arbitrary_outcome).unwrap_err();
        assert!(
            error.contains("not re-derived from the served/decision text"),
            "a fabricated memory confirmation must fail: {error}"
        );

        let stale_current = copied_database(&active);
        stale_current
            .connection()
            .execute("UPDATE speech_segments SET verdict = 'human_reject' WHERE id = 'effect-forgery'", [])
            .unwrap();
        let error = validate_restore_target_semantics(&stale_current).unwrap_err();
        assert!(error.contains("latest active human-decision"), "forged current state must fail: {error}");

        let undone = crate::db::Database::open(":memory:").unwrap();
        undone.initialize().unwrap();
        let (effect_id, operation) = record_canonical_phone_edit(&undone, "effect-inverse", 106);
        assert!(matches!(
            undone.undo_human_decision(effect_id, Some("Reviewer"), &operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        validate_restore_target_semantics(&undone).unwrap();
        let forged_inverse = copied_database(&undone);
        forged_inverse
            .connection()
            .execute("DROP TRIGGER human_decision_effect_reversals_immutable_update", [])
            .unwrap();
        forged_inverse
            .connection()
            .execute("UPDATE human_decision_effect_reversals SET operation_id = ?1", [canonical_operation(107)])
            .unwrap();
        let error = validate_restore_target_semantics(&forged_inverse).unwrap_err();
        assert!(error.contains("operation-bound compensation inverse"), "wrong inverse identity must fail: {error}");
    }

    #[test]
    fn staged_restore_rejects_forged_flag_state_and_reversal_identity() {
        let active = crate::db::Database::open(":memory:").unwrap();
        active.initialize().unwrap();
        insert_canonical_pay_segment(&active, "flag-active-forgery");
        active
            .record_review_flag_for_test("flag-active-forgery", "listen again", "00000000-0000-4000-8000-000000000804")
            .unwrap();
        validate_restore_target_semantics(&active).unwrap();

        let laundered_human_truth = copied_database(&active);
        laundered_human_truth
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET verified = 1, annotated_transcript = 'forged unbound truth'
                  WHERE id = 'flag-active-forgery'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&laundered_human_truth).unwrap_err();
        assert!(
            error.contains("unsnapshotted human review truth")
                || error.contains("unbound human transcript/verification state"),
            "a flag effect must not launder unrelated verified/annotated truth: {error}"
        );

        active
            .connection()
            .execute("UPDATE speech_segments SET verdict = NULL WHERE id = 'flag-active-forgery'", [])
            .unwrap();
        let error = validate_restore_target_semantics(&active).unwrap_err();
        assert!(error.contains("latest active review-flag"), "forged active flag state must fail: {error}");

        let undone = crate::db::Database::open(":memory:").unwrap();
        undone.initialize().unwrap();
        insert_canonical_pay_segment(&undone, "flag-undo-forgery");
        let flag = undone
            .record_review_flag_for_test("flag-undo-forgery", "temporary flag", "00000000-0000-4000-8000-000000000805")
            .unwrap();
        let undo_operation = canonical_operation(110);
        assert!(matches!(
            undone.undo_review_flag(flag.effect_event_id, &undo_operation).unwrap(),
            crate::db::HumanFlagUndoOutcome::Applied { .. }
        ));
        validate_restore_target_semantics(&undone).unwrap();

        let wrong_identity = copied_database(&undone);
        wrong_identity.connection().execute("DROP TRIGGER review_flag_effect_reversals_immutable_update", []).unwrap();
        wrong_identity
            .connection()
            .execute("UPDATE review_flag_effect_reversals SET operation_id = 'not-a-uuid'", [])
            .unwrap();
        let error = validate_restore_target_semantics(&wrong_identity).unwrap_err();
        assert!(
            error.contains("review-flag effect") && error.contains("operation"),
            "forged flag undo must fail: {error}"
        );

        let stale_inverse = copied_database(&undone);
        stale_inverse
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET verdict = 'escalated', rationale = 'forged', escalated = 1
                  WHERE id = 'flag-undo-forgery'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&stale_inverse).unwrap_err();
        assert!(
            error.contains("review-flag reversal") || error.contains("exact mixed decision/flag effect chain"),
            "a stale flag after reversal must fail: {error}"
        );

        let legacy = crate::db::Database::open(":memory:").unwrap();
        legacy.initialize().unwrap();
        rollback_to_legacy_machine_schema(&legacy);
        let mut legacy_segment = test_segment("flag-legacy-authority", "flag-legacy.wav", "machine draft");
        legacy_segment.verified = true;
        legacy_segment.annotated_transcript = Some("immutable legacy truth".into());
        legacy.insert_segment_full(&legacy_segment).unwrap();
        migrate_legacy_machine_schema_to_head(&legacy);
        legacy
            .record_review_flag_for_test(
                "flag-legacy-authority",
                "legacy concern",
                "00000000-0000-4000-8000-000000000807",
            )
            .unwrap();
        validate_restore_target_semantics(&legacy)
            .expect("an exact immutable pre-v60 reviewed baseline remains a valid first flag origin");
    }

    #[test]
    fn mixed_flag_decision_chains_preserve_exact_rationale_through_undo_and_restore() {
        let flag_then_decision = crate::db::Database::open(":memory:").unwrap();
        flag_then_decision.initialize().unwrap();
        rollback_to_legacy_machine_schema(&flag_then_decision);
        insert_canonical_pay_segment(&flag_then_decision, "rationale-flag-decision");
        flag_then_decision
            .write_segment_verdict(
                "rationale-flag-decision",
                "jury_accept",
                Some("machine draft"),
                Some("machine rationale"),
                None,
                Some(0.9),
                false,
            )
            .unwrap();
        migrate_legacy_machine_schema_to_head(&flag_then_decision);
        flag_then_decision
            .record_review_flag_for_test(
                "rationale-flag-decision",
                "flag rationale",
                "00000000-0000-4000-8000-000000000808",
            )
            .unwrap();
        flag_then_decision.finalize_human_review("rationale-flag-decision", "accept", None, Some(1), None).unwrap();
        validate_restore_target_semantics(&flag_then_decision)
            .expect("flag-to-decision chain must retain the exact flag rationale");

        let forged_decision_prior = copied_database(&flag_then_decision);
        forged_decision_prior
            .connection()
            .execute("DROP TRIGGER human_decision_effect_events_immutable_update", [])
            .unwrap();
        forged_decision_prior
            .connection()
            .execute(
                "UPDATE human_decision_effect_events
                    SET prior_rationale = 'forged prior', decision_rationale = 'forged prior'
                  WHERE segment_id = 'rationale-flag-decision'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&forged_decision_prior).unwrap_err();
        assert!(error.contains("rationale"), "a decision must not invent the flag rationale it inherited: {error}");

        let decision_effect: i64 = flag_then_decision
            .connection()
            .query_row(
                "SELECT id FROM human_decision_effect_events
                  WHERE segment_id = 'rationale-flag-decision'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(matches!(
            flag_then_decision
                .undo_latest_desktop_human_decision(
                    &match flag_then_decision.desktop_review_undo_availability().unwrap() {
                        crate::db::DesktopReviewUndoAvailability::Available(
                            crate::db::DesktopReviewUndoAuthority::Decision(authority),
                        ) => {
                            assert_eq!(authority.effect_event_id, decision_effect);
                            authority
                        }
                        other => panic!("expected decision Undo authority, got {other:?}"),
                    },
                    "00000000-0000-4000-8000-000000000809",
                )
                .unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        assert_eq!(
            flag_then_decision.get_segment_by_id("rationale-flag-decision").unwrap().unwrap().rationale.as_deref(),
            Some("flag rationale")
        );
        validate_restore_target_semantics(&flag_then_decision)
            .expect("decision Undo must restore the exact preceding active flag state");

        let decision_then_flag = crate::db::Database::open(":memory:").unwrap();
        decision_then_flag.initialize().unwrap();
        rollback_to_legacy_machine_schema(&decision_then_flag);
        insert_canonical_pay_segment(&decision_then_flag, "rationale-decision-flag");
        decision_then_flag
            .write_segment_verdict(
                "rationale-decision-flag",
                "jury_accept",
                Some("machine draft"),
                Some("original rationale"),
                None,
                Some(0.9),
                false,
            )
            .unwrap();
        migrate_legacy_machine_schema_to_head(&decision_then_flag);
        decision_then_flag.finalize_human_review("rationale-decision-flag", "accept", None, Some(2), None).unwrap();
        let effect_id: i64 = decision_then_flag
            .connection()
            .query_row(
                "SELECT id FROM human_decision_effect_events
                  WHERE segment_id = 'rationale-decision-flag'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(matches!(
            decision_then_flag
                .undo_latest_desktop_human_decision(
                    &match decision_then_flag.desktop_review_undo_availability().unwrap() {
                        crate::db::DesktopReviewUndoAvailability::Available(
                            crate::db::DesktopReviewUndoAuthority::Decision(authority),
                        ) => {
                            assert_eq!(authority.effect_event_id, effect_id);
                            authority
                        }
                        other => panic!("expected decision Undo authority, got {other:?}"),
                    },
                    "00000000-0000-4000-8000-000000000810",
                )
                .unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        decision_then_flag
            .record_review_flag_for_test(
                "rationale-decision-flag",
                "later flag rationale",
                "00000000-0000-4000-8000-000000000811",
            )
            .unwrap();
        validate_restore_target_semantics(&decision_then_flag)
            .expect("a reversed decision followed by a flag must preserve the original rationale prior-state");

        let forged_flag_prior = copied_database(&decision_then_flag);
        forged_flag_prior.connection().execute("DROP TRIGGER review_flag_effect_events_immutable_update", []).unwrap();
        forged_flag_prior
            .connection()
            .execute(
                "UPDATE review_flag_effect_events
                    SET prior_rationale = 'forged prior'
                  WHERE segment_id = 'rationale-decision-flag'",
                [],
            )
            .unwrap();
        let error = validate_restore_target_semantics(&forged_flag_prior).unwrap_err();
        assert!(
            error.contains("rationale"),
            "a flag must not invent the rationale inherited from a decision/Undo chain: {error}"
        );
    }

    #[test]
    fn staged_review_effect_semantics_require_zero_effects_for_skip_and_spot_check() {
        for (source, action, operation_index) in [("couch", "skip", 108_u64), ("couch_spot_check", "edit", 109_u64)] {
            let db = crate::db::Database::open(":memory:").unwrap();
            db.initialize().unwrap();
            insert_canonical_pay_segment(&db, &format!("zero-effect-{source}"));
            let segment_id = format!("zero-effect-{source}");
            if source == "couch_spot_check" {
                db.record_spot_check(&segment_id, "Reviewer", action, "expected", "expected").unwrap();
                validate_review_effect_semantics(&db).unwrap();
            } else {
                db.record_review_event_with_operation(
                    &segment_id,
                    "Reviewer",
                    action,
                    source,
                    i64::try_from(operation_index).unwrap(),
                    &canonical_operation(operation_index),
                    &crate::db::review_operation_payload_hash(&segment_id, action, "", "Reviewer"),
                )
                .unwrap();
                validate_restore_target_semantics(&db).unwrap();
            }

            let event_id: i64 =
                db.connection().query_row("SELECT MAX(id) FROM review_events", [], |row| row.get(0)).unwrap();
            let prior_revision = db.segment_review_revision(&segment_id).unwrap().unwrap();
            db.connection()
                .execute("DROP TRIGGER human_decision_effect_events_validate_review_event_insert", [])
                .unwrap();
            db.connection()
                .execute(
                    "INSERT INTO human_decision_effect_events
                        (review_event_id, segment_id, reviewer, source, action,
                         served_transcript, decision_transcript, decision_annotated_transcript,
                         decision_verified, decision_corrected_at,
                         prior_revision, decision_revision, prior_verified,
                         prior_escalated)
                     VALUES (?1, ?2, 'Reviewer', 'couch', 'edit',
                             'served transcript', 'forged edit', 'forged edit', 1,
                             '2026-08-22 00:00:00',
                             ?3, ?3 + 1, 0, 0)",
                    rusqlite::params![event_id, segment_id, prior_revision],
                )
                .unwrap();
            let error = validate_review_effect_semantics(&db).unwrap_err();
            assert!(
                error.contains("must not create a human-decision effect"),
                "{source}/{action} forged effect must fail: {error}"
            );
        }
    }

    #[test]
    fn staged_restore_rejects_current_human_truth_without_legacy_or_effect_authority() {
        for (segment_id, reviewed_by) in
            [("forged-unbound-desktop", None), ("forged-unrostered-reviewer", Some("Mallory"))]
        {
            let db = crate::db::Database::open(":memory:").unwrap();
            db.initialize().unwrap();
            insert_canonical_pay_segment(&db, segment_id);
            db.connection()
                .execute(
                    "UPDATE speech_segments
                        SET human_decision = 'accept', verdict = 'human_accept',
                            verdict_transcript = raw_transcript,
                            annotated_transcript = raw_transcript, verified = 1,
                            reviewed_by = ?2, corrected_at = datetime('now')
                      WHERE id = ?1",
                    rusqlite::params![segment_id, reviewed_by],
                )
                .unwrap();
            let error = validate_restore_target_semantics(&db).unwrap_err();
            assert!(
                error.contains("neither immutable legacy authority nor a schema-v60 effect chain"),
                "unbound current human truth must fail for {segment_id}: {error}"
            );
        }
    }

    #[test]
    fn schema_v60_machine_only_dataset_merge_remains_restore_safe() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let first = vec![crate::db::SpeechSegment {
            id: "restore-machine-merge".to_string(),
            created_at: Some("2026-08-22 01:02:03".to_string()),
            audio_path: "restore-machine-merge.wav".to_string(),
            raw_transcript: "machine draft one".to_string(),
            normalized_transcript: Some("machine normalized one".to_string()),
            duration_ms: 1_000,
            confidence: Some(0.7),
            model_version_id: Some("omniasr-7b-champion".to_string()),
            confidence_source: Some("real_posterior".to_string()),
            ..Default::default()
        }];
        assert_eq!(db.merge_dataset_json(&serde_json::to_string(&first).unwrap()).unwrap(), (1, 0));
        validate_restore_target_semantics(&db).expect("machine-only inserted row is valid restore material");

        let replacement = vec![crate::db::SpeechSegment {
            id: "restore-machine-merge".to_string(),
            audio_path: "restore-machine-merge.wav".to_string(),
            raw_transcript: "machine draft two".to_string(),
            normalized_transcript: Some("machine normalized two".to_string()),
            duration_ms: 1_000,
            confidence: Some(0.8),
            model_version_id: Some("omniasr-7b-champion".to_string()),
            confidence_source: Some("real_posterior".to_string()),
            ..Default::default()
        }];
        assert_eq!(db.merge_dataset_json(&serde_json::to_string(&replacement).unwrap()).unwrap(), (0, 1));
        validate_restore_target_semantics(&db).expect("machine-only updated row remains valid restore material");
        let row = db.get_segment_by_id("restore-machine-merge").unwrap().unwrap();
        assert_eq!(row.raw_transcript, "machine draft two");
        assert!(row.annotated_transcript.is_none() && !row.verified && row.human_decision.is_none());
    }

    #[test]
    fn generic_machine_history_after_human_effect_preserves_exact_review_state_and_restore() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_canonical_pay_segment(&db, "restore-history-review");
        let original = db.get_segment_by_id("restore-history-review").unwrap().unwrap();
        let updated = crate::db::SpeechSegment {
            raw_transcript: "machine draft two".to_string(),
            speaker_id: Some("speaker-a".to_string()),
            ..original.clone()
        };
        db.insert_segment(&updated).unwrap();
        let history = crate::history::HistoryManager::new(10);
        history.record_segment_update(original, updated);

        db.finalize_human_review("restore-history-review", "accept", Some("machine draft two"), Some(123), None)
            .unwrap();
        let reviewed = db.get_segment_by_id("restore-history-review").unwrap().unwrap();
        validate_restore_target_semantics(&db).expect("decision state is restore-safe before generic history");

        history.undo(&db).expect("generic machine undo");
        let undone = db.get_segment_by_id("restore-history-review").unwrap().unwrap();
        assert_eq!(undone.raw_transcript, "machine draft");
        assert_eq!(undone.annotated_transcript, reviewed.annotated_transcript);
        assert_eq!(undone.verified, reviewed.verified);
        assert_eq!(undone.human_decision, reviewed.human_decision);
        assert_eq!(undone.verdict, reviewed.verdict);
        assert_eq!(undone.rationale, reviewed.rationale);
        assert_eq!(undone.reviewed_by, reviewed.reviewed_by);
        validate_restore_target_semantics(&db).expect("machine undo must preserve a valid exact effect graph");

        history.redo(&db).expect("generic machine redo");
        let redone = db.get_segment_by_id("restore-history-review").unwrap().unwrap();
        assert_eq!(redone.raw_transcript, "machine draft two");
        assert_eq!(redone.annotated_transcript, reviewed.annotated_transcript);
        assert_eq!(redone.verified, reviewed.verified);
        assert_eq!(redone.human_decision, reviewed.human_decision);
        assert_eq!(redone.verdict, reviewed.verdict);
        assert_eq!(redone.rationale, reviewed.rationale);
        assert_eq!(redone.reviewed_by, reviewed.reviewed_by);
        validate_restore_target_semantics(&db).expect("machine redo must preserve a valid exact effect graph");
    }

    #[test]
    fn staged_restore_rejects_a_forged_undo_of_a_shadowed_canonical_alias() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        insert_canonical_pay_segment(&db, "restore-alias-a");
        let proof_a = canonical_policy4_phone_playback(&db, "restore-alias-a", "Reviewer");
        let operation_a = canonical_operation(120);
        let commit_a = db
            .record_phone_human_decision_by_at_revision_with_operation_limit(
                "restore-alias-a",
                "accept",
                Some("machine draft"),
                "Reviewer",
                proof_a.segment_revision,
                &proof_a,
                &operation_a,
                &crate::db::review_operation_payload_hash("restore-alias-a", "accept", "machine draft", "Reviewer"),
                "accept",
                "machine draft",
                None,
            )
            .unwrap()
            .unwrap();

        let (_effect_b, _operation_b) = record_canonical_phone_edit(&db, "restore-alias-b", 121);
        validate_restore_target_semantics(&db).unwrap();

        // Model a trigger-disabled staged target that tries to retract A after B became the latest
        // entitlement mutation for the same reviewer/BLAKE3/source-span work identity.
        db.connection()
            .execute(
                "INSERT INTO review_compensation_ledger
                    (entry_id, entry_key, policy_version, canonical_work_id,
                     canonical_identity_kind, reviewer, segment_id, source,
                     compensation_action, effective_decision, decision_revision, duration_ms,
                     rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                     corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id)
                 SELECT '00000000-0000-4000-8000-000000000900',
                        'undo:' || ?2, original.policy_version, original.canonical_work_id,
                        original.canonical_identity_kind, original.reviewer,
                        original.segment_id, 'couch_undo', 'undo', 'undo',
                        original.decision_revision, original.duration_ms, 0, 0,
                        -original.delta_micro_iqd,
                        (SELECT COALESCE(SUM(delta_corrected_ms), 0)
                           FROM review_compensation_ledger
                          WHERE canonical_work_id = original.canonical_work_id)
                            - original.delta_corrected_ms,
                        -original.delta_corrected_ms, original.entry_id
                   FROM review_compensation_ledger original
                   JOIN human_decision_effect_events effect
                     ON effect.review_event_id = original.review_event_id
                  WHERE effect.id = ?1 AND original.reverses_entry_id IS NULL",
                rusqlite::params![commit_a.effect_event_id, operation_a],
            )
            .unwrap();
        db.connection()
            .execute(
                "INSERT INTO human_decision_effect_reversals (effect_event_id, operation_id)
                 VALUES (?1, ?2)",
                rusqlite::params![commit_a.effect_event_id, operation_a],
            )
            .unwrap();

        let error = validate_restore_target_semantics(&db).unwrap_err();
        assert!(
            error.contains("does not exactly bind its earlier decision entry"),
            "a shadowed alias reversal must fail the actual restore gate: {error}"
        );
    }

    #[test]
    fn staged_restore_exact_schema_contract_rejects_weakened_hidden_key_trigger() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("weakened-hidden-trigger.db");
        let candidate = crate::db::Database::open(path.to_string_lossy().as_ref()).unwrap();
        candidate.initialize().unwrap();
        candidate
            .connection()
            .execute_batch(
                "DROP TRIGGER review_pilot_hidden_keys_quota_insert;
                 CREATE TRIGGER review_pilot_hidden_keys_quota_insert
                 BEFORE INSERT ON review_pilot_hidden_keys
                 BEGIN SELECT 1; END;",
            )
            .unwrap();
        candidate.wal_checkpoint().unwrap();
        drop(candidate);

        let error = match crate::db::Database::stage_restore_source(&path) {
            Ok(_) => panic!("weakened hidden-key trigger must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("changed=") && error.contains("review_pilot_hidden_keys_quota_insert"),
            "staged restore must compare exact sqlite_schema SQL for hidden-key authority: {error}"
        );
    }

    #[test]
    fn staged_target_hidden_namespaces_and_used_policy_identity_are_fail_closed() {
        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        let policy = test_pilot_policy(0, "ReviewerA", "ReviewerB");
        let digest = policy.policy_sha256().unwrap();

        let valid = crate::db::Database::open(":memory:").unwrap();
        valid.initialize().unwrap();
        valid
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'reviewera', 'valid-1')", [&digest])
            .unwrap();
        require_active_pilot_policy_binding(&floor, None, &valid, &pilot_restore_action(&policy)).unwrap();

        let wrong_sha = crate::db::Database::open(":memory:").unwrap();
        wrong_sha.initialize().unwrap();
        wrong_sha
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'wrong-sha')", ["b".repeat(64)])
            .unwrap();
        let error =
            require_active_pilot_policy_binding(&floor, None, &wrong_sha, &pilot_restore_action(&policy)).unwrap_err();
        assert!(error.contains("SHA/baseline"), "{error}");

        let wrong_baseline = crate::db::Database::open(":memory:").unwrap();
        wrong_baseline.initialize().unwrap();
        wrong_baseline
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 1, 'ReviewerA', 'wrong-baseline')", [&digest])
            .unwrap();
        let error = require_active_pilot_policy_binding(&floor, None, &wrong_baseline, &pilot_restore_action(&policy))
            .unwrap_err();
        assert!(error.contains("SHA/baseline"), "{error}");

        let unauthorized = crate::db::Database::open(":memory:").unwrap();
        unauthorized.initialize().unwrap();
        unauthorized
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerC', 'unauthorized')", [&digest])
            .unwrap();
        let error = require_active_pilot_policy_binding(&floor, None, &unauthorized, &pilot_restore_action(&policy))
            .unwrap_err();
        assert!(error.contains("exact policy roster"), "{error}");

        let over_quota = crate::db::Database::open(":memory:").unwrap();
        over_quota.initialize().unwrap();
        over_quota.connection().execute("DROP TRIGGER review_pilot_hidden_keys_quota_insert", []).unwrap();
        for segment_id in ["over-1", "over-2", "over-3"] {
            over_quota
                .connection()
                .execute(
                    "INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', ?2)",
                    rusqlite::params![&digest, segment_id],
                )
                .unwrap();
        }
        let error =
            require_active_pilot_policy_binding(&floor, None, &over_quota, &pilot_restore_action(&policy)).unwrap_err();
        assert!(error.contains("structural") || error.contains("quota"), "{error}");

        let historical_conflict = crate::db::Database::open(":memory:").unwrap();
        historical_conflict.initialize().unwrap();
        historical_conflict.connection().execute("DROP TRIGGER review_pilot_hidden_keys_policy_insert", []).unwrap();
        historical_conflict
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 17, 'PastReviewer', 'past-1')", ["c".repeat(64)])
            .unwrap();
        historical_conflict
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 17, 'PastReviewer', 'past-2')", ["d".repeat(64)])
            .unwrap();
        let error = require_active_pilot_policy_binding(
            &floor,
            None,
            &historical_conflict,
            &SnapshotPilotPolicyRestore::ExplicitlyAbsent,
        )
        .unwrap_err();
        assert!(error.contains("one-policy-per-baseline"), "{error}");

        let used_floor = crate::db::Database::open(":memory:").unwrap();
        used_floor.initialize().unwrap();
        insert_test_review_event(&used_floor, "pilot-work", "ReviewerA", "skip", "couch", 1);
        let used_target = copied_database(&used_floor);
        require_active_pilot_policy_binding(&used_floor, Some(&policy), &used_target, &pilot_restore_action(&policy))
            .unwrap();
        let replacement = test_pilot_policy(0, "ReviewerA", "ReviewerC");
        let error = require_active_pilot_policy_binding(
            &used_floor,
            Some(&policy),
            &used_target,
            &pilot_restore_action(&replacement),
        )
        .unwrap_err();
        assert!(error.contains("identity differs"), "{error}");
    }

    #[test]
    fn staged_target_active_pilot_semantics_reject_corrupt_extras_and_keep_legacy_hidden_skip() {
        let floor = crate::db::Database::open(":memory:").unwrap();
        floor.initialize().unwrap();
        let policy = test_pilot_policy(0, "ReviewerA", "ReviewerB");
        let digest = policy.policy_sha256().unwrap();
        let action = pilot_restore_action(&policy);
        let validate =
            |target: &crate::db::Database| require_active_pilot_policy_binding(&floor, None, target, &action);

        let legacy_skip = crate::db::Database::open(":memory:").unwrap();
        legacy_skip.initialize().unwrap();
        legacy_skip
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'legacy-hidden')", [&digest])
            .unwrap();
        insert_test_review_event(&legacy_skip, "legacy-hidden", "ReviewerA", "skip", "couch", 1);
        validate(&legacy_skip).unwrap();

        let valid_hidden = crate::db::Database::open(":memory:").unwrap();
        valid_hidden.initialize().unwrap();
        valid_hidden
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-ok')", [&digest])
            .unwrap();
        insert_test_review_event(&valid_hidden, "hidden-ok", "ReviewerA", "edit", "couch_spot_check", 1);
        insert_test_spot_result(&valid_hidden, "hidden-ok", "reviewera", "edit");
        validate(&valid_hidden).unwrap();

        let unauthorized = crate::db::Database::open(":memory:").unwrap();
        unauthorized.initialize().unwrap();
        insert_test_review_event(&unauthorized, "work", "Intruder", "skip", "couch", 1);
        let error = validate(&unauthorized).unwrap_err();
        assert!(error.contains("unauthorized reviewer"), "{error}");

        let invalid_action = crate::db::Database::open(":memory:").unwrap();
        invalid_action.initialize().unwrap();
        insert_test_review_event(&invalid_action, "work", "ReviewerA", "approve", "couch", 1);
        let error = validate(&invalid_action).unwrap_err();
        assert!(error.contains("invalid action"), "{error}");

        let ungranted_hidden = crate::db::Database::open(":memory:").unwrap();
        ungranted_hidden.initialize().unwrap();
        insert_test_review_event(&ungranted_hidden, "not-granted", "ReviewerA", "edit", "couch_spot_check", 1);
        let error = validate(&ungranted_hidden).unwrap_err();
        assert!(error.contains("no active durable grant"), "{error}");

        let corpus_finalized_hidden = crate::db::Database::open(":memory:").unwrap();
        corpus_finalized_hidden.initialize().unwrap();
        corpus_finalized_hidden
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-corpus')", [&digest])
            .unwrap();
        insert_test_review_event(&corpus_finalized_hidden, "hidden-corpus", "ReviewerA", "accept", "couch", 1);
        let error = validate(&corpus_finalized_hidden).unwrap_err();
        assert!(error.contains("non-skip finalized"), "{error}");

        let duplicate_resolution = crate::db::Database::open(":memory:").unwrap();
        duplicate_resolution.initialize().unwrap();
        duplicate_resolution
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-duplicate')", [&digest])
            .unwrap();
        insert_test_review_event(&duplicate_resolution, "hidden-duplicate", "ReviewerA", "edit", "couch_spot_check", 1);
        insert_test_review_event(&duplicate_resolution, "hidden-duplicate", "ReviewerA", "edit", "couch_spot_check", 2);
        insert_test_spot_result(&duplicate_resolution, "hidden-duplicate", "ReviewerA", "edit");
        let error = validate(&duplicate_resolution).unwrap_err();
        assert!(error.contains("resolved more than once"), "{error}");

        let mismatched_result = crate::db::Database::open(":memory:").unwrap();
        mismatched_result.initialize().unwrap();
        mismatched_result
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-mismatch')", [&digest])
            .unwrap();
        insert_test_review_event(&mismatched_result, "hidden-mismatch", "ReviewerA", "edit", "couch_spot_check", 1);
        insert_test_spot_result(&mismatched_result, "hidden-mismatch", "ReviewerA", "accept");
        let error = validate(&mismatched_result).unwrap_err();
        assert!(error.contains("event/result actions"), "{error}");

        let impossible_score = crate::db::Database::open(":memory:").unwrap();
        impossible_score.initialize().unwrap();
        impossible_score
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-impossible')", [&digest])
            .unwrap();
        insert_test_review_event(&impossible_score, "hidden-impossible", "ReviewerA", "edit", "couch_spot_check", 1);
        insert_test_spot_result(&impossible_score, "hidden-impossible", "ReviewerA", "edit");
        impossible_score
            .connection()
            .execute(
                "UPDATE spot_checks SET submitted_transcript = 'different', noticed = 1, cer = 0.0
                  WHERE segment_id = 'hidden-impossible'",
                [],
            )
            .unwrap();
        let error = validate(&impossible_score).unwrap_err();
        assert!(error.contains("impossible noticed/CER"), "{error}");

        let orphan_result = crate::db::Database::open(":memory:").unwrap();
        orphan_result.initialize().unwrap();
        orphan_result
            .connection()
            .execute("INSERT INTO review_pilot_hidden_keys VALUES (?1, 0, 'ReviewerA', 'hidden-orphan')", [&digest])
            .unwrap();
        insert_test_spot_result(&orphan_result, "hidden-orphan", "ReviewerA", "edit");
        let error = validate(&orphan_result).unwrap_err();
        assert!(error.contains("orphan hidden-check result"), "{error}");

        let over_corpus_cap = crate::db::Database::open(":memory:").unwrap();
        over_corpus_cap.initialize().unwrap();
        for index in 0..=crate::review_pilot::REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER {
            insert_test_review_event(&over_corpus_cap, &format!("work-{index}"), "ReviewerA", "skip", "couch", index);
        }
        let error = validate(&over_corpus_cap).unwrap_err();
        assert!(error.contains("per-reviewer corpus-action ceiling"), "{error}");

        let half_written = crate::db::Database::open(":memory:").unwrap();
        half_written.initialize().unwrap();
        half_written.insert_segment(&test_segment("half-written", "half.wav", "draft")).unwrap();
        half_written
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET human_decision = 'accept', reviewed_by = 'ReviewerA', verified = 1
                  WHERE id = 'half-written'",
                [],
            )
            .unwrap();
        let error = validate(&half_written).unwrap_err();
        assert!(error.contains("no matching active campaign event/ledger"), "{error}");

        let missing_current_state = crate::db::Database::open(":memory:").unwrap();
        missing_current_state.initialize().unwrap();
        insert_canonical_pay_segment(&missing_current_state, "event-without-state");
        canonical_phone_playback(&missing_current_state, "event-without-state", "ReviewerA");
        let revision = missing_current_state.segment_review_revision("event-without-state").unwrap().unwrap();
        missing_current_state
            .record_phone_human_decision_by_at_revision_with_operation(
                "event-without-state",
                "reject",
                None,
                "ReviewerA",
                revision,
                &canonical_operation(20),
                &crate::db::review_operation_payload_hash("event-without-state", "reject", "", "ReviewerA"),
            )
            .unwrap()
            .unwrap();
        // Model a self-consistent staged file whose event+ledger survived but whose same-revision
        // corpus state did not. Recreate the exact trigger so only semantics—not schema drift—refuse.
        missing_current_state.connection().execute("DROP TRIGGER speech_segments_review_revision", []).unwrap();
        missing_current_state
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET human_decision = NULL, reviewed_by = NULL, verified = 0
                  WHERE id = 'event-without-state'",
                [],
            )
            .unwrap();
        missing_current_state
            .connection()
            .execute_batch(
                "CREATE TRIGGER speech_segments_review_revision
                 AFTER UPDATE ON speech_segments
                 WHEN new.review_revision = old.review_revision
                 BEGIN
                     UPDATE speech_segments
                     SET review_revision = old.review_revision + 1
                     WHERE id = old.id;
                 END;",
            )
            .unwrap();
        let error = validate(&missing_current_state).unwrap_err();
        assert!(error.contains("no matching current-revision segment state"), "{error}");

        let pre_pilot = crate::db::Database::open(":memory:").unwrap();
        pre_pilot.initialize().unwrap();
        insert_canonical_pay_segment(&pre_pilot, "pre-pilot-state");
        canonical_phone_playback(&pre_pilot, "pre-pilot-state", "ReviewerA");
        let revision = pre_pilot.segment_review_revision("pre-pilot-state").unwrap().unwrap();
        pre_pilot
            .record_phone_human_decision_by_at_revision_with_operation(
                "pre-pilot-state",
                "reject",
                None,
                "ReviewerA",
                revision,
                &canonical_operation(21),
                &crate::db::review_operation_payload_hash("pre-pilot-state", "reject", "", "ReviewerA"),
            )
            .unwrap()
            .unwrap();
        let pre_pilot_policy = test_pilot_policy(1, "ReviewerA", "ReviewerB");
        validate_active_pilot_semantics(&pre_pilot, &pre_pilot_policy, "pre-pilot regression")
            .expect("an exact same-reviewer decision at the baseline is legitimate prior state");

        let forged_after_undo = crate::db::Database::open(":memory:").unwrap();
        forged_after_undo.initialize().unwrap();
        insert_canonical_pay_segment(&forged_after_undo, "forged-after-undo");
        canonical_phone_playback(&forged_after_undo, "forged-after-undo", "ReviewerA");
        let revision = forged_after_undo.segment_review_revision("forged-after-undo").unwrap().unwrap();
        let operation = canonical_operation(22);
        forged_after_undo
            .record_phone_human_decision_by_at_revision_with_operation(
                "forged-after-undo",
                "reject",
                None,
                "ReviewerA",
                revision,
                &operation,
                &crate::db::review_operation_payload_hash("forged-after-undo", "reject", "", "ReviewerA"),
            )
            .unwrap()
            .unwrap();
        let effect_id = forged_after_undo.human_decision_effect_for_operation(&operation).unwrap().unwrap().0;
        assert!(matches!(
            forged_after_undo.undo_human_decision(effect_id, Some("ReviewerA"), &operation).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        forged_after_undo.connection().execute("DROP TRIGGER speech_segments_review_revision", []).unwrap();
        forged_after_undo
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET human_decision = 'accept', reviewed_by = 'ReviewerA', verified = 1
                  WHERE id = 'forged-after-undo'",
                [],
            )
            .unwrap();
        forged_after_undo
            .connection()
            .execute_batch(
                "CREATE TRIGGER speech_segments_review_revision
                 AFTER UPDATE ON speech_segments
                 WHEN new.review_revision = old.review_revision
                 BEGIN
                     UPDATE speech_segments
                     SET review_revision = old.review_revision + 1
                     WHERE id = old.id;
                 END;",
            )
            .unwrap();
        let error = validate(&forged_after_undo).unwrap_err();
        assert!(error.contains("no matching active campaign event/ledger"), "{error}");
    }

    #[test]
    fn restore_admission_is_exclusive_and_releases_waiters_after_error_and_panic() {
        let admission = RestoreAdmission::new();

        let first = admission.try_reserve().expect("first restore owns the reservation");
        assert!(admission.try_reserve().is_err(), "overlapping restores must fail instead of sharing a flag");
        drop(first);
        assert!(!admission.is_pending());

        let failed: Result<(), &str> = {
            let _reservation = admission.try_reserve().expect("reservation for failing restore");
            Err("injected restore error")
        };
        assert_eq!(failed, Err("injected restore error"));
        assert!(!admission.is_pending(), "an error path must release the restore reservation");

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _reservation = admission.try_reserve().expect("reservation for panicking restore");
            panic!("injected restore panic");
        }));
        assert!(panicked.is_err());
        assert!(!admission.is_pending(), "an unwind must release the restore reservation");

        let capture = admission.begin_capture().expect("snapshot capture enters while no restore is pending");
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let admission_ref = &admission;
            scope.spawn(move || {
                let _ = started_tx.send(());
                let reservation = admission_ref.try_reserve().expect("restore waits for active capture to drain");
                let _ = acquired_tx.send(());
                let _ = release_rx.recv();
                drop(reservation);
            });
            started_rx.recv_timeout(std::time::Duration::from_secs(2)).expect("restore waiter thread started");
            let pending_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !admission.is_pending() && std::time::Instant::now() < pending_deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(admission.is_pending(), "restore must publish pending before waiting for the capture");
            assert!(admission.begin_capture().is_err(), "no new snapshot may cross a pending restore generation");
            assert!(
                acquired_rx.recv_timeout(std::time::Duration::from_millis(50)).is_err(),
                "restore publication must wait until the complete capture token drops"
            );
            drop(capture);
            acquired_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("restore proceeds after the snapshot capture drains");
            release_tx.send(()).unwrap();
        });
        assert!(!admission.is_pending());

        let database = std::sync::Mutex::new(());
        let reservation = admission.try_reserve().expect("reservation that fences a queued writer");
        std::thread::scope(|scope| {
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let admission_ref = &admission;
            let database_ref = &database;
            scope.spawn(move || {
                let _db = admission_ref.lock(database_ref).unwrap_or_else(|poisoned| poisoned.into_inner());
                entered_tx.send(()).unwrap();
            });
            assert!(
                entered_rx.recv_timeout(std::time::Duration::from_millis(50)).is_err(),
                "an ordinary AppState DB caller must remain fenced through the complete restore"
            );
            // Models the point after DB pages are restored but before history/config/pipeline work ends.
            // The waiter must still be blocked until the outer command drops its reservation.
            assert!(admission.is_pending());
            drop(reservation);
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("the queued caller resumes after the complete restore");
        });
    }

    #[test]
    fn armed_restore_parks_on_error_and_exact_recovery_is_the_only_reentry() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().expect("new restore reservation");
        reservation.arm_named_restore().expect("durable transaction arm");
        drop(reservation); // models any error/unwind after the marker commit boundary

        assert!(admission.is_pending(), "an armed error must keep ordinary DB/config work fenced");
        assert!(admission.begin_mutation().is_err(), "no mutation may start in recovery-required state");
        assert!(admission.try_reserve().is_err(), "an unrelated restore cannot claim a parked transaction");

        let recovery = admission.claim_recovery().expect("the exact recovery path reclaims parked admission");
        recovery.commit_named_restore().expect("coherent generation commit releases the fence");
        let next = admission.try_reserve().expect("normal restore admission resumes after recovery commit");
        drop(recovery); // a stale generation token must not clear the newer reservation
        assert!(admission.is_pending(), "stale guard drop must not release a newer restore generation");
        drop(next);
        assert!(!admission.is_pending());
    }

    #[test]
    fn full_operation_mutation_and_restore_admission_are_race_closed() {
        let admission = RestoreAdmission::new();
        let mutation = admission.begin_mutation().expect("mutation starts while idle");
        assert!(admission.try_reserve().is_err(), "restore must refuse an already-running long mutation");
        assert!(!admission.is_pending(), "a refused new restore must not strand the admission flag");
        drop(mutation);

        let restore = admission.try_reserve().expect("restore starts after mutation ends");
        assert!(admission.begin_mutation().is_err(), "new long mutation must refuse a published restore");
        drop(restore);
        assert!(admission.begin_mutation().is_ok(), "mutations resume after an unarmed restore ends");
    }

    #[test]
    fn restore_helper_pins_the_old_live_db_before_swapping_to_the_source() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().expect("local restore reservation");
        let temp = tempfile::TempDir::new().unwrap();
        let live_path = temp.path().join("live.db");
        let source_path = temp.path().join("source.db");
        let data_dir = temp.path().join("app-data");
        std::fs::create_dir_all(&data_dir).unwrap();

        let live = crate::db::Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("old-live", "old.wav", "must be recoverable")).unwrap();

        // Derive the source from live so immutable compensation-policy provenance (including its
        // database-owned timestamp) is truly the same generation; independent initialize() calls can
        // straddle a one-second clock boundary and are not a legitimate restore-superset fixture.
        live.backup(&source_path).unwrap();
        let source = crate::db::Database::open(source_path.to_string_lossy().as_ref()).unwrap();
        source.delete_segment("old-live").unwrap();
        source.insert_segment(&test_segment("restored", "restored.wav", "from snapshot")).unwrap();
        // Finish this fixture like snapshot promotion does. Restore staging is WAL-aware, but the
        // raw file bytes below model a frozen manifest-bound backup rather than an active writer.
        source.wal_checkpoint().unwrap();
        drop(source);

        let shared = std::sync::Mutex::new(live);
        let pinned = {
            // This is the exact production ownership shape: one DB mutex guard is held while the
            // helper first snapshots and then restores. Rust cannot release/reacquire it in between.
            let mut guard = shared.lock().unwrap();
            restore_with_mandatory_snapshot(&reservation, &mut guard, &data_dir, &source_path).unwrap()
        };

        let restored = shared.lock().unwrap();
        assert!(restored.get_segment_by_id("restored").unwrap().is_some());
        assert!(restored.get_segment_by_id("old-live").unwrap().is_none());

        assert!(
            crate::snapshot::verify_snapshot_manifest_for_restore(&pinned).unwrap(),
            "the mandatory pre-restore pin must be self-verifying"
        );
        let pinned_db = pinned.join("cortex-speech.db");
        let read_only = rusqlite::Connection::open_with_flags(
            pinned_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        let old_rows: i64 = read_only
            .query_row("SELECT COUNT(*) FROM speech_segments WHERE id = 'old-live'", [], |row| row.get(0))
            .unwrap();
        let new_rows: i64 = read_only
            .query_row("SELECT COUNT(*) FROM speech_segments WHERE id = 'restored'", [], |row| row.get(0))
            .unwrap();
        assert_eq!((old_rows, new_rows), (1, 0), "the mandatory pin must contain the exact pre-restore library");
        // A WAL-mode read-only connection may keep transient -shm/-wal siblings open. Production
        // staging closes its source before the final manifest re-verification; model that boundary.
        drop(read_only);
    }

    #[test]
    fn bare_restore_refuses_durable_review_activity_before_pin_or_swap() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("app-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let live_path = temp.path().join("live.db");
        let source_path = temp.path().join("source.db");
        let mut live = crate::db::Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "reviewed");
        live.backup(&source_path).unwrap();
        live.record_review_event("reviewed", "Reviewer", "skip", "test", 1).unwrap();

        let error = restore_with_mandatory_snapshot(&reservation, &mut live, &data_dir, &source_path).unwrap_err();
        assert!(error.contains("Bare database restore is refused"), "{error}");
        let events: i64 =
            live.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(events, 1, "the live audit row must remain in place");
        assert!(!data_dir.join("snapshots").join("pinned").exists(), "refusal must happen before safety-pin I/O");
    }

    #[test]
    fn bare_restore_refuses_target_only_durable_review_activity_before_pin_or_swap() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("app-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let live_path = temp.path().join("live.db");
        let source_path = temp.path().join("source.db");
        let mut live = crate::db::Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "target-reviewed");
        live.backup(&source_path).unwrap();
        let target = crate::db::Database::open(source_path.to_string_lossy().as_ref()).unwrap();
        target.record_review_event("target-reviewed", "Reviewer", "skip", "test", 1).unwrap();
        target.wal_checkpoint().unwrap();
        drop(target);

        let error = restore_with_mandatory_snapshot(&reservation, &mut live, &data_dir, &source_path).unwrap_err();
        assert!(error.contains("either the live or target generation"), "{error}");
        let live_events: i64 =
            live.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(live_events, 0, "target-only audit history must not be imported through the bare path");
        assert!(!data_dir.join("snapshots").join("pinned").exists());
    }

    #[test]
    fn bare_restore_refuses_snapshot_that_predates_a_legacy_path_withdrawal_before_pin_or_swap() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("app-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let live_path = temp.path().join("live.db");
        let source_path = temp.path().join("before-withdrawal.db");
        let mut live = crate::db::Database::open(live_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("legacy-withdrawal", "legacy-withdrawal.wav", "private recording")).unwrap();
        assert!(live.segment_audio_content_hash("legacy-withdrawal").unwrap().is_none());
        live.backup(&source_path).unwrap();
        assert_eq!(live.revoke_recording("legacy-withdrawal.wav").unwrap(), 1);

        let error = restore_with_mandatory_snapshot(&reservation, &mut live, &data_dir, &source_path).unwrap_err();
        assert!(error.contains("resurrect 1 withdrawn recording"), "{error}");
        assert!(live.rights_for_segment("legacy-withdrawal").unwrap().is_revoked());
        assert!(
            !data_dir.join("snapshots").join("pinned").exists(),
            "consent regression must be refused before safety-pin I/O or page publication"
        );
    }

    #[test]
    fn named_restore_rejects_missing_durable_rows_before_pin_marker_or_swap() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        let mut live = crate::db::Database::open(":memory:").unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "reviewed");
        let source_dir = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 1000).unwrap().unwrap();
        let source = source_dir.join("cortex-speech.db");
        live.record_review_event("reviewed", "Reviewer", "skip", "test", 1).unwrap();

        let error = prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &source_dir,
            &source,
            "snapshot_0000001000",
        )
        .unwrap_err();
        assert!(error.contains("review_events"), "{error}");
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let pins = data_dir.join("snapshots").join("pinned");
        assert!(!pins.exists() || std::fs::read_dir(pins).unwrap().next().is_none());
        let events: i64 =
            live.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(events, 1, "the live generation must not be swapped on floor regression");
    }

    #[test]
    fn named_restore_cannot_roll_a_reviewed_recording_back_across_consent_withdrawal() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        let mut live = crate::db::Database::open(":memory:").unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "reviewed-withdrawal");
        live.finalize_human_review("reviewed-withdrawal", "accept", Some("machine draft"), Some(1), None).unwrap();
        // Fork the target before withdrawal. Give that old generation one unrelated metadata update
        // after the fork so its review revision equals the withdrawal-updated live row. This defeats
        // an accidental dependency on review_revision and proves the dedicated consent floor is what
        // refuses the otherwise review-history-equivalent target.
        let target = copied_database(&live);
        assert_eq!(live.revoke_recording("reviewed-withdrawal.wav").unwrap(), 1);
        target
            .connection()
            .execute("UPDATE speech_segments SET speaker_id = 'metadata-only' WHERE id = 'reviewed-withdrawal'", [])
            .unwrap();
        assert_eq!(
            live.segment_review_revision("reviewed-withdrawal").unwrap(),
            target.segment_review_revision("reviewed-withdrawal").unwrap(),
            "the adversarial target must pass the reviewed-row revision projection"
        );
        let source_dir = crate::snapshot::take_snapshot_at(&target, data_dir, 5, 1_500).unwrap().unwrap();
        let source = source_dir.join("cortex-speech.db");
        let selector = snapshot_selector(&source_dir, false);

        let error =
            prepare_and_restore_named_transaction(&reservation, &mut live, data_dir, &source_dir, &source, &selector)
                .unwrap_err();
        assert!(error.contains("resurrect 1 withdrawn recording"), "{error}");
        assert!(live.rights_for_segment("reviewed-withdrawal").unwrap().is_revoked());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let pins = data_dir.join("snapshots").join("pinned");
        assert!(
            !pins.exists() || std::fs::read_dir(pins).unwrap().next().is_none(),
            "withdrawal regression must be refused before a restore marker or page swap"
        );
    }

    #[test]
    fn named_restore_rejects_target_only_half_written_pilot_state_before_pin_or_marker() {
        let admission = RestoreAdmission::new();
        let reservation = admission.try_reserve().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        let mut live = crate::db::Database::open(":memory:").unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("half-target", "half.wav", "draft")).unwrap();

        let target = copied_database(&live);
        target
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET human_decision = 'accept', reviewed_by = 'Hawzhin', verified = 1
                  WHERE id = 'half-target'",
                [],
            )
            .unwrap();
        // Snapshot creation must keep enforcing the real controlled-pilot authority contract. Use
        // its exact roster so this fixture passes snapshot admission and reaches the independent
        // named-restore semantic gate for the deliberately half-written corpus row below.
        let policy = test_pilot_policy(0, "Hawzhin", "Pavel");
        crate::review_pilot::install_test_focus(data_dir, ["half-target"]);
        std::fs::write(
            data_dir.join(crate::review_pilot::REVIEW_PILOT_FILE),
            serde_json::to_vec_pretty(&policy).unwrap(),
        )
        .unwrap();
        let snapshot = crate::snapshot::take_snapshot_at(&target, data_dir, 5, 6000).unwrap().unwrap();
        let source = snapshot.join("cortex-speech.db");
        let selector = snapshot.file_name().unwrap().to_string_lossy().to_string();

        let error =
            prepare_and_restore_named_transaction(&reservation, &mut live, data_dir, &snapshot, &source, &selector)
                .unwrap_err();
        assert!(error.contains("no matching active campaign event/ledger"), "{error}");
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let pins = data_dir.join("snapshots").join("pinned");
        assert!(!pins.exists() || std::fs::read_dir(pins).unwrap().next().is_none());
        let unchanged = live.get_segment_by_id("half-target").unwrap().unwrap();
        assert!(unchanged.human_decision.is_none() && unchanged.reviewed_by.is_none());
    }

    #[test]
    fn named_restore_reuses_original_transaction_pin_and_bad_source_creates_no_barrier() {
        let admission = RestoreAdmission::new();
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path();
        let mut live = crate::db::Database::open(":memory:").unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("before-source", "before.wav", "source generation")).unwrap();
        let source_dir = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 1000).unwrap().unwrap();
        let source = source_dir.join("cortex-speech.db");
        live.insert_segment(&test_segment("pre-restore-only", "later.wav", "must remain in original pin")).unwrap();

        let reservation = admission.try_reserve().unwrap();
        let first = prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &source_dir,
            &source,
            "snapshot_0000001000",
        )
        .unwrap();
        assert_eq!(first.optional.len(), crate::snapshot::OPTIONAL_SNAPSHOT_STATE.len());
        let pending = load_named_restore_pending(data_dir).unwrap().unwrap();
        let original_pin = crate::snapshot::resolve_snapshot_dir(data_dir, &pending.pre_restore_pin_selector).unwrap();
        let pin_db = rusqlite::Connection::open_with_flags(
            original_pin.join("cortex-speech.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        let original_only: i64 = pin_db
            .query_row("SELECT COUNT(*) FROM speech_segments WHERE id='pre-restore-only'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(original_only, 1, "the transaction pin is the exact generation before the first page swap");

        prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &source_dir,
            &source,
            "snapshot_0000001000",
        )
        .unwrap();
        let pins = std::fs::read_dir(data_dir.join("snapshots").join("pinned"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("prerestore_"))
            .count();
        assert_eq!(pins, 1, "a retry must reuse, never rotate out, the original pre-restore generation");
        assert_eq!(load_named_restore_pending(data_dir).unwrap().unwrap(), pending);
        clear_review_pilot_restore_pending(data_dir).unwrap();
        reservation.commit_named_restore().unwrap();
        drop(reservation);

        let broken_dir = data_dir.join("snapshots").join("snapshot_0000002000");
        std::fs::create_dir_all(&broken_dir).unwrap();
        std::fs::write(broken_dir.join("cortex-speech.db"), b"not sqlite").unwrap();
        let reservation = admission.try_reserve().unwrap();
        let error = prepare_and_restore_named_transaction(
            &reservation,
            &mut live,
            data_dir,
            &broken_dir,
            &broken_dir.join("cortex-speech.db"),
            "snapshot_0000002000",
        )
        .unwrap_err();
        assert!(!error.is_empty());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let pins_after = std::fs::read_dir(data_dir.join("snapshots").join("pinned"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("prerestore_"))
            .count();
        assert_eq!(pins_after, 1, "invalid source validation happens before any new pin or barrier");
        drop(reservation);
    }

    fn snapshot_selector(path: &Path, pinned: bool) -> String {
        let name = path.file_name().and_then(|name| name.to_str()).expect("snapshot directory name");
        if pinned {
            format!("pinned/{name}")
        } else {
            name.to_string()
        }
    }

    #[test]
    fn interrupted_recovery_uses_original_pin_floor_not_possibly_swapped_live() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let mut live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        insert_canonical_pay_segment(&live, "original");
        live.insert_segment(&test_segment("target-only", "target.wav", "target generation")).unwrap();
        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 2000).unwrap().unwrap();

        live.delete_segment("target-only").unwrap();
        record_canonical_skip(&live, "original", "Reviewer", 30);
        let original_pin = crate::snapshot::take_pinned_snapshot_at(&live, data_dir, "history-floor", 3, 3000).unwrap();

        // Model a crash after target page publication: live now lacks the original floor's audit/pay
        // rows. A broken implementation that compares against live would accept target again.
        let staged_target = crate::db::Database::stage_restore_source(target.join("cortex-speech.db")).unwrap();
        live.commit_staged_restore(&staged_target).unwrap();
        drop(staged_target);
        drop(live);

        let pending = NamedRestorePending {
            schema: 2,
            source_selector: snapshot_selector(&target, false),
            pre_restore_pin_selector: snapshot_selector(&original_pin, true),
            target_db_generation_sha256: None,
            completed_selector: None,
            completed_db_generation_sha256: None,
        };
        write_named_restore_pending(data_dir, &pending).unwrap();
        let admission = RestoreAdmission::new();
        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());

        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        let events: i64 = recovered
            .connection()
            .query_row("SELECT COUNT(*) FROM review_events WHERE segment_id = 'original'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(events, 1, "fallback must restore the original pin's durable review floor");
        assert!(recovered.get_segment_by_id("original").unwrap().is_some());
        assert!(
            recovered.get_segment_by_id("target-only").unwrap().is_none(),
            "the regressive target must be rejected and the original floor published"
        );
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
    }

    #[test]
    fn interrupted_recovery_rejects_a_pre_withdrawal_target_and_restores_the_revoked_floor() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let mut live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("interrupted-withdrawal", "interrupted-withdrawal.wav", "private recording"))
            .unwrap();
        let content_hash = "d".repeat(64);
        live.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash = ?1 WHERE id = 'interrupted-withdrawal'",
                [&content_hash],
            )
            .unwrap();
        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 2_500).unwrap().unwrap();

        assert_eq!(live.revoke_recording("interrupted-withdrawal.wav").unwrap(), 1);
        let original_pin =
            crate::snapshot::take_pinned_snapshot_at(&live, data_dir, "withdrawal-floor", 3, 2_600).unwrap();

        // Model a process death after the old target pages were swapped but before required state and
        // the durable marker completed.  Startup recovery must compare against the original pin, not
        // trust the now-unrevoked live pages.
        let staged_target = crate::db::Database::stage_restore_source(target.join("cortex-speech.db")).unwrap();
        live.commit_staged_restore(&staged_target).unwrap();
        assert!(!live.rights_for_segment("interrupted-withdrawal").unwrap().is_revoked());
        drop(staged_target);
        drop(live);

        let pending = NamedRestorePending {
            schema: 2,
            source_selector: snapshot_selector(&target, false),
            pre_restore_pin_selector: snapshot_selector(&original_pin, true),
            target_db_generation_sha256: None,
            completed_selector: None,
            completed_db_generation_sha256: None,
        };
        write_named_restore_pending(data_dir, &pending).unwrap();
        let admission = RestoreAdmission::new();
        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());

        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        assert!(
            recovered.rights_for_segment("interrupted-withdrawal").unwrap().is_revoked(),
            "startup fallback must republish the original withdrawal authority"
        );
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
    }

    #[test]
    fn startup_recovery_completes_the_recorded_target_before_any_normal_work() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("original", "original.wav", "original generation")).unwrap();
        let original_pin =
            crate::snapshot::take_pinned_snapshot_at(&live, data_dir, "startup-original", 3, 1000).unwrap();
        live.insert_segment(&test_segment("target", "target.wav", "target generation")).unwrap();
        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 2000).unwrap().unwrap();
        live.delete_segment("target").unwrap(); // prove recovery publishes the target snapshot, not current pages
        drop(live);

        let pending = NamedRestorePending {
            schema: 2,
            source_selector: snapshot_selector(&target, false),
            pre_restore_pin_selector: snapshot_selector(&original_pin, true),
            target_db_generation_sha256: None,
            completed_selector: None,
            completed_db_generation_sha256: None,
        };
        write_named_restore_pending(data_dir, &pending).unwrap();
        let admission = RestoreAdmission::new();

        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());
        assert!(!admission.is_pending());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        assert!(recovered.get_segment_by_id("original").unwrap().is_some());
        assert!(recovered.get_segment_by_id("target").unwrap().is_some());
    }

    #[test]
    fn startup_recovery_rolls_back_the_verified_full_original_when_target_preflight_fails() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("original", "original.wav", "original generation")).unwrap();
        let original_pin =
            crate::snapshot::take_pinned_snapshot_at(&live, data_dir, "startup-rollback", 3, 3000).unwrap();
        live.insert_segment(&test_segment("target", "target.wav", "must be rolled back")).unwrap();
        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 4000).unwrap().unwrap();
        drop(live);
        std::fs::write(target.join("settings.json"), b"tampered after manifest").unwrap();

        let pending = NamedRestorePending {
            schema: 2,
            source_selector: snapshot_selector(&target, false),
            pre_restore_pin_selector: snapshot_selector(&original_pin, true),
            target_db_generation_sha256: None,
            completed_selector: None,
            completed_db_generation_sha256: None,
        };
        write_named_restore_pending(data_dir, &pending).unwrap();
        let admission = RestoreAdmission::new();

        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());
        assert!(!admission.is_pending());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        assert!(recovered.get_segment_by_id("original").unwrap().is_some());
        assert!(recovered.get_segment_by_id("target").unwrap().is_none());
    }

    #[test]
    fn exact_completed_restore_generation_cleans_up_after_a_lost_response_without_replay() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("committed", "committed.wav", "already coherent")).unwrap();
        let committed_digest = live.restore_generation_sha256().unwrap();
        drop(live);
        let pending = NamedRestorePending {
            schema: NAMED_RESTORE_PENDING_SCHEMA,
            source_selector: "snapshot_0000009999".to_string(),
            pre_restore_pin_selector: "pinned/missing_0000009998".to_string(),
            target_db_generation_sha256: Some(committed_digest.clone()),
            completed_selector: Some("snapshot_0000009999".to_string()),
            completed_db_generation_sha256: Some(committed_digest),
        };
        write_named_restore_pending(data_dir, &pending).unwrap();
        let admission = RestoreAdmission::new();

        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());
        assert!(!admission.is_pending());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let recovered = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        assert!(recovered.get_segment_by_id("committed").unwrap().is_some());
    }

    #[test]
    fn completed_marker_digest_mismatch_replays_the_exact_recorded_target() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();
        AppSettings::default().save(&data_dir.join("settings.json")).unwrap();
        let db_path = data_dir.join("cortex-speech.db");
        let live = crate::db::Database::open(db_path.to_string_lossy().as_ref()).unwrap();
        live.initialize().unwrap();
        live.insert_segment(&test_segment("original", "original.wav", "original authority")).unwrap();
        let original_pin = crate::snapshot::take_pinned_snapshot_at(&live, data_dir, "digest-floor", 3, 8_000).unwrap();
        live.insert_segment(&test_segment("target", "target.wav", "recorded target authority")).unwrap();
        let target = crate::snapshot::take_snapshot_at(&live, data_dir, 5, 9_000).unwrap().unwrap();
        let target_selector = snapshot_selector(&target, false);
        let target_staged = crate::db::Database::stage_restore_source(target.join("cortex-speech.db")).unwrap();
        let target_digest = target_staged.restore_generation_sha256().unwrap();
        drop(target_staged);

        // Model a healthy but wrong generation under a durable completion marker. The old health-only
        // cleanup accepted this state and erased the barrier. Exact-generation recovery must replay.
        live.delete_segment("target").unwrap();
        live.insert_segment(&test_segment("wrong", "wrong.wav", "healthy but wrong")).unwrap();
        drop(live);
        let pending = NamedRestorePending {
            schema: NAMED_RESTORE_PENDING_SCHEMA,
            source_selector: target_selector.clone(),
            pre_restore_pin_selector: snapshot_selector(&original_pin, true),
            target_db_generation_sha256: Some(target_digest.clone()),
            completed_selector: Some(target_selector),
            completed_db_generation_sha256: Some(target_digest.clone()),
        };
        write_named_restore_pending(data_dir, &pending).unwrap();

        let admission = RestoreAdmission::new();
        assert!(recover_interrupted_named_restore_with_admission(data_dir, &admission).unwrap());
        assert!(!admission.is_pending());
        assert!(load_named_restore_pending(data_dir).unwrap().is_none());
        let recovered = crate::db::Database::open_detached_read_snapshot(db_path.to_string_lossy().as_ref()).unwrap();
        assert!(recovered.get_segment_by_id("original").unwrap().is_some());
        assert!(recovered.get_segment_by_id("target").unwrap().is_some());
        assert!(recovered.get_segment_by_id("wrong").unwrap().is_none());
        assert_eq!(recovered.restore_generation_sha256().unwrap(), target_digest);
    }

    #[test]
    fn named_restore_rehashes_after_plan_and_staging_and_never_mutates_invalid_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&test_segment("source", "source.wav", "source")).unwrap();
        let source_dir = crate::snapshot::take_snapshot_at(&db, temp.path(), 5, 1000).unwrap().unwrap();
        let source_db = source_dir.join("cortex-speech.db");
        let settings = source_dir.join("settings.json");
        let original = std::fs::read(&settings).unwrap();
        let error = prepare_named_restore_artifacts(&source_dir, &source_db, || {
            std::fs::write(&settings, b"tampered after plan capture").unwrap();
        })
        .err()
        .expect("post-capture mutation must be rejected");
        assert!(error.contains("mismatch"), "{error}");

        // Restore the exact source, then make malformed settings honestly match the manifest. This
        // reaches semantic preflight and proves it rejects without AppSettings::load renaming/mutating
        // the frozen source or touching a live DB.
        std::fs::write(&settings, &original).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source_dir.join(crate::snapshot::MANIFEST_FILE)).unwrap()).unwrap();
        let invalid = b"{}";
        std::fs::write(&settings, invalid).unwrap();
        let row =
            manifest["files"].as_array_mut().unwrap().iter_mut().find(|row| row["path"] == "settings.json").unwrap();
        row["sizeBytes"] = serde_json::json!(invalid.len());
        row["sha256"] = serde_json::json!(crate::models::compute_file_sha256(&settings).unwrap());
        std::fs::write(source_dir.join(crate::snapshot::MANIFEST_FILE), serde_json::to_vec_pretty(&manifest).unwrap())
            .unwrap();
        let semantic = prepare_named_restore_artifacts(&source_dir, &source_db, || {})
            .err()
            .expect("correctly hashed malformed config must fail semantic preflight");
        assert!(semantic.contains("settings.json is invalid"), "{semantic}");
        assert_eq!(std::fs::read(&settings).unwrap(), invalid);
        assert!(
            std::fs::read_dir(&source_dir)
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains("corrupt")),
            "strict recovery parsing must not rename or repair the frozen source"
        );
    }

    #[test]
    fn required_snapshot_routing_state_is_atomic_and_failure_keeps_review_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let live = temp.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
        let payloads = [
            ("champion.json", b"snapshot champion".as_slice()),
            ("reviewer_dialects.json", b"snapshot dialects".as_slice()),
            ("voice_focus.json", b"snapshot focus".as_slice()),
        ];
        let plan = crate::snapshot::OPTIONAL_SNAPSHOT_STATE
            .iter()
            .copied()
            .filter(|state| state.live_file != "settings.json")
            .map(|state| {
                let bytes = payloads.iter().find(|(name, _)| *name == state.live_file).unwrap().1.to_vec();
                (state, crate::snapshot::OptionalSnapshotRestore::Install(bytes))
            })
            .collect::<Vec<_>>();
        std::fs::write(live.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"pending").unwrap();

        // A non-file destination is an injected install failure. The helper must fail, and the
        // command-level commit marker remains authoritative because only the successful tail clears it.
        std::fs::create_dir(live.join("champion.json")).unwrap();
        let error = restore_required_snapshot_state_atomic(&plan, &live).unwrap_err();
        assert!(error.contains("champion.json") && error.contains("regular file or absent"), "{error}");
        assert!(live.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE).is_file());

        std::fs::remove_dir(live.join("champion.json")).unwrap();
        restore_required_snapshot_state_atomic(&plan, &live).unwrap();
        assert_eq!(std::fs::read(live.join("champion.json")).unwrap(), b"snapshot champion");
        assert_eq!(std::fs::read(live.join("reviewer_dialects.json")).unwrap(), b"snapshot dialects");
        assert_eq!(std::fs::read(live.join("voice_focus.json")).unwrap(), b"snapshot focus");
        assert!(live.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE).is_file());

        let stale = live.join("voice_focus.json.replace-bak-99999");
        std::fs::write(&stale, b"stale focus").unwrap();
        let absent = plan
            .iter()
            .map(|(state, _)| (*state, crate::snapshot::OptionalSnapshotRestore::ExplicitlyAbsent))
            .collect::<Vec<_>>();
        restore_required_snapshot_state_atomic(&absent, &live).unwrap();
        for (state, _) in &absent {
            assert!(!live.join(state.live_file).exists(), "{} must restore as explicit absence", state.live_file);
        }
        assert!(!stale.exists(), "explicit absence must not allow an atomic-recovery backup to resurrect old policy");
        assert!(live.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE).is_file());
    }

    #[test]
    fn snapshot_restore_preserves_live_champion_routing_and_gpu_supervision_choice() {
        let live = AppSettings {
            asr_model_size: AsrModelSize::WSL7B,
            use_finetuned_asr: false,
            multi_engine_hypotheses: false,
            external_asr_script_path: "C:/cortex/scripts/cortex_7b_client.py".to_string(),
            champion_supervision_enabled: false,
            ..AppSettings::default()
        };
        let mut restored = AppSettings {
            asr_model_size: AsrModelSize::CTC300M,
            use_finetuned_asr: true,
            multi_engine_hypotheses: true,
            external_asr_script_path: "old-or-missing-client.py".to_string(),
            champion_supervision_enabled: true,
            vad_threshold: 0.77,
            ..AppSettings::default()
        };

        preserve_live_asr_runtime_controls(&mut restored, &live);

        assert_eq!(restored.asr_model_size, AsrModelSize::WSL7B);
        assert!(!restored.use_finetuned_asr);
        assert!(!restored.multi_engine_hypotheses);
        assert_eq!(restored.external_asr_script_path, live.external_asr_script_path);
        assert!(!restored.champion_supervision_enabled, "a restore must not auto-load the 30 GB server");
        assert_eq!(restored.vad_threshold, 0.77, "ordinary snapshot configuration should still restore");
    }

    #[test]
    fn snapshot_pilot_policy_is_exact_atomic_and_fail_closed_during_restore() {
        let temp = tempfile::tempdir().unwrap();
        let snapshot_dir = temp.path().join("snapshot_1");
        let live_dir = temp.path().join("live");
        std::fs::create_dir_all(&snapshot_dir).unwrap();
        std::fs::create_dir_all(&live_dir).unwrap();
        let snapshot_db = snapshot_dir.join("cortex-speech.db");
        let source = crate::db::Database::open(snapshot_db.to_string_lossy().as_ref()).unwrap();
        source.initialize().unwrap();
        source.wal_checkpoint().unwrap();
        drop(source);
        let (_snapshot_authority, snapshot_schema, snapshot_max_review_event_id) =
            crate::db::Database::stage_restore_source_with_original_evidence(&snapshot_db).unwrap();

        let policy = br#"{
          "schema_version": 1,
          "after_review_event_id": 0,
          "max_total_corpus_actions": 20,
          "reviewers": [
            {"name": "Pavel", "max_corpus_actions": 10},
            {"name": "Hawzhin", "max_corpus_actions": 10}
          ]
        }"#;
        std::fs::write(snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_FILE), policy).unwrap();
        let legacy_policy =
            inspect_snapshot_pilot_policy(&snapshot_dir, snapshot_schema, snapshot_max_review_event_id, false)
                .unwrap_err();
        assert!(
            legacy_policy.contains("requires a verified manifest"),
            "a manifestless policy-bearing tree can never authorize a named restore: {legacy_policy}"
        );
        crate::review_pilot::install_test_focus(&snapshot_dir, ["snapshot-focus"]);
        std::fs::write(
            snapshot_dir.join(crate::voice_focus::VOICE_FOCUS_FILE),
            br#"{"segment_ids":["snapshot-wrong"]}"#,
        )
        .unwrap();
        let wrong_focus =
            inspect_snapshot_pilot_policy(&snapshot_dir, snapshot_schema, snapshot_max_review_event_id, true)
                .unwrap_err();
        assert!(wrong_focus.contains("digest mismatch"), "{wrong_focus}");
        crate::review_pilot::install_test_focus(&snapshot_dir, ["snapshot-focus"]);
        let migrated_legacy = inspect_snapshot_pilot_policy(&snapshot_dir, 58, 0, true).unwrap_err();
        assert!(
            migrated_legacy.contains("predates durable hidden-key authority"),
            "a fully staged migration must not upgrade the snapshot's original policy authority: {migrated_legacy}"
        );
        let install =
            inspect_snapshot_pilot_policy(&snapshot_dir, snapshot_schema, snapshot_max_review_event_id, true).unwrap();
        assert!(matches!(install, SnapshotPilotPolicyRestore::Install(_)));
        crate::review_pilot::install_test_focus(&live_dir, ["snapshot-focus"]);

        // Both representations are ambiguous and must fail before any DB swap.
        std::fs::write(
            snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE),
            crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES,
        )
        .unwrap();
        assert!(
            inspect_snapshot_pilot_policy(&snapshot_dir, snapshot_schema, snapshot_max_review_event_id, false).is_err()
        );
        std::fs::remove_file(snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE)).unwrap();

        std::fs::write(live_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"pending").unwrap();
        assert!(
            crate::review_pilot::load(&live_dir).unwrap_err().contains("restore did not finish"),
            "an interrupted cross-file restore must block paid review"
        );
        apply_snapshot_pilot_policy(&install, &live_dir).unwrap();
        assert!(
            crate::review_pilot::load(&live_dir).is_err(),
            "installing policy is not the commit point; the pending barrier remains authoritative"
        );
        clear_review_pilot_restore_pending(&live_dir).unwrap();
        assert_eq!(crate::review_pilot::load(&live_dir).unwrap().unwrap().reviewer_names(), vec!["Hawzhin", "Pavel"]);

        // Explicit absence removes an existing policy only under the same fail-closed marker.
        std::fs::remove_file(snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_FILE)).unwrap();
        std::fs::write(
            snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE),
            crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_BYTES,
        )
        .unwrap();
        let absent =
            inspect_snapshot_pilot_policy(&snapshot_dir, snapshot_schema, snapshot_max_review_event_id, false).unwrap();
        assert_eq!(absent, SnapshotPilotPolicyRestore::ExplicitlyAbsent);
        std::fs::write(live_dir.join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"pending").unwrap();
        let stale_policy_backup =
            live_dir.join(format!("{}.replace-bak-99999", crate::review_pilot::REVIEW_PILOT_FILE));
        std::fs::write(&stale_policy_backup, policy).unwrap();
        apply_snapshot_pilot_policy(&absent, &live_dir).unwrap();
        assert!(!stale_policy_backup.exists(), "explicit absence must remove recoverable stale policy bytes");
        assert!(crate::review_pilot::load(&live_dir).is_err());
        let stale_barrier_backup =
            live_dir.join(format!("{}.replace-bak-99999", crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE));
        std::fs::write(&stale_barrier_backup, b"stale pending").unwrap();
        clear_review_pilot_restore_pending(&live_dir).unwrap();
        assert!(!stale_barrier_backup.exists(), "a completed restore must not resurrect its barrier");
        assert_eq!(crate::review_pilot::load(&live_dir), Ok(None));

        // Legacy snapshots remain recoverable but can never delete or replace a current live policy.
        std::fs::remove_file(snapshot_dir.join(crate::review_pilot::REVIEW_PILOT_ABSENT_MARKER_FILE)).unwrap();
        assert!(
            inspect_snapshot_pilot_policy(&snapshot_dir, snapshot_schema, snapshot_max_review_event_id, true).is_err(),
            "manifest-bearing snapshots can never infer unrestricted state from missing files"
        );
        assert_eq!(
            inspect_snapshot_pilot_policy(&snapshot_dir, snapshot_schema, snapshot_max_review_event_id, false).unwrap(),
            SnapshotPilotPolicyRestore::PreserveLegacy
        );
    }

    #[test]
    fn bare_database_restore_refuses_active_or_uncertain_controlled_pilot_state() {
        let live = tempfile::tempdir().unwrap();
        assert!(refuse_bare_restore_during_controlled_pilot(live.path()).is_ok());

        let policy = br#"{
          "schema_version": 1,
          "after_review_event_id": 0,
          "max_total_corpus_actions": 20,
          "reviewers": [
            {"name": "Hawzhin", "max_corpus_actions": 10},
            {"name": "Pavel", "max_corpus_actions": 10}
          ]
        }"#;
        crate::review_pilot::install_test_focus(live.path(), ["live-focus"]);
        std::fs::write(live.path().join(crate::review_pilot::REVIEW_PILOT_FILE), policy).unwrap();
        let active = refuse_bare_restore_during_controlled_pilot(live.path()).unwrap_err();
        assert!(active.contains("policy-bearing named snapshot"), "{active}");

        std::fs::remove_file(live.path().join(crate::review_pilot::REVIEW_PILOT_FILE)).unwrap();
        let remembered = serde_json::json!({
            "reviewers": {},
            "db_path": "remembered.db",
            "pilot_policy": serde_json::from_slice::<serde_json::Value>(policy).unwrap(),
        });
        std::fs::write(live.path().join("couch_session.json"), serde_json::to_vec(&remembered).unwrap()).unwrap();
        let durable = refuse_bare_restore_during_controlled_pilot(live.path()).unwrap_err();
        assert!(durable.contains("durable Couch session"), "{durable}");
        std::fs::remove_file(live.path().join("couch_session.json")).unwrap();

        std::fs::write(live.path().join(crate::review_pilot::REVIEW_PILOT_RESTORE_PENDING_FILE), b"pending").unwrap();
        let uncertain = refuse_bare_restore_during_controlled_pilot(live.path()).unwrap_err();
        assert!(uncertain.contains("not provably safe"), "{uncertain}");
    }

    fn insert_hypothesis(
        db: &crate::db::Database,
        segment_id: &str,
        model_id: &str,
        transcript: &str,
        confidence: f64,
    ) {
        db.insert_hypothesis(&crate::db::SegmentHypothesis {
            segment_id: segment_id.to_string(),
            model_id: model_id.to_string(),
            transcript: transcript.to_string(),
            confidence: Some(confidence),
        })
        .unwrap();
    }

    fn insert_source_reference(db: &crate::db::Database, audio_path: &str, text: &str) {
        insert_source_reference_with_model(db, audio_path, "gemini-2.5-pro", text);
    }

    fn insert_source_reference_with_model(db: &crate::db::Database, audio_path: &str, model_id: &str, text: &str) {
        let identity = crate::pipeline::source_audio_identity(Path::new(audio_path)).ok();
        db.upsert_source_transcript(&crate::db::SourceTranscriptRecord {
            audio_path: audio_path.to_string(),
            model_id: model_id.to_string(),
            audio_content_hash: identity.as_ref().map(|identity| identity.content_hash.clone()),
            audio_size_bytes: identity.as_ref().map(|identity| identity.size_bytes),
            transcript_path: format!("{model_id}.source_reference.txt"),
            transcript_text: text.to_string(),
            created_at: None,
        })
        .unwrap();
    }

    fn test_source_audio(dir: &tempfile::TempDir, name: &str) -> String {
        let audio = dir.path().join(name);
        std::fs::write(&audio, format!("source-audio-bytes:{name}")).unwrap();
        audio.to_string_lossy().to_string()
    }

    /// A real, decodable silence WAV of `duration_ms` so `get_duration_ms` returns a true duration —
    /// required for the source-reference POSITIONAL window (round-24 hunt #15). A source-reference
    /// auto-commit needs the source duration AND the segment's offsets; production always has both
    /// (real audio + chunked segments), so the reference-commit tests must supply both too.
    fn real_source_audio(dir: &tempfile::TempDir, name: &str, duration_ms: u32) -> String {
        let audio = dir.path().join(name);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&audio, spec).expect("create wav");
        for _ in 0..(16_000u32 * duration_ms / 1000) {
            writer.write_sample(0i16).expect("write sample");
        }
        writer.finalize().expect("finalize wav");
        audio.to_string_lossy().to_string()
    }

    /// Whole-file source-offset meta (chunk 0 of 1) so a single-segment source spans the whole
    /// reference — a real positional window covering the entire transcript.
    fn whole_file_alignment(duration_ms: i64) -> String {
        crate::chunking::SegmentSourceMeta {
            source_start_ms: 0,
            source_end_ms: duration_ms,
            chunk_index: 0,
            chunk_count: 1,
        }
        .to_alignment_json()
    }

    /// The jury tests below exercise the reference/guard/commit MACHINERY, which only runs when the
    /// Autonomy Dial permits machine commits. The shipped default (AutonLevel::Propose) stages
    /// instead of committing — that contract is pinned separately by
    /// autonomy_dial_governs_every_machine_commit_stage_not_just_t0 — so these tests opt into
    /// ActConfirm, the level whose semantics they were written against.
    fn settings_act_confirm() -> crate::settings::AppSettings {
        crate::settings::AppSettings {
            // These tests intentionally exercise the legacy multi-model jury machinery. Production
            // defaults to WSL7B and bypasses that machinery entirely.
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            jury_autonomy_level: crate::settings::AutonLevel::ActConfirm,
            ..crate::settings::AppSettings::default()
        }
    }

    fn settings_with_source_reference_models(models: &[&str]) -> crate::settings::AppSettings {
        crate::settings::AppSettings {
            source_reference_models: models.iter().map(|model| (*model).to_string()).collect(),
            ..settings_act_confirm()
        }
    }

    #[test]
    fn t2_endpoint_resolves_gemini_by_default_and_openrouter_only_with_a_key() {
        use crate::jury::t2_listener::T2Endpoint;
        let mut s = crate::settings::AppSettings::default();

        // Default provider ("gemini") -> direct Gemini with the passed key + configured jury_model,
        // even when an OpenRouter key happens to be available.
        let (ep, key, model) = resolve_t2_endpoint_from_keys(&s, "gkey", Some("orkey"));
        assert!(matches!(ep, T2Endpoint::GeminiDirect));
        assert_eq!(key, "gkey");
        assert_eq!(model, s.jury_model);

        // Provider "openrouter" but NO OpenRouter key -> fall back to direct Gemini (never keyless cloud).
        s.jury_provider = "openrouter".into();
        let (ep, key, _) = resolve_t2_endpoint_from_keys(&s, "gkey", None);
        assert!(matches!(ep, T2Endpoint::GeminiDirect), "no OR key must stay on Gemini");
        assert_eq!(key, "gkey");
        let (ep, _, _) = resolve_t2_endpoint_from_keys(&s, "gkey", Some("   "));
        assert!(matches!(ep, T2Endpoint::GeminiDirect), "blank OR key must stay on Gemini");

        // Provider "openrouter" WITH a key -> OpenRouter endpoint, the OR key, mapped model slug.
        s.jury_model = "gemini-2.5-pro".into();
        let (ep, key, model) = resolve_t2_endpoint_from_keys(&s, "gkey", Some("orkey"));
        match ep {
            T2Endpoint::OpenAiCompatible { url } => assert!(url.contains("openrouter.ai"), "{url}"),
            _ => panic!("expected OpenRouter endpoint"),
        }
        assert_eq!(key, "orkey");
        assert_eq!(model, "google/gemini-2.5-pro", "a bare Gemini id maps to the OpenRouter slug");

        // An already-slugged model passes through unchanged (mechanism only — policy is that the ckb
        // judge is strictly google/gemini-2.5-pro until another model has a measured ckb CER).
        s.jury_model = "example/future-approved-judge".into();
        let (_, _, model) = resolve_t2_endpoint_from_keys(&s, "gkey", Some("orkey"));
        assert_eq!(model, "example/future-approved-judge");
    }

    #[test]
    fn segment_awaits_wsl7b_flags_only_empty_or_placeholder_transcripts() {
        assert!(segment_awaits_wsl7b(""));
        assert!(segment_awaits_wsl7b("   "));
        assert!(segment_awaits_wsl7b("[Pending WSL 7B ASR]"));
        // Also the placeholders the LOCAL CTC import path can leave, so the batch can recover them too.
        assert!(segment_awaits_wsl7b("[ASR unavailable: model load failed]"));
        assert!(segment_awaits_wsl7b("n/a"));
        // A real transcript (CTC or human) must NOT be flagged — the batch never clobbers good text.
        assert!(!segment_awaits_wsl7b("سڵاو ئەمە دەقێکی ڕاستەقینەیە"));
        assert!(!segment_awaits_wsl7b("a real ctc transcript"));
    }

    #[test]
    fn select_wsl_refinement_targets_orders_by_chunk_offset_within_a_file_not_by_uuid() {
        // Two chunks of ONE file sharing a created_at. The LATER chunk has the lexically-SMALLER id,
        // so a naive id/UUID order (the old `.rev()` of `id ASC`) would pick it first; the chunk's
        // source offset must win so test_one transcribes chunk 0.
        let meta = |start: i64, idx: u32| {
            crate::chunking::SegmentSourceMeta {
                source_start_ms: start,
                source_end_ms: start + 1000,
                chunk_index: idx,
                chunk_count: 2,
            }
            .to_alignment_json()
        };
        let mut early = test_segment("zzz-chunk0", "file.wav", "[Pending WSL 7B ASR]");
        early.created_at = Some("2026-01-01T00:00:00Z".to_string());
        early.alignment_json = Some(meta(0, 0));
        let mut late = test_segment("aaa-chunk1", "file.wav", "[Pending WSL 7B ASR]");
        late.created_at = Some("2026-01-01T00:00:00Z".to_string());
        late.alignment_json = Some(meta(5000, 1));
        // get_segments returns newest-first; pass the later-position chunk first.
        let segments = vec![late, early];
        let targets = select_wsl_refinement_targets(&segments, None, None, true);
        assert_eq!(
            targets,
            vec![("zzz-chunk0".to_string(), "file.wav".to_string())],
            "test_one must pick chunk 0 (earliest source offset), not the lexically-smaller UUID"
        );
    }

    #[test]
    fn select_wsl_refinement_targets_takes_only_pending_oldest_first() {
        // get_segments returns newest-first; the batch must drain oldest-first and skip non-pending.
        let segments = vec![
            test_segment("s4", "b.wav", "real transcript"), // newest, has text -> excluded
            test_segment("s3", "b.wav", "[Pending WSL 7B ASR]"), // pending
            test_segment("s2", "a.wav", ""),                // pending
            test_segment("s1", "a.wav", "already done"),    // oldest, has text -> excluded
        ];
        let targets = select_wsl_refinement_targets(&segments, None, None, false);
        assert_eq!(targets, vec![("s2".to_string(), "a.wav".to_string()), ("s3".to_string(), "b.wav".to_string())]);
    }

    #[test]
    fn select_wsl_refinement_targets_honors_file_segment_and_test_one_limits() {
        // newest-first input; oldest-first pending order is s1(a) s2(a) s3(b) s4(b) s5(c).
        let segments = vec![
            test_segment("s5", "c.wav", ""),
            test_segment("s4", "b.wav", ""),
            test_segment("s3", "b.wav", ""),
            test_segment("s2", "a.wav", ""),
            test_segment("s1", "a.wav", ""),
        ];
        let ids = |targets: Vec<(String, String)>| targets.into_iter().map(|(id, _)| id).collect::<Vec<_>>();

        // limit_files = 2 keeps only the first two distinct files (a, b), not c.
        assert_eq!(ids(select_wsl_refinement_targets(&segments, Some(2), None, false)), vec!["s1", "s2", "s3", "s4"]);
        // limit_segments = 3 caps to the three oldest pending.
        assert_eq!(ids(select_wsl_refinement_targets(&segments, None, Some(3), false)), vec!["s1", "s2", "s3"]);
        // test_one overrides limit_segments down to a single oldest segment.
        assert_eq!(ids(select_wsl_refinement_targets(&segments, None, Some(3), true)), vec!["s1"]);
    }

    fn downloaded_model_status(filename: &str) -> serde_json::Value {
        serde_json::json!({
            "filename": filename,
            "downloaded": true,
        })
    }

    #[test]
    fn agentic_readiness_offline_source_reference_is_not_required_not_blocked() {
        let readiness = build_agentic_readiness(
            &crate::settings::AppSettings::default(), // cloud off, no models
            &[],
            &serde_json::json!({
                "available": false,
                "message": "No external ASR provider script configured"
            }),
        );

        // Cloud whole-file references are OPTIONAL — with cloud off (offline by choice) the check must
        // NOT nag, and must NOT claim coverage either. It previously said "ready", which painted a
        // switched-off dependency the same emerald as proven coverage (deep audit 2026-08-05). It is
        // now `not_required`: neither a warning nor a green tick.
        assert!(
            readiness.checks.iter().any(|c| c.id == "source_reference" && c.status == "not_required"),
            "an optional dependency that is OFF must report not_required, never ready"
        );
        assert!(
            !readiness.checks.iter().any(|c| c.id == "source_reference" && c.status == "ready"),
            "a disabled cloud cross-check must not claim readiness"
        );
        assert_eq!(readiness.status, "blocked");
        assert!(!readiness.ready);
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.id == "hypothesis_coverage" && check.status == "not_required"));
        assert!(readiness.available_hypothesis_models.is_empty());
        assert_eq!(readiness.required_hypothesis_models, quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE);
    }

    /// The whole risk of introducing `not_required` is that it silently becomes a nag: if the
    /// aggregate treated an unknown status as not-ready, switching cloud OFF would flip the overall
    /// verdict and pester the owner on every import — the exact outcome the original "ready" was
    /// chosen to avoid. This pins that the new state is visible per-check WITHOUT changing the verdict.
    #[test]
    fn disabled_cloud_reports_not_required_not_ready_but_keeps_the_overall_verdict_ready() {
        let settings = crate::settings::AppSettings {
            jury_cloud_opt_in: false, // the state under test
            external_asr_script_path: "/root/cortex_env/omniasr.py".to_string(),
            ..crate::settings::AppSettings::default()
        };
        let model_status = vec![
            downloaded_model_status(models::OMNIASR_CTC_300M_MODEL),
            downloaded_model_status(models::OMNIASR_CTC_300M_TOKENS),
        ];
        let readiness = build_agentic_readiness(
            &settings,
            &model_status,
            &serde_json::json!({
                "available": true,
                "script": "/root/cortex_env/omniasr.py",
                "message": "WSL is available; provider script will be used for external ASR"
            }),
        );

        assert!(
            readiness.checks.iter().any(|c| c.id == "source_reference" && c.status == "not_required"),
            "checks: {:?}",
            readiness.checks.iter().map(|c| (&c.id, &c.status)).collect::<Vec<_>>()
        );
        assert_eq!(
            readiness.status, "ready",
            "not_required must not degrade the overall verdict — that would nag on every import"
        );
        assert!(readiness.ready, "the aggregate `ready` flag must stay true for an off-by-choice option");
    }

    #[test]
    fn champion_readiness_ignores_installed_optional_ctc_models() {
        let settings = crate::settings::AppSettings {
            jury_cloud_opt_in: true,
            llm_api_key: "session-key".to_string(),
            external_asr_script_path: "/root/cortex_env/omniasr.py".to_string(),
            source_reference_models: vec!["gemini-2.5-pro".to_string()],
            ..crate::settings::AppSettings::default()
        };
        let model_status = vec![
            downloaded_model_status(models::OMNIASR_CTC_300M_MODEL),
            downloaded_model_status(models::OMNIASR_CTC_300M_TOKENS),
        ];
        let readiness = build_agentic_readiness(
            &settings,
            &model_status,
            &serde_json::json!({
                "available": true,
                "script": "/root/cortex_env/omniasr.py",
                "message": "WSL is available; provider script will be used for external ASR"
            }),
        );

        assert_eq!(readiness.status, "ready");
        assert!(readiness.ready);
        assert_eq!(readiness.available_hypothesis_models, vec!["omniasr-wsl-7b".to_string()]);
        assert!(readiness.checks.iter().any(|check| {
            check.id == "hypothesis_coverage"
                && check.status == "not_required"
                && check.detail.contains("Optional engines will not run")
        }));
        assert_eq!(
            readiness.required_hypothesis_models,
            quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE,
            "operational readiness must not weaken the separate training/export proof threshold"
        );
    }

    #[test]
    fn local_readiness_checks_the_exact_explicitly_selected_engine() {
        let settings = crate::settings::AppSettings {
            asr_model_size: AsrModelSize::CTC1B,
            ..crate::settings::AppSettings::default()
        };
        let model_status = vec![
            downloaded_model_status(models::OMNIASR_CTC_1B_MODEL),
            downloaded_model_status(models::OMNIASR_CTC_1B_TOKENS),
        ];
        let readiness = build_agentic_readiness(
            &settings,
            &model_status,
            &serde_json::json!({ "available": false, "message": "WSL is unavailable" }),
        );

        assert_eq!(readiness.status, "ready");
        assert_eq!(readiness.available_hypothesis_models, vec!["omniasr-ctc-1b".to_string()]);
        assert!(readiness.checks.iter().any(|check| check.id == "primary_asr" && check.status == "ready"));
    }

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }

    #[test]
    fn wsl_log_preview_keeps_short_lines_unchanged() {
        assert_eq!(wsl_log_preview("ready"), "ready");
    }

    #[test]
    fn drain_log_lines_survives_non_utf8_and_delivers_every_line() {
        // A single non-UTF-8 byte mid-stream must NOT terminate the feed (the old
        // lines().map_while(Result::ok) did, silently freezing the live WSL progress feed). Every
        // later line must still arrive, with the bad byte replaced lossily and a trailing CR trimmed.
        let mut data = Vec::new();
        data.extend_from_slice(b"line1\n");
        data.extend_from_slice(&[b'b', b'a', b'd', 0xFF, b'\n']); // invalid UTF-8 line
        data.extend_from_slice("کوردی\n".as_bytes()); // valid Sorani after the bad line
        data.extend_from_slice(b"line4\r\n"); // CRLF — trailing CR must be trimmed

        let mut got = Vec::new();
        drain_log_lines(std::io::Cursor::new(data), |l| got.push(l.to_string()));

        assert_eq!(got.len(), 4, "all four lines must be delivered despite the bad byte: {got:?}");
        assert_eq!(got[0], "line1");
        assert!(got[1].starts_with("bad"), "bad line still delivered lossily: {:?}", got[1]);
        assert_eq!(got[2], "کوردی", "a valid line AFTER the bad one must still arrive");
        assert_eq!(got[3], "line4", "trailing CR is trimmed");
    }

    #[test]
    fn wsl_log_preview_caps_long_lines() {
        let long = format!("{}{}", "x".repeat(WSL_LOG_LINE_PREVIEW_CHARS), "extra");

        let preview = wsl_log_preview(&long);

        assert!(preview.contains("[truncated WSL log line]"));
        assert!(!preview.contains("extra"));
        assert_eq!(preview.split(' ').next().unwrap().len(), WSL_LOG_LINE_PREVIEW_CHARS);
    }

    #[test]
    fn wsl_log_preview_caps_without_splitting_kurdish_chars() {
        let long = format!("{}{}", "ژ".repeat(WSL_LOG_LINE_PREVIEW_CHARS), "extra");

        let preview = wsl_log_preview(&long);

        assert!(preview.contains("[truncated WSL log line]"));
        assert!(!preview.contains("extra"));
        assert_eq!(preview.split(' ').next().unwrap().chars().count(), WSL_LOG_LINE_PREVIEW_CHARS);
    }

    #[test]
    fn source_reference_commit_runs_before_t0_auto_accept() {
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = real_source_audio(&dir, "source.wav", 4000);
        let mut segment = test_segment("seg-reference-first", &audio_path, "wrong local consensus");
        segment.alignment_json = Some(whole_file_alignment(4000)); // real positional window
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "wrong local consensus", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "wrong local consensus", 0.95);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "correct reference phrase", 0.90);
        insert_source_reference(&db, &audio_path, "correct reference phrase");

        let settings = settings_with_source_reference_models(&["gemini-2.5-pro"]);
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(fresh.verdict.as_deref(), Some("jury_accept"));
        assert_eq!(fresh.verdict_transcript.as_deref(), Some("correct reference phrase"));
        let evidence = fresh.evidence_json.as_deref().unwrap_or("");
        assert!(evidence.contains("referenceModelId"));
        assert!(evidence.contains("gemini-2.5-pro"));
    }

    #[test]
    fn champion_review_filters_every_stale_auxiliary_vote() {
        let mut segment = test_segment("champion-consensus", "/audio/champion.wav", "champion draft");
        segment.model_version_id = Some(crate::pipeline::CHAMPION_MODEL_ID.to_string());
        let hypotheses = vec![
            crate::db::SegmentHypothesis {
                segment_id: segment.id.clone(),
                model_id: "omniasr-ctc-300m".to_string(),
                transcript: "stale 300m draft".to_string(),
                confidence: Some(0.99),
            },
            crate::db::SegmentHypothesis {
                segment_id: segment.id.clone(),
                model_id: "finetuned-mms-ckb".to_string(),
                transcript: "stale mms draft".to_string(),
                confidence: Some(0.99),
            },
            crate::db::SegmentHypothesis {
                segment_id: segment.id.clone(),
                model_id: "scribe-v1".to_string(),
                transcript: "stale cloud draft".to_string(),
                confidence: Some(0.99),
            },
        ];

        let filtered = hypotheses_for_selected_asr(&crate::settings::AsrModelSize::WSL7B, &segment, hypotheses, true);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].model_id, crate::pipeline::CHAMPION_MODEL_ID);
        assert_eq!(filtered[0].transcript, "champion draft");
    }

    #[test]
    fn champion_mode_jury_is_a_no_write_human_review_handoff() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let mut segment = test_segment("champion-no-jury", "/audio/champion.wav", "champion draft");
        segment.model_version_id = Some(crate::pipeline::CHAMPION_MODEL_ID.to_string());
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "stale smaller-model vote", 0.99);

        let settings = crate::settings::AppSettings {
            asr_model_size: crate::settings::AsrModelSize::WSL7B,
            multi_engine_hypotheses: true,
            jury_cloud_opt_in: true,
            llm_api_key: "must-not-be-used".to_string(),
            ..crate::settings::AppSettings::default()
        };
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["mode"], "not_required");
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert!(fresh.verdict.is_none(), "champion handoff must not manufacture a machine verdict");
        assert_eq!(fresh.raw_transcript, "champion draft");
    }

    #[test]
    fn current_schema_retires_machine_jury_even_for_an_auxiliary_diagnostic_selection() {
        let db = crate::db::Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let segment = test_segment("current-schema-no-jury", "/audio/diagnostic.wav", "diagnostic draft");
        db.insert_segment(&segment).unwrap();

        let settings = crate::settings::AppSettings {
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            multi_engine_hypotheses: true,
            jury_cloud_opt_in: true,
            llm_api_key: "must-not-be-used".to_string(),
            ..crate::settings::AppSettings::default()
        };
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["mode"], "not_required");
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert!(
            report["reason"].as_str().unwrap_or("").contains("Schema v60+"),
            "the handoff must explain the current trust boundary: {report}"
        );
        assert!(fresh.verdict.is_none(), "the retired machine jury must not author review truth");
        assert!(!fresh.escalated, "the retired machine jury must not rewrite queue state");
    }

    #[test]
    fn autonomy_dial_governs_every_machine_commit_stage_not_just_t0() {
        // Round-24 hunt #1 (HIGH): under Observe/Propose the dial was enforced only inside
        // run_t0_gate — the SAME pipeline run then machine-committed 'jury_accept' via the
        // reference-selection stage (and T2), silently removing from the human queue the very
        // segments the dial promised to stage.

        // ── Propose: a committable reference selection must STAGE, never commit. ──
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "propose.wav");
        let segment = test_segment("seg-propose", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "wrong local consensus", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "wrong local consensus", 0.95);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "correct reference phrase", 0.90);
        insert_source_reference(&db, &audio_path, "correct reference phrase");

        let mut settings = settings_with_source_reference_models(&["gemini-2.5-pro"]);
        settings.jury_autonomy_level = crate::settings::AutonLevel::Propose;
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0), "Propose must never machine-commit: {report}");
        assert_eq!(
            fresh.verdict.as_deref(),
            Some("escalated"),
            "Propose stages the segment for the human, not 'jury_accept'"
        );
        assert!(fresh.escalated, "the staged segment must sit in the human escalation queue");
        assert_eq!(report["humanInbox"].as_u64(), Some(1), "the staged segment is reported in humanInbox: {report}");

        // ── Observe: the pipeline writes NOTHING — a pre-staged verdict survives untouched. ──
        // (Before the fix, the T2-disabled fallback REWROTE the verdict rationale and NULLed the
        // IRT confidence of every escalated segment fed back through the review loop.)
        let db2 = legacy_machine_db();
        let seg2 = test_segment("seg-observe", "/audio/observe.wav", "draft");
        db2.insert_segment(&seg2).unwrap();
        insert_hypothesis(&db2, &seg2.id, "omniasr-wsl-7b", "draft", 0.9);
        insert_hypothesis(&db2, &seg2.id, "omniasr-ctc-300m", "other draft", 0.5);
        db2.write_segment_verdict(&seg2.id, "escalated", None, Some("original rationale"), None, Some(0.42), true)
            .unwrap();

        let observe = crate::settings::AppSettings {
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            jury_autonomy_level: crate::settings::AutonLevel::Observe,
            ..crate::settings::AppSettings::default()
        };
        run_jury_pipeline_core(&db2, &observe, vec![seg2.id.clone()]).unwrap();
        let fresh2 = db2.get_segment_by_id(&seg2.id).unwrap().unwrap();
        assert_eq!(fresh2.rationale.as_deref(), Some("original rationale"), "Observe must not rewrite verdicts");
        assert_eq!(fresh2.agreement_score, Some(0.42), "Observe must not NULL the staged IRT confidence");

        // ── Propose: an already-staged segment keeps its confidence (riskiest-first ordering). ──
        let propose = crate::settings::AppSettings {
            asr_model_size: crate::settings::AsrModelSize::CTC300M,
            jury_autonomy_level: crate::settings::AutonLevel::Propose,
            ..crate::settings::AppSettings::default()
        };
        run_jury_pipeline_core(&db2, &propose, vec![seg2.id.clone()]).unwrap();
        let fresh3 = db2.get_segment_by_id(&seg2.id).unwrap().unwrap();
        assert_eq!(
            fresh3.agreement_score,
            Some(0.42),
            "Propose must not clobber an already-staged segment's IRT confidence"
        );
        assert!(fresh3.escalated, "the segment stays in the human queue");
    }

    /// The provenance string must not depend on the order the references were written.
    ///
    /// `agreeing_source_references_preserve_per_model_evidence` below caught the defect by LUCK: it
    /// asserts one exact string, and the suite happened to fail on the run where SQLite returned the
    /// two same-second rows the other way round. Passing it five times proves nothing, because
    /// nothing in the test controls that tie. This one does: same two models, INSERTED IN THE
    /// OPPOSITE ORDER, must produce the identical canonical value — which cannot hold unless the
    /// join is sorted, whatever the query hands back.
    #[test]
    fn the_consensus_provenance_string_is_canonical_not_insertion_ordered() {
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = real_source_audio(&dir, "reference-order.wav", 4000);
        let mut segment = test_segment("seg-reference-order", &audio_path, "wrong local consensus");
        segment.alignment_json = Some(whole_file_alignment(4000));
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "correct reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        // FLASH FIRST — the reverse of the sibling test.
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "correct reference phrase");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "correct reference phrase");

        run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();
        let evidence: serde_json::Value =
            serde_json::from_str(fresh.evidence_json.as_deref().expect("reference evidence json")).unwrap();
        assert_eq!(
            evidence.get("referenceModelId").and_then(serde_json::Value::as_str),
            Some("multi-reference-consensus:gemini-2.5-flash+gemini-2.5-pro"),
            "the same set of agreeing models must render identically however they were written — a \
             string that changes between identical runs is not provenance"
        );
    }

    #[test]
    fn agreeing_source_references_preserve_per_model_evidence() {
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = real_source_audio(&dir, "agreeing-references.wav", 4000);
        let mut segment = test_segment("seg-reference-agreement", &audio_path, "wrong local consensus");
        segment.alignment_json = Some(whole_file_alignment(4000)); // real positional window
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "correct reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "correct reference phrase");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "correct reference phrase");

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("jury_accept"));
        assert_eq!(fresh.verdict_transcript.as_deref(), Some("correct reference phrase"));
        let evidence: serde_json::Value =
            serde_json::from_str(fresh.evidence_json.as_deref().expect("reference evidence json")).unwrap();
        // CANONICAL (sorted), not insertion order. This assertion is what caught the defect: the
        // same binary produced `pro+flash` on one run and `flash+pro` on another, because the two
        // references are written in the same second and the ORDER BY had no tiebreaker. A provenance
        // string that changes between identical runs is not provenance.
        assert_eq!(
            evidence.get("referenceModelId").and_then(serde_json::Value::as_str),
            Some("multi-reference-consensus:gemini-2.5-flash+gemini-2.5-pro")
        );
        let agreement = evidence.get("referenceAgreement").and_then(serde_json::Value::as_array).unwrap();
        assert_eq!(agreement.len(), 2);
        assert!(agreement.iter().any(|item| item["referenceModelId"] == "gemini-2.5-pro"));
        assert!(agreement.iter().any(|item| item["referenceModelId"] == "gemini-2.5-flash"));
    }

    #[test]
    fn source_reference_guard_blocks_t0_and_t1_auto_commit_when_inconclusive() {
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "guarded.wav");
        let segment = test_segment("seg-reference-guard", &audio_path, "fluent local phrase");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "fluent local phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "fluent local phrase", 0.95);
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "unrelated source context");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "unrelated source context");

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        assert!(fresh.rationale.as_deref().unwrap_or("").contains("T2 disabled"));
    }

    #[test]
    fn incomplete_source_reference_model_coverage_blocks_auto_commit() {
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "incomplete-reference-coverage.wav");
        let segment = test_segment("seg-incomplete-reference-coverage", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "correct reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        // The sole canonical advisory model exists but has no usable text: coverage must fail closed
        // without inventing a second cloud model merely to exercise the guard.
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "");

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Source-reference coverage guard blocked automatic adjudication"));
        assert!(rationale.contains("gemini-2.5-pro"));
        assert!(rationale.contains("T2 disabled"));
    }

    #[test]
    fn stale_source_reference_audio_identity_blocks_automatic_jury_commit() {
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio = dir.path().join("same-path-source.wav");
        std::fs::write(&audio, b"current-audio-bytes").unwrap();
        let audio_path = audio.to_string_lossy().to_string();
        let segment = test_segment("seg-stale-source-reference", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "stale reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        db.upsert_source_transcript(&crate::db::SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: Some("old-audio-content-hash".to_string()),
            audio_size_bytes: Some(1),
            transcript_path: "gemini-2.5-pro.source_reference.txt".to_string(),
            transcript_text: "stale reference phrase".to_string(),
            created_at: None,
        })
        .unwrap();

        let settings = settings_with_source_reference_models(&["gemini-2.5-pro"]);
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Source-reference audio identity guard blocked automatic adjudication"));
        assert!(rationale.contains("gemini-2.5-pro"));
        assert!(rationale.contains("T2 disabled"));
    }

    #[test]
    fn missing_source_reference_audio_identity_blocks_automatic_jury_commit() {
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "legacy-source-reference.wav");
        let segment = test_segment("seg-legacy-source-reference", &audio_path, "wrong local consensus");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "legacy reference phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "wrong local consensus", 0.98);
        db.upsert_source_transcript(&crate::db::SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: None,
            audio_size_bytes: None,
            transcript_path: "gemini-2.5-pro.source_reference.txt".to_string(),
            transcript_text: "legacy reference phrase".to_string(),
            created_at: None,
        })
        .unwrap();

        let settings = settings_with_source_reference_models(&["gemini-2.5-pro"]);
        let report = run_jury_pipeline_core(&db, &settings, vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Source-reference audio identity guard blocked automatic adjudication"));
        assert!(rationale.contains("gemini-2.5-pro"));
        assert!(rationale.contains("T2 disabled"));
    }

    #[test]
    fn incomplete_hypothesis_model_coverage_blocks_t0_and_t1_auto_commit() {
        let db = legacy_machine_db();
        let audio_path = "/audio/one-hypothesis.wav";
        let segment = test_segment("seg-one-hypothesis", audio_path, "fluent single model phrase");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "fluent single model phrase", 0.99);

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["hypothesisGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        let rationale = fresh.rationale.as_deref().unwrap_or("");
        assert!(rationale.contains("Multi-model hypothesis coverage guard blocked automatic adjudication"));
        assert!(rationale.contains("fewer than 2 non-empty model hypotheses"));
        assert!(rationale.contains("omniasr-ctc-300m"));
        assert!(rationale.contains("T2 disabled"));
    }

    #[test]
    fn source_reference_disagreement_blocks_automatic_commit() {
        let db = legacy_machine_db();
        let dir = tempfile::TempDir::new().unwrap();
        let audio_path = test_source_audio(&dir, "conflicting-references.wav");
        let segment = test_segment("seg-reference-conflict", &audio_path, "fluent local phrase");
        db.insert_segment(&segment).unwrap();
        insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "first correct phrase", 0.99);
        insert_hypothesis(&db, &segment.id, "omniasr-ctc-1b", "second correct phrase", 0.98);
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-pro", "first correct phrase");
        insert_source_reference_with_model(&db, &audio_path, "gemini-2.5-flash", "second correct phrase");

        let report = run_jury_pipeline_core(&db, &settings_act_confirm(), vec![segment.id.clone()]).unwrap();
        let fresh = db.get_segment_by_id(&segment.id).unwrap().unwrap();

        assert_eq!(report["referenceCommitted"].as_u64(), Some(0));
        assert_eq!(report["referenceGuarded"].as_u64(), Some(1));
        assert_eq!(report["t0AutoAccepted"].as_u64(), Some(0));
        assert_eq!(report["t1Committed"].as_u64(), Some(0));
        assert_eq!(report["humanInbox"].as_u64(), Some(1));
        assert_eq!(fresh.verdict.as_deref(), Some("escalated"));
        assert!(fresh.rationale.as_deref().unwrap_or("").contains("T2 disabled"));
    }
}

// ════════════════════════════════════════════════════════════════════════════
// State-taking command coverage through the shared MockRuntime harness
// (`crate::test_support`): every call below goes through a genuine
// `State<'_, AppState>` exactly as the renderer's IPC dispatch would deliver it.
// ════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod state_command_harness_tests {
    use super::*;
    use crate::test_support::managed_app_state;

    #[test]
    fn get_settings_serves_the_scrubbed_default_snapshot_never_the_session_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        {
            let state = app.state::<AppState>();
            let mut live = state.lock_settings();
            live.llm_api_key = "session-only-secret".to_string();
        }

        let served = get_settings(app.state()).expect("get_settings");

        assert_eq!(served.llm_api_key, "", "the session secret must never leave through get_settings");
        assert!(served.llm_api_key_configured, "a present secret must still report as configured");
        let expected = AppSettings { llm_api_key_configured: true, ..AppSettings::default() }.for_client_response();
        assert_eq!(
            serde_json::to_value(&served).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "everything else must be the exact default client snapshot"
        );
    }

    #[test]
    fn get_settings_v1_snapshot_is_stable_and_reports_the_default_privacy_posture() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let first = get_settings_v1(app.state()).expect("first snapshot");
        let second = get_settings_v1(app.state()).expect("second snapshot");

        assert_eq!(first, second, "an unchanged store must serve an identical snapshot + revision");
        assert!(!first.settings.cloud_llm_opt_in, "cloud LLM consent defaults OFF");
        assert!(!first.settings.jury_cloud_opt_in, "jury cloud consent defaults OFF");
        assert!(!first.settings.llm_api_key_configured);
        assert!(!first.settings.use_finetuned_asr, "champion supremacy: no finetuned override by default");
        assert_eq!(first.settings.language, "ckb");
        assert!(first.settings_revision >= 0, "revision must survive the i64->number TS binding");
    }

    #[test]
    fn get_configured_providers_speaks_a_closed_vocabulary_and_refuses_without_a_store() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        // No secrets.env exists in the temp data dir; ambient env keys may still legitimately
        // surface, so the exact-content claim here is the closed provider vocabulary.
        let providers = get_configured_providers(app.state()).expect("providers with a data dir");
        for name in &providers {
            assert!(name == "gemini" || name == "openrouter", "unexpected provider name {name:?}");
        }

        *app.state::<AppState>().lock_data_dir() = None;
        let refused = get_configured_providers(app.state()).expect_err("no data dir must refuse");
        assert_eq!(refused.code, "API_KEY_STORE_UNAVAILABLE");
        assert!(!refused.retryable);
        assert_eq!(refused.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::OpenHealth));
    }

    #[test]
    fn list_model_versions_serves_only_the_champion_family_with_exact_public_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        {
            let state = app.state::<AppState>();
            let db = state.lock_db();
            crate::registry::register_candidate(
                &db,
                &crate::registry::NewModelVersion {
                    id: "cand-7b".to_string(),
                    family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                    model_card_name: Some("owner-card".to_string()),
                    checkpoint_sha256: "a".repeat(64),
                    checkpoint_path: "/ckpt/cand-7b".to_string(),
                    source: "meta-stock".to_string(),
                    license: "SAIL".to_string(),
                },
            )
            .expect("register 7B candidate");
            crate::registry::register_candidate(
                &db,
                &crate::registry::NewModelVersion {
                    id: "diag-mms".to_string(),
                    family: "mms".to_string(),
                    model_card_name: None,
                    checkpoint_sha256: "b".repeat(64),
                    checkpoint_path: "/ckpt/diag-mms".to_string(),
                    source: "meta-stock".to_string(),
                    license: "CC-BY-NC".to_string(),
                },
            )
            .expect("register diagnostic-family row");
        }

        let rows = list_model_versions(app.state()).expect("list_model_versions");

        assert_eq!(
            rows,
            vec![ModelVersionSummaryV1 {
                id: "cand-7b".to_string(),
                family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                model_card_name: Some("owner-card".to_string()),
                checkpoint_sha256: "a".repeat(64),
                source: "meta-stock".to_string(),
                license: "SAIL".to_string(),
                status: "candidate".to_string(),
            }],
            "diagnostic families must be filtered out and the public row must carry no checkpoint path"
        );
    }

    #[test]
    fn db_info_reports_the_live_library_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let empty = db_info(app.state()).expect("db_info on a fresh library");
        assert_eq!(empty["segmentCount"], 0);
        assert_eq!(empty["journalMode"], "wal");
        assert_eq!(empty["path"], tmp.path().join("app-state.db").to_string_lossy().as_ref());
        assert!(empty["sizeBytes"].as_u64().unwrap() > 0, "a migrated database file is never empty");

        app.state::<AppState>()
            .lock_db()
            .insert_segment(&crate::db::SpeechSegment {
                id: "seg-info-1".to_string(),
                audio_path: "C:/fixtures/seg-info-1.wav".to_string(),
                raw_transcript: "deng".to_string(),
                ..Default::default()
            })
            .expect("seed segment");
        let seeded = db_info(app.state()).expect("db_info after insert");
        assert_eq!(seeded["segmentCount"], 1);
    }

    #[test]
    fn get_history_status_v1_reports_honest_empty_stacks() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let status = get_history_status_v1(app.state()).expect("history status");

        assert_eq!(
            status,
            crate::ipc_contract::HistoryStatusV1 { undo_action: None, redo_action: None },
            "a fresh session has nothing to undo or redo — no English fallback allowed"
        );
    }

    #[test]
    fn cancel_operation_signals_every_armed_cancellation_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        cancel_operation(app.state()).expect("cancel with nothing armed is still Ok");
        assert!(!app.state::<AppState>().is_cancelled(), "no token was armed, so nothing may read cancelled");

        let import_token = crate::cancel::CancellationToken::new();
        let batch_token = crate::cancel::CancellationToken::new();
        {
            let state = app.state::<AppState>();
            *state.import_cancel_token.lock().unwrap() = Some(import_token.clone());
            *state.batch_cancel_token.lock().unwrap() = Some(batch_token.clone());
        }

        cancel_operation(app.state()).expect("cancel with armed tokens");

        assert!(import_token.is_cancelled(), "the import slot must be signalled");
        assert!(batch_token.is_cancelled(), "the batch slot must be signalled");
        assert!(app.state::<AppState>().is_cancelled());
    }

    #[test]
    fn get_jobs_serves_seeded_durable_jobs_newest_first_with_exact_wire_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        {
            let state = app.state::<AppState>();
            let db = state.lock_db();
            db.create_or_get_job("job-a", "import", None, Some(4)).expect("seed job-a");
            db.create_or_get_job("job-b", "export_dataset", None, None).expect("seed job-b");
        }

        let jobs = tauri::async_runtime::block_on(get_jobs(app.state())).expect("get_jobs");

        assert_eq!(
            jobs,
            vec![
                JobV1 {
                    id: "job-b".to_string(),
                    kind: "export_dataset".to_string(),
                    state: JobStateV1::Queued,
                    progress: 0.0,
                    completed: 0,
                    total: None,
                    error_code: None,
                },
                JobV1 {
                    id: "job-a".to_string(),
                    kind: "import".to_string(),
                    state: JobStateV1::Queued,
                    progress: 0.0,
                    completed: 0,
                    total: Some(4),
                    error_code: None,
                },
            ],
            "newest first (created_at DESC, id DESC), exact queued lifecycle fields"
        );
    }

    #[test]
    fn merge_dataset_json_creates_rows_and_refuses_malformed_or_blanking_payloads() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let incoming = crate::db::SpeechSegment {
            id: "seg-merge-1".to_string(),
            audio_path: "C:/fixtures/seg-merge-1.wav".to_string(),
            raw_transcript: "deng yek du".to_string(),
            ..Default::default()
        };
        let payload = serde_json::to_string(&vec![incoming]).unwrap();

        let merged = tauri::async_runtime::block_on(merge_dataset_json(payload, app.state())).expect("merge new row");
        assert_eq!(merged, crate::ipc_contract::MergeDatasetResultV1 { created: 1, updated: 0 });
        let stored = app
            .state::<AppState>()
            .lock_db()
            .get_segment_by_id("seg-merge-1")
            .expect("read back")
            .expect("merged row exists");
        assert_eq!(stored.raw_transcript, "deng yek du");

        let malformed = tauri::async_runtime::block_on(merge_dataset_json("this is not json".to_string(), app.state()))
            .expect_err("malformed payload must refuse");
        assert_eq!(malformed.code, "DATASET_MERGE_FAILED");
        assert!(!malformed.retryable);
        assert_eq!(malformed.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::OpenHealth));

        // The blank-transcript guard: a pre-ASR export must never blank an existing good draft.
        let blanking = serde_json::to_string(&vec![crate::db::SpeechSegment {
            id: "seg-merge-1".to_string(),
            audio_path: "C:/fixtures/seg-merge-1.wav".to_string(),
            raw_transcript: "".to_string(),
            ..Default::default()
        }])
        .unwrap();
        let refused = tauri::async_runtime::block_on(merge_dataset_json(blanking, app.state()))
            .expect_err("blank transcript must refuse atomically");
        assert_eq!(refused.code, "DATASET_MERGE_FAILED");
        let untouched = app.state::<AppState>().lock_db().get_segment_by_id("seg-merge-1").unwrap().unwrap();
        assert_eq!(untouched.raw_transcript, "deng yek du", "the refusal must leave the good draft intact");

        // The other honest counter: re-merging an existing id with a fresh non-blank draft reports
        // updated (never created) and actually replaces the stored machine text.
        let refreshed = serde_json::to_string(&vec![crate::db::SpeechSegment {
            id: "seg-merge-1".to_string(),
            audio_path: "C:/fixtures/seg-merge-1.wav".to_string(),
            raw_transcript: "deng yek du sê".to_string(),
            ..Default::default()
        }])
        .unwrap();
        let second = tauri::async_runtime::block_on(merge_dataset_json(refreshed, app.state()))
            .expect("merge onto an existing unreviewed row");
        assert_eq!(second, crate::ipc_contract::MergeDatasetResultV1 { created: 0, updated: 1 });
        let replaced = app.state::<AppState>().lock_db().get_segment_by_id("seg-merge-1").unwrap().unwrap();
        assert_eq!(replaced.raw_transcript, "deng yek du sê");
    }

    #[test]
    fn get_segment_consensus_refuses_typed_and_serves_empty_provenance_honestly() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let invalid = get_segment_consensus(app.state(), "bad id!".to_string())
            .expect_err("a non-identifier must refuse before any read");
        assert_eq!(invalid.code, "INVALID_SEGMENT_ID");

        let missing =
            get_segment_consensus(app.state(), "seg-missing".to_string()).expect_err("an unknown segment must refuse");
        assert_eq!(missing.code, "SEGMENT_NOT_FOUND");
        assert_eq!(missing.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::ReloadClip));

        // A row with NO recorded producing model: consensus must invent no provenance.
        app.state::<AppState>()
            .lock_db()
            .insert_segment(&crate::db::SpeechSegment {
                id: "seg-unattributed".to_string(),
                audio_path: "C:/fixtures/seg-unattributed.wav".to_string(),
                raw_transcript: "deng yek du".to_string(),
                ..Default::default()
            })
            .expect("seed unattributed segment");
        let empty = get_segment_consensus(app.state(), "seg-unattributed".to_string()).expect("unattributed consensus");
        assert_eq!(
            empty,
            crate::ipc_contract::SegmentConsensusV1 {
                draft: String::new(),
                words: Vec::new(),
                model_count: 0,
                min_agreement: 0.0,
                mean_agreement: 0.0,
                models: Vec::new(),
            },
            "no persisted model id means no attributable vote — never invented provenance"
        );
    }

    #[test]
    fn get_segment_consensus_votes_the_champion_draft_when_stored_hypotheses_are_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        app.state::<AppState>()
            .lock_db()
            .insert_segment(&crate::db::SpeechSegment {
                id: "seg-champion".to_string(),
                audio_path: "C:/fixtures/seg-champion.wav".to_string(),
                raw_transcript: "deng yek du".to_string(),
                model_version_id: Some(LEGACY_CHAMPION_MODEL_ID.to_string()),
                confidence: Some(0.9),
                ..Default::default()
            })
            .expect("seed champion-attributed segment");

        let consensus = get_segment_consensus(app.state(), "seg-champion".to_string()).expect("champion consensus");

        assert_eq!(consensus.draft, "deng yek du");
        assert_eq!(consensus.models, vec![LEGACY_CHAMPION_MODEL_ID.to_string()]);
        assert_eq!(consensus.model_count, 1);
        assert_eq!(consensus.min_agreement, 1.0);
        assert_eq!(consensus.mean_agreement, 1.0);
        assert_eq!(consensus.words.len(), 3);
        for (word, expected_text) in consensus.words.iter().zip(["deng", "yek", "du"]) {
            assert_eq!(word.text, expected_text);
            assert_eq!(word.agreement, 1.0, "a single-model vote is unanimous by construction");
            assert_eq!(word.models_agreeing, 1);
            assert_eq!(word.total_models, 1);
            assert!(word.alternatives.is_empty());
        }
    }

    #[test]
    fn clear_escalation_is_a_retired_endpoint_with_exact_refusals() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let invalid =
            clear_escalation(app.state(), "bad id".to_string()).expect_err("a malformed id must fail validation first");
        assert_eq!(invalid, "Identifier must be alphanumeric (underscore, hyphen, dot allowed)");

        let retired =
            clear_escalation(app.state(), "seg-1".to_string()).expect_err("the identity-free path is retired");
        assert_eq!(
            retired,
            "EXACT_FLAG_UNDO_REQUIRED: identity-free escalation clearing is retired; use the immutable flag effect and an operation UUID"
        );
    }

    #[test]
    fn review_team_reports_are_exactly_empty_on_a_fresh_library() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        assert!(spot_check_report(app.state()).expect("spot_check_report").is_empty());
        assert!(reviewer_throughput(app.state()).expect("reviewer_throughput").is_empty());
        assert!(
            export_agreement_sample(app.state()).expect("export_agreement_sample").is_none(),
            "nothing double-reviewed yet is an honest None, not an error"
        );
        assert!(!tmp.path().join("agreement_sample.tsv").exists(), "an empty sample must not leave a TSV behind");
        assert!(list_gold_segments(app.state()).expect("list_gold_segments").is_empty());
        assert!(list_eval_runs(app.state()).expect("list_eval_runs").is_empty());
    }

    #[test]
    fn get_few_shot_examples_validates_the_id_and_serves_nothing_from_an_empty_store() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());

        let invalid =
            get_few_shot_examples(app.state(), "../etc".to_string(), 3).expect_err("a path-like id must refuse");
        assert_eq!(invalid, "Identifier must be alphanumeric (underscore, hyphen, dot allowed)");

        let examples = get_few_shot_examples(app.state(), "seg-none".to_string(), 3).expect("empty example store");
        assert!(examples.is_empty(), "no human-verified corrections exist, so no exemplars may be served");
    }

    #[test]
    fn couch_review_status_reports_the_stopped_server_exactly() {
        let status = couch_review_status().expect("couch status");
        assert!(!status.running, "no test starts the couch server; status must say stopped");
        assert!(status.reviewers.is_empty(), "a stopped server mints no reviewer links");
        assert_eq!(status.certificate_fingerprint, None);
    }

    // ── Wave-4: commands.rs-root helpers + remaining State commands ─────────────────────────────

    fn selection_report(
        model: &str,
        transcript: &str,
        score: f64,
        margin: f64,
        commit: bool,
    ) -> crate::agentic::CandidateSelectionReport {
        crate::agentic::CandidateSelectionReport {
            reference_model_id: Some(format!("ref-{model}")),
            selected_model_id: model.to_string(),
            selected_transcript: transcript.to_string(),
            selected_score: score,
            confidence: 0.5,
            margin,
            should_commit: commit,
            positional_window: false,
            rationale: "test rationale".to_string(),
            reference_window_preview: String::new(),
            scores: Vec::new(),
            reference_agreement: Vec::new(),
        }
    }

    fn eval_run_fixture(id: &str, wer: f64, cer: f64) -> crate::ipc_contract::EvalRunResultV1 {
        crate::ipc_contract::EvalRunResultV1 {
            run: crate::ipc_contract::EvalRunV1 {
                id: id.to_string(),
                model_id: "engine-under-eval".to_string(),
                run_at: "2026-08-31T00:00:00Z".to_string(),
                num_segs: 2,
                wer,
                cer,
                meta_json: None,
            },
            segments: vec![
                crate::ipc_contract::EvalSegmentResultV1 {
                    gold_id: "gold-1".to_string(),
                    audio_path: "C:/fixtures/gold-1.wav".to_string(),
                    reference: "deng yek du".to_string(),
                    hypothesis: "deng yek".to_string(),
                    wer,
                    cer,
                },
                crate::ipc_contract::EvalSegmentResultV1 {
                    gold_id: "gold-2".to_string(),
                    audio_path: "C:/fixtures/gold-2.wav".to_string(),
                    reference: "deng sê".to_string(),
                    hypothesis: "deng sê".to_string(),
                    wer: 0.0,
                    cer: 0.0,
                },
            ],
        }
    }

    #[test]
    fn build_scorecard_serves_both_baseline_arms_from_real_eval_rows() {
        let solo = build_scorecard(eval_run_fixture("run-sys", 0.5, 0.25), None).expect("scorecard without baseline");
        assert!(
            matches!(solo.scorecard, crate::ipc_contract::ScorecardV1::WithoutBaseline(_)),
            "no baseline supplied means no comparison may be invented"
        );
        assert!(solo.markdown.contains("engine-under-eval"), "markdown names the measured engine");
        assert!(solo.markdown.contains("ASR Scorecard"), "markdown renders the scorecard header");

        let compared =
            build_scorecard(eval_run_fixture("run-sys", 0.5, 0.25), Some(eval_run_fixture("run-base", 1.0, 0.5)))
                .expect("scorecard with baseline");
        assert!(
            matches!(compared.scorecard, crate::ipc_contract::ScorecardV1::WithBaseline(_)),
            "a supplied baseline must produce the significance comparison"
        );
    }

    #[test]
    fn batch_identity_and_config_hashing_are_deterministic_and_typed() {
        #[derive(serde::Serialize)]
        struct Config {
            schema: u8,
            flag: bool,
        }
        let first = canonical_batch_config_sha256(&Config { schema: 1, flag: true }).expect("hash config");
        let again = canonical_batch_config_sha256(&Config { schema: 1, flag: true }).expect("hash config again");
        let other = canonical_batch_config_sha256(&Config { schema: 1, flag: false }).expect("hash other config");
        assert_eq!(first, again, "the same closed config must hash identically");
        assert_ne!(first, other, "a result-affecting flag change must change the hash");
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));

        // Real serde_json behavior: a non-finite float does NOT error — it canonicalizes silently as
        // JSON null, so it hashes identically to `()` (also "null"). Callers must therefore pass
        // closed structs, never raw floats, or two different broken configs share one digest.
        let non_finite = canonical_batch_config_sha256(&f64::NAN).expect("non-finite floats canonicalize as null");
        assert_eq!(non_finite, canonical_batch_config_sha256(&()).expect("unit serializes as null"));

        // The error arm needs a value serde_json genuinely refuses: a map with non-string keys.
        let mut tuple_keyed = std::collections::HashMap::new();
        tuple_keyed.insert((1_u8, 2_u8), 3_u8);
        let unserializable =
            canonical_batch_config_sha256(&tuple_keyed).expect_err("non-string map keys cannot serialize");
        assert!(unserializable.starts_with("BATCH_CONFIG_SERIALIZATION_FAILED"));

        let identity = new_batch_executor_identity();
        assert_eq!(identity.git_sha, crate::GIT_SHA);
        assert_eq!(identity.attempt_generation, 1);
        assert_eq!(identity.token_sha256.len(), 64);
        assert_ne!(identity.token_sha256, new_batch_executor_identity().token_sha256, "attempt tokens are unique");

        let cancelled = batch_start_commit_error(crate::BatchStartCommitError::Cancelled);
        assert_eq!(cancelled.code, "BATCH_START_CANCELLED");
        assert!(cancelled.retryable);
        let lost = batch_start_commit_error(crate::BatchStartCommitError::AuthorityLost);
        assert_eq!(lost.code, "BATCH_START_AUTHORITY_LOST");
        assert!(!lost.retryable);
        assert_eq!(lost.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::OpenHealth));
    }

    #[test]
    fn model_status_probe_and_readiness_snapshot_report_reality() {
        let status = vec![
            serde_json::json!({ "filename": "model-a.onnx", "downloaded": true }),
            serde_json::json!({ "filename": "model-b.onnx", "downloaded": false }),
            serde_json::json!({ "note": "no filename or downloaded keys" }),
        ];
        assert!(model_downloaded(&status, "model-a.onnx"));
        assert!(!model_downloaded(&status, "model-b.onnx"), "a present-but-not-downloaded model is not ready");
        assert!(!model_downloaded(&status, "model-c.onnx"), "an unlisted model is not ready");

        // Offline default: no provider script configured — the exact closed message, no probe run.
        let settings = AppSettings::default();
        let no_script = external_provider_status(&settings);
        assert_eq!(no_script["available"], false);
        assert_eq!(no_script["message"], "No external ASR provider script configured");

        let snapshot = build_agentic_readiness_snapshot(&settings, &[], &no_script);
        assert_eq!(snapshot["ready"], false, "an unconfigured champion provider cannot claim readiness");
        assert_eq!(snapshot["status"], "blocked");
        assert_eq!(
            snapshot["requiredHypothesisModels"].as_u64().map(|count| count as usize),
            Some(quality::MIN_HYPOTHESIS_MODELS_FOR_TRAINING_READY_MACHINE)
        );
        let checks = snapshot["checks"].as_array().expect("readiness checks");
        assert_eq!(checks[0]["id"], "source_reference");
        assert_eq!(checks[0]["status"], "not_required", "cloud references off by choice is not a green tick");
        assert_eq!(checks[1]["id"], "primary_asr");
        assert_eq!(checks[1]["status"], "blocked");
        assert_eq!(checks[2]["id"], "hypothesis_coverage");
        assert_eq!(checks[2]["status"], "not_required");
    }

    #[test]
    fn agentic_readiness_reports_cloud_reference_and_diagnostic_engine_arms() {
        let no_script = serde_json::json!({ "available": false, "message": "no provider" });

        // Opted-in but keyless: whole-file references are blocked and block the verdict.
        let keyless = AppSettings { jury_cloud_opt_in: true, ..AppSettings::default() };
        let readiness = build_agentic_readiness(&keyless, &[], &no_script);
        assert_eq!(readiness.checks[0].id, "source_reference");
        assert_eq!(readiness.checks[0].status, "blocked");
        assert!(!readiness.ready);

        // Opted-in with a loaded session key: references are ready and name the advisory model.
        let keyed =
            AppSettings { jury_cloud_opt_in: true, llm_api_key: "session-key".to_string(), ..AppSettings::default() };
        let readiness = build_agentic_readiness(&keyed, &[], &no_script);
        assert_eq!(readiness.checks[0].status, "ready");
        assert!(readiness.checks[0].detail.contains("gemini-2.5-pro"));

        // Explicit diagnostic CTC selection with its model files present: the selected primary is
        // ready and single-engine mode reports coverage as not_required, so the verdict is ready.
        let ctc = AppSettings { asr_model_size: AsrModelSize::CTC300M, ..AppSettings::default() };
        let ctc_status = vec![
            serde_json::json!({ "filename": models::OMNIASR_CTC_300M_MODEL, "downloaded": true }),
            serde_json::json!({ "filename": models::OMNIASR_CTC_300M_TOKENS, "downloaded": true }),
        ];
        let readiness = build_agentic_readiness(&ctc, &ctc_status, &no_script);
        assert_eq!(readiness.status, "ready");
        assert_eq!(readiness.available_hypothesis_models, vec!["omniasr-ctc-300m".to_string()]);
        assert_eq!(readiness.checks[2].id, "hypothesis_coverage");
        assert_eq!(readiness.checks[2].status, "not_required");

        // Auxiliary multi-engine mode with only one engine ready: hypothesis coverage blocks.
        let auxiliary = AppSettings {
            asr_model_size: AsrModelSize::CTC300M,
            multi_engine_hypotheses: true,
            ..AppSettings::default()
        };
        let readiness = build_agentic_readiness(&auxiliary, &ctc_status, &no_script);
        assert_eq!(readiness.checks[2].status, "blocked");
        assert!(!readiness.ready, "one usable engine cannot claim multi-model corroboration coverage");
    }

    #[test]
    fn external_provider_status_with_a_script_runs_the_real_wsl_probe() {
        let settings =
            AppSettings { external_asr_script_path: "/opt/cortex/provider.py".to_string(), ..AppSettings::default() };
        let status = external_provider_status(&settings);
        assert_eq!(status["script"], "/opt/cortex/provider.py");
        let available = status["available"].as_bool().expect("probe result is a bool");
        let message = status["message"].as_str().expect("probe message");
        if available {
            assert_eq!(message, "WSL is available; provider script will be used for external ASR");
        } else {
            assert_eq!(message, "WSL is not available or not healthy on this machine");
        }
    }

    #[test]
    fn kill_and_reap_child_stops_a_live_worker_process() {
        // Spawn the long-running probe binary directly (no shell wrapper) so the kill reaps the
        // actual worker instead of a shell that would orphan it.
        let mut command = if cfg!(windows) {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        let mut child = command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleeper child");
        kill_and_reap_child(&mut child, "test sleeper");
        assert!(
            child.try_wait().expect("reaped child reports a status").is_some(),
            "kill_and_reap_child must leave the child fully reaped"
        );
    }

    #[test]
    fn run_blocking_returns_worker_results_and_converts_panics_to_errors() {
        let ok = tauri::async_runtime::block_on(run_blocking(|| Ok::<_, String>(41 + 1))).expect("worker result");
        assert_eq!(ok, 42);

        let failed = tauri::async_runtime::block_on(run_blocking::<(), _>(|| panic!("worker exploded")))
            .expect_err("a worker panic must become a clean error, never an abort");
        assert!(failed.starts_with("background task failed"), "unexpected panic mapping: {failed}");
    }

    #[test]
    fn job_center_rate_refusal_and_cloud_consent_gate_are_exact() {
        let busy = public_job_rate_limited_error();
        assert_eq!(busy.code, "JOB_CENTER_BUSY");
        assert!(busy.retryable);
        assert_eq!(busy.suggested_action, Some(crate::ipc_contract::SuggestedActionV1::Retry));

        let tmp = tempfile::tempdir().unwrap();
        let app_state = crate::test_support::app_state(tmp.path().to_path_buf());
        let refused = require_cloud_llm_consent(&app_state).expect_err("consent defaults OFF");
        assert_eq!(refused, "Cloud LLM opt-in is required for this cloud upload. Enable it in Settings.");
        app_state.lock_settings().cloud_llm_opt_in = true;
        require_cloud_llm_consent(&app_state).expect("explicit opt-in unlocks the cloud channel");
    }

    #[test]
    fn background_db_writer_guard_arms_the_restore_fence_for_its_lifetime() {
        let before = BG_DB_WRITERS.load(std::sync::atomic::Ordering::SeqCst);
        {
            let _writer = BgDbWriterGuard::new();
            assert!(bg_db_writers_active(), "a live background writer must fence a restore");
            assert!(BG_DB_WRITERS.load(std::sync::atomic::Ordering::SeqCst) > before);
        }
        // No absolute zero assertion: other suites may hold their own writers concurrently. The
        // guard's own increment must be gone.
        assert!(BG_DB_WRITERS.load(std::sync::atomic::Ordering::SeqCst) >= before);
    }

    #[test]
    fn jury_db_access_uses_a_dedicated_connection_and_falls_back_to_the_shared_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let state = app.state::<AppState>();

        // File-backed library: `with` opens its own dedicated connection to the same database.
        let source = jury_db_source(&state);
        assert!(source.with(|db| db.get_segment_by_id("seg-nope").expect("dedicated read")).is_none());

        // ":memory:" is connection-private, so `with` must fall back to the shared handle.
        let fallback = JuryDbSource { db_path: ":memory:".to_string(), shared: state.db_arc() };
        assert!(fallback.with(|db| db.get_segment_by_id("seg-nope").expect("shared-handle read")).is_none());
    }

    #[test]
    fn open_jury_db_connection_requires_a_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let state = app.state::<AppState>();
        assert!(open_jury_db_connection(&state).is_some(), "a configured data dir opens a dedicated connection");
        *state.lock_data_dir() = None;
        assert!(open_jury_db_connection(&state).is_none(), "no data dir means no jury connection, not a panic");
    }

    #[test]
    fn champion_provenance_recognition_fails_closed_on_unknown_models() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        {
            let state = app.state::<AppState>();
            let db = state.lock_db();
            crate::registry::register_candidate(
                &db,
                &crate::registry::NewModelVersion {
                    id: "cand-7b-prov".to_string(),
                    family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                    model_card_name: None,
                    checkpoint_sha256: "c".repeat(64),
                    checkpoint_path: "/ckpt/cand-7b-prov".to_string(),
                    source: "meta-stock".to_string(),
                    license: "SAIL".to_string(),
                },
            )
            .expect("register champion-family candidate");
            crate::registry::register_candidate(
                &db,
                &crate::registry::NewModelVersion {
                    id: "diag-mms-prov".to_string(),
                    family: "mms".to_string(),
                    model_card_name: None,
                    checkpoint_sha256: "d".repeat(64),
                    checkpoint_path: "/ckpt/diag-mms-prov".to_string(),
                    source: "meta-stock".to_string(),
                    license: "CC-BY-NC".to_string(),
                },
            )
            .expect("register diagnostic-family candidate");
        }
        let state = app.state::<AppState>();
        let db = state.lock_db();
        let segment_with = |model: Option<&str>| crate::db::SpeechSegment {
            id: "seg-prov".to_string(),
            audio_path: "C:/fixtures/seg-prov.wav".to_string(),
            model_version_id: model.map(str::to_string),
            ..Default::default()
        };
        assert!(!segment_recorded_model_is_champion(&db, &segment_with(None)), "no provenance is never champion");
        assert!(!segment_recorded_model_is_champion(&db, &segment_with(Some("   "))));
        assert!(segment_recorded_model_is_champion(&db, &segment_with(Some(LEGACY_CHAMPION_MODEL_ID))));
        assert!(segment_recorded_model_is_champion(&db, &segment_with(Some("cand-7b-prov"))));
        assert!(!segment_recorded_model_is_champion(&db, &segment_with(Some("diag-mms-prov"))));
        assert!(
            !segment_recorded_model_is_champion(&db, &segment_with(Some("never-registered"))),
            "unknown provenance must fail closed"
        );
    }

    #[test]
    fn decision_predicates_read_only_real_recorded_signals() {
        let base = crate::db::SpeechSegment {
            id: "seg-pred".to_string(),
            audio_path: "C:/fixtures/seg-pred.wav".to_string(),
            ..Default::default()
        };
        assert!(!has_human_decision(&base));
        assert!(!has_human_decision(&crate::db::SpeechSegment {
            human_decision: Some("   ".to_string()),
            ..base.clone()
        }));
        assert!(has_human_decision(&crate::db::SpeechSegment {
            human_decision: Some("approved".to_string()),
            ..base.clone()
        }));

        assert!(!has_final_machine_verdict(&base), "no verdict is not final");
        assert!(!has_final_machine_verdict(&crate::db::SpeechSegment {
            verdict: Some("  ".to_string()),
            ..base.clone()
        }));
        assert!(has_final_machine_verdict(&crate::db::SpeechSegment {
            verdict: Some("jury_accept".to_string()),
            escalated: false,
            ..base.clone()
        }));
        assert!(
            !has_final_machine_verdict(&crate::db::SpeechSegment {
                verdict: Some("escalated".to_string()),
                escalated: true,
                ..base
            }),
            "an escalated row is still open, never final"
        );
    }

    #[test]
    fn openrouter_jury_model_slug_mapping_is_a_closed_mechanism() {
        assert_eq!(openrouter_jury_model_id(""), "google/gemini-2.5-pro");
        assert_eq!(openrouter_jury_model_id("  "), "google/gemini-2.5-pro");
        assert_eq!(openrouter_jury_model_id("gemini-2.5-pro"), "google/gemini-2.5-pro");
        assert_eq!(openrouter_jury_model_id("google/gemini-2.5-pro"), "google/gemini-2.5-pro");
        assert_eq!(openrouter_jury_model_id("vendor/some-model"), "vendor/some-model");
    }

    #[test]
    fn reference_report_consensus_and_best_selection_are_exact() {
        assert!(!reference_reports_have_commit_consensus(&[]), "no reports can never be consensus");
        assert!(!reference_reports_have_commit_consensus(&[
            selection_report("model-a", "deng yek", 0.9, 0.2, true),
            selection_report("model-b", "deng yek", 0.8, 0.1, false),
        ]));
        assert!(reference_reports_have_commit_consensus(&[
            selection_report("model-a", "deng yek", 0.9, 0.2, true),
            selection_report("model-b", "deng yek", 0.8, 0.1, true),
        ]));
        assert!(!reference_reports_have_commit_consensus(&[
            selection_report("model-a", "deng yek", 0.9, 0.2, true),
            selection_report("model-b", "deng du", 0.8, 0.1, true),
        ]));
        assert!(
            !reference_reports_have_commit_consensus(&[
                selection_report("model-a", "", 0.9, 0.2, true),
                selection_report("model-b", "", 0.8, 0.1, true),
            ]),
            "an empty normalized transcript can never count as agreement"
        );

        assert!(best_reference_report(&[]).is_none());
        let best = best_reference_report(&[
            selection_report("model-low", "deng", 0.4, 0.9, true),
            selection_report("model-high", "deng", 0.9, 0.1, true),
        ])
        .expect("best by score");
        assert_eq!(best.selected_model_id, "model-high");
        let tie_break = best_reference_report(&[
            selection_report("model-thin", "deng", 0.9, 0.05, true),
            selection_report("model-wide", "deng", 0.9, 0.4, true),
        ])
        .expect("best by margin on a score tie");
        assert_eq!(tie_break.selected_model_id, "model-wide");

        let mut scored = selection_report("model-scored", "deng", 0.8, 0.2, true);
        scored.scores.push(crate::agentic::CandidateSelectionScore {
            model_id: "model-scored".to_string(),
            transcript: "deng".to_string(),
            final_score: 0.8,
            reference_window_overlap: 0.7,
            reference_global_overlap: 0.4,
            text_quality: 0.9,
            model_prior: 0.5,
        });
        let unscored = selection_report("model-unscored", "deng", 0.6, 0.1, false);
        let agreement = reference_agreement_reports(&[scored, unscored]);
        assert_eq!(agreement.len(), 2);
        assert_eq!(agreement[0].reference_window_overlap, 0.7);
        assert_eq!(agreement[0].reference_global_overlap, 0.4);
        assert!(agreement[0].should_commit);
        assert_eq!(agreement[1].reference_window_overlap, 0.0, "no score rows means no invented overlap");
        assert!(!agreement[1].should_commit);

        let evidence = reference_selection_evidence(&agreement_source_report());
        assert_eq!(evidence.tool, "source_reference_adjudicator");
        assert!(evidence.supports_hypothesis);
        assert!(evidence.result.contains("winner=model-scored"));
    }

    fn agreement_source_report() -> crate::agentic::CandidateSelectionReport {
        let mut report = selection_report("model-scored", "deng", 0.8, 0.2, true);
        report.scores.push(crate::agentic::CandidateSelectionScore {
            model_id: "model-scored".to_string(),
            transcript: "deng".to_string(),
            final_score: 0.8,
            reference_window_overlap: 0.7,
            reference_global_overlap: 0.4,
            text_quality: 0.9,
            model_prior: 0.5,
        });
        report
    }

    #[test]
    fn hypothesis_coverage_guard_blocks_thin_corroboration_with_the_served_transcript() {
        let segment = crate::db::SpeechSegment {
            id: "seg-coverage".to_string(),
            audio_path: "C:/fixtures/seg-coverage.wav".to_string(),
            raw_transcript: "raw machine draft".to_string(),
            normalized_transcript: Some("normalized draft".to_string()),
            verdict_transcript: Some("verdict draft".to_string()),
            ..Default::default()
        };
        let hypothesis = |model: &str, transcript: &str| crate::db::SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: model.to_string(),
            transcript: transcript.to_string(),
            confidence: Some(0.9),
        };

        assert!(
            hypothesis_coverage_guard(&segment, &[hypothesis("model-a", "deng"), hypothesis("model-b", "deng")])
                .is_none(),
            "two distinct non-empty model hypotheses satisfy the corroboration floor"
        );

        let guard = hypothesis_coverage_guard(&segment, &[hypothesis("model-a", "deng")])
            .expect("a single model must be guarded");
        assert_eq!(guard.selected_model_id, "multi-model-hypothesis-coverage-guard");
        assert!(!guard.should_commit);
        assert_eq!(guard.selected_transcript, "verdict draft", "the guard serves the segment's precedence text");
        assert!(guard.rationale.contains("fewer than 2 non-empty model hypotheses"));
    }

    #[test]
    fn source_reference_identity_checks_guard_stale_whole_file_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let audio = tmp.path().join("source-identity.wav");
        std::fs::write(&audio, b"source-identity-fixture-bytes").unwrap();
        crate::test_support::await_stable_fixture(&audio);
        let audio_path = audio.to_string_lossy().to_string();

        let record = |hash: Option<&str>, size: Option<i64>| crate::db::SourceTranscriptRecord {
            audio_path: audio_path.clone(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: hash.map(str::to_string),
            audio_size_bytes: size,
            transcript_path: "C:/fixtures/reference.json".to_string(),
            transcript_text: "deng yek du".to_string(),
            created_at: None,
        };
        assert!(!source_reference_has_stored_audio_identity(&record(None, None)));
        assert!(!source_reference_has_stored_audio_identity(&record(Some("   "), None)));
        assert!(source_reference_has_stored_audio_identity(&record(Some("hash"), None)));
        assert!(source_reference_has_stored_audio_identity(&record(None, Some(1))));

        let mut cache = std::collections::HashMap::new();
        let identity = source_reference_current_audio_identity(&audio_path, &mut cache)
            .expect("a readable source file has an identity");
        assert_eq!(identity.content_hash.len(), 64);
        assert!(identity.size_bytes > 0);
        // Cache arm: mutate the file, then re-ask — the cached identity (not a re-hash) is served.
        std::fs::write(&audio, b"different bytes entirely").unwrap();
        let cached = source_reference_current_audio_identity(&audio_path, &mut cache).expect("cached identity");
        assert_eq!(cached.content_hash, identity.content_hash, "the per-run identity cache must serve its snapshot");
        std::fs::write(&audio, b"source-identity-fixture-bytes").unwrap();
        crate::test_support::await_stable_fixture(&audio);

        let missing = tmp.path().join("never-existed.wav").to_string_lossy().to_string();
        assert!(source_reference_current_audio_identity(&missing, &mut cache).is_none());

        let matching = record(Some(&identity.content_hash), Some(identity.size_bytes));
        assert!(source_reference_matches_current_audio(&matching, &mut cache));
        let wrong_hash = record(Some(&"0".repeat(64)), Some(identity.size_bytes));
        assert!(!source_reference_matches_current_audio(&wrong_hash, &mut cache));
        assert!(
            !source_reference_matches_current_audio(&record(None, None), &mut cache),
            "a reference without stored identity can never match"
        );

        let (usable, stale) =
            filter_source_references_for_current_audio(vec![matching.clone(), wrong_hash.clone()], &mut cache);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].audio_content_hash.as_deref(), Some(identity.content_hash.as_str()));
        assert_eq!(stale, vec!["gemini-2.5-pro".to_string()]);
    }

    #[test]
    fn source_reference_coverage_guard_arms_are_exact() {
        let segment = crate::db::SpeechSegment {
            id: "seg-ref-guard".to_string(),
            audio_path: "C:/fixtures/seg-ref-guard.wav".to_string(),
            raw_transcript: "raw draft".to_string(),
            ..Default::default()
        };
        let reference = |model: &str, text: &str| crate::db::SourceTranscriptRecord {
            audio_path: segment.audio_path.clone(),
            model_id: model.to_string(),
            audio_content_hash: Some("hash".to_string()),
            audio_size_bytes: Some(1),
            transcript_path: "C:/fixtures/reference.json".to_string(),
            transcript_text: text.to_string(),
            created_at: None,
        };

        let offline = AppSettings::default();
        assert!(
            source_reference_coverage_guard(&offline, &segment, &[], &[]).is_none(),
            "offline mode with no stored references requires no coverage"
        );

        let stale = source_reference_coverage_guard(&offline, &segment, &[], &["gemini-2.5-pro".to_string()])
            .expect("a stale required reference must guard even offline");
        assert_eq!(stale.selected_model_id, "source-reference-audio-identity-guard");
        assert!(!stale.should_commit);
        assert!(stale.reference_model_id.as_deref().unwrap().contains("stale:gemini-2.5-pro"));

        let opted_in = AppSettings { jury_cloud_opt_in: true, ..AppSettings::default() };
        let missing = source_reference_coverage_guard(&opted_in, &segment, &[], &[])
            .expect("opt-in with no stored references must guard");
        assert_eq!(missing.selected_model_id, "source-reference-coverage-guard");
        assert!(missing.rationale.contains("gemini-2.5-pro"));
        assert_eq!(missing.selected_transcript, "raw draft", "no verdict/normalized text falls back to raw");

        assert!(
            source_reference_coverage_guard(&opted_in, &segment, &[reference("gemini-2.5-pro", "deng yek du")], &[],)
                .is_none(),
            "full non-empty coverage of the required model needs no guard"
        );
        assert!(
            source_reference_coverage_guard(&opted_in, &segment, &[reference("gemini-2.5-pro", "   ")], &[]).is_some(),
            "an empty-text reference is not coverage"
        );
    }

    #[test]
    fn reference_selection_for_segment_serves_none_without_references_and_guards_stale_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let audio = tmp.path().join("selection-source.wav");
        std::fs::write(&audio, b"selection-source-fixture-bytes").unwrap();
        crate::test_support::await_stable_fixture(&audio);
        let segment = crate::db::SpeechSegment {
            id: "seg-selection".to_string(),
            audio_path: audio.to_string_lossy().to_string(),
            raw_transcript: "deng yek du".to_string(),
            ..Default::default()
        };
        let settings = AppSettings::default();
        let mut duration_cache = std::collections::HashMap::new();
        let mut identity_cache = std::collections::HashMap::new();

        let state = app.state::<AppState>();
        let db = state.lock_db();
        db.insert_segment(&segment).expect("seed segment");
        let none =
            reference_selection_for_segment(&db, &settings, &segment, &[], &mut duration_cache, &mut identity_cache)
                .expect("selection without references");
        assert!(none.is_none(), "no stored whole-file references means no reference report");

        db.upsert_source_transcript(&crate::db::SourceTranscriptRecord {
            audio_path: segment.audio_path.clone(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: Some("1".repeat(64)),
            audio_size_bytes: Some(1),
            transcript_path: "C:/fixtures/stale-reference.json".to_string(),
            transcript_text: "deng yek du".to_string(),
            created_at: None,
        })
        .expect("seed stale reference");
        let guarded =
            reference_selection_for_segment(&db, &settings, &segment, &[], &mut duration_cache, &mut identity_cache)
                .expect("selection with a stale reference")
                .expect("stale identity must produce a guard report");
        assert_eq!(guarded.selected_model_id, "source-reference-audio-identity-guard");
        assert!(!guarded.should_commit);
    }

    #[test]
    fn load_hypotheses_synthesizes_a_vote_only_for_diagnostic_engines() {
        let tmp = tempfile::tempdir().unwrap();
        let app = managed_app_state(tmp.path());
        let state = app.state::<AppState>();
        let db = state.lock_db();
        let segment = crate::db::SpeechSegment {
            id: "seg-hyps".to_string(),
            audio_path: "C:/fixtures/seg-hyps.wav".to_string(),
            raw_transcript: "deng yek du".to_string(),
            confidence: Some(0.7),
            ..Default::default()
        };
        db.insert_segment(&segment).expect("seed segment");

        let champion_mode = AppSettings::default();
        assert_eq!(champion_mode.asr_model_size, AsrModelSize::WSL7B, "champion is the shipped default");
        let champion_hyps =
            load_hypotheses_for_segment(&db, &champion_mode, &segment.id, &segment).expect("champion-mode load");
        assert!(
            champion_hyps.is_empty(),
            "champion mode must never invent provenance for a row without a recorded producer"
        );

        let diagnostic_mode = AppSettings { asr_model_size: AsrModelSize::CTC300M, ..AppSettings::default() };
        let diagnostic_hyps =
            load_hypotheses_for_segment(&db, &diagnostic_mode, &segment.id, &segment).expect("diagnostic-mode load");
        assert_eq!(diagnostic_hyps.len(), 1, "diagnostic mode synthesizes the one honest ASR vote");
        assert_eq!(diagnostic_hyps[0].model_id, "asr");
        assert_eq!(diagnostic_hyps[0].transcript, "deng yek du");
        assert_eq!(diagnostic_hyps[0].confidence, Some(0.7));
    }

    #[test]
    fn reference_selection_text_key_normalizes_for_metric_equality() {
        let spaced = selection_report("model-a", "  deng   yek  ", 0.9, 0.2, true);
        let tight = selection_report("model-b", "deng yek", 0.9, 0.2, true);
        assert_eq!(
            reference_selection_text_key(&spaced),
            reference_selection_text_key(&tight),
            "whitespace variants must agree on one metric key"
        );
        assert!(reference_selection_text_key(&selection_report("model-c", "   ", 0.9, 0.2, true)).is_empty());
    }
}
