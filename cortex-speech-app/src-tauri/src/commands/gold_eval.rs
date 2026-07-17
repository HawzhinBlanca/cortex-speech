//! Gold-set + gold-eval IPC commands — slice 5 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use gold_eval::*;`),
//! so `lib.rs`'s invoke_handler still names `commands::run_gold_eval` and the frontend invokes are
//! untouched. Same functions, only relocated. (The eval-adjacent list_eval_runs / build_scorecard stay
//! in commands.rs for now.)
//!
//! Gold-set imports + the WER/CER eval runs are whole-dataset/model work, so the heavy ones run via
//! `run_blocking` to keep the UI thread free.

use super::{run_blocking, RATE_LIMITER, STRICT_RATE_LIMITER};
use crate::validation::input as validate;
use crate::AppState;
use tauri::State;

#[tauri::command]
pub async fn import_gold_segments(
    state: State<'_, AppState>,
    inputs: Vec<crate::eval::GoldSegmentInput>,
) -> Result<usize, String> {
    RATE_LIMITER.check("import_gold_segments")?;
    // Validate every frontend-supplied input BEFORE any file is opened — the same guard every other
    // file-opening command applies (import_audio_file, import_model_checkpoint, get_waveform, ...).
    // Without it, a compromised/XSS'd renderer could pass an arbitrary, UNC, or traversal path that
    // eval::import_gold_segments -> source_audio_identity opens and fully reads (info disclosure /
    // outbound-SMB on Windows), plus persist it; the reference was likewise uncapped.
    let validated: Vec<crate::eval::GoldSegmentInput> = inputs
        .into_iter()
        .map(|inp| {
            let audio_path = validate::validate_file_path(&inp.audio_path)?;
            validate::validate_text(&inp.reference, 100_000, "Gold reference")?;
            Ok::<_, String>(crate::eval::GoldSegmentInput { audio_path, ..inp })
        })
        .collect::<Result<_, _>>()?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::eval::import_gold_segments(&db, validated).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn run_gold_eval(
    state: State<'_, AppState>,
    model_id: String,
    hypotheses: Vec<(String, String)>,
) -> Result<crate::eval::EvalRunResult, String> {
    RATE_LIMITER.check("run_gold_eval")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::eval::run_gold_eval(&db, &model_id, hypotheses).map_err(|e| e.to_string())
    })
    .await
}

/// Closed-loop gold eval: runs the real local ASR over the gold set's audio and scores
/// the produced hypotheses (no caller-supplied text). This is the honest-CER entrypoint.
/// `model_id` defaults to the active local model when omitted.
#[tauri::command]
pub async fn run_gold_eval_asr(
    state: State<'_, AppState>,
    model_id: Option<String>,
) -> Result<crate::eval::EvalRunResult, String> {
    RATE_LIMITER.check("run_gold_eval_asr")?;
    // Clone the pipeline so the (potentially long) ASR loop does not hold the pipeline lock, and run
    // it OFF the main thread.
    let pipeline = state.lock_pipeline().clone();
    run_blocking(move || pipeline.run_gold_eval_asr(model_id.as_deref()).map_err(|e| e.to_string())).await
}

#[tauri::command]
pub async fn run_gold_eval_local(
    state: State<'_, AppState>,
    model_id: String,
) -> Result<crate::eval::EvalRunResult, String> {
    RATE_LIMITER.check("run_gold_eval_local")?;
    // Clone the pipeline and let it open its own DB connection, so neither global mutex is held
    // across the multi-segment ASR eval loop, and run it OFF the main thread (was minutes of freeze).
    let pipeline = state.lock_pipeline().clone();
    run_blocking(move || pipeline.run_gold_eval_local(&model_id).map_err(|e| e.to_string())).await
}

/// Turn the human-corrected segments of one source file into a holdout GOLD benchmark entry. Run it
/// after correcting a file in the Review inbox: it concatenates the corrected transcripts into the
/// gold reference (excluded from all training). Returns the number of gold rows created.
#[tauri::command]
pub async fn create_gold_from_file(audio_path: String, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("create_gold_from_file")?;
    if audio_path.contains('\0') {
        return Err("Audio path contains null bytes".to_string());
    }
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::eval::create_gold_from_verified_file(&db, &audio_path).map_err(|e| e.to_string())
    })
    .await
}

/// M2.7 / P1.6: bulk-promote every reviewed source file into the gold set. Returns rows created.
#[tauri::command]
pub async fn import_verified_segments_as_gold(state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("import_verified_segments_as_gold")?;
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::eval::import_verified_segments_as_gold(&db).map_err(|e| e.to_string())
    })
    .await
}
