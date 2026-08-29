//! Export IPC commands — the first slice extracted from `commands.rs` (Week-4 decomposition).
//!
//! Behaviour and command NAMES are unchanged: `commands.rs` re-exports this module with
//! `pub use export::*;`, so `lib.rs`'s `invoke_handler` still names `commands::export_dataset` and the
//! frontend's `invoke('export_dataset')` is untouched. Nothing here is a rewrite — these are the same
//! functions that lived in `commands.rs`, only relocated.
//!
//! They are all `async` + `run_blocking`: a full-library export (DB scan + serialize + hash + atomic
//! write) would otherwise freeze the UI thread. The DB guard is taken INSIDE the blocking task, never
//! held across an await.

use super::{run_blocking, STRICT_RATE_LIMITER};
use crate::validation::input as validate;
use crate::AppState;
use std::path::Path;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn export_dataset(
    path: String,
    format: String,
    state: State<'_, AppState>,
) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_dataset")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_dataset"))?;
    let validated_path =
        validate::validate_output_path(&path).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let fmt = match format.to_lowercase().as_str() {
        "csv" => crate::settings::ExportFormat::Csv,
        "jsonl" => crate::settings::ExportFormat::Jsonl,
        "parquet" => crate::settings::ExportFormat::Parquet,
        _ => crate::settings::ExportFormat::Json,
    };
    // Off the main thread: a full-library export (DB scan + serialize + hash + atomic write) would
    // otherwise freeze the UI. The DB guard is taken INSIDE the blocking task, never across an await.
    // Bracketed as a durable job so a crash mid-export is reaped as INTERRUPTED at the next startup and
    // the outcome shows up in get_jobs — the op's real work is unchanged.
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result = run_blocking(move || {
        store.export_dataset(&job_id, Path::new(&validated_path), &fmt).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner dataset export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::Dataset, &error)
    })
}

/// Export a plain, human-facing transcript / subtitle file (txt | srt | vtt) from the library —
/// distinct from the ML dataset export. Path is validated the same way; unknown formats fall to txt.
#[tauri::command]
#[specta::specta]
pub async fn export_transcript(
    path: String,
    format: String,
    state: State<'_, AppState>,
) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_transcript")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_transcript"))?;
    let validated_path =
        validate::validate_output_path(&path).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let fmt = crate::transcript_export::TranscriptFormat::from_str_lossy(&format);
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result = run_blocking(move || {
        store.export_transcript(&job_id, Path::new(&validated_path), fmt).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner transcript export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::Transcript, &error)
    })
}

#[tauri::command]
pub async fn export_dataset_bundle(
    path: String,
    production: bool,
    warning_threshold: Option<usize>,
    state: State<'_, AppState>,
) -> Result<crate::export_bundle::BundleExportResult, String> {
    STRICT_RATE_LIMITER.check("export_dataset_bundle")?;
    let validated_path = validate::validate_output_path(&path)?;
    let warning_threshold = warning_threshold.unwrap_or(0);
    let settings = state.lock_settings().clone();
    let model_manager = state.lock_model_manager().clone(); // ModelManager is Clone (a PathBuf)
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    run_blocking(move || {
        store
            .export_dataset_bundle(
                &job_id,
                &model_manager,
                Path::new(&validated_path),
                &settings,
                production,
                warning_threshold,
            )
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn export_huggingface_dataset(
    path: String,
    state: State<'_, AppState>,
) -> Result<(), crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_huggingface_dataset")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_huggingface_dataset"))?;
    let validated_path =
        validate::validate_output_path(&path).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let settings = state.lock_settings().clone();
    // Bracketed as a durable job (same as export_dataset): a crash mid-export is reaped as INTERRUPTED
    // at the next startup and the outcome shows in get_jobs / the activity pill. Work is unchanged.
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result = run_blocking(move || {
        store.export_huggingface_dataset(&job_id, Path::new(&validated_path), &settings).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner Hugging Face export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::HuggingFace, &error)
    })
}

