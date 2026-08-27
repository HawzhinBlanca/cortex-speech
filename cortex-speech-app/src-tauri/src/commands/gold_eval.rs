//! Gold-set + shipped champion-eval IPC commands. Auxiliary-engine evaluation remains available to
//! explicit offline diagnostic code and is deliberately not registered with the desktop renderer.
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

/// Stamp on every `eval_runs` row whose hypotheses came from the renderer rather than from this app
/// running an engine. Unstamped rows are reserved for the closed loop (`run_gold_eval_asr`), which
/// derives both the text and the label from the registered champion.
pub const EXTERNAL_HYPOTHESIS_MODEL_PREFIX: &str = "external-hypotheses:";

/// Label an eval row built from caller-supplied text. `run_gold_eval` lets the caller choose the
/// hypotheses AND the model label, so without this an XSS'd or scripted renderer mints a durable row
/// reading `omniasr-wsl-7b — CER 0.00%` (hypotheses = the references) that is indistinguishable in the
/// eval history from a measured champion run, and that `scorecard` would happily select as a baseline.
/// The prefix keeps such a row readable and honest instead: visibly not a measurement.
pub fn external_hypothesis_label(model_id: &str) -> String {
    if model_id.starts_with(EXTERNAL_HYPOTHESIS_MODEL_PREFIX) {
        model_id.to_string()
    } else {
        format!("{EXTERNAL_HYPOTHESIS_MODEL_PREFIX}{model_id}")
    }
}

#[tauri::command]
pub async fn run_gold_eval(
    state: State<'_, AppState>,
    model_id: String,
    hypotheses: Vec<(String, String)>,
) -> Result<crate::eval::EvalRunResult, String> {
    RATE_LIMITER.check("run_gold_eval")?;
    let model_id = external_hypothesis_label(&model_id);
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::eval::run_gold_eval(&db, &model_id, hypotheses).map_err(|e| e.to_string())
    })
    .await
}

/// Closed-loop gold eval: runs the exact registered WSL7B champion over gold audio and scores the
/// produced hypotheses. The renderer supplies neither text nor a model label.
#[tauri::command]
pub async fn run_gold_eval_asr(state: State<'_, AppState>) -> Result<crate::eval::EvalRunResult, String> {
    RATE_LIMITER.check("run_gold_eval_asr")?;
    let mutation = super::begin_mutation()?;
    // Clone the pipeline so the (potentially long) ASR loop does not hold the pipeline lock, and run
    // it OFF the main thread.
    let pipeline = state.lock_pipeline().clone();
    run_blocking(move || {
        let _mutation = mutation;
        pipeline.run_gold_eval_asr().map_err(|e| e.to_string())
    })
    .await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_supplied_eval_rows_cannot_wear_a_measured_model_label() {
        // The champion's own registry id must never be reachable as a label for renderer-supplied text.
        let stamped = external_hypothesis_label("omniasr-wsl-7b");
        assert!(
            stamped.starts_with(EXTERNAL_HYPOTHESIS_MODEL_PREFIX),
            "an eval row built from caller text must be stamped, not labeled as a measurement: {stamped}"
        );
        assert_ne!(stamped, "omniasr-wsl-7b");
        assert!(stamped.contains("omniasr-wsl-7b"), "the caller's claimed label stays readable: {stamped}");

        // Idempotent: a re-submitted stamped label must not grow a second prefix, and a caller that
        // spoofs the prefix buys nothing — the row still reads as caller-supplied either way.
        assert_eq!(external_hypothesis_label(&stamped), stamped);
    }
}