#[tauri::command]
#[specta::specta]
pub async fn export_audio(
    segment_ids: Vec<String>,
    options: crate::ipc_contract::AudioExportOptionsV1,
    state: State<'_, AppState>,
) -> Result<crate::ipc_contract::AudioExportResultV1, crate::ipc_contract::CommandErrorV1> {
    // Decodes + re-encodes one clip per segment to disk — throttle it like every sibling export
    // command (round-22 #5: it was the lone export missing a rate-limiter, a local DoS/disk-fill gap).
    STRICT_RATE_LIMITER
        .check("export_audio")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_audio"))?;
    for id in &segment_ids {
        validate::validate_identifier(id).map_err(|_| crate::ipc_contract::invalid_segment_id_error())?;
    }
    let mut options: crate::export_audio::AudioExportOptions = options.into();
    options.output_dir = validate::validate_output_path(&options.output_dir)
        .map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result =
        run_blocking(move || store.export_audio(&job_id, &segment_ids, &options).map_err(|e| e.to_string())).await;
    result.map(crate::ipc_contract::AudioExportResultV1::from).map_err(|error| {
        tracing::warn!("Owner reviewed-audio export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::Audio, &error)
    })
}

/// M2.7 / P1.6: export the gold set as a portable eval set (manifest.jsonl + 16 kHz WAV clips) under
/// `out_dir`. Returns the export summary (counts + manifest path).
#[tauri::command]
#[specta::specta]
pub async fn export_gold_eval_set(
    out_dir: String,
    state: State<'_, AppState>,
) -> Result<crate::eval::GoldEvalExport, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_gold_eval_set")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_gold_eval_set"))?;
    // Same trust-boundary guard every sibling export (incl. the directory export export_audio) enforces:
    // reject null bytes + Windows UNC syntactically, so a compromised renderer can't hand a
    // `\\attacker\share` out_dir straight to create_dir_all — which would drive the SMB redirector (a
    // forced-auth NTLM-credential leak) and write the gold clips off-machine. A null-byte-only check
    // (what this had) left that open. The dir comes from an OS folder picker in the real flow, so it
    // always exists → validate_output_path's parent-must-exist requirement never rejects a legit run.
    let validated =
        validate::validate_output_path(&out_dir).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result =
        run_blocking(move || store.export_gold_eval_set(&job_id, Path::new(&validated)).map_err(|e| e.to_string()))
            .await;
    result.map_err(|error| {
        tracing::warn!("Owner gold evaluation export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::GoldEval, &error)
    })
}

/// M5.1 / P5.1: export a fine-tune training pack (trainer manifest + 16 kHz clips) from human-verified
/// segments under `out_dir`, EXCLUDING holdout gold (the leak guard). Returns the pack summary.
#[tauri::command]
#[specta::specta]
pub async fn export_finetune_pack(
    out_dir: String,
    state: State<'_, AppState>,
) -> Result<crate::eval::FinetunePackResult, crate::ipc_contract::CommandErrorV1> {
    STRICT_RATE_LIMITER
        .check("export_finetune_pack")
        .map_err(|_| crate::ipc_contract::owner_critical_rate_limited("export_finetune_pack"))?;
    // Same UNC/null trust-boundary guard as export_gold_eval_set above (and every sibling export): the
    // pack writes 16 kHz clips under out_dir via create_dir_all, so a webview-supplied `\\attacker\share`
    // would leak NTLM creds + write off-machine. Null-byte-only was insufficient.
    let validated =
        validate::validate_output_path(&out_dir).map_err(|_| crate::ipc_contract::invalid_output_path_error())?;
    // P5.5: every pack export appends its provenance line to the durable corpus ledger.
    let ledger = state.lock_data_dir().clone().map(|d| d.join("corpus_ledger.jsonl"));
    let store = state.job_store();
    let job_id = uuid::Uuid::new_v4().to_string();
    let result = run_blocking(move || {
        store.export_finetune_pack(&job_id, Path::new(&validated), ledger.as_deref()).map_err(|e| e.to_string())
    })
    .await;
    result.map_err(|error| {
        tracing::warn!("Owner fine-tune export failed: {error}");
        crate::ipc_contract::public_export_error(crate::ipc_contract::ExportOperationV1::Finetune, &error)
    })
}
